# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The zone maps' API: how many QSOs the log has in each CQ or ITU zone."""

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


def _log(client: TestClient, headers: dict[str, str], call: str, **more: str) -> str:
    fields = [
        {"name": "CALL", "value": call},
        {"name": "QSO_DATE", "value": "20261018"},
        {"name": "TIME_ON", "value": "0100"},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": "CW"},
        *({"name": name, "value": value} for name, value in more.items()),
    ]
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    qso_id: str = response.json()["qso"]["id"]
    return qso_id


def _zones(client: TestClient, headers: dict[str, str], **params: str) -> dict[str, object]:
    response = client.get("/api/v1/zones", headers=headers, params=params)
    assert response.status_code == 200, response.text
    body: dict[str, object] = response.json()
    return body


def _counts(body: dict[str, object]) -> dict[int, int]:
    zones = body["zones"]
    assert isinstance(zones, list)
    return {z["zone"]: z["qsos"] for z in zones if z["qsos"]}


def test_cq_zones(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Every zone is listed; QSOs count in the zone their CQZ field names."""
    _log(client, k9ksu, "G4ABC", CQZ="14", ITUZ="27")
    _log(client, k9ksu, "M0XYZ", CQZ="14")
    _log(client, k9ksu, "JA1XYZ", CQZ="25")
    _log(client, k9ksu, "W1AW")
    _log(client, k9ksu, "K1ABC", CQZ="77")
    body = _zones(client, k9ksu, kind="cq")
    assert body["kind"] == "cq"
    assert body["field"] == "CQZ"
    assert len(body["zones"]) == 40  # type: ignore[arg-type]
    assert _counts(body) == {14: 2, 25: 1}
    assert body["worked"] == 2
    assert body["unrecognized"] == [{"value": "77", "qsos": 1}]
    assert body["without_zone"] == 1


def test_itu_zones(client: TestClient, k9ksu: dict[str, str]) -> None:
    """ITU zones come from the ITUZ field, and there are 90 of them."""
    _log(client, k9ksu, "G4ABC", CQZ="14", ITUZ="27")
    _log(client, k9ksu, "DA0HQ", ITUZ="DARC")
    body = _zones(client, k9ksu, kind="itu")
    assert body["field"] == "ITUZ"
    assert len(body["zones"]) == 90  # type: ignore[arg-type]
    assert _counts(body) == {27: 1}
    assert body["unrecognized"] == [{"value": "DARC", "qsos": 1}]


def test_a_deleted_qso_does_not_count(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Deleted QSOs stay in the log's history but not on the map (Principle VII)."""
    qso_id = _log(client, k9ksu, "G4ABC", CQZ="14")
    deleted = client.post(f"/api/v1/qsos/{qso_id}/delete", headers=k9ksu, json={})
    assert deleted.status_code == 200, deleted.text
    assert _counts(_zones(client, k9ksu, kind="cq")) == {}


@pytest.mark.parametrize("params", [{}, {"kind": "waz"}, {"kind": "cq", "operation": "x"}])
def test_a_bad_question_is_refused(
    client: TestClient, k9ksu: dict[str, str], params: dict[str, str]
) -> None:
    """The kind of zone is required, and must be cq or itu."""
    response = client.get("/api/v1/zones", headers=k9ksu, params=params)
    assert response.status_code == 422


def test_reading_zones_needs_view(
    client: TestClient,
    identity: IdentityStore,
    shack: Actor,
) -> None:
    """Signed in, and allowed to read the log (Principle XXI)."""
    assert client.get("/api/v1/zones", params={"kind": "cq"}).status_code == 401
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.LOG])
    headers = _sign_in(client, "W0XYZ", VISITOR_SECRET)
    assert client.get("/api/v1/zones", headers=headers, params={"kind": "cq"}).status_code == 403


def test_the_zone_maps_ship_with_the_engine(client: TestClient) -> None:
    """They draw with no internet connection (Principle V)."""
    for path in ("zones.js", "zones.css", "maps/cq-zones.json", "maps/itu-zones.json"):
        assert client.get(f"/static/{path}").status_code == 200, path
