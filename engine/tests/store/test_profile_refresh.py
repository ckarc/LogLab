# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Kept QRZ.com profiles re-checked in the background: off Starlink, gently (V, VI, XIII)."""

from __future__ import annotations

from datetime import timedelta
from typing import TYPE_CHECKING

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.callbook.profiles import Network, NetworkSeen, ProfileRefresher, RefreshReport
from loglab_engine.store import NotAuthorizedError
from loglab_engine.store.callbook import CallbookStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_callbook import JPEG, FakeQrz, _bio, _look, _set_up, _sign_in

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

NEW_PHOTO = "https://cdn-xml.qrz.com/u/w1aw/w1aw-2026.jpg"
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 40


class FakeNetwork:
    """Which network the Engine's internet comes through, as the test says."""

    def __init__(self) -> None:
        """The shop's internet."""
        self.seen = NetworkSeen(Network.OTHER, "203.0.113.7", "AS64500 Shop Fiber")
        self.asked = 0

    def __call__(self) -> NetworkSeen:
        """Say which network it is."""
        self.asked += 1
        return self.seen


@pytest.fixture
def qrz() -> FakeQrz:
    """A stand-in for QRZ.com."""
    return FakeQrz()


@pytest.fixture
def network() -> FakeNetwork:
    """A stand-in for the network check."""
    return FakeNetwork()


@pytest.fixture
def client(database: str, qrz: FakeQrz, network: FakeNetwork) -> TestClient:
    """The Engine's API, reaching the stand-in QRZ.com through the stand-in network."""
    app = create_app(
        conninfo(database), qrz_fetch=qrz, qrz_fetch_large=qrz.large, profile_network=network
    )
    app.state.profiles = ProfileRefresher(
        conninfo(database), app.state.qrz, network, pause=lambda _seconds: None
    )
    return TestClient(app)


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the administrator, with a QRZ.com login."""
    del shack
    headers = _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")
    _set_up(client, headers)
    return headers


def _kept(client: TestClient, headers: dict[str, str]) -> None:
    """W1AW looked up, with its biography and photo kept."""
    _look(client, headers, "W1AW")
    assert _bio(client, headers, "W1AW")["text"] == "First licensed in 1914."
    photo = client.get("/api/v1/lookup/photo", headers=headers, params={"call": "W1AW"})
    assert photo.content == JPEG


def _age(conn: psycopg.Connection[TupleRow], days: int = 31) -> None:
    with conn.transaction():
        conn.execute("ALTER TABLE reference.callbook_entry DISABLE TRIGGER callbook_stamp")
        conn.execute(
            "UPDATE reference.callbook_entry SET fetched_at = now() - make_interval(days => %s)",
            (days,),
        )
        conn.execute("ALTER TABLE reference.callbook_entry ENABLE TRIGGER callbook_stamp")


def _run(client: TestClient) -> RefreshReport:
    refresher: ProfileRefresher = client.app.state.profiles  # type: ignore[attr-defined]
    return refresher.run_once()


def _downloads(qrz: FakeQrz) -> tuple[int, int]:
    """How many biographies and photos QRZ.com has been asked for."""
    bios = sum(1 for q in qrz.asked if "html" in q)
    photos = sum(1 for q in qrz.asked if "photo" in q)
    return bios, photos


def test_an_unchanged_profile_costs_one_small_lookup_and_nothing_else(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz, conn: psycopg.Connection[TupleRow]
) -> None:
    """The biography and photo are not downloaded again when QRZ.com says they are the same."""
    _kept(client, k9ksu)
    _age(conn)
    lookups, downloads = qrz.lookups(), _downloads(qrz)
    report = _run(client)
    assert (report.network, report.checked, report.bios, report.photos) == (Network.OTHER, 1, 0, 0)
    assert qrz.lookups() == lookups + 1
    assert _downloads(qrz) == downloads
    # Now fresh: the next run asks QRZ.com nothing.
    assert _run(client).message == "Every kept profile is up to date."
    assert qrz.lookups() == lookups + 1


def test_a_changed_biography_and_photo_are_fetched_with_no_one_asking(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz, conn: psycopg.Connection[TupleRow]
) -> None:
    """A newer biography date, or a different photo, brings the new one in."""
    _kept(client, k9ksu)
    _age(conn)
    qrz.bio, qrz.bio_date = "Now on FT8 too.", "2026-10-19 01:00:00"
    qrz.image, qrz.photo = NEW_PHOTO, PNG
    report = _run(client)
    assert (report.checked, report.bios, report.photos) == (1, 1, 1)
    asked = len(qrz.asked)
    assert _bio(client, k9ksu, "W1AW")["text"] == "Now on FT8 too."
    photo = client.get("/api/v1/lookup/photo", headers=k9ksu, params={"call": "W1AW"})
    assert (photo.headers["content-type"], photo.content) == ("image/png", PNG)
    assert len(qrz.asked) == asked  # shown from the kept copies


def test_only_profiles_with_a_kept_biography_or_photo_are_rechecked(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz, conn: psycopg.Connection[TupleRow]
) -> None:
    """A call merely looked up is re-checked when someone looks it up again, not here."""
    _look(client, k9ksu, "W1AW")
    _look(client, k9ksu, "XX9XX")
    _age(conn)
    lookups = qrz.lookups()
    assert _run(client).message == "Every kept profile is up to date."
    assert qrz.lookups() == lookups


def test_on_starlink_nothing_is_rechecked_unless_an_admin_says_always(
    client: TestClient,
    k9ksu: dict[str, str],
    qrz: FakeQrz,
    network: FakeNetwork,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Starlink is metered in the field: the default pauses on it."""
    _kept(client, k9ksu)
    _age(conn)
    network.seen = NetworkSeen(Network.STARLINK, "98.97.16.1", "AS14593 Space Exploration")
    lookups = qrz.lookups()
    report = _run(client)
    assert (report.network, report.checked) == (Network.STARLINK, 0)
    assert report.message == "Paused: the internet is Starlink."
    assert qrz.lookups() == lookups
    network.seen = NetworkSeen(Network.UNKNOWN)
    assert _run(client).message == "Paused: LabLog can't tell which network this is."
    saved = client.put("/api/v1/callbook/refresh", headers=k9ksu, json={"mode": "always"})
    assert saved.status_code == 200, saved.text
    assert (saved.json()["mode"], saved.json()["changed_by"]) == ("always", "K9KSU")
    network.seen = NetworkSeen(Network.STARLINK, "98.97.16.1", "AS14593 Space Exploration")
    assert _run(client).checked == 1
    assert qrz.lookups() == lookups + 1


def test_off_and_no_internet_ask_nothing_and_lookups_still_work(
    client: TestClient,
    k9ksu: dict[str, str],
    qrz: FakeQrz,
    network: FakeNetwork,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Turned off it does nothing; with no internet it tries nothing (Principle V)."""
    _kept(client, k9ksu)
    _age(conn)
    client.put("/api/v1/callbook/refresh", headers=k9ksu, json={"mode": "off"})
    asked, checks = len(qrz.asked), network.asked
    assert _run(client).message == "Turned off."
    assert (len(qrz.asked), network.asked) == (asked, checks)
    client.put("/api/v1/callbook/refresh", headers=k9ksu, json={"mode": "auto"})
    network.seen = NetworkSeen(Network.NONE)
    assert _run(client).message == "No internet right now."
    assert len(qrz.asked) == asked
    qrz.online = False
    assert _look(client, k9ksu, "W1AW")["name"] == "Hiram Percy Maxim"


def test_qrz_going_quiet_mid_check_stops_the_run_and_keeps_the_copies(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz, conn: psycopg.Connection[TupleRow]
) -> None:
    """The kept profile stays as it was, and waits for the next check."""
    _kept(client, k9ksu)
    _age(conn)
    qrz.online = False
    report = _run(client)
    assert report.checked == 0
    assert "stopped answering" in report.message
    store = CallbookStore(conn)
    assert store.kept_profiles_due("qrz", timedelta(days=30), 10) == ["W1AW"]
    assert _bio(client, k9ksu, "W1AW")["text"] == "First licensed in 1914."


def test_the_engine_uses_an_accepted_login_of_its_own_session(
    client: TestClient, k9ksu: dict[str, str], qrz: FakeQrz, conn: psycopg.Connection[TupleRow]
) -> None:
    """No operator is signed in for it; it logs in to QRZ.com itself."""
    _kept(client, k9ksu)
    _age(conn)
    logins = sum(1 for q in qrz.asked if "username" in q)
    assert _run(client).checked == 1
    assert sum(1 for q in qrz.asked if "username" in q) == logins + 1


def test_only_an_admin_changes_the_setting_and_anyone_can_see_it(
    client: TestClient,
    k9ksu: dict[str, str],
    visitor: Actor,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Who may change it is the database's rule too (Principle XIII)."""
    w0xyz = _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")
    shown = client.get("/api/v1/callbook/refresh", headers=w0xyz)
    assert shown.status_code == 200
    assert shown.json()["mode"] == "auto"
    refused = client.put("/api/v1/callbook/refresh", headers=w0xyz, json={"mode": "always"})
    assert refused.status_code == 403
    bad = client.put("/api/v1/callbook/refresh", headers=k9ksu, json={"mode": "sometimes"})
    assert bad.status_code == 422
    assert client.get("/api/v1/callbook/refresh").status_code == 401
    with pytest.raises(NotAuthorizedError):
        CallbookStore(conn).save_refresh_mode(visitor, "always")
    assert CallbookStore(conn).refresh_setting().mode == "auto"
