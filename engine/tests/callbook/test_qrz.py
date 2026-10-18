# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading QRZ.com's answers: untrusted input, validated where it enters (Principle XII)."""

from __future__ import annotations

import pytest

from loglab_engine.callbook import (
    CallbookEntry,
    QrzClient,
    QrzLoginError,
    QrzReplyError,
    QrzSessionError,
    parse_reply,
)
from loglab_engine.callbook.qrz import MAX_REPLY, clean_text

LOGIN = b"""<?xml version="1.0" encoding="utf-8" ?>
<QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
  <Session>
    <Key>2331uf894c4bd29f3923f3bacf02c532d7bd9</Key>
    <Count>123</Count>
    <SubExp>Wed Jan 1 12:34:03 2027</SubExp>
  </Session>
</QRZDatabase>
"""

RECORD = b"""<?xml version="1.0" encoding="utf-8" ?>
<QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
  <Callsign>
    <call>w1aw</call>
    <fname>Hiram  Percy</fname>
    <name>Maxim</name>
    <addr1>225 Main St</addr1>
    <addr2>Newington</addr2>
    <state>CT</state>
    <country>United States</country>
    <grid>fn31PR</grid>
  </Callsign>
  <Session><Key>abc</Key></Session>
</QRZDatabase>
"""


def test_a_login_answer_gives_the_session_key() -> None:
    """The namespace QRZ.com uses does not get in the way."""
    reply = parse_reply(LOGIN)
    assert reply.key == "2331uf894c4bd29f3923f3bacf02c532d7bd9"
    assert reply.entry is None
    assert reply.error is None


def test_a_record_fills_name_qth_state_country_and_grid() -> None:
    """Name is first and last together; QTH is the city; the grid is written properly."""
    assert parse_reply(RECORD).entry == CallbookEntry(
        call="W1AW",
        name="Hiram Percy Maxim",
        qth="Newington",
        state="CT",
        country="United States",
        grid="FN31pr",
    )


def test_a_grid_that_is_not_a_locator_is_dropped() -> None:
    """A value that is not Maidenhead never reaches the form (Principle XII)."""
    body = RECORD.replace(b"fn31PR", b"ZZ99<b/>")
    entry = parse_reply(body).entry
    assert entry is not None
    assert entry.grid is None
    assert entry.name == "Hiram Percy Maxim"


def test_control_characters_and_long_values_are_cleaned() -> None:
    """Every value kept is printable, single-spaced, and capped."""
    assert clean_text("A\x00B\tC\n  D") == "A B C D"
    assert clean_text("x" * 500) == "x" * 64
    assert clean_text("   ") is None


@pytest.mark.parametrize(
    "body",
    [
        b'<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><QRZDatabase>&a;</QRZDatabase>',
        b"<QRZDatabase>" + b" " * MAX_REPLY + b"</QRZDatabase>",
        b"<QRZDatabase><Session>",
        b"<html><body>Service unavailable</body></html>",
        "<QRZDatabase/>".encode("utf-16"),
        b"<QRZDatabase><Callsign><call>not a call!</call></Callsign></QRZDatabase>",
    ],
    ids=["entities", "oversized", "malformed", "not-qrz", "utf-16", "bad-call"],
)
def test_unusable_answers_are_refused(body: bytes) -> None:
    """Anything QRZ.com's interface does not describe fails closed."""
    with pytest.raises(QrzReplyError):
        parse_reply(body)


def _client(*answers: bytes) -> tuple[QrzClient, list[str]]:
    asked: list[str] = []
    queue = list(answers)

    def fetch(url: str) -> bytes:
        asked.append(url)
        return queue.pop(0)

    return QrzClient("LogLab-test", fetch), asked


def test_login_sends_the_agent_and_refuses_a_bad_password() -> None:
    """QRZ.com asks each program to name itself; a refusal is a login error."""
    client, asked = _client(
        LOGIN,
        b"<QRZDatabase><Session><Error>Username/password incorrect</Error></Session></QRZDatabase>",
    )
    assert client.login("k9ksu", "pw").startswith("2331")
    assert asked[0].startswith("https://xmldata.qrz.com/xml/current/?")
    assert "agent=LogLab-test" in asked[0]
    with pytest.raises(QrzLoginError, match="incorrect"):
        client.login("k9ksu", "wrong")


def test_lookup_tells_not_found_from_an_ended_session() -> None:
    """A session part with a key and "Not found" is an answer; one without a key is not."""
    client, _ = _client(
        RECORD,
        b"<QRZDatabase><Session><Error>Not found: XX9XX</Error><Key>abc</Key>"
        b"</Session></QRZDatabase>",
        b"<QRZDatabase><Session><Error>Session Timeout</Error></Session></QRZDatabase>",
    )
    entry, _ = client.lookup("abc", "W1AW")
    assert entry is not None
    assert client.lookup("abc", "XX9XX") == (None, None)
    with pytest.raises(QrzSessionError):
        client.lookup("abc", "W1AW")
