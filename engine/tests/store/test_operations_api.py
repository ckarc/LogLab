# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operations: club-call logging with each operator recorded (VII, XIII), GOTA (FD 4.1.1.1)."""

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
    """K9KSU's headers: an administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log only, at the GOTA position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "gota")


FIELD_DAY = {
    "overlay": "arrl-fd",
    "name": "Field Day 2027",
    "station_call": "w0ck",
    "gota_call": "K0GTA",
    "sent": {"class": "3a", "section": "ks"},
}


@pytest.fixture
def field_day(client: TestClient, k9ksu: dict[str, str]) -> dict[str, Any]:
    """The club's Field Day operation, set up by K9KSU."""
    response = client.post("/api/v1/operations", headers=k9ksu, json=FIELD_DAY)
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()
    return out


QSO = [
    {"name": "CALL", "value": "W1AW"},
    {"name": "QSO_DATE", "value": "20270626"},
    {"name": "TIME_ON", "value": "1830"},
    {"name": "BAND", "value": "20m"},
    {"name": "MODE", "value": "CW"},
    {"name": "CLASS", "value": "2A"},
    {"name": "ARRL_SECT", "value": "CT"},
]


def _log(
    client: TestClient, headers: dict[str, str], operation: dict[str, Any], *, gota: bool = False
) -> dict[str, Any]:
    body = {"fields": QSO, "operation": {"id": operation["id"], "gota": gota}}
    response = client.post("/api/v1/qsos", headers=headers, json=body)
    assert response.status_code == 201, response.text
    qso: dict[str, Any] = response.json()["qso"]
    return qso


def _values(qso: dict[str, Any]) -> dict[str, str]:
    return {f["name"]: f["value"] for f in qso["fields"]}


def test_an_administrator_sets_up_an_operation(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Calls and the exchange are kept in capitals; everyone signed in can see it."""
    assert field_day["station_call"] == "W0CK"
    assert field_day["sent"] == {"class": "3A", "section": "KS"}
    assert field_day["overlay_name"] == "ARRL Field Day"
    listed = client.get("/api/v1/operations", headers=k9ksu).json()
    assert [o["id"] for o in listed] == [field_day["id"]]


def test_only_an_administrator_sets_up_an_operation(
    client: TestClient, w0xyz: dict[str, str]
) -> None:
    """Logging does not grant administration (Principle XIII)."""
    response = client.post("/api/v1/operations", headers=w0xyz, json=FIELD_DAY)
    assert response.status_code == 403


@pytest.mark.parametrize(
    ("change", "why"),
    [
        ({"sent": {"class": "3Z", "section": "KS"}}, "not a Field Day class"),
        ({"sent": {"class": "3A"}}, "no section"),
        ({"sent": {"class": "3A", "section": "KS", "power": "100"}}, "a part FD doesn't send"),
        ({"gota_call": "W0CK"}, "GOTA call is the station call"),
        ({"overlay": "nope"}, "no such overlay"),
        ({"station_call": "not a call"}, "not a call"),
    ],
)
def test_an_operation_that_is_not_right_is_refused(
    client: TestClient, k9ksu: dict[str, str], change: dict[str, object], why: str
) -> None:
    """The exchange is checked against the overlay before anything is saved."""
    response = client.post("/api/v1/operations", headers=k9ksu, json={**FIELD_DAY, **change})
    assert response.status_code in {404, 422}, why


def test_every_operator_logs_under_the_club_call_and_is_recorded(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Brian: all QSOs go to the club call, with the operator noted on each one."""
    mine = _log(client, k9ksu, field_day)
    theirs = _log(client, w0xyz, field_day)
    assert _values(mine)["STATION_CALLSIGN"] == "W0CK"
    assert (_values(mine)["OPERATOR"], mine["operator"]) == ("K9KSU", "K9KSU")
    assert (_values(theirs)["OPERATOR"], theirs["operator"]) == ("W0XYZ", "W0XYZ")
    assert _values(theirs)["CONTEST_ID"] == "ARRL-FD"
    assert _values(theirs)["STX_STRING"] == "3A KS"


def test_the_gota_station_logs_under_the_gota_call(
    client: TestClient, w0xyz: dict[str, str], field_day: dict[str, Any]
) -> None:
    """FD Rule 4.1.1.1: the GOTA station uses its own call, and its own dupes."""
    main = _log(client, w0xyz, field_day)
    gota = _log(client, w0xyz, field_day, gota=True)
    assert _values(gota)["STATION_CALLSIGN"] == "K0GTA"
    body: dict[str, Any] = {"fields": QSO, "operation": {"id": field_day["id"], "gota": True}}
    check = client.post("/api/v1/overlays/arrl-fd/check", headers=w0xyz, json=body).json()
    assert (check["status"], check["station_call"]) == ("dupe", "K0GTA")
    assert [d["id"] for d in check["dupes"]] == [gota["id"]]
    body["operation"]["gota"] = False
    check = client.post("/api/v1/overlays/arrl-fd/check", headers=w0xyz, json=body).json()
    assert [d["id"] for d in check["dupes"]] == [main["id"]]


def test_an_ended_operation_logs_nothing_more(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Ending keeps the operation and its QSOs, but no new QSO can be logged in it."""
    _log(client, k9ksu, field_day)
    response = client.put(
        f"/api/v1/operations/{field_day['id']}", headers=k9ksu, json={**FIELD_DAY, "ended": True}
    )
    assert response.status_code == 200, response.text
    assert client.get("/api/v1/operations", headers=k9ksu).json() == []
    ended = client.get("/api/v1/operations?include_ended=true", headers=k9ksu).json()
    assert [o["ended"] for o in ended] == [True]
    body = {"fields": QSO, "operation": {"id": field_day["id"]}}
    assert client.post("/api/v1/qsos", headers=k9ksu, json=body).status_code == 409


def test_a_check_in_another_overlay_is_refused(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """A Field Day operation can't be checked under Winter Field Day's rules."""
    body = {"fields": QSO, "operation": {"id": field_day["id"]}}
    assert client.post("/api/v1/overlays/wfd/check", headers=k9ksu, json=body).status_code == 422


def test_an_unknown_operation_is_not_found(client: TestClient, k9ksu: dict[str, str]) -> None:
    """A QSO naming an operation that doesn't exist is refused, not logged without it."""
    body = {"fields": QSO, "operation": {"id": "00000000-0000-4000-8000-000000000000"}}
    assert client.post("/api/v1/qsos", headers=k9ksu, json=body).status_code == 404


def test_the_chosen_operation_is_saved_in_the_layout(
    client: TestClient, k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Each operator's choice of operation follows them, like the rest of their layout."""
    layout = {"modules": [{"id": "entry"}], "operation": {"id": field_day["id"], "gota": True}}
    assert client.put("/api/v1/layout", headers=k9ksu, json=layout).status_code == 200
    saved = client.get("/api/v1/layout", headers=k9ksu).json()["layout"]
    assert saved["operation"] == {"id": field_day["id"], "gota": True}


def test_an_operator_sets_up_field_day_under_their_own_call(
    client: TestClient, w0xyz: dict[str, str], k9ksu: dict[str, str], field_day: dict[str, Any]
) -> None:
    """Field Day from home as 1D needs no administrator; a club call or a GOTA call does."""
    home = {"overlay": "arrl-fd", "name": "FD at home", "station_call": "W0XYZ"}
    home["sent"] = {"class": "1D", "section": "KS"}  # type: ignore[assignment]
    response = client.post("/api/v1/operations", headers=w0xyz, json=home)
    assert response.status_code == 201, response.text
    mine = response.json()
    assert _values(_log(client, w0xyz, mine))["STATION_CALLSIGN"] == "W0XYZ"
    gota = client.post("/api/v1/operations", headers=w0xyz, json={**home, "gota_call": "K0GTA"})
    assert gota.status_code == 403
    club = client.put(f"/api/v1/operations/{field_day['id']}", headers=w0xyz, json=home)
    assert club.status_code == 403
    ended = client.put(
        f"/api/v1/operations/{mine['id']}", headers=w0xyz, json={**home, "ended": True}
    )
    assert ended.status_code == 200, ended.text
    del k9ksu


def test_only_field_day_has_a_gota_call(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Winter Field Day has no GOTA station, so its operations take no GOTA call."""
    wfd = {**FIELD_DAY, "overlay": "wfd", "sent": {"class": "2O", "section": "KS"}}
    response = client.post("/api/v1/operations", headers=k9ksu, json=wfd)
    assert response.status_code == 422
    assert "GOTA" in response.json()["detail"]
