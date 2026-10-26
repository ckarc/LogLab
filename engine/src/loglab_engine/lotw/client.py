# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Talking to Logbook of the World: sending a signed file, and asking for confirmations.

Two published interfaces at ``lotw.arrl.org``:

* Upload: an HTTPS POST of the ``.tq8`` file as the form field ``upfile`` to
  ``/lotw/upload``. The answer is a web page carrying the result in comments,
  ``<!-- .UPL. accepted -->`` (or ``rejected``) and ``<!-- .UPLMESSAGE. ... -->``
  (the values TrustedQSL's published source reads).
* Confirmations: an HTTPS GET of ``/lotwuser/lotwreport.adi`` with the
  account's login, which answers an ADIF file of the QSLs recorded since a
  date. A refused login answers a web page instead.

Everything that comes back is untrusted (Principle XII): it is size-capped, an
ADIF answer goes through the Engine's ADIF reader, and messages are cleaned
and cut short. Every failure to reach LoTW is a :class:`LotwUnreachableError`,
which callers treat as "not this time" (Principle V).
"""

from __future__ import annotations

import re
import secrets
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final, Protocol

from loglab_engine.adif import AdiParseError, ParseLimits, Record, parse_adi
from loglab_engine.callbook.qrz import clean_text

if TYPE_CHECKING:
    from datetime import date

UPLOAD_URL: Final = "https://lotw.arrl.org/lotw/upload"
REPORT_URL: Final = "https://lotw.arrl.org/lotwuser/lotwreport.adi"
UPLOAD_TIMEOUT: Final = 60.0
REPORT_TIMEOUT: Final = 120.0
MAX_UPLOAD_REPLY: Final = 256 * 1024
MAX_REPORT: Final = 64 * 1024 * 1024
MAX_MESSAGE: Final = 300
_STATUS: Final = re.compile(rb"<!-- \.UPL\.\s*([^-]+?)\s*-->")
_MESSAGE: Final = re.compile(rb"<!-- \.UPLMESSAGE\.\s*(.+?)\s*-->", re.DOTALL)
_REPORT_LIMITS: Final = ParseLimits(
    max_document_bytes=MAX_REPORT,
    max_value_bytes=4096,
    max_fields_per_record=200,
    max_records=1_000_000,
)


class LotwError(Exception):
    """LoTW could not do what was asked."""


class LotwUnreachableError(LotwError):
    """LoTW could not be reached (no internet, a timeout, or an HTTP error)."""


class LotwReplyError(LotwError):
    """LoTW's answer was too large, malformed, or not what the interface describes."""


class LotwLoginError(LotwError):
    """LoTW refused the username and password."""


class LotwRejectedError(LotwError):
    """LoTW received the file and refused it; the message is LoTW's."""


@dataclass(frozen=True, slots=True)
class Report:
    """The confirmations LoTW sent, and the date to ask from next time."""

    records: tuple[Record, ...]
    last_qsl: str | None


class Transport(Protocol):
    """How the Engine makes an HTTPS request; tests give a stand-in."""

    def __call__(
        self, url: str, body: bytes | None, headers: dict[str, str], limit: int, timeout: float
    ) -> bytes:
        """Send the request (a POST when ``body`` is given) and return at most ``limit`` bytes."""
        ...


def https(
    url: str, body: bytes | None, headers: dict[str, str], limit: int, timeout: float
) -> bytes:
    """Make one HTTPS request.

    Raises:
        LotwUnreachableError: It could not be made.
        LotwReplyError: The answer is larger than ``limit``.
    """
    if not url.startswith("https://"):
        raise LotwUnreachableError("only HTTPS is used")
    request = urllib.request.Request(url, data=body, headers=headers)  # noqa: S310 - https only
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310 - https only
            answer: bytes = response.read(limit + 1)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        reason = getattr(error, "reason", error)
        raise LotwUnreachableError(f"LoTW could not be reached ({reason})") from error
    if len(answer) > limit:
        raise LotwReplyError(f"LoTW's answer is larger than {limit} bytes")
    return answer


def _clean(raw: bytes) -> str:
    return clean_text(raw.decode("utf-8", "replace"), MAX_MESSAGE) or ""


def read_upload_reply(page: bytes) -> str:
    """LoTW's message when it accepted an upload.

    Raises:
        LotwRejectedError: LoTW refused the file.
        LotwReplyError: The page carries no result.
    """
    status = _STATUS.search(page)
    found = _MESSAGE.search(page)
    message = _clean(found.group(1)) if found else ""
    if status is None:
        raise LotwReplyError("LoTW's answer did not say whether it accepted the file")
    if _clean(status.group(1)).lower() != "accepted":
        raise LotwRejectedError(message or "LoTW refused the file")
    return message


def read_report(body: bytes) -> Report:
    """The confirmations in a ``lotwreport.adi`` answer.

    Raises:
        LotwLoginError: LoTW answered with its refused-login page.
        LotwReplyError: The answer is not an ADIF report.
    """
    if b"<eoh>" not in body[:65536].lower():
        if b"password incorrect" in body.lower():
            raise LotwLoginError("LoTW refused this username and password")
        raise LotwReplyError("LoTW's answer is not a confirmation report")
    try:
        document = parse_adi(body, _REPORT_LIMITS).document
    except AdiParseError as error:
        raise LotwReplyError(f"LoTW's report could not be read: {error}") from error
    last = None if document.header is None else document.header.get("APP_LoTW_LASTQSL")
    return Report(document.records, clean_text(last, 19))


class LotwClient:
    """Sends signed files to LoTW and fetches confirmation reports, through ``transport``."""

    def __init__(self, transport: Transport = https) -> None:
        """Make requests with ``transport``."""
        self._transport = transport

    def upload(self, tq8: bytes) -> str:
        """Send a signed ``.tq8`` file; return LoTW's message.

        Raises:
            LotwRejectedError: LoTW refused the file.
            LotwUnreachableError: LoTW could not be reached.
            LotwReplyError: The answer could not be read.
        """
        boundary = f"loglab-{secrets.token_hex(16)}"
        body = b"".join(
            (
                f"--{boundary}\r\n".encode(),
                b'Content-Disposition: form-data; name="upfile"; filename="loglab.tq8"\r\n',
                b"Content-Type: application/octet-stream\r\n\r\n",
                tq8,
                f"\r\n--{boundary}--\r\n".encode(),
            )
        )
        headers = {"Content-Type": f"multipart/form-data; boundary={boundary}"}
        page = self._transport(UPLOAD_URL, body, headers, MAX_UPLOAD_REPLY, UPLOAD_TIMEOUT)
        return read_upload_reply(page)

    def report(
        self, username: str, password: str, since: date | str, own_call: str | None
    ) -> Report:
        """Ask for the QSLs LoTW has recorded since ``since``.

        Raises:
            LotwLoginError: LoTW refused the login.
            LotwUnreachableError: LoTW could not be reached.
            LotwReplyError: The answer could not be read.
        """
        query = {
            "login": username,
            "password": password,
            "qso_query": "1",
            "qso_qsl": "yes",
            "qso_qslsince": str(since),
            "qso_mydetail": "yes",
        }
        if own_call is not None:
            query["qso_owncall"] = own_call
        url = f"{REPORT_URL}?{urllib.parse.urlencode(query)}"
        return read_report(self._transport(url, None, {}, MAX_REPORT, REPORT_TIMEOUT))
