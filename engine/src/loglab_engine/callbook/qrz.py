# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The QRZ.com XML callbook service, from its published interface specification.

QRZ.com answers two kinds of request at one address: a login, which returns a
session key, and a lookup by callsign with that key. Both answers are small
XML documents. Everything in them is untrusted (Principle XII): an answer is
size-capped, refused if it declares a DTD or entities, and every value kept is
cleaned and length-limited; a grid square that is not a Maidenhead locator is
dropped. Nothing here touches the database or the log.

The network is optional (Principle V). Every failure to reach QRZ.com is a
:class:`QrzUnreachableError`, which callers treat as "no lookup this time".
"""

from __future__ import annotations

import re
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final
from xml.etree import ElementTree

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

BASE_URL: Final = "https://xmldata.qrz.com/xml/current/"
TIMEOUT_SECONDS: Final = 5.0
MAX_REPLY: Final = 64 * 1024
MAX_TEXT: Final = 64
MAX_MESSAGE: Final = 200

_CALL: Final = re.compile(r"^[A-Z0-9]+(/[A-Z0-9]+)*$")
_GRID: Final = re.compile(r"^[A-R]{2}[0-9]{2}([A-X]{2}([0-9]{2})?)?$", re.IGNORECASE)
_MAX_CALL: Final = 20
_DECLARATION: Final = re.compile(rb"<!(DOCTYPE|ENTITY)", re.IGNORECASE)


class QrzError(Exception):
    """QRZ.com could not answer the request."""


class QrzUnreachableError(QrzError):
    """QRZ.com could not be reached (no internet, a timeout, or an HTTP error)."""


class QrzReplyError(QrzError):
    """QRZ.com's answer was too large, malformed, or not what the interface describes."""


class QrzLoginError(QrzError):
    """QRZ.com refused the username and password."""


class QrzSessionError(QrzError):
    """The session key is no longer valid; log in again."""


@dataclass(frozen=True, slots=True)
class CallbookEntry:
    """What a callbook says about a call, cleaned, with each value optional."""

    call: str
    name: str | None = None
    qth: str | None = None
    state: str | None = None
    country: str | None = None
    grid: str | None = None

    def to_json(self) -> dict[str, str]:
        """The entry as a JSON object, leaving out empty values."""
        values = {
            "call": self.call,
            "name": self.name,
            "qth": self.qth,
            "state": self.state,
            "country": self.country,
            "grid": self.grid,
        }
        return {k: v for k, v in values.items() if v is not None}

    @classmethod
    def from_json(cls, data: Mapping[str, object]) -> CallbookEntry:
        """Rebuild an entry saved with :meth:`to_json`, cleaning it again.

        Raises:
            QrzReplyError: It has no usable call.
        """

        def text(key: str) -> str | None:
            value = data.get(key)
            return value if isinstance(value, str) else None

        return _entry(text("call"), text)


@dataclass(frozen=True, slots=True)
class QrzReply:
    """One answer from QRZ.com: its session part, and the callsign record if any."""

    key: str | None
    error: str | None
    message: str | None
    entry: CallbookEntry | None


def clean_text(value: str | None, limit: int = MAX_TEXT) -> str | None:
    """``value`` with control characters removed, spaces collapsed, and length capped."""
    if value is None:
        return None
    kept = "".join(" " if unicodedata.category(c).startswith("C") else c for c in value)
    collapsed = " ".join(kept.split())[:limit].strip()
    return collapsed or None


def _clean_grid(value: str | None) -> str | None:
    text = clean_text(value)
    if text is None or _GRID.match(text) is None:
        return None
    # Maidenhead writes the field in capitals and the subsquare in lower case.
    return text[:2].upper() + text[2:4] + text[4:6].lower() + text[6:]


def _clean_call(value: str | None) -> str | None:
    text = clean_text(value, _MAX_CALL)
    if text is None:
        return None
    text = text.upper()
    return text if _CALL.match(text) else None


def _entry(call: str | None, text: Callable[[str], str | None]) -> CallbookEntry:
    cleaned = _clean_call(call)
    if cleaned is None:
        raise QrzReplyError("the callsign record has no valid call")
    return CallbookEntry(
        call=cleaned,
        name=clean_text(text("name")),
        qth=clean_text(text("qth")),
        state=clean_text(text("state"), 32),
        country=clean_text(text("country")),
        grid=_clean_grid(text("grid")),
    )


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def parse_reply(body: bytes) -> QrzReply:
    """Read one QRZ.com XML answer.

    Raises:
        QrzReplyError: The answer is too large, declares a DTD or entities, is
            not well-formed, or is not a QRZ.com database answer.
    """
    if len(body) > MAX_REPLY:
        raise QrzReplyError(f"the answer is larger than {MAX_REPLY} bytes")
    # UTF-16 or UTF-32 would hide a declaration from the byte check below.
    if b"\x00" in body or _DECLARATION.search(body):
        raise QrzReplyError("the answer declares a DTD or entities; it is refused")
    try:
        # A DTD is refused above, so no entity can expand here.
        root = ElementTree.fromstring(body)  # noqa: S314 - size-capped, no DTD
    except ElementTree.ParseError as error:
        raise QrzReplyError(f"the answer is not well-formed XML: {error}") from error
    if _local(root.tag) != "QRZDatabase":
        raise QrzReplyError("the answer is not a QRZ.com database answer")

    def child(parent: ElementTree.Element, name: str) -> ElementTree.Element | None:
        return next((c for c in parent if _local(c.tag) == name), None)

    def value(parent: ElementTree.Element | None, name: str) -> str | None:
        found = None if parent is None else child(parent, name)
        return None if found is None else "".join(found.itertext())

    session = child(root, "Session")
    record = child(root, "Callsign")
    entry = None
    if record is not None:

        def text(key: str) -> str | None:
            if key == "name":
                parts = (clean_text(value(record, "fname")), clean_text(value(record, "name")))
                return " ".join(p for p in parts if p) or None
            return value(record, {"qth": "addr2"}.get(key, key))

        entry = _entry(value(record, "call"), text)
    return QrzReply(
        key=clean_text(value(session, "Key"), 128),
        error=clean_text(value(session, "Error"), MAX_MESSAGE),
        message=clean_text(value(session, "Message"), MAX_MESSAGE),
        entry=entry,
    )


def http_fetch(url: str) -> bytes:
    """GET ``url`` over HTTPS, with a timeout and a size cap.

    Raises:
        QrzUnreachableError: It could not be fetched.
        QrzReplyError: The answer is larger than :data:`MAX_REPLY`.
    """
    if not url.startswith("https://"):
        raise QrzUnreachableError("only HTTPS is used")
    request = urllib.request.Request(url, headers={"Accept": "application/xml"})  # noqa: S310 - https only
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:  # noqa: S310 - https only
            body: bytes = response.read(MAX_REPLY + 1)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        reason = getattr(error, "reason", error)
        raise QrzUnreachableError(f"QRZ.com could not be reached ({reason})") from error
    if len(body) > MAX_REPLY:
        raise QrzReplyError(f"the answer is larger than {MAX_REPLY} bytes")
    return body


class QrzClient:
    """Logs in to QRZ.com and looks up calls, through ``fetch``."""

    def __init__(self, agent: str, fetch: Callable[[str], bytes] = http_fetch) -> None:
        """Identify as ``agent`` (QRZ.com asks every program to); fetch with ``fetch``."""
        self._agent = agent
        self._fetch = fetch

    def _ask(self, query: dict[str, str]) -> QrzReply:
        return parse_reply(self._fetch(f"{BASE_URL}?{urllib.parse.urlencode(query)}"))

    def login(self, username: str, password: str) -> str:
        """Log in and return the session key.

        Raises:
            QrzLoginError: QRZ.com refused the login.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        reply = self._ask({"username": username, "password": password, "agent": self._agent})
        if reply.key is None:
            raise QrzLoginError(reply.error or "QRZ.com did not accept the login")
        return reply.key

    def lookup(self, key: str, call: str) -> tuple[CallbookEntry | None, str | None]:
        """Look ``call`` up; return its entry (``None`` if not found) and QRZ.com's note.

        Raises:
            QrzSessionError: The key has expired; log in again.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read, or reported an error.
        """
        reply = self._ask({"s": key, "callsign": call})
        if reply.entry is not None:
            return reply.entry, reply.message
        # The specification: a session part without a key means the session ended.
        if reply.key is None:
            raise QrzSessionError(reply.error or "the QRZ.com session ended")
        if reply.error is not None and reply.error.lower().startswith("not found"):
            return None, reply.message
        raise QrzReplyError(reply.error or "QRZ.com sent neither a record nor an error")
