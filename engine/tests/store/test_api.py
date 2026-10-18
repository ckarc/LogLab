# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Engine's HTTP API: signed in (XIII), validated (XII), audited (VII), UTC (VIII)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, Permission

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from loglab_engine.store import Actor

W1AW = [
    {"name": "CALL", "value": "W1AW"},
    {"name": "QSO_DATE", "value": "20261018"},
    {"name": "TIME_ON", "value": "0136"},
    {"name": "BAND", "value": "20m"},
    {"name": "MODE", "value": "SSB"},
    {"name": "RST_RCVD", "value": "57"},
]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


def _sign_in(client: TestClient, callsign: str, secret: str, station: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions",
        json={"callsign": callsign, "secret": secret, "station": station},
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the first administrator, signed in through the API."""
    del shack  # enrolled K9KSU
    return _sign_in(client, "k9ksu", ADMIN_SECRET, "shack")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log, at a Field Day position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def test_status_names_the_source_without_signing_in(client: TestClient) -> None:
    """Anyone on the network can find the source (AGPL-3.0, Licensing)."""
    response = client.get("/api/v1/status")
    assert response.status_code == 200
    assert response.json()["source"] == "https://github.com/ckarc/loglab"
    assert response.json()["license"] == "AGPL-3.0-only"


def test_the_web_client_is_served_with_a_strict_policy(client: TestClient) -> None:
    """The page and its files load only from the Engine itself (Principle V)."""
    page = client.get("/")
    assert page.status_code == 200
    assert "<title>LogLab</title>" in page.text
    assert "default-src 'self'" in page.headers["content-security-policy"]
    assert client.get("/static/app.js").status_code == 200
    assert client.get("/static/app.css").status_code == 200


@pytest.mark.parametrize(
    ("method", "path"),
    [
        ("GET", "/api/v1/qsos"),
        ("POST", "/api/v1/qsos"),
        ("GET", "/api/v1/sessions/current"),
        ("GET", "/api/v1/qsos/00000000-0000-0000-0000-000000000000/history"),
    ],
)
def test_everything_else_needs_a_signed_in_operator(
    client: TestClient,
    method: str,
    path: str,
) -> None:
    """No token, a malformed one, or a made-up one: refused (Principle XIII)."""
    for headers in ({}, {"Authorization": "Basic abc"}, {"Authorization": "Bearer made-up"}):
        response = client.request(method, path, headers=headers, json={"fields": W1AW})
        assert response.status_code == 401, (headers, response.text)


def test_sign_in_and_out(client: TestClient, shack: Actor) -> None:
    """A wrong secret is 401; a session ends when signed out."""
    del shack
    wrong = client.post(
        "/api/v1/sessions",
        json={"callsign": "K9KSU", "secret": "000000", "station": "shack"},
    )
    assert wrong.status_code == 401
    assert wrong.json()["detail"] == "wrong callsign or secret"
    headers = _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")
    me = client.get("/api/v1/sessions/current", headers=headers).json()
    assert me["operator"] == "K9KSU"
    assert me["permissions"] == ["view", "log", "radio", "admin"]
    assert client.delete("/api/v1/sessions/current", headers=headers).status_code == 204
    assert client.get("/api/v1/sessions/current", headers=headers).status_code == 401


def test_enter_edit_delete_restore_and_read_history(
    client: TestClient,
    k9ksu: dict[str, str],
    w0xyz: dict[str, str],
) -> None:
    """The whole audited life of a QSO through the API, by two operators."""
    created = client.post("/api/v1/qsos", headers=k9ksu, json={"fields": W1AW})
    assert created.status_code == 201, created.text
    body = created.json()
    assert body["logged"] is False  # Principle XV is not claimed
    assert "not yet logged" in body["message"]
    qso = body["qso"]
    assert (qso["call"], qso["band"], qso["mode"]) == ("W1AW", "20m", "SSB")
    assert qso["qso_start"] == "2026-10-18T01:36:00Z"  # UTC, Principle VIII
    assert (qso["operator"], qso["station"]) == ("K9KSU", "shack")
    path = f"/api/v1/qsos/{qso['id']}"

    edited_fields = [*W1AW[:-1], {"name": "RST_RCVD", "value": "59"}]
    edited = client.put(
        path,
        headers=w0xyz,
        json={"base_version": 1, "fields": edited_fields, "note": "misheard"},
    )
    assert edited.status_code == 200, edited.text
    assert edited.json()["qso"]["version"] == 2
    stale = client.put(path, headers=k9ksu, json={"base_version": 1, "fields": W1AW})
    assert stale.status_code == 409

    assert client.post(f"{path}/delete", headers=w0xyz, json={"note": "dupe"}).status_code == 200
    listed = client.get("/api/v1/qsos", headers=k9ksu).json()
    assert listed == []
    listed = client.get("/api/v1/qsos?include_deleted=true", headers=k9ksu).json()
    assert [q["deleted"] for q in listed] == [True]
    assert client.post(f"{path}/restore", headers=k9ksu, json={}).status_code == 200

    history = client.get(f"{path}/history", headers=w0xyz).json()
    assert [(v["action"], v["operator"], v["station"], v["note"]) for v in history] == [
        ("create", "K9KSU", "shack", None),
        ("edit", "W0XYZ", "fd-2", "misheard"),
        ("delete", "W0XYZ", "fd-2", "dupe"),
        ("restore", "K9KSU", "shack", None),
    ]
    assert history[0]["fields"][-1] == {"name": "RST_RCVD", "value": "57", "type": None}
    assert all(v["changed_at"].endswith("Z") for v in history)


def test_listing_is_newest_first_and_filters_by_call(
    client: TestClient,
    k9ksu: dict[str, str],
) -> None:
    """The latest QSOs come first; a call search ignores case and spaces."""
    for call in ("Q1A", "Q2B", "Q3C"):
        fields = [{"name": "CALL", "value": call}, *W1AW[1:]]
        assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": fields}).is_success
    calls = [q["call"] for q in client.get("/api/v1/qsos?limit=2", headers=k9ksu).json()]
    assert calls == ["Q3C", "Q2B"]
    found = client.get("/api/v1/qsos", headers=k9ksu, params={"call": " q2b "}).json()
    assert [q["call"] for q in found] == ["Q2B"]


def test_permissions_are_enforced(
    client: TestClient,
    identity: IdentityStore,
    shack: Actor,
    w0xyz: dict[str, str],
) -> None:
    """Without log you can read but not write; without view you cannot read."""
    identity.set_permissions(shack, "W0XYZ", [Permission.LOG], granted=False)
    refused = client.post("/api/v1/qsos", headers=w0xyz, json={"fields": W1AW})
    assert refused.status_code == 403
    assert client.get("/api/v1/qsos", headers=w0xyz).status_code == 200
    identity.set_permissions(shack, "W0XYZ", [Permission.VIEW], granted=False)
    assert client.get("/api/v1/qsos", headers=w0xyz).status_code == 403


@pytest.mark.parametrize(
    ("body", "fragment"),
    [
        ({"fields": []}, "at least 1"),
        ({"fields": [{"name": "CALL", "value": "W1AW", "extra": 1}]}, "Extra inputs"),
        ({"fields": [{"name": "CA<LL", "value": "W1AW"}]}, "forbidden character"),
        ({"fields": [{"name": "CALL", "value": 5}]}, "valid string"),
        ({"fields": [{"name": "CALL", "value": "x" * 70000}]}, "at most 65536"),
        ({"fields": [{"name": "CALL", "value": "W1AW"}] * 501}, "at most 500"),
        ({"fields": W1AW, "note": "n" * 1001}, "at most 1000"),
        ({"fields": [{"name": "CALL", "value": "a\x00b"}]}, "NUL"),
    ],
)
def test_bad_input_is_refused_at_the_boundary(
    client: TestClient,
    k9ksu: dict[str, str],
    body: dict[str, object],
    fragment: str,
) -> None:
    """Every request body is validated before it reaches the log (Principle XII)."""
    response = client.post("/api/v1/qsos", headers=k9ksu, json=body)
    assert response.status_code == 422
    assert fragment in response.text
    assert client.get("/api/v1/qsos", headers=k9ksu).json() == []


def test_unknown_qsos_are_404(client: TestClient, k9ksu: dict[str, str]) -> None:
    """A QSO that does not exist is reported as such."""
    missing = "/api/v1/qsos/00000000-0000-0000-0000-000000000000"
    assert client.get(missing, headers=k9ksu).status_code == 404
    assert client.post(f"{missing}/delete", headers=k9ksu, json={}).status_code == 404
