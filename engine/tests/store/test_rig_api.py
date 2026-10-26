# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Each station's radio, as its Station Agent reports it (Principles IX, X, XXI)."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.pairing import LIFETIME, MAX_WAITING, PairingDesk
from loglab_engine.rig import STALE, RigBoard, RigState, adif_mode, band_for
from loglab_engine.store import IdentityStore, NotAuthorizedError, Permission
from loglab_engine.store.agent import AgentKeyStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

IC7300: dict[str, Any] = {
    "maker": "Icom",
    "model": "IC-7300",
    "path": {"kind": "hamlib", "model": 3073},
    "frequency_hz": 14_205_000,
    "mode": "USB",
    "passband_hz": 2400,
    "ptt": False,
    "vfo": "VFOA",
}


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
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: an administrator at the first Field Day position."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log only, at the second position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> dict[str, str]:
    """The headers of the Agent at fd-1, with a key K9KSU made for it."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fd-1')}"}


def _report(radio: dict[str, Any] | None = IC7300, problem: str | None = None) -> dict[str, Any]:
    return {"agent": "loglab-agent 0.1.0", "radio": radio, "problem": problem}


def test_the_agent_reports_and_its_station_sees_the_radio(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """The radio at fd-1 shows at fd-1, with its band and the mode a QSO records."""
    assert client.get("/api/v1/rig", headers=k9ksu).json()["heard"] is False
    response = client.put("/api/v1/agent/rig", headers=agent, json=_report())
    assert response.status_code == 204, response.text
    rig = client.get("/api/v1/rig", headers=k9ksu).json()
    assert rig["station"] == "fd-1"
    assert rig["heard"] is True
    assert rig["radio"]["maker"] == "Icom"
    assert rig["radio"]["model"] == "IC-7300"
    assert rig["radio"]["path"] == "Hamlib model 3073"
    assert rig["radio"]["mhz"] == "14.205000"
    assert rig["radio"]["band"] == "20m"
    assert (rig["radio"]["adif_mode"], rig["radio"]["adif_submode"]) == ("SSB", "USB")
    assert rig["radio"]["transmitting"] is False
    # Another station has its own radio, or none (Principle IX).
    other = client.get("/api/v1/rig", headers=w0xyz).json()
    assert (other["station"], other["heard"], other["radio"]) == ("fd-2", False, None)


def test_a_flexradio_reports_through_its_api(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """A FlexRadio's slice shows like any other radio, named by its own model (Principle IX)."""
    flex = {
        "maker": "FlexRadio",
        "model": "FLEX-8400",
        "path": {"kind": "flex"},
        "frequency_hz": 14_074_000,
        "mode": "DATA-USB",
        "passband_hz": 3000,
        "ptt": True,
        "vfo": "Slice A",
    }
    response = client.put("/api/v1/agent/rig", headers=agent, json=_report(flex))
    assert response.status_code == 204, response.text
    radio = client.get("/api/v1/rig", headers=k9ksu).json()["radio"]
    assert (radio["maker"], radio["model"], radio["path"]) == (
        "FlexRadio",
        "FLEX-8400",
        "FlexRadio API",
    )
    assert (radio["band"], radio["transmitting"], radio["vfo"]) == ("20m", True, "Slice A")


def test_an_icom_radio_reports_through_the_agents_own_ci_v_driver(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """A radio the Agent reads over CI-V itself shows like any other radio (Principle IX)."""
    civ = {**IC7300, "path": {"kind": "civ"}, "mode": "DATA-USB", "passband_hz": None}
    response = client.put("/api/v1/agent/rig", headers=agent, json=_report(civ))
    assert response.status_code == 204, response.text
    radio = client.get("/api/v1/rig", headers=k9ksu).json()["radio"]
    assert (radio["model"], radio["path"]) == ("IC-7300", "Icom CI-V")


def test_a_radio_the_agent_cannot_read_says_why(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """The Agent says what is wrong, and the station sees it."""
    body = _report(None, "the radio did not answer in time")
    assert client.put("/api/v1/agent/rig", headers=agent, json=body).status_code == 204
    rig = client.get("/api/v1/rig", headers=k9ksu).json()
    assert rig["heard"] is True
    assert rig["radio"] is None
    assert rig["problem"] == "the radio did not answer in time"


def test_only_a_working_station_key_is_heard(
    client: TestClient, conn: psycopg.Connection[TupleRow], shack: Actor, k9ksu: dict[str, str]
) -> None:
    """No key, a session token, or a replaced or ended key is refused (Principle XXI)."""
    keys = AgentKeyStore(conn)
    assert client.put("/api/v1/agent/rig", json=_report()).status_code == 401
    # An operator's session is not a station key.
    assert client.put("/api/v1/agent/rig", headers=k9ksu, json=_report()).status_code == 401
    first = keys.make(shack, "fd-1")
    second = keys.make(shack, "fd-1")
    old = {"Authorization": f"Bearer {first}"}
    new = {"Authorization": f"Bearer {second}"}
    assert client.put("/api/v1/agent/rig", headers=old, json=_report()).status_code == 401
    assert client.put("/api/v1/agent/rig", headers=new, json=_report()).status_code == 204
    assert keys.stations() == ("fd-1",)
    assert keys.end(shack, "fd-1") is True
    assert keys.end(shack, "fd-1") is False
    assert client.put("/api/v1/agent/rig", headers=new, json=_report()).status_code == 401
    assert keys.stations() == ()


def test_only_an_administrator_makes_station_keys(
    conn: psycopg.Connection[TupleRow], identity: IdentityStore, shack: Actor
) -> None:
    """Logging permission is not enough, and the database refuses it too."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    visitor = identity.sign_in("W0XYZ", VISITOR_SECRET, "fd-2").actor
    keys = AgentKeyStore(conn)
    with pytest.raises(NotAuthorizedError):
        keys.make(visitor, "fd-2")
    keys.make(shack, "fd-2")
    with pytest.raises(NotAuthorizedError):
        keys.end(visitor, "fd-2")
    assert keys.stations() == ("fd-2",)
    # Kept forever (Principle VII).
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("DELETE FROM log.agent_key")


@pytest.mark.parametrize(
    "change",
    [
        {"frequency_hz": 0},
        {"frequency_hz": 300_000_000_001},
        {"frequency_hz": "14205000"},
        {"mode": "FT8"},
        {"maker": "Icom\x1b[31m"},
        {"model": "x" * 41},
        {"passband_hz": 0},
        {"path": {"kind": "serial"}},
        {"tune_to": 7_074_000},
    ],
)
def test_a_report_is_checked_before_it_is_believed(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str], change: dict[str, Any]
) -> None:
    """Nothing out of range, of the wrong type, or unexpected gets in (Principle XII)."""
    response = client.put("/api/v1/agent/rig", headers=agent, json=_report({**IC7300, **change}))
    assert response.status_code == 422, response.text
    assert client.get("/api/v1/rig", headers=k9ksu).json()["heard"] is False


def test_the_stream_sends_the_radio_live(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """One JSON line per change, for a signed-in viewer."""
    assert client.get("/api/v1/rig/stream").status_code == 401
    client.put("/api/v1/agent/rig", headers=agent, json=_report())
    with client.stream("GET", "/api/v1/rig/stream?max_seconds=1", headers=k9ksu) as response:
        assert response.status_code == 200
        assert response.headers["content-type"].startswith("application/x-ndjson")
        lines = [json.loads(line) for line in response.iter_lines() if line]
    assert lines
    assert lines[0]["radio"]["frequency_hz"] == 14_205_000


def test_on_air_takes_the_band_and_mode_from_the_radio(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """What the radio reports outranks what was typed (Principle X), except a data mode's name."""
    body = {
        "overlay": "arrl-fd",
        "name": "Field Day 2027",
        "station_call": "W0CK",
        "sent": {"class": "3A", "section": "KS"},
    }
    op = client.post("/api/v1/operations", headers=k9ksu, json=body).json()
    url = f"/api/v1/operations/{op['id']}/on-air"
    client.put(url, headers=k9ksu, json={"band": "40m", "mode": "CW"})
    client.put(url, headers=w0xyz, json={"band": "20m", "mode": "SSB"})
    client.put("/api/v1/agent/rig", headers=agent, json=_report())
    stations = {s["station"]: s for s in client.get(url, headers=w0xyz).json()["stations"]}
    assert stations["fd-1"]["from_radio"] is True
    assert (stations["fd-1"]["band"], stations["fd-1"]["mode"]) == ("20m", "USB")
    assert stations["fd-1"]["frequency_hz"] == 14_205_000
    assert stations["fd-1"]["transmitting"] is False
    assert stations["fd-2"]["from_radio"] is False
    # Both are now on 20 m phone as W0CK.
    assert stations["fd-1"]["clash"] is True
    assert stations["fd-2"]["clash"] is True

    data = {**IC7300, "frequency_hz": 7_074_000, "mode": "DATA-USB", "ptt": True}
    client.put(url, headers=k9ksu, json={"band": "40m", "mode": "FT8"})
    client.put("/api/v1/agent/rig", headers=agent, json=_report(data))
    stations = {s["station"]: s for s in client.get(url, headers=w0xyz).json()["stations"]}
    assert (stations["fd-1"]["band"], stations["fd-1"]["mode"]) == ("40m", "FT8")
    assert stations["fd-1"]["transmitting"] is True
    assert not stations["fd-1"]["clash"]


def test_a_silent_agent_is_forgotten() -> None:
    """A report older than STALE no longer counts as the radio's state."""
    board = RigBoard()
    heard = datetime(2027, 6, 26, 18, 0, tzinfo=UTC)
    board.report(RigState("fd-1", "loglab-agent 0.1.0", None, "off", heard))
    assert board.current("fd-1", heard + STALE) is not None
    assert board.current("fd-1", heard + STALE + timedelta(seconds=1)) is None
    first = board.version("fd-1")
    board.report(RigState("fd-1", "loglab-agent 0.1.0", None, "off", heard + STALE))
    assert board.version("fd-1") == first  # nothing changed but the time
    board.report(RigState("fd-1", "loglab-agent 0.1.0", None, "unplugged", heard))
    assert board.version("fd-1") > first


@pytest.mark.parametrize(
    ("hz", "band"),
    [
        (1_800_000, "160m"),
        (3_573_000, "80m"),
        (14_350_000, "20m"),
        (14_350_001, None),
        (50_313_000, "6m"),
        (146_520_000, "2m"),
        (446_000_000, "70cm"),
        (10_368_100_000, "3cm"),
        (12_000_000, None),
    ],
)
def test_bands_follow_the_adif_band_table(hz: int, band: str | None) -> None:
    """A frequency outside every band has no band."""
    assert band_for(hz) == band


def test_rig_modes_become_adif_modes() -> None:
    """Data modes are left for the operator to name."""
    assert adif_mode("LSB") == ("SSB", "LSB")
    assert adif_mode("CW-R") == ("CW", None)
    assert adif_mode("DATA-USB") is None
    assert adif_mode(None) is None


def _ask(client: TestClient, station: str = "fd-1") -> dict[str, Any]:
    response = client.post(
        "/api/v1/agent/pairings", json={"station": station, "agent": "loglab-agent 0.1.0"}
    )
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()
    return out


def _state(client: TestClient, asked: dict[str, Any], ticket: str | None = None) -> dict[str, Any]:
    headers = {"Authorization": f"Bearer {ticket or asked['ticket']}"}
    out: dict[str, Any] = client.get(
        f"/api/v1/agent/pairings/{asked['id']}", headers=headers
    ).json()
    return out


def test_an_agent_joins_when_an_administrator_approves(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """The Agent shows a code, the administrator sees the same one, and the key comes once."""
    asked = _ask(client)
    assert len(asked["code"]) == 6
    assert _state(client, asked) == {"state": "waiting", "key": None}
    # Only an administrator sees the requests and decides them (Principle XXI).
    assert client.get("/api/v1/agent/pairings", headers=w0xyz).status_code == 403
    url = f"/api/v1/agent/pairings/{asked['id']}"
    assert client.post(f"{url}/approve", headers=w0xyz).status_code == 403
    waiting = client.get("/api/v1/agent/pairings", headers=k9ksu).json()
    assert [(p["station"], p["code"], p["replaces"]) for p in waiting] == [
        ("fd-1", asked["code"], False)
    ]
    assert client.post(f"{url}/approve", headers=k9ksu).status_code == 204
    assert client.get("/api/v1/agent/pairings", headers=k9ksu).json() == []
    # A wrong ticket learns nothing, and the key is not given away.
    assert _state(client, asked, "not-the-ticket") == {"state": "expired", "key": None}
    got = _state(client, asked)
    assert got["state"] == "approved"
    key = got["key"]
    assert key.startswith("lls_")
    assert _state(client, asked) == {"state": "expired", "key": None}  # handed over once
    headers = {"Authorization": f"Bearer {key}"}
    assert client.put("/api/v1/agent/rig", headers=headers, json=_report()).status_code == 204
    assert client.get("/api/v1/rig", headers=k9ksu).json()["radio"]["model"] == "IC-7300"
    assert client.get("/api/v1/agent/keys", headers=k9ksu).json() == {"stations": ["fd-1"]}
    # Ending the key from the web client refuses the Agent.
    assert client.delete("/api/v1/agent/keys/fd-1", headers=k9ksu).status_code == 204
    assert client.put("/api/v1/agent/rig", headers=headers, json=_report()).status_code == 401


def test_a_refused_or_repeated_request_gets_no_key(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """Refusing gives nothing; asking again replaces the old request; the old key keeps working."""
    first = _ask(client)
    second = _ask(client)
    assert _state(client, first)["state"] == "expired"
    waiting = client.get("/api/v1/agent/pairings", headers=k9ksu).json()
    assert [(p["id"], p["replaces"]) for p in waiting] == [(second["id"], True)]
    url = f"/api/v1/agent/pairings/{second['id']}"
    assert client.post(f"{url}/refuse", headers=k9ksu).status_code == 204
    assert client.post(f"{url}/approve", headers=k9ksu).status_code == 409
    assert _state(client, second) == {"state": "refused", "key": None}
    assert client.put("/api/v1/agent/rig", headers=agent, json=_report()).status_code == 204


def test_a_request_is_checked_and_limited(client: TestClient) -> None:
    """A bad station name is refused, and only so many requests may wait (Principle XII)."""
    for bad in ["", "has space", "x" * 65, "-leading"]:
        body = {"station": bad, "agent": "loglab-agent 0.1.0"}
        assert client.post("/api/v1/agent/pairings", json=body).status_code == 422
    for n in range(MAX_WAITING):
        _ask(client, f"pos-{n}")
    response = client.post(
        "/api/v1/agent/pairings", json={"station": "one-more", "agent": "loglab-agent 0.1.0"}
    )
    assert response.status_code == 429


def test_requests_expire() -> None:
    """A request not decided within LIFETIME is gone."""
    desk = PairingDesk()
    now = datetime(2027, 6, 26, 18, 0, tzinfo=UTC)
    pairing, ticket = desk.ask("fd-1", "loglab-agent 0.1.0", now)
    assert desk.waiting(now + LIFETIME) == (pairing,)
    assert desk.waiting(now + LIFETIME + timedelta(seconds=1)) == ()
    assert desk.collect(pairing.id, ticket, now + LIFETIME + timedelta(seconds=1)) == (
        "expired",
        None,
    )
