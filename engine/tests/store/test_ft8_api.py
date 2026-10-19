# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""FT8 heard by each station's Agent (Principles X, XIII, XX), and practice operations."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app
from loglab_engine.ft8 import COLUMNS, STALE, Decode, Ft8Board, Parsed, parse_message
from loglab_engine.store import IdentityStore, LogStore, Permission
from loglab_engine.store.agent import AgentKeyStore
from loglab_engine.store.qrz_logbook import Logbook, LogbookStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    import httpx2 as httpx
    from psycopg.rows import TupleRow

    from loglab_engine.store import Actor

SLOT = 117_333_333  # 2025-10-09 08:53:15 UTC
ROW = "18" * COLUMNS


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
    """K9KSU's headers: an administrator at the shack's first position."""
    del shack
    return _sign_in(client, "K9KSU", ADMIN_SECRET, "fd-1")


@pytest.fixture
def w0xyz(client: TestClient, identity: IdentityStore, shack: Actor) -> dict[str, str]:
    """W0XYZ's headers: view and log only, at the second position."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    return _sign_in(client, "W0XYZ", VISITOR_SECRET, "fd-2")


@pytest.fixture
def agent(conn: psycopg.Connection[TupleRow], shack: Actor) -> dict[str, str]:
    """The headers of the Agent at fd-1, with a key K9KSU made for it."""
    return {"Authorization": f"Bearer {AgentKeyStore(conn).make(shack, 'fd-1')}"}


def _report(**parts: object) -> dict[str, Any]:
    return {"agent": "loglab-agent 0.1.0", "source": "pretend", "dial_hz": 14_074_000, **parts}


DECODES = {
    "slot": SLOT,
    "decodes": [
        {"message": "CQ K1ABC FN42", "snr_db": -12.4, "dt_s": 0.21, "base_hz": 1200.25},
        {"message": "K1ABC W9XYZ EN52", "snr_db": 3.0, "dt_s": -0.1, "base_hz": 640.0},
        {"message": "W9XYZ K1ABC RR73", "snr_db": -20.0, "dt_s": 0.3, "base_hz": 1200.0},
    ],
}


# -- Reading messages ------------------------------------------------------------


@pytest.mark.parametrize(
    ("message", "parsed"),
    [
        ("CQ K1ABC FN42", Parsed("K1ABC", None, "", "FN42", None, roger=False, ending=None)),
        ("CQ DX K1ABC FN42", Parsed("K1ABC", None, "DX", "FN42", None, roger=False, ending=None)),
        ("CQ POTA W9XYZ", Parsed("W9XYZ", None, "POTA", None, None, roger=False, ending=None)),
        (
            "K1ABC W9XYZ EN52",
            Parsed("W9XYZ", "K1ABC", None, "EN52", None, roger=False, ending=None),
        ),
        ("W9XYZ K1ABC -12", Parsed("K1ABC", "W9XYZ", None, None, "-12", roger=False, ending=None)),
        ("K1ABC W9XYZ R+05", Parsed("W9XYZ", "K1ABC", None, None, "+05", roger=True, ending=None)),
        (
            "W9XYZ K1ABC RR73",
            Parsed("K1ABC", "W9XYZ", None, None, None, roger=False, ending="RR73"),
        ),
        ("K1ABC W9XYZ 73", Parsed("W9XYZ", "K1ABC", None, None, None, roger=False, ending="73")),
        ("K1ABC W9XYZ", Parsed("W9XYZ", "K1ABC", None, None, None, roger=False, ending=None)),
    ],
)
def test_standard_messages_say_who_sent_them(message: str, parsed: Parsed) -> None:
    """The sender is the second call; a grid or report is what it says."""
    assert parse_message(message) == parsed


@pytest.mark.parametrize("message", ["", "TNX BOB 73 GL", "CQ", "<...> W9XYZ -10", "CQ CQ CQ"])
def test_anything_else_is_left_unread(message: str) -> None:
    """Free text and hashed calls are not guessed at (Principle XX)."""
    assert parse_message(message).de is None


# -- The board ----------------------------------------------------------------


def test_the_board_hands_out_only_what_is_new() -> None:
    """A browser asks after the last number it has and gets only newer rows and slots."""
    board = Ft8Board()
    now = datetime(2026, 10, 19, 15, 0, tzinfo=UTC)
    common: dict[str, Any] = {"agent": "a", "source": "pretend", "dial_hz": 14_074_000}
    board.report("fd-1", now=now, rows=(SLOT, 0, (ROW, ROW)), **common)
    first = board.heard("fd-1", now)
    assert first is not None
    assert [r.row for r in first.rows] == [0, 1]
    board.report("fd-1", now=now, rows=(SLOT, 2, (ROW,)), **common)
    board.report(
        "fd-1",
        now=now,
        decodes=(SLOT, (Decode("CQ K1ABC FN42", -3, 0, 900, "received"),)),
        **common,
    )
    later = board.heard("fd-1", now, after=first.seq)
    assert later is not None
    assert [r.row for r in later.rows] == [2]
    assert [d.message for s in later.slots for d in s.decodes] == ["CQ K1ABC FN42"]
    # A new dial frequency starts afresh: nothing heard before is shown as heard there.
    board.report("fd-1", now=now, rows=(SLOT + 1, 0, (ROW,)), **{**common, "dial_hz": 7_074_000})
    moved = board.heard("fd-1", now)
    assert moved is not None
    assert (len(moved.rows), moved.slots) == (1, ())
    # An Agent that goes quiet is no longer listening.
    assert board.heard("fd-1", now + STALE + timedelta(seconds=1)) is None
    assert board.heard("fd-2", now) is None


# -- The API ------------------------------------------------------------------


def test_the_agent_reports_and_its_station_sees_the_decodes(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str], w0xyz: dict[str, str]
) -> None:
    """Rows and decodes reach the Agent's own station, labelled received and pretend."""
    assert client.get("/api/v1/ft8", headers=k9ksu).json()["listening"] is False
    waterfall = {"slot": SLOT, "first_row": 0, "rows": [ROW, ROW]}
    response = client.put("/api/v1/agent/ft8", headers=agent, json=_report(waterfall=waterfall))
    assert response.status_code == 204, response.text
    response = client.put("/api/v1/agent/ft8", headers=agent, json=_report(decodes=DECODES))
    assert response.status_code == 204, response.text

    heard = client.get("/api/v1/ft8", headers=k9ksu).json()
    assert heard["listening"] is True
    assert (heard["source"], heard["dial_hz"], heard["band"]) == ("pretend", 14_074_000, "20m")
    assert (heard["columns"], heard["column_hz"], heard["rows_per_second"]) == (960, 3.125, 4)
    assert [r["row"] for r in heard["rows"]] == [0, 1]
    (slot,) = heard["slots"]
    assert slot["starts_at"] == "2025-10-09T08:53:15Z"
    # Strongest first, every one received and pretend (Principle XX).
    decodes = slot["decodes"]
    assert [d["message"] for d in decodes] == [
        "K1ABC W9XYZ EN52",
        "CQ K1ABC FN42",
        "W9XYZ K1ABC RR73",
    ]
    assert {d["kind"] for d in decodes} == {"received"}
    assert all(d["pretend"] for d in decodes)
    cq = decodes[1]
    assert (cq["de"], cq["to"], cq["grid"], cq["snr_db"], cq["base_hz"]) == (
        "K1ABC",
        None,
        "FN42",
        -12,
        1200,
    )
    assert cq["freq_hz"] == 14_075_200
    # Only what is new comes next time.
    again = client.get(f"/api/v1/ft8?after={heard['seq']}", headers=k9ksu).json()
    assert (again["rows"], again["slots"]) == ([], [])
    # Another station hears nothing from this Agent (Principle IX).
    assert client.get("/api/v1/ft8", headers=w0xyz).json()["listening"] is False


def test_only_a_station_key_reports_ft8(client: TestClient, k9ksu: dict[str, str]) -> None:
    """An operator's session is not a station key (Principle XIII)."""
    assert client.put("/api/v1/agent/ft8", json=_report(decodes=DECODES)).status_code == 401
    response = client.put("/api/v1/agent/ft8", headers=k9ksu, json=_report(decodes=DECODES))
    assert response.status_code == 401


@pytest.mark.parametrize(
    "bad",
    [
        _report(source="radio", decodes=DECODES),
        _report(waterfall={"slot": SLOT, "first_row": 59, "rows": [ROW, ROW]}),
        _report(waterfall={"slot": SLOT, "first_row": 0, "rows": ["zz" * COLUMNS]}),
        _report(waterfall={"slot": SLOT, "first_row": 0, "rows": ["18" * 10]}),
        _report(
            decodes={
                "slot": SLOT,
                "decodes": [{"message": "cq <script>", "snr_db": 0, "dt_s": 0, "base_hz": 1}],
            }
        ),
        _report(decodes={"slot": 5, "decodes": []}),
    ],
)
def test_a_report_that_does_not_fit_is_refused(
    client: TestClient, agent: dict[str, str], bad: dict[str, Any]
) -> None:
    """Every part is checked at the boundary (Principle XII)."""
    assert client.put("/api/v1/agent/ft8", headers=agent, json=bad).status_code == 422


# -- Practice operations and pretend contacts ---------------------------------

PRACTICE = {
    "overlay": "everyday",
    "name": "FT8 practice",
    "station_call": "K9KSU",
    "practice": True,
}
REAL = {**PRACTICE, "name": "Everyday", "practice": False}
PRETEND_QSO = [
    {"name": "CALL", "value": "K1ABC"},
    {"name": "QSO_DATE", "value": "20261019"},
    {"name": "TIME_ON", "value": "1500"},
    {"name": "BAND", "value": "20m"},
    {"name": "MODE", "value": "FT8"},
    {"name": "APP_LOGLAB_PRETEND", "value": "Y"},
]
REAL_QSO = [f for f in PRETEND_QSO if f["name"] != "APP_LOGLAB_PRETEND"]


def _operation(client: TestClient, headers: dict[str, str], body: dict[str, Any]) -> dict[str, Any]:
    response = client.post("/api/v1/operations", headers=headers, json=body)
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()
    return out


def _log(
    client: TestClient,
    headers: dict[str, str],
    fields: list[dict[str, str]],
    operation: dict[str, Any] | None,
) -> httpx.Response:
    body: dict[str, Any] = {"fields": fields}
    if operation is not None:
        body["operation"] = {"id": operation["id"]}
    return client.post("/api/v1/qsos", headers=headers, json=body)


def test_a_pretend_contact_goes_only_into_a_practice_operation(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """A made-up contact can never land in a real log (Principle XX)."""
    practice = _operation(client, k9ksu, PRACTICE)
    real = _operation(client, k9ksu, REAL)
    assert (practice["practice"], real["practice"]) == (True, False)
    refused = _log(client, k9ksu, PRETEND_QSO, real)
    assert refused.status_code == 409
    assert "practice operation" in refused.json()["detail"]
    assert _log(client, k9ksu, PRETEND_QSO, None).status_code == 409
    logged = _log(client, k9ksu, PRETEND_QSO, practice)
    assert logged.status_code == 201, logged.text


def test_a_pretend_contact_cannot_be_moved_into_a_real_operation(
    client: TestClient, k9ksu: dict[str, str]
) -> None:
    """Editing a pretend QSO into a real operation is refused like logging it there."""
    practice = _operation(client, k9ksu, PRACTICE)
    real = _operation(client, k9ksu, REAL)
    qso = _log(client, k9ksu, PRETEND_QSO, practice).json()["qso"]
    moved = [
        {
            "name": f["name"],
            "value": real["id"] if f["name"] == "APP_LOGLAB_OPERATION" else f["value"],
        }
        for f in qso["fields"]
    ]
    body = {"fields": moved, "base_version": qso["version"]}
    assert client.put(f"/api/v1/qsos/{qso['id']}", headers=k9ksu, json=body).status_code == 409


def test_practice_is_set_once(client: TestClient, k9ksu: dict[str, str]) -> None:
    """An operation can't become a practice one later, or stop being one."""
    practice = _operation(client, k9ksu, PRACTICE)
    real = _operation(client, k9ksu, REAL)
    for op, body in (
        (practice, {**PRACTICE, "practice": False}),
        (real, {**REAL, "practice": True}),
    ):
        response = client.put(f"/api/v1/operations/{op['id']}", headers=k9ksu, json=body)
        assert response.status_code == 409, response.text
    renamed = client.put(
        f"/api/v1/operations/{practice['id']}", headers=k9ksu, json={**PRACTICE, "name": "Drill"}
    )
    assert renamed.status_code == 200, renamed.text
    assert renamed.json()["practice"] is True


def test_practice_qsos_are_never_sent_to_lotw_or_qrz(
    client: TestClient, k9ksu: dict[str, str], conn: psycopg.Connection[TupleRow]
) -> None:
    """Only the real QSO waits for LoTW and QRZ.com; the practice ones never do."""
    practice = _operation(client, k9ksu, PRACTICE)
    real = _operation(client, k9ksu, REAL)
    assert _log(client, k9ksu, PRETEND_QSO, practice).status_code == 201
    assert _log(client, k9ksu, REAL_QSO, practice).status_code == 201
    kept = _log(client, k9ksu, REAL_QSO, real)
    assert kept.status_code == 201
    queue = LogStore(conn).qsl_queue("LOTW", "K9KSU", 10)
    assert [v.qso_id for v in queue] == [_id(kept.json())]
    pending = LogbookStore(conn).pending(
        Logbook(book_call="K9KSU", api_key="x", from_entry_seq=0, operator="K9KSU")
    )
    assert [p.qso_id for p in pending] == [_id(kept.json())]


def _id(written: dict[str, Any]) -> uuid.UUID:
    return uuid.UUID(written["qso"]["id"])


def test_the_log_marks_who_was_already_worked(
    client: TestClient, agent: dict[str, str], k9ksu: dict[str, str]
) -> None:
    """With an operation, a decode says its sender is already in that operation's log."""
    practice = _operation(client, k9ksu, PRACTICE)
    assert _log(client, k9ksu, PRETEND_QSO, practice).status_code == 201
    assert (
        client.put("/api/v1/agent/ft8", headers=agent, json=_report(decodes=DECODES)).status_code
        == 204
    )
    heard = client.get(f"/api/v1/ft8?operation={practice['id']}", headers=k9ksu).json()
    worked = {d["message"]: d["worked"] for s in heard["slots"] for d in s["decodes"]}
    assert worked == {"CQ K1ABC FN42": True, "W9XYZ K1ABC RR73": True, "K1ABC W9XYZ EN52": False}


def test_the_waterfall_is_a_box_of_its_own(client: TestClient, k9ksu: dict[str, str]) -> None:
    """The FT8 waterfall and the list of stations heard are saved as separate boxes."""
    layout = {
        "modules": [
            {"id": "waterfall", "width": "half", "side": "left"},
            {"id": "ft8", "width": "half", "side": "right", "hidden": True},
        ]
    }
    assert client.put("/api/v1/layout", headers=k9ksu, json=layout).status_code == 200
    saved = client.get("/api/v1/layout", headers=k9ksu).json()["layout"]
    assert [(m["id"], m["width"], m.get("hidden")) for m in saved["modules"]] == [
        ("waterfall", "half", False),
        ("ft8", "half", True),
    ]
