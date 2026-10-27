# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""A radio's settings, kept by radio so every station recognizes it (Principle X)."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store.agent import AgentKeyStore

from .conftest import ADMIN_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

RADIO = {"maker": "Icom", "model": "IC-7300", "serial": "IC-7300_02065962"}


def _settings(nr: str = "00", rf_gain: str = "02 55") -> list[dict[str, Any]]:
    return [
        {"id": "noise_reduction", "label": "Noise reduction (NR)", "bytes": nr},
        {"id": "rf_gain", "label": "RF gain", "bytes": rf_gain},
        {"id": "item_0050", "label": "item 0050", "bytes": None},
    ]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def agents(conn: psycopg.Connection[TupleRow], shack: Actor) -> dict[str, dict[str, str]]:
    """The headers of the Agents at two FieldLab computers, fl-1 and fl-2."""
    keys = AgentKeyStore(conn)
    return {
        station: {"Authorization": f"Bearer {keys.make(shack, station)}"}
        for station in ("fl-1", "fl-2")
    }


def _keep(client: TestClient, agent: dict[str, str], **body: Any) -> Any:  # noqa: ANN401
    return client.post("/api/v1/agent/radio-settings", headers=agent, json=RADIO | body)


def _kept(client: TestClient, agent: dict[str, str], radio: dict[str, str] = RADIO) -> Any:  # noqa: ANN401
    response = client.get("/api/v1/agent/radio-settings", headers=agent, params=radio)
    assert response.status_code == 200, response.text
    return response.json()


def test_a_radio_carried_to_another_computer_is_recognized(
    client: TestClient, agents: dict[str, dict[str, str]]
) -> None:
    """What one station's Agent kept, another station's Agent finds, by serial number."""
    response = _keep(client, agents["fl-1"], settings=_settings())
    assert response.status_code == 201, response.text
    assert response.json()["created"] is True
    response = _keep(client, agents["fl-1"], name="FT8", settings=_settings(rf_gain="01 80"))
    assert response.status_code == 201, response.text
    found = _kept(client, agents["fl-2"])
    assert found["as_received"]["station"] == "fl-1"
    assert found["as_received"]["settings"] == _settings()
    ((ft8,),) = (found["setups"],)
    assert (ft8["name"], ft8["station"], ft8["operator"]) == ("FT8", "fl-1", None)
    assert ft8["settings"][1] == {"id": "rf_gain", "label": "RF gain", "bytes": "01 80"}


def test_the_as_received_record_is_never_replaced(
    client: TestClient, agents: dict[str, dict[str, str]]
) -> None:
    """A second first reading is answered with the first one, unchanged."""
    assert _keep(client, agents["fl-1"], settings=_settings()).status_code == 201
    response = _keep(client, agents["fl-2"], settings=_settings(nr="01"))
    assert response.status_code == 200, response.text
    assert response.json()["created"] is False
    assert response.json()["reading"]["station"] == "fl-1"
    assert _kept(client, agents["fl-2"])["as_received"]["settings"][0]["bytes"] == "00"


def test_keeping_a_setup_again_makes_a_new_version(
    client: TestClient, agents: dict[str, dict[str, str]], conn: psycopg.Connection[TupleRow]
) -> None:
    """The newest version is the setup; every version is kept."""
    _keep(client, agents["fl-1"], name="VARA", settings=_settings(nr="00"))
    _keep(client, agents["fl-2"], name="VARA", settings=_settings(nr="01"))
    _keep(client, agents["fl-2"], name="FT8", settings=_settings())
    setups = _kept(client, agents["fl-1"])["setups"]
    assert [s["name"] for s in setups] == ["FT8", "VARA"]
    assert setups[1]["settings"][0]["bytes"] == "01"
    assert setups[1]["station"] == "fl-2"
    (count,) = conn.execute("SELECT count(*) FROM log.radio_setup").fetchone() or (0,)
    assert count == 3
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("UPDATE log.radio_setup SET name = 'x'")


def test_radios_are_kept_apart_by_serial_number(
    client: TestClient, agents: dict[str, dict[str, str]]
) -> None:
    """Another IC-7300 has nothing kept until its own Agent sends it."""
    _keep(client, agents["fl-1"], name="FT8", settings=_settings())
    other = RADIO | {"serial": "IC-7300_03000001"}
    assert _kept(client, agents["fl-1"], other) == {"as_received": None, "setups": []}


def test_a_setup_names_who_was_signed_in_there(
    client: TestClient, agents: dict[str, dict[str, str]], shack: Actor
) -> None:
    """The operator signed in at the station is kept with the setup."""
    del shack
    signed = client.post(
        "/api/v1/sessions", json={"callsign": "K9KSU", "secret": ADMIN_SECRET, "station": "fl-1"}
    )
    assert signed.status_code == 201, signed.text
    response = _keep(client, agents["fl-1"], name="FT8", settings=_settings())
    assert response.json()["reading"]["operator"] == "K9KSU"


def test_only_an_agent_with_a_station_key_may_send_or_ask(client: TestClient) -> None:
    """No key, no settings (Principle XXI)."""
    response = client.post("/api/v1/agent/radio-settings", json=RADIO | {"settings": _settings()})
    assert response.status_code == 401
    response = client.get("/api/v1/agent/radio-settings", params=RADIO)
    assert response.status_code == 401
    bad = {"Authorization": "Bearer lls_nope"}
    assert client.get("/api/v1/agent/radio-settings", headers=bad, params=RADIO).status_code == 401


@pytest.mark.parametrize(
    "change",
    [
        {"serial": "has space"},
        {"name": ""},
        {"settings": []},
        {"settings": [{"id": "Bad-Id", "label": "x", "bytes": "00"}]},
        {"settings": [{"id": "rf_gain", "label": "RF gain", "bytes": "zz"}]},
        {"settings": [{"id": "rf_gain", "label": "RF\ngain", "bytes": "00"}]},
        {"extra": True},
    ],
)
def test_a_reading_that_is_not_valid_is_refused(
    client: TestClient, agents: dict[str, dict[str, str]], change: dict[str, Any]
) -> None:
    """Every field is checked at the door (Principle XII)."""
    body = RADIO | {"settings": _settings()} | change
    response = client.post("/api/v1/agent/radio-settings", headers=agents["fl-1"], json=body)
    assert response.status_code == 422, response.text


def test_the_same_setting_twice_is_refused(
    client: TestClient, agents: dict[str, dict[str, str]]
) -> None:
    """A reading names each setting once."""
    twice = [*_settings(), _settings()[0]]
    response = _keep(client, agents["fl-1"], settings=twice)
    assert response.status_code in {400, 422}, response.text
