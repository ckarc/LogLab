# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Looking a call up: the Engine's cache first, then QRZ.com with the operator's own login.

A lookup never blocks logging and never fails a request (Principle V): when
QRZ.com is not set up, cannot be reached, or refuses the login, the answer says
so and the operator logs as usual. Every answer QRZ.com gives, found or not, is
kept, so the same call is not fetched twice unless an operator asks for a fresh
copy (Principle VI). After QRZ.com cannot be reached, the Engine does not try
it again for a minute, so a station with no internet is never kept waiting.
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Final

from loglab_engine.callbook.qrz import (
    CallbookEntry,
    QrzClient,
    QrzLoginError,
    QrzReplyError,
    QrzSessionError,
    QrzUnreachableError,
)
from loglab_engine.store.callbook import CallbookStore

if TYPE_CHECKING:
    from collections.abc import Callable
    from datetime import datetime

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.callbook import CachedEntry, CallbookLogin
    from loglab_engine.store.log import Actor

SOURCE: Final = "qrz"
RETRY_AFTER_SECONDS: Final = 60.0


class LookupStatus(StrEnum):
    """How a lookup turned out."""

    FOUND = "found"
    NOT_FOUND = "not_found"
    NOT_SET_UP = "not_set_up"
    UNREACHABLE = "unreachable"
    LOGIN_FAILED = "login_failed"


@dataclass(frozen=True, slots=True)
class LookupResult:
    """A lookup's answer, and where it came from."""

    status: LookupStatus
    call: str
    entry: CallbookEntry | None
    cached: bool
    fetched_at: datetime | None
    message: str


@dataclass(frozen=True, slots=True)
class LoginCheck:
    """What happened when a saved QRZ.com login was tried.

    ``status`` is ``None`` when QRZ.com accepted it; ``note`` is what QRZ.com
    said: its reason for refusing, or a notice such as a missing subscription.
    """

    status: LookupStatus | None
    note: str | None


class Link:
    """The Engine's one connection to QRZ.com, shared by every request.

    It remembers when QRZ.com last could not be reached, so requests in the
    next minute skip it instead of each waiting for a timeout.
    """

    def __init__(self, client: QrzClient, clock: Callable[[], float] = time.monotonic) -> None:
        """Use ``client``; ``clock`` is a monotonic clock in seconds."""
        self.client = client
        self._clock = clock
        self._lock = threading.Lock()
        self._down_until = 0.0

    def down(self) -> bool:
        """Whether QRZ.com could not be reached within the last minute."""
        with self._lock:
            return self._clock() < self._down_until

    def mark_down(self) -> None:
        """Record that QRZ.com could not be reached just now."""
        with self._lock:
            self._down_until = self._clock() + RETRY_AFTER_SECONDS

    def mark_up(self) -> None:
        """Record that QRZ.com answered."""
        with self._lock:
            self._down_until = 0.0


_UNREACHABLE: Final = "QRZ.com can't be reached right now. Log as usual."


class Callbook:
    """Lookups and QRZ.com logins for the operator a request is made by."""

    def __init__(self, conn: psycopg.Connection[TupleRow], link: Link) -> None:
        """Use ``conn``, an autocommit connection, and the Engine's ``link`` to QRZ.com."""
        self._store = CallbookStore(conn)
        self._link = link

    def lookup(self, actor: Actor, call: str, *, refresh: bool = False) -> LookupResult:
        """Look ``call`` (already validated, upper case) up for ``actor``."""
        cached = self._store.cached(SOURCE, call)
        if cached is not None and not refresh:
            return _from_cache(call, cached, "")
        login = self._store.login(actor, SOURCE)
        if login is None:
            return self._fallback(
                LookupStatus.NOT_SET_UP,
                call,
                cached,
                "Set up QRZ.com to fill in names and places.",
            )
        return self._fetch(actor, login, call, cached)

    def _fetch(
        self, actor: Actor, login: CallbookLogin, call: str, cached: CachedEntry | None
    ) -> LookupResult:
        failed: tuple[LookupStatus, str]
        if self._link.down():
            return self._fallback(LookupStatus.UNREACHABLE, call, cached, _UNREACHABLE)
        try:
            entry, note = self._ask(actor, login, call)
        except QrzLoginError as error:
            self._store.save_key(actor, SOURCE, None)  # no longer a login QRZ.com accepts
            failed = LookupStatus.LOGIN_FAILED, f"QRZ.com refused your login: {error}"
        except QrzUnreachableError:
            self._link.mark_down()
            failed = LookupStatus.UNREACHABLE, _UNREACHABLE
        except QrzReplyError as error:
            failed = LookupStatus.UNREACHABLE, f"QRZ.com's answer was not usable: {error}"
        else:
            self._link.mark_up()
            kept = self._store.remember(
                SOURCE,
                call,
                found=entry is not None,
                entry={} if entry is None else entry.to_json(),
            )
            return _from_cache(call, kept, note or "", cached=False)
        status, message = failed
        return self._fallback(status, call, cached, message)

    def _ask(
        self, actor: Actor, login: CallbookLogin, call: str
    ) -> tuple[CallbookEntry | None, str | None]:
        client = self._link.client
        key = login.service_key
        if key is not None:
            try:
                return client.lookup(key, call)
            except QrzSessionError:
                pass  # expired: log in again, once
        key = client.login(login.username, login.password)
        self._store.save_key(actor, SOURCE, key)
        try:
            return client.lookup(key, call)
        except QrzSessionError as error:
            raise QrzReplyError(f"a new session ended at once ({error})") from error

    def _fallback(
        self, status: LookupStatus, call: str, cached: CachedEntry | None, message: str
    ) -> LookupResult:
        if cached is not None:
            return _from_cache(call, cached, f"{message} Showing what QRZ.com said before.")
        return LookupResult(status, call, None, cached=False, fetched_at=None, message=message)

    def login_name(self, actor: Actor) -> tuple[str, bool] | None:
        """The QRZ.com username the operator saved, if any (never the password).

        With it, whether QRZ.com accepted that login the last time it was tried.
        """
        login = self._store.login(actor, SOURCE)
        return None if login is None else (login.username, login.service_key is not None)

    def save_login(self, actor: Actor, username: str, password: str) -> LoginCheck:
        """Save the operator's QRZ.com login and try it.

        The answer says whether QRZ.com accepted it (``status`` is ``None``) or
        why it could not be checked, with what QRZ.com said. The login is saved
        either way, so it can be set up with no internet.
        """
        self._store.save_login(actor, SOURCE, username, password)
        try:
            key, note = self._link.client.sign_in(username, password)
        except QrzLoginError as error:
            return LoginCheck(LookupStatus.LOGIN_FAILED, str(error))
        except (QrzUnreachableError, QrzReplyError):
            self._link.mark_down()
            return LoginCheck(LookupStatus.UNREACHABLE, None)
        self._link.mark_up()
        self._store.save_key(actor, SOURCE, key)
        return LoginCheck(None, note)

    def remove_login(self, actor: Actor) -> bool:
        """Remove the operator's QRZ.com login; ``False`` if there was none."""
        return self._store.remove_login(actor, SOURCE)


def _from_cache(
    call: str, answer: CachedEntry, message: str, *, cached: bool = True
) -> LookupResult:
    entry = CallbookEntry.from_json(answer.entry) if answer.found else None
    status = LookupStatus.FOUND if entry is not None else LookupStatus.NOT_FOUND
    if not message:
        message = "Filled in from QRZ.com." if entry else f"QRZ.com has no record of {call}."
    return LookupResult(
        status, call, entry, cached=cached, fetched_at=answer.fetched_at, message=message
    )
