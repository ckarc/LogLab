# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""QSOs a program at a station logged, sent by its Station Agent (XXI, XV, VII)."""

from __future__ import annotations

import uuid
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, LogStore, Permission
from loglab_engine.store.agent import AgentKeyStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

# What WSJT-X sends in its "Logged ADIF" message: a little header, then one QSO.
LOGGED = (
    "\n<adif_ver:5>3.1.0\n<programid:6>WSJT-X\n<EOH>\n"
    "<call:4>W1AW <gridsquare:4>FN31 <mode:3>FT8 <rst_sent:3>-10 <rst_rcvd:3>-12 "
    "<qso_date:8>20270626 <time_on:6>183015 <qso_date_off:8>20270626 <time_off:6>183130 "
    "<band:3>20m <freq:9>14.075500 <station_callsign:5>K9KSU <my_gridsquare:4>EN52 <EOR>"
)


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


def _sign_in(client: TestClient, callsign: str, secret: str, station: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions", json={"callsign": callsign, "secret": secret, "station": station}
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> dict[str, str]:
    """The headers of the Agent at fd-1, with a key K9KSU made for it."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fd-1')}"}


def _send(
    client: TestClient, agent: dict[str, str], adif: str = LOGGED, qso_id: uuid.UUID | None = None
) -> Any:  # noqa: ANN401 - the test client's response
    body = {"id": str(qso_id or uuid.uuid4()), "adif": adif, "program": "WSJT-X"}
    return client.post("/api/v1/agent/qsos", headers=agent, json=body)


def test_a_wsjtx_qso_is_logged_as_the_operator_at_that_station(
    client: TestClient, agent: dict[str, str], shack: Actor, conn: psycopg.Connection[TupleRow]
) -> None:
    """The QSO is logged as if typed by whoever is signed in at the Agent's station."""
    del shack
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    response = _send(client, agent)
    assert response.status_code == 201, response.text
    assert response.json()["operator"] == "K9KSU"
    (logged,) = LogStore(conn).find(limit=5)
    assert (logged.operator, logged.station) == ("K9KSU", "fd-1")
    assert logged.record.get("RST_RCVD") == "-12"
    assert logged.record.get("GRIDSQUARE") == "FN31"
    history = LogStore(conn).history(logged.qso_id)
    assert "WSJT-X" in (history[0].note or "")


def test_the_latest_operator_at_the_station_is_the_one(
    client: TestClient,
    agent: dict[str, str],
    identity: IdentityStore,
    shack: Actor,
) -> None:
    """The operator who signed in last at fd-1 takes over its programs' QSOs."""
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-1")
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-2")  # elsewhere: doesn't count
    response = _send(client, agent)
    assert response.json()["operator"] == "W0XYZ"


def test_with_nobody_signed_in_the_qso_waits(client: TestClient, agent: dict[str, str]) -> None:
    """503 tells the Agent to keep the QSO in its journal and try again (Principle XV)."""
    response = _send(client, agent)
    assert response.status_code == 503
    assert "nobody is signed in" in response.json()["detail"]


def test_a_resend_logs_nothing_more(
    client: TestClient, agent: dict[str, str], shack: Actor, conn: psycopg.Connection[TupleRow]
) -> None:
    """The same journal ID again is the same QSO (Principle XV)."""
    del shack
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    qso_id = uuid.uuid4()
    assert _send(client, agent, qso_id=qso_id).status_code == 201
    again = _send(client, agent, qso_id=qso_id)
    assert again.status_code == 200
    assert again.json()["created"] is False
    assert len(LogStore(conn).find(limit=5)) == 1


def test_a_qso_already_imported_is_not_logged_twice(
    client: TestClient, agent: dict[str, str], shack: Actor, conn: psycopg.Connection[TupleRow]
) -> None:
    """The same contact from the log file and from the message is one QSO."""
    del shack
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    assert _send(client, agent).status_code == 201
    other = _send(client, agent)  # a new journal ID, the same contact
    assert other.status_code == 200
    assert "already in the log" in other.json()["message"]
    assert len(LogStore(conn).find(limit=5)) == 1


def test_it_goes_in_the_operators_chosen_operation(
    client: TestClient, agent: dict[str, str], shack: Actor, conn: psycopg.Connection[TupleRow]
) -> None:
    """In an operation, the QSO gets its fields and must fall within its hours."""
    del shack
    headers = _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    response = client.post(
        "/api/v1/operations",
        headers=headers,
        json={
            "overlay": "everyday",
            "name": "Saturday",
            "station_call": "W0CK",
            "starts_at": "2027-06-26T18:00:00Z",
            "ends_at": "2027-06-26T18:59:00Z",
        },
    )
    assert response.status_code == 201, response.text
    op = response.json()
    layout = {"modules": [{"id": "entry"}], "operation": {"id": op["id"], "gota": False}}
    assert client.put("/api/v1/layout", headers=headers, json=layout).status_code == 200
    assert _send(client, agent).status_code == 201
    (logged,) = LogStore(conn).find(limit=5)
    assert logged.record.get("APP_LOGLAB_OPERATION") == op["id"]
    assert logged.record.get("STATION_CALLSIGN") == "K9KSU"  # WSJT-X's own field stays
    late = LOGGED.replace("<time_on:6>183015", "<time_on:6>190015").replace("W1AW", "K0AB")
    refused = _send(client, agent, late)
    assert refused.status_code == 409
    assert "outside" in refused.json()["detail"]


def test_what_is_not_one_qso_is_refused(
    client: TestClient, agent: dict[str, str], shack: Actor
) -> None:
    """Untrusted input is checked where it enters (Principle XII)."""
    del shack
    _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    assert _send(client, agent, "<call:99>W1AW").status_code == 422
    two = LOGGED + LOGGED.split("<EOH>\n", 1)[1]
    assert _send(client, agent, two).status_code == 422


def test_only_an_agent_with_a_key_can_send(client: TestClient, shack: Actor) -> None:
    """A browser's session or no key at all is refused (Principle XXI)."""
    del shack
    browser = _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    assert _send(client, browser).status_code == 401
    assert _send(client, {}).status_code == 401
