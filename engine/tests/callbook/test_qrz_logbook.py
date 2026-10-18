# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading QRZ Logbook API answers: untrusted input (Principle XII)."""

from __future__ import annotations

import pytest

from loglab_engine.callbook import QrzReplyError
from loglab_engine.callbook.qrz import MAX_REPLY
from loglab_engine.callbook.qrz_logbook import (
    InsertOutcome,
    QrzKeyError,
    QrzLogbookClient,
    parse_answer,
)


def test_an_answer_is_read_and_cleaned() -> None:
    """Names are upper-cased; values are decoded and stripped of control characters."""
    answer = parse_answer(b"RESULT=FAIL&reason=Bad+date%0A%07range&COUNT=0")
    assert answer == {"RESULT": "FAIL", "REASON": "Bad date range", "COUNT": "0"}


@pytest.mark.parametrize(
    "body",
    [b"COUNT=1", b"RESULT=OK" + b"&X=1" * MAX_REPLY, b"\xff\xfe=1"],
    ids=["no-result", "oversized", "not-utf8"],
)
def test_unusable_answers_are_refused(body: bytes) -> None:
    """Anything the developer guide does not describe fails closed."""
    with pytest.raises(QrzReplyError):
        parse_answer(body)


def _client(answer: bytes) -> QrzLogbookClient:
    return QrzLogbookClient("LogLab/test", lambda _url, _body, _agent: answer)


def test_insert_reads_logid_either_way_the_guide_writes_it() -> None:
    """The guide documents LOGID; its own example answers with LOGIDS."""
    assert _client(b"RESULT=OK&LOGID=7&COUNT=1").insert("k", "<call:4>W1AW<eor>").logid == 7
    assert _client(b"RESULT=OK&LOGIDS=8&COUNT=1").insert("k", "x").logid == 8
    replaced = _client(b"RESULT=REPLACE&LOGID=9").insert("k", "x", replace=True)
    assert replaced.outcome is InsertOutcome.REPLACED
    refused = _client(b"RESULT=FAIL&REASON=duplicate").insert("k", "x")
    assert (refused.outcome, refused.logid, refused.reason) == (
        InsertOutcome.REFUSED,
        None,
        "duplicate",
    )


def test_a_refused_key_is_an_error_not_a_refused_qso() -> None:
    """AUTH, or an invalid key, stops sending instead of marking QSOs refused."""
    with pytest.raises(QrzKeyError):
        _client(b"RESULT=AUTH&REASON=no+privileges").insert("k", "x")
    with pytest.raises(QrzKeyError):
        _client(b"RESULT=FAIL&REASON=invalid+api+key").insert("k", "x")
    with pytest.raises(QrzKeyError):
        _client(b"RESULT=FAIL&REASON=invalid+api+key").status("k")
