# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Asking a station's Agent to read its radio, for a lab profile.

The Engine never talks to a radio, and it can't call an Agent: each Agent
asks the Engine. So an administrator who wants to make a program's lab
profile from the radio at a station **asks** here; that station's Agent
finds the request the next time it looks (every few seconds), reads the
radio, and **answers** with what it read: every setting it may write, the
profile it puts on for that program now, and which settings changed on the
radio since the Agent started or last put a profile on. The administrator then
chooses what the new profile holds (:mod:`loglab_engine.store.lab_profile`).

Reading changes nothing on the radio (Principle X). Requests and readings
are kept in memory only, for :data:`LIFETIME`; a restart clears them, and the
administrator simply asks again. Only what an administrator saves is kept.
"""

from __future__ import annotations

import threading
import uuid
from dataclasses import dataclass, replace
from datetime import datetime, timedelta
from typing import TYPE_CHECKING, Final, Literal

if TYPE_CHECKING:
    from loglab_engine.store.lab_stock import StockSetting

LIFETIME: Final = timedelta(minutes=10)
# The most requests kept at once, so they can't pile up.
MAX_KEPT: Final = 50

State = Literal["waiting", "read", "failed"]


class ReadError(Exception):
    """A request can't be made, found, or answered; the message says why."""


@dataclass(frozen=True, slots=True)
class RadioRead:
    """What an Agent read from its radio for a request."""

    maker: str
    model: str
    serial: str
    # Every setting the Agent may write, as the radio has it now.
    settings: tuple[StockSetting, ...]
    # The profile the Agent puts on for the program now, and where it is from.
    profile: tuple[StockSetting, ...]
    profile_from: str
    # The ids of settings changed on the radio since the Agent started or
    # last put a profile on.
    changed: tuple[str, ...]
    read_at: datetime


@dataclass(frozen=True, slots=True)
class ReadAsk:
    """An administrator's request that a station's Agent read its radio."""

    id: uuid.UUID
    station: str
    program: str
    asked_by: str
    asked_at: datetime
    state: State = "waiting"
    read: RadioRead | None = None
    problem: str | None = None


class ReadDesk:
    """The requests waiting or just answered, kept in memory, thread-safe."""

    def __init__(self) -> None:
        """Start with no requests."""
        self._lock = threading.Lock()
        self._asks: dict[uuid.UUID, ReadAsk] = {}

    def _tidy(self, now: datetime) -> None:
        for key in [k for k, a in self._asks.items() if now - a.asked_at > LIFETIME]:
            del self._asks[key]

    def ask(self, station: str, program: str, admin: str, now: datetime) -> ReadAsk:
        """Ask ``station``'s Agent to read its radio for ``program``'s profile.

        A request still waiting for that station is replaced.

        Raises:
            ReadError: Too many requests are kept already.
        """
        found = ReadAsk(uuid.uuid4(), station, program, admin, now)
        with self._lock:
            self._tidy(now)
            for key in [
                k for k, a in self._asks.items() if a.station == station and a.state == "waiting"
            ]:
                del self._asks[key]
            if len(self._asks) >= MAX_KEPT:
                msg = "too many radio readings are waiting; try again in a few minutes"
                raise ReadError(msg)
            self._asks[found.id] = found
        return found

    def waiting_for(self, station: str, now: datetime) -> tuple[ReadAsk, ...]:
        """The requests ``station``'s Agent has still to answer, oldest first."""
        with self._lock:
            self._tidy(now)
            found = [
                a for a in self._asks.values() if a.station == station and a.state == "waiting"
            ]
        return tuple(sorted(found, key=lambda a: a.asked_at))

    def answer(
        self,
        station: str,
        ask_id: uuid.UUID,
        now: datetime,
        *,
        read: RadioRead | None = None,
        problem: str | None = None,
    ) -> None:
        """Keep ``station``'s Agent's answer: what it read, or why it couldn't.

        Raises:
            ReadError: No such request is waiting for that station.
        """
        with self._lock:
            self._tidy(now)
            found = self._asks.get(ask_id)
            if found is None or found.station != station or found.state != "waiting":
                msg = "that request has expired or was already answered"
                raise ReadError(msg)
            state: State = "read" if read is not None else "failed"
            self._asks[ask_id] = replace(found, state=state, read=read, problem=problem)

    def get(self, ask_id: uuid.UUID, now: datetime) -> ReadAsk:
        """A request, and its answer once there is one.

        Raises:
            ReadError: It has expired or never existed.
        """
        with self._lock:
            self._tidy(now)
            found = self._asks.get(ask_id)
        if found is None:
            msg = "that radio reading has expired; read the radio again"
            raise ReadError(msg)
        return found


__all__ = ["LIFETIME", "MAX_KEPT", "RadioRead", "ReadAsk", "ReadDesk", "ReadError"]
