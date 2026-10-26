# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Lossless round-trip properties (Principle IV) and fuzzing of the reader (Principle XII)."""

from __future__ import annotations

import contextlib

from hypothesis import given, settings
from hypothesis import strategies as st

from loglab_engine.adif import (
    AdifDocument,
    AdiParseError,
    Field,
    Header,
    Record,
    differences,
    parse_adi,
    write_adi,
)

from .test_adi_parse import SPEC_SAMPLE

_NAME_CHARS = "".join(
    chr(c) for c in range(33, 127) if chr(c) not in ",:<>{}"
)  # printable ASCII except forbidden characters; spaces only inside
_name_core = st.text(alphabet=_NAME_CHARS, min_size=1, max_size=24)
names = st.one_of(
    _name_core,
    st.builds(lambda a, b: f"{a} {b}", _name_core, _name_core),
).filter(lambda n: n.upper() not in {"EOH", "EOR"})
indicators = st.one_of(st.none(), st.sampled_from("BDNTSIMGELbdnts"))
# Any text at all: ASCII, markup-like strings, CR/LF, and out-of-spec Unicode.
values = st.one_of(
    st.text(max_size=60),
    st.text(alphabet="<>:EORHeoh0123456789 \r\n", max_size=40),
)
fields = st.builds(Field, names, values, indicators)
records = st.builds(Record, st.lists(fields, min_size=1, max_size=8).map(tuple))
preambles = st.text(min_size=1, max_size=60).filter(lambda t: "<" not in t)
headers = st.one_of(
    st.none(),
    st.builds(Header, preambles, st.lists(fields, max_size=5).map(tuple)),
)
documents = st.builds(AdifDocument, headers, st.lists(records, max_size=6).map(tuple))


@settings(max_examples=500)
@given(documents)
def test_write_then_parse_returns_the_same_document(document: AdifDocument) -> None:
    """Every field survives, in order, with name, data, and indicator unchanged."""
    parsed = parse_adi(write_adi(document)).document
    assert parsed == document
    assert differences(document, parsed) == []


@settings(max_examples=200)
@given(documents)
def test_writer_output_is_a_fixed_point(document: AdifDocument) -> None:
    """Re-exporting an imported export reproduces the same bytes."""
    once = write_adi(document)
    assert write_adi(parse_adi(once).document) == once


@settings(max_examples=1000)
@given(st.binary(max_size=300))
def test_arbitrary_bytes_parse_or_fail_closed(data: bytes) -> None:
    """Garbage input never crashes the reader with anything but AdiParseError."""
    with contextlib.suppress(AdiParseError):
        parse_adi(data)


@settings(max_examples=500)
@given(st.lists(st.sampled_from([b"<", b">", b":", b"EOR", b"EOH", b"CALL", b"3", b"0", b"x"])))
def test_markup_fragments_parse_or_fail_closed(parts: list[bytes]) -> None:
    """Near-miss markup, which random bytes rarely hit, also fails closed."""
    with contextlib.suppress(AdiParseError):
        parse_adi(b"".join(parts))


def test_spec_sample_round_trips() -> None:
    """The specification's sample survives import, export, and re-import."""
    original = parse_adi(SPEC_SAMPLE).document
    assert parse_adi(write_adi(original)).document == original


def test_differences_reports_each_change() -> None:
    """The comparison names what changed and where."""
    before = AdifDocument(
        Header("h\n", (Field("ADIF_VER", "3.1.7"),)),
        (Record((Field("CALL", "Q1A"), Field("BAND", "20m"))),),
    )
    after = AdifDocument(
        Header("h\n", (Field("ADIF_VER", "3.1.6"),)),
        (Record((Field("CALL", "Q1A"), Field("BAND", "20m", "E"))), Record((Field("X", "y"),))),
    )
    found = differences(before, after)
    assert len(found) == 3
    assert found[0].startswith("header, field 1")
    assert found[1] == "record count: 1 != 2"
    assert found[2].startswith("record 1, field 2")
    assert differences(before, before) == []


def test_differences_detects_header_presence_and_text() -> None:
    """A dropped header or changed header text is a difference."""
    record = (Record((Field("CALL", "Q1A"),)),)
    with_header = AdifDocument(Header("a\n"), record)
    assert differences(with_header, AdifDocument(None, record)) == [
        "header: present in one document only",
    ]
    assert differences(with_header, AdifDocument(Header("b\n"), record)) == [
        "header: text differs",
    ]
