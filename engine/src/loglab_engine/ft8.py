# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""What each station's Station Agent hears on FT8, live.

The Agent listens to FT8 audio, draws it as a waterfall, and decodes each
15-second slot with LogLab's own decoder. It sends the Engine the waterfall
a few rows at a time and each slot's decodes once the slot is over. Like the
radio's live state (:mod:`loglab_engine.rig`), this is kept in memory only,
never written to the log, and a restart clears it.

Every decode is labelled with how it was obtained (Principle XX). Today
every one is ``received``: it came from the signal and passed the decoder's
checks. Where the audio came from is kept too: ``pretend`` audio is the
Agent's pretend FT8 band, made-up stations for trying LabLog with no radio,
and it is never presented as real.

:func:`parse_message` reads a standard FT8 message for who sent it, to whom,
and any grid or report. That is reading the decoded text, not guessing: a
message that does not fit the standard shapes is left unparsed.
"""

from __future__ import annotations

import re
import threading
from collections import deque
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Final, Literal

# Rows of waterfall the Agent draws each second, and in one 15-second slot.
ROWS_PER_SECOND: Final = 2
ROWS_PER_SLOT: Final = 30
# The waterfall's columns: one per 6.25 Hz from 0 to 3,000 Hz.
COLUMNS: Final = 480
COLUMN_HZ: Final = 6.25
SLOT_SECONDS: Final = 15
# How much the Engine keeps for each station: two minutes of waterfall and
# the decodes of the last ten minutes.
KEEP_ROWS: Final = 240
KEEP_SLOTS: Final = 40
# An Agent that sends nothing for this long has stopped listening.
STALE: Final = timedelta(seconds=20)

Kind = Literal["received", "assisted", "predicted"]
Source = Literal["pretend"]

# A standard call. A hashed call (<...>) is not read: it is recalled from an
# earlier message, not sent in full, so it is left as text.
_CALL: Final = r"[A-Z0-9]{1,4}[0-9][A-Z0-9]{0,4}(?:/[A-Z0-9]+)?"
_GRID: Final = re.compile(r"^[A-R]{2}[0-9]{2}$")
_REPORT: Final = re.compile(r"^R?[+-][0-9]{2}$")
_CALL_ONLY: Final = re.compile(rf"^{_CALL}$")
_ENDINGS: Final = frozenset({"RRR", "RR73", "73"})
# A standard message starts with two calls: to, then from.
_TWO: Final = 2


@dataclass(frozen=True, slots=True)
class Parsed:
    """What a standard FT8 message says, read from its text.

    ``de`` is the station sending it and ``to`` the station it is for (or
    ``None`` for a CQ). ``grid`` is the sender's grid square if the message
    gives one; ``report`` is the signal report the sender gives, like
    ``-12``, and ``roger`` says it came with an R. ``cq`` is the CQ's
    modifier when it has one (``DX``, ``POTA``) or an empty string.
    """

    de: str | None
    to: str | None
    cq: str | None
    grid: str | None
    report: str | None
    roger: bool
    ending: str | None


_NOTHING: Final = Parsed(None, None, None, None, None, roger=False, ending=None)


def parse_message(message: str) -> Parsed:
    """Read who sent a standard FT8 message, to whom, and what it carries."""
    words = message.split()
    if words[:1] == ["CQ"]:
        return _parse_cq(words[1:])
    calls, extra = words[:_TWO], words[_TWO : _TWO + 1]
    if len(calls) < _TWO or not all(_CALL_ONLY.match(c) for c in calls):
        return _NOTHING
    to, de = calls
    said = extra[0] if extra else ""
    grid = said if _GRID.match(said) and said != "RR73" else None
    report = said.removeprefix("R") if _REPORT.match(said) else None
    ending = said if said in _ENDINGS else None
    roger = report is not None and said.startswith("R")
    return Parsed(de, to, None, grid, report, roger=roger, ending=ending)


def _parse_cq(rest: list[str]) -> Parsed:
    """A CQ: ``CQ CALL GRID``, maybe with a modifier like ``CQ DX CALL GRID``."""
    modifier = ""
    if len(rest) >= _TWO and not _CALL_ONLY.match(rest[0]) and _CALL_ONLY.match(rest[1]):
        modifier, rest = rest[0], rest[1:]
    if not rest or not _CALL_ONLY.match(rest[0]):
        return _NOTHING
    grid = rest[1] if len(rest) > 1 and _GRID.match(rest[1]) else None
    return Parsed(rest[0], None, modifier, grid, None, roger=False, ending=None)


def slot_start(slot: int) -> datetime:
    """When slot number ``slot`` began, in UTC (Principle VIII)."""
    return datetime.fromtimestamp(slot * SLOT_SECONDS, UTC)


@dataclass(frozen=True, slots=True)
class Decode:
    """One message decoded in a slot, as the decoder measured it."""

    message: str
    snr_db: float
    dt_s: float
    base_hz: float
    kind: Kind


@dataclass(frozen=True, slots=True)
class Row:
    """One row of waterfall: a hex byte per column, dark to bright."""

    seq: int
    slot: int
    row: int
    levels: str


@dataclass(frozen=True, slots=True)
class SlotDecodes:
    """Everything decoded in one slot."""

    seq: int
    slot: int
    decodes: tuple[Decode, ...]


@dataclass(frozen=True, slots=True)
class Heard:
    """What a station's Agent has sent since ``after``, and how it is listening."""

    agent: str
    source: Source
    dial_hz: int
    heard_at: datetime
    seq: int
    rows: tuple[Row, ...]
    slots: tuple[SlotDecodes, ...]


class _Station:
    def __init__(self, agent: str, source: Source, dial_hz: int, now: datetime) -> None:
        self.agent = agent
        self.source: Source = source
        self.dial_hz = dial_hz
        self.heard_at = now
        self.rows: deque[Row] = deque(maxlen=KEEP_ROWS)
        self.slots: deque[SlotDecodes] = deque(maxlen=KEEP_SLOTS)


class Ft8Board:
    """What each station's Agent hears on FT8, kept in memory, thread-safe.

    Every row and every slot of decodes gets the next number, so a browser
    asks only for what came after the last one it has.
    """

    def __init__(self) -> None:
        """Start with nothing heard."""
        self._lock = threading.Lock()
        self._stations: dict[str, _Station] = {}
        self._seq = 0

    def report(  # noqa: PLR0913 - one report's parts, all keyword-only
        self,
        station: str,
        *,
        agent: str,
        source: Source,
        dial_hz: int,
        now: datetime,
        rows: tuple[int, int, tuple[str, ...]] | None = None,
        decodes: tuple[int, tuple[Decode, ...]] | None = None,
    ) -> None:
        """Keep what the Agent at ``station`` just heard.

        ``rows`` is a slot, its first row's number and the rows; ``decodes``
        is a slot and its decodes. A change of source or dial frequency
        starts the station afresh, so nothing heard before is shown as heard
        on the new frequency.
        """
        with self._lock:
            kept = self._stations.get(station)
            if kept is None or (kept.source, kept.dial_hz) != (source, dial_hz):
                kept = self._stations[station] = _Station(agent, source, dial_hz, now)
            kept.agent = agent
            kept.heard_at = now
            if rows is not None:
                slot, first, levels = rows
                for n, line in enumerate(levels):
                    self._seq += 1
                    kept.rows.append(Row(self._seq, slot, first + n, line))
            if decodes is not None:
                slot, found = decodes
                self._seq += 1
                kept.slots.append(SlotDecodes(self._seq, slot, found))

    def heard(self, station: str, now: datetime, after: int = 0) -> Heard | None:
        """What ``station``'s Agent sent after number ``after``; ``None`` if it isn't listening."""
        with self._lock:
            kept = self._stations.get(station)
            if kept is None or now - kept.heard_at > STALE:
                return None
            return Heard(
                agent=kept.agent,
                source=kept.source,
                dial_hz=kept.dial_hz,
                heard_at=kept.heard_at,
                seq=self._seq,
                rows=tuple(r for r in kept.rows if r.seq > after),
                slots=tuple(s for s in kept.slots if s.seq > after),
            )


__all__ = [
    "COLUMNS",
    "COLUMN_HZ",
    "ROWS_PER_SECOND",
    "ROWS_PER_SLOT",
    "STALE",
    "Decode",
    "Ft8Board",
    "Heard",
    "Kind",
    "Parsed",
    "Row",
    "SlotDecodes",
    "Source",
    "parse_message",
    "slot_start",
]
