# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The section map's API: which ARRL and RAC sections the log has worked."""

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
        json={"callsign": callsign, "secret": secret, "station": "fd-1"},
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the first administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET)


def _log(client: TestClient, headers: dict[str, str], call: str, section: str | None) -> str:
    fields = [
        {"name": "CALL", "value": call},
        {"name": "QSO_DATE", "value": "20260627"},
        {"name": "TIME_ON", "value": "1805"},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": "SSB"},
        {"name": "CLASS", "value": "3A"},
    ]
    if section is not None:
        fields.append({"name": "arrl_sect", "value": section})
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    qso_id: str = response.json()["qso"]["id"]
    return qso_id


def _sections(client: TestClient, headers: dict[str, str]) -> dict[str, object]:
    response = client.get("/api/v1/sections", headers=headers)
    assert response.status_code == 200, response.text
    body: dict[str, object] = response.json()
    return body


def _qsos(body: dict[str, object]) -> dict[str, int]:
    rows = body["sections"]
    assert isinstance(rows, list)
    return {row["code"]: row["qsos"] for row in rows}


def test_an_empty_log_has_worked_no_sections(client: TestClient, k9ksu: dict[str, str]) -> None:
    """All 85 sections are listed, none worked."""
    body = _sections(client, k9ksu)
    assert len(_qsos(body)) == 85
    assert set(_qsos(body).values()) == {0}
    assert body["worked"] == 0
    assert body["without_section"] == 0


def test_sections_in_the_log_are_worked(client: TestClient, k9ksu: dict[str, str]) -> None:
    """ARRL_SECT decides the section, in any case; others are reported, not dropped."""
    _log(client, k9ksu, "W0AAA", "KS")
    _log(client, k9ksu, "K0BBB", "ks")
    _log(client, k9ksu, "VE3CCC", "ONE")
    _log(client, k9ksu, "VE1DDD", "MAR")
    _log(client, k9ksu, "N0OY", None)
    body = _sections(client, k9ksu)
    worked = {code: n for code, n in _qsos(body).items() if n}
    assert worked == {"KS": 2, "ONE": 1}
    assert body["worked"] == 2
    assert body["unrecognized"] == [{"value": "MAR", "qsos": 1}]
    assert body["without_section"] == 1


def test_a_deleted_qso_no_longer_counts(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Deleting the only QSO with a section uncolours it; restoring colours it again."""
    qso_id = _log(client, k9ksu, "W1AW", "CT")
    assert _qsos(_sections(client, k9ksu))["CT"] == 1
    deleted = client.post(f"/api/v1/qsos/{qso_id}/delete", headers=k9ksu, json={})
    assert deleted.status_code == 200, deleted.text
    assert _qsos(_sections(client, k9ksu))["CT"] == 0
    restored = client.post(f"/api/v1/qsos/{qso_id}/restore", headers=k9ksu, json={})
    assert restored.status_code == 200, restored.text
    assert _qsos(_sections(client, k9ksu))["CT"] == 1


def test_an_edit_moves_the_qso_to_its_new_section(
    client: TestClient,
    k9ksu: dict[str, str],
) -> None:
    """Only the QSO's current version counts (Principle VII keeps the old one)."""
    qso_id = _log(client, k9ksu, "W0EEE", "MO")
    current = client.get(f"/api/v1/qsos/{qso_id}", headers=k9ksu).json()
    fields = [f if f["name"] != "arrl_sect" else {**f, "value": "IA"} for f in current["fields"]]
    edited = client.put(
        f"/api/v1/qsos/{qso_id}",
        headers=k9ksu,
        json={"base_version": current["version"], "fields": fields},
    )
    assert edited.status_code == 200, edited.text
    counts = _qsos(_sections(client, k9ksu))
    assert counts["MO"] == 0
    assert counts["IA"] == 1


def test_reading_sections_needs_view(
    client: TestClient,
    identity: IdentityStore,
    shack: Actor,
) -> None:
    """Signed in, and allowed to read the log (Principle XXI)."""
    assert client.get("/api/v1/sections").status_code == 401
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.LOG])
    headers = _sign_in(client, "W0XYZ", VISITOR_SECRET)
    assert client.get("/api/v1/sections", headers=headers).status_code == 403


def test_the_map_ships_with_the_engine(client: TestClient) -> None:
    """The page, its script and the map's shapes come from the Engine (Principle V)."""
    assert client.get("/static/map.js").status_code == 200
    assert client.get("/static/map.css").status_code == 200
    assert client.get("/static/maps/arrl-sections.json").status_code == 200
