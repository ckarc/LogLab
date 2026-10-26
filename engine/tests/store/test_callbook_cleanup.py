# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Clearing old kept lookups of calls never logged: admins only, the log untouched (VI, XXI)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store.callbook import CallbookStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_api import W1AW
from .test_callbook import JPEG, _sign_in

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

K1XX = [{**f, "value": "K1XX"} if f["name"] == "CALL" else f for f in W1AW]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")


def _keep(conn: psycopg.Connection[TupleRow], call: str, *, days: int, photo: bool = False) -> None:
    store = CallbookStore(conn)
    store.remember("qrz", call, found=True, entry={"call": call, "bio": "no"})
    if photo:
        store.remember_media(
            "qrz", call, "photo", content=JPEG, content_type="image/jpeg", origin=None
        )
    with conn.transaction():
        for table in ("callbook_entry", "callbook_media"):
            conn.execute(f"ALTER TABLE reference.{table} DISABLE TRIGGER ALL")
            conn.execute(
                f"UPDATE reference.{table} SET fetched_at = now() - make_interval(days => %s)"  # noqa: S608 - fixed table names
                " WHERE call = %s",
                (days, call),
            )
            conn.execute(f"ALTER TABLE reference.{table} ENABLE TRIGGER ALL")


def _kept(conn: psycopg.Connection[TupleRow]) -> list[str]:
    rows = conn.execute("SELECT call FROM reference.callbook_entry ORDER BY call").fetchall()
    return [row[0] for row in rows]


def test_only_old_lookups_of_never_logged_calls_are_counted_and_cleared(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """A logged call keeps its lookup, a deleted QSO's too; a recent one stays."""
    assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": W1AW}).status_code == 201
    gone = client.post("/api/v1/qsos", headers=k9ksu, json={"fields": K1XX}).json()["qso"]
    deleted = client.post(f"/api/v1/qsos/{gone['id']}/delete", headers=k9ksu, json={})
    assert deleted.status_code == 200
    _keep(conn, "W1AW", days=400, photo=True)  # logged
    _keep(conn, "K1XX", days=400)  # in a deleted QSO: still logged once
    _keep(conn, "XX9XX", days=400, photo=True)  # never logged, old
    _keep(conn, "N0NEW", days=10)  # never logged, recent

    shown = client.get("/api/v1/callbook/cleanup", headers=k9ksu, params={"days": 90})
    assert shown.status_code == 200, shown.text
    assert shown.json() == {"days": 90, "calls": 1, "photos_and_bios": 1, "bytes": len(JPEG)}
    assert client.get("/api/v1/callbook/cleanup", headers=k9ksu).json()["days"] == 90

    cleared = client.post("/api/v1/callbook/cleanup", headers=k9ksu, json={"days": 90})
    assert cleared.status_code == 200, cleared.text
    assert cleared.json() == {"days": 90, "removed": 1}
    assert _kept(conn) == ["K1XX", "N0NEW", "W1AW"]
    assert CallbookStore(conn).media("qrz", "XX9XX", "photo") is None
    assert CallbookStore(conn).media("qrz", "W1AW", "photo") is not None
    # The log is exactly as it was.
    listed = client.get("/api/v1/qsos?include_deleted=true", headers=k9ksu).json()
    assert sorted(q["call"] for q in listed) == ["K1XX", "W1AW"]
    # A shorter age now reaches the recent one; nothing else.
    assert client.post("/api/v1/callbook/cleanup", headers=k9ksu, json={"days": 30}).json() == {
        "days": 30,
        "removed": 0,
    }


def test_only_an_admin_may_count_or_clear_and_only_offered_ages(
    client: TestClient, k9ksu: dict[str, str], visitor: Actor, conn: psycopg.Connection[TupleRow]
) -> None:
    """The clean-up is an admin's tool (Principle XXI)."""
    del visitor
    _keep(conn, "XX9XX", days=400)
    w0xyz = _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")
    assert client.get("/api/v1/callbook/cleanup", headers=w0xyz).status_code == 403
    refused = client.post("/api/v1/callbook/cleanup", headers=w0xyz, json={"days": 90})
    assert refused.status_code == 403
    assert client.post("/api/v1/callbook/cleanup", json={"days": 90}).status_code == 401
    odd = client.post("/api/v1/callbook/cleanup", headers=k9ksu, json={"days": 1})
    assert odd.status_code == 422
    assert _kept(conn) == ["XX9XX"]
