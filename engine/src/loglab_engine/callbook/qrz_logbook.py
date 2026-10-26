# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The QRZ Logbook API, from QRZ.com's published developer guide.

One address takes form-encoded POSTs with the logbook's API key and an action
(``STATUS``, ``INSERT``, ``DELETE``) and answers with ``NAME=value`` pairs
joined by ``&``. The key alone chooses the logbook. Answers are untrusted
(Principle XII): size-capped, and every value kept is cleaned and capped.
Failing to reach QRZ.com is a :class:`QrzUnreachableError`; callers keep the
work for later (Principle V).
"""

from __future__ import annotations

import re
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Final

from loglab_engine.callbook.qrz import (
    MAX_MESSAGE,
    MAX_REPLY,
    TIMEOUT_SECONDS,
    QrzError,
    QrzReplyError,
    QrzUnreachableError,
    clean_text,
)

if TYPE_CHECKING:
    from collections.abc import Callable

LOGBOOK_URL: Final = "https://logbook.qrz.com/api"
_LOGID: Final = re.compile(r"^[0-9]{1,19}$")
_MAX_PAIRS: Final = 64


class QrzKeyError(QrzError):
    """QRZ.com refused the API key, or it lacks the needed privileges."""


class InsertOutcome(StrEnum):
    """What QRZ.com did with a QSO it was sent."""

    INSERTED = "inserted"
    REFUSED = "refused"


@dataclass(frozen=True, slots=True)
class InsertResult:
    """QRZ.com's answer to an insert: what it did, the QSO's logid there, and why if refused."""

    outcome: InsertOutcome
    logid: int | None
    reason: str | None


def http_post(url: str, body: bytes, agent: str) -> bytes:
    """POST a form-encoded ``body`` to ``url`` over HTTPS as ``agent``.

    Raises:
        QrzUnreachableError: It could not be sent, or QRZ.com answered with an HTTP error.
        QrzReplyError: The answer is larger than :data:`MAX_REPLY`.
    """
    if not url.startswith("https://"):
        raise QrzUnreachableError("only HTTPS is used")
    request = urllib.request.Request(  # noqa: S310 - https only
        url,
        data=body,
        method="POST",
        headers={"Content-Type": "application/x-www-form-urlencoded", "User-Agent": agent},
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:  # noqa: S310 - https only
            answer: bytes = response.read(MAX_REPLY + 1)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        reason = getattr(error, "reason", error)
        raise QrzUnreachableError(f"QRZ.com could not be reached ({reason})") from error
    if len(answer) > MAX_REPLY:
        raise QrzReplyError(f"the answer is larger than {MAX_REPLY} bytes")
    return answer


def parse_answer(body: bytes) -> dict[str, str]:
    """Read a ``NAME=value&NAME=value`` answer; names are upper-cased, values cleaned.

    Raises:
        QrzReplyError: It is too large, not text, or has no ``RESULT``.
    """
    if len(body) > MAX_REPLY:
        raise QrzReplyError(f"the answer is larger than {MAX_REPLY} bytes")
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError as error:
        raise QrzReplyError("the answer is not UTF-8 text") from error
    pairs = urllib.parse.parse_qsl(text.strip(), keep_blank_values=True, max_num_fields=_MAX_PAIRS)
    answer: dict[str, str] = {}
    for name, value in pairs:
        key = name.strip().upper()
        if key and key not in answer:
            answer[key] = clean_text(value, MAX_MESSAGE) or ""
    if "RESULT" not in answer:
        raise QrzReplyError("the answer has no RESULT")
    return answer


def _logid(value: str | None) -> int | None:
    if value is None:
        return None
    first = value.split(",", 1)[0].strip()
    return int(first) if _LOGID.match(first) else None


class QrzLogbookClient:
    """Talks to one QRZ logbook at a time, by its API key, through ``post``."""

    def __init__(self, agent: str, post: Callable[[str, bytes, str], bytes] = http_post) -> None:
        """Identify as ``agent`` (QRZ.com requires a named program); send with ``post``."""
        self._agent = agent
        self._post = post

    def _ask(self, fields: dict[str, str]) -> dict[str, str]:
        body = urllib.parse.urlencode(fields).encode("ascii")
        answer = parse_answer(self._post(LOGBOOK_URL, body, self._agent))
        if answer["RESULT"].upper() == "AUTH":
            raise QrzKeyError(answer.get("REASON") or "the API key lacks the privileges needed")
        return answer

    def status(self, key: str) -> None:
        """Check that QRZ.com accepts ``key``.

        Raises:
            QrzKeyError: It does not.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        answer = self._ask({"KEY": key, "ACTION": "STATUS"})
        if answer["RESULT"].upper() != "OK":
            raise QrzKeyError(answer.get("REASON") or "QRZ.com did not accept the API key")

    def insert(self, key: str, adif: str) -> InsertResult:
        """Add one ADIF record to the logbook. A duplicate is refused, never overwritten.

        Raises:
            QrzKeyError: QRZ.com refused the key.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        # No OPTION=REPLACE, ever: it would overwrite a QSO LabLog did not send.
        answer = self._ask({"KEY": key, "ACTION": "INSERT", "ADIF": adif})
        result = answer["RESULT"].upper()
        # The guide documents LOGID; its own example answers with LOGIDS.
        logid = _logid(answer.get("LOGID") or answer.get("LOGIDS"))
        if result == "OK":
            return InsertResult(InsertOutcome.INSERTED, logid, None)
        if result == "FAIL" and "invalid api key" in answer.get("REASON", "").lower():
            raise QrzKeyError(answer["REASON"])
        return InsertResult(InsertOutcome.REFUSED, None, answer.get("REASON") or result)

    def delete(self, key: str, logid: int) -> bool:
        """Remove one QSO from the logbook; ``False`` if QRZ.com no longer had it.

        Raises:
            QrzKeyError: QRZ.com refused the key.
            QrzUnreachableError: QRZ.com could not be reached.
            QrzReplyError: The answer could not be read.
        """
        answer = self._ask({"KEY": key, "ACTION": "DELETE", "LOGIDS": str(logid)})
        return answer["RESULT"].upper() == "OK"
