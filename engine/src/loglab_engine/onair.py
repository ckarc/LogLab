# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Who is on the air in an operation: each workstation's band and mode right now.

Every signed-in browser in an operation tells the Engine, now and then, the
band and mode its New QSO box is set to. The Engine keeps the latest word
from each operator at each station in memory (signing in again at the same
station replaces the old line), and forgets one it has not heard from in
:data:`STALE`: this is live state, not part of the log, so it is never
written to the database and a restart simply clears it. Every workstation
asks the Engine (Principle II), so they all see the same list, on the local
network with no internet connection (Principle V).

The band and mode are what the operator set, as entered; they are not read
from a radio. Once the Station Agent reports a rig's state, that measured
value takes over (Principle X).
"""

from __future__ import annotations

import threading
import uuid
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Final

# A session not heard from for this long has gone: its browser was closed,
# or the workstation lost the network.
STALE: Final = timedelta(seconds=90)


@dataclass(frozen=True, slots=True)
class Report:
    """What one session last said about its workstation."""

    session_id: uuid.UUID
    operation: uuid.UUID
    operator: str
    station: str
    call: str
    band: str | None
    mode: str | None
    submode: str | None
    seen_at: datetime
    # A pretend workstation's line, from demo mode (Principle XX).
    pretend: bool = False


class OnAir:
    """The latest report from each operator at each station, kept in memory, thread-safe."""

    def __init__(self) -> None:
        """Start with nobody on the air."""
        self._lock = threading.Lock()
        self._reports: dict[tuple[str, str], Report] = {}

    def report(self, report: Report) -> None:
        """Keep ``report`` as its operator's latest at its station, in place of any earlier one."""
        with self._lock:
            self._reports[(report.operator, report.station)] = report

    def leave(self, session_id: uuid.UUID) -> None:
        """Forget a session's line, as when its operator signs out or leaves the operation."""
        with self._lock:
            for key in [k for k, r in self._reports.items() if r.session_id == session_id]:
                del self._reports[key]

    def current(self, operation: uuid.UUID, now: datetime) -> tuple[Report, ...]:
        """The live reports in ``operation``, by operator then station.

        Reports older than :data:`STALE` are dropped.
        """
        with self._lock:
            for key in [k for k, r in self._reports.items() if now - r.seen_at > STALE]:
                del self._reports[key]
            found = [r for r in self._reports.values() if r.operation == operation]
        return tuple(sorted(found, key=lambda r: (r.operator, r.station)))


__all__ = ["STALE", "OnAir", "Report"]
