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
from html.parser import HTMLParser
from typing import TYPE_CHECKING, Final, override
from xml.etree import ElementTree

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

BASE_URL: Final = "https://xmldata.qrz.com/xml/current/"
TIMEOUT_SECONDS: Final = 5.0
MAX_REPLY: Final = 64 * 1024
MAX_TEXT: Final = 64
MAX_MESSAGE: Final = 200
MAX_BIO_PAGE: Final = 512 * 1024
MAX_BIO_TEXT: Final = 20_000
MAX_PHOTO: Final = 4 * 1024 * 1024
# The picture formats a browser shows safely, by their first bytes. Never SVG,
# which can carry script.
_PICTURES: Final = (
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
)

_CALL: Final = re.compile(r"^[A-Z0-9]+(/[A-Z0-9]+)*$")
_GRID: Final = re.compile(r"^[A-R]{2}[0-9]{2}([A-X]{2}([0-9]{2})?)?$", re.IGNORECASE)
_MAX_CALL: Final = 20
MAX_URL: Final = 300
# A host name with a dot, of letters, digits and hyphens: never an address or "localhost".
_HOST: Final = re.compile(r"^(?=.*[a-z])([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$")
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
    image: str | None = None
    bio: bool = False
    bio_date: str | None = None

    def to_json(self) -> dict[str, str]:
        """The entry as a JSON object, leaving out empty values."""
        values = {
            "call": self.call,
            "name": self.name,
            "qth": self.qth,
            "state": self.state,
            "country": self.country,
            "grid": self.grid,
            "image": self.image,
            # Always written, so an entry kept before biographies were noted can be told apart.
            "bio": "yes" if self.bio else "no",
            "biodate": self.bio_date,
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


def _clean_image(value: str | None) -> str | None:
    """A photo's address, kept only if it is plain HTTPS to a public host name."""
    text = clean_text(value, MAX_URL)
    if text is None:
        return None
    try:
        parts = urllib.parse.urlsplit(text)
        port = parts.port
    except ValueError:
        return None
    host = parts.hostname or ""
    if parts.scheme != "https" or parts.username or parts.password or port is not None:
        return None
    if _HOST.match(host) is None:
        return None
    return text


def _has_bio(value: str | None) -> bool:
    """Whether the record says there is a biography: QRZ.com gives its size, or "yes" when kept."""
    text = clean_text(value) or ""
    return text == "yes" or (text.isdigit() and int(text) > 0)


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
        image=_clean_image(text("image")),
        bio=_has_bio(text("bio")),
        bio_date=clean_text(text("biodate"), 32),
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


def http_fetch_large(url: str, limit: int) -> bytes:
    """GET ``url`` over HTTPS, as :func:`http_fetch` does, allowing up to ``limit`` bytes.

    Raises:
        QrzUnreachableError: It could not be fetched.
        QrzReplyError: The answer is larger than ``limit``.
    """
    if not url.startswith("https://"):
        raise QrzUnreachableError("only HTTPS is used")
    request = urllib.request.Request(url, headers={"Accept": "*/*"})  # noqa: S310 - https only
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:  # noqa: S310 - https only
            body: bytes = response.read(limit + 1)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        reason = getattr(error, "reason", error)
        raise QrzUnreachableError(f"QRZ.com could not be reached ({reason})") from error
    if len(body) > limit:
        raise QrzReplyError(f"the answer is larger than {limit} bytes")
    return body


def picture_type(body: bytes) -> str | None:
    """The picture's media type from its first bytes, if it is one a page may show."""
    for magic, media_type in _PICTURES:
        if body.startswith(magic):
            return media_type
    if body[:4] == b"RIFF" and body[8:12] == b"WEBP":
        return "image/webp"
    return None


class _BioText(HTMLParser):
    """The words of a biography page, with its paragraphs, and nothing else."""

    _BREAKS: Final = frozenset(
        {
            "p",
            "div",
            "br",
            "li",
            "tr",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "table",
            "ul",
            "ol",
            "blockquote",
            "hr",
            "section",
            "article",
            "center",
            "pre",
        }
    )
    _SKIPPED: Final = frozenset({"script", "style", "head", "title", "noscript", "template"})

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self._skipping = 0

    @override
    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in self._SKIPPED:
            self._skipping += 1
        elif tag in self._BREAKS:
            self.parts.append("\n")

    @override
    def handle_endtag(self, tag: str) -> None:
        if tag in self._SKIPPED:
            self._skipping = max(0, self._skipping - 1)
        elif tag in self._BREAKS:
            self.parts.append("\n")

    @override
    def handle_data(self, data: str) -> None:
        if not self._skipping:
            self.parts.append(data)


def bio_text(page: str) -> str | None:
    """A biography page as plain text: paragraphs of cleaned words, capped in length.

    Only words are kept. No markup, link, picture, or script from the page
    survives, so the page can show it as text and nothing else (Principle XII).
    """
    reader = _BioText()
    reader.feed(page)
    reader.close()
    paragraphs = [clean_text(p, MAX_BIO_TEXT) for p in "".join(reader.parts).split("\n")]
    text = "\n".join(p for p in paragraphs if p)[:MAX_BIO_TEXT].strip()
    return text or None


class QrzClient:
    """Logs in to QRZ.com and looks up calls, through ``fetch``."""

    def __init__(
        self,
        agent: str,
        fetch: Callable[[str], bytes] = http_fetch,
        fetch_large: Callable[[str, int], bytes] = http_fetch_large,
    ) -> None:
        """Identify as ``agent`` (QRZ.com asks every program to); fetch with ``fetch``.

        ``fetch_large`` fetches biography pages and photos, which can be larger.
        """
        self._agent = agent
        self._fetch = fetch
        self._fetch_large = fetch_large

    def _ask(self, query: dict[str, str]) -> QrzReply:
        return parse_reply(self._fetch(f"{BASE_URL}?{urllib.parse.urlencode(query)}"))

    def login(self, username: str, password: str) -> str:
        """Log in and return the session key.

        Raises:
            QrzLoginError: QRZ.com refused the login.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        return self.sign_in(username, password)[0]

    def sign_in(self, username: str, password: str) -> tuple[str, str | None]:
        """Log in; return the session key and QRZ.com's note, such as a subscription notice.

        Raises:
            QrzLoginError: QRZ.com refused the login, with QRZ.com's own reason.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        reply = self._ask({"username": username, "password": password, "agent": self._agent})
        if reply.key is None:
            raise QrzLoginError(reply.error or "QRZ.com did not accept the login")
        return reply.key, reply.message

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

    def biography(self, key: str, call: str) -> str | None:
        """The biography ``call`` wrote on QRZ.com, as plain text; ``None`` if it has none.

        QRZ.com sends it as a web page inside the opening of its XML answer
        (so not well-formed XML), or as a plain XML answer when it cannot.

        Raises:
            QrzSessionError: The key has expired; log in again.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read, or reported an error.
        """
        query = urllib.parse.urlencode({"s": key, "html": call})
        body = self._fetch_large(f"{BASE_URL}?{query}", MAX_BIO_PAGE)
        if b"<html" not in body[:4096].lower():
            if len(body) > MAX_REPLY:
                raise QrzReplyError("QRZ.com's answer is not a biography")
            reply = parse_reply(body)
            if reply.key is None:
                raise QrzSessionError(reply.error or "the QRZ.com session ended")
            if reply.error is not None and reply.error.lower().startswith("not found"):
                return None
            raise QrzReplyError(reply.error or "QRZ.com sent no biography")
        try:
            page = body.decode("utf-8")
        except UnicodeDecodeError:
            page = body.decode("cp1252", errors="replace")  # older biographies say ISO-8859-1
        return bio_text(page)

    def photo(self, url: str) -> tuple[bytes, str]:
        """The photo at ``url`` (an address from a callsign record) and its media type.

        Raises:
            QrzUnreachableError: It could not be fetched.
            QrzReplyError: It is too large, or not a JPEG, PNG, GIF or WebP picture.
        """
        if _clean_image(url) != url:
            raise QrzReplyError("the photo's address is not plain HTTPS")
        body = self._fetch_large(url, MAX_PHOTO)
        media_type = picture_type(body)
        if media_type is None:
            raise QrzReplyError("the photo is not a JPEG, PNG, GIF or WebP picture")
        return body, media_type
