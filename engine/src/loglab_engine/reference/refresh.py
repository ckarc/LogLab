# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Keeping the US and Canadian license lists current, whenever there is internet.

Every few hours the Engine asks the FCC and ISED, by a small HEAD request,
when their files were last published, and downloads only what is newer than
what it already has (Principle V: fetch each thing once):

* the FCC's weekly full list, ``l_amat.zip``, replacing the US list;
* each of the FCC's daily change files published since that full list,
  applied in the order they were published;
* ISED's list, ``amateur_delim.zip``, replacing the Canadian list.

With no internet the check simply fails and is tried again later; lookups
keep using the list already loaded. A station that is never online loads the
files by hand with ``loglab-engine licenses import``.
"""

from __future__ import annotations

import email.utils
import logging
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import UTC, date, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Final, Protocol

import psycopg

from loglab_engine.reference.licenses import LicenseFileError
from loglab_engine.store import connect
from loglab_engine.store.licenses import ImportResult, Kind, LicenseStore, Source

if TYPE_CHECKING:
    from collections.abc import Callable

FCC_FULL_URL: Final = "https://data.fcc.gov/download/pub/uls/complete/l_amat.zip"
FCC_DAILY_URL: Final = "https://data.fcc.gov/download/pub/uls/daily/l_am_{day}.zip"
FCC_DAYS: Final = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")
ISED_URL: Final = "https://apc-cap.ic.gc.ca/datafiles/amateur_delim.zip"

MAX_FULL_DOWNLOAD: Final = 1024 * 1024 * 1024
MAX_SMALL_DOWNLOAD: Final = 64 * 1024 * 1024
CHECK_EVERY_SECONDS: Final = 6 * 3600.0
FIRST_CHECK_AFTER_SECONDS: Final = 60.0
TIMEOUT_SECONDS: Final = 30.0
RETRY_PAUSE_SECONDS: Final = 5.0
_LOCK_KEY: Final = 0x4C6963656E7365  # "License": one refresh at a time
_log = logging.getLogger(__name__)


class UnreachableError(Exception):
    """A license file could not be fetched (no internet, a timeout, or an HTTP error)."""


class Downloads(Protocol):
    """How the Engine reaches the agencies' files; tests give a stand-in."""

    def published(self, url: str) -> datetime | None:
        """When the file at ``url`` was last published, if the server says."""
        ...

    def fetch(self, url: str, dest: Path, cap: int) -> datetime | None:
        """Save the file at ``url`` to ``dest``, refusing more than ``cap`` bytes."""
        ...


def _last_modified(value: str | None) -> datetime | None:
    if not value:
        return None
    try:
        return email.utils.parsedate_to_datetime(value).astimezone(UTC)
    except (TypeError, ValueError):
        return None


def _twice[T](attempt: Callable[[], T]) -> T:
    """``attempt()``, tried once more after a short pause if the site could not be reached.

    ISED's site sometimes times out on the first connection and answers the second.
    """
    try:
        return attempt()
    except UnreachableError:
        time.sleep(RETRY_PAUSE_SECONDS)
        return attempt()


class HttpDownloads:
    """The agencies' files over HTTPS, with timeouts and size caps."""

    def __init__(self, agent: str) -> None:
        """Identify as ``agent``."""
        self._agent = agent

    def _request(self, url: str, method: str) -> urllib.request.Request:
        if not url.startswith("https://"):
            raise UnreachableError("only HTTPS is used")
        return urllib.request.Request(  # noqa: S310 - https only
            url, method=method, headers={"User-Agent": self._agent}
        )

    def published(self, url: str) -> datetime | None:
        """When the file at ``url`` was last published (its Last-Modified date)."""
        return _twice(lambda: self._published(url))

    def fetch(self, url: str, dest: Path, cap: int) -> datetime | None:
        """Save the file at ``url`` to ``dest``; return its publication date."""
        return _twice(lambda: self._fetch(url, dest, cap))

    def _published(self, url: str) -> datetime | None:
        try:
            with urllib.request.urlopen(  # noqa: S310 - https only
                self._request(url, "HEAD"), timeout=TIMEOUT_SECONDS
            ) as response:
                return _last_modified(response.headers.get("Last-Modified"))
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise UnreachableError(f"{url} could not be reached ({error})") from error

    def _fetch(self, url: str, dest: Path, cap: int) -> datetime | None:
        try:
            with (
                urllib.request.urlopen(  # noqa: S310 - https only
                    self._request(url, "GET"), timeout=TIMEOUT_SECONDS
                ) as response,
                dest.open("wb") as out,
            ):
                size = 0
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > cap:
                        raise LicenseFileError(f"{url} is larger than {cap} bytes")
                    out.write(chunk)
                return _last_modified(response.headers.get("Last-Modified"))
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise UnreachableError(f"{url} could not be fetched ({error})") from error


@dataclass(frozen=True, slots=True)
class RefreshReport:
    """What one refresh did, and what stopped it, if anything."""

    imported: list[ImportResult]
    problems: list[str]


def _newer(remote: datetime | None, have: tuple[datetime | None, datetime] | None) -> bool:
    if have is None:
        return True
    if remote is None:
        return False  # the server doesn't say; keep what is loaded (fetch each thing once)
    return have[0] is None or remote > have[0]


class Refresher:
    """Checks for newer license files, downloads them, and loads them."""

    def __init__(
        self,
        database: str,
        downloads: Downloads,
        today: Callable[[], date] = lambda: datetime.now(UTC).date(),
    ) -> None:
        """Use the database ``database``, reaching the agencies through ``downloads``."""
        self._database = database
        self.downloads = downloads
        self._today = today
        self._thread: threading.Thread | None = None
        self._wake = threading.Event()
        self._lock = threading.Lock()
        self._last: RefreshReport | None = None

    def last_report(self) -> RefreshReport | None:
        """What the last background refresh did, if one has run."""
        with self._lock:
            return self._last

    def start(self) -> None:
        """Check every few hours, in the background, until the process ends."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, name="licenses", daemon=True)
            self._thread.start()

    def wake(self) -> None:
        """Check now rather than at the next scheduled time."""
        self._wake.set()

    def _loop(self) -> None:
        self._wake.wait(timeout=FIRST_CHECK_AFTER_SECONDS)
        while True:
            self._wake.clear()
            try:
                report = self.run_once()
            except psycopg.Error:
                _log.exception("license refresh: database error; trying again later")
            else:
                with self._lock:
                    self._last = report
                for problem in report.problems:
                    _log.info("license refresh: %s", problem)
            self._wake.wait(timeout=CHECK_EVERY_SECONDS)

    def run_once(self) -> RefreshReport:
        """Fetch and load whatever is newer than what is loaded."""
        report = RefreshReport([], [])
        with connect(self._database) as conn:
            locked = conn.execute("SELECT pg_try_advisory_lock(%s)", (_LOCK_KEY,)).fetchone()
            if locked is None or locked[0] is not True:
                report.problems.append("another refresh is running")
                return report
            store = LicenseStore(conn)
            with tempfile.TemporaryDirectory(prefix="loglab-licenses-") as work:
                folder = Path(work)
                for step in (self._fcc_full, self._fcc_daily, self._ised):
                    try:
                        step(store, folder, report)
                    except (UnreachableError, LicenseFileError) as error:
                        report.problems.append(str(error))
        return report

    def _load(
        self,
        folder: Path,
        url: str,
        *,
        cap: int,
        known: datetime | None,
        load: Callable[[Path, datetime | None], ImportResult],
    ) -> ImportResult:
        path = folder / url.rsplit("/", 1)[-1]
        try:
            fetched = self.downloads.fetch(url, path, cap)
            # Recorded as the date the check saw, so the next check matches it.
            return load(path, known or fetched)
        finally:
            path.unlink(missing_ok=True)  # only the loaded list is kept, never the download

    def _fcc_full(self, store: LicenseStore, folder: Path, report: RefreshReport) -> None:
        have = store.latest(Source.FCC, Kind.FULL)
        published = self.downloads.published(FCC_FULL_URL)
        if not _newer(published, have):
            return
        today = self._today()
        report.imported.append(
            self._load(
                folder,
                FCC_FULL_URL,
                cap=MAX_FULL_DOWNLOAD,
                known=published,
                load=lambda path, when: store.import_fcc(
                    path, Kind.FULL, today=today, published=when
                ),
            )
        )

    def _fcc_daily(self, store: LicenseStore, folder: Path, report: RefreshReport) -> None:
        full = store.latest(Source.FCC, Kind.FULL)
        if full is None:
            return  # the changes only make sense on top of a full list
        since = full[0] or full[1]
        applied = store.applied_since_full(Source.FCC)
        waiting: list[tuple[datetime, str]] = []
        for day in FCC_DAYS:
            url = FCC_DAILY_URL.format(day=day)
            try:
                published = self.downloads.published(url)
            except UnreachableError as error:
                report.problems.append(str(error))  # one day's file missing stops no other
                continue
            name = url.rsplit("/", 1)[-1]
            if published is not None and published > since and (name, published) not in applied:
                waiting.append((published, url))
        today = self._today()
        for published, url in sorted(waiting):
            report.imported.append(
                self._load(
                    folder,
                    url,
                    cap=MAX_SMALL_DOWNLOAD,
                    known=published,
                    load=lambda path, when: store.import_fcc(
                        path, Kind.DAILY, today=today, published=when
                    ),
                )
            )

    def _ised(self, store: LicenseStore, folder: Path, report: RefreshReport) -> None:
        have = store.latest(Source.ISED, Kind.FULL)
        published = self.downloads.published(ISED_URL)
        if not _newer(published, have):
            return
        report.imported.append(
            self._load(
                folder,
                ISED_URL,
                cap=MAX_SMALL_DOWNLOAD,
                known=published,
                load=lambda path, when: store.import_ised(path, published=when),
            )
        )
