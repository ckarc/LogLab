# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""What counts as a dupe in each Field Day, and what doesn't count at all (XVII)."""

from __future__ import annotations

import uuid

import pytest

from loglab_engine.contests import EVERYDAY, Earlier, Overlay, check, overlay


def _overlay(overlay_id: str) -> Overlay:
    found = overlay(overlay_id)
    assert found is not None
    return found


def _qso(band: str, mode: str, **more: str) -> dict[str, str]:
    fields = {"CALL": "W1AW", "BAND": band, "MODE": mode, "CLASS": "3A", "ARRL_SECT": "CT"}
    fields.update(more)
    return fields


def _earlier(band: str, mode: str, **more: str) -> Earlier:
    return Earlier(uuid.uuid4(), _qso(band, mode, **more), None, "K9KSU")


FD = "arrl-fd"
WFD = "wfd"
BOTH = pytest.mark.parametrize("overlay_id", [FD, WFD])


@BOTH
def test_the_first_qso_with_a_station_is_new(overlay_id: str) -> None:
    """Nothing worked yet: new."""
    result = check(_overlay(overlay_id), _qso("20m", "SSB"), [])
    assert result.status == "new"
    assert (result.band, result.group) == ("20m", "Phone")


@BOTH
def test_the_same_band_and_mode_is_a_dupe(overlay_id: str) -> None:
    """Once per band per mode: a second 20 m phone QSO is a dupe."""
    first = _earlier("20m", "SSB")
    result = check(_overlay(overlay_id), _qso("20m", "SSB"), [first])
    assert result.status == "dupe"
    assert [d.qso_id for d in result.dupes] == [first.qso_id]
    assert "Dupe" in result.message


@BOTH
def test_every_voice_mode_is_the_same_mode(overlay_id: str) -> None:
    """FD Rule 6.6 (and WFD's Phone group): SSB then FM on one band is a dupe."""
    assert check(_overlay(overlay_id), _qso("2m", "FM"), [_earlier("2m", "SSB")]).status == "dupe"


@BOTH
def test_every_digital_mode_is_the_same_mode(overlay_id: str) -> None:
    """FD Rule 6.7 (and WFD's Digital group): RTTY then PSK on one band is a dupe."""
    result = check(_overlay(overlay_id), _qso("40m", "PSK"), [_earlier("40m", "RTTY")])
    assert result.status == "dupe"
    assert result.group == "Digital"


@BOTH
def test_another_mode_or_band_is_new(overlay_id: str) -> None:
    """CW and phone on 20 m, and phone on 40 m, are three different contacts."""
    earlier = [_earlier("20m", "SSB")]
    found = _overlay(overlay_id)
    assert check(found, _qso("20m", "CW"), earlier).status == "new"
    result = check(found, _qso("40m", "SSB"), earlier)
    assert result.status == "new"
    assert "Already worked on 20m Phone" in result.message


@BOTH
def test_three_modes_on_one_band_and_then_a_dupe(overlay_id: str) -> None:
    """WFD: "once per band-mode combination, for a maximum of three times per band"."""
    earlier = [_earlier("20m", "CW"), _earlier("20m", "SSB"), _earlier("20m", "RTTY")]
    result = check(_overlay(overlay_id), _qso("20m", "FT8" if overlay_id == FD else "PSK"), earlier)
    assert result.status == "dupe"


@BOTH
def test_a_satellite_qso_counts_on_its_own_band(overlay_id: str) -> None:
    """FD Rule 7.3.7: a satellite QSO is its own band, whatever its uplink band."""
    found = _overlay(overlay_id)
    earlier = [_earlier("2m", "FM")]
    result = check(found, _qso("2m", "FM", PROP_MODE="SAT"), earlier)
    assert (result.status, result.band) == ("new", "SAT")
    sat = [_earlier("70cm", "FM", PROP_MODE="SAT")]
    assert check(found, _qso("2m", "FM", PROP_MODE="SAT"), sat).status == "dupe"


@pytest.mark.parametrize("band", ["60m", "30m", "17m", "12m"])
@BOTH
def test_the_warc_bands_and_60m_do_not_count(overlay_id: str, band: str) -> None:
    """Neither Field Day counts 60, 30, 17 or 12 m; such a QSO is not called a dupe."""
    result = check(_overlay(overlay_id), _qso(band, "CW"), [_earlier(band, "CW")])
    assert result.status == "not_counted"
    assert result.dupes == ()


@pytest.mark.parametrize(
    ("band", "fd", "wfd"), [("160m", True, True), ("6m", True, True), ("2190m", False, True)]
)
def test_which_bands_count(band: str, *, fd: bool, wfd: bool) -> None:
    """FD lists its bands; WFD counts every band but four."""
    assert (check(_overlay(FD), _qso(band, "CW"), []).status == "new") is fd
    assert (check(_overlay(WFD), _qso(band, "CW"), []).status == "new") is wfd


@pytest.mark.parametrize(
    ("mode", "submode"), [("FT8", ""), ("MFSK", "FT4"), ("MFSK", "Q65"), ("JT65", ""), ("WSPR", "")]
)
def test_winter_field_day_does_not_count_wsjt_modes(mode: str, submode: str) -> None:
    """WFD 2026: WSJT modes, FT8 and FT4 among them, are not allowed. Field Day counts them."""
    qso = _qso("20m", mode, SUBMODE=submode)
    result = check(_overlay(WFD), qso, [])
    assert result.status == "not_counted"
    assert "doesn't count in Winter Field Day" in result.message
    assert check(_overlay(FD), qso, []).status == "new"


def test_js8_is_digital_in_winter_field_day() -> None:
    """JS8Call is an MFSK submode, but not a WSJT one: WFD counts it as digital."""
    result = check(_overlay(WFD), _qso("40m", "MFSK", SUBMODE="JS8"), [])
    assert (result.status, result.group) == ("new", "Digital")


def test_without_a_call_band_or_mode_nothing_is_decided() -> None:
    """Half-typed: incomplete, never a dupe."""
    result = check(_overlay(FD), {"CALL": "W1AW", "BAND": "20m"}, [_earlier("20m", "SSB")])
    assert result.status == "incomplete"


def test_everyday_never_checks_for_dupes() -> None:
    """Brian, 2026-10-18: dupe checks are only for contest overlays."""
    result = check(_overlay(EVERYDAY), _qso("20m", "SSB"), [_earlier("20m", "SSB")])
    assert result.status == "no_check"
    assert result.dupes == ()


@pytest.mark.parametrize(
    ("overlay_id", "klass", "section", "bad"),
    [
        (FD, "3A", "KS", set()),
        (FD, "12F", "DX", set()),
        (FD, "3O", "KS", {"CLASS"}),
        (FD, "3A", "MX", {"ARRL_SECT"}),
        (FD, "", "ZZZ", {"CLASS", "ARRL_SECT"}),
        (WFD, "2O", "MX", set()),
        (WFD, "1H", "ONE", set()),
        (WFD, "3A", "KS", {"CLASS"}),
    ],
)
def test_the_received_exchange_is_checked(
    overlay_id: str, klass: str, section: str, bad: set[str]
) -> None:
    """FD class is a count and A to F; WFD's is a count and H, I, O or M. MX is WFD only."""
    qso = _qso("20m", "CW", CLASS=klass, ARRL_SECT=section)
    problems = check(_overlay(overlay_id), qso, []).problems
    assert {p.field for p in problems} == bad
