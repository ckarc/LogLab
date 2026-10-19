# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The world map's API: the latest QSOs, each from the station to the other station."""

from __future__ import annotations

from typing import TYPE_CHECKING

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


def _sign_in(client: TestClient, callsign: str, secret: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions",
        json={"callsign": callsign, "secret": secret, "station": "shack"},
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the first administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET)


def _log(client: TestClient, headers: dict[str, str], call: str, time: str, **more: str) -> str:
    fields = [
        {"name": "CALL", "value": call},
        {"name": "QSO_DATE", "value": "20261018"},
        {"name": "TIME_ON", "value": time},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": "SSB"},
        *({"name": name, "value": value} for name, value in more.items()),
    ]
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    qso_id: str = response.json()["qso"]["id"]
    return qso_id


def _paths(client: TestClient, headers: dict[str, str], **params: str) -> dict[str, object]:
    response = client.get("/api/v1/paths", headers=headers, params=params)
    assert response.status_code == 200, response.text
    body: dict[str, object] = response.json()
    return body


def test_an_empty_log_draws_nothing(client: TestClient, k9ksu: dict[str, str]) -> None:
    """No QSOs, and no station location yet."""
    assert _paths(client, k9ksu) == {
        "home": None,
        "paths": [],
        "without_place": 0,
        "without_home": 0,
    }


def test_the_latest_qsos_by_time_with_places(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Newest QSO time first; QSOs without a place for either end are counted, not drawn."""
    _log(client, k9ksu, "G4ABC", "0100", GRIDSQUARE="IO91wm", MY_GRIDSQUARE="EM28")
    _log(client, k9ksu, "JA1XYZ", "0300", GRIDSQUARE="PM95", MY_GRIDSQUARE="EM29")
    _log(client, k9ksu, "W1AW", "0200")  # no grid
    _log(client, k9ksu, "VK2DX", "0400", GRIDSQUARE="QF56")  # the station's grid unknown
    body = _paths(client, k9ksu)
    paths = body["paths"]
    assert isinstance(paths, list)
    assert [p["call"] for p in paths] == ["JA1XYZ", "G4ABC"]
    assert paths[0]["station"] == {"lat": 39.5, "lon": -95.0}
    assert paths[0]["other"] == {"lat": 35.5, "lon": 139.0}
    assert 10000 < paths[0]["km"] < 11000
    assert body["home"] == {"lat": 39.5, "lon": -95.0}
    assert body["without_place"] == 1
    assert body["without_home"] == 1
    # Only the latest two QSOs: VK2DX (no home) and JA1XYZ.
    two = _paths(client, k9ksu, limit="2")
    two_paths = two["paths"]
    assert isinstance(two_paths, list)
    assert [p["call"] for p in two_paths] == ["JA1XYZ"]
    assert two["without_home"] == 1


def test_a_deleted_qso_is_not_drawn(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Only current QSOs count (Principle VII)."""
    qso_id = _log(client, k9ksu, "G4ABC", "0100", GRIDSQUARE="IO91", MY_GRIDSQUARE="EM28")
    deleted = client.post(f"/api/v1/qsos/{qso_id}/delete", headers=k9ksu, json={})
    assert deleted.status_code == 200, deleted.text
    assert _paths(client, k9ksu)["paths"] == []


@pytest.mark.parametrize("limit", ["0", "501", "many"])
def test_a_bad_limit_is_refused(client: TestClient, k9ksu: dict[str, str], limit: str) -> None:
    """Checked at the boundary (Principle XII)."""
    response = client.get("/api/v1/paths", headers=k9ksu, params={"limit": limit})
    assert response.status_code == 422


def test_reading_paths_needs_view(
    client: TestClient,
    identity: IdentityStore,
    shack: Actor,
) -> None:
    """Signed in, and allowed to read the log (Principle XIII)."""
    assert client.get("/api/v1/paths").status_code == 401
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.LOG])
    headers = _sign_in(client, "W0XYZ", VISITOR_SECRET)
    assert client.get("/api/v1/paths", headers=headers).status_code == 403


def test_the_world_map_ships_with_the_engine(client: TestClient) -> None:
    """Its script, style and shapes come from the Engine (Principle V)."""
    assert client.get("/static/world.js").status_code == 200
    assert client.get("/static/world.css").status_code == 200
    assert client.get("/static/maps/world.json").status_code == 200
