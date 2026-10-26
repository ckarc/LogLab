# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Operation stats and On air boxes' answers: the same for every workstation (II)."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, Permission

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from loglab_engine.store import Actor


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
    """K9KSU's headers: an administrator at the first position."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log only, at the second position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


@pytest.fixture
def field_day(client: TestClient, k9ksu: dict[str, str]) -> dict[str, Any]:
    """The club's Field Day, open from now on, with a GOTA station."""
    body = {
        "overlay": "arrl-fd",
        "name": "Field Day 2027",
        "station_call": "W0CK",
        "gota_call": "K0GTA",
        "sent": {"class": "3A", "section": "KS"},
    }
    response = client.post("/api/v1/operations", headers=k9ksu, json=body)
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()
    return out


def _log(
    client: TestClient,
    headers: dict[str, str],
    op: dict[str, Any],
    call: str,
    mode: str = "CW",
) -> None:
    now = datetime.now(UTC)
    fields = [
        {"name": "CALL", "value": call},
        {"name": "QSO_DATE", "value": now.strftime("%Y%m%d")},
        {"name": "TIME_ON", "value": now.strftime("%H%M%S")},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": mode},
        {"name": "CLASS", "value": "2A"},
        {"name": "ARRL_SECT", "value": "CT"},
    ]
    body = {"fields": fields, "operation": {"id": op["id"]}}
    response = client.post("/api/v1/qsos", headers=headers, json=body)
    assert response.status_code == 201, response.text


def test_every_workstation_sees_the_same_stats(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Two operators' QSOs add up to one score, shown alike at both positions."""
    _log(client, k9ksu, field_day, "W1AW")
    _log(client, w0xyz, field_day, "K1ABC", "SSB")
    _log(client, w0xyz, field_day, "K1ABC", "SSB")
    url = f"/api/v1/operations/{field_day['id']}/stats"
    mine = client.get(url, headers=k9ksu).json()
    theirs = client.get(url, headers=w0xyz).json()
    for key in ("qsos", "counted", "dupes", "score", "operators", "by_band_mode"):
        assert mine[key] == theirs[key]
    assert (mine["qsos"], mine["counted"], mine["dupes"], mine["score"]) == (3, 2, 1, 3)
    assert mine["last_hour"] == 3
    assert [o["operator"] for o in mine["operators"]] == ["W0XYZ", "K9KSU"]
    assert mine["sections_worked"] == 1
    assert "score" in mine["show"]


def test_stats_need_a_sign_in(client: TestClient, field_day: dict[str, Any]) -> None:
    """Nobody reads the log without signing in (Principle XXI)."""
    assert client.get(f"/api/v1/operations/{field_day['id']}/stats").status_code == 401


def test_on_air_lists_each_workstation_with_its_band_and_mode(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Each position says what it is set to; everyone sees the whole list."""
    url = f"/api/v1/operations/{field_day['id']}/on-air"
    assert client.put(url, headers=k9ksu, json={"band": "20m", "mode": "CW"}).status_code == 200
    _log(client, k9ksu, field_day, "W1AW")
    response = client.put(url, headers=w0xyz, json={"band": "40m", "mode": "SSB", "gota": True})
    assert response.status_code == 200, response.text
    stations = {s["operator"]: s for s in client.get(url, headers=w0xyz).json()["stations"]}
    assert stations["K9KSU"]["band"] == "20m"
    assert stations["K9KSU"]["call"] == "W0CK"
    assert stations["K9KSU"]["active"] is True
    assert stations["W0XYZ"]["call"] == "K0GTA"
    assert stations["W0XYZ"]["active"] is False
    assert stations["W0XYZ"]["you"] is True
    assert not any(s["clash"] for s in stations.values())


def test_two_stations_on_one_band_and_mode_are_flagged(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Field Day allows one transmitter per band and mode; SSB and FM are both Phone."""
    url = f"/api/v1/operations/{field_day['id']}/on-air"
    client.put(url, headers=k9ksu, json={"band": "20m", "mode": "SSB"})
    client.put(url, headers=w0xyz, json={"band": "20m", "mode": "FM"})
    stations = client.get(url, headers=k9ksu).json()["stations"]
    assert [s["clash"] for s in stations] == [True, True]
    assert {s["mode_group"] for s in stations} == {"Phone"}


def test_signing_out_leaves_the_list(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """A workstation that signs out is no longer on the air."""
    url = f"/api/v1/operations/{field_day['id']}/on-air"
    client.put(url, headers=w0xyz, json={"band": "20m", "mode": "CW"})
    assert client.delete("/api/v1/sessions/current", headers=w0xyz).status_code == 204
    assert client.get(url, headers=k9ksu).json()["stations"] == []


def test_a_bad_band_is_refused(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """What a browser reports is parsed like any other input (Principle XII)."""
    url = f"/api/v1/operations/{field_day['id']}/on-air"
    assert client.put(url, headers=k9ksu, json={"band": "<b>", "mode": "CW"}).status_code == 422


def test_signing_in_again_at_a_station_replaces_its_line(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """One line per operator per station, however often the browser signs in."""
    url = f"/api/v1/operations/{field_day['id']}/on-air"
    client.put(url, headers=k9ksu, json={"band": "20m", "mode": "CW"})
    again = _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")
    client.put(url, headers=again, json={"band": "40m", "mode": "CW"})
    stations = client.get(url, headers=k9ksu).json()["stations"]
    assert [(s["station"], s["band"]) for s in stations] == [("fd-1", "40m")]


def _log_everyday(client: TestClient, headers: dict[str, str], call: str, **more: str) -> str:
    now = datetime.now(UTC)
    fields = [
        {"name": "CALL", "value": call},
        {"name": "QSO_DATE", "value": now.strftime("%Y%m%d")},
        {"name": "TIME_ON", "value": now.strftime("%H%M%S")},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": "SSB"},
        *({"name": k, "value": v} for k, v in more.items()),
    ]
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    qso_id: str = response.json()["qso"]["id"]
    return qso_id


def test_personal_stats_are_each_operators_own(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """With no operation, each operator sees only their own QSOs; deleted ones don't count."""
    _log_everyday(client, k9ksu, "W1AW", STATE="CT", DXCC="291")
    _log_everyday(client, k9ksu, "VE3XYZ", STATE="ON", DXCC="1")
    gone = _log_everyday(client, k9ksu, "DL1ABC", COUNTRY="GERMANY")
    _log_everyday(client, w0xyz, "K1ABC", STATE="MA")
    response = client.post(f"/api/v1/qsos/{gone}/delete", headers=k9ksu, json={})
    assert response.status_code == 200, response.text
    mine = client.get("/api/v1/stats/personal", headers=k9ksu).json()
    assert mine["operator"] == "K9KSU"
    assert (mine["today"], mine["week"], mine["year"], mine["total"]) == (2, 2, 2, 2)
    assert mine["states"] == ["CT"]
    assert [c["name"] for c in mine["countries"]] == ["Canada", "United States of America"]
    assert mine["by_band_mode"] == [{"band": "20m", "mode": "SSB", "qsos": 2}]
    theirs = client.get("/api/v1/stats/personal", headers=w0xyz).json()
    assert (theirs["total"], theirs["states"], theirs["no_country"]) == (1, ["MA"], 1)


def test_personal_stats_need_a_sign_in(client: TestClient) -> None:
    """Nobody reads the log without signing in (Principle XXI)."""
    assert client.get("/api/v1/stats/personal").status_code == 401
