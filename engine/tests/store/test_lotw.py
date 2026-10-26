# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""LoTW through the API: optional (V), audited (VII), per operator (XXI)."""

from __future__ import annotations

import base64
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.adif import Field, Record
from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, LogStore, Permission

from ..lotw.fake import CERT_PASSWORD, LOTW_PASSWORD, FakeLotw, make_p12, report
from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_qrz_logbook import KEY, FakeLogbook

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.callbook.logbook_sync import LogbookSync
    from loglab_engine.store import Actor

KANSAS = {"grid": "EM18", "ituz": 7, "cqz": 4, "us_state": "KS", "us_county": "Ellis"}


def qso(call: str = "W1AW", **extra: str) -> list[dict[str, str]]:
    """A QSO's fields as the web client sends them."""
    fields = {
        "CALL": call,
        "QSO_DATE": "20261017",
        "TIME_ON": "1805",
        "BAND": "20m",
        "MODE": "SSB",
        **extra,
    }
    return [{"name": k, "value": v} for k, v in fields.items()]


@pytest.fixture
def lotw() -> FakeLotw:
    """A stand-in for LoTW."""
    return FakeLotw()


@pytest.fixture
def client(database: str, lotw: FakeLotw) -> TestClient:
    """The Engine's API on this test's database, reaching the stand-in LoTW."""
    return TestClient(create_app(conninfo(database), lotw_transport=lotw))


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
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log, nothing set up for LoTW."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def _put(client: TestClient, headers: dict[str, str], path: str, body: object) -> dict[str, Any]:
    response = client.put(f"/api/v1/lotw/{path}", headers=headers, json=body)
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def _post(client: TestClient, headers: dict[str, str], path: str) -> dict[str, Any]:
    response = client.post(f"/api/v1/lotw/{path}", headers=headers)
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def _send(client: TestClient, headers: dict[str, str]) -> dict[str, Any]:
    """Prepare a batch of the waiting QSOs, then send it, as the operator does."""
    prepared = _post(client, headers, "batches")
    if prepared["batch"] is None:
        return {**prepared, "sent": 0}
    sent = _post(client, headers, f"batches/{prepared['batch']['id']}/send")
    return {**sent, "skipped": prepared["skipped"] + sent["skipped"]}


def _set_up(
    client: TestClient, headers: dict[str, str], p12: bytes | None = None
) -> dict[str, Any]:
    p12 = make_p12() if p12 is None else p12
    _put(
        client,
        headers,
        "certificate",
        {"p12_base64": base64.b64encode(p12).decode(), "password": CERT_PASSWORD},
    )
    _put(client, headers, "login", {"username": "k9ksu", "password": LOTW_PASSWORD})
    return _put(client, headers, "location", KANSAS)


def _log(client: TestClient, headers: dict[str, str], fields: list[dict[str, str]]) -> str:
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    return str(response.json()["qso"]["id"])


def _fields(client: TestClient, headers: dict[str, str], qso_id: str) -> dict[str, str]:
    response = client.get(f"/api/v1/qsos/{qso_id}", headers=headers)
    assert response.status_code == 200, response.text
    return {f["name"]: f["value"] for f in response.json()["fields"]}


def test_with_nothing_set_up_lotw_says_so_and_logging_is_unchanged(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """LoTW is an enhancement; nothing depends on it (Principle V)."""
    setup = client.get("/api/v1/lotw", headers=k9ksu).json()
    assert setup["certificate"] is None
    assert setup["message"] == "LoTW is not set up."
    _log(client, k9ksu, qso())
    assert _send(client, k9ksu)["status"] == "not_set_up"
    assert _post(client, k9ksu, "download")["status"] == "not_set_up"
    assert lotw.uploads == []
    assert lotw.queries == []


def test_setting_up_shows_the_certificate_but_never_a_password(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """The API returns what the certificate says, never its key or either password."""
    setup = _set_up(client, k9ksu)
    assert setup["certificate"]["call"] == "K9KSU"
    assert setup["certificate"]["entity"] == "UNITED STATES OF AMERICA"
    assert setup["certificate"]["qso_first"] == "1990-01-01"
    assert setup["username"] == "k9ksu"
    assert setup["location"] == {**KANSAS, "ca_province": None}
    assert setup["problem"] is None
    text = client.get("/api/v1/lotw", headers=k9ksu).text
    assert CERT_PASSWORD not in text
    assert LOTW_PASSWORD not in text
    assert "PRIVATE" not in text


def test_a_bad_certificate_or_location_is_refused(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """Checked when saved, so a send never fails on it later (Principle XII)."""
    p12 = base64.b64encode(make_p12()).decode()
    wrong = client.put(
        "/api/v1/lotw/certificate", headers=k9ksu, json={"p12_base64": p12, "password": "nope"}
    )
    assert wrong.status_code == 422
    assert "password is wrong" in wrong.json()["detail"]
    junk = client.put(
        "/api/v1/lotw/certificate", headers=k9ksu, json={"p12_base64": "!!", "password": "x"}
    )
    assert junk.status_code == 422
    early = client.put("/api/v1/lotw/location", headers=k9ksu, json=KANSAS)
    assert early.status_code == 422
    assert "certificate first" in early.json()["detail"]
    _set_up(client, k9ksu)
    county = client.put(
        "/api/v1/lotw/location", headers=k9ksu, json={**KANSAS, "us_county": "Nowhere"}
    )
    assert county.status_code == 422
    assert "not a county LoTW lists in Kansas" in county.json()["detail"]


def test_waiting_qsos_are_signed_sent_and_marked_as_a_new_audited_version(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """LoTW gets verifiable signatures; each QSO records it was sent (Principles IV, VII)."""
    _set_up(client, k9ksu)
    first = _log(client, k9ksu, qso("W1AW", FREQ="14.205"))
    second = _log(client, k9ksu, qso("N0OY", MODE="MFSK", SUBMODE="FT4", BAND="40m"))
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["waiting"] == 2
    sent = _send(client, k9ksu)
    assert sent["status"] == "done"
    assert sent["sent"] == 2
    assert sent["message"].startswith("Batch 1 sent. LoTW queued 2 QSOs.")
    [upload] = lotw.uploads
    assert [c["SIGNDATA"] for c in upload] == [
        "4EM187ELLISKS20MW1AW14.205SSB2026-10-1718:05:00Z",
        "4EM187ELLISKS40MN0OYFT42026-10-1718:05:00Z",
    ]
    for qso_id in (first, second):
        fields = _fields(client, k9ksu, qso_id)
        assert fields["LOTW_QSL_SENT"] == "Y"
        assert len(fields["LOTW_QSLSDATE"]) == 8
        history = client.get(f"/api/v1/qsos/{qso_id}/history", headers=k9ksu).json()
        assert [(v["version"], v["action"], v["operator"]) for v in history] == [
            (1, "create", "K9KSU"),
            (2, "edit", "K9KSU"),
        ]
        assert history[1]["note"] == "Sent to LoTW in batch 1"
    # Nothing is sent twice.
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["waiting"] == 0
    again = _send(client, k9ksu)
    assert again["status"] == "nothing"
    assert len(lotw.uploads) == 1


def test_with_no_internet_nothing_changes_and_lotw_is_left_alone_for_a_minute(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """QSOs simply keep waiting (Principle V)."""
    _set_up(client, k9ksu)
    qso_id = _log(client, k9ksu, qso())
    lotw.online = False
    result = _send(client, k9ksu)
    assert result["status"] == "unreachable"
    assert "can't be reached" in result["message"]
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, qso_id)
    lotw.online = True
    assert _send(client, k9ksu)["status"] == "unreachable"  # not tried again yet
    assert _post(client, k9ksu, "download")["status"] == "unreachable"
    assert lotw.uploads == []
    assert lotw.queries == []


def test_a_refused_upload_changes_nothing(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """LoTW's own reason is shown."""
    _set_up(client, k9ksu)
    qso_id = _log(client, k9ksu, qso())
    lotw.refuse = "Certificate not issued by LoTW"
    result = _send(client, k9ksu)
    assert result["status"] == "rejected"
    assert result["message"] == "LoTW refused the upload: Certificate not issued by LoTW"
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, qso_id)


def test_only_the_certificate_s_own_signable_qsos_are_sent(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], lotw: FakeLotw
) -> None:
    """Another station's QSOs, ones LoTW can't take, and ones made elsewhere stay behind."""
    _set_up(client, k9ksu)
    _log(client, w0xyz, qso("W1AW"))  # entered by W0XYZ, so made as W0XYZ
    _log(client, k9ksu, qso("W1AW", STATION_CALLSIGN="W0CKA"))  # a club call
    _log(client, k9ksu, qso("W1AW", LOTW_QSL_SENT="N"))  # not to be sent
    _log(client, k9ksu, qso("W1AW", LOTW_QSL_SENT="Y"))  # sent before (an imported log)
    _log(client, w0xyz, qso("N0OY", OPERATOR="K9KSU"))  # K9KSU operating at W0XYZ's position
    bad_band = _log(client, k9ksu, qso("K0ABC", BAND="21m"))
    elsewhere = _log(client, k9ksu, qso("K0DEF", MY_GRIDSQUARE="DM79"))
    early = _log(client, k9ksu, qso("K0GHI", QSO_DATE="19850101"))
    result = _send(client, k9ksu)
    assert result["status"] == "done"
    assert result["sent"] == 1
    assert [c["CALL"] for c in lotw.uploads[0]] == ["N0OY"]
    reasons = {s["id"]: s["reason"] for s in result["skipped"]}
    assert reasons == {
        bad_band: "Not sent: LoTW does not know the band '21M'.",
        elsewhere: "Not sent: it was made from grid DM79, and your LoTW location is EM18.",
        early: "Not sent: your certificate signs QSOs from 1990-01-01.",
    }
    # They keep waiting, so they can be fixed and sent.
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["waiting"] == 3


def test_confirmations_mark_the_matching_qsos(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """Same call, band and mode, within 30 minutes; marked as a new audited version."""
    _set_up(client, k9ksu)
    w1aw = _log(client, k9ksu, qso("W1AW"))
    ft4 = _log(client, k9ksu, qso("N0OY", MODE="MFSK", SUBMODE="FT4", TIME_ON="1830"))
    later = _log(client, k9ksu, qso("N0OY", MODE="MFSK", SUBMODE="FT4", TIME_ON="2300"))
    lotw.report = report(
        {
            "CALL": "W1AW",
            "BAND": "20M",
            "MODE": "SSB",
            "QSO_DATE": "20261017",
            "TIME_ON": "180600",
            "QSL_RCVD": "Y",
            "QSLRDATE": "20261018",
            "STATION_CALLSIGN": "K9KSU",
        },
        {
            "CALL": "N0OY",
            "BAND": "20M",
            "MODE": "FT4",
            "APP_LoTW_MODEGROUP": "DATA",
            "QSO_DATE": "20261017",
            "TIME_ON": "182900",
            "QSL_RCVD": "Y",
            "QSLRDATE": "20261018",
        },
        {
            "CALL": "K0XYZ",
            "BAND": "20M",
            "MODE": "CW",
            "QSO_DATE": "20261017",
            "TIME_ON": "1200",
            "QSL_RCVD": "Y",
        },
        {
            "CALL": "W1AW",
            "BAND": "20M",
            "MODE": "SSB",
            "QSO_DATE": "20261017",
            "TIME_ON": "1805",
            "QSL_RCVD": "N",
        },
    )
    result = _post(client, k9ksu, "download")
    assert result == {
        "status": "done",
        "confirmed": 2,
        "already": 0,
        "unmatched": 1,
        "message": "2 QSOs newly confirmed by LoTW. 1 confirmation matched no QSO in this log.",
    }
    assert lotw.queries[0]["qso_qslsince"] == "1900-01-01"
    assert lotw.queries[0]["qso_owncall"] == "K9KSU"
    for qso_id in (w1aw, ft4):
        fields = _fields(client, k9ksu, qso_id)
        assert (fields["LOTW_QSL_RCVD"], fields["LOTW_QSLRDATE"]) == ("Y", "20261018")
        assert fields["LOTW_QSL_SENT"] == "Y"
        history = client.get(f"/api/v1/qsos/{qso_id}/history", headers=k9ksu).json()
        assert history[-1]["note"] == "Confirmed on LoTW"
    assert "LOTW_QSL_RCVD" not in _fields(client, k9ksu, later)
    # The next fetch asks only for what is new, and a repeat changes nothing.
    again = _post(client, k9ksu, "download")
    assert lotw.queries[1]["qso_qslsince"] == "2026-10-18"
    assert (again["confirmed"], again["already"]) == (0, 2)
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["qsl_since"] == "2026-10-18"


def test_a_wrong_lotw_login_is_reported(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """And nothing is marked."""
    _set_up(client, k9ksu)
    _put(client, k9ksu, "login", {"username": "k9ksu", "password": "wrong"})
    result = _post(client, k9ksu, "download")
    assert result["status"] == "login_failed"
    assert lotw.queries[0]["password"] == "wrong"  # noqa: S105 - the stand-in login typed


def test_a_set_up_belongs_to_its_operator(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], lotw: FakeLotw
) -> None:
    """W0XYZ never sees or uses K9KSU's certificate (Principle XXI)."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    assert client.get("/api/v1/lotw", headers=w0xyz).json()["certificate"] is None
    assert _send(client, w0xyz)["status"] == "not_set_up"
    assert lotw.uploads == []
    assert client.delete("/api/v1/lotw", headers=w0xyz).status_code == 204
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["certificate"] is not None
    assert client.delete("/api/v1/lotw", headers=k9ksu).status_code == 204
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["certificate"] is None


def test_sending_and_fetching_need_the_log_permission(
    client: TestClient, identity: IdentityStore, shack: Actor
) -> None:
    """They change QSOs, so a view-only operator can't (Principle XXI)."""
    identity.add_operator(shack, "KD0VUE", VISITOR_SECRET, [Permission.VIEW])
    viewer = _sign_in(client, "KD0VUE", VISITOR_SECRET, "fd-3")
    assert client.post("/api/v1/lotw/batches", headers=viewer).status_code == 403
    assert client.post("/api/v1/lotw/download", headers=viewer).status_code == 403
    for path in ("", "/places"):
        assert client.get(f"/api/v1/lotw{path}").status_code == 401


def test_the_places_list_gives_counties_and_zones(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """So the form can offer LoTW's own spellings."""
    places = client.get("/api/v1/lotw/places", headers=k9ksu).json()
    kansas = next(s for s in places["us_states"] if s["code"] == "KS")
    assert kansas["zones"] == [[7, 4]]
    assert "Ellis" in kansas["counties"]
    assert places["source"].startswith("TrustedQSL")


def test_a_background_edit_never_overwrites_an_operator_s_edit(
    conn: psycopg.Connection[TupleRow], store: LogStore, shack: Actor
) -> None:
    """``edit_many`` leaves a QSO that moved on, or would not change, as it is (VII)."""
    record = Record((Field("CALL", "W1AW"), Field("BAND", "20m")))
    one = store.create_qso(record, actor=shack)
    two = store.create_qso(record, actor=shack)
    three = store.create_qso(record, actor=shack)
    store.edit_qso(two.qso_id, Record((Field("CALL", "W1AX"),)), actor=shack, base_version=1)
    marked = Record((*record.fields, Field("LOTW_QSL_SENT", "Y")))
    changed = store.edit_many(
        [(one.qso_id, 1, marked), (two.qso_id, 1, marked), (three.qso_id, 1, record)],
        actor=shack,
        note="Sent to LoTW",
    )
    assert changed == (one.qso_id,)
    assert store.current(two.qso_id).record.get("CALL") == "W1AX"
    assert store.current(three.qso_id).version == 1
    assert store.edit_many([], actor=shack) == ()
    del conn


def test_lotw_marks_never_send_a_qso_to_qrz_again(
    database: str, shack: Actor, lotw: FakeLotw
) -> None:
    """QRZ.com's copy, with what QRZ.com got from LoTW itself, is left alone (Brian's rule).

    A real edit is still sent.
    """
    del shack
    qrz = FakeLogbook()
    app = create_app(conninfo(database), qrz_post=qrz, lotw_transport=lotw)
    client = TestClient(app)
    sync: LogbookSync = app.state.logbook
    k9ksu = _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")
    assert (
        client.put("/api/v1/logbook/qrz", headers=k9ksu, json={"api_key": KEY}).status_code == 200
    )
    _set_up(client, k9ksu)
    qso_id = _log(client, k9ksu, qso())
    sync.run_once()
    inserts = [a for a in qrz.asked if a["ACTION"] == "INSERT"]
    assert len(inserts) == 1
    lotw.report = report(
        {
            "CALL": "W1AW",
            "BAND": "20M",
            "MODE": "SSB",
            "QSO_DATE": "20261017",
            "TIME_ON": "1805",
            "QSL_RCVD": "Y",
        }
    )
    assert _send(client, k9ksu)["sent"] == 1
    assert _post(client, k9ksu, "download")["confirmed"] == 1
    sync.run_once()
    assert [a for a in qrz.asked if a["ACTION"] in {"INSERT", "DELETE"}] == inserts
    assert client.get("/api/v1/logbook/qrz", headers=k9ksu).json()["sent"] == 1
    # A change of the operator's own is still sent.
    version = client.get(f"/api/v1/qsos/{qso_id}", headers=k9ksu).json()["version"]
    fields = [{"name": k, "value": v} for k, v in _fields(client, k9ksu, qso_id).items()] + [
        {"name": "NAME", "value": "Hiram"}
    ]
    edited = client.put(
        f"/api/v1/qsos/{qso_id}", headers=k9ksu, json={"base_version": version, "fields": fields}
    )
    assert edited.status_code == 200, edited.text
    sync.run_once()
    (copy,) = qrz.books[KEY].values()
    assert copy["NAME"] == "Hiram"


def test_nothing_goes_to_lotw_until_a_reviewed_batch_is_sent(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """Preparing a batch fixes its QSOs and sends nothing; sending it is a separate act (XXI)."""
    _set_up(client, k9ksu)
    w1aw = _log(client, k9ksu, qso("W1AW"))
    n0oy = _log(client, k9ksu, qso("N0OY", TIME_ON="1830"))
    prepared = _post(client, k9ksu, "batches")
    assert prepared["status"] == "done"
    assert "Nothing has been sent yet" in prepared["message"]
    batch = prepared["batch"]
    assert (batch["state"], batch["count"], batch["sent"]) == ("ready", 2, 0)
    assert [(q["id"], q["call"], q["band"], q["mode"]) for q in batch["qsos"]] == [
        (w1aw, "W1AW", "20m", "SSB"),
        (n0oy, "N0OY", "20m", "SSB"),
    ]
    assert lotw.uploads == []
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, w1aw)
    setup = client.get("/api/v1/lotw", headers=k9ksu).json()
    assert setup["batch"]["id"] == batch["id"]
    assert setup["message"] == f"Batch {batch['id']} (2 QSOs) is waiting for your review."
    # A QSO logged after the batch was prepared is not in it, and asking
    # again answers with the batch already waiting.
    later = _log(client, k9ksu, qso("K0ABC", TIME_ON="1900"))
    again = _post(client, k9ksu, "batches")
    assert again["batch"]["id"] == batch["id"]
    assert "still waiting for your review" in again["message"]
    assert lotw.uploads == []
    sent = _post(client, k9ksu, f"batches/{batch['id']}/send")
    assert (sent["status"], sent["sent"]) == ("done", 2)
    assert [c["CALL"] for c in lotw.uploads[0]] == ["W1AW", "N0OY"]
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, later)
    # The batch is kept, with what LoTW said and which QSOs went.
    kept = client.get(f"/api/v1/lotw/batches/{batch['id']}", headers=k9ksu).json()
    assert (kept["state"], kept["sent"]) == ("sent", 2)
    assert kept["answer"].startswith("LoTW queued 2 QSOs.")
    assert all(q["sent"] for q in kept["qsos"])
    listed = client.get("/api/v1/lotw/batches", headers=k9ksu).json()
    assert [(b["id"], b["state"], b["qsos"]) for b in listed] == [(batch["id"], "sent", None)]
    # A sent batch is never sent again.
    assert _post(client, k9ksu, f"batches/{batch['id']}/send")["status"] == "nothing"
    assert len(lotw.uploads) == 1


def test_a_qso_edited_after_review_is_held_back_for_a_later_batch(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """Only what the operator reviewed is sent; the edited QSO keeps waiting."""
    _set_up(client, k9ksu)
    kept = _log(client, k9ksu, qso("W1AW"))
    edited = _log(client, k9ksu, qso("N0OY", TIME_ON="1830"))
    batch = _post(client, k9ksu, "batches")["batch"]
    fields = [{"name": k, "value": v} for k, v in _fields(client, k9ksu, edited).items()]
    changed = client.put(
        f"/api/v1/qsos/{edited}",
        headers=k9ksu,
        json={"base_version": 1, "fields": [*fields, {"name": "NAME", "value": "Hiram"}]},
    )
    assert changed.status_code == 200, changed.text
    sent = _post(client, k9ksu, f"batches/{batch['id']}/send")
    assert sent["sent"] == 1
    assert [c["CALL"] for c in lotw.uploads[0]] == ["W1AW"]
    assert [(s["id"], s["reason"]) for s in sent["skipped"]] == [
        (
            edited,
            "Not sent: it changed after the batch was prepared; it goes in a later batch.",
        )
    ]
    assert _fields(client, k9ksu, kept)["LOTW_QSL_SENT"] == "Y"
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, edited)
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["waiting"] == 1


def test_a_cancelled_batch_sends_nothing_and_its_qsos_keep_waiting(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """Cancelling is final; a new batch picks the QSOs up again."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    batch = _post(client, k9ksu, "batches")["batch"]
    cancelled = client.post(f"/api/v1/lotw/batches/{batch['id']}/cancel", headers=k9ksu)
    assert cancelled.status_code == 200, cancelled.text
    assert cancelled.json()["state"] == "cancelled"
    again = client.post(f"/api/v1/lotw/batches/{batch['id']}/cancel", headers=k9ksu)
    assert again.status_code == 409
    assert _post(client, k9ksu, f"batches/{batch['id']}/send")["status"] == "nothing"
    assert lotw.uploads == []
    setup = client.get("/api/v1/lotw", headers=k9ksu).json()
    assert (setup["waiting"], setup["batch"]) == (1, None)
    assert _post(client, k9ksu, "batches")["batch"]["id"] != batch["id"]


def test_a_refused_batch_keeps_waiting_with_lotw_s_reason(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """So it can be sent again once the problem is fixed."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    batch = _post(client, k9ksu, "batches")["batch"]
    lotw.refuse = "Certificate not issued by LoTW"
    assert _post(client, k9ksu, f"batches/{batch['id']}/send")["status"] == "rejected"
    kept = client.get(f"/api/v1/lotw/batches/{batch['id']}", headers=k9ksu).json()
    assert kept["state"] == "ready"
    assert kept["answer"] == "LoTW refused the upload: Certificate not issued by LoTW"
    lotw.refuse = None
    assert _post(client, k9ksu, f"batches/{batch['id']}/send")["sent"] == 1


def test_a_batch_belongs_to_its_operator(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], lotw: FakeLotw
) -> None:
    """W0XYZ can't see, send or cancel K9KSU's batch (Principle XXI)."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    batch = _post(client, k9ksu, "batches")["batch"]
    assert client.get(f"/api/v1/lotw/batches/{batch['id']}", headers=w0xyz).status_code == 404
    assert client.get("/api/v1/lotw/batches", headers=w0xyz).json() == []
    assert _post(client, w0xyz, f"batches/{batch['id']}/send")["status"] == "nothing"
    cancel = client.post(f"/api/v1/lotw/batches/{batch['id']}/cancel", headers=w0xyz)
    assert cancel.status_code == 409
    assert lotw.uploads == []
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["batch"]["id"] == batch["id"]


def test_the_database_keeps_every_batch_as_it_was(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """A batch is never deleted, and a finished one never changes."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    batch = _post(client, k9ksu, "batches")["batch"]
    _post(client, k9ksu, f"batches/{batch['id']}/send")
    for statement in (
        "DELETE FROM workspace.lotw_batch_qso",
        "UPDATE workspace.lotw_batch_qso SET sent = false",
        "DELETE FROM workspace.lotw_batch",
        "UPDATE workspace.lotw_batch SET state = 'ready', finished_at = NULL",
    ):
        with pytest.raises(psycopg.Error):
            conn.execute(statement)
