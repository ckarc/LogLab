# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Who programs at an Agent's station log as, asked by the Agent (Principles XXI, V)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.fieldlab import FieldLabBus
from loglab_engine.store.agent import AgentKeyStore

from .conftest import ADMIN_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor


NOW = 1_800_000_000.0


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> dict[str, str]:
    """The headers of the Agent at fd-1, with a key K9KSU made for it."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fd-1')}"}


def _sign_in(client: TestClient, station: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions", json={"callsign": "K9KSU", "secret": ADMIN_SECRET, "station": station}
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


def test_with_nobody_signed_in_there_is_no_call(client: TestClient, agent: dict[str, str]) -> None:
    """The Agent learns the station is empty, so programs keep what they have."""
    response = client.get("/api/v1/agent/station", headers=agent)
    assert response.status_code == 200, response.text
    assert response.json() == {
        "station": "fd-1",
        "operator": None,
        "station_call": None,
        "operation": None,
        "grid": None,
        "grid_source": None,
    }


def test_the_operator_signed_in_there_is_the_call(
    client: TestClient, agent: dict[str, str]
) -> None:
    """With no operation chosen, programs send the operator's own call."""
    _sign_in(client, "fd-1")
    _sign_in(client, "fd-2")  # elsewhere: doesn't count, but K9KSU is still at fd-1
    body = client.get("/api/v1/agent/station", headers=agent).json()
    assert (body["operator"], body["station_call"], body["operation"]) == ("K9KSU", "K9KSU", None)


def test_the_chosen_operation_gives_the_call(client: TestClient, agent: dict[str, str]) -> None:
    """In an operation, programs send its station call (Field Day's club call, say)."""
    headers = _sign_in(client, "fd-1")
    response = client.post(
        "/api/v1/operations",
        headers=headers,
        json={"overlay": "everyday", "name": "Saturday", "station_call": "W0CK"},
    )
    assert response.status_code == 201, response.text
    layout = {"modules": [{"id": "entry"}], "operation": {"id": response.json()["id"]}}
    assert client.put("/api/v1/layout", headers=headers, json=layout).status_code == 200
    body = client.get("/api/v1/agent/station", headers=agent).json()
    assert (body["operator"], body["station_call"], body["operation"]) == (
        "K9KSU",
        "W0CK",
        "Saturday",
    )


def test_fieldlabs_gps_gives_the_grid(database: str, agent: dict[str, str]) -> None:
    """A measured fix comes first, and says it is from GPS (W1AW's, here)."""
    fix = b'{"lat": 41.714775, "lon": -72.727260, "ts": %d}' % (NOW - 20)
    bus = FieldLabBus({"fieldlab/location": fix}.get, clock=lambda: NOW)
    bus.read_once()
    client = TestClient(create_app(conninfo(database), fieldlab=bus))
    body = client.get("/api/v1/agent/station", headers=agent).json()
    assert (body["grid"], body["grid_source"]) == ("FN31pr", "gps")


def test_only_an_agent_with_a_key_may_ask(client: TestClient, shack: Actor) -> None:
    """A browser's session or no key at all is refused (Principle XXI)."""
    del shack
    browser = _sign_in(client, "fd-1")
    assert client.get("/api/v1/agent/station", headers=browser).status_code == 401
    assert client.get("/api/v1/agent/station").status_code == 401
