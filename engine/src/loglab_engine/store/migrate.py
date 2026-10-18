# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Versioned schema migrations: numbered SQL files, applied in order, recorded with checksums.

Migrations live in ``loglab_engine/store/migrations`` as ``NNNN_name.sql``,
numbered from 0001 with no gaps. Each one runs in its own transaction and is
recorded in ``meta.schema_migration`` with the SHA-256 of its file in the same
transaction, so a migration is either wholly applied and recorded or not at all
(Principle XIV).

Before anything is applied, every recorded migration is checked against its
file. A migration whose file changed after it was applied, or one the database
has but this code does not, stops everything with a :class:`MigrationError`:
the database and the code disagree about what the schema is, and guessing
would put the log at risk.

A migration file must not contain its own transaction control (``BEGIN``,
``COMMIT``); the runner provides the transaction.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from importlib.resources import files
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from collections.abc import Sequence
    from datetime import datetime
    from importlib.resources.abc import Traversable

    import psycopg
    from psycopg.rows import TupleRow

_FILE_NAME: Final = re.compile(r"^(\d{4})_([a-z0-9_]+)\.sql$")

# Serializes migration runs across processes; any constant unique to LogLab works.
_LOCK_KEY: Final = 0x4C6F674C6162  # "LogLab"

_BOOTSTRAP: Final = b"""
CREATE SCHEMA IF NOT EXISTS meta;
CREATE TABLE IF NOT EXISTS meta.schema_migration (
    version    integer PRIMARY KEY CHECK (version >= 1),
    name       text NOT NULL,
    sha256     text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);
"""


class MigrationError(RuntimeError):
    """The migration files or the database's migration record are not usable."""


@dataclass(frozen=True, slots=True)
class Migration:
    """One migration file."""

    version: int
    name: str
    sql: bytes

    @property
    def sha256(self) -> str:
        """Hex SHA-256 of the file's exact bytes."""
        return hashlib.sha256(self.sql).hexdigest()

    @property
    def label(self) -> str:
        """The migration as its file is named, e.g. ``0002_log``."""
        return f"{self.version:04d}_{self.name}"


@dataclass(frozen=True, slots=True)
class AppliedMigration:
    """A migration as the database recorded it."""

    version: int
    name: str
    sha256: str
    applied_at: datetime


def load_migrations(directory: Traversable | None = None) -> tuple[Migration, ...]:
    """Read the migration files, in order.

    Args:
        directory: Where the ``.sql`` files are; defaults to the packaged ones.

    Raises:
        MigrationError: A file is misnamed, or the numbering has a gap or duplicate.
    """
    root = directory if directory is not None else files("loglab_engine.store") / "migrations"
    found: list[Migration] = []
    for entry in root.iterdir():
        if not entry.name.endswith(".sql"):
            continue
        match = _FILE_NAME.match(entry.name)
        if match is None:
            msg = f"migration file {entry.name!r} is not named NNNN_lowercase_name.sql"
            raise MigrationError(msg)
        found.append(Migration(int(match[1]), match[2], entry.read_bytes()))
    found.sort(key=lambda m: m.version)
    for expected, migration in enumerate(found, start=1):
        if migration.version != expected:
            msg = f"migrations must be numbered 0001 upward with no gaps; found {migration.label}"
            raise MigrationError(msg)
    return tuple(found)


def applied_migrations(conn: psycopg.Connection[TupleRow]) -> list[AppliedMigration]:
    """Return the migrations recorded in the database, oldest first (none if it is new)."""
    exists = conn.execute("SELECT to_regclass('meta.schema_migration') IS NOT NULL").fetchone()
    if exists is None or exists[0] is not True:
        return []
    rows = conn.execute(
        "SELECT version, name, sha256, applied_at FROM meta.schema_migration ORDER BY version",
    ).fetchall()
    return [AppliedMigration(*row) for row in rows]


def pending_migrations(
    conn: psycopg.Connection[TupleRow],
    migrations: Sequence[Migration],
) -> list[Migration]:
    """Check the database's record against ``migrations`` and return those not yet applied.

    Raises:
        MigrationError: An applied migration's file changed, or the database has
            migrations this code does not know.
    """
    known = {m.version: m for m in migrations}
    applied = applied_migrations(conn)
    for record in applied:
        migration = known.get(record.version)
        if migration is None:
            msg = (
                f"the database has migration {record.version:04d}_{record.name}, which this "
                "version of LogLab does not know; it is newer than this code"
            )
            raise MigrationError(msg)
        if migration.sha256 != record.sha256 or migration.name != record.name:
            msg = (
                f"migration {migration.label} has changed since it was applied "
                f"(recorded SHA-256 {record.sha256}, file SHA-256 {migration.sha256}); "
                "an applied migration must never be edited, add a new one instead"
            )
            raise MigrationError(msg)
    done = {record.version for record in applied}
    return [m for m in migrations if m.version not in done]


def migrate(
    conn: psycopg.Connection[TupleRow],
    migrations: Sequence[Migration] | None = None,
) -> list[Migration]:
    """Apply every pending migration, each in its own transaction; return those applied.

    ``conn`` must be in autocommit mode so each migration gets its own transaction.

    Raises:
        MigrationError: See :func:`pending_migrations`, or ``conn`` is not in autocommit.
        psycopg.Error: A migration failed; it and everything after it were not applied.
    """
    if not conn.autocommit:
        msg = "migrate needs an autocommit connection so each migration has its own transaction"
        raise MigrationError(msg)
    wanted = load_migrations() if migrations is None else migrations
    with conn.transaction():
        conn.execute("SELECT pg_advisory_xact_lock(%s)", (_LOCK_KEY,))
        conn.execute(_BOOTSTRAP)
    pending_migrations(conn, wanted)  # Fail before applying anything if the record is wrong.
    applied: list[Migration] = []
    for migration in wanted:
        with conn.transaction():
            conn.execute("SELECT pg_advisory_xact_lock(%s)", (_LOCK_KEY,))
            # Another process may have applied it while this one waited for the lock.
            if migration not in pending_migrations(conn, wanted):
                continue
            conn.execute(migration.sql)
            conn.execute(
                "INSERT INTO meta.schema_migration (version, name, sha256) VALUES (%s, %s, %s)",
                (migration.version, migration.name, migration.sha256),
            )
        applied.append(migration)
    return applied
