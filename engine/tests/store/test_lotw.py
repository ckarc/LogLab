# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""LoTW through the API: optional (V), audited (VII), per operator (XIII)."""

from __future__ import annotations

import base64
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.adif import Field, Record
from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, LogStore, Permission

from ..lotw.fake import CERT_PASSWORD, LOTW_PASSWORD, FakeLotw, make_p12, report
from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

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
    assert _post(client, k9ksu, "upload")["status"] == "not_set_up"
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
    sent = _post(client, k9ksu, "upload")
    assert sent["status"] == "done"
    assert sent["sent"] == 2
    assert sent["message"].startswith("LoTW accepted 2 QSOs.")
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
        assert history[1]["note"] == "Sent to LoTW"
    # Nothing is sent twice.
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["waiting"] == 0
    again = _post(client, k9ksu, "upload")
    assert again["status"] == "nothing"
    assert len(lotw.uploads) == 1


def test_with_no_internet_nothing_changes_and_lotw_is_left_alone_for_a_minute(
    client: TestClient, k9ksu: dict[str, str], lotw: FakeLotw
) -> None:
    """QSOs simply keep waiting (Principle V)."""
    _set_up(client, k9ksu)
    qso_id = _log(client, k9ksu, qso())
    lotw.online = False
    result = _post(client, k9ksu, "upload")
    assert result["status"] == "unreachable"
    assert "can't be reached" in result["message"]
    assert "LOTW_QSL_SENT" not in _fields(client, k9ksu, qso_id)
    lotw.online = True
    assert _post(client, k9ksu, "upload")["status"] == "unreachable"  # not tried again yet
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
    result = _post(client, k9ksu, "upload")
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
    result = _post(client, k9ksu, "upload")
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
    """W0XYZ never sees or uses K9KSU's certificate (Principle XIII)."""
    _set_up(client, k9ksu)
    _log(client, k9ksu, qso())
    assert client.get("/api/v1/lotw", headers=w0xyz).json()["certificate"] is None
    assert _post(client, w0xyz, "upload")["status"] == "not_set_up"
    assert lotw.uploads == []
    assert client.delete("/api/v1/lotw", headers=w0xyz).status_code == 204
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["certificate"] is not None
    assert client.delete("/api/v1/lotw", headers=k9ksu).status_code == 204
    assert client.get("/api/v1/lotw", headers=k9ksu).json()["certificate"] is None


def test_sending_and_fetching_need_the_log_permission(
    client: TestClient, identity: IdentityStore, shack: Actor
) -> None:
    """They change QSOs, so a view-only operator can't (Principle XIII)."""
    identity.add_operator(shack, "KD0VUE", VISITOR_SECRET, [Permission.VIEW])
    viewer = _sign_in(client, "KD0VUE", VISITOR_SECRET, "fd-3")
    assert client.post("/api/v1/lotw/upload", headers=viewer).status_code == 403
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
