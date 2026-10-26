# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The US and Canadian license lists in the Engine's database, for lookups with no internet.

An import loads the file into temporary staging tables, then changes
``reference.license`` in one transaction (Principle XIV): a lookup sees the
old list until the new one is complete, and a file that fails part way
changes nothing. A file with too many unreadable records is refused, keeping
the list as it was (Principle XII). Every import is recorded with its source
and publication date (Principle VI).
"""

from __future__ import annotations

import zipfile
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Final, LiteralString

from loglab_engine.reference.licenses import (
    LicenseFileError,
    Tally,
    fcc_classes,
    fcc_entities,
    fcc_headers,
    ised_licenses,
)

if TYPE_CHECKING:
    from datetime import date, datetime
    from pathlib import Path

    import psycopg
    from psycopg.rows import TupleRow

# A file may have a few odd records; one with more than this share is refused.
MAX_SKIPPED_SHARE: Final = 0.01
MIN_SKIPPED_ALLOWED: Final = 100


class Source(StrEnum):
    """Where a license list comes from."""

    FCC = "fcc"
    ISED = "ised"


class Kind(StrEnum):
    """A whole list, or one day's changes to it."""

    FULL = "full"
    DAILY = "daily"


COUNTRY: Final = {Source.FCC: "United States", Source.ISED: "Canada"}


@dataclass(frozen=True, slots=True)
class License:
    """What a license list says about a call."""

    source: Source
    call: str
    name: str | None
    city: str | None
    state: str | None
    license_class: str | None
    active: bool
    expires: date | None


@dataclass(frozen=True, slots=True)
class ImportResult:
    """What an import did."""

    source: Source
    kind: Kind
    records: int
    skipped: int
    examples: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True)
class SourceState:
    """How current one source's list is."""

    source: Source
    calls: int
    full_published: datetime | None
    full_imported: datetime | None
    daily_published: datetime | None
    daily_imported: datetime | None


_STAGE: Final = """
CREATE TEMP TABLE stage_hd (usi text, call text, active boolean, expires date, kept boolean)
    ON COMMIT DROP;
CREATE TEMP TABLE stage_en (usi text, name text, city text, state text, licensee boolean)
    ON COMMIT DROP;
CREATE TEMP TABLE stage_am (usi text, class text) ON COMMIT DROP;
"""

_FULL_FCC: Final = """
INSERT INTO reference.license
    (source, record_id, call, name, city, state, class, active, expires)
SELECT 'fcc', h.usi, h.call, e.name, e.city, e.state, a.class, h.active, h.expires
FROM (SELECT DISTINCT ON (usi) * FROM stage_hd WHERE kept ORDER BY usi) h
LEFT JOIN (SELECT DISTINCT ON (usi) * FROM stage_en ORDER BY usi, licensee DESC) e USING (usi)
LEFT JOIN (SELECT DISTINCT ON (usi) * FROM stage_am ORDER BY usi) a USING (usi)
"""

# One day's changes: a license the FCC now lists as cancelled is removed; one
# it changed or added is written; licensee and class changes are applied to
# the licenses LabLog keeps.
_DAILY_FCC: Final = (
    """
    DELETE FROM reference.license l USING stage_hd h
    WHERE l.source = 'fcc' AND l.record_id = h.usi AND NOT h.kept
    """,
    """
    INSERT INTO reference.license (source, record_id, call, active, expires)
    SELECT DISTINCT ON (usi) 'fcc', usi, call, active, expires
    FROM stage_hd WHERE kept ORDER BY usi
    ON CONFLICT (source, record_id) DO UPDATE SET
        call = EXCLUDED.call, active = EXCLUDED.active, expires = EXCLUDED.expires
    """,
    """
    UPDATE reference.license l SET name = e.name, city = e.city, state = e.state
    FROM (SELECT DISTINCT ON (usi) * FROM stage_en ORDER BY usi, licensee DESC) e
    WHERE l.source = 'fcc' AND l.record_id = e.usi
    """,
    """
    UPDATE reference.license l SET class = a.class
    FROM (SELECT DISTINCT ON (usi) * FROM stage_am ORDER BY usi) a
    WHERE l.source = 'fcc' AND l.record_id = a.usi
    """,
)


def _check(tally: Tally, what: str) -> None:
    allowed = max(MIN_SKIPPED_ALLOWED, int(tally.read * MAX_SKIPPED_SHARE))
    if tally.skipped > allowed:
        reasons = "; ".join(tally.examples)
        raise LicenseFileError(
            f"{what}: {tally.skipped} of {tally.read} records could not be read ({reasons})"
        )


def _open(path: Path) -> zipfile.ZipFile:
    try:
        return zipfile.ZipFile(path)
    except (zipfile.BadZipFile, OSError) as error:
        raise LicenseFileError(f"{path.name} is not a readable zip file ({error})") from error


class LicenseStore:
    """Reads and replaces the license lists."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def find(self, call: str) -> License | None:
        """The license for ``call``: an active one first, else the latest to expire."""
        row = self._conn.execute(
            """
            SELECT source, call, name, city, state, class, active, expires
            FROM reference.license WHERE call = %s
            ORDER BY active DESC, expires DESC NULLS LAST, source LIMIT 1
            """,
            (call,),
        ).fetchone()
        if row is None:
            return None
        return License(Source(row[0]), *row[1:])

    def import_fcc(
        self,
        path: Path,
        kind: Kind,
        *,
        today: date,
        published: datetime | None = None,
    ) -> ImportResult:
        """Load an FCC download: ``l_amat.zip`` (``FULL``) or one day's ``l_am_<day>.zip``.

        Raises:
            LicenseFileError: The file is not an FCC amateur download, or too
                much of it could not be read. Nothing is changed.
        """
        tally = Tally()
        with _open(path) as archive, self._conn.transaction():
            self._conn.execute(_STAGE)
            with self._conn.cursor().copy(
                "COPY stage_hd (usi, call, active, expires, kept) FROM STDIN"
            ) as copy:
                for h in fcc_headers(archive, today, tally):
                    if h.kept or kind is Kind.DAILY:
                        copy.write_row((h.usi, h.call, h.active, h.expires, h.kept))
            with self._conn.cursor().copy(
                "COPY stage_en (usi, name, city, state, licensee) FROM STDIN"
            ) as copy:
                for e in fcc_entities(archive, tally):
                    copy.write_row((e.usi, e.name, e.city, e.state, e.licensee))
            with self._conn.cursor().copy("COPY stage_am (usi, class) FROM STDIN") as copy:
                for a in fcc_classes(archive, tally):
                    copy.write_row((a.usi, a.license_class))
            _check(tally, path.name)
            if kind is Kind.FULL:
                kept = self._count("SELECT count(DISTINCT usi) FROM stage_hd WHERE kept")
                if kept == 0:
                    raise LicenseFileError(f"{path.name} lists no current licenses")
                self._conn.execute("DELETE FROM reference.license WHERE source = 'fcc'")
                records = self._conn.execute(_FULL_FCC).rowcount
            else:
                records = self._count("SELECT count(DISTINCT usi) FROM stage_hd")
                for statement in _DAILY_FCC:
                    self._conn.execute(statement)
            self._record(Source.FCC, kind, path.name, published, records)
        return ImportResult(Source.FCC, kind, records, tally.skipped, tuple(tally.examples))

    def import_ised(self, path: Path, *, published: datetime | None = None) -> ImportResult:
        """Load ISED's ``amateur_delim.zip``, replacing every Canadian call.

        Raises:
            LicenseFileError: The file is not ISED's amateur list, or too much
                of it could not be read. Nothing is changed.
        """
        tally = Tally()
        with _open(path) as archive, self._conn.transaction():
            self._conn.execute(
                "CREATE TEMP TABLE stage_ca"
                " (call text, name text, city text, state text, class text) ON COMMIT DROP"
            )
            with self._conn.cursor().copy(
                "COPY stage_ca (call, name, city, state, class) FROM STDIN"
            ) as copy:
                for c in ised_licenses(archive, tally):
                    copy.write_row((c.call, c.name, c.city, c.province, c.license_class))
            _check(tally, path.name)
            if self._count("SELECT count(*) FROM stage_ca") == 0:
                raise LicenseFileError(f"{path.name} lists no calls")
            self._conn.execute("DELETE FROM reference.license WHERE source = 'ised'")
            records = self._conn.execute(
                """
                INSERT INTO reference.license
                    (source, record_id, call, name, city, state, class, active)
                SELECT DISTINCT ON (call) 'ised', call, call, name, city, state, class, true
                FROM stage_ca ORDER BY call
                """
            ).rowcount
            self._record(Source.ISED, Kind.FULL, path.name, published, records)
        return ImportResult(Source.ISED, Kind.FULL, records, tally.skipped, tuple(tally.examples))

    def _count(self, query: LiteralString) -> int:
        row = self._conn.execute(query).fetchone()
        return 0 if row is None else int(row[0])

    def _record(
        self, source: Source, kind: Kind, name: str, published: datetime | None, records: int
    ) -> None:
        self._conn.execute(
            "INSERT INTO reference.license_import (source, kind, file_name, published, records)"
            " VALUES (%s, %s, %s, %s, %s)",
            (source.value, kind.value, name[:255], published, records),
        )

    def latest(self, source: Source, kind: Kind) -> tuple[datetime | None, datetime] | None:
        """The publication and import time of the last import of this kind, if any."""
        row = self._conn.execute(
            """
            SELECT published, imported_at FROM reference.license_import
            WHERE source = %s AND kind = %s ORDER BY imported_at DESC, id DESC LIMIT 1
            """,
            (source.value, kind.value),
        ).fetchone()
        return None if row is None else (row[0], row[1])

    def applied_since_full(self, source: Source) -> set[tuple[str, datetime | None]]:
        """The daily files (name and publication time) applied since the last full import."""
        rows = self._conn.execute(
            """
            SELECT file_name, published FROM reference.license_import
            WHERE source = %(source)s AND kind = 'daily' AND imported_at >= coalesce(
                (SELECT max(imported_at) FROM reference.license_import
                 WHERE source = %(source)s AND kind = 'full'), '-infinity')
            """,
            {"source": source.value},
        ).fetchall()
        return {(r[0], r[1]) for r in rows}

    def states(self) -> list[SourceState]:
        """How current each source's list is."""
        counts: dict[str, int] = {
            row[0]: int(row[1])
            for row in self._conn.execute(
                "SELECT source, count(*) FROM reference.license GROUP BY source"
            ).fetchall()
        }
        found: list[SourceState] = []
        for source in Source:
            full = self.latest(source, Kind.FULL)
            daily = self.latest(source, Kind.DAILY)
            found.append(
                SourceState(
                    source=source,
                    calls=counts.get(source.value, 0),
                    full_published=None if full is None else full[0],
                    full_imported=None if full is None else full[1],
                    daily_published=None if daily is None else daily[0],
                    daily_imported=None if daily is None else daily[1],
                )
            )
        return found
