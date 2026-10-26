# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Personal stats: what one operator has done, for everyday logging.

With no operation picked, the Operation stats box shows the signed-in
operator's own numbers instead of the group's: QSOs today, this week and this
year (UTC days and weeks, Principle VIII), bands and modes, and the states and
countries they have worked. The Engine works them out from the local log
(Principles I and V).

States and countries are counted from the QSO's own ``STATE``, ``DXCC`` and
``COUNTRY`` fields, as logged. Nothing is guessed from the call: a QSO that
names no country is counted as such, never placed by its prefix, so the box
never presents a guess as something logged.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Final

from loglab_engine.lotw.tables import US_DXCC, tables

if TYPE_CHECKING:
    from collections.abc import Iterable

    from loglab_engine.store import Worked

# Worked All States counts the fifty states; the District of Columbia counts as Maryland.
DC: Final = "DC"
# Names a log often gives a country that the DXCC list spells otherwise.
COUNTRY_ALIASES: Final = {
    "UNITED STATES": 291,
    "USA": 291,
    "GERMANY": 230,
    "ASIATIC RUSSIA": 15,
    "EUROPEAN RUSSIA": 54,
}


@dataclass(frozen=True, slots=True)
class BandMode:
    """The operator's QSOs on one band in one mode."""

    band: str
    mode: str
    qsos: int


@dataclass(frozen=True, slots=True)
class Country:
    """A country the operator has worked, as the log names it.

    ``dxcc`` is its DXCC entity number when the log gives one or names an
    entity on the DXCC list; otherwise ``None``, and ``name`` is the name as
    logged.
    """

    name: str
    dxcc: int | None
    qsos: int


@dataclass(frozen=True, slots=True)
class PersonalStats:
    """One operator's stats, as of ``now``.

    ``today``, ``week`` and ``year`` count QSOs made since the start of this
    UTC day, week (from Monday) and year; ``total`` is every QSO they made.
    ``states`` are the US states worked (of ``states_total``) and
    ``states_needed`` the ones not worked yet; ``no_country``
    counts QSOs whose log names no country.
    """

    now: datetime
    today: int
    week: int
    year: int
    total: int
    last_qso: datetime | None
    by_band_mode: tuple[BandMode, ...]
    states: tuple[str, ...]
    states_needed: tuple[str, ...]
    states_total: int
    countries: tuple[Country, ...]
    no_country: int


def _title(name: str) -> str:
    small = {"Of", "And", "The", "De", "Du", "Da"}
    words = name.title().split(" ")
    return " ".join(w.lower() if i and w in small else w for i, w in enumerate(words))


def _country(dxcc: str, country: str) -> tuple[str, int | None] | None:
    """The country a QSO's ``DXCC`` and ``COUNTRY`` fields name, if any."""
    entities = tables().dxcc
    if dxcc.isdigit() and int(dxcc) in entities and int(dxcc) != 0:
        return _title(entities[int(dxcc)]), int(dxcc)
    if not country:
        return None
    number = COUNTRY_ALIASES.get(country)
    if number is None:
        number = next((n for n, name in entities.items() if name == country and n != 0), None)
    if number is not None:
        return _title(entities[number]), number
    return _title(country), None


def personal_stats(qsos: Iterable[Worked], *, now: datetime) -> PersonalStats:
    """Work out an operator's stats from their QSOs."""
    now = now.astimezone(UTC)
    day = now.replace(hour=0, minute=0, second=0, microsecond=0)
    week = day - timedelta(days=day.weekday())
    year = day.replace(month=1, day=1)
    states_all = set(tables().us_states) - {DC}

    today = this_week = this_year = total = no_country = 0
    last: datetime | None = None
    by_band_mode: Counter[tuple[str, str]] = Counter()
    states: set[str] = set()
    countries: Counter[tuple[str, int | None]] = Counter()
    for q in qsos:
        total += 1
        if q.qso_start is not None:
            last = q.qso_start if last is None else max(last, q.qso_start)
            if q.qso_start <= now:
                today += q.qso_start >= day
                this_week += q.qso_start >= week
                this_year += q.qso_start >= year
        by_band_mode[(q.band or "?", q.mode or "?")] += 1
        place = _country(q.dxcc, q.country)
        if place is None:
            no_country += 1
        else:
            countries[place] += 1
        if q.state in states_all and (place is None or (place[1] or 0) in US_DXCC):
            states.add(q.state)
    return PersonalStats(
        now=now,
        today=today,
        week=this_week,
        year=this_year,
        total=total,
        last_qso=last,
        by_band_mode=tuple(BandMode(b, m, n) for (b, m), n in sorted(by_band_mode.items())),
        states=tuple(sorted(states)),
        states_needed=tuple(sorted(states_all - states)),
        states_total=len(states_all),
        countries=tuple(
            Country(name, number, n)
            for (name, number), n in sorted(countries.items(), key=lambda c: (-c[1], c[0][0]))
        ),
        no_country=no_country,
    )


__all__ = ["BandMode", "Country", "PersonalStats", "personal_stats"]
