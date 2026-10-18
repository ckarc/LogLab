# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Sending logged QSOs to QRZ.com logbooks, in the background.

Logging never waits on this (Principle V): a QSO is logged when the Engine
commits it (Principle XV), and only afterwards does this copy it to QRZ.com.
It works from what the log says now, so nothing is queued that could be lost:
each run sends every QSO whose current version QRZ.com does not have yet. A
new QSO is inserted; an edited one takes the place of its earlier copy; a deleted one is
removed from the QRZ logbook, and sent again if it is restored.

Only LogLab's own copies are ever touched, by the logid QRZ.com gave them. An
edited QSO's old copy is removed and the new one inserted; LogLab never asks
QRZ.com to replace a matching QSO, so one that reached QRZ.com another way
(from LoTW, or an earlier upload) is never changed or removed.

It runs when a QSO changes, and while anything is still waiting, again every
few minutes. With no internet it makes one try, then waits; nothing is lost.
"""

from __future__ import annotations

import logging
import threading
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

import psycopg

from loglab_engine.adif import AdifDocument, Field, Record, write_adi
from loglab_engine.callbook.qrz import QrzReplyError, QrzUnreachableError, clean_text
from loglab_engine.callbook.qrz_logbook import InsertOutcome, QrzKeyError, QrzLogbookClient
from loglab_engine.store import LogStore, connect
from loglab_engine.store.qrz_logbook import Logbook, LogbookStore, PendingQso

if TYPE_CHECKING:
    from collections.abc import Callable

    from psycopg.rows import TupleRow

RETRY_SECONDS: Final = 300.0
_LOCK_KEY: Final = 0x51525A4C6F67  # "QRZLog": one sender at a time
_log = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class BookState:
    """What happened the last time a logbook was worked on."""

    problem: str | None
    at: float


def _adif(record: Record, book_call: str) -> str:
    """One QSO as an ADIF record, naming the logbook's callsign if it does not already."""
    fields = record.fields
    if not any(f.name.upper() == "STATION_CALLSIGN" for f in fields):
        fields = (*fields, Field("STATION_CALLSIGN", book_call))
    return write_adi(AdifDocument(None, (Record(fields),))).decode("utf-8").strip()


class LogbookSync:
    """Copies QSOs to the QRZ logbooks operators saved keys for."""

    def __init__(
        self,
        database: str,
        client: QrzLogbookClient,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        """Use the database ``database`` and ``client``; ``clock`` is monotonic seconds."""
        self._database = database
        self.client = client
        self._clock = clock
        self._wake = threading.Event()
        self._states: dict[str, BookState] = {}
        self._lock = threading.Lock()
        self._thread: threading.Thread | None = None

    def state(self, book_call: str) -> BookState | None:
        """The last problem sending to ``book_call``'s logbook, if the last run had one."""
        with self._lock:
            return self._states.get(book_call)

    def _note(self, book_call: str, problem: str | None) -> None:
        with self._lock:
            self._states[book_call] = BookState(problem, self._clock())

    def wake(self) -> None:
        """Ask the background sender to run soon (a QSO changed, or a key was saved)."""
        self._wake.set()

    def start(self) -> None:
        """Run the background sender until the process ends."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, name="qrz-logbook", daemon=True)
            self._thread.start()

    def _loop(self) -> None:
        waiting = True
        while True:
            self._wake.wait(timeout=RETRY_SECONDS if waiting else None)
            self._wake.clear()
            try:
                waiting = self.run_once()
            except psycopg.Error:
                _log.exception("QRZ logbook sender: database error; trying again later")
                waiting = True

    def run_once(self) -> bool:
        """Send everything waiting; return whether anything is still waiting."""
        with connect(self._database) as conn:
            locked = conn.execute("SELECT pg_try_advisory_lock(%s)", (_LOCK_KEY,)).fetchone()
            if locked is None or locked[0] is not True:
                return True
            store = LogbookStore(conn)
            waiting = False
            for book in store.all():
                waiting = self._send_book(conn, store, book) or waiting
            return waiting

    def _send_book(
        self, conn: psycopg.Connection[TupleRow], store: LogbookStore, book: Logbook
    ) -> bool:
        log = LogStore(conn)
        while batch := store.pending(book):
            for qso in batch:
                try:
                    self._send(log, store, book, qso)
                except QrzUnreachableError:
                    self._note(book.book_call, "QRZ.com can't be reached right now.")
                    return True
                except QrzKeyError as error:
                    self._note(book.book_call, f"QRZ.com refused the API key: {error}")
                    return True
                except QrzReplyError as error:
                    self._note(book.book_call, f"QRZ.com's answer was not usable: {error}")
                    return True
        self._note(book.book_call, None)
        return False

    def _send(self, log: LogStore, store: LogbookStore, book: Logbook, qso: PendingQso) -> None:
        if qso.deleted:
            if qso.logid is not None:
                self.client.delete(book.api_key, qso.logid)
            store.record(qso, book, outcome="deleted", logid=None)
            return
        if qso.logid is not None:
            # An edit or a restore: remove LogLab's own earlier copy by its logid,
            # then send the QSO as it stands. LogLab never asks QRZ.com to replace
            # a matching QSO, so a QSO that reached QRZ.com another way (from LoTW,
            # say) is never changed or removed (Principle VII).
            self.client.delete(book.api_key, qso.logid)
        record = log.current(qso.qso_id).record
        answer = self.client.insert(book.api_key, _adif(record, book.book_call))
        if answer.outcome is InsertOutcome.REFUSED:
            reason = clean_text(answer.reason, 300) or "refused"
            store.record(qso, book, outcome="refused", logid=None, message=reason)
            return
        store.record(qso, book, outcome="sent", logid=answer.logid)
