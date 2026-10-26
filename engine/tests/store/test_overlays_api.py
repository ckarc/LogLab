# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The overlay API: the overlays, and the Engine's dupe check against the real log (I, II)."""

from __future__ import annotations

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
    """K9KSU's headers."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: a second operator at another Field Day position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def _fields(**changes: str | None) -> list[dict[str, str]]:
    """A Field Day QSO with W1AW as W0CK in 2027; ``changes`` set or (with None) drop fields."""
    fields: dict[str, str | None] = {
        "CALL": "W1AW",
        "QSO_DATE": "20270626",
        "TIME_ON": "1830",
        "BAND": "20m",
        "MODE": "SSB",
        "CLASS": "2A",
        "ARRL_SECT": "CT",
        "CONTEST_ID": "ARRL-FD",
        "STATION_CALLSIGN": "W0CK",
    }
    fields.update({k.upper(): v for k, v in changes.items()})
    return [{"name": k, "value": v} for k, v in fields.items() if v is not None]


def _log(client: TestClient, headers: dict[str, str], fields: list[dict[str, str]]) -> str:
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    qso_id: str = response.json()["qso"]["id"]
    return qso_id


def _check(
    client: TestClient,
    headers: dict[str, str],
    fields: list[dict[str, str]],
    overlay: str = "arrl-fd",
    **more: str,
) -> dict[str, Any]:
    response = client.post(
        f"/api/v1/overlays/{overlay}/check", headers=headers, json={"fields": fields, **more}
    )
    assert response.status_code == 200, response.text
    out: dict[str, Any] = response.json()
    return out


def test_the_overlays_are_listed_for_a_signed_in_operator(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """Everyday first, and each overlay as its file describes it."""
    assert client.get("/api/v1/overlays").status_code == 401
    listed = client.get("/api/v1/overlays", headers=k9ksu).json()
    assert [o["id"] for o in listed][:1] == ["everyday"]
    fd = client.get("/api/v1/overlays/arrl-fd", headers=k9ksu).json()
    assert fd["contest_id"] == "ARRL-FD"
    assert client.get("/api/v1/overlays/nope", headers=k9ksu).status_code == 404


def test_a_dupe_logged_at_another_position_is_found(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """Every position checks the same log: W0XYZ sees the QSO K9KSU logged (Principle II)."""
    first = _log(client, k9ksu, _fields())
    result = _check(client, w0xyz, _fields(mode="FM"))
    assert result["status"] == "dupe"
    assert result["station_call"] == "W0CK"
    assert [d["id"] for d in result["dupes"]] == [first]
    assert result["dupes"][0]["operator"] == "K9KSU"
    assert _check(client, w0xyz, _fields(mode="CW"))["status"] == "new"


def test_a_dupe_is_still_logged(client: TestClient, k9ksu: dict[str, str]) -> None:
    """A dupe is a warning, not a refusal: storing it works."""
    _log(client, k9ksu, _fields())
    assert _check(client, k9ksu, _fields())["status"] == "dupe"
    _log(client, k9ksu, _fields())


@pytest.mark.parametrize(
    ("change", "why"),
    [
        ({"contest_id": "WFD"}, "another contest"),
        ({"contest_id": None}, "no contest"),
        ({"station_callsign": "K9KSU"}, "another station call"),
        ({"qso_date": "20260627"}, "another year"),
        ({"call": "W1AW/M"}, "another call"),
    ],
)
def test_qsos_outside_the_contest_are_not_dupes(
    client: TestClient, k9ksu: dict[str, str], change: dict[str, str | None], why: str
) -> None:
    """Only the same call, station call, contest and year can make a dupe."""
    _log(client, k9ksu, _fields(**change))
    assert _check(client, k9ksu, _fields())["status"] == "new", why


def test_a_deleted_qso_is_not_a_dupe(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Deleting the first QSO frees its band and mode."""
    first = _log(client, k9ksu, _fields())
    response = client.post(f"/api/v1/qsos/{first}/delete", headers=k9ksu, json={})
    assert response.status_code == 200, response.text
    assert _check(client, k9ksu, _fields())["status"] == "new"


def test_a_qso_being_edited_is_not_its_own_dupe(client: TestClient, k9ksu: dict[str, str]) -> None:
    """``exclude`` leaves out the QSO being checked."""
    first = _log(client, k9ksu, _fields())
    assert _check(client, k9ksu, _fields(), exclude=first)["status"] == "new"


def test_the_station_call_falls_back_to_the_operator(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """With no STATION_CALLSIGN, the QSO is made as the signed-in operator."""
    _log(client, k9ksu, _fields(station_callsign=None))
    result = _check(client, k9ksu, _fields(station_callsign=None))
    assert (result["status"], result["station_call"]) == ("dupe", "K9KSU")
    other = _check(client, k9ksu, _fields(station_callsign=None), station_call="W0CK")
    assert other["status"] == "new"


def test_everyday_does_not_check(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Everyday never checks for dupes, even with the same QSO in the log."""
    _log(client, k9ksu, _fields(contest_id=None))
    assert _check(client, k9ksu, _fields(contest_id=None), "everyday")["status"] == "no_check"


def test_winter_field_day_reports_ft8_and_a_bad_class(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """FT8 doesn't count in WFD, and 2A is a Field Day class, not a WFD one."""
    result = _check(client, k9ksu, _fields(mode="FT8", contest_id="WFD"), "wfd")
    assert result["status"] == "not_counted"
    assert [p["field"] for p in result["problems"]] == ["CLASS"]


def test_the_check_needs_view_and_a_valid_body(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Signed in, and a body the Engine can read (Principles XII and XXI)."""
    response = client.post("/api/v1/overlays/arrl-fd/check", json={"fields": _fields()})
    assert response.status_code == 401
    response = client.post(
        "/api/v1/overlays/arrl-fd/check", headers=k9ksu, json={"fields": [], "extra": 1}
    )
    assert response.status_code == 422
