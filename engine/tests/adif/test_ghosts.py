# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Ghost QSOs are deterministic, unmistakably fake, spec-shaped, and lossless."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta, timezone

import pytest

from loglab_engine.adif import parse_adi, write_adi
from loglab_engine.adif.ghosts import EDGE_CASES, GHOST_FLAG, generate

REQUIRED = ("CALL", "QSO_DATE", "TIME_ON", "BAND", "MODE", "STATION_CALLSIGN")


def test_same_seed_same_ghosts_different_seed_different_ghosts() -> None:
    """Generation is reproducible from its seed."""
    assert write_adi(generate(50, seed=7)) == write_adi(generate(50, seed=7))
    assert write_adi(generate(50, seed=7)) != write_adi(generate(50, seed=8))


def test_every_ghost_is_flagged_and_uses_an_unissued_prefix() -> None:
    """Ghosts can never pass for real contacts."""
    document = generate(100)
    assert document.header is not None
    assert "not real contacts" in document.header.preamble
    for record in document.records:
        assert record.get(GHOST_FLAG) == "Y"
        call = record.get("CALL")
        assert call is not None
        assert call.startswith("Q")


def test_every_ghost_has_the_core_qso_fields() -> None:
    """Each record carries who, when, band, mode, and the logging station."""
    for record in generate(100).records:
        for name in REQUIRED:
            assert record.get(name), f"missing {name}"


def test_ghost_times_are_utc_and_in_order() -> None:
    """QSO start times never go backwards and begin at the requested UTC time."""
    start = datetime(2026, 1, 24, 19, 0, tzinfo=UTC)
    stamps = [
        datetime.strptime(f"{r.get('QSO_DATE')}{r.get('TIME_ON')}", "%Y%m%d%H%M").replace(
            tzinfo=UTC,
        )
        for r in generate(200, start=start).records
    ]
    assert stamps[0] == start
    assert stamps == sorted(stamps)


def test_ten_ghosts_cover_every_edge_case() -> None:
    """Any file of ten or more ghosts exercises each ADI edge case."""
    records = generate(EDGE_CASES).records
    comments = {r.get("COMMENT") for r in records}
    assert "<EOR> is not the end <CALL:4>FAKE <EOH>" in comments
    assert "" in comments
    assert any("\r\n" in (r.get("NOTES") or "") for r in records)
    assert any(f.name.islower() for r in records for f in r.fields)
    assert any(f.type_indicator == "D" for r in records for f in r.fields)
    assert any(r.get("CONTEST_ID") for r in records)
    assert any(r.get("GHOST_LEVEL") for r in records)
    assert any(r.get("APP_LOGLAB_HAUNTING") for r in records)
    assert any(r.fields[0].key != "CALL" for r in records)
    assert any(len(r.get("NOTES") or "") > 1000 for r in records)


def test_ghost_files_round_trip_byte_for_byte() -> None:
    """A ghost file re-exports to the identical bytes, with no warnings."""
    data = write_adi(generate(500, seed=3))
    result = parse_adi(data)
    assert result.warnings == ()
    assert write_adi(result.document) == data


def test_zero_ghosts_is_a_header_only_file() -> None:
    """Asking for no QSOs still produces a valid, empty log."""
    document = parse_adi(write_adi(generate(0))).document
    assert document.header is not None
    assert document.records == ()


def test_bad_arguments_are_refused() -> None:
    """Negative counts and non-UTC start times are errors (Principle VIII)."""
    with pytest.raises(ValueError, match="negative"):
        generate(-1)
    central = timezone(timedelta(hours=-5))
    with pytest.raises(ValueError, match="UTC"):
        generate(1, start=datetime(2026, 1, 1, tzinfo=central))
