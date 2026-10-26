# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Personal stats: one operator's everyday numbers, from their QSOs as logged."""

from __future__ import annotations

from datetime import UTC, datetime

from loglab_engine.personal import personal_stats
from loglab_engine.store import Worked

# A Thursday afternoon, UTC.
NOW = datetime(2026, 10, 22, 18, 0, tzinfo=UTC)


def _qso(when: datetime | None, **fields: str) -> Worked:
    """A QSO on 20m FT8 at ``when``, with ``state``, ``dxcc`` or ``country`` as given."""
    return Worked(
        when,
        fields.get("band", "20m"),
        fields.get("mode", "FT8"),
        fields.get("state", ""),
        fields.get("dxcc", ""),
        fields.get("country", ""),
    )


def test_today_this_week_and_this_year_start_at_utc_midnight() -> None:
    """Days, weeks (from Monday) and years are UTC (Principle VIII)."""
    stats = personal_stats(
        [
            _qso(datetime(2026, 10, 22, 0, 0, tzinfo=UTC)),  # today
            _qso(datetime(2026, 10, 21, 23, 59, tzinfo=UTC)),  # yesterday, this week
            _qso(datetime(2026, 10, 19, 0, 0, tzinfo=UTC)),  # Monday, this week
            _qso(datetime(2026, 10, 18, 23, 59, tzinfo=UTC)),  # last Sunday
            _qso(datetime(2026, 1, 1, 0, 0, tzinfo=UTC)),  # this year
            _qso(datetime(2025, 12, 31, 23, 59, tzinfo=UTC)),  # last year
            _qso(None),  # no time: counts only in the total
        ],
        now=NOW,
    )
    assert (stats.today, stats.week, stats.year, stats.total) == (1, 3, 5, 7)
    assert stats.last_qso == datetime(2026, 10, 22, 0, 0, tzinfo=UTC)


def test_bands_and_modes_are_counted_as_logged() -> None:
    """Each band and mode pair, with a QSO missing either under "?"."""
    stats = personal_stats(
        [_qso(NOW), _qso(NOW), _qso(NOW, band="40m", mode="CW"), _qso(NOW, band="", mode="")],
        now=NOW,
    )
    assert [(b.band, b.mode, b.qsos) for b in stats.by_band_mode] == [
        ("20m", "FT8", 2),
        ("40m", "CW", 1),
        ("?", "?", 1),
    ]


def test_states_count_the_fifty_and_only_in_the_us() -> None:
    """DC is not a state, and WA in Australia is not Washington."""
    stats = personal_stats(
        [
            _qso(NOW, state="KS"),
            _qso(NOW, state="KS", dxcc="291"),
            _qso(NOW, state="AK", dxcc="6"),
            _qso(NOW, state="DC"),
            _qso(NOW, state="WA", dxcc="150"),
            _qso(NOW, state="ON", dxcc="1"),
        ],
        now=NOW,
    )
    assert stats.states == ("AK", "KS")
    assert stats.states_total == 50
    assert len(stats.states_needed) == 48
    assert "DC" not in stats.states_needed


def test_countries_come_from_the_log_never_from_the_call() -> None:
    """DXCC numbers and country names agree; a QSO naming neither is counted apart."""
    stats = personal_stats(
        [
            _qso(NOW, dxcc="291"),
            _qso(NOW, country="UNITED STATES"),
            _qso(NOW, country="UNITED STATES OF AMERICA"),
            _qso(NOW, dxcc="1"),
            _qso(NOW, country="ATLANTIS"),
            _qso(NOW, dxcc="0"),
            _qso(NOW),
        ],
        now=NOW,
    )
    assert [(c.name, c.dxcc, c.qsos) for c in stats.countries] == [
        ("United States of America", 291, 3),
        ("Atlantis", None, 1),
        ("Canada", 1, 1),
    ]
    assert stats.no_country == 2
