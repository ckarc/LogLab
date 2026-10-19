# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Overlay files: shipped as data, parsed strictly, refused whole when wrong (III, XII)."""

from __future__ import annotations

import json

import pytest

from loglab_engine.contests import EVERYDAY, OverlayError, overlay, overlays, parse


def test_the_shipped_overlays_are_everyday_then_the_field_days() -> None:
    """Everyday comes first; the two Field Days are the first contests."""
    ids = [o.id for o in overlays()]
    assert ids[0] == EVERYDAY
    assert set(ids) >= {"everyday", "arrl-fd", "wfd"}


def test_everyday_is_not_a_contest() -> None:
    """Everyday logging has no contest, no exchange, and no dupe rule."""
    everyday = overlay(EVERYDAY)
    assert everyday is not None
    assert not everyday.is_contest
    assert everyday.dupes is None
    assert everyday.exchange is None


@pytest.mark.parametrize(("overlay_id", "contest_id"), [("arrl-fd", "ARRL-FD"), ("wfd", "WFD")])
def test_each_field_day_names_its_rules_and_counts_by_band_and_mode(
    overlay_id: str, contest_id: str
) -> None:
    """Both Field Days count a station once per band per mode, and cite where that is written."""
    found = overlay(overlay_id)
    assert found is not None
    assert found.contest_id == contest_id
    assert found.dupes is not None
    assert found.dupes.per == ("band", "mode")
    assert found.rules is not None
    assert found.rules.url.startswith("https://")
    assert "CALL" in found.entry.fields


@pytest.mark.parametrize(
    ("overlay_id", "contest_id", "per", "zone_field"),
    [
        ("cq-ww-ssb", "CQ-WW-SSB", ("band",), "CQZ"),
        ("cq-ww-cw", "CQ-WW-CW", ("band",), "CQZ"),
        ("iaru-hf", "IARU-HF", ("band", "mode"), "ITUZ"),
        ("arrl-ss-cw", "ARRL-SS-CW", (), None),
        ("arrl-ss-ssb", "ARRL-SS-SSB", (), None),
    ],
)
def test_the_zone_and_sweepstakes_contests_follow_their_rules(
    overlay_id: str, contest_id: str, per: tuple[str, ...], zone_field: str | None
) -> None:
    """CQ WW counts once per band, IARU once per band and mode, Sweepstakes once at all.

    The zone contests receive the zone in the ADIF field their map counts.
    """
    found = overlay(overlay_id)
    assert found is not None
    assert found.contest_id == contest_id
    assert found.dupes is not None
    assert found.dupes.per == per
    assert found.rules is not None
    assert found.rules.url.startswith("https://")
    assert found.bands is not None
    assert found.bands.allowed == ("160m", "80m", "40m", "20m", "15m", "10m")
    assert found.exchange is not None
    fields = {p.field for p in found.exchange.received}
    if zone_field is None:
        assert any(p.sections for p in found.exchange.received)
    else:
        assert zone_field in fields
        assert zone_field in found.entry.fields
    # Their points depend on continent or zone, not mode group, so no score is shown.
    assert found.stats is not None
    assert "score" not in found.stats.show
    assert ("sections" in found.stats.show) == (zone_field is None)


def test_a_single_mode_contest_does_not_count_the_other_modes() -> None:
    """CQ WW CW counts CW only; the SSB weekend counts phone only."""
    cw = overlay("cq-ww-cw")
    ssb = overlay("cq-ww-ssb")
    assert cw is not None
    assert ssb is not None
    assert cw.modes is not None
    assert ssb.modes is not None
    assert cw.modes.group("CW", None) == "CW"
    assert cw.modes.group("SSB", None) is None
    assert cw.modes.group("RTTY", None) is None
    assert ssb.modes.group("SSB", "USB") == "Phone"
    assert ssb.modes.group("CW", None) is None
    assert ssb.modes.group("FT8", None) is None


def _minimal(**changes: object) -> dict[str, object]:
    raw: dict[str, object] = {
        "format": 1,
        "id": "test",
        "name": "Test",
        "summary": "A test overlay.",
        "entry": {"fields": ["CALL"]},
    }
    raw.update(changes)
    return raw


def _bytes(raw: dict[str, object]) -> bytes:
    return json.dumps(raw).encode()


def test_a_minimal_overlay_parses() -> None:
    """An overlay needs only its id, name, summary and the fields it asks for."""
    assert parse(_bytes(_minimal()), "test.json").name == "Test"


@pytest.mark.parametrize(
    "raw",
    [
        _minimal(surprise=True),
        _minimal(entry={"fields": ["NAME"]}),
        _minimal(entry={"fields": ["CALL", "CALL"]}),
        _minimal(entry={"fields": ["CALL"], "store_after": "NAME"}),
        _minimal(contest_id="TEST"),
        _minimal(dupes={"per": ["band"], "rule": "x"}),
        _minimal(id="Bad Id"),
        _minimal(format=2),
    ],
    ids=[
        "unknown key",
        "no call",
        "field twice",
        "store after a field it lacks",
        "half a contest",
        "dupes, no contest",
        "id",
        "format",
    ],
)
def test_an_overlay_that_is_not_whole_is_refused(raw: dict[str, object]) -> None:
    """A wrong overlay is refused, never half-loaded (Principle XII)."""
    with pytest.raises(OverlayError):
        parse(_bytes(raw), "test.json")


def test_the_store_button_can_follow_any_field_new_qso_asks_for() -> None:
    """Tab can reach Store QSO straight after the exchange, before the rest."""
    raw = _minimal(entry={"fields": ["CALL", "NAME", "COMMENT"], "store_after": "NAME"})
    assert parse(_bytes(raw), "test.json").entry.store_after == "NAME"
    assert parse(_bytes(_minimal()), "test.json").entry.store_after is None


def test_an_overlay_must_be_named_after_its_id() -> None:
    """The file name and the id agree, so two files can't claim one id."""
    with pytest.raises(OverlayError, match="named after its id"):
        parse(_bytes(_minimal()), "other.json")


@pytest.mark.parametrize("data", [b"\xff\xfe", b"{", b"[]", b" " * (64 * 1024 + 1)])
def test_an_overlay_file_that_is_not_json_or_too_large_is_refused(data: bytes) -> None:
    """Bad bytes, bad JSON, the wrong shape, or too much of it are all refused."""
    with pytest.raises(OverlayError):
        parse(data, "test.json")


def test_an_exchange_pattern_must_compile() -> None:
    """A pattern that isn't a regular expression is refused when the file loads."""
    fd = overlay("arrl-fd")
    assert fd is not None
    raw = json.loads(fd.model_dump_json())
    raw["id"] = "test"
    raw["exchange"]["received"][0]["pattern"] = "["
    with pytest.raises(OverlayError, match="pattern"):
        parse(_bytes(raw), "test.json")
