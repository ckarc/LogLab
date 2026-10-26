# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Real PostgreSQL databases for the storage tests.

Each test gets its own database, so tests cannot see each other's QSOs. Fresh
ones are cloned from a template that is migrated once per run. The server is
whatever libpq's environment points at (``PGHOST``, ``PGUSER``, ...); the
maintenance database used to create and drop test databases is
``$LABLOG_TEST_ADMIN_DATABASE`` (default ``dbname=postgres``). There is no
skip: if PostgreSQL is unreachable, these tests fail.
"""

from __future__ import annotations

import io
import os
import uuid
from typing import TYPE_CHECKING

import psycopg
import pytest
from psycopg import sql

from loglab_engine.cli import log, operator
from loglab_engine.environment import setting
from loglab_engine.store import Actor, IdentityStore, LogStore, Permission, connect, migrate

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator
    from pathlib import Path

    from psycopg.rows import TupleRow

ADMIN = setting("TEST_ADMIN_DATABASE") or "dbname=postgres"


def _admin(statement: sql.Composed) -> None:
    with psycopg.connect(ADMIN, autocommit=True) as conn:
        conn.execute(statement)


def create_database(template: str | None = None) -> str:
    """Create an empty (or ``template``-cloned) database and return its name."""
    name = f"loglab_test_{uuid.uuid4().hex[:12]}"
    statement = sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name))
    if template is not None:
        statement = sql.SQL("{} TEMPLATE {}").format(statement, sql.Identifier(template))
    _admin(statement)
    return name


def drop_database(name: str) -> None:
    """Drop a test database, closing any connections still open to it."""
    _admin(sql.SQL("DROP DATABASE IF EXISTS {} WITH (FORCE)").format(sql.Identifier(name)))


def conninfo(name: str) -> str:
    """A connection string for a test database."""
    return f"dbname={name}"


@pytest.fixture(scope="session")
def migrated_template() -> Iterator[str]:
    """A database with every migration applied, cloned for each test."""
    name = create_database()
    try:
        with connect(conninfo(name)) as conn:
            migrate(conn)
        yield name
    finally:
        drop_database(name)


@pytest.fixture
def empty_database() -> Iterator[str]:
    """A brand-new database with no LabLog schema at all."""
    name = create_database()
    try:
        yield name
    finally:
        drop_database(name)


@pytest.fixture
def database(migrated_template: str) -> Iterator[str]:
    """A fresh, fully migrated database of this test's own."""
    name = create_database(migrated_template)
    try:
        yield name
    finally:
        drop_database(name)


@pytest.fixture
def conn(database: str) -> Iterator[psycopg.Connection[TupleRow]]:
    """An autocommit UTC connection to this test's database."""
    with connect(conninfo(database)) as connection:
        yield connection


@pytest.fixture
def store(conn: psycopg.Connection[TupleRow]) -> LogStore:
    """The log store on this test's database."""
    return LogStore(conn)


ADMIN_SECRET = "73-de-k9ksu"  # noqa: S105 - a test account's PIN
VISITOR_SECRET = "246810"  # noqa: S105 - a test account's PIN


@pytest.fixture
def identity(conn: psycopg.Connection[TupleRow]) -> IdentityStore:
    """Operator accounts and sign-in on this test's database."""
    return IdentityStore(conn)


@pytest.fixture
def shack(identity: IdentityStore) -> Actor:
    """K9KSU, the first administrator, signed in at the shack."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    return identity.sign_in("K9KSU", ADMIN_SECRET, "shack").actor


@pytest.fixture
def visitor(identity: IdentityStore, shack: Actor) -> Actor:
    """W0XYZ, added by K9KSU with view and log, signed in at a Field Day position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return identity.sign_in("W0XYZ", VISITOR_SECRET, "field-day-2").actor


class Terminal:
    """``lablog-log`` in a terminal of its own, with its own session file."""

    def __init__(self, database: str, session_file: Path) -> None:
        """A terminal on ``database`` that keeps its session in ``session_file``."""
        self.database = conninfo(database)
        self.session_file = session_file

    def run(
        self, entry: Callable[..., int], argv: tuple[str, ...], *extra: object
    ) -> tuple[int, str, str]:
        """Run a command line's ``main`` with this terminal's session file."""
        out, err = io.StringIO(), io.StringIO()
        before = os.environ.get(operator.SESSION_FILE_ENV)
        os.environ[operator.SESSION_FILE_ENV] = str(self.session_file)
        try:
            code = entry(["--database", self.database, *argv], out, err, *extra)
        finally:
            if before is None:
                del os.environ[operator.SESSION_FILE_ENV]
            else:
                os.environ[operator.SESSION_FILE_ENV] = before
        return code, out.getvalue(), err.getvalue()

    def __call__(self, *argv: str) -> tuple[int, str, str]:
        """Run ``lablog-log``; return its exit code, output, and errors."""
        return self.run(log.main, argv)

    def operator(self, *argv: str, secret: str = ADMIN_SECRET) -> tuple[int, str, str]:
        """Run ``lablog-operator``, typing ``secret`` at every prompt."""
        return self.run(operator.main, argv, lambda _prompt: secret)


@pytest.fixture
def cli(database: str, tmp_path: Path) -> Terminal:
    """K9KSU's terminal: the first administrator, signed in at the shack."""
    terminal = Terminal(database, tmp_path / "k9ksu.session")
    assert terminal.operator("setup", "k9ksu")[0] == operator.EXIT_OK
    assert terminal.operator("sign-in", "k9ksu", "--station", "shack")[0] == operator.EXIT_OK
    return terminal


@pytest.fixture
def visitor_cli(cli: Terminal, database: str, tmp_path: Path) -> Terminal:
    """W0XYZ's terminal at a Field Day position, added by K9KSU with view and log."""
    assert cli.operator("add", "w0xyz", secret=VISITOR_SECRET)[0] == operator.EXIT_OK
    terminal = Terminal(database, tmp_path / "w0xyz.session")
    code, _, err = terminal.operator("sign-in", "W0XYZ", "--station", "fd-2", secret=VISITOR_SECRET)
    assert code == operator.EXIT_OK, err
    return terminal
