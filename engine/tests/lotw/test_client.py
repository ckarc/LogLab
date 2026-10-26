# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading LoTW's answers, which are untrusted input (Principle XII)."""

from __future__ import annotations

import pytest

from loglab_engine.lotw import (
    LotwLoginError,
    LotwRejectedError,
    LotwReplyError,
    LotwUnreachableError,
    https,
    read_report,
    read_upload_reply,
)

from .fake import report


def test_an_accepted_upload_returns_lotw_s_message() -> None:
    """The result is in comments TrustedQSL's published source reads."""
    page = (
        b"<html><!-- .UPL.  accepted -->\n<!-- .UPLMESSAGE. File queued for processing --></html>"
    )
    assert read_upload_reply(page) == "File queued for processing"


def test_a_refused_upload_says_why() -> None:
    """LoTW's own message is passed on, cleaned."""
    page = b"<!-- .UPL. rejected --><!-- .UPLMESSAGE. Certificate\x07 not valid -->"
    with pytest.raises(LotwRejectedError, match="Certificate not valid"):
        read_upload_reply(page)


def test_a_page_with_no_result_is_not_taken_as_accepted() -> None:
    """Anything else is not understood, never success."""
    with pytest.raises(LotwReplyError):
        read_upload_reply(b"<html>Service unavailable</html>")


def test_a_report_lists_confirmations_and_the_date_to_ask_from_next() -> None:
    """The report is read by the Engine's own ADIF reader."""
    found = read_report(report({"CALL": "W1AW", "QSL_RCVD": "Y"}, last_qsl="2026-10-18 01:02:03"))
    assert [r.get("CALL") for r in found.records] == ["W1AW"]
    assert found.last_qsl == "2026-10-18 01:02:03"


def test_a_refused_login_and_a_strange_answer_are_told_apart() -> None:
    """LoTW answers a web page, not ADIF, when the login is wrong."""
    with pytest.raises(LotwLoginError):
        read_report(b"<html>Username/password incorrect</html>")
    with pytest.raises(LotwReplyError, match="not a confirmation report"):
        read_report(b"<html>Down for maintenance</html>")
    with pytest.raises(LotwReplyError, match="could not be read"):
        read_report(b"Report\n<eoh><CALL:99>W1AW")


def test_only_https_is_used() -> None:
    """No request leaves the Engine unencrypted with a password in it."""
    with pytest.raises(LotwUnreachableError, match="HTTPS"):
        https("http://lotw.arrl.org/", None, {}, 10, 1.0)
