# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Each station's radio, live, as its Station Agent reports it.

The Station Agent owns the radio (Principle IX) and sends the Engine what the
radio last reported: its frequency, mode, filter width, and whether it is
transmitting. Truth flows radio → Agent → Engine (Principle X): the Engine
keeps the latest report from each station in memory and passes it on to the
browsers signed in at that station, and never sends anything back to the
radio. This is live state, not part of the log, so it is never written to the
database and a restart simply clears it.

A report is forgotten once the Agent has not been heard from in
:data:`STALE`. The Agent sends one at least every few seconds even when
nothing changes, so a silent Agent means it has stopped or lost the network.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Final, Literal

# The Agent sends a report at least this often, even when nothing changes.
HEARTBEAT: Final = timedelta(seconds=5)
# An Agent not heard from for this long has stopped or lost the network.
STALE: Final = timedelta(seconds=15)

RigMode = Literal[
    "USB",
    "LSB",
    "CW",
    "CW-R",
    "RTTY",
    "RTTY-R",
    "AM",
    "FM",
    "DATA-USB",
    "DATA-LSB",
    "DATA-FM",
    "OTHER",
]

# (band, lowest Hz, highest Hz), from the ADIF 3.1.7 band enumeration.
BANDS: Final = (
    ("2190m", 135_700, 137_800),
    ("630m", 472_000, 479_000),
    ("560m", 501_000, 504_000),
    ("160m", 1_800_000, 2_000_000),
    ("80m", 3_500_000, 4_000_000),
    ("60m", 5_060_000, 5_450_000),
    ("40m", 7_000_000, 7_300_000),
    ("30m", 10_100_000, 10_150_000),
    ("20m", 14_000_000, 14_350_000),
    ("17m", 18_068_000, 18_168_000),
    ("15m", 21_000_000, 21_450_000),
    ("12m", 24_890_000, 24_990_000),
    ("10m", 28_000_000, 29_700_000),
    ("8m", 40_000_000, 45_000_000),
    ("6m", 50_000_000, 54_000_000),
    ("5m", 54_000_001, 69_900_000),
    ("4m", 70_000_000, 71_000_000),
    ("2m", 144_000_000, 148_000_000),
    ("1.25m", 222_000_000, 225_000_000),
    ("70cm", 420_000_000, 450_000_000),
    ("33cm", 902_000_000, 928_000_000),
    ("23cm", 1_240_000_000, 1_300_000_000),
    ("13cm", 2_300_000_000, 2_450_000_000),
    ("9cm", 3_300_000_000, 3_500_000_000),
    ("6cm", 5_650_000_000, 5_925_000_000),
    ("3cm", 10_000_000_000, 10_500_000_000),
    ("1.25cm", 24_000_000_000, 24_250_000_000),
    ("6mm", 47_000_000_000, 47_200_000_000),
    ("4mm", 75_500_000_000, 81_000_000_000),
    ("2.5mm", 119_980_000_000, 123_000_000_000),
    ("2mm", 134_000_000_000, 149_000_000_000),
    ("1mm", 241_000_000_000, 250_000_000_000),
    ("submm", 300_000_000_000, 7_500_000_000_000),
)

# The ADIF mode (and submode) for each rig mode. Data modes say only that the
# radio is set up for data; which one (FT8, PSK31...) is the operator's to say.
_ADIF_MODES: Final[dict[str, tuple[str, str | None] | None]] = {
    "USB": ("SSB", "USB"),
    "LSB": ("SSB", "LSB"),
    "CW": ("CW", None),
    "CW-R": ("CW", None),
    "RTTY": ("RTTY", None),
    "RTTY-R": ("RTTY", None),
    "AM": ("AM", None),
    "FM": ("FM", None),
    "DATA-USB": None,
    "DATA-LSB": None,
    "DATA-FM": None,
    "OTHER": None,
}


def band_for(frequency_hz: int) -> str | None:
    """The ADIF band a frequency is in, or ``None`` if it is in none."""
    for band, low, high in BANDS:
        if low <= frequency_hz <= high:
            return band
    return None


def adif_mode(mode: str | None) -> tuple[str, str | None] | None:
    """The ADIF mode and submode for a rig mode, or ``None`` for data and unknown modes."""
    return None if mode is None else _ADIF_MODES.get(mode)


@dataclass(frozen=True, slots=True)
class Radio:
    """A radio and its live state, as the radio last reported it."""

    maker: str
    model: str
    path: str
    frequency_hz: int
    mode: RigMode | None
    passband_hz: int | None
    ptt: bool | None
    vfo: str | None


@dataclass(frozen=True, slots=True)
class RigState:
    """What one station's Agent last said: its radio, or why it has none."""

    station: str
    agent: str
    radio: Radio | None
    problem: str | None
    heard_at: datetime


class RigBoard:
    """The latest report from each station's Agent, kept in memory, thread-safe.

    Every report gets the next number in :attr:`version`, so a stream can tell
    whether anything is new without comparing reports.
    """

    def __init__(self) -> None:
        """Start with no Agent heard from."""
        self._lock = threading.Lock()
        self._states: dict[str, RigState] = {}
        self._versions: dict[str, int] = {}
        self._counter = 0

    def report(self, state: RigState) -> None:
        """Keep ``state`` as its station's latest, in place of the one before."""
        with self._lock:
            before = self._states.get(state.station)
            self._states[state.station] = state
            if before is None or _differs(before, state):
                self._counter += 1
                self._versions[state.station] = self._counter

    def current(self, station: str, now: datetime) -> RigState | None:
        """The station's live state, or ``None`` if its Agent has not been heard lately."""
        with self._lock:
            state = self._states.get(station)
        if state is None or now - state.heard_at > STALE:
            return None
        return state

    def version(self, station: str) -> int:
        """A number that changes whenever the station's radio or problem changes."""
        with self._lock:
            return self._versions.get(station, 0)


def _differs(before: RigState, after: RigState) -> bool:
    return (before.radio, before.problem, before.agent) != (after.radio, after.problem, after.agent)


__all__ = [
    "BANDS",
    "HEARTBEAT",
    "STALE",
    "Radio",
    "RigBoard",
    "RigMode",
    "RigState",
    "adif_mode",
    "band_for",
]
