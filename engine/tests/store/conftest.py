# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Real PostgreSQL databases for the storage tests.

Each test gets its own database, so tests cannot see each other's QSOs. Fresh
ones are cloned from a template that is migrated once per run. The server is
whatever libpq's environment points at (``PGHOST``, ``PGUSER``, ...); the
maintenance database used to create and drop test databases is
``$LOGLAB_TEST_ADMIN_DATABASE`` (default ``dbname=postgres``). There is no
skip: if PostgreSQL is unreachable, these tests fail.
"""

from __future__ import annotations

import os
import uuid
from typing import TYPE_CHECKING

import psycopg
import pytest
from psycopg import sql

from loglab_engine.store import LogStore, connect, migrate

if TYPE_CHECKING:
    from collections.abc import Iterator

    from psycopg.rows import TupleRow

ADMIN = os.environ.get("LOGLAB_TEST_ADMIN_DATABASE", "dbname=postgres")


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
    """A brand-new database with no LogLab schema at all."""
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
