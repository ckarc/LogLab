# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Station Agents asking to join: the way an Agent gets its station key.

A Station Agent that has no key asks to join as a station. The Engine gives
it a short code, which the Agent shows on its own screen, and a private
ticket for asking how its request is going. An administrator sees the request
in the web client, with the same code, and approves or refuses it (Principle
XIII). Approving makes the station's key (:mod:`loglab_engine.store.agent`),
which the Agent collects once with its ticket and keeps. Nobody has to copy a
key from one computer to another.

A request grants nothing until an administrator approves it. Requests are
kept in memory only, for :data:`LIFETIME`; a restart clears them, and the
Agent simply asks again. A station has at most one request waiting: a new one
replaces it. At most :data:`MAX_WAITING` wait at once, so the list an
administrator sees can't be flooded.
"""

from __future__ import annotations

import hashlib
import hmac
import secrets
import threading
import uuid
from dataclasses import dataclass, replace
from datetime import datetime, timedelta
from typing import Final, Literal

LIFETIME: Final = timedelta(minutes=10)
MAX_WAITING: Final = 20
# No 0/O, 1/I/L, 5/S or 2/Z, so a code read off a screen is typed right.
_ALPHABET: Final = "34679ACDEFGHJKMNPQRTUVWXY"
CODE_LENGTH: Final = 6

State = Literal["waiting", "approved", "refused", "expired"]


class PairingError(Exception):
    """A request can't be made, found, or decided; the message says why."""


@dataclass(frozen=True, slots=True)
class Pairing:
    """One Agent's request to join as a station."""

    id: uuid.UUID
    station: str
    agent: str
    code: str
    ticket_sha256: str
    asked_at: datetime
    state: State = "waiting"
    key: str | None = None
    decided_by: str | None = None


def _digest(ticket: str) -> str:
    return hashlib.sha256(ticket.encode()).hexdigest()


class PairingDesk:
    """The requests waiting or just decided, kept in memory, thread-safe."""

    def __init__(self) -> None:
        """Start with no requests."""
        self._lock = threading.Lock()
        self._pairings: dict[uuid.UUID, Pairing] = {}

    def _tidy(self, now: datetime) -> None:
        for key in [k for k, p in self._pairings.items() if now - p.asked_at > LIFETIME]:
            del self._pairings[key]

    def ask(self, station: str, agent: str, now: datetime) -> tuple[Pairing, str]:
        """File a request for ``station``; return it and the Agent's private ticket.

        Raises:
            PairingError: Too many requests are already waiting.
        """
        ticket = secrets.token_urlsafe(32)
        code = "".join(secrets.choice(_ALPHABET) for _ in range(CODE_LENGTH))
        pairing = Pairing(uuid.uuid4(), station, agent, code, _digest(ticket), now)
        with self._lock:
            self._tidy(now)
            for key in [k for k, p in self._pairings.items() if p.station == station]:
                del self._pairings[key]
            if sum(p.state == "waiting" for p in self._pairings.values()) >= MAX_WAITING:
                msg = "too many stations are waiting to join; try again in a few minutes"
                raise PairingError(msg)
            self._pairings[pairing.id] = pairing
        return pairing, ticket

    def waiting(self, now: datetime) -> tuple[Pairing, ...]:
        """The requests an administrator can still decide, oldest first."""
        with self._lock:
            self._tidy(now)
            found = [p for p in self._pairings.values() if p.state == "waiting"]
        return tuple(sorted(found, key=lambda p: p.asked_at))

    def find(self, pairing_id: uuid.UUID, now: datetime) -> Pairing:
        """A waiting request.

        Raises:
            PairingError: It has expired, was decided, or never existed.
        """
        with self._lock:
            self._tidy(now)
            pairing = self._pairings.get(pairing_id)
        if pairing is None or pairing.state != "waiting":
            msg = "that request has expired or was already decided; the Agent will ask again"
            raise PairingError(msg)
        return pairing

    def decide(self, pairing_id: uuid.UUID, admin: str, key: str | None, now: datetime) -> None:
        """Approve a waiting request with the station's new ``key``, or refuse it with ``None``.

        Raises:
            PairingError: It has expired, was decided, or never existed.
        """
        with self._lock:
            self._tidy(now)
            pairing = self._pairings.get(pairing_id)
            if pairing is None or pairing.state != "waiting":
                msg = "that request has expired or was already decided; the Agent will ask again"
                raise PairingError(msg)
            state: State = "approved" if key is not None else "refused"
            self._pairings[pairing_id] = replace(pairing, state=state, key=key, decided_by=admin)

    def collect(
        self, pairing_id: uuid.UUID, ticket: str, now: datetime
    ) -> tuple[State, str | None]:
        """How the Agent's request stands, and its key once approved.

        The key is handed over once; the request is then forgotten. A wrong
        ticket gets the same answer as a request that never existed.
        """
        with self._lock:
            self._tidy(now)
            pairing = self._pairings.get(pairing_id)
            if pairing is None or not hmac.compare_digest(pairing.ticket_sha256, _digest(ticket)):
                return "expired", None
            if pairing.state != "waiting":
                del self._pairings[pairing_id]
            return pairing.state, pairing.key


__all__ = ["CODE_LENGTH", "LIFETIME", "MAX_WAITING", "Pairing", "PairingDesk", "PairingError"]
