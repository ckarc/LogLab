# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Re-checking kept QRZ.com profiles in the background, on the right network.

A profile the Engine keeps a biography or photo for is looked up again once
its entry is more than a month old. The lookup is QRZ.com's small XML answer,
which dates the biography and names the photo; the biography and photo, the
larger downloads, are fetched again only when that date or that photo changed.
The lookups are spaced a few seconds apart and capped per hour, to go easy on
QRZ.com.

Starlink is a metered link in the field, so by default nothing is re-checked
while the Engine's internet comes through Starlink. The Engine tells by asking
which network its public address belongs to: SpaceX's (AS14593), or a name
under starlinkisp.net. An administrator can instead re-check on any internet,
or never. When the network can't be told, nothing is re-checked.

This is reference data only, apart from the log (Principle VI). It never
blocks a lookup or logging, and with no internet it does nothing (Principle V).
"""

from __future__ import annotations

import ipaddress
import json
import logging
import socket
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum
from typing import TYPE_CHECKING, Final

import psycopg

from loglab_engine.callbook.qrz import (
    CallbookEntry,
    QrzLoginError,
    QrzReplyError,
    QrzSessionError,
    QrzUnreachableError,
    clean_text,
)
from loglab_engine.callbook.service import RECHECK_AFTER, SOURCE
from loglab_engine.store import connect
from loglab_engine.store.callbook import CallbookStore

if TYPE_CHECKING:
    from collections.abc import Callable

    from loglab_engine.callbook.qrz import QrzClient
    from loglab_engine.callbook.service import Link

FIRST_CHECK_AFTER_SECONDS: Final = 120.0
CHECK_EVERY_SECONDS: Final = 3600.0
PACE_SECONDS: Final = 3.0
BATCH: Final = 60  # lookups an hour at most
NETWORK_TIMEOUT_SECONDS: Final = 5.0
MAX_ANSWER: Final = 4096
_LOCK_KEY: Final = 0x51525A50726F  # "QRZPro": one re-check at a time
_STARLINK_AS: Final = "AS14593"
_STARLINK_NAME: Final = ".starlinkisp.net"
WHERE_AM_I: Final = "https://ipinfo.io/json"
WHAT_IS_MY_ADDRESS: Final = "https://api.ipify.org"
_log = logging.getLogger(__name__)


class Mode(StrEnum):
    """When kept profiles are re-checked (an administrator's setting)."""

    AUTO = "auto"  # on any internet but Starlink
    ALWAYS = "always"
    OFF = "off"


class Network(StrEnum):
    """What the Engine's internet comes through, as far as it can tell."""

    NONE = "none"
    STARLINK = "starlink"
    OTHER = "other"
    UNKNOWN = "unknown"


@dataclass(frozen=True, slots=True)
class NetworkSeen:
    """The network, with the public address and who it belongs to, where known."""

    network: Network
    address: str | None = None
    provider: str | None = None


class NetworkUnreachableError(Exception):
    """The address service could not be reached."""


def fetch_small(url: str) -> bytes:
    """GET ``url`` over HTTPS with a short timeout, keeping at most :data:`MAX_ANSWER` bytes.

    Raises:
        NetworkUnreachableError: It could not be fetched.
    """
    if not url.startswith("https://"):
        raise NetworkUnreachableError("only HTTPS is used")
    request = urllib.request.Request(url, headers={"Accept": "application/json, text/plain"})  # noqa: S310 - https only
    try:
        with urllib.request.urlopen(request, timeout=NETWORK_TIMEOUT_SECONDS) as response:  # noqa: S310 - https only
            body: bytes = response.read(MAX_ANSWER)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise NetworkUnreachableError(str(getattr(error, "reason", error))) from error
    return body


def reverse_name(address: str) -> str | None:
    """The host name the public ``address`` is registered under, if any."""
    try:
        return socket.gethostbyaddr(address)[0]
    except OSError:
        return None


def _address(value: object) -> str | None:
    if not isinstance(value, str):
        return None
    try:
        return str(ipaddress.ip_address(value.strip()))
    except ValueError:
        return None


def look_at_network(
    fetch: Callable[[str], bytes] = fetch_small,
    reverse: Callable[[str], str | None] = reverse_name,
) -> NetworkSeen:
    """Which network the Engine's internet comes through.

    Asks an address service for the public address and the network it belongs
    to; with no answer, a second service for the address alone, and its
    registered name. Everything it says is untrusted and only compared.
    """
    address: str | None = None
    provider: str | None = None
    name: str | None = None
    try:
        answer = json.loads(fetch(WHERE_AM_I))
    except NetworkUnreachableError:
        answer = None
    except ValueError:
        answer = {}
    if isinstance(answer, dict):
        address = _address(answer.get("ip"))
        org = answer.get("org")
        provider = clean_text(org, 80) if isinstance(org, str) else None
        host = answer.get("hostname")
        name = clean_text(host, 255) if isinstance(host, str) else None
    if address is None:
        try:
            address = _address(fetch(WHAT_IS_MY_ADDRESS).decode("ascii", errors="replace"))
        except NetworkUnreachableError:
            if answer is None:
                return NetworkSeen(Network.NONE)
    if address is None:
        return NetworkSeen(Network.UNKNOWN)
    if name is None:
        name = reverse(address)
    starlink = (provider or "").split(" ", 1)[0] == _STARLINK_AS or (name or "").lower().rstrip(
        "."
    ).endswith(_STARLINK_NAME)
    return NetworkSeen(Network.STARLINK if starlink else Network.OTHER, address, provider)


# Why nothing is re-checked on a network; on ``always`` only having none holds it back.
_HELD: Final = {
    Network.NONE: "No internet right now.",
    Network.STARLINK: "Paused: the internet is Starlink.",
    Network.UNKNOWN: "Paused: LabLog can't tell which network this is.",
}


@dataclass(frozen=True, slots=True)
class RefreshReport:
    """What one background re-check did."""

    at: datetime
    network: Network | None
    checked: int
    bios: int
    photos: int
    message: str


class ProfileRefresher:
    """Re-checks kept QRZ.com profiles in the background, once an hour."""

    def __init__(
        self,
        database: str,
        link: Link,
        network: Callable[[], NetworkSeen] = look_at_network,
        pause: Callable[[float], None] = time.sleep,
    ) -> None:
        """Use the database ``database`` and the Engine's ``link`` to QRZ.com.

        ``network`` tells which network the internet comes through; ``pause``
        waits between lookups. Tests give stand-ins.
        """
        self._database = database
        self._link = link
        self._network = network
        self._pause = pause
        self._thread: threading.Thread | None = None
        self._wake = threading.Event()
        self._lock = threading.Lock()
        self._last: RefreshReport | None = None

    def last_report(self) -> RefreshReport | None:
        """What the last re-check did, if one has run."""
        with self._lock:
            return self._last

    def start(self) -> None:
        """Re-check every hour, in the background, until the process ends."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, name="qrz-profiles", daemon=True)
            self._thread.start()

    def wake(self) -> None:
        """Re-check now rather than at the next scheduled time."""
        self._wake.set()

    def _loop(self) -> None:
        self._wake.wait(timeout=FIRST_CHECK_AFTER_SECONDS)
        while True:
            self._wake.clear()
            try:
                report = self.run_once()
            except psycopg.Error:
                _log.exception("QRZ profile re-check: database error; trying again later")
            else:
                _log.info("QRZ profile re-check: %s", report.message)
            self._wake.wait(timeout=CHECK_EVERY_SECONDS)

    def run_once(self) -> RefreshReport:
        """Re-check the profiles that are due, if this network allows it."""
        with connect(self._database) as conn:
            locked = conn.execute("SELECT pg_try_advisory_lock(%s)", (_LOCK_KEY,)).fetchone()
            if locked is None or locked[0] is not True:
                return self._report(None, 0, 0, 0, "Another re-check is running.")
            return self._run(CallbookStore(conn))

    def _report(
        self, network: Network | None, checked: int, bios: int, photos: int, message: str
    ) -> RefreshReport:
        report = RefreshReport(datetime.now(UTC), network, checked, bios, photos, message)
        with self._lock:
            self._last = report
        return report

    def _run(self, store: CallbookStore) -> RefreshReport:
        mode = Mode(store.refresh_setting().mode)
        if mode is Mode.OFF:
            return self._report(None, 0, 0, 0, "Turned off.")
        due = store.kept_profiles_due(SOURCE, RECHECK_AFTER, BATCH)
        if not due:
            return self._report(None, 0, 0, 0, "Every kept profile is up to date.")
        login = store.accepted_login(SOURCE)
        if login is None:
            return self._report(None, 0, 0, 0, "Waiting for a QRZ.com login that QRZ.com accepts.")
        if self._link.down():
            return self._report(Network.NONE, 0, 0, 0, "QRZ.com can't be reached right now.")
        network = self._network().network
        if network is Network.NONE or (mode is Mode.AUTO and network in _HELD):
            return self._report(network, 0, 0, 0, _HELD[network])
        return self._recheck(store, login, due, network)

    def _recheck(
        self, store: CallbookStore, login: tuple[str, str], due: list[str], network: Network
    ) -> RefreshReport:
        session = _Session(self._link.client, *login)
        checked = bios = photos = 0
        problem: str | None = None
        for index, call in enumerate(due):
            if index:
                self._pause(PACE_SECONDS)
            try:
                bio, photo = self._one(store, session, call)
            except QrzUnreachableError:
                self._link.mark_down()
                problem = "QRZ.com stopped answering; the rest wait for the next check."
                break
            except QrzLoginError as error:
                problem = f"QRZ.com refused the login: {error}"
                break
            except QrzReplyError as error:
                _log.info("QRZ profile re-check: %s: %s", call, error)
                continue
            self._link.mark_up()
            checked += 1
            bios += bio
            photos += photo
        message = f"Checked {checked} kept profiles: {bios} new biographies, {photos} new photos."
        if problem is not None:
            message += f" {problem}"
        return self._report(network, checked, bios, photos, message)

    def _one(self, store: CallbookStore, session: _Session, call: str) -> tuple[bool, bool]:
        """Look ``call`` up again; fetch its biography and photo only if they changed."""
        entry, _ = session.ask(lambda key: session.client.lookup(key, call))
        store.remember(
            SOURCE, call, found=entry is not None, entry={} if entry is None else entry.to_json()
        )
        if entry is None:
            return False, False
        return self._bio(store, session, entry), self._photo(store, session, entry)

    @staticmethod
    def _bio(store: CallbookStore, session: _Session, entry: CallbookEntry) -> bool:
        kept = store.media(SOURCE, entry.call, "bio")
        if kept is None or not entry.bio or kept.origin == entry.bio_date:
            return False
        text = session.ask(lambda key: session.client.biography(key, entry.call))
        if text is None:
            return False
        store.remember_media(
            SOURCE,
            entry.call,
            "bio",
            content=text.encode(),
            content_type="text/plain",
            origin=entry.bio_date,
        )
        return True

    @staticmethod
    def _photo(store: CallbookStore, session: _Session, entry: CallbookEntry) -> bool:
        kept = store.media(SOURCE, entry.call, "photo")
        if kept is None or entry.image is None or kept.origin == entry.image:
            return False
        body, media_type = session.client.photo(entry.image)
        store.remember_media(
            SOURCE, entry.call, "photo", content=body, content_type=media_type, origin=entry.image
        )
        return True


class _Session:
    """A QRZ.com session of the Engine's own, kept in memory for one re-check."""

    def __init__(self, client: QrzClient, username: str, password: str) -> None:
        self.client = client
        self._username = username
        self._password = password
        self._key: str | None = None

    def ask[T](self, question: Callable[[str], T]) -> T:
        """Ask with the session key, logging in first, and again once if it expired."""
        if self._key is not None:
            try:
                return question(self._key)
            except QrzSessionError:
                pass  # expired: log in again, once
        self._key = self.client.login(self._username, self._password)
        try:
            return question(self._key)
        except QrzSessionError as error:
            raise QrzReplyError(f"a new session ended at once ({error})") from error
