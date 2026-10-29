# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Engine's HTTP API: signed in (XXI), validated (XII), audited (VII), UTC (VIII)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import psycopg
import pytest
from fastapi.testclient import TestClient
from psycopg.types.json import Jsonb

from loglab_engine.api import create_app
from loglab_engine.store import IdentityStore, Permission

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from loglab_engine.store import Actor

W1AW = [
    {"name": "CALL", "value": "W1AW"},
    {"name": "QSO_DATE", "value": "20261018"},
    {"name": "TIME_ON", "value": "0136"},
    {"name": "BAND", "value": "20m"},
    {"name": "MODE", "value": "SSB"},
    {"name": "RST_RCVD", "value": "57"},
]


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


def _sign_in(client: TestClient, callsign: str, secret: str, station: str) -> dict[str, str]:
    response = client.post(
        "/api/v1/sessions",
        json={"callsign": callsign, "secret": secret, "station": station},
    )
    assert response.status_code == 201, response.text
    return {"Authorization": f"Bearer {response.json()['token']}"}


@pytest.fixture
def k9ksu(client: TestClient, shack: Actor) -> dict[str, str]:
    """K9KSU's headers: the first administrator, signed in through the API."""
    del shack  # enrolled K9KSU
    return _sign_in(client, "k9ksu", ADMIN_SECRET, "shack")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log, at a Field Day position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


def test_status_names_the_source_without_signing_in(client: TestClient) -> None:
    """Anyone on the network can find the source (AGPL-3.0, Licensing)."""
    response = client.get("/api/v1/status")
    assert response.status_code == 200
    assert response.json()["source"] == "https://github.com/ckarc/loglab"
    assert response.json()["license"] == "AGPL-3.0-only"


def test_the_web_client_is_served_with_a_strict_policy(client: TestClient) -> None:
    """The page and its files load only from the Engine itself (Principle V)."""
    page = client.get("/")
    assert page.status_code == 200
    assert "<title>LabLog</title>" in page.text
    assert "default-src 'self'" in page.headers["content-security-policy"]
    assert client.get("/static/app.js").status_code == 200
    assert client.get("/static/app.css").status_code == 200
    assert client.get("/static/lookup.js").status_code == 200
    assert client.get("/static/lookup.css").status_code == 200
    assert client.get("/static/overlay.js").status_code == 200
    assert client.get("/static/overlay.css").status_code == 200
    assert client.get("/static/profile.js").status_code == 200
    assert client.get("/static/profile.css").status_code == 200
    assert client.get("/static/lab-stock.js").status_code == 200
    assert client.get("/static/lab-stock.css").status_code == 200


@pytest.mark.parametrize(
    ("method", "path"),
    [
        ("GET", "/api/v1/qsos"),
        ("POST", "/api/v1/qsos"),
        ("GET", "/api/v1/sessions/current"),
        ("GET", "/api/v1/qsos/00000000-0000-0000-0000-000000000000/history"),
    ],
)
def test_everything_else_needs_a_signed_in_operator(
    client: TestClient,
    method: str,
    path: str,
) -> None:
    """No token, a malformed one, or a made-up one: refused (Principle XXI)."""
    for headers in ({}, {"Authorization": "Basic abc"}, {"Authorization": "Bearer made-up"}):
        response = client.request(method, path, headers=headers, json={"fields": W1AW})
        assert response.status_code == 401, (headers, response.text)


def test_sign_in_and_out(client: TestClient, shack: Actor) -> None:
    """A wrong secret is 401; a session ends when signed out."""
    del shack
    wrong = client.post(
        "/api/v1/sessions",
        json={"callsign": "K9KSU", "secret": "000000", "station": "shack"},
    )
    assert wrong.status_code == 401
    assert wrong.json()["detail"] == "wrong callsign or secret"
    headers = _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")
    me = client.get("/api/v1/sessions/current", headers=headers).json()
    assert me["operator"] == "K9KSU"
    assert me["permissions"] == ["view", "log", "radio", "admin"]
    assert client.delete("/api/v1/sessions/current", headers=headers).status_code == 204
    assert client.get("/api/v1/sessions/current", headers=headers).status_code == 401


def test_enter_edit_delete_restore_and_read_history(
    client: TestClient,
    k9ksu: dict[str, str],
    w0xyz: dict[str, str],
) -> None:
    """The whole audited life of a QSO through the API, by two operators."""
    created = client.post("/api/v1/qsos", headers=k9ksu, json={"fields": W1AW})
    assert created.status_code == 201, created.text
    body = created.json()
    assert body["created"] is True
    assert body["message"] == "Logged W1AW, by K9KSU at shack."  # committed: Principle XV
    qso = body["qso"]
    assert (qso["call"], qso["band"], qso["mode"]) == ("W1AW", "20m", "SSB")
    assert qso["qso_start"] == "2026-10-18T01:36:00Z"  # UTC, Principle VIII
    assert (qso["operator"], qso["station"]) == ("K9KSU", "shack")
    path = f"/api/v1/qsos/{qso['id']}"

    edited_fields = [*W1AW[:-1], {"name": "RST_RCVD", "value": "59"}]
    edited = client.put(
        path,
        headers=w0xyz,
        json={"base_version": 1, "fields": edited_fields, "note": "misheard"},
    )
    assert edited.status_code == 200, edited.text
    assert edited.json()["qso"]["version"] == 2
    stale = client.put(path, headers=k9ksu, json={"base_version": 1, "fields": W1AW})
    assert stale.status_code == 409

    assert client.post(f"{path}/delete", headers=w0xyz, json={"note": "dupe"}).status_code == 200
    listed = client.get("/api/v1/qsos", headers=k9ksu).json()
    assert listed == []
    listed = client.get("/api/v1/qsos?include_deleted=true", headers=k9ksu).json()
    assert [q["deleted"] for q in listed] == [True]
    assert client.post(f"{path}/restore", headers=k9ksu, json={}).status_code == 200

    history = client.get(f"{path}/history", headers=w0xyz).json()
    assert [(v["action"], v["operator"], v["station"], v["note"]) for v in history] == [
        ("create", "K9KSU", "shack", None),
        ("edit", "W0XYZ", "fd-2", "misheard"),
        ("delete", "W0XYZ", "fd-2", "dupe"),
        ("restore", "K9KSU", "shack", None),
    ]
    assert history[0]["fields"][-1] == {"name": "RST_RCVD", "value": "57", "type": None}
    assert all(v["changed_at"].endswith("Z") for v in history)


def test_a_journal_can_send_a_qso_again_without_duplicating_it(
    client: TestClient,
    k9ksu: dict[str, str],
    w0xyz: dict[str, str],
) -> None:
    """The client's ID makes a re-send harmless; a different QSO under it is refused (XV)."""
    qso_id = "6f1c2b8e-4d3a-4e2f-9a1b-0c5d7e8f9a10"
    first = client.post("/api/v1/qsos", headers=k9ksu, json={"id": qso_id, "fields": W1AW})
    assert first.status_code == 201, first.text
    assert first.json()["qso"]["id"] == qso_id
    again = client.post("/api/v1/qsos", headers=w0xyz, json={"id": qso_id, "fields": W1AW})
    assert again.status_code == 200, again.text
    assert again.json()["created"] is False
    assert again.json()["message"] == "Already logged: W1AW. Nothing was added."
    assert again.json()["qso"]["operator"] == "K9KSU"  # the first entry stands
    other = [*W1AW[:-1], {"name": "RST_RCVD", "value": "59"}]
    clash = client.post("/api/v1/qsos", headers=k9ksu, json={"id": qso_id, "fields": other})
    assert clash.status_code == 409
    assert "different QSO" in clash.json()["detail"]
    assert len(client.get("/api/v1/qsos", headers=k9ksu).json()) == 1
    history = client.get(f"/api/v1/qsos/{qso_id}/history", headers=k9ksu).json()
    assert [v["action"] for v in history] == ["create"]


def test_listing_is_newest_first_and_filters_by_call(
    client: TestClient,
    k9ksu: dict[str, str],
) -> None:
    """The latest QSOs come first; a call search ignores case and spaces."""
    for call in ("Q1A", "Q2B", "Q3C"):
        fields = [{"name": "CALL", "value": call}, *W1AW[1:]]
        assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": fields}).is_success
    calls = [q["call"] for q in client.get("/api/v1/qsos?limit=2", headers=k9ksu).json()]
    assert calls == ["Q3C", "Q2B"]
    found = client.get("/api/v1/qsos", headers=k9ksu, params={"call": " q2b "}).json()
    assert [q["call"] for q in found] == ["Q2B"]


def test_permissions_are_enforced(
    client: TestClient,
    identity: IdentityStore,
    shack: Actor,
    w0xyz: dict[str, str],
) -> None:
    """Without log you can read but not write; without view you cannot read."""
    identity.set_permissions(shack, "W0XYZ", [Permission.LOG], granted=False)
    refused = client.post("/api/v1/qsos", headers=w0xyz, json={"fields": W1AW})
    assert refused.status_code == 403
    assert client.get("/api/v1/qsos", headers=w0xyz).status_code == 200
    identity.set_permissions(shack, "W0XYZ", [Permission.VIEW], granted=False)
    assert client.get("/api/v1/qsos", headers=w0xyz).status_code == 403


@pytest.mark.parametrize(
    ("body", "fragment"),
    [
        ({"fields": []}, "at least 1"),
        ({"fields": [{"name": "CALL", "value": "W1AW", "extra": 1}]}, "Extra inputs"),
        ({"fields": [{"name": "CA<LL", "value": "W1AW"}]}, "forbidden character"),
        ({"fields": [{"name": "CALL", "value": 5}]}, "valid string"),
        ({"fields": [{"name": "CALL", "value": "x" * 70000}]}, "at most 65536"),
        ({"fields": [{"name": "CALL", "value": "W1AW"}] * 501}, "at most 500"),
        ({"fields": W1AW, "note": "n" * 1001}, "at most 1000"),
        ({"fields": [{"name": "CALL", "value": "a\x00b"}]}, "NUL"),
        ({"id": "not-a-uuid", "fields": W1AW}, "valid UUID"),
    ],
)
def test_bad_input_is_refused_at_the_boundary(
    client: TestClient,
    k9ksu: dict[str, str],
    body: dict[str, object],
    fragment: str,
) -> None:
    """Every request body is validated before it reaches the log (Principle XII)."""
    response = client.post("/api/v1/qsos", headers=k9ksu, json=body)
    assert response.status_code == 422
    assert fragment in response.text
    assert client.get("/api/v1/qsos", headers=k9ksu).json() == []


def test_unknown_qsos_are_404(client: TestClient, k9ksu: dict[str, str]) -> None:
    """A QSO that does not exist is reported as such."""
    missing = "/api/v1/qsos/00000000-0000-0000-0000-000000000000"
    assert client.get(missing, headers=k9ksu).status_code == 404
    assert client.post(f"{missing}/delete", headers=k9ksu, json={}).status_code == 404


LAYOUT = {
    "version": 1,
    "modules": [
        {"id": "log", "width": "full", "side": "left", "scale": 100, "hidden": False},
        {"id": "entry", "width": "half", "side": "right", "scale": 85, "hidden": False},
        {"id": "journal", "width": "half", "side": "left", "scale": 100, "hidden": True},
        {"id": "map", "width": "third", "side": "middle", "scale": 150, "hidden": False},
        {"id": "profile", "width": "quarter", "side": "middle-right", "scale": 60, "hidden": False},
        {"id": "stats", "width": "quarter", "side": "right", "scale": 100, "hidden": False},
        {"id": "world", "width": "two-thirds", "side": "right", "scale": 100, "hidden": False},
    ],
    "tables": {
        "profile": {"columns": ["date", "band", "mode"], "sort": "qso_start", "descending": True},
        "log": {
            "columns": ["call", "date", "field:FREQ", "band"],
            "sort": "field:FREQ",
            "descending": False,
        },
    },
    "panels": {"map.to-work": "right"},
    "detail": {"fields": ["CALL", "QSO_DATE", "NAME", "MY_GRIDSQUARE", "APP_LOGLAB_X"]},
    "theme": "dark",
    "operation": None,
    "by_overlay": {},
    "world_qsos": 120,
    "bio_open": True,
    "stats_open": False,
    "expanded": ["stats", "log"],
    "auto_expand": ["profile"],
    "distance_unit": "km",
    "log_mine": False,
}


def test_a_layout_belongs_to_the_operator_wherever_they_sign_in(
    client: TestClient,
    k9ksu: dict[str, str],
    w0xyz: dict[str, str],
) -> None:
    """Saved under the operator, it comes back at another station; others are unaffected."""
    assert client.get("/api/v1/layout", headers=k9ksu).json() == {"layout": None}
    saved = client.put("/api/v1/layout", headers=k9ksu, json=LAYOUT)
    assert saved.status_code == 200, saved.text
    elsewhere = _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-3")
    assert client.get("/api/v1/layout", headers=elsewhere).json() == {"layout": LAYOUT}
    assert client.get("/api/v1/layout", headers=w0xyz).json() == {"layout": None}
    changed = {
        "version": 1,
        "modules": [{"id": "log", "width": "full", "side": "left", "scale": 100, "hidden": False}],
    }
    assert client.put("/api/v1/layout", headers=elsewhere, json=changed).status_code == 200
    assert client.get("/api/v1/layout", headers=k9ksu).json() == {
        "layout": {**changed, "tables": {}, "panels": {}, "detail": None, "theme": "auto"}
        | {"operation": None, "by_overlay": {}, "world_qsos": 50}
        | {
            "bio_open": False,
            "stats_open": False,
            "expanded": [],
            "auto_expand": ["stats", "profile", "onair"],
            "distance_unit": "mi",
        }
        | {"log_mine": True}
    }


@pytest.mark.parametrize(
    ("body", "fragment"),
    [
        ({"version": 1, "modules": []}, "at least 1"),
        ({"version": 2, "modules": [{"id": "log"}]}, "version"),
        ({"version": 1, "modules": [{"id": "radar"}]}, "id"),
        ({"version": 1, "modules": [{"id": "log", "width": "wide"}]}, "width"),
        ({"version": 1, "modules": [{"id": "log", "width": "half", "side": "middle"}]}, "side"),
        ({"version": 1, "modules": [{"id": "log", "width": "full", "side": "centre"}]}, "side"),
        (
            {"version": 1, "modules": [{"id": "log", "width": "third", "side": "middle-left"}]},
            "only a quarter",
        ),
        ({"version": 1, "modules": [{"id": "log", "width": "quarter", "side": "middle"}]}, "side"),
        (
            {"version": 1, "modules": [{"id": "log", "width": "two-thirds", "side": "middle"}]},
            "side",
        ),
        ({"version": 1, "modules": [{"id": "log", "scale": 49}]}, "scale"),
        ({"version": 1, "modules": [{"id": "log", "scale": 201}]}, "scale"),
        ({"version": 1, "modules": [{"id": "log", "scale": 1.5}]}, "scale"),
        ({"version": 1, "modules": [{"id": "log"}, {"id": "log"}]}, "only once"),
        ({"version": 1, "modules": [{"id": "log", "x": 1}]}, "Extra inputs"),
        ({"version": 1, "modules": [{"id": "log"}], "world_qsos": 0}, "world_qsos"),
        ({"version": 1, "modules": [{"id": "log"}], "world_qsos": 501}, "world_qsos"),
        ({"version": 1, "modules": [{"id": "log"}], "bio_open": "yes"}, "bio_open"),
        ({"version": 1, "modules": [{"id": "log"}], "stats_open": "yes"}, "stats_open"),
        ({"version": 1, "modules": [{"id": "log"}], "expanded": ["map"]}, "expanded"),
        ({"version": 1, "modules": [{"id": "log"}], "auto_expand": ["map"]}, "auto_expand"),
        ({"version": 1, "modules": [{"id": "log"}], "distance_unit": "nm"}, "distance_unit"),
        (
            {"version": 1, "modules": [{"id": "log"}], "tables": {"radar": {"columns": ["call"]}}},
            "tables",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "tables": {"log": {"columns": []}}},
            "at least 1",
        ),
        (
            {
                "version": 1,
                "modules": [{"id": "log"}],
                "tables": {"log": {"columns": ["call", "CALL"]}},
            },
            "pattern",
        ),
        (
            {
                "version": 1,
                "modules": [{"id": "log"}],
                "tables": {"log": {"columns": ["field:FREQ", "field:freq"]}},
            },
            "only once",
        ),
        (
            {
                "version": 1,
                "modules": [{"id": "log"}],
                "tables": {"log": {"columns": ["call"], "sort": "1; drop"}},
            },
            "pattern",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "panels": {"map.radar": "left"}},
            "panels",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "panels": {"map.to-work": "middle"}},
            "panels",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "detail": {"fields": ["NAME", "name"]}},
            "only once",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "detail": {"fields": ["MY GRID"]}},
            "pattern",
        ),
        (
            {"version": 1, "modules": [{"id": "log"}], "detail": {"fields": ["CALL"] * 81}},
            "at most 80",
        ),
        ({"version": 1, "modules": [{"id": "log"}], "theme": "night"}, "theme"),
    ],
)
def test_a_bad_layout_is_refused(
    client: TestClient,
    k9ksu: dict[str, str],
    body: dict[str, object],
    fragment: str,
) -> None:
    """Layouts are checked at the boundary like everything else (Principle XII)."""
    response = client.put("/api/v1/layout", headers=k9ksu, json=body)
    assert response.status_code == 422
    assert fragment in response.text
    assert client.get("/api/v1/layout", headers=k9ksu).json() == {"layout": None}


def test_a_layout_needs_a_signed_in_operator(client: TestClient) -> None:
    """No session, no layout (Principle XXI)."""
    assert client.get("/api/v1/layout").status_code == 401
    assert client.put("/api/v1/layout", json=LAYOUT).status_code == 401


def _calls(client: TestClient, headers: dict[str, str], **params: str) -> list[str | None]:
    return [q["call"] for q in client.get("/api/v1/qsos", headers=headers, params=params).json()]


def test_the_whole_log_sorts_by_any_column(client: TestClient, k9ksu: dict[str, str]) -> None:
    """The Engine sorts the log, not just the rows on the page (Principle II)."""
    for call, band, freq, time in (
        ("K1B", "2m", "146.52", "0300"),
        ("K1C", "160m", "1.84", "0100"),
        ("K1A", "20m", "14.2", "0200"),
        ("K1D", "70cm", "", "0000"),
    ):
        fields = [
            {"name": "CALL", "value": call},
            {"name": "QSO_DATE", "value": "20261018"},
            {"name": "TIME_ON", "value": time},
            {"name": "BAND", "value": band},
            {"name": "MODE", "value": "FM"},
        ]
        if freq:
            fields.append({"name": "FREQ", "value": freq})
        assert client.post("/api/v1/qsos", headers=k9ksu, json={"fields": fields}).is_success
    assert _calls(client, k9ksu) == ["K1D", "K1A", "K1C", "K1B"]
    assert _calls(client, k9ksu, sort="call", newest_first="false") == ["K1A", "K1B", "K1C", "K1D"]
    assert _calls(client, k9ksu, sort="call") == ["K1D", "K1C", "K1B", "K1A"]
    # Bands go by frequency, not by their names' spelling.
    assert _calls(client, k9ksu, sort="band", newest_first="false") == ["K1C", "K1A", "K1B", "K1D"]
    assert _calls(client, k9ksu, sort="qso_start", newest_first="false") == [
        "K1D",
        "K1C",
        "K1A",
        "K1B",
    ]
    # A number sorts as a number; a QSO without the field comes last either way.
    assert _calls(client, k9ksu, sort="field:freq", newest_first="false") == [
        "K1C",
        "K1A",
        "K1B",
        "K1D",
    ]
    assert _calls(client, k9ksu, sort="field:FREQ") == ["K1B", "K1A", "K1C", "K1D"]
    # The sort covers the whole log before the limit is applied.
    assert _calls(client, k9ksu, sort="call", newest_first="false", limit="1") == ["K1A"]


@pytest.mark.parametrize("sort", ["dxcc", "field:", "field:FREQ;", "call DESC", "field:1X"])
def test_an_unknown_sort_is_refused(client: TestClient, k9ksu: dict[str, str], sort: str) -> None:
    """Only known columns and ADIF field names can sort the log (Principle XII)."""
    assert client.get("/api/v1/qsos", headers=k9ksu, params={"sort": sort}).status_code == 422


def test_a_layout_keeps_each_overlays_arrangement(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """How the page was arranged in Field Day comes back after a spell of everyday logging."""
    kept = {"modules": [{"id": "map", "width": "half", "side": "right"}]}
    layout = {"modules": [{"id": "log"}], "by_overlay": {"arrl-fd": kept}}
    assert client.put("/api/v1/layout", headers=k9ksu, json=layout).status_code == 200
    saved = client.get("/api/v1/layout", headers=k9ksu).json()["layout"]
    assert saved["by_overlay"]["arrl-fd"]["modules"][0]["side"] == "right"
    bad = {"modules": [{"id": "log"}], "by_overlay": {"Not An Id": kept}}
    assert client.put("/api/v1/layout", headers=k9ksu, json=bad).status_code == 422


def test_a_layout_saved_with_the_ft8_boxes_keeps_everything_else(
    client: TestClient, k9ksu: dict[str, str], database: str
) -> None:
    """The FT8 box and waterfall are gone; a layout saved while they existed still opens."""
    assert client.put("/api/v1/layout", headers=k9ksu, json=LAYOUT).status_code == 200
    ft8 = {"id": "ft8", "width": "half", "side": "right"}
    log_table = {"columns": ["call", "band"], "sort": "call", "descending": False}
    old = {
        "modules": [{"id": "log", "width": "half", "side": "left"}, {"id": "waterfall"}, ft8],
        "tables": {"log": log_table, "ft8": {"columns": ["call"]}},
        "by_overlay": {"arrl-fd": {"modules": [ft8, {"id": "map"}]}},
    }
    with psycopg.connect(conninfo(database), autocommit=True) as conn:
        conn.execute(
            "INSERT INTO workspace.layout (operator_id, session_id, layout)"
            " SELECT operator_id, session_id, %s FROM workspace.layout",
            [Jsonb(old)],
        )
    saved = client.get("/api/v1/layout", headers=k9ksu).json()["layout"]
    assert [m["id"] for m in saved["modules"]] == ["log"]
    assert saved["modules"][0]["width"] == "half"
    assert saved["tables"] == {"log": log_table}
    assert [m["id"] for m in saved["by_overlay"]["arrl-fd"]["modules"]] == ["map"]
