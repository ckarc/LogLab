# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The overlay editor: administrators make and copy overlays (III, XII, XXI)."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, Permission

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from loglab_engine.store import Actor

_ABOUT = ("builtin", "version", "retired", "saved_by", "saved_at")


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
    """K9KSU's headers: an administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log only."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def _copy(client: TestClient, headers: dict[str, str], **changes: object) -> dict[str, Any]:
    """Field Day as the editor would copy it: a new id and name, the rest the same."""
    fd = client.get("/api/v1/overlays/arrl-fd", headers=headers).json()
    definition = {k: v for k, v in fd.items() if k not in _ABOUT}
    return {**definition, "id": "club-fd", "name": "Club Field Day", **changes}


def _save(
    client: TestClient, headers: dict[str, str], definition: dict[str, Any], base: int = 0
) -> Any:  # noqa: ANN401 - a test response
    return client.put(
        f"/api/v1/overlays/{definition['id']}",
        headers=headers,
        json={"definition": definition, "base_version": base},
    )


def test_an_administrator_saves_a_copy_of_field_day(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """The copy is listed beside the built-in ones, with who saved it."""
    response = _save(client, k9ksu, _copy(client, k9ksu))
    assert response.status_code == 200, response.text
    saved = response.json()
    assert (saved["builtin"], saved["version"], saved["saved_by"]) == (False, 1, "K9KSU")
    listed = {o["id"]: o for o in client.get("/api/v1/overlays", headers=k9ksu).json()}
    assert listed["club-fd"]["name"] == "Club Field Day"
    assert listed["arrl-fd"]["builtin"] is True


def test_a_saved_copy_checks_dupes_like_its_original(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """The editor's overlay is used exactly as a built-in one is."""
    assert _save(client, k9ksu, _copy(client, k9ksu)).status_code == 200
    fields = [
        {"name": "CALL", "value": "W1AW"},
        {"name": "QSO_DATE", "value": "20270626"},
        {"name": "BAND", "value": "20m"},
        {"name": "MODE", "value": "CW"},
        {"name": "CLASS", "value": "2A"},
        {"name": "ARRL_SECT", "value": "CT"},
    ]
    response = client.post("/api/v1/overlays/club-fd/check", headers=k9ksu, json={"fields": fields})
    assert response.status_code == 200, response.text
    assert response.json()["status"] == "new"


def test_only_an_administrator_saves_an_overlay(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """Logging does not grant changing the overlays everyone uses (Principle XXI)."""
    assert _save(client, w0xyz, _copy(client, k9ksu)).status_code == 403


def test_a_built_in_overlay_cannot_be_changed(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Field Day as shipped stays as shipped; it can only be copied."""
    response = _save(client, k9ksu, _copy(client, k9ksu, id="arrl-fd"))
    assert response.status_code == 409
    assert "copy" in response.json()["detail"]
    response = client.delete("/api/v1/overlays/arrl-fd?base_version=1", headers=k9ksu)
    assert response.status_code == 409


@pytest.mark.parametrize(
    ("change", "why"),
    [
        ({"id": "Not An Id"}, "an id is lower case"),
        ({"name": ""}, "no name"),
        ({"format": 2}, "a format this Engine does not read"),
        ({"bands": {"allowed": ["twenty"]}}, "not a band"),
        ({"surprise": True}, "a key overlays do not have"),
    ],
)
def test_an_overlay_that_is_not_right_is_refused(
    client: TestClient, k9ksu: dict[str, str], change: dict[str, object], why: str
) -> None:
    """Every saved overlay passes the same model as the shipped files (Principle XII)."""
    definition = _copy(client, k9ksu, **change)
    response = client.put(
        "/api/v1/overlays/club-fd",
        headers=k9ksu,
        json={"definition": definition, "base_version": 0},
    )
    assert response.status_code == 422, why


def test_a_save_over_a_newer_version_is_refused(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Two administrators editing at once can't overwrite each other."""
    assert _save(client, k9ksu, _copy(client, k9ksu)).status_code == 200
    assert _save(client, k9ksu, _copy(client, k9ksu, name="Mine")).status_code == 409
    second = _save(client, k9ksu, _copy(client, k9ksu, name="Mine"), base=1)
    assert second.status_code == 200, second.text
    assert (second.json()["name"], second.json()["version"]) == ("Mine", 2)
    assert _save(client, k9ksu, _copy(client, k9ksu, name="Theirs"), base=1).status_code == 409


def test_a_retired_overlay_is_hidden_and_can_come_back(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """Retiring keeps every version; saving it again brings it back."""
    assert _save(client, k9ksu, _copy(client, k9ksu)).status_code == 200
    response = client.delete("/api/v1/overlays/club-fd?base_version=1", headers=k9ksu)
    assert response.status_code == 200, response.text
    assert response.json()["retired"] is True
    listed = [o["id"] for o in client.get("/api/v1/overlays", headers=k9ksu).json()]
    assert "club-fd" not in listed
    every = client.get("/api/v1/overlays?include_retired=true", headers=k9ksu).json()
    assert "club-fd" in [o["id"] for o in every]
    assert (
        client.post(
            "/api/v1/operations",
            headers=k9ksu,
            json={"overlay": "club-fd", "name": "FD", "station_call": "W0CK", "sent": {}},
        ).status_code
        == 404
    )
    back = _save(client, k9ksu, _copy(client, k9ksu), base=2)
    assert back.status_code == 200, back.text
    assert (back.json()["retired"], back.json()["version"]) == (False, 3)


def test_an_operation_uses_an_overlay_from_the_editor(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """An operation can run on a copy; the copy can't be retired while it does."""
    assert _save(client, k9ksu, _copy(client, k9ksu)).status_code == 200
    op = client.post(
        "/api/v1/operations",
        headers=k9ksu,
        json={
            "overlay": "club-fd",
            "name": "Field Day 2027",
            "station_call": "W0CK",
            "gota_call": "K0GTA",
            "sent": {"class": "3A", "section": "KS"},
        },
    )
    assert op.status_code == 201, op.text
    assert op.json()["overlay_name"] == "Club Field Day"
    response = client.delete("/api/v1/overlays/club-fd?base_version=1", headers=k9ksu)
    assert response.status_code == 409
    assert "Field Day 2027" in response.json()["detail"]


def test_two_overlays_cannot_share_a_name(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Operators pick an overlay by its name, so each name is used once."""
    assert _save(client, k9ksu, _copy(client, k9ksu)).status_code == 200
    again = _save(client, k9ksu, _copy(client, k9ksu, id="club-fd-2"))
    assert again.status_code == 409
    builtin = _save(client, k9ksu, _copy(client, k9ksu, id="club-fd-3", name="arrl field day"))
    assert builtin.status_code == 409
