# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The US and Canadian license lists: offline lookups (V, VI), loaded atomically (XIV)."""

from __future__ import annotations

import io
from datetime import UTC, date, datetime
from typing import TYPE_CHECKING, Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.cli import engine
from loglab_engine.reference.licenses import LicenseFileError
from loglab_engine.reference.refresh import (
    FCC_DAILY_URL,
    FCC_FULL_URL,
    ISED_URL,
    Refresher,
    UnreachableError,
)
from loglab_engine.store.licenses import Kind, LicenseStore, Source

from ..reference.uls import am, en, fcc_zip, hd, ised, ised_zip
from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo
from .test_callbook import QRZ_PASSWORD, FakeQrz

if TYPE_CHECKING:
    from pathlib import Path

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor, IdentityStore

TODAY = date(2026, 10, 19)
SUNDAY = datetime(2026, 10, 18, 12, 0, tzinfo=UTC)
MONDAY = datetime(2026, 10, 19, 12, 0, tzinfo=UTC)
TUESDAY = datetime(2026, 10, 20, 12, 0, tzinfo=UTC)


def _weekly(tmp_path: Path) -> Path:
    return fcc_zip(
        tmp_path / "l_amat.zip",
        [
            hd("1", "K9KSU"),
            hd("2", "W9AB"),
            hd("3", "K1ABC", "E", "05/01/2025"),
            hd("4", "K1CAN", "C"),
        ],
        [
            en("1", "K9KSU", "BRIAN", "KRENZIN", city="SAINT LOUIS", state="MO"),
            en("2", "W9AB", "JOHN", "SMITH", city="PEORIA", state="IL"),
            en("3", "K1ABC", "ANN", "JONES"),
            en("4", "K1CAN", "OLD", "HOLDER"),
        ],
        [am("1", "K9KSU", "E"), am("2", "W9AB", "G"), am("3", "K1ABC", "T")],
    )


def _canada(tmp_path: Path) -> Path:
    return ised_zip(
        tmp_path / "amateur_delim.zip",
        [ised("VE3ABC", "JEAN", "TREMBLAY", "OTTAWA", province="ON", qualifications="AD")],
    )


@pytest.fixture
def licenses(conn: psycopg.Connection[TupleRow]) -> LicenseStore:
    """The license lists on this test's database, empty."""
    return LicenseStore(conn)


def test_the_weekly_list_loads_what_fills_in_a_qso(licenses: LicenseStore, tmp_path: Path) -> None:
    """Active and recently expired licenses are kept; cancelled ones are not."""
    result = licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY, published=SUNDAY)
    assert (result.records, result.skipped) == (3, 0)
    found = licenses.find("K9KSU")
    assert found is not None
    assert (found.source, found.name, found.city, found.state, found.license_class) == (
        Source.FCC,
        "Brian Krenzin",
        "Saint Louis",
        "MO",
        "Extra",
    )
    expired = licenses.find("K1ABC")
    assert expired is not None
    assert not expired.active
    assert licenses.find("K1CAN") is None
    assert licenses.latest(Source.FCC, Kind.FULL) is not None


def test_a_new_weekly_list_replaces_the_old_one(licenses: LicenseStore, tmp_path: Path) -> None:
    """A license gone from the list is gone from the Engine."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    newer = fcc_zip(tmp_path / "newer.zip", [hd("2", "W9AB")], [en("2", "W9AB", "JOHN", "SMITH")])
    licenses.import_fcc(newer, Kind.FULL, today=TODAY)
    assert licenses.find("K9KSU") is None
    assert licenses.find("W9AB") is not None


def test_a_daily_change_file_adds_changes_and_removes_licenses(
    licenses: LicenseStore, tmp_path: Path
) -> None:
    """New calls appear, moves and upgrades show, and cancelled licenses go."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    daily = fcc_zip(
        tmp_path / "l_am_mon.zip",
        [hd("2", "W9AB", "C"), hd("5", "KD9NEW")],
        [
            en("5", "KD9NEW", "NEW", "HAM", city="CHICAGO", state="IL"),
            en("1", "K9KSU", "BRIAN", "KRENZIN", city="ST CHARLES", state="MO"),
        ],
        [am("5", "KD9NEW", "T"), am("1", "K9KSU", "A")],
    )
    result = licenses.import_fcc(daily, Kind.DAILY, today=TODAY, published=MONDAY)
    assert result.records == 2
    assert licenses.find("W9AB") is None
    new = licenses.find("KD9NEW")
    assert new is not None
    assert (new.name, new.city, new.license_class) == (
        "New Ham",
        "Chicago",
        "Technician",
    )
    moved = licenses.find("K9KSU")
    assert moved is not None
    assert (moved.city, moved.license_class) == ("St Charles", "Advanced")
    assert licenses.applied_since_full(Source.FCC) == {("l_am_mon.zip", MONDAY)}


def test_a_mostly_unreadable_file_is_refused_and_nothing_changes(
    licenses: LicenseStore, tmp_path: Path
) -> None:
    """The list already loaded stays as it was (Principles XII and XIV)."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    bad = fcc_zip(tmp_path / "bad.zip", [hd("9", "W1AW")] + ["HD|x"] * 200)
    with pytest.raises(LicenseFileError, match="could not be read"):
        licenses.import_fcc(bad, Kind.FULL, today=TODAY)
    empty = fcc_zip(tmp_path / "empty.zip", [hd("4", "K1CAN", "C")])
    with pytest.raises(LicenseFileError, match="no current licenses"):
        licenses.import_fcc(empty, Kind.FULL, today=TODAY)
    not_zip = tmp_path / "not.zip"
    not_zip.write_bytes(b"<html>maintenance</html>")
    with pytest.raises(LicenseFileError, match="not a readable zip"):
        licenses.import_fcc(not_zip, Kind.FULL, today=TODAY)
    assert licenses.find("K9KSU") is not None
    assert licenses.find("W1AW") is None
    assert len([s for s in licenses.states() if s.calls]) == 1


def test_the_canadian_list_loads_and_replaces(licenses: LicenseStore, tmp_path: Path) -> None:
    """ISED's list replaces the Canadian calls only."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    assert licenses.import_ised(_canada(tmp_path)).records == 1
    found = licenses.find("VE3ABC")
    assert found is not None
    assert (found.source, found.name, found.state, found.license_class) == (
        Source.ISED,
        "Jean Tremblay",
        "ON",
        "Advanced",
    )
    licenses.import_ised(ised_zip(tmp_path / "next.zip", [ised("VA3XYZ", "AL", "BEE")]))
    assert licenses.find("VE3ABC") is None
    assert licenses.find("K9KSU") is not None


# -- Lookups through the API --------------------------------------------------


@pytest.fixture
def qrz() -> FakeQrz:
    """A stand-in for QRZ.com."""
    return FakeQrz()


@pytest.fixture
def client(database: str, qrz: FakeQrz) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(
        create_app(conninfo(database), qrz_fetch=qrz, license_downloads=FakeAgencies())
    )


def _sign_in(client: TestClient, callsign: str, secret: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions", json={"callsign": callsign, "secret": secret, "station": "shack"}
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the administrator."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET)


def _look(client: TestClient, headers: dict[str, str], call: str) -> dict[str, Any]:
    response = client.get("/api/v1/lookup", headers=headers, params={"call": call})
    assert response.status_code == 200, response.text
    return response.json()  # type: ignore[no-any-return]


def test_with_no_internet_and_no_qrz_a_us_call_fills_in_from_the_fcc_list(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, qrz: FakeQrz, tmp_path: Path
) -> None:
    """US callsign lookup is a core offline function (Principle V)."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    qrz.online = False
    found = _look(client, k9ksu, "k9ksu")
    assert found["status"] == "found"
    assert (found["name"], found["qth"], found["state"], found["country"]) == (
        "Brian Krenzin",
        "Saint Louis",
        "MO",
        "United States",
    )
    assert found["grid"] is None
    assert found["sources"] == ["fcc"]
    assert found["license_class"] == "Extra"
    assert found["message"] == "Filled in from the FCC's license list."
    assert qrz.asked == []


def test_qrz_fills_in_only_what_the_license_list_lacks(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """Online lookups supplement the local data; they never replace it (Principle VI)."""
    licenses.import_fcc(
        fcc_zip(
            tmp_path / "w1aw.zip", [hd("7", "W1AW")], [en("7", "W1AW", entity="ARRL INC", city="")]
        ),
        Kind.FULL,
        today=TODAY,
    )
    response = client.put(
        "/api/v1/callbook/qrz", headers=k9ksu, json={"username": "K9KSU", "password": QRZ_PASSWORD}
    )
    assert response.status_code == 200
    found = _look(client, k9ksu, "W1AW")
    assert found["name"] == "ARRL INC"  # the FCC's, not QRZ.com's "Hiram Percy Maxim"
    assert found["qth"] == "Newington"  # the FCC lists no city; QRZ.com's fills the gap
    assert found["grid"] == "FN31pr"
    assert found["sources"] == ["fcc", "qrz"]
    assert found["message"] == "Filled in from the FCC's license list and QRZ.com."


def test_an_expired_license_says_so(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """Within the renewal grace period it still fills in, with a note."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    found = _look(client, k9ksu, "K1ABC")
    assert found["status"] == "found"
    assert found["license_expires"] == "2025-05-01"
    assert "expired on 2025-05-01" in found["message"]


def test_a_license_in_its_grace_period_says_it_has_expired(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """The FCC lists such a license as active until the grace period ends."""
    licenses.import_fcc(
        fcc_zip(tmp_path / "grace.zip", [hd("8", "K1GRC", "A", "05/01/2025")]),
        Kind.FULL,
        today=TODAY,
    )
    found = _look(client, k9ksu, "K1GRC")
    assert found["status"] == "found"
    assert "expired on 2025-05-01" in found["message"]


def test_a_canadian_call_fills_in_from_canadas_list(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """With no QRZ.com login at all."""
    licenses.import_ised(_canada(tmp_path))
    found = _look(client, k9ksu, "VE3ABC")
    assert (found["name"], found["qth"], found["state"], found["country"]) == (
        "Jean Tremblay",
        "Ottawa",
        "ON",
        "Canada",
    )
    assert found["sources"] == ["ised"]


def test_a_call_in_neither_list_is_answered_as_before(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """QRZ.com alone, for calls outside the US and Canada."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    found = _look(client, k9ksu, "G4ABC")
    assert found["status"] == "not_set_up"
    assert found["sources"] == []


def test_the_licenses_page_says_how_current_each_list_is_and_credits_it(
    client: TestClient, k9ksu: dict[str, str], licenses: LicenseStore, tmp_path: Path
) -> None:
    """Every list's source and publication date are shown (Principle VI)."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY, published=SUNDAY)
    response = client.get("/api/v1/licenses", headers=k9ksu)
    assert response.status_code == 200, response.text
    fcc, canada = response.json()["sources"]
    assert (fcc["source"], fcc["calls"], fcc["published"]) == ("fcc", 3, "2026-10-18T12:00:00Z")
    assert "public domain" in fcc["credit"]
    assert (canada["source"], canada["calls"], canada["imported"]) == ("ised", 0, None)
    assert "Open Government Licence" in canada["credit"]
    assert client.get("/static/licenses.js").status_code == 200
    assert client.get("/static/licenses.css").status_code == 200


def test_only_an_admin_can_ask_for_a_refresh(
    client: TestClient, k9ksu: dict[str, str], identity: IdentityStore, shack: Actor
) -> None:
    """Checking now is an admin's call; seeing the lists is anyone's."""
    from loglab_engine.store import Permission  # noqa: PLC0415 - only this test needs it

    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    visitor = _sign_in(client, "W0XYZ", VISITOR_SECRET)
    assert client.get("/api/v1/licenses", headers=visitor).status_code == 200
    assert client.post("/api/v1/licenses/refresh", headers=visitor).status_code == 403
    assert client.post("/api/v1/licenses/refresh", headers=k9ksu).status_code == 202


# -- The scheduled refresh ----------------------------------------------------


class FakeAgencies:
    """The FCC's and ISED's download sites: files with publication dates, or no internet."""

    def __init__(self) -> None:
        """Online, publishing nothing yet."""
        self.online = True
        self.files: dict[str, tuple[Path, datetime | None]] = {}
        self.fetched: list[str] = []

    def publish(self, url: str, path: Path, when: datetime | None) -> None:
        """Put ``path`` up at ``url``, published at ``when``."""
        self.files[url] = (path, when)

    def published(self, url: str) -> datetime | None:
        """When ``url`` was published."""
        if not self.online:
            raise UnreachableError("no internet")
        if url not in self.files:
            raise UnreachableError(f"{url}: 404")
        return self.files[url][1]

    def fetch(self, url: str, dest: Path, cap: int) -> datetime | None:
        """Copy the published file."""
        if not self.online or url not in self.files:
            raise UnreachableError("no internet")
        path, when = self.files[url]
        assert path.stat().st_size <= cap
        dest.write_bytes(path.read_bytes())
        self.fetched.append(url)
        return when


def _daily(day: str) -> str:
    return FCC_DAILY_URL.format(day=day)


def test_the_refresh_loads_each_new_file_once_in_order(
    database: str, licenses: LicenseStore, tmp_path: Path
) -> None:
    """The weekly list, then each newer day's changes, then nothing until something is newer."""
    agencies = FakeAgencies()
    agencies.publish(FCC_FULL_URL, _weekly(tmp_path), SUNDAY)
    agencies.publish(ISED_URL, _canada(tmp_path), SUNDAY)
    older = fcc_zip(tmp_path / "sat.zip", [hd("1", "K9KSU", "C")])
    agencies.publish(_daily("sat"), older, datetime(2026, 10, 17, tzinfo=UTC))  # in the weekly
    tuesday = fcc_zip(tmp_path / "tue.zip", [hd("6", "KD9TUE")], [en("6", "KD9TUE", "TUE", "DAY")])
    monday = fcc_zip(tmp_path / "mon.zip", [hd("5", "KD9MON")], [en("5", "KD9MON", "MON", "DAY")])
    agencies.publish(_daily("tue"), tuesday, TUESDAY)
    agencies.publish(_daily("mon"), monday, MONDAY)
    refresher = Refresher(conninfo(database), agencies, today=lambda: TODAY)

    report = refresher.run_once()
    assert [(r.source, r.kind) for r in report.imported] == [
        (Source.FCC, Kind.FULL),
        (Source.FCC, Kind.DAILY),
        (Source.FCC, Kind.DAILY),
        (Source.ISED, Kind.FULL),
    ]
    assert agencies.fetched == [FCC_FULL_URL, _daily("mon"), _daily("tue"), ISED_URL]
    assert licenses.find("K9KSU") is not None  # Saturday's change was already in the weekly list
    assert licenses.find("KD9MON") is not None
    assert licenses.find("KD9TUE") is not None
    assert licenses.find("VE3ABC") is not None
    # Five days have no file at the stand-in: each is reported, and nothing else is.
    assert len(report.problems) == 4

    agencies.fetched.clear()
    again = refresher.run_once()
    assert again.imported == []
    assert agencies.fetched == []

    agencies.publish(_daily("tue"), tuesday, datetime(2026, 10, 27, tzinfo=UTC))
    assert [r.kind for r in refresher.run_once().imported] == [Kind.DAILY]


def test_with_no_internet_the_refresh_says_so_and_the_list_stays(
    database: str, licenses: LicenseStore, tmp_path: Path
) -> None:
    """Lookups keep using the list already loaded."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY)
    agencies = FakeAgencies()
    agencies.online = False
    report = Refresher(conninfo(database), agencies, today=lambda: TODAY).run_once()
    assert report.imported == []
    assert report.problems
    assert all("no internet" in p for p in report.problems)
    assert licenses.find("K9KSU") is not None


def test_a_bad_download_is_reported_and_the_list_stays(
    database: str, licenses: LicenseStore, tmp_path: Path
) -> None:
    """A maintenance page served in place of the zip changes nothing."""
    licenses.import_fcc(_weekly(tmp_path), Kind.FULL, today=TODAY, published=SUNDAY)
    page = tmp_path / "page.zip"
    page.write_bytes(b"<html>down for maintenance</html>")
    agencies = FakeAgencies()
    agencies.publish(FCC_FULL_URL, page, TUESDAY)
    report = Refresher(conninfo(database), agencies, today=lambda: TODAY).run_once()
    assert any("not a readable zip" in p for p in report.problems)
    assert licenses.find("K9KSU") is not None


# -- The command line ---------------------------------------------------------


def _engine(database: str, *argv: str) -> tuple[int, str, str]:
    out, err = io.StringIO(), io.StringIO()
    code = engine.main(["--database", conninfo(database), *argv], out, err)
    return code, out.getvalue(), err.getvalue()


def test_an_offline_station_loads_the_lists_by_hand(database: str, tmp_path: Path) -> None:
    """Files carried in on a USB stick load the same way."""
    code, out, _ = _engine(database, "licenses", "status")
    assert code == engine.EXIT_OK
    assert "United States: 0 calls, never loaded" in out
    daily = fcc_zip(tmp_path / "l_am_mon.zip", [hd("5", "KD9NEW")])
    code, out, err = _engine(
        database,
        "licenses",
        "import",
        "--fcc",
        str(_weekly(tmp_path)),
        "--fcc-daily",
        str(daily),
        "--canada",
        str(_canada(tmp_path)),
    )
    assert code == engine.EXIT_OK, err
    assert out.splitlines() == [
        "loaded 3 licenses in the United States list",
        "loaded 1 changes to the United States list",
        "loaded 1 licenses in the Canada list",
    ]
    code, out, _ = _engine(database, "licenses", "status")
    assert "United States: 4 calls, loaded" in out
    assert "changes applied" in out
    assert "Canada: 1 calls, loaded" in out


def test_a_bad_file_is_refused_on_the_command_line(database: str, tmp_path: Path) -> None:
    """Exit code 2, with the reason."""
    bad = tmp_path / "l_amat.zip"
    bad.write_text("not a zip")
    code, _, err = _engine(database, "licenses", "import", "--fcc", str(bad))
    assert code == engine.EXIT_REFUSED
    assert "not a readable zip" in err
    code, _, err = _engine(database, "licenses", "import")
    assert code == engine.EXIT_REFUSED
