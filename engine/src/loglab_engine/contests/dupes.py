# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Contest checks: is a QSO a dupe, does it count, and is its exchange well formed.

Everything here is decided by the overlay's data (Principle III): which bands
and modes count, how modes are grouped, and what makes two QSOs the same
contact. The Engine runs the check (Principle I), so every operating position
gets the same answer from the same log (Principle II).

A dupe is reported, never refused: neither Field Day penalizes one, and the
other station may have lost the first QSO.
"""

from __future__ import annotations

import re
import uuid
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from datetime import datetime
from typing import TYPE_CHECKING, Literal

from loglab_engine.reference import section_for

if TYPE_CHECKING:
    from loglab_engine.contests.overlay import ExchangePart, ModeGroup, Overlay

Status = Literal["new", "dupe", "not_counted", "no_check", "incomplete"]


@dataclass(frozen=True, slots=True)
class Worked:
    """An earlier QSO with the same call in the same contest."""

    qso_id: uuid.UUID
    band: str | None
    group: ModeGroup | None
    qso_start: datetime | None
    operator: str


@dataclass(frozen=True, slots=True)
class Earlier:
    """What the check needs to know about an earlier QSO."""

    qso_id: uuid.UUID
    fields: Mapping[str, str]
    qso_start: datetime | None
    operator: str


@dataclass(frozen=True, slots=True)
class Problem:
    """A part of the received exchange that does not look right."""

    field: str
    label: str
    message: str


@dataclass(frozen=True, slots=True)
class Check:
    """The outcome of checking one QSO against the log.

    ``dupes`` are the earlier QSOs that make it a dupe; ``worked`` is every
    earlier QSO with the same call in the same contest, so an operator can
    see which bands and modes are still open.
    """

    status: Status
    band: str | None
    group: ModeGroup | None
    dupes: tuple[Worked, ...]
    worked: tuple[Worked, ...]
    problems: tuple[Problem, ...]
    message: str


def _get(fields: Mapping[str, str], name: str) -> str | None:
    value = fields.get(name)
    return value.strip() if value and value.strip() else None


def park_key(fields: Mapping[str, str], per: Iterable[str]) -> tuple[str, str]:
    """The parks a dupe rule compares: the station's own and the other station's, as listed."""
    per = tuple(per)
    mine = (_get(fields, "MY_SIG_INFO") or "").upper() if "my_park" in per else ""
    theirs = (_get(fields, "SIG_INFO") or "").upper() if "their_park" in per else ""
    return mine, theirs


def classify(overlay: Overlay, fields: Mapping[str, str]) -> tuple[str | None, ModeGroup | None]:
    """The band and mode group a QSO counts on in ``overlay``; ``None`` where it does not count.

    ``fields`` maps upper-case ADIF names to values.
    """
    if overlay.bands is None or overlay.modes is None:
        return None, None
    band = overlay.bands.key(_get(fields, "BAND"), _get(fields, "PROP_MODE"))
    group = overlay.modes.group(_get(fields, "MODE"), _get(fields, "SUBMODE"))
    return band, group


def _part_ok(part: ExchangePart, value: str) -> bool:
    value = value.strip().upper()
    if value in part.also:
        return True
    if part.sections and section_for(value) is not None:
        return True
    return part.pattern is not None and re.fullmatch(part.pattern, value) is not None


def exchange_problems(overlay: Overlay, fields: Mapping[str, str]) -> tuple[Problem, ...]:
    """The parts of the received exchange that are missing or not well formed."""
    if overlay.exchange is None:
        return ()
    problems: list[Problem] = []
    for part in overlay.exchange.received:
        name = part.field or ""
        value = _get(fields, name)
        if value is None:
            if not part.optional:
                problems.append(Problem(name, part.label, f"{part.label} is missing."))
        elif not _part_ok(part, value):
            problems.append(Problem(name, part.label, f"{value} is not a valid {part.label}."))
    return tuple(problems)


def sent_problems(overlay: Overlay, values: Mapping[str, str]) -> tuple[Problem, ...]:
    """The parts of an operation's sent exchange that are missing or not well formed."""
    if overlay.exchange is None:
        return ()
    problems: list[Problem] = []
    for part in overlay.exchange.sent:
        value = (values.get(part.key) or "").strip()
        if not value and part.optional:
            continue
        if not value:
            problems.append(Problem(part.key, part.label, f"{part.label} is missing."))
        elif not _part_ok(part, value):
            problems.append(Problem(part.key, part.label, f"{value} is not a valid {part.label}."))
    return tuple(problems)


def check(overlay: Overlay, fields: Mapping[str, str], earlier: Iterable[Earlier]) -> Check:
    """Check a QSO, given by its ``fields``, against ``earlier`` QSOs with the same call.

    ``earlier`` must already be limited to the same call, station callsign,
    contest and year (see :meth:`loglab_engine.store.LogStore.contest_qsos`),
    or for a program like POTA, the same program and UTC day
    (:meth:`loglab_engine.store.LogStore.program_qsos`).
    """
    if not overlay.checks_dupes or overlay.dupes is None:
        return Check("no_check", None, None, (), (), (), f"{overlay.name} doesn't check for dupes.")
    problems = exchange_problems(overlay, fields)
    band, group = classify(overlay, fields)
    call = _get(fields, "CALL")
    per = overlay.dupes.per
    parks = park_key(fields, per)
    pairs = [
        (
            Worked(e.qso_id, *classify(overlay, e.fields), e.qso_start, e.operator),
            park_key(e.fields, per),
        )
        for e in earlier
    ]
    worked = tuple(w for w, _ in pairs)
    if call is None or _get(fields, "BAND") is None or _get(fields, "MODE") is None:
        return Check(
            "incomplete", band, group, (), worked, problems, "Enter the call, band and mode."
        )
    if band is None or group is None:
        what = (
            _get(fields, "BAND")
            if band is None
            else _get(fields, "SUBMODE") or _get(fields, "MODE")
        )
        message = f"{what} doesn't count in {overlay.name}."
        return Check("not_counted", band, group, (), worked, problems, message)
    dupes = tuple(
        w
        for w, where in pairs
        if where == parks
        and w.band is not None
        and w.group is not None
        and ("band" not in per or w.band == band)
        and ("mode" not in per or w.group == group)
    )
    if dupes:
        message = f"Dupe: {call.upper()} was already worked on {band} {group}."
        return Check("dupe", band, group, dupes, worked, problems, message)
    before = sorted({f"{w.band} {w.group}" for w in worked if w.band and w.group})
    if before:
        message = f"New: {call.upper()} on {band} {group}. Already worked on " + ", ".join(before)
        return Check("new", band, group, (), worked, problems, message + ".")
    return Check(
        "new", band, group, (), worked, problems, f"New: {call.upper()} on {band} {group}."
    )


__all__ = [
    "Check",
    "Earlier",
    "Problem",
    "Status",
    "Worked",
    "check",
    "classify",
    "exchange_problems",
    "park_key",
    "sent_problems",
]
