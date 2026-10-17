# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The field-level model refuses values that cannot be written as ADI."""

from __future__ import annotations

import pytest

from loglab_engine.adif import AdifDocument, AdifModelError, Field, Header, Record


@pytest.mark.parametrize("name", ["", " CALL", "CALL ", "CA:LL", "C<L", "C>L", "C,L", "{X}", "eor"])
def test_invalid_field_names_are_refused(name: str) -> None:
    """Names an ADI file cannot carry never make it into the model."""
    with pytest.raises(AdifModelError):
        Field(name, "x")


def test_names_with_inner_spaces_and_app_prefix_are_allowed() -> None:
    """Interior spaces and application-defined names are valid."""
    assert Field("APP_LOGLAB_GHOST", "Y", "B").key == "APP_LOGLAB_GHOST"
    assert Field("my field", "x").key == "MY FIELD"


@pytest.mark.parametrize("indicator", ["", "SS", "1", "é"])
def test_invalid_type_indicators_are_refused(indicator: str) -> None:
    """A type indicator is exactly one ASCII letter."""
    with pytest.raises(AdifModelError):
        Field("CALL", "x", indicator)


def test_empty_record_and_bad_header_text_are_refused() -> None:
    """Records need a field; header text must be non-empty and free of '<'."""
    with pytest.raises(AdifModelError):
        Record(())
    with pytest.raises(AdifModelError):
        Header("")
    with pytest.raises(AdifModelError):
        Header("text <with> markup")


def test_lookup_ignores_case_and_counts_fields() -> None:
    """Lookups are case-insensitive; the document counts every field."""
    record = Record((Field("call", "Q1A"), Field("BAND", "20m")))
    header = Header("h\n", (Field("adif_ver", "3.1.7"),))
    document = AdifDocument(header, (record, record))
    assert record.get("CALL") == "Q1A"
    assert record.get("MODE") is None
    assert header.get("ADIF_VER") == "3.1.7"
    assert document.field_count == 5
    assert not Field("NAME", "Jörg").is_ascii
