# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Sending QSOs to QRZ logbooks: never in logging's way (V), per operator (XXI)."""

from __future__ import annotations

import urllib.parse
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.adif import parse_adi
from loglab_engine.api import create_app
from loglab_engine.callbook import QrzUnreachableError
from loglab_engine.store import IdentityStore, NotAuthorizedError, Permission
from loglab_engine.store.qrz_logbook import LogbookStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_api import W1AW

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.callbook.logbook_sync import LogbookSync
    from loglab_engine.store import Actor

KEY = "ABCD-1234-EF56-7890"
CLUB_KEY = "CLUB-0000-1111-2222"


class FakeLogbook:
    """QRZ.com's logbook service as its developer guide describes it."""

    def __init__(self) -> None:
        """Online, with K9KSU's and W0CKA's logbooks empty."""
        self.online = True
        self.books: dict[str, dict[int, dict[str, str]]] = {KEY: {}, CLUB_KEY: {}}
        self.asked: list[dict[str, str]] = []
        self.next_logid = 1000
        self.refuse_dates: set[str] = set()
        self.agents: set[str] = set()

    def __call__(self, url: str, body: bytes, agent: str) -> bytes:
        """Answer one POST."""
        assert url == "https://logbook.qrz.com/api"
        if not self.online:
            raise QrzUnreachableError("no internet")
        self.agents.add(agent)
        form = dict(urllib.parse.parse_qsl(body.decode()))
        self.asked.append(form)
        book = self.books.get(form["KEY"])
        if book is None:
            return b"RESULT=FAIL&REASON=invalid api key"
        if form["ACTION"] == "STATUS":
            return b"RESULT=OK&DATA=BOOKID%3D1"
        if form["ACTION"] == "DELETE":
            logid = int(form["LOGIDS"])
            return b"RESULT=OK&COUNT=1" if book.pop(logid, None) else b"RESULT=FAIL&COUNT=0"
        assert form["ACTION"] == "INSERT"
        return self._insert(book, form)

    def _insert(self, book: dict[int, dict[str, str]], form: dict[str, str]) -> bytes:
        (record,) = parse_adi(form["ADIF"].encode()).document.records
        qso = {f.name.upper(): f.value for f in record.fields}
        if qso["QSO_DATE"] in self.refuse_dates:
            return b"RESULT=FAIL&REASON=QSO+date+is+outside+the+logbook+date+range&COUNT=0"
        match = (qso["CALL"], qso["QSO_DATE"], qso["TIME_ON"], qso["BAND"], qso["MODE"])
        for logid, have in book.items():
            if (
                have["CALL"],
                have["QSO_DATE"],
                have["TIME_ON"],
                have["BAND"],
                have["MODE"],
            ) == match:
                if form.get("OPTION") != "REPLACE":
                    return b"RESULT=FAIL&REASON=Unable+to+add+QSO+to+database%3A+duplicate&COUNT=0"
                book[logid] = qso
                return f"RESULT=REPLACE&LOGID={logid}&COUNT=1".encode()
        self.next_logid += 1
        book[self.next_logid] = qso
        return f"RESULT=OK&LOGIDS={self.next_logid}&COUNT=1".encode()


@pytest.fixture
def qrz() -> FakeLogbook:
    """A stand-in for QRZ.com's logbook service."""
    return FakeLogbook()


@pytest.fixture
def client(database: str, qrz: FakeLogbook) -> TestClient:
    """The Engine's API, reaching the stand-in logbook service."""
    return TestClient(create_app(conninfo(database), qrz_post=qrz))


@pytest.fixture
def sync(client: TestClient) -> LogbookSync:
    """The QRZ logbook sender, run by hand in these tests."""
    found: LogbookSync = client.app.state.logbook  # type: ignore[attr-defined]
    return found


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
    """W0XYZ's headers: view and log."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def _save(
    client: TestClient, headers: dict[str, str], key: str = KEY, book: str | None = None
) -> dict[str, Any]:
    body = {"api_key": key} if book is None else {"api_key": key, "book_call": book}
    response = client.put("/api/v1/logbook/qrz", headers=headers, json=body)
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def _log(client: TestClient, headers: dict[str, str], **change: str) -> str:
    fields = [f for f in W1AW if f["name"] not in change]
    fields += [{"name": k, "value": v} for k, v in change.items()]
    response = client.post("/api/v1/qsos", headers=headers, json={"fields": fields})
    assert response.status_code == 201, response.text
    return response.json()["qso"]["id"]  # type: ignore[no-any-return]


def _status(client: TestClient, headers: dict[str, str]) -> dict[str, Any]:
    return client.get("/api/v1/logbook/qrz", headers=headers).json()  # type: ignore[no-any-return]


def test_a_logged_qso_goes_to_the_operators_qrz_logbook(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """Saved key, then a QSO: it is sent, naming the logbook's callsign."""
    saved = _save(client, k9ksu)
    assert saved["message"].startswith("Saved. QRZ.com accepted the API key.")
    assert saved["book_call"] == "K9KSU"
    assert KEY not in client.get("/api/v1/logbook/qrz", headers=k9ksu).text
    _log(client, k9ksu)
    assert _status(client, k9ksu)["waiting"] == 1
    assert sync.run_once() is False
    (sent,) = qrz.books[KEY].values()
    assert sent["CALL"] == "W1AW"
    assert sent["STATION_CALLSIGN"] == "K9KSU"
    assert _status(client, k9ksu) | {"message": ""} == {
        "book_call": "K9KSU",
        "sent": 1,
        "waiting": 0,
        "refused": 0,
        "last_refusal": None,
        "problem": None,
        "message": "",
    }
    assert qrz.agents == {"LabLog/0.0.0"}  # QRZ.com asks every program to name itself
    sync.run_once()
    assert sum(1 for q in qrz.asked if q["ACTION"] == "INSERT") == 1  # sent once


def test_qsos_logged_before_the_key_was_saved_are_not_sent(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """Saving a key never floods QRZ.com with a log imported earlier."""
    _log(client, k9ksu)
    _save(client, k9ksu)
    sync.run_once()
    assert qrz.books[KEY] == {}
    assert _status(client, k9ksu)["waiting"] == 0


def test_with_no_internet_qsos_wait_and_logging_is_unchanged(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """Nothing is lost; everything goes when QRZ.com is back (Principle V)."""
    qrz.online = False
    assert "can't be reached" in _save(client, k9ksu)["message"]
    _log(client, k9ksu)
    _log(client, k9ksu, CALL="K0ABC")
    assert sync.run_once() is True
    status = _status(client, k9ksu)
    assert status["waiting"] == 2
    assert status["problem"] == "QRZ.com can't be reached right now."
    qrz.online = True
    assert sync.run_once() is False
    assert len(qrz.books[KEY]) == 2
    assert _status(client, k9ksu)["problem"] is None


def test_edits_deletes_and_restores_follow_the_log(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """QRZ.com ends up holding what the log holds now."""
    _save(client, k9ksu)
    qso_id = _log(client, k9ksu)
    sync.run_once()
    (first,) = qrz.books[KEY]

    # An edit removes LabLog's own copy and sends the QSO as it stands now.
    fields = [*W1AW, {"name": "NAME", "value": "Hiram"}]
    edited = client.put(
        f"/api/v1/qsos/{qso_id}", headers=k9ksu, json={"base_version": 1, "fields": fields}
    )
    assert edited.status_code == 200
    sync.run_once()
    ((second, qso),) = qrz.books[KEY].items()
    assert second != first
    assert qso["NAME"] == "Hiram"

    fields = [{**f, "value": "W1AX"} if f["name"] == "CALL" else f for f in fields]
    client.put(f"/api/v1/qsos/{qso_id}", headers=k9ksu, json={"base_version": 2, "fields": fields})
    sync.run_once()
    ((logid, qso),) = qrz.books[KEY].items()
    assert logid not in {first, second}
    assert qso["CALL"] == "W1AX"

    client.post(f"/api/v1/qsos/{qso_id}/delete", headers=k9ksu, json={})
    sync.run_once()
    assert qrz.books[KEY] == {}
    client.post(f"/api/v1/qsos/{qso_id}/restore", headers=k9ksu, json={})
    sync.run_once()
    assert [q["CALL"] for q in qrz.books[KEY].values()] == ["W1AX"]
    assert _status(client, k9ksu)["sent"] == 1


def test_qsos_qrz_got_another_way_are_never_touched(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """A QSO QRZ.com got from LoTW is never changed or removed, whatever LabLog does."""
    lotw = {
        "CALL": "K0ABC",
        "QSO_DATE": "20261018",
        "TIME_ON": "0136",
        "BAND": "20m",
        "MODE": "SSB",
        "APP_QRZLOG_STATUS": "C",
    }
    qrz.books[KEY][1] = dict(lotw)
    _save(client, k9ksu)
    # The same QSO typed into LabLog: QRZ.com says it has it, and keeps its own.
    same = _log(client, k9ksu, CALL="K0ABC")
    sync.run_once()
    assert qrz.books[KEY] == {1: lotw}
    assert _status(client, k9ksu)["refused"] == 1
    # A LabLog QSO edited to match it, then deleted: QRZ.com's copy stays as it was.
    other = _log(client, k9ksu, CALL="W1AW")
    sync.run_once()
    fields = [{**f, "value": "K0ABC"} if f["name"] == "CALL" else f for f in W1AW]
    client.put(f"/api/v1/qsos/{other}", headers=k9ksu, json={"base_version": 1, "fields": fields})
    sync.run_once()
    for qso_id in (same, other):
        client.post(f"/api/v1/qsos/{qso_id}/delete", headers=k9ksu, json={})
    sync.run_once()
    assert qrz.books[KEY] == {1: lotw}
    assert all(q.get("LOGIDS") != "1" for q in qrz.asked)


def test_a_qso_qrz_refuses_is_reported_and_not_sent_again(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """A refusal is shown to the operator; it is tried again only if the QSO changes."""
    _save(client, k9ksu)
    qrz.refuse_dates.add("20261018")
    _log(client, k9ksu)
    assert sync.run_once() is False
    status = _status(client, k9ksu)
    assert (status["sent"], status["refused"], status["waiting"]) == (0, 1, 0)
    assert "date range" in status["last_refusal"]
    sync.run_once()
    assert sum(1 for q in qrz.asked if q["ACTION"] == "INSERT") == 1


def test_a_club_logbook_gets_only_the_club_calls_qsos(
    client: TestClient,
    k9ksu: dict[str, str],
    w0xyz: dict[str, str],
    qrz: FakeLogbook,
    sync: LogbookSync,
) -> None:
    """A QSO goes to the logbook for its STATION_CALLSIGN, else its operator's own."""
    _save(client, k9ksu)
    assert _save(client, w0xyz, CLUB_KEY, "w0cka")["book_call"] == "W0CKA"
    taken = client.put(
        "/api/v1/logbook/qrz", headers=k9ksu, json={"api_key": KEY, "book_call": "W0CKA"}
    )
    assert taken.status_code == 409  # one key per logbook, so nothing is sent twice
    _log(client, k9ksu)
    _log(client, w0xyz, CALL="K0ABC", STATION_CALLSIGN="W0CKA")
    _log(client, w0xyz, CALL="K0DEF")  # W0XYZ's own: no logbook for W0XYZ
    sync.run_once()
    assert [q["CALL"] for q in qrz.books[KEY].values()] == ["W1AW"]
    assert [q["CALL"] for q in qrz.books[CLUB_KEY].values()] == ["K0ABC"]


def test_a_refused_key_is_reported(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """The operator hears at once, and QSOs wait for a good key."""
    saved = _save(client, k9ksu, key="WRONG-KEY-0000")
    assert "refused this API key" in saved["message"]
    _log(client, k9ksu)
    assert sync.run_once() is True
    assert "refused the API key" in _status(client, k9ksu)["problem"]
    _save(client, k9ksu)
    assert sync.run_once() is False
    assert len(qrz.books[KEY]) == 1


def test_removing_the_key_stops_sending(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeLogbook, sync: LogbookSync
) -> None:
    """With no key, nothing goes to QRZ.com."""
    _save(client, k9ksu)
    assert client.delete("/api/v1/logbook/qrz", headers=k9ksu).status_code == 204
    _log(client, k9ksu)
    sync.run_once()
    assert qrz.books[KEY] == {}
    assert _status(client, k9ksu)["book_call"] is None


@pytest.mark.parametrize(
    ("method", "body"),
    [("GET", None), ("PUT", {"api_key": KEY}), ("DELETE", None)],
)
def test_logbook_operations_need_sign_in(
    client: TestClient, method: str, body: dict[str, str] | None
) -> None:
    """Anonymous requests are refused (Principle XXI)."""
    assert client.request(method, "/api/v1/logbook/qrz", json=body).status_code == 401


@pytest.mark.parametrize(
    "body",
    [{"api_key": "short"}, {"api_key": "has spaces in it"}, {"api_key": KEY, "book_call": "W0/"}],
)
def test_a_bad_key_or_callsign_is_refused(
    client: TestClient, k9ksu: dict[str, str], body: dict[str, str]
) -> None:
    """Checked before anything is saved (Principle XII)."""
    assert client.put("/api/v1/logbook/qrz", headers=k9ksu, json=body).status_code == 422


def test_the_database_lets_only_the_signed_in_operator_change_a_key(
    conn: psycopg.Connection[TupleRow], identity: IdentityStore, shack: Actor
) -> None:
    """A key cannot be saved or removed from a session that has ended (Principle XXI)."""
    store = LogbookStore(conn)
    store.save(shack, KEY, "K9KSU")
    session = identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    identity.sign_out(session.token)
    with pytest.raises(NotAuthorizedError):
        store.save(session.actor, CLUB_KEY, "K9KSU")
    with pytest.raises(NotAuthorizedError):
        store.remove(session.actor)
    book = store.mine(shack)
    assert book is not None
    assert book.api_key == KEY
