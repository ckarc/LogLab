# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Lab Stock: each model's everyday settings, changed only through an administrator's approval."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store.agent import AgentKeyStore
from loglab_engine.store.lab_stock import LabStockStore, StockSetting
from loglab_engine.store.log import NotAuthorizedError

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

RADIO = {"maker": "Icom", "model": "IC-7300", "serial": "IC-7300_02065962"}
Headers = dict[str, str]


def _settings(nr: str = "00", data_mod: str = "03") -> list[dict[str, Any]]:
    return [
        {"id": "noise_reduction", "label": "Noise reduction (NR)", "bytes": nr, "shown": "OFF"},
        {"id": "data_mod", "label": "DATA MOD", "bytes": data_mod, "shown": "USB"},
    ]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> Headers:
    """The headers of the Agent at FieldLab's first computer."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fl-1')}"}


def _sign_in(client: TestClient, callsign: str, secret: str) -> Headers:
    response = client.post(
        "/api/v1/sessions", json={"callsign": callsign, "secret": secret, "station": "shack"}
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def admin(client: TestClient, shack: Actor) -> Headers:
    """K9KSU, an administrator, signed in to the web client."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET)


@pytest.fixture
def operator(client: TestClient, visitor: Actor) -> Headers:
    """W0XYZ, who may view and log but is not an administrator."""
    del visitor
    return _sign_in(client, "W0XYZ", VISITOR_SECRET)


def _propose(client: TestClient, agent: Headers, **change: Any) -> Any:  # noqa: ANN401
    body = RADIO | {"settings": _settings(), "note": "after Field Day"} | change
    response = client.post("/api/v1/agent/lab-stock", headers=agent, json=body)
    assert response.status_code == 201, response.text
    return response.json()


def _current(client: TestClient, agent: Headers) -> Any:  # noqa: ANN401
    response = client.get(
        "/api/v1/agent/lab-stock", headers=agent, params={"maker": "Icom", "model": "IC-7300"}
    )
    assert response.status_code == 200, response.text
    return response.json()["lab_stock"]


def _approve(client: TestClient, who: Headers, proposal: int, against: int | None) -> Any:  # noqa: ANN401
    return client.post(
        f"/api/v1/lab-stock/{proposal}/approve", headers=who, json={"against": against}
    )


def test_a_proposal_changes_nothing_until_an_administrator_approves_it(
    client: TestClient, agent: Headers, admin: Headers
) -> None:
    """The Agent's proposal is kept; only the approval makes it the Lab Stock."""
    proposed = _propose(client, agent)
    assert proposed["status"] == "proposed"
    assert (proposed["station"], proposed["serial"]) == ("fl-1", RADIO["serial"])
    assert proposed["settings"] == _settings()
    assert _current(client, agent) is None

    response = _approve(client, admin, proposed["id"], None)
    assert response.status_code == 200, response.text
    approved = response.json()
    assert (approved["status"], approved["current"]) == ("approved", True)
    assert approved["decision"]["by"] == "K9KSU"
    assert _current(client, agent)["id"] == proposed["id"]


def test_a_newer_approval_replaces_the_lab_stock_and_every_version_is_kept(
    client: TestClient, agent: Headers, admin: Headers, conn: psycopg.Connection[TupleRow]
) -> None:
    """The Lab Stock is the newest approval; earlier ones stay, and none can be edited."""
    first = _propose(client, agent)["id"]
    assert _approve(client, admin, first, None).status_code == 200
    second = _propose(client, agent, settings=_settings(nr="01"))["id"]
    assert _current(client, agent)["id"] == first
    assert _approve(client, admin, second, first).status_code == 200
    assert _current(client, agent)["settings"][0]["bytes"] == "01"

    listed = client.get("/api/v1/lab-stock", headers=admin).json()
    assert [(p["id"], p["status"], p["current"]) for p in listed] == [
        (second, "approved", True),
        (first, "approved", False),
    ]
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("UPDATE log.lab_stock SET note = 'x'")
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("DELETE FROM log.lab_stock_decision")


def test_approving_against_a_lab_stock_that_has_changed_is_refused(
    client: TestClient, agent: Headers, admin: Headers
) -> None:
    """Nobody approves a change they didn't see: the comparison must still hold."""
    first = _propose(client, agent)["id"]
    second = _propose(client, agent, settings=_settings(nr="01"))["id"]
    assert _approve(client, admin, first, None).status_code == 200
    response = _approve(client, admin, second, None)
    assert response.status_code == 409, response.text
    assert "changed" in response.json()["detail"]
    assert _current(client, agent)["id"] == first


def test_a_proposal_is_decided_once(client: TestClient, agent: Headers, admin: Headers) -> None:
    """Refused stays refused; approved can't be refused afterwards."""
    refused = _propose(client, agent)["id"]
    response = client.post(f"/api/v1/lab-stock/{refused}/refuse", headers=admin)
    assert response.status_code == 200, response.text
    assert response.json()["status"] == "refused"
    assert _approve(client, admin, refused, None).status_code == 409
    assert _current(client, agent) is None
    assert client.post("/api/v1/lab-stock/999/refuse", headers=admin).status_code == 404


def test_only_an_administrator_passes_the_gate(
    client: TestClient, agent: Headers, operator: Headers
) -> None:
    """An operator without admin, or a station key, can't see, approve, refuse or re-propose."""
    proposal = _propose(client, agent)["id"]
    for who in (operator, agent, {}):
        assert client.get("/api/v1/lab-stock", headers=who).status_code in {401, 403}
        assert _approve(client, who, proposal, None).status_code in {401, 403}
        response = client.post(f"/api/v1/lab-stock/{proposal}/refuse", headers=who)
        assert response.status_code in {401, 403}
        response = client.post(f"/api/v1/lab-stock/{proposal}/again", headers=who)
        assert response.status_code in {401, 403}
    assert _current(client, agent) is None


def test_the_database_itself_refuses_a_decision_by_a_non_administrator(
    conn: psycopg.Connection[TupleRow], visitor: Actor
) -> None:
    """The gate holds whatever code asks, not only through the API."""
    store = LabStockStore(conn)
    proposal = store.propose(
        "fl-1", "Icom", "IC-7300", RADIO["serial"], (StockSetting("att", "ATT", "00", None),)
    )
    with pytest.raises(NotAuthorizedError):
        store.decide(visitor, proposal.id, approve=True)
    with pytest.raises(psycopg.errors.InsufficientPrivilege):
        conn.execute(
            """
            INSERT INTO log.lab_stock_decision (proposal_id, approved, decided_by, session_id)
            SELECT %s, true, s.operator_id, s.id FROM log.session s WHERE s.id = %s
            """,
            (proposal.id, visitor.session_id),
        )
    assert store.current("Icom", "IC-7300") is None


def test_going_back_to_an_earlier_lab_stock_needs_its_own_approval(
    client: TestClient, agent: Headers, admin: Headers
) -> None:
    """Proposing an earlier one again makes a new proposal, which still waits for approval."""
    first = _propose(client, agent)["id"]
    _approve(client, admin, first, None)
    second = _propose(client, agent, settings=_settings(nr="01"))["id"]
    _approve(client, admin, second, first)
    response = client.post(f"/api/v1/lab-stock/{first}/again", headers=admin)
    assert response.status_code == 201, response.text
    again = response.json()
    assert (again["status"], again["again_of"], again["proposed_by"]) == (
        "proposed",
        first,
        "K9KSU",
    )
    assert again["settings"] == _settings()
    assert _current(client, agent)["id"] == second
    assert _approve(client, admin, again["id"], second).status_code == 200
    assert _current(client, agent)["settings"][0]["bytes"] == "00"


def test_models_have_their_own_lab_stock(
    client: TestClient, agent: Headers, admin: Headers
) -> None:
    """An IC-9700 proposal is compared with, and becomes, only the IC-9700's Lab Stock."""
    ic7300 = _propose(client, agent)["id"]
    _approve(client, admin, ic7300, None)
    ic9700 = _propose(client, agent, model="IC-9700", serial="IC-9700_1")["id"]
    assert _approve(client, admin, ic9700, None).status_code == 200
    assert _current(client, agent)["id"] == ic7300


def test_a_proposal_names_who_was_signed_in_at_the_station(
    client: TestClient, agent: Headers, shack: Actor
) -> None:
    """Who was at the radio is kept with what they proposed."""
    del shack
    signed = client.post(
        "/api/v1/sessions", json={"callsign": "K9KSU", "secret": ADMIN_SECRET, "station": "fl-1"}
    )
    assert signed.status_code == 201, signed.text
    assert _propose(client, agent)["proposed_by"] == "K9KSU"


@pytest.mark.parametrize(
    "change",
    [
        {"serial": "has space"},
        {"settings": []},
        {"settings": [{"id": "Bad-Id", "label": "x", "bytes": "00"}]},
        {"settings": [{"id": "rf_gain", "label": "RF gain", "bytes": None}]},
        {"settings": [{"id": "rf_gain", "label": "RF gain", "bytes": "zz"}]},
        {"note": "two\nlines"},
        {"extra": True},
    ],
)
def test_a_proposal_that_is_not_valid_is_refused(
    client: TestClient, agent: Headers, change: dict[str, Any]
) -> None:
    """Every field is checked at the door (Principle XII)."""
    body = RADIO | {"settings": _settings()} | change
    response = client.post("/api/v1/agent/lab-stock", headers=agent, json=body)
    assert response.status_code == 422, response.text


def test_only_an_agent_with_a_station_key_may_propose_or_ask(client: TestClient) -> None:
    """No key, no Lab Stock (Principle XXI)."""
    body = RADIO | {"settings": _settings()}
    assert client.post("/api/v1/agent/lab-stock", json=body).status_code == 401
    params = {"maker": "Icom", "model": "IC-7300"}
    assert client.get("/api/v1/agent/lab-stock", params=params).status_code == 401
