# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""QRZ.com lookups through the API: optional (V), cached (VI), per operator (XIII)."""

from __future__ import annotations

import urllib.parse
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.callbook import QrzUnreachableError
from loglab_engine.store import IdentityStore, NotAuthorizedError, Permission
from loglab_engine.store.callbook import CallbookStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_api import W1AW

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

QRZ_PASSWORD = "qrz-pass"  # noqa: S105 - a stand-in QRZ.com account


class FakeQrz:
    """QRZ.com as its interface specification describes it, with one account."""

    def __init__(self) -> None:
        """Online, knowing W1AW, with no sessions yet."""
        self.online = True
        self.asked: list[dict[str, str]] = []
        self.keys: set[str] = set()

    def __call__(self, url: str) -> bytes:
        """Answer one request."""
        if not self.online:
            raise QrzUnreachableError("no internet")
        query = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(url).query))
        self.asked.append(query)
        if "username" in query:
            if query["username"] != "K9KSU" or query["password"] != QRZ_PASSWORD:
                return self._session("<Error>Username/password incorrect</Error>")
            key = f"key{len(self.keys) + 1}"
            self.keys.add(key)
            return self._session(f"<Key>{key}</Key>")
        if query.get("s") not in self.keys:
            return self._session("<Error>Session Timeout</Error>")
        call = query["callsign"].upper()
        key = f"<Key>{query['s']}</Key>"
        if call != "W1AW":
            return self._session(f"<Error>Not found: {call}</Error>{key}")
        record = (
            "<Callsign><call>W1AW</call><fname>Hiram Percy</fname><name>Maxim</name>"
            "<addr2>Newington</addr2><state>CT</state><country>United States</country>"
            "<grid>FN31pr</grid></Callsign>"
        )
        return f'<QRZDatabase xmlns="http://xmldata.qrz.com">{record}<Session>{key}</Session></QRZDatabase>'.encode()

    @staticmethod
    def _session(inner: str) -> bytes:
        return f'<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>{inner}</Session></QRZDatabase>'.encode()

    def lookups(self) -> int:
        """How many lookups (not logins) QRZ.com has been asked for."""
        return sum(1 for q in self.asked if "callsign" in q)


@pytest.fixture
def qrz() -> FakeQrz:
    """A stand-in for QRZ.com."""
    return FakeQrz()


@pytest.fixture
def client(database: str, qrz: FakeQrz) -> TestClient:
    """The Engine's API on this test's database, reaching the stand-in QRZ.com."""
    return TestClient(create_app(conninfo(database), qrz_fetch=qrz))


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
    """W0XYZ's headers: view and log, no QRZ.com login."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def _set_up(
    client: TestClient, headers: dict[str, str], password: str = QRZ_PASSWORD
) -> dict[str, Any]:
    response = client.put(
        "/api/v1/callbook/qrz", headers=headers, json={"username": "K9KSU", "password": password}
    )
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def _look(client: TestClient, headers: dict[str, str], call: str, **extra: str) -> dict[str, Any]:
    response = client.get("/api/v1/lookup", headers=headers, params={"call": call, **extra})
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def test_with_no_qrz_login_lookup_says_so_and_logging_is_unchanged(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz
) -> None:
    """QRZ.com is an enhancement; nothing depends on it (Principle V)."""
    found = _look(client, k9ksu, "w1aw")
    assert found["status"] == "not_set_up"
    assert found["name"] is None
    assert qrz.asked == []
    assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": W1AW}).status_code == 201


def test_a_lookup_fills_in_the_call_and_is_fetched_only_once(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz
) -> None:
    """The second lookup comes from the Engine's own copy (Principle VI)."""
    saved = _set_up(client, k9ksu)
    assert saved == {
        "username": "K9KSU",
        "status": None,
        "message": "Saved. QRZ.com accepted your login.",
    }
    first = _look(client, k9ksu, "w1aw")
    assert first["status"] == "found"
    assert (first["name"], first["qth"], first["state"], first["grid"]) == (
        "Hiram Percy Maxim",
        "Newington",
        "CT",
        "FN31pr",
    )
    assert first["cached"] is False
    assert first["fetched_at"].endswith("Z")  # UTC (Principle VIII)
    again = _look(client, k9ksu, "W1AW")
    assert again["cached"] is True
    assert again["name"] == "Hiram Percy Maxim"
    assert qrz.lookups() == 1
    # Not found is kept too.
    assert _look(client, k9ksu, "xx9xx")["status"] == "not_found"
    assert _look(client, k9ksu, "XX9XX")["cached"] is True
    assert qrz.lookups() == 2
    # Asking for a fresh copy fetches again.
    assert _look(client, k9ksu, "W1AW", refresh="true")["cached"] is False
    assert qrz.lookups() == 3


def test_with_no_internet_lookup_answers_quickly_and_logging_is_unchanged(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz
) -> None:
    """One failed try, then QRZ.com is left alone for a minute (Principle V)."""
    qrz.online = False
    saved = _set_up(client, k9ksu)
    assert saved["status"] == "unreachable"
    assert saved["message"] == "Saved. QRZ.com can't be reached right now to check it."
    qrz.online = True
    qrz.asked.clear()
    # The Engine marked QRZ.com down when the check failed, so this does not wait on it.
    result = _look(client, k9ksu, "W1AW")
    assert result["status"] == "unreachable"
    assert "Log as usual" in result["message"]
    assert qrz.asked == []
    assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": W1AW}).status_code == 201


def test_with_no_internet_a_fresh_copy_falls_back_to_the_kept_one(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz
) -> None:
    """What QRZ.com said before is still shown when it can't be reached."""
    _set_up(client, k9ksu)
    _look(client, k9ksu, "W1AW")
    qrz.online = False
    result = _look(client, k9ksu, "W1AW", refresh="true")
    assert result["status"] == "found"
    assert result["cached"] is True
    assert result["name"] == "Hiram Percy Maxim"
    assert "said before" in result["message"]


def test_a_wrong_password_is_saved_but_reported(client: TestClient, k9ksu: dict[str, str]) -> None:
    """The operator hears at once that QRZ.com refused it."""
    assert _set_up(client, k9ksu, password="wrong")["status"] == "login_failed"  # noqa: S106
    result = _look(client, k9ksu, "W1AW")
    assert result["status"] == "login_failed"
    assert "incorrect" in result["message"]


def test_an_expired_session_logs_in_again(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz
) -> None:
    """QRZ.com sessions end; the Engine logs in again once and carries on."""
    _set_up(client, k9ksu)
    qrz.keys.clear()
    assert _look(client, k9ksu, "W1AW")["status"] == "found"
    assert [("username" in q, "callsign" in q) for q in qrz.asked] == [
        (True, False),  # the check when the login was saved
        (False, True),  # with the old key: Session Timeout
        (True, False),  # log in again
        (False, True),  # found
    ]


def test_the_password_is_never_returned_and_logins_are_per_operator(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], qrz: FakeQrz
) -> None:
    """Each operator uses their own QRZ.com login; looked-up calls are shared."""
    _set_up(client, k9ksu)
    mine = client.get("/api/v1/callbook/qrz", headers=k9ksu)
    assert mine.json() == {
        "username": "K9KSU",
        "status": None,
        "message": "Using QRZ.com as K9KSU.",
    }
    assert QRZ_PASSWORD not in mine.text
    assert client.get("/api/v1/callbook/qrz", headers=w0xyz).json()["username"] is None
    assert _look(client, w0xyz, "K0ABC")["status"] == "not_set_up"
    _look(client, k9ksu, "W1AW")
    assert _look(client, w0xyz, "W1AW")["name"] == "Hiram Percy Maxim"
    assert qrz.lookups() == 1


def test_an_operator_can_remove_their_login(client: TestClient, k9ksu: dict[str, str]) -> None:
    """Removing it turns lookups off for that operator."""
    _set_up(client, k9ksu)
    assert client.delete("/api/v1/callbook/qrz", headers=k9ksu).status_code == 204
    assert client.get("/api/v1/callbook/qrz", headers=k9ksu).json()["username"] is None
    assert _look(client, k9ksu, "W1AW")["status"] == "not_set_up"


@pytest.mark.parametrize(
    ("method", "path", "body"),
    [
        ("GET", "/api/v1/lookup?call=W1AW", None),
        ("GET", "/api/v1/callbook/qrz", None),
        ("PUT", "/api/v1/callbook/qrz", {"username": "K9KSU", "password": "x"}),
        ("DELETE", "/api/v1/callbook/qrz", None),
    ],
)
def test_callbook_operations_need_sign_in(
    client: TestClient, method: str, path: str, body: dict[str, str] | None
) -> None:
    """Anonymous requests are refused (Principle XIII)."""
    assert client.request(method, path, json=body).status_code == 401


@pytest.mark.parametrize("call", ["", "W1AW;DROP", "W" * 21, "W1AW//4", "<b>"])
def test_a_bad_call_is_refused(client: TestClient, k9ksu: dict[str, str], call: str) -> None:
    """The call is validated before anything else (Principle XII)."""
    response = client.get("/api/v1/lookup", headers=k9ksu, params={"call": call})
    assert response.status_code == 422


def test_the_database_lets_only_the_signed_in_operator_change_a_login(
    conn: psycopg.Connection[TupleRow], identity: IdentityStore, shack: Actor
) -> None:
    """A login cannot be saved or removed from a session that has ended (Principle XIII)."""
    store = CallbookStore(conn)
    store.save_login(shack, "qrz", "K9KSU", QRZ_PASSWORD)
    session = identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    identity.sign_out(session.token)
    with pytest.raises(NotAuthorizedError):
        store.save_login(session.actor, "qrz", "K9KSU", "other")
    with pytest.raises(NotAuthorizedError):
        store.remove_login(session.actor, "qrz")
    login = store.login(shack, "qrz")
    assert login is not None
    assert login.password == QRZ_PASSWORD
