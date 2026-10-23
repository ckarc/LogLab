# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Demo mode: pretend operators on the air, logging made-up QSOs, for showing LabLog.

An administrator turns it on and off from the Menu. While it is on, a few
pretend workstations (``demo-1``, ``demo-2``, ...) each with a pretend
operator (``Q1PRE``, ``Q2PRE``, ...) and a pretend radio are on the air in a
practice operation, and every so often each logs a pretend QSO with a real
station taken from the local US and Canada license lists (with its real
section), so the Operation stats, On air, map and Station profile boxes have
something to show. With no license list loaded it makes up Q calls, which the
ITU issues to no station.

Everything it makes is marked pretend (Principle XX):

* every QSO carries ``APP_LOGLAB_PRETEND = Y`` and the comment "Pretend QSO",
  and every table shows it as pretend;
* it logs only in a practice operation, and the database refuses a pretend
  QSO anywhere else, so none can reach LoTW or QRZ.com;
* its operators are Q calls, and its On air lines and radios say they are
  pretend.

Its QSOs are logged through the versioned, audited path (Principle VII), in
the session of the administrator who turned it on (Principle XIII): when that
session ends, demo mode stops. It runs only in memory, so a restart of the
Engine turns it off. All times are UTC (Principle VIII); nothing here needs
the internet (Principle V).
"""

# Pretend traffic needs pseudo-randomness, not cryptographic randomness.
# ruff: noqa: S311

from __future__ import annotations

import random
import string
import threading
import uuid
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Final

from loglab_engine.adif import Field, Record
from loglab_engine.contests.stamp import Station, stamp
from loglab_engine.onair import OnAir, Report
from loglab_engine.reference import SECTIONS, section_for
from loglab_engine.rig import Radio, RigBoard, RigMode, RigState
from loglab_engine.store import LogStore, StoreError, connect
from loglab_engine.store.operation import OperationStore

if TYPE_CHECKING:
    from collections.abc import Callable

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.contests.overlay import Overlay
    from loglab_engine.store import Actor
    from loglab_engine.store.operation import Operation

PRETEND_FLAG: Final = "APP_LOGLAB_PRETEND"
COMMENT: Final = "Pretend QSO"
NAME: Final = "Demo Field Day"
WORKSTATIONS: Final = 3
# Each pretend operator logs a QSO about this often, in seconds, on average.
EVERY: Final = 75.0
# The chance a pretend operator changes band or mode after a QSO.
MOVE: Final = 0.1
# How often the pretend workstations say they are on the air, and their
# radios report, in seconds: well inside the On air and rig staleness limits.
TICK: Final = 1.0
REPORT_EVERY: Final = 30
# Each pretend radio transmits for TALK seconds in every CYCLE.
CYCLE: Final = 15
TALK: Final = 4


@dataclass(frozen=True, slots=True)
class Spot:
    """Where a pretend workstation operates: band, ADIF mode, radio mode, and kHz range."""

    band: str
    mode: str
    rig_mode: RigMode
    low_khz: int
    high_khz: int


# Typical Field Day spots: CW, phone and digital on the HF bands, FM on 2 m.
SPOTS: Final[tuple[Spot, ...]] = (
    Spot("20m", "CW", "CW", 14_025, 14_060),
    Spot("20m", "SSB", "USB", 14_225, 14_300),
    Spot("20m", "FT8", "DATA-USB", 14_075, 14_076),
    Spot("40m", "CW", "CW", 7_025, 7_060),
    Spot("40m", "SSB", "LSB", 7_180, 7_280),
    Spot("40m", "FT8", "DATA-USB", 7_075, 7_076),
    Spot("15m", "CW", "CW", 21_025, 21_060),
    Spot("15m", "SSB", "USB", 21_300, 21_400),
    Spot("80m", "CW", "CW", 3_525, 3_560),
    Spot("80m", "SSB", "LSB", 3_850, 3_950),
    Spot("10m", "SSB", "USB", 28_350, 28_500),
    Spot("2m", "FM", "FM", 146_400, 146_580),
)

# Classes as heard on Field Day: mostly 1-3 transmitters, home and club stations.
_CLASSES: Final = ("1A", "1B", "1D", "1E", "2A", "3A", "4A", "1F", "2F", "5A")
_CLASS_WEIGHTS: Final = (20, 8, 25, 15, 8, 6, 3, 3, 2, 1)


def pretend_call(rng: random.Random) -> str:
    """A made-up callsign: ``Q``, a digit, and two or three letters, like ``Q4XYZ``."""
    letters = "".join(rng.choices(string.ascii_uppercase, k=rng.choice((2, 3))))
    return f"Q{rng.randrange(10)}{letters}"


def pretend_qso(
    rng: random.Random,
    spot: Spot,
    frequency_hz: int,
    now: datetime,
    worked: tuple[str, str] | None = None,
) -> Record:
    """A made-up Field Day QSO on ``spot`` at ``now`` (UTC), as an operator would type it.

    ``worked`` is the call and section of a real station to pretend to work;
    without it the call is a made-up Q call in a random section.
    """
    when = now.astimezone(UTC)
    call, section = worked or (pretend_call(rng), rng.choice(SECTIONS).code)
    return Record(
        (
            Field("CALL", call),
            Field("CLASS", rng.choices(_CLASSES, weights=_CLASS_WEIGHTS)[0]),
            Field("ARRL_SECT", section),
            Field("BAND", spot.band),
            Field("MODE", spot.mode),
            Field("FREQ", f"{frequency_hz / 1e6:.6f}"),
            Field("QSO_DATE", when.strftime("%Y%m%d")),
            Field("TIME_ON", when.strftime("%H%M%S")),
            Field("COMMENT", COMMENT),
            Field(PRETEND_FLAG, "Y"),
        )
    )


def real_stations(conn: psycopg.Connection[TupleRow], limit: int = 200) -> list[tuple[str, str]]:
    """Up to ``limit`` active US and Canadian calls from the local license list, with sections.

    Only calls whose state or province is a section of its own are used, so
    the section is the station's real one. The list is local (Principle V);
    an empty list gives none, and demo mode makes up Q calls instead.
    """
    query = (
        "SELECT call, state FROM reference.license {} "
        "WHERE active AND state IS NOT NULL {} LIMIT %s"
    )
    rows = conn.execute(query.format("TABLESAMPLE BERNOULLI (0.2)", ""), (limit,)).fetchall()
    if len(rows) < limit // 4:  # a small list: sampling finds too few
        rows = conn.execute(query.format("", "ORDER BY random()"), (limit,)).fetchall()
    found = []
    for call, state in rows:
        section = section_for(state)
        if section is not None:
            found.append((str(call), section.code))
    return found


def pause(rng: random.Random, mean_seconds: float) -> float:
    """Seconds until the next QSO: random, about ``mean_seconds`` on average, never runaway."""
    return min(rng.expovariate(1 / mean_seconds), 6 * mean_seconds)


def tune(rng: random.Random, spot: Spot) -> int:
    """A frequency in ``spot``, in hertz, on a 100 Hz step."""
    return rng.randrange(spot.low_khz * 10, spot.high_khz * 10 + 1) * 100


@dataclass(slots=True)
class Workstation:
    """One pretend operator at their pretend station."""

    operator: str
    station: str
    session_id: uuid.UUID
    spot: Spot
    frequency_hz: int
    next_qso: float
    logged: int = 0


@dataclass(frozen=True, slots=True)
class DemoState:
    """Whether demo mode is on, where, since when and by whom, and how it is going."""

    on: bool
    operation: uuid.UUID | None = None
    operation_name: str | None = None
    started_by: str | None = None
    started_at: datetime | None = None
    qsos: int = 0
    stations: tuple[str, ...] = ()
    problem: str | None = None


@dataclass(slots=True)
class _Run:
    actor: Actor
    operation: Operation
    overlay: Overlay
    started_at: datetime
    stations: list[Workstation]
    rng: random.Random
    # Real stations still to pretend to work: (call, section).
    worked: list[tuple[str, str]] = field(default_factory=list)
    stop: threading.Event = field(default_factory=threading.Event)
    thread: threading.Thread | None = None


class Demo:
    """Demo mode's pretend workstations, run in a background thread while it is on."""

    def __init__(
        self,
        database: str,
        board: OnAir,
        rigs: RigBoard,
        *,
        clock: Callable[[], datetime] = lambda: datetime.now(UTC),
    ) -> None:
        """Log in ``database`` and put the pretend workstations on ``board`` and ``rigs``."""
        self._database = database
        self._board = board
        self._rigs = rigs
        self._clock = clock
        self._lock = threading.Lock()
        self._run: _Run | None = None
        self._problem: str | None = None

    def state(self) -> DemoState:
        """Whether demo mode is on now, and how it is going."""
        with self._lock:
            run = self._run
            problem = self._problem
        if run is None:
            return DemoState(on=False, problem=problem)
        return DemoState(
            on=True,
            operation=run.operation.id,
            operation_name=run.operation.name,
            started_by=run.actor.operator,
            started_at=run.started_at,
            qsos=sum(s.logged for s in run.stations),
            stations=tuple(s.station for s in run.stations),
        )

    def start(
        self,
        actor: Actor,
        operation: Operation,
        overlay: Overlay,
        *,
        seed: int | None = None,
        background: bool = True,
    ) -> None:
        """Turn demo mode on in ``operation``, a practice one, logging as ``actor``.

        Already on, it carries on in its new operation. ``background`` False
        leaves the ticking to the caller (:meth:`step`), for tests.

        Raises:
            ValueError: ``operation`` is not a practice operation, or has ended.
        """
        if not operation.practice or operation.ended:
            msg = f"demo mode runs only in an open practice operation, and {operation.name} is not"
            raise ValueError(msg)
        self.stop()
        rng = random.Random(seed)
        stations = []
        for n in range(1, WORKSTATIONS + 1):
            spot = rng.choice(SPOTS)
            stations.append(
                Workstation(
                    operator=f"Q{n}PRE",
                    station=f"demo-{n}",
                    session_id=uuid.uuid4(),
                    spot=spot,
                    frequency_hz=tune(rng, spot),
                    next_qso=rng.uniform(5, EVERY / 2),
                )
            )
        run = _Run(actor, operation, overlay, self._clock(), stations, rng)
        with self._lock:
            self._run = run
            self._problem = None
        if background:
            run.thread = threading.Thread(
                target=self._loop, args=(run,), name="demo-mode", daemon=True
            )
            run.thread.start()

    def stop(self, problem: str | None = None) -> None:
        """Turn demo mode off: its workstations leave the air at once."""
        with self._lock:
            run, self._run = self._run, None
            self._problem = problem
        if run is None:
            return
        run.stop.set()
        if run.thread is not None and run.thread is not threading.current_thread():
            run.thread.join(timeout=10)
        for s in run.stations:
            self._board.leave(s.session_id)

    def _loop(self, run: _Run) -> None:
        elapsed = 0.0
        while not run.stop.is_set():
            self._tick(run, elapsed)
            run.stop.wait(TICK)
            elapsed += TICK

    def step(self, elapsed: float) -> None:
        """Do whatever is due ``elapsed`` seconds after demo mode started, if it is on."""
        with self._lock:
            run = self._run
        if run is not None:
            self._tick(run, elapsed)

    def _tick(self, run: _Run, elapsed: float) -> None:
        rng = run.rng
        now = self._clock()
        for n, s in enumerate(run.stations):
            if elapsed >= s.next_qso:
                if not self._log(run, rng, s, now):
                    return
                s.next_qso = elapsed + pause(rng, EVERY)
            ptt = (int(elapsed) + 5 * n) % CYCLE < TALK  # calling CQ now and then
            self._rigs.report(
                RigState(
                    station=s.station,
                    agent="LabLog demo mode",
                    radio=Radio(
                        maker="LabLog",
                        model="Pretend radio",
                        path="pretend",
                        frequency_hz=s.frequency_hz,
                        mode=s.spot.rig_mode,
                        passband_hz=None,
                        ptt=ptt,
                        vfo="VFOA",
                    ),
                    problem=None,
                    heard_at=now,
                )
            )
            if int(elapsed) % REPORT_EVERY == 0:
                self._report(run, s, now)

    def _report(self, run: _Run, s: Workstation, now: datetime) -> None:
        self._board.report(
            Report(
                session_id=s.session_id,
                operation=run.operation.id,
                operator=s.operator,
                station=s.station,
                call=run.operation.station_call,
                band=s.spot.band,
                mode=s.spot.mode,
                submode=None,
                seen_at=now,
                pretend=True,
            )
        )

    def _log(self, run: _Run, rng: random.Random, s: Workstation, now: datetime) -> bool:
        """Log a pretend QSO for ``s``; ``False`` if demo mode had to stop."""
        if s.logged and rng.random() < MOVE:
            s.spot = rng.choice(SPOTS)
            s.frequency_hz = tune(rng, s.spot)
            self._report(run, s, now)
        else:
            s.frequency_hz = min(s.frequency_hz + 100, s.spot.high_khz * 1000)
        op = run.operation
        try:
            with connect(self._database) as conn:
                if OperationStore(conn).get(op.id).ended:
                    self.stop(f"Demo mode stopped: {op.name} has ended.")
                    return False
                if not run.worked:
                    run.worked = real_stations(conn)
                    rng.shuffle(run.worked)
                worked = run.worked.pop() if run.worked else None
                record = stamp(
                    pretend_qso(rng, s.spot, s.frequency_hz, now, worked),
                    run.overlay,
                    Station(op.station_call, s.operator, op.sent, str(op.id)),
                )
                LogStore(conn).create_qso(record, actor=run.actor, note="Demo mode")
        except StoreError as error:
            self.stop(f"Demo mode stopped: {error}")
            return False
        s.logged += 1
        return True


__all__ = [
    "NAME",
    "SPOTS",
    "Demo",
    "DemoState",
    "Spot",
    "pause",
    "pretend_call",
    "pretend_qso",
    "tune",
]
