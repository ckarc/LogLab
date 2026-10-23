# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The POTA park list: offline lookups (V, VI), loaded atomically (XIV), fetched frugally."""

from __future__ import annotations

import io
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.cli import engine
from loglab_engine.reference.parks import ParkFileError
from loglab_engine.reference.refresh import PARKS_URL, Refresher
from loglab_engine.store.parks import ParkStore

from ..reference.test_parks import parks_csv
from .conftest import VISITOR_SECRET, conninfo
from .test_licenses import FakeAgencies, _sign_in

if TYPE_CHECKING:
    from pathlib import Path

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor, IdentityStore

MONDAY = datetime(2026, 10, 19, 12, 0, tzinfo=UTC)
TUESDAY = datetime(2026, 10, 20, 12, 0, tzinfo=UTC)

ROWS = (
    '"US-0001","Acadia National Park","1","291","US-ME","44.31","-68.2034","FN54vh"',
    '"US-1562","Devil\'s Lake State Park","1","291","US-WI","43.42","-89.73","EN43ok"',
    '"US-4567","Old Lake Trail","0","291","US-WI,US-IL","42.5","-89.1","EN52"',
    '"VE-0042","Algonquin Provincial Park","1","1","CA-ON","45.58","-78.35","FN05"',
)


def _list(tmp_path: Path, *rows: str) -> Path:
    return parks_csv(tmp_path / "all_parks_ext.csv", *(rows or ROWS))


@pytest.fixture
def parks(conn: psycopg.Connection[TupleRow]) -> ParkStore:
    """The park list on this test's database, empty."""
    return ParkStore(conn)


def test_the_list_loads_and_a_park_is_found_by_reference(parks: ParkStore, tmp_path: Path) -> None:
    """Any spelling of the reference finds it."""
    result = parks.import_file(_list(tmp_path), published=MONDAY)
    assert (result.records, result.skipped) == (4, 0)
    found = parks.find(" us-1562")
    assert found is not None
    assert (found.name, found.locations, found.grid) == (
        "Devil's Lake State Park",
        "US-WI",
        "EN43OK",
    )
    assert parks.find("US-9999") is None
    assert parks.find("nonsense") is None
    state = parks.state()
    assert (state.parks, state.published, state.file_name) == (4, MONDAY, "all_parks_ext.csv")


def test_search_puts_references_then_name_starts_then_the_rest(
    parks: ParkStore, tmp_path: Path
) -> None:
    """Active parks come before inactive ones; % and _ are only letters."""
    parks.import_file(_list(tmp_path))
    assert [p.reference for p in parks.search("us-15")] == ["US-1562"]
    assert [p.reference for p in parks.search("lake")] == ["US-1562", "US-4567"]
    assert [p.reference for p in parks.search("old lake")] == ["US-4567"]
    assert [p.reference for p in parks.search("ALGONQUIN")] == ["VE-0042"]
    assert parks.search("%") == []
    assert parks.search("  ") == []


def test_a_new_list_replaces_the_old_one(parks: ParkStore, tmp_path: Path) -> None:
    """A park gone from the list is gone from the Engine."""
    parks.import_file(_list(tmp_path))
    parks.import_file(_list(tmp_path, ROWS[0]))
    assert parks.find("US-0001") is not None
    assert parks.find("US-1562") is None


def test_a_mostly_unreadable_list_is_refused_and_nothing_changes(
    parks: ParkStore, tmp_path: Path
) -> None:
    """The list already loaded stays as it was (Principles XII and XIV)."""
    parks.import_file(_list(tmp_path))
    bad = parks_csv(tmp_path / "bad.csv", ROWS[0], *(['"junk","x"'] * 60))
    with pytest.raises(ParkFileError, match="could not be read"):
        parks.import_file(bad)
    page = tmp_path / "page.csv"
    page.write_text("<html>maintenance</html>")
    with pytest.raises(ParkFileError, match="not a park list"):
        parks.import_file(page)
    nothing = parks_csv(tmp_path / "none.csv")
    with pytest.raises(ParkFileError, match="no parks"):
        parks.import_file(nothing)
    assert parks.state().parks == 4


# -- The API ------------------------------------------------------------------


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database), license_downloads=FakeAgencies()))


@pytest.fixture
def visitor(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """An operator who may only view."""
    from loglab_engine.store import Permission  # noqa: PLC0415 - only these tests need it

    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET)


def test_parks_are_looked_up_with_no_internet(
    client: TestClient, visitor: dict[str, str], parks: ParkStore, tmp_path: Path
) -> None:
    """Search, one park by reference, and how current the list is (Principles V and VI)."""
    parks.import_file(_list(tmp_path), published=MONDAY)
    found = client.get("/api/v1/parks", headers=visitor, params={"q": "lake"})
    assert found.status_code == 200, found.text
    assert [p["reference"] for p in found.json()["parks"]] == ["US-1562", "US-4567"]
    one = client.get("/api/v1/parks/us-4567", headers=visitor)
    assert one.status_code == 200
    assert one.json() == {
        "reference": "US-4567",
        "name": "Old Lake Trail",
        "active": False,
        "locations": ["US-WI", "US-IL"],
        "latitude": 42.5,
        "longitude": -89.1,
        "grid": "EN52",
    }
    assert client.get("/api/v1/parks/US-9999", headers=visitor).status_code == 404
    state = client.get("/api/v1/park-list", headers=visitor).json()
    assert (state["parks"], state["published"]) == (4, "2026-10-19T12:00:00Z")
    assert "pota.app" in state["credit"]
    assert client.get("/api/v1/parks", params={"q": "lake"}).status_code == 401
    assert client.get("/static/parks.js").status_code == 200
    assert client.get("/static/parks.css").status_code == 200


def test_only_an_admin_can_ask_for_a_park_list_check(
    client: TestClient, visitor: dict[str, str]
) -> None:
    """Seeing the list is anyone's; checking now is an admin's call."""
    assert client.post("/api/v1/park-list/refresh", headers=visitor).status_code == 403


# -- The scheduled refresh ----------------------------------------------------


class Clock:
    """A clock the test moves."""

    def __init__(self, now: datetime) -> None:
        """Start at ``now``."""
        self.now = now

    def __call__(self) -> datetime:
        """What time it is."""
        return self.now


def test_the_list_is_fetched_once_and_again_only_when_newer(
    database: str, parks: ParkStore, tmp_path: Path
) -> None:
    """pota.app's Last-Modified date decides (Principle V: fetch each thing once)."""
    site = FakeAgencies()
    site.publish(PARKS_URL, _list(tmp_path), MONDAY)
    clock = Clock(MONDAY)
    refresher = Refresher(conninfo(database), site, now=clock)
    report = refresher.run_once()
    assert [p.records for p in report.parks] == [4]
    assert parks.find("US-0001") is not None
    site.fetched.clear()
    clock.now += timedelta(days=30)
    assert refresher.run_once().parks == []
    assert PARKS_URL not in site.fetched
    site.publish(PARKS_URL, _list(tmp_path, ROWS[0]), TUESDAY)
    assert [p.records for p in refresher.run_once().parks] == [1]


def test_with_no_date_from_pota_app_the_list_is_fetched_weekly(
    database: str, tmp_path: Path
) -> None:
    """A server that gives no Last-Modified is asked again after a week, not every check."""
    site = FakeAgencies()
    site.publish(PARKS_URL, _list(tmp_path), None)
    clock = Clock(datetime.now(UTC))
    refresher = Refresher(conninfo(database), site, now=clock)
    assert len(refresher.run_once().parks) == 1
    clock.now += timedelta(days=6)
    assert refresher.run_once().parks == []
    clock.now += timedelta(days=2)
    assert len(refresher.run_once().parks) == 1


def test_on_a_metered_link_nothing_is_fetched(
    database: str, parks: ParkStore, tmp_path: Path
) -> None:
    """On Starlink or the phone, the park list waits for the shop line."""
    site = FakeAgencies()
    site.publish(PARKS_URL, _list(tmp_path), MONDAY)
    refresher = Refresher(conninfo(database), site, hold=lambda: "the internet is Starlink")
    report = refresher.run_once()
    assert report.parks == []
    assert report.park_problems == ["the internet is Starlink"]
    assert site.fetched == []
    assert parks.state().parks == 0


def test_a_bad_download_is_reported_apart_and_the_list_stays(
    database: str, parks: ParkStore, tmp_path: Path
) -> None:
    """The park list's problem is its own, not the license lists'."""
    parks.import_file(_list(tmp_path), published=MONDAY)
    page = tmp_path / "page.csv"
    page.write_text("<html>maintenance</html>")
    site = FakeAgencies()
    site.publish(PARKS_URL, page, TUESDAY)
    report = Refresher(conninfo(database), site).run_once()
    assert any("not a park list" in p for p in report.park_problems)
    assert not any("park" in p for p in report.problems)
    assert parks.state().parks == 4


# -- The command line ---------------------------------------------------------


def _engine(database: str, *argv: str) -> tuple[int, str, str]:
    out, err = io.StringIO(), io.StringIO()
    code = engine.main(["--database", conninfo(database), *argv], out, err)
    return code, out.getvalue(), err.getvalue()


def test_an_offline_station_loads_the_park_list_by_hand(database: str, tmp_path: Path) -> None:
    """A file carried in on a USB stick loads the same way."""
    code, out, _ = _engine(database, "parks", "status")
    assert (code, out) == (engine.EXIT_OK, "Parks on the Air: 0 parks, never loaded\n")
    code, out, err = _engine(database, "parks", "import", str(_list(tmp_path)))
    assert code == engine.EXIT_OK, err
    assert out == "loaded 4 parks\n"
    code, out, _ = _engine(database, "parks", "status")
    assert out.startswith("Parks on the Air: 4 parks, loaded ")
    bad = tmp_path / "bad.csv"
    bad.write_text("not a park list")
    code, _, err = _engine(database, "parks", "import", str(bad))
    assert code == engine.EXIT_REFUSED
    assert "not a park list" in err
