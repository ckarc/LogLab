# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Schema migrations: ordered, one transaction each, checksummed, and loud when tampered with."""

from __future__ import annotations

from typing import TYPE_CHECKING

import psycopg
import pytest

from loglab_engine.store import (
    Migration,
    MigrationError,
    applied_migrations,
    connect,
    load_migrations,
    migrate,
    pending_migrations,
)

from .conftest import conninfo

if TYPE_CHECKING:
    from pathlib import Path

    from psycopg.rows import TupleRow


def _schemas(conn: psycopg.Connection[TupleRow]) -> set[str]:
    rows = conn.execute("SELECT nspname FROM pg_namespace").fetchall()
    return {str(row[0]) for row in rows}


def test_packaged_migrations_are_numbered_from_one() -> None:
    """The shipped migrations load, in order, with no gaps."""
    migrations = load_migrations()
    assert [m.version for m in migrations] == list(range(1, len(migrations) + 1))
    assert migrations[0].label == "0001_schemas"


def test_fresh_database_gets_every_migration_recorded_with_its_checksum(
    empty_database: str,
) -> None:
    """Migrating a new database applies all files and records each one's SHA-256."""
    migrations = load_migrations()
    with connect(conninfo(empty_database)) as conn:
        applied = migrate(conn)
        assert applied == list(migrations)
        recorded = applied_migrations(conn)
        assert [(r.version, r.name, r.sha256) for r in recorded] == [
            (m.version, m.name, m.sha256) for m in migrations
        ]
        assert all(r.applied_at.utcoffset() is not None for r in recorded)
        assert {"log", "reference", "meta"} <= _schemas(conn)
        assert migrate(conn) == []
        assert pending_migrations(conn, migrations) == []


def test_a_changed_past_migration_fails_loudly_and_applies_nothing(empty_database: str) -> None:
    """Editing an applied migration file stops every later run before it touches anything."""
    first = Migration(1, "one", b"CREATE TABLE public.one (x integer);")
    second = Migration(2, "two", b"CREATE TABLE public.two (x integer);")
    with connect(conninfo(empty_database)) as conn:
        migrate(conn, [first])
        edited = Migration(1, "one", b"CREATE TABLE public.one (x bigint);")
        with pytest.raises(MigrationError, match="0001_one has changed since it was applied"):
            migrate(conn, [edited, second])
        assert conn.execute("SELECT to_regclass('public.two')").fetchone() == (None,)
        with pytest.raises(MigrationError, match="has changed"):
            pending_migrations(conn, [edited, second])


def test_a_renamed_past_migration_fails_loudly(empty_database: str) -> None:
    """The recorded name is part of the identity too."""
    with connect(conninfo(empty_database)) as conn:
        migrate(conn, [Migration(1, "one", b"SELECT 1;")])
        with pytest.raises(MigrationError, match="has changed"):
            migrate(conn, [Migration(1, "uno", b"SELECT 1;")])


def test_a_database_newer_than_the_code_is_refused(empty_database: str) -> None:
    """Migrations the code doesn't know mean it must not touch this database."""
    with connect(conninfo(empty_database)) as conn:
        migrate(conn, [Migration(1, "one", b"SELECT 1;"), Migration(2, "two", b"SELECT 2;")])
        with pytest.raises(MigrationError, match="newer than this code"):
            migrate(conn, [Migration(1, "one", b"SELECT 1;")])


def test_each_migration_is_its_own_transaction(empty_database: str) -> None:
    """A failing migration leaves nothing of itself behind; earlier ones stay applied."""
    good = Migration(1, "good", b"CREATE TABLE public.good (x integer);")
    bad = Migration(
        2,
        "bad",
        b"CREATE TABLE public.half (x integer); SELECT no_such_function();",
    )
    with connect(conninfo(empty_database)) as conn:
        with pytest.raises(psycopg.errors.UndefinedFunction):
            migrate(conn, [good, bad])
        assert [r.version for r in applied_migrations(conn)] == [1]
        assert conn.execute("SELECT to_regclass('public.good')").fetchone() != (None,)
        assert conn.execute("SELECT to_regclass('public.half')").fetchone() == (None,)
        fixed = Migration(2, "bad", b"CREATE TABLE public.half (x integer);")
        assert migrate(conn, [good, fixed]) == [fixed]


def test_migrate_needs_autocommit(empty_database: str) -> None:
    """Without autocommit, migrations would share one implicit transaction."""
    with psycopg.connect(conninfo(empty_database)) as conn:
        with pytest.raises(MigrationError, match="autocommit"):
            migrate(conn, [])
        conn.rollback()


def test_migration_files_are_checked(tmp_path: Path) -> None:
    """Misnamed files and gaps in the numbering are refused."""
    (tmp_path / "0001_first.sql").write_bytes(b"SELECT 1;")
    (tmp_path / "README.txt").write_text("not a migration")
    assert [m.label for m in load_migrations(tmp_path)] == ["0001_first"]

    (tmp_path / "0003_third.sql").write_bytes(b"SELECT 3;")
    with pytest.raises(MigrationError, match="no gaps"):
        load_migrations(tmp_path)

    (tmp_path / "0003_third.sql").unlink()
    (tmp_path / "2_Bad-Name.sql").write_bytes(b"SELECT 2;")
    with pytest.raises(MigrationError, match="is not named"):
        load_migrations(tmp_path)


def test_checksum_is_of_the_exact_bytes() -> None:
    """Even a whitespace change is a different migration."""
    assert Migration(1, "a", b"SELECT 1;").sha256 != Migration(1, "a", b"SELECT 1; ").sha256
