# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Lab profiles: an administrator updates a program's default profile from a station's radio."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.profile_reads import RadioRead
from loglab_engine.store.agent import AgentKeyStore
from loglab_engine.store.lab_profile import LabProfileStore
from loglab_engine.store.lab_stock import StockSetting
from loglab_engine.store.log import NotAuthorizedError

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

RADIO = {"maker": "Icom", "model": "IC-7300", "serial": "IC-7300_02065962"}
Headers = dict[str, str]


def _radio(rf_power: str = "00 69", data_mod: str = "03") -> list[dict[str, Any]]:
    return [
        {"id": "mode_and_filter", "label": "Mode and filter", "bytes": "01 01", "shown": "USB"},
        {"id": "rf_power", "label": "RF power", "bytes": rf_power, "shown": "176 of 255 (69%)"},
        {"id": "data_mod", "label": "DATA MOD", "bytes": data_mod, "shown": "USB (03h)"},
        {"id": "agc", "label": "AGC", "bytes": "02", "shown": "MID"},
    ]


def _built_in() -> list[dict[str, Any]]:
    return [
        {"id": "mode_and_filter", "label": "Mode and filter", "bytes": "01 01", "shown": "USB"},
        {"id": "data_mod", "label": "DATA MOD", "bytes": "03", "shown": "USB (03h)"},
    ]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> Headers:
    """The headers of the Agent at FieldLab's first computer."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fl-1')}"}


@pytest.fixture
def other_agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> Headers:
    """The headers of the Agent at FieldLab's second computer."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fl-2')}"}


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


def _ask(client: TestClient, admin: Headers, program: str = "VARA") -> Any:  # noqa: ANN401
    response = client.post(
        "/api/v1/profile-reads", headers=admin, json={"station": "fl-1", "program": program}
    )
    assert response.status_code == 201, response.text
    return response.json()


def _answer(client: TestClient, agent: Headers, read_id: str, **change: Any) -> None:  # noqa: ANN401
    body = (
        RADIO
        | {
            "settings": _radio(),
            "profile": _built_in(),
            "profile_from": "built into the Agent",
            "changed": ["rf_power"],
        }
        | change
    )
    response = client.post(f"/api/v1/agent/profile-reads/{read_id}", headers=agent, json=body)
    assert response.status_code == 204, response.text


def _read(client: TestClient, admin: Headers, program: str = "VARA", **change: Any) -> Any:  # noqa: ANN401
    """Ask fl-1's Agent to read its radio, have it answer, and return the reading."""
    asked = _ask(client, admin, program)
    agent = {"Authorization": admin["X-Agent"]}
    _answer(client, agent, asked["id"], **change)
    response = client.get(f"/api/v1/profile-reads/{asked['id']}", headers=admin)
    assert response.status_code == 200, response.text
    return response.json()


def _save(client: TestClient, who: Headers, read: Any, ids: list[str], against: Any) -> Any:  # noqa: ANN401
    return client.post(
        "/api/v1/lab-profiles",
        headers=who,
        json={"read": read["id"], "settings": ids, "against": against},
    )


def _current(client: TestClient, agent: Headers) -> list[Any]:
    response = client.get(
        "/api/v1/agent/lab-profiles", headers=agent, params={"maker": "Icom", "model": "IC-7300"}
    )
    assert response.status_code == 200, response.text
    profiles: list[Any] = response.json()["profiles"]
    return profiles


@pytest.fixture
def desk(admin: Headers, agent: Headers) -> Headers:
    """The administrator's headers, carrying fl-1's Agent's key for the helpers."""
    return admin | {"X-Agent": agent["Authorization"]}


def test_the_agent_reads_its_radio_when_an_administrator_asks(
    client: TestClient, admin: Headers, agent: Headers
) -> None:
    """The request waits for that station's Agent, which answers with what the radio has."""
    asked = _ask(client, admin)
    assert (asked["state"], asked["station"], asked["asked_by"]) == ("waiting", "fl-1", "K9KSU")
    waiting = client.get("/api/v1/agent/profile-reads", headers=agent)
    assert waiting.status_code == 200, waiting.text
    assert waiting.json()["reads"] == [{"id": asked["id"], "program": "VARA"}]

    _answer(client, agent, asked["id"])
    assert client.get("/api/v1/agent/profile-reads", headers=agent).json()["reads"] == []
    read = client.get(f"/api/v1/profile-reads/{asked['id']}", headers=admin).json()
    assert read["state"] == "read"
    assert (read["maker"], read["model"], read["serial"]) == tuple(RADIO.values())
    assert read["settings"] == _radio()
    assert read["profile"] == _built_in()
    assert read["changed"] == ["rf_power"]
    assert read["against"] is None


def test_one_press_makes_the_chosen_settings_the_default_profile(
    client: TestClient, desk: Headers, agent: Headers
) -> None:
    """Saving takes the radio's own values for the settings chosen, in the radio's order."""
    read = _read(client, desk)
    response = _save(client, desk, read, ["data_mod", "rf_power", "mode_and_filter"], None)
    assert response.status_code == 201, response.text
    saved = response.json()
    assert (saved["program"], saved["current"], saved["saved_by"]) == ("VARA", True, "K9KSU")
    assert (saved["station"], saved["serial"], saved["replaces"]) == ("fl-1", RADIO["serial"], None)
    assert [s["id"] for s in saved["settings"]] == ["mode_and_filter", "rf_power", "data_mod"]
    assert saved["settings"][1]["bytes"] == "00 69"

    current = _current(client, agent)
    assert [(p["id"], p["program"]) for p in current] == [(saved["id"], "VARA")]


def test_a_newer_version_replaces_the_profile_and_every_version_is_kept(
    client: TestClient, desk: Headers, agent: Headers, conn: psycopg.Connection[TupleRow]
) -> None:
    """The profile is the newest version; earlier ones stay, and none can be edited."""
    first = _save(client, desk, _read(client, desk), ["rf_power"], None).json()["id"]
    read = _read(client, desk, settings=_radio(rf_power="00 50"))
    assert read["against"] == first
    second = _save(client, desk, read, ["rf_power"], first).json()
    assert second["replaces"] == first
    assert _current(client, agent)[0]["settings"][0]["bytes"] == "00 50"

    listed = client.get("/api/v1/lab-profiles", headers=desk).json()
    assert [(p["id"], p["current"]) for p in listed] == [(second["id"], True), (first, False)]
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("UPDATE log.lab_profile SET note = 'x'")
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("DELETE FROM log.lab_profile")


def test_saving_against_a_profile_that_has_changed_is_refused(
    client: TestClient, desk: Headers, agent: Headers
) -> None:
    """Nobody replaces a profile they didn't see: the comparison must still hold."""
    read = _read(client, desk)
    first = _save(client, desk, read, ["rf_power"], None).json()["id"]
    response = _save(client, desk, read, ["data_mod"], None)
    assert response.status_code == 409, response.text
    assert "changed" in response.json()["detail"]
    assert [p["id"] for p in _current(client, agent)] == [first]


def test_programs_and_models_have_their_own_profiles(
    client: TestClient, desk: Headers, agent: Headers
) -> None:
    """A FT8 profile doesn't replace the VARA one, nor an IC-9700's the IC-7300's."""
    vara = _save(client, desk, _read(client, desk), ["rf_power"], None).json()["id"]
    ft8 = _save(client, desk, _read(client, desk, "FT8"), ["agc"], None).json()["id"]
    ic9700 = _read(client, desk, model="IC-9700", serial="IC-9700_1")
    assert ic9700["against"] is None
    assert _save(client, desk, ic9700, ["agc"], None).status_code == 201
    assert sorted((p["program"], p["id"]) for p in _current(client, agent)) == [
        ("FT8", ft8),
        ("VARA", vara),
    ]


def test_going_back_to_an_earlier_version_saves_it_again(
    client: TestClient, desk: Headers, agent: Headers
) -> None:
    """The earlier version becomes the newest, as a copy that says where it came from."""
    first = _save(client, desk, _read(client, desk), ["rf_power"], None).json()["id"]
    read = _read(client, desk, settings=_radio(rf_power="00 50"))
    second = _save(client, desk, read, ["rf_power"], first).json()["id"]
    stale = client.post(
        f"/api/v1/lab-profiles/{first}/again", headers=desk, json={"against": first}
    )
    assert stale.status_code == 409, stale.text
    response = client.post(
        f"/api/v1/lab-profiles/{first}/again", headers=desk, json={"against": second}
    )
    assert response.status_code == 201, response.text
    again = response.json()
    assert (again["again_of"], again["replaces"], again["station"]) == (first, second, None)
    assert _current(client, agent)[0]["settings"][0]["bytes"] == "00 69"
    missing = client.post("/api/v1/lab-profiles/999/again", headers=desk, json={"against": None})
    assert missing.status_code == 404


def test_a_profile_holds_only_settings_the_radio_was_read_for(
    client: TestClient, desk: Headers
) -> None:
    """The administrator chooses settings, never types a value."""
    read = _read(client, desk)
    response = _save(client, desk, read, ["rf_power", "nb_level"], None)
    assert response.status_code == 422, response.text
    assert _save(client, desk, read, [], None).status_code == 422


def test_a_radio_the_agent_could_not_read_says_why(
    client: TestClient, admin: Headers, agent: Headers
) -> None:
    """The Agent answers with the problem, and there is nothing to save."""
    asked = _ask(client, admin)
    response = client.post(
        f"/api/v1/agent/profile-reads/{asked['id']}",
        headers=agent,
        json={"problem": "the radio is transmitting for a program; try again when it's done"},
    )
    assert response.status_code == 204, response.text
    read = client.get(f"/api/v1/profile-reads/{asked['id']}", headers=admin).json()
    assert (read["state"], read["settings"]) == ("failed", [])
    assert "transmitting" in read["problem"]
    assert _save(client, admin, read, ["rf_power"], None).status_code == 409


def test_only_the_asked_stations_agent_answers_and_only_once(
    client: TestClient, admin: Headers, agent: Headers, other_agent: Headers
) -> None:
    """Another station's Agent neither sees the request nor answers it."""
    asked = _ask(client, admin)
    assert client.get("/api/v1/agent/profile-reads", headers=other_agent).json()["reads"] == []
    body = RADIO | {"settings": _radio(), "profile": [], "profile_from": "x", "changed": []}
    url = f"/api/v1/agent/profile-reads/{asked['id']}"
    assert client.post(url, headers=other_agent, json=body).status_code == 404
    assert client.post(url, headers=agent, json=body).status_code == 204
    assert client.post(url, headers=agent, json=body).status_code == 404
    assert client.post(url, json=body).status_code == 401


def test_only_an_administrator_asks_reads_or_saves(
    client: TestClient, desk: Headers, operator: Headers, agent: Headers
) -> None:
    """An operator without admin, or a station key, can't touch a lab profile."""
    read = _read(client, desk)
    for who in (operator, agent, {}):
        response = client.post(
            "/api/v1/profile-reads", headers=who, json={"station": "fl-1", "program": "VARA"}
        )
        assert response.status_code in {401, 403}
        assert client.get(f"/api/v1/profile-reads/{read['id']}", headers=who).status_code in {
            401,
            403,
        }
        assert _save(client, who, read, ["rf_power"], None).status_code in {401, 403}
        assert client.get("/api/v1/lab-profiles", headers=who).status_code in {401, 403}
    assert _current(client, agent) == []


def test_the_database_itself_refuses_a_profile_from_a_non_administrator(
    conn: psycopg.Connection[TupleRow], visitor: Actor
) -> None:
    """The rule holds whatever code asks, not only through the API."""
    read = RadioRead(
        maker="Icom",
        model="IC-7300",
        serial=RADIO["serial"],
        settings=(StockSetting("agc", "AGC", "01", None),),
        profile=(),
        profile_from="built into the Agent",
        changed=(),
        read_at=datetime.now(UTC),
    )
    store = LabProfileStore(conn)
    with pytest.raises(NotAuthorizedError):
        store.save(visitor, "fl-1", read, "VARA", ("agc",), against=None)
    with pytest.raises(psycopg.errors.InsufficientPrivilege):
        conn.execute(
            """
            INSERT INTO log.lab_profile (maker, model, program, settings, station_id, serial,
                saved_by, session_id)
            SELECT 'Icom', 'IC-7300', 'VARA', '[]', NULL, NULL, s.operator_id, s.id
            FROM log.session s WHERE s.id = %s
            """,
            (visitor.session_id,),
        )
    assert store.current("Icom", "IC-7300") == ()


@pytest.mark.parametrize(
    "change",
    [
        {"serial": "has space"},
        {"settings": []},
        {"settings": [{"id": "Bad-Id", "label": "x", "bytes": "00"}]},
        {"settings": [{"id": "rf_gain", "label": "RF gain", "bytes": "zz"}]},
        {"changed": ["Bad-Id"]},
        {"profile_from": "two\nlines"},
        {"extra": True},
    ],
)
def test_a_reading_that_is_not_valid_is_refused(
    client: TestClient, admin: Headers, agent: Headers, change: dict[str, Any]
) -> None:
    """Every field the Agent sends is checked at the door (Principle XII)."""
    asked = _ask(client, admin)
    body = (
        RADIO | {"settings": _radio(), "profile": [], "profile_from": "x", "changed": []} | change
    )
    response = client.post(f"/api/v1/agent/profile-reads/{asked['id']}", headers=agent, json=body)
    assert response.status_code == 422, response.text


def test_a_profile_is_for_one_of_the_programs_the_agent_knows(
    client: TestClient, admin: Headers
) -> None:
    """Only VARA, FT8, JS8, fldigi and SSTV have profiles."""
    response = client.post(
        "/api/v1/profile-reads", headers=admin, json={"station": "fl-1", "program": "Winlink"}
    )
    assert response.status_code == 422, response.text
