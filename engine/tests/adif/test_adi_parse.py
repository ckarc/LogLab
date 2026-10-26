# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""ADI reader tests against ADIF 3.1.7 section IV.A, including malformed input."""

from __future__ import annotations

import pytest

from loglab_engine.adif import AdiParseError, Field, ParseLimits, parse_adi

# ADIF 3.1.7 IV.A.7, "Sample ADI File".
SPEC_SAMPLE = b"""Generated on 2011-11-22 at 02:15:23Z for WN4AZY
<adif_ver:5>3.0.5
<programid:7>monolog
<USERDEF1:3:N>EPC
<USERDEF2:19:E>SweaterSize,{S,M,L}
<USERDEF3:15:N>ShoeSize,{5:20}
<EOH>
<qso_date:8>19900620
<time_on:4>1523
<call:5>VK9NS
<band:3>20M
<mode:4>RTTY
<sweatersize:1>M
<shoesize:2>11
<app_monolog_compression:3>off
<eor>
<qso_date:8>20101022
<time_on:4>0111
<call:5>ON4UN
<band:3>40M
<mode:3>PSK
<submode:5>PSK63
<epc:5>32123
<app_monolog_compression:3>off
<eor>
"""


def test_spec_sample_parses_exactly() -> None:
    """The specification's own sample file yields its header and both records."""
    result = parse_adi(SPEC_SAMPLE)
    doc = result.document
    assert doc.header is not None
    assert doc.header.preamble == "Generated on 2011-11-22 at 02:15:23Z for WN4AZY\n"
    assert doc.header.get("ADIF_VER") == "3.0.5"
    assert doc.header.fields[3] == Field("USERDEF2", "SweaterSize,{S,M,L}", "E")
    assert len(doc.records) == 2
    first, second = doc.records
    assert first.get("CALL") == "VK9NS"
    assert first.get("SweaterSize") == "M"
    assert first.fields[0] == Field("qso_date", "19900620")
    assert second.get("SUBMODE") == "PSK63"
    assert second.get("app_monolog_compression") == "off"
    assert result.warnings == ()


def test_spec_single_line_record_without_header() -> None:
    """A file starting with '<' has no header (ADIF 3.1.7 IV.A.6 example)."""
    data = b"<call:6>WN4AZY<band:3>20M<mode:4>RTTY<qso_date:8>19960513<time_on:4>1305<eor>"
    doc = parse_adi(data).document
    assert doc.header is None
    assert [f.key for f in doc.records[0].fields] == ["CALL", "BAND", "MODE", "QSO_DATE", "TIME_ON"]


def test_data_is_delimited_by_length_not_by_markup() -> None:
    """Data may contain '<', '>', and tag-like text; only the length ends it."""
    data = b"<COMMENT:18><EOR> <CALL:4>FAKE<CALL:5>Q1ABC<EOR>"
    record = parse_adi(data).document.records[0]
    assert record.fields == (Field("COMMENT", "<EOR> <CALL:4>FAKE"), Field("CALL", "Q1ABC"))


def test_multiline_and_empty_data() -> None:
    """CRLF line breaks and zero-length data are preserved exactly."""
    data = b"<NOTES:8:M>one\r\ntwo <COMMENT:0><CALL:3>Q1A<EOR>"
    fields = parse_adi(data).document.records[0].fields
    assert fields[0] == Field("NOTES", "one\r\ntwo", "M")
    assert fields[1] == Field("COMMENT", "")


def test_text_outside_specifiers_is_ignored() -> None:
    """Line breaks and other text between specifiers are not data."""
    data = b"<CALL:3>Q1A  ignored text\r\n <BAND:3>20m\n<eOr>\nmore ignored text\n"
    doc = parse_adi(data).document
    assert [f.value for f in doc.records[0].fields] == ["Q1A", "20m"]


def test_header_with_no_fields_and_empty_log() -> None:
    """A header alone, with zero records, is an empty log, not an error."""
    doc = parse_adi(b"Exported by nobody\n<EOH>\n").document
    assert doc.header is not None
    assert doc.header.fields == ()
    assert doc.records == ()


def test_empty_input_is_an_empty_log() -> None:
    """Zero bytes parse to no header and no records."""
    doc = parse_adi(b"").document
    assert doc.header is None
    assert doc.records == ()


def test_leading_zero_length_is_accepted_with_warning() -> None:
    """Leading zeros are import-only: read them, and say so."""
    result = parse_adi(b"<CALL:0005>Q1ABC<EOR>")
    assert result.document.records[0].get("CALL") == "Q1ABC"
    assert len(result.warnings) == 1
    assert "leading zeros" in result.warnings[0].message


def test_non_ascii_utf8_is_preserved_with_warning() -> None:
    """Out-of-spec UTF-8 survives byte-for-byte, flagged as non-compliant."""
    data = "<NAME:5>Jörg<CALL:3>Q1A<EOR>".encode()
    result = parse_adi(data)
    assert result.document.records[0].get("NAME") == "Jörg"
    assert result.warnings[0].field == "NAME"


@pytest.mark.parametrize(
    ("data", "complaint"),
    [
        (b"header text with no end", "not terminated by <EOH>"),
        (b"header <CALL:3>Q1A", "not terminated by <EOH>"),
        (b"header <EOR>", "<EOR> inside the header"),
        (b"<CALL:3>Q1A<EOH>", "<EOH> after the header"),
        (b"<CALL:3>Q1A", "not terminated by <EOR>"),
        (b"<EOR>", "no fields"),
        (b"<CALL:3>Q1A<EOR><EOR>", "no fields"),
        (b"<CALL:3>Q1", "past the end"),
        (b"<CALL:+3>Q1A<EOR>", "not an unsigned integer"),
        (b"<STX:3.0>001<EOR>", "not an unsigned integer"),
        (b"<CALL:>Q1A<EOR>", "not an unsigned integer"),
        (b"<CALL:3:>Q1A<EOR>", "single ASCII letter"),
        (b"<CALL:3:SS>Q1A<EOR>", "single ASCII letter"),
        (b"<CALL:3:S:X>Q1A<EOR>", "too many ':'"),
        (b"<:3>Q1A<EOR>", "empty"),
        (b"< CALL:3>Q1A<EOR>", "space"),
        (b"<CA,LL:3>Q1A<EOR>", "forbidden character"),
        (b"<EOR:0><EOR>", "reserved tag"),
        (b"<HELLO>", "unknown tag"),
        (b"<CALL:3>Q1A <oops <EOR>", "'<' inside a tag"),
        (b"<CALL:3>Q1A <unclosed", "not closed"),
        (b"<CALL:2>\xff\xfe<EOR>", "not valid UTF-8"),
        ("<CÄLL:3>Q1A<EOR>".encode(), "non-ASCII byte inside a tag"),
        (b"\xff header <EOH>", "header text is not valid UTF-8"),
    ],
)
def test_malformed_input_fails_closed(data: bytes, complaint: str) -> None:
    """Malformed ADI raises AdiParseError naming the problem (Principle XII)."""
    with pytest.raises(AdiParseError, match=complaint):
        parse_adi(data)


def test_error_reports_record_number_and_offset() -> None:
    """Errors say where they happened so an operator can find the bad record."""
    data = b"<CALL:3>Q1A<EOR><CALL:3>Q2B<EOR><CALL:+1>X<EOR>"
    with pytest.raises(AdiParseError) as caught:
        parse_adi(data)
    assert caught.value.record == 3
    assert caught.value.offset == data.index(b"<CALL:+1>")


@pytest.mark.parametrize(
    ("limits", "data", "complaint"),
    [
        (ParseLimits(max_document_bytes=10), b"<CALL:3>Q1A<EOR>", "larger than 10 bytes"),
        (ParseLimits(max_value_bytes=2), b"<CALL:3>Q1A<EOR>", "exceeds 2 bytes"),
        (ParseLimits(max_tag_bytes=5), b"<CALLSIGN:3>Q1A<EOR>", "within 5 bytes"),
        (ParseLimits(max_fields_per_record=1), b"<A:1>x<B:1>y<EOR>", "more than 1 fields"),
        (ParseLimits(max_fields_per_record=1), b"h<A:1>x<B:1>y<EOH>", "header has more"),
        (ParseLimits(max_records=1), b"<A:1>x<EOR><A:1>y<EOR>", "more than 1 records"),
    ],
)
def test_limits_are_enforced(limits: ParseLimits, data: bytes, complaint: str) -> None:
    """Every size limit stops the reader before it allocates past it."""
    with pytest.raises(AdiParseError, match=complaint):
        parse_adi(data, limits)


def test_huge_declared_length_is_rejected_without_reading() -> None:
    """A length far past the cap fails on the number alone."""
    with pytest.raises(AdiParseError, match="exceeds"):
        parse_adi(b"<NOTES:99999999999999999999>x<EOR>")
