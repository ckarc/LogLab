# SPDX-FileCopyrightText: 2026 LabLog contributors
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
    bio_text,
    parse_reply,
    picture_type,
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

    return QrzClient("LabLog-test", fetch), asked


def test_login_sends_the_agent_and_refuses_a_bad_password() -> None:
    """QRZ.com asks each program to name itself; a refusal is a login error."""
    client, asked = _client(
        LOGIN,
        b"<QRZDatabase><Session><Error>Username/password incorrect</Error></Session></QRZDatabase>",
    )
    assert client.login("k9ksu", "pw").startswith("2331")
    assert asked[0].startswith("https://xmldata.qrz.com/xml/current/?")
    assert "agent=LabLog-test" in asked[0]
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


# What QRZ.com sent for a biography on 2026-10-19: the page, after the opening
# of an XML answer that is never closed. The words here are made up.
BIO_PAGE = b"""<?xml version="1.0" encoding="utf-8" ?>
<QRZDatabase version="1.36" xmlns="http://xmldata.qrz.com">
<!DOCTYPE html
\tPUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN"
\t "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" lang="en-US" xml:lang="en-US">
<head><title>Untitled Document</title><style>p { color: red }</style></head>
<body>
<p>Welcome to the &amp; ghost   station.</p>
<script>alert("never shown")</script>
<div>QSL via <a href="https://example.com/">the bureau</a><br>73!</div>
<img src="https://example.com/x.png" onerror="alert(1)">
</body></html>
"""


def test_a_record_notes_its_photo_and_whether_it_has_a_biography() -> None:
    """QRZ.com gives the photo's address and the biography's size, not its words."""
    body = RECORD.replace(
        b"<grid>",
        b"<image>https://cdn-xml.qrz.com/u/w1aw/station.jpg</image><bio>9070</bio><grid>",
    )
    entry = parse_reply(body).entry
    assert entry is not None
    assert entry.image == "https://cdn-xml.qrz.com/u/w1aw/station.jpg"
    assert entry.bio is True
    assert CallbookEntry.from_json(entry.to_json()) == entry
    plain = parse_reply(RECORD).entry
    assert plain is not None
    assert (plain.image, plain.bio) == (None, False)


@pytest.mark.parametrize(
    "image",
    [
        "http://cdn-xml.qrz.com/u/w1aw/a.jpg",
        "https://127.0.0.1/a.jpg",
        "https://localhost/a.jpg",
        "https://cdn-xml.qrz.com:8443/a.jpg",
        "https://user:pw@cdn-xml.qrz.com/a.jpg",
        "javascript:alert(1)",
        "file:///etc/passwd",
    ],
)
def test_a_photo_address_that_is_not_plain_https_to_a_named_host_is_dropped(image: str) -> None:
    """The Engine never fetches a photo from a local address or another scheme (Principle XII)."""
    body = RECORD.replace(b"<grid>", f"<image>{image}</image><grid>".encode())
    entry = parse_reply(body).entry
    assert entry is not None
    assert entry.image is None


def test_a_biography_keeps_only_its_words() -> None:
    """No markup, script, style, link or picture survives; paragraphs do."""
    assert bio_text(BIO_PAGE.decode()) == "Welcome to the & ghost station.\nQSL via the bureau\n73!"
    assert bio_text("<html><body><script>x</script></body></html>") is None


def _large(*answers: bytes) -> tuple[QrzClient, list[tuple[str, int]]]:
    asked: list[tuple[str, int]] = []
    queue = list(answers)

    def fetch_large(url: str, limit: int) -> bytes:
        asked.append((url, limit))
        return queue.pop(0)

    def fetch(url: str) -> bytes:
        raise AssertionError(url)

    return QrzClient("LabLog-test", fetch, fetch_large), asked


def test_biography_reads_the_page_and_tells_an_ended_session() -> None:
    """The html request gives the page; an XML answer means none, or a session to renew."""
    client, asked = _large(
        BIO_PAGE,
        b"<QRZDatabase><Session><Error>Not found: XX9XX</Error><Key>abc</Key>"
        b"</Session></QRZDatabase>",
        b"<QRZDatabase><Session><Error>Session Timeout</Error></Session></QRZDatabase>",
    )
    assert (
        client.biography("abc", "W1AW")
        == "Welcome to the & ghost station.\nQSL via the bureau\n73!"
    )
    assert "html=W1AW" in asked[0][0]
    assert client.biography("abc", "XX9XX") is None
    with pytest.raises(QrzSessionError):
        client.biography("abc", "W1AW")


JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF" + b"\x00" * 40


@pytest.mark.parametrize(
    ("body", "media_type"),
    [
        (JPEG, "image/jpeg"),
        (b"\x89PNG\r\n\x1a\n" + b"\x00" * 20, "image/png"),
        (b"GIF89a" + b"\x00" * 20, "image/gif"),
        (b"RIFF\x00\x00\x00\x00WEBPVP8 ", "image/webp"),
        (b'<svg xmlns="http://www.w3.org/2000/svg"><script/></svg>', None),
        (b"<html>not a picture</html>", None),
    ],
)
def test_only_pictures_a_page_shows_safely_are_photos(body: bytes, media_type: str | None) -> None:
    """A photo is known by its first bytes, never by what the server calls it; never SVG."""
    assert picture_type(body) == media_type


def test_a_photo_is_fetched_only_from_a_plain_https_address_and_must_be_a_picture() -> None:
    """Anything else fails closed (Principle XII)."""
    client, asked = _large(JPEG, b"<svg/>")
    assert client.photo("https://cdn-xml.qrz.com/u/w1aw/a.jpg") == (JPEG, "image/jpeg")
    with pytest.raises(QrzReplyError):
        client.photo("https://cdn-xml.qrz.com/u/w1aw/b.svg")
    with pytest.raises(QrzReplyError):
        client.photo("https://127.0.0.1/a.jpg")
    assert len(asked) == 2
