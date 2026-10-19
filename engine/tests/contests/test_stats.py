# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operation stats: rate, hours, dupes and QSO points, all from the overlay's data (III)."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta

import pytest

from loglab_engine.contests import OverlayError, overlay, parse
from loglab_engine.contests.overlay import Overlay
from loglab_engine.contests.stats import Logged, operation_stats

START = datetime(2027, 6, 26, 18, 0, tzinfo=UTC)
END = datetime(2027, 6, 27, 20, 59, tzinfo=UTC)


def _fd() -> Overlay:
    found = overlay("arrl-fd")
    assert found is not None
    return found


def _qso(  # noqa: PLR0913 - one keyword per field a test changes
    call: str,
    minutes: int,
    *,
    band: str = "20m",
    mode: str = "CW",
    operator: str = "K9KSU",
    section: str = "KS",
    station: str = "W0CK",
) -> Logged:
    fields = {
        "CALL": call,
        "BAND": band,
        "MODE": mode,
        "OPERATOR": operator,
        "ARRL_SECT": section,
        "STATION_CALLSIGN": station,
    }
    return Logged(fields=fields, qso_start=START + timedelta(minutes=minutes), operator=operator)


def test_field_day_scores_two_for_cw_and_digital_one_for_phone() -> None:
    """ARRL Field Day Rule 7.1: Phone 1 point, CW 2, Digital 2."""
    qsos = [
        _qso("W1AW", 1, mode="CW"),
        _qso("K1ABC", 2, mode="SSB"),
        _qso("N0XYZ", 3, mode="FT8"),
    ]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert stats.score == 5
    assert stats.by_mode == {"CW": 1, "Digital": 1, "Phone": 1}


def test_a_dupe_is_counted_apart_and_earns_nothing() -> None:
    """The second W1AW on 20 m CW is a dupe; on 40 m it is a new contact."""
    qsos = [_qso("W1AW", 1), _qso("w1aw", 2), _qso("W1AW", 3, band="40m")]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert (stats.qsos, stats.counted, stats.dupes) == (3, 2, 1)
    assert stats.score == 4


def test_the_gota_station_is_not_a_dupe_of_the_main_station() -> None:
    """The GOTA station has its own call, so it may work the same station (Rule 4.1.1.1)."""
    qsos = [_qso("W1AW", 1), _qso("W1AW", 2, station="K0GTA")]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert stats.dupes == 0


def test_qsos_that_do_not_count_are_set_apart() -> None:
    """17 m is not a Field Day band; a QSO before the start is outside the hours."""
    qsos = [_qso("W1AW", -5), _qso("K1ABC", 1, band="17m"), _qso("N0XYZ", 2)]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert (stats.outside, stats.not_counted, stats.counted) == (1, 1, 1)


def test_rate_counts_the_last_hour_and_ten_minutes() -> None:
    """The rate looks back from now; QSOs after now (a wrong clock) don't count."""
    now = START + timedelta(hours=2)
    qsos = [_qso("A1A", 30), _qso("B1B", 70), _qso("C1C", 115), _qso("D1D", 119), _qso("E1E", 130)]
    stats = operation_stats(_fd(), qsos, starts_at=START, ends_at=END, now=now)
    assert stats.last_hour == 3
    assert stats.last_10 == 2


def test_each_hour_of_the_operation_has_a_bar_quiet_ones_too() -> None:
    """From the start's hour to now's, with the busiest one picked out."""
    now = START + timedelta(hours=3, minutes=10)
    qsos = [_qso("A1A", 5), _qso("B1B", 10), _qso("C1C", 130)]
    stats = operation_stats(_fd(), qsos, starts_at=START, ends_at=END, now=now)
    assert [h.qsos for h in stats.hourly] == [2, 0, 1, 0]
    assert stats.hourly[0].start == START
    assert stats.best_hour is not None
    assert stats.best_hour.start == START


def test_operators_are_tallied_with_their_points() -> None:
    """The busiest operator comes first."""
    qsos = [
        _qso("A1A", 1, operator="W0XYZ"),
        _qso("B1B", 2, operator="W0XYZ", mode="SSB"),
        _qso("C1C", 3),
    ]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert [(o.operator, o.qsos, o.points, o.last_hour) for o in stats.operators] == [
        ("W0XYZ", 2, 3, 2),
        ("K9KSU", 1, 2, 1),
    ]


def test_sections_worked_are_counted_once() -> None:
    """Two QSOs with Kansas and one with Connecticut are two sections."""
    qsos = [_qso("A1A", 1), _qso("B1B", 2), _qso("C1C", 3, section="CT")]
    stats = operation_stats(
        _fd(), qsos, starts_at=START, ends_at=END, now=START + timedelta(hours=1)
    )
    assert stats.sections_worked == 2
    assert stats.sections_total == 85


def test_winter_field_day_gives_satellite_qsos_no_points() -> None:
    """WFD 2026 rules: satellite QSOs count only toward the satellite objective."""
    wfd = overlay("wfd")
    assert wfd is not None
    sat = _qso("A1A", 1, band="2m", mode="FM")
    sat = Logged({**sat.fields, "PROP_MODE": "SAT"}, sat.qso_start, sat.operator)
    stats = operation_stats(
        wfd, [sat], starts_at=None, ends_at=None, now=START + timedelta(hours=1)
    )
    assert (stats.counted, stats.score) == (1, 0)


def test_everyday_has_no_score_and_no_dupes() -> None:
    """Without a contest, bands and modes are taken as logged."""
    everyday = overlay("everyday")
    assert everyday is not None
    qsos = [_qso("A1A", 1, mode="FT8"), _qso("A1A", 2, mode="FT8")]
    stats = operation_stats(
        everyday, qsos, starts_at=None, ends_at=None, now=START + timedelta(hours=1)
    )
    assert (stats.score, stats.dupes, stats.by_mode) == (None, 0, {"FT8": 2})
    assert everyday.shown_stats == ("rate", "hourly", "band_mode", "operators")


def test_field_day_shows_every_stat() -> None:
    """The shipped Field Day overlay asks for the score and sections too."""
    assert set(_fd().shown_stats) == {
        "rate",
        "hourly",
        "band_mode",
        "operators",
        "dupes",
        "sections",
        "score",
    }


@pytest.mark.parametrize(
    ("stats", "why"),
    [
        ({"show": ["score"]}, "a score with no points"),
        ({"show": ["rate", "rate"]}, "a stat twice"),
        ({"show": ["fun"]}, "no such stat"),
        ({"show": ["dupes"]}, "dupes without a dupe check"),
    ],
)
def test_an_overlay_with_impossible_stats_is_refused(stats: object, why: str) -> None:
    """The Engine refuses stats it could not work out (Principle XII)."""
    raw = {
        "format": 1,
        "id": "bad",
        "name": "Bad",
        "summary": why,
        "entry": {"fields": ["CALL"]},
        "stats": stats,
    }
    with pytest.raises(OverlayError):
        parse(json.dumps(raw).encode(), "bad.json")
