# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operation stats: how the whole group is doing in an operation, like Field Day.

The Engine works these out from the log (Principles I and II), so every
operating position shows the same numbers. What counts, what is a dupe, and
what a QSO is worth come from the overlay's data (Principle III). Everything
is worked out from the local log, with no internet connection (Principle V).

A QSO is a dupe here when an earlier QSO in the same operation has the same
call, station callsign and UTC year (UTC day, for a program like POTA), and
agrees on the band, mode group and parks where the overlay's dupe rule says
it must: the same test the dupe check makes before a QSO is stored. The first
QSO counts; later ones are dupes.

For a program like POTA, each station callsign's QSOs from each park on each
UTC day are counted toward an activation: dupes do not count, and the park
is activated once it has the program's goal (ten, for POTA). A club call
operated by several operators is one station callsign, so all their QSOs
count together; an operator logging under their own call counts on their own.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from datetime import UTC, date, datetime, timedelta
from typing import TYPE_CHECKING, Final

from loglab_engine.contests.dupes import classify, park_key
from loglab_engine.reference import SECTIONS, section_for

if TYPE_CHECKING:
    from collections.abc import Iterable, Mapping

    from loglab_engine.contests.overlay import Overlay

HOUR: Final = timedelta(hours=1)
RATE_WINDOW: Final = timedelta(minutes=10)
# The hourly chart shows at most this many hours, the latest ones: two days
# for an operation with no start time, a week for one with hours.
MAX_HOURS: Final = 48
MAX_HOURS_SET: Final = 7 * 24


@dataclass(frozen=True, slots=True)
class Logged:
    """What the stats need to know about one QSO in the operation.

    ``fields`` maps upper-case ADIF names to values; ``operator`` is who
    logged it, used when the QSO has no ``OPERATOR``.
    """

    fields: Mapping[str, str]
    qso_start: datetime | None
    operator: str


@dataclass(frozen=True, slots=True)
class Hour:
    """The QSOs made in one UTC hour, starting at ``start``."""

    start: datetime
    qsos: int


@dataclass(slots=True)
class OperatorStats:
    """One operator's part in the operation."""

    operator: str
    qsos: int = 0
    last_hour: int = 0
    points: int = 0


@dataclass(frozen=True, slots=True)
class BandMode:
    """Counting QSOs (dupes left out) on one band in one mode group."""

    band: str
    mode: str
    qsos: int
    points: int


@dataclass(frozen=True, slots=True)
class Activation:
    """One station callsign's QSOs from one park on one UTC day, toward an activation."""

    station_call: str
    park: str
    day: date
    qsos: int
    goal: int
    p2p: int

    @property
    def done(self) -> bool:
        """Whether the park is activated: it has the goal's QSOs."""
        return self.qsos >= self.goal


@dataclass(frozen=True, slots=True)
class Stats:
    """An operation's stats, as of ``now``.

    ``qsos`` is every QSO logged in it; of those, ``outside`` were made
    outside its hours, ``not_counted`` on a band or mode the overlay leaves
    out, and ``dupes`` repeat an earlier contact. ``counted`` are the rest.
    ``last_hour`` and ``last_10`` count QSOs made in the hour and ten minutes
    up to ``now``; ``best_hour`` is the hour with the most. ``score`` is
    ``None`` when the overlay gives no points.
    """

    now: datetime
    qsos: int
    counted: int
    outside: int
    not_counted: int
    dupes: int
    last_hour: int
    last_10: int
    hourly: tuple[Hour, ...]
    best_hour: Hour | None
    by_band_mode: tuple[BandMode, ...]
    by_mode: Mapping[str, int]
    operators: tuple[OperatorStats, ...]
    sections_worked: int
    sections_total: int
    score: int | None
    points: Mapping[str, int] = field(default_factory=dict)
    activations: tuple[Activation, ...] = ()
    p2p: int = 0


def _get(fields: Mapping[str, str], name: str) -> str:
    return (fields.get(name) or "").strip().upper()


def _hour(when: datetime) -> datetime:
    return when.astimezone(UTC).replace(minute=0, second=0, microsecond=0)


def _hours(
    made: Iterable[datetime], starts_at: datetime | None, ends_at: datetime | None, now: datetime
) -> tuple[Hour, ...]:
    """QSOs in each hour of the operation so far, quiet hours included."""
    made = sorted(made)
    first = starts_at or (made[0] if made else None)
    if first is None:
        return ()
    last = min(now, ends_at) if ends_at else now
    if made and starts_at is None:
        last = max(last, made[-1])
    start, end = _hour(first), _hour(last)
    if end < start:
        return ()
    most = MAX_HOURS if starts_at is None else MAX_HOURS_SET
    start = max(start, end - (most - 1) * HOUR)
    counts = Counter(_hour(m) for m in made)
    out: list[Hour] = []
    at = start
    while at <= end:
        out.append(Hour(at, counts.get(at, 0)))
        at += HOUR
    return tuple(out)


def operation_stats(  # noqa: PLR0912, PLR0915 - one pass over the operation's QSOs
    overlay: Overlay | None,
    qsos: Iterable[Logged],
    *,
    starts_at: datetime | None,
    ends_at: datetime | None,
    now: datetime,
) -> Stats:
    """Work out an operation's stats from its QSOs, oldest first.

    With no ``overlay`` (it was removed), bands and modes are taken as logged
    and nothing is a dupe or scores.
    """
    stats = overlay.stats if overlay is not None else None
    points = {str(k): v for k, v in (stats.points or {}).items()} if stats is not None else {}
    satellite_points = stats.satellite_points if stats is not None else True
    contest = overlay is not None and overlay.bands is not None and overlay.modes is not None
    per = overlay.dupes.per if overlay is not None and overlay.dupes is not None else None

    total = outside = not_counted = dupes = last_hour = last_10 = 0
    made: list[datetime] = []
    seen: set[tuple[str, ...]] = set()
    by_band_mode: Counter[tuple[str, str]] = Counter()
    band_mode_points: Counter[tuple[str, str]] = Counter()
    by_mode: Counter[str] = Counter()
    operators: dict[str, OperatorStats] = {}
    sections: set[str] = set()
    program = overlay.program if overlay is not None else None
    activations: Counter[tuple[str, str, date]] = Counter()
    p2p_by: Counter[tuple[str, str, date]] = Counter()
    p2p = 0
    score = 0
    for q in qsos:
        total += 1
        who = _get(q.fields, "OPERATOR") or q.operator.upper()
        op = operators.setdefault(who, OperatorStats(who))
        op.qsos += 1
        if q.qso_start is not None:
            made.append(q.qso_start)
            if now - HOUR < q.qso_start <= now:
                last_hour += 1
                op.last_hour += 1
            if now - RATE_WINDOW < q.qso_start <= now:
                last_10 += 1
        if q.qso_start is not None and not (
            (starts_at is None or q.qso_start >= starts_at)
            and (ends_at is None or q.qso_start <= ends_at)
        ):
            outside += 1
            continue
        band: str | None
        group: str | None
        if contest and overlay is not None:
            band, group = classify(overlay, q.fields)
            if band is None or group is None:
                not_counted += 1
                continue
        else:
            band, group = _get(q.fields, "BAND").lower() or "?", _get(q.fields, "MODE") or "?"
        if per is not None and overlay is not None and overlay.dupes is not None:
            if q.qso_start is None:
                when = ""
            elif overlay.dupes.within == "day":
                when = q.qso_start.astimezone(UTC).date().isoformat()
            else:
                when = str(q.qso_start.year)
            key = (
                _get(q.fields, "CALL"),
                _get(q.fields, "STATION_CALLSIGN"),
                when,
                band if "band" in per else "",
                group if "mode" in per else "",
                *park_key(q.fields, per),
            )
            if key in seen:
                dupes += 1
                continue
            seen.add(key)
        by_band_mode[(band, group)] += 1
        by_mode[group] += 1
        if program is not None and q.qso_start is not None:
            park = _get(q.fields, "MY_SIG_INFO")
            if park:
                where = (
                    _get(q.fields, "STATION_CALLSIGN") or who,
                    park,
                    q.qso_start.astimezone(UTC).date(),
                )
                activations[where] += 1
                if _get(q.fields, "SIG_INFO"):
                    p2p_by[where] += 1
                    p2p += 1
        section = section_for(_get(q.fields, "ARRL_SECT"))
        if section is not None:
            sections.add(section.code)
        if points and (band != "SAT" or satellite_points):
            worth = points.get(group, 0)
            score += worth
            op.points += worth
            band_mode_points[(band, group)] += worth
    hourly = _hours(made, starts_at, ends_at, now)
    best = max(hourly, key=lambda h: h.qsos, default=None)
    return Stats(
        now=now,
        qsos=total,
        counted=total - outside - not_counted - dupes,
        outside=outside,
        not_counted=not_counted,
        dupes=dupes,
        last_hour=last_hour,
        last_10=last_10,
        hourly=hourly,
        best_hour=best if best is not None and best.qsos else None,
        by_band_mode=tuple(
            BandMode(b, g, n, band_mode_points[(b, g)])
            for (b, g), n in sorted(by_band_mode.items())
        ),
        by_mode=dict(sorted(by_mode.items())),
        operators=tuple(sorted(operators.values(), key=lambda o: (-o.qsos, o.operator))),
        sections_worked=len(sections),
        sections_total=len(SECTIONS),
        score=score if points else None,
        points=points,
        activations=tuple(
            Activation(call, park, day, n, program.goal, p2p_by[(call, park, day)])
            for (call, park, day), n in sorted(
                activations.items(), key=lambda kv: (kv[0][2], kv[0][0], kv[0][1]), reverse=True
            )
        )
        if program is not None
        else (),
        p2p=p2p,
    )


__all__ = ["Activation", "BandMode", "Hour", "Logged", "OperatorStats", "Stats", "operation_stats"]
