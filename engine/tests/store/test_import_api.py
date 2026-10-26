# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Taking in an ADIF file from the web client: new QSOs only (XV), deletes stay (VII)."""

from __future__ import annotations

import base64
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, LogStore, Permission

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

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


def _qso(call: str, day: str, time: str, band: str = "20m", mode: str = "FT8") -> str:
    fields = {"CALL": call, "QSO_DATE": day, "TIME_ON": time, "BAND": band, "MODE": mode}
    return "".join(f"<{k}:{len(v)}>{v}" for k, v in fields.items()) + "<EOR>\n"


HEADER = "WSJT-X ADIF Export\n<PROGRAMID:6>WSJT-X <EOH>\n"
FIRST = _qso("W1AW", "20270626", "183015") + _qso("K0ABC", "20270626", "184500")


def _send(
    client: TestClient,
    headers: dict[str, str],
    text: str,
    *,
    operation: dict[str, Any] | None = None,
    name: str = "wsjtx_log.adi",
) -> dict[str, Any]:
    body: dict[str, Any] = {
        "file_name": name,
        "data_base64": base64.b64encode(text.encode()).decode(),
    }
    if operation is not None:
        body["operation"] = operation
    response = client.post("/api/v1/imports", headers=headers, json=body)
    assert response.status_code in {200, 201}, response.text
    out: dict[str, Any] = response.json()
    return out


def test_a_file_is_taken_in_with_every_field(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """Each QSO keeps the fields the file gave it (Principle IV)."""
    out = _send(client, k9ksu, HEADER + FIRST)
    assert out["records"] == 2
    assert out["added"] == 2
    assert out["import_id"] is not None
    calls = sorted(v.record.get("CALL") or "" for v in LogStore(conn).find(limit=10))
    assert calls == ["K0ABC", "W1AW"]


def test_a_log_file_read_again_adds_only_its_new_qsos(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """A program's log grows; taking it in again never makes a duplicate (Principle XV)."""
    _send(client, k9ksu, HEADER + FIRST)
    grown = HEADER + FIRST + _qso("N0CALL", "20270626", "190000")
    out = _send(client, k9ksu, grown)
    assert (out["added"], out["already_logged"]) == (1, 2)


def test_the_same_file_twice_adds_nothing(client: TestClient, k9ksu: dict[str, str]) -> None:
    """The exact same file is recognised and named, not imported twice."""
    first = _send(client, k9ksu, HEADER + FIRST)
    again = _send(client, k9ksu, HEADER + FIRST)
    assert again["added"] == 0
    assert again["already_imported"] == first["import_id"]


def test_a_qso_typed_without_seconds_matches_one_with_seconds(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """Same call, band and mode in the same UTC minute is the same contact."""
    _send(client, k9ksu, HEADER + _qso("W1AW", "20270626", "1830"))
    out = _send(client, k9ksu, HEADER + _qso("w1aw ", "20270626", "183059", band="20M"))
    assert (out["added"], out["already_logged"]) == (0, 1)


def test_a_different_band_or_mode_is_a_different_contact(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """W1AW on 20m and on 40m in the same minute are two QSOs."""
    _send(client, k9ksu, HEADER + _qso("W1AW", "20270626", "1830"))
    out = _send(
        client,
        k9ksu,
        HEADER
        + _qso("W1AW", "20270626", "1830", band="40m")
        + _qso("W1AW", "20270626", "1830", mode="FT4"),
    )
    assert out["added"] == 2


def test_a_contact_twice_in_one_file_is_taken_once(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """A file that lists a contact twice adds it once."""
    out = _send(client, k9ksu, HEADER + FIRST + _qso("W1AW", "20270626", "183040"))
    assert (out["added"], out["repeated"]) == (2, 1)


def test_a_deleted_qso_is_not_brought_back(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """Deleting is deliberate; reading the file again doesn't undo it (Principle VII)."""
    _send(client, k9ksu, HEADER + FIRST)
    store = LogStore(conn)
    w1aw = next(v for v in store.find(limit=10) if v.record.get("CALL") == "W1AW")
    response = client.post(f"/api/v1/qsos/{w1aw.qso_id}/delete", headers=k9ksu, json={})
    assert response.status_code == 200, response.text
    out = _send(client, k9ksu, HEADER + FIRST + _qso("N0CALL", "20270626", "190000"))
    assert (out["added"], out["already_logged"], out["already_deleted"]) == (1, 1, 1)


def test_into_an_operation_only_its_hours_count(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """In an operation, QSOs get its fields, and ones outside its hours are left out."""
    response = client.post(
        "/api/v1/operations",
        headers=k9ksu,
        json={
            "overlay": "everyday",
            "name": "Saturday",
            "station_call": "W0CK",
            "starts_at": "2027-06-26T18:00:00Z",
            "ends_at": "2027-06-26T18:59:00Z",
        },
    )
    assert response.status_code == 201, response.text
    op = response.json()
    text = HEADER + FIRST + _qso("N0CALL", "20270626", "190000") + "<CALL:4>K1XX<EOR>\n"
    out = _send(client, k9ksu, text, operation={"id": op["id"]})
    assert (out["added"], out["outside_hours"], out["no_time"]) == (2, 1, 1)
    logged = LogStore(conn).find(limit=10)
    assert {v.record.get("STATION_CALLSIGN") for v in logged} == {"W0CK"}
    assert {v.record.get("APP_LOGLAB_OPERATION") for v in logged} == {op["id"]}


def test_a_file_that_is_not_adif_is_refused(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Untrusted input is checked where it enters, and nothing is stored (Principle XII)."""
    body = {"file_name": "x.adi", "data_base64": base64.b64encode(b"<CALL:99>W1AW").decode()}
    response = client.post("/api/v1/imports", headers=k9ksu, json=body)
    assert response.status_code == 422
    assert "not an ADIF file" in response.json()["detail"]
    body["data_base64"] = "not base64!"
    assert client.post("/api/v1/imports", headers=k9ksu, json=body).status_code == 422


def test_importing_needs_the_logging_permission(
    client: TestClient, identity: IdentityStore, shack: Actor
) -> None:
    """Someone who may only view can't add QSOs this way either (Principle XXI)."""
    identity.add_operator(shack, "W0VUE", VISITOR_SECRET, [Permission.VIEW])
    viewer = _sign_in(client, "W0VUE", VISITOR_SECRET, "fd-2")
    body = {"file_name": "x.adi", "data_base64": base64.b64encode(FIRST.encode()).decode()}
    assert client.post("/api/v1/imports", headers=viewer, json=body).status_code == 403
    assert client.post("/api/v1/imports", json=body).status_code == 401


def test_the_web_client_offers_the_import(client: TestClient) -> None:
    """The Menu's Import an ADIF file entry and its script are served."""
    assert 'id="import-open"' in client.get("/").text
    assert client.get("/static/import.js").status_code == 200
    assert client.get("/static/import.css").status_code == 200
