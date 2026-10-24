# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Demo mode: pretend operators logging made-up QSOs, practice only and marked pretend (XX)."""

# Seeded pseudo-randomness keeps the tests repeatable.
# ruff: noqa: S311

from __future__ import annotations

import random
import re
import time
from datetime import UTC, datetime, timedelta, timezone
from typing import TYPE_CHECKING, Any

import psycopg
import pytest
from fastapi.testclient import TestClient

from loglab_engine.adif import Record
from loglab_engine.api import create_app
from loglab_engine.contests.overlay import overlay
from loglab_engine.demo import (
    EVERY,
    SPOTS,
    Demo,
    pause,
    pretend_call,
    pretend_qso,
    real_stations,
    tune,
)
from loglab_engine.onair import OnAir
from loglab_engine.reference import section_for
from loglab_engine.rig import RigBoard, band_for
from loglab_engine.store import (
    IdentityStore,
    LogStore,
    NotAuthorizedError,
    Permission,
    QsoFilter,
    StoreError,
)
from loglab_engine.store.operation import Operation, OperationSetup, OperationStore

from .conftest import ADMIN_SECRET, VISITOR_SECRET, conninfo

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.contests.overlay import Overlay
    from loglab_engine.store import Actor

# -- Making up QSOs -------------------------------------------------------------


def test_every_call_is_a_q_call() -> None:
    """Made-up calls start with Q, which the ITU issues to no station."""
    rng = random.Random(1)
    for _ in range(500):
        assert re.fullmatch(r"Q[0-9][A-Z]{2,3}", pretend_call(rng))


def test_a_qso_is_marked_pretend_and_fits_its_spot() -> None:
    """Every QSO says it is pretend and is a sensible Field Day contact."""
    rng = random.Random(2)
    for spot in SPOTS:
        hz = tune(rng, spot)
        assert band_for(hz) == spot.band
        fields = {f.name: f.value for f in pretend_qso(rng, spot, hz, datetime.now(UTC)).fields}
        assert fields["APP_LOGLAB_PRETEND"] == "Y"
        assert fields["COMMENT"] == "Pretend QSO"
        assert fields["BAND"] == spot.band
        assert fields["MODE"] == spot.mode
        assert round(float(fields["FREQ"]) * 1e6) == hz
        assert section_for(fields["ARRL_SECT"]) is not None
        assert re.fullmatch(r"[1-9][0-9]?[A-F]", fields["CLASS"])


def test_a_real_station_keeps_its_call_and_section() -> None:
    """Working a real station uses its call and section; the QSO is still marked pretend."""
    record = pretend_qso(random.Random(5), SPOTS[0], 14_030_000, datetime.now(UTC), ("W0CK", "KS"))
    fields = {f.name: f.value for f in record.fields}
    assert (fields["CALL"], fields["ARRL_SECT"]) == ("W0CK", "KS")
    assert fields["APP_LOGLAB_PRETEND"] == "Y"


def _licenses(conn: psycopg.Connection[TupleRow]) -> None:
    for call, state, active in (
        ("W0CK", "KS", True),
        ("K0ABC", "CO", True),
        ("W6XYZ", "CA", True),  # California has several sections: not used
        ("N0OLD", "KS", False),  # expired: not used
        ("VE3AA", "QC", True),
    ):
        conn.execute(
            "INSERT INTO reference.license (source, record_id, call, state, active) "
            "VALUES ('fcc', %s, %s, %s, %s)",
            (call, call, state, active),
        )


def test_real_stations_come_from_the_license_list(conn: psycopg.Connection[TupleRow]) -> None:
    """Active calls whose state is one section are used, with that section (Principle V)."""
    assert real_stations(conn) == []
    _licenses(conn)
    assert sorted(real_stations(conn)) == [("K0ABC", "CO"), ("VE3AA", "QC"), ("W0CK", "KS")]


def test_qso_time_is_utc() -> None:
    """The QSO's date and time are UTC whatever clock it is given (Principle VIII)."""
    when = datetime(2026, 6, 27, 20, 30, 15, tzinfo=timezone(timedelta(hours=-5)))
    record = pretend_qso(random.Random(3), SPOTS[0], 14_030_000, when)
    fields = {f.name: f.value for f in record.fields}
    assert (fields["QSO_DATE"], fields["TIME_ON"]) == ("20260628", "013015")


def test_pauses_are_positive_and_bounded() -> None:
    """Pauses average about the mean and never run away."""
    rng = random.Random(4)
    waits = [pause(rng, 60.0) for _ in range(1000)]
    assert all(0 <= w <= 360 for w in waits)
    assert 40 < sum(waits) / len(waits) < 80


# -- Running it -----------------------------------------------------------------


def _operation(conn: psycopg.Connection[TupleRow], actor: Actor, *, practice: bool) -> Operation:
    return OperationStore(conn).create(
        actor,
        OperationSetup(
            overlay="arrl-fd",
            name="Practice" if practice else "Field Day 2027",
            station_call="W0CK",
            gota_call=None,
            sent={"class": "3A", "section": "KS"},
            practice=practice,
        ),
    )


def _arrl_fd() -> Overlay:
    found = overlay("arrl-fd")
    assert found is not None
    return found


def test_never_runs_in_a_real_operation(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Demo mode refuses an operation that is not practice (Principle XX)."""
    demo = Demo(conninfo(database), OnAir(), RigBoard())
    with pytest.raises(ValueError, match="practice"):
        demo.start(shack, _operation(conn, shack, practice=False), _arrl_fd(), background=False)
    assert not demo.state().on


def test_pretend_stations_go_on_the_air_and_log(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Pretend workstations report, carry pretend radios, and log pretend QSOs (VII, XX)."""
    board, rigs = OnAir(), RigBoard()
    demo = Demo(conninfo(database), board, rigs)
    op = _operation(conn, shack, practice=True)
    demo.start(shack, op, _arrl_fd(), seed=7, background=False)
    demo.step(0)
    now = datetime.now(UTC)
    on_air = board.current(op.id, now)
    assert [(r.operator, r.station, r.pretend) for r in on_air] == [
        ("Q1PRE", "demo-1", True),
        ("Q2PRE", "demo-2", True),
        ("Q3PRE", "demo-3", True),
    ]
    rig = rigs.current("demo-1", now)
    assert rig is not None
    assert rig.radio is not None
    assert (rig.radio.model, rig.radio.path) == ("Pretend radio", "pretend")
    demo.step(EVERY * 7)  # every workstation is due a QSO by now
    logged = LogStore(conn).find(QsoFilter(operation=op.id), limit=10)
    assert len(logged) == 3
    for v in logged:
        fields = {f.name: f.value for f in v.record.fields}
        assert fields["CALL"].startswith("Q")  # no license list here: made-up calls
        assert fields["APP_LOGLAB_PRETEND"] == "Y"
        assert fields["OPERATOR"] in {"Q1PRE", "Q2PRE", "Q3PRE"}
        assert fields["STATION_CALLSIGN"] == "W0CK"
        assert fields["APP_LOGLAB_OPERATION"] == str(op.id)
        assert v.operator == "K9KSU"  # entered in the administrator's session (XXI)
    assert demo.state().qsos == 3
    demo.stop()
    assert board.current(op.id, datetime.now(UTC)) == ()
    assert not demo.state().on


def test_works_real_stations_when_it_has_them(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """With a license list, the pretend QSOs are with real calls in their real sections."""
    _licenses(conn)
    op = _operation(conn, shack, practice=True)
    demo = Demo(conninfo(database), OnAir(), RigBoard())
    demo.start(shack, op, _arrl_fd(), seed=3, background=False)
    demo.step(EVERY * 7)
    logged = LogStore(conn).find(QsoFilter(operation=op.id), limit=10)
    worked = {(v.call, {f.name: f.value for f in v.record.fields}["ARRL_SECT"]) for v in logged}
    assert worked == {("K0ABC", "CO"), ("VE3AA", "QC"), ("W0CK", "KS")}
    assert all(
        {f.name: f.value for f in v.record.fields}["APP_LOGLAB_PRETEND"] == "Y" for v in logged
    )
    demo.stop()


def test_stops_when_the_session_ends(
    database: str,
    conn: psycopg.Connection[TupleRow],
    identity: IdentityStore,
) -> None:
    """With nobody signed in to log them, demo mode stops itself (Principle XXI)."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    session = identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    op = _operation(conn, session.actor, practice=True)
    demo = Demo(conninfo(database), OnAir(), RigBoard())
    demo.start(session.actor, op, _arrl_fd(), seed=1, background=False)
    identity.sign_out(session.token)
    demo.step(EVERY * 7)
    state = demo.state()
    assert not state.on
    assert state.problem is not None
    assert "signed-in" in state.problem


def test_stops_when_its_operation_ends(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Ending the practice operation stops demo mode at its next QSO."""
    op = _operation(conn, shack, practice=True)
    demo = Demo(conninfo(database), OnAir(), RigBoard())
    demo.start(shack, op, _arrl_fd(), seed=1, background=False)
    OperationStore(conn).update(
        shack,
        op.id,
        OperationSetup(
            overlay=op.overlay,
            name=op.name,
            station_call=op.station_call,
            gota_call=None,
            sent=op.sent,
            ended=True,
            practice=True,
        ),
    )
    demo.step(EVERY * 7)
    assert not demo.state().on
    assert LogStore(conn).find(QsoFilter(operation=op.id), limit=10) == ()


# -- Erasing pretend QSOs (Principle VII, as amended in 2.1.0) -----------------


def _pretend_qsos(
    database: str, conn: psycopg.Connection[TupleRow], actor: Actor, op: Operation
) -> int:
    demo = Demo(conninfo(database), OnAir(), RigBoard())
    demo.start(actor, op, _arrl_fd(), seed=2, background=False)
    demo.step(EVERY * 7)
    demo.stop()
    return len(LogStore(conn).find(QsoFilter(operation=op.id), limit=50))


def test_erasing_removes_pretend_qsos_and_records_it(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Pretend QSOs go entirely, history and all; the erasure is recorded; real QSOs stay."""
    store = LogStore(conn)
    typed = pretend_qso(random.Random(1), SPOTS[0], 14_030_000, datetime.now(UTC), ("W0CK", "KS"))
    real = store.create_qso(  # a real QSO, logged outside any operation
        Record(tuple(f for f in typed.fields if f.name != "APP_LOGLAB_PRETEND")), actor=shack
    )
    op = _operation(conn, shack, practice=True)
    made = _pretend_qsos(database, conn, shack, op)
    assert made == 3
    assert store.erase_pretend(op.id, actor=shack) == 3
    assert store.find(QsoFilter(operation=op.id, include_deleted=True), limit=50) == ()
    assert store.current(real.qso_id).version == 1  # untouched
    row = conn.execute(
        "SELECT o.callsign, e.operation_id, e.qsos FROM log.pretend_erasure e "
        "JOIN log.operator o ON o.id = e.operator_id"
    ).fetchone()
    assert row == ("K9KSU", op.id, 3)
    assert store.erase_pretend(op.id, actor=shack) == 0


def test_a_qso_once_real_is_never_erased(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """A QSO with any version that is not pretend keeps its history (Principle VII)."""
    store = LogStore(conn)
    op = _operation(conn, shack, practice=True)
    _pretend_qsos(database, conn, shack, op)
    first = store.find(QsoFilter(operation=op.id), limit=1)[0]
    kept = Record(tuple(f for f in first.record.fields if f.name != "APP_LOGLAB_PRETEND"))
    store.edit_qso(first.qso_id, kept, actor=shack, base_version=first.version)
    assert store.erase_pretend(op.id, actor=shack) == 2
    assert store.current(first.qso_id).version == 2


def test_only_an_administrator_erases(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor, visitor: Actor
) -> None:
    """An operator without admin is refused, and nothing is erased (Principle XXI)."""
    op = _operation(conn, shack, practice=True)
    made = _pretend_qsos(database, conn, shack, op)
    with pytest.raises(NotAuthorizedError):
        LogStore(conn).erase_pretend(op.id, actor=visitor)
    assert len(LogStore(conn).find(QsoFilter(operation=op.id), limit=50)) == made


def test_never_erases_from_a_real_operation(
    conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Only a practice operation's pretend QSOs can be erased."""
    op = _operation(conn, shack, practice=False)
    with pytest.raises(StoreError, match="practice"):
        LogStore(conn).erase_pretend(op.id, actor=shack)


def test_the_log_is_still_append_only(
    database: str, conn: psycopg.Connection[TupleRow], shack: Actor
) -> None:
    """Outside the erasure, even a pretend QSO can't be deleted from the tables."""
    op = _operation(conn, shack, practice=True)
    _pretend_qsos(database, conn, shack, op)
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute("DELETE FROM log.qso_version")
    with pytest.raises(psycopg.errors.RestrictViolation):
        conn.execute(
            "SELECT set_config('loglab.erasing_pretend', 'not-this-one', false); "
            "DELETE FROM log.qso_version"
        )


# -- Through the API ------------------------------------------------------------


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


def test_only_an_administrator_turns_it_on(
    client: TestClient, identity: IdentityStore, shack: Actor
) -> None:
    """Operators can see whether it is on; only administrators switch it (XXI)."""
    identity.add_operator(shack, "W0XYZ", VISITOR_SECRET, [Permission.VIEW, Permission.LOG])
    w0xyz = _sign_in(client, "W0XYZ", VISITOR_SECRET, "gota")
    assert client.get("/api/v1/demo", headers=w0xyz).json()["on"] is False
    response = client.put("/api/v1/demo", headers=w0xyz, json={"on": True})
    assert response.status_code == 403, response.text


def test_on_sets_up_a_practice_operation_and_off_stops(client: TestClient, shack: Actor) -> None:
    """Turning it on sets up Demo Field Day, practice; pretend stations show On air."""
    del shack
    k9ksu = _sign_in(client, "K9KSU", ADMIN_SECRET, "shack")
    try:
        state = client.put("/api/v1/demo", headers=k9ksu, json={"on": True}).json()
        assert state["on"] is True
        assert state["operation_name"] == "Demo Field Day"
        assert state["stations"] == ["demo-1", "demo-2", "demo-3"]
        ops = client.get("/api/v1/operations", headers=k9ksu).json()
        demo_op = next(o for o in ops if o["id"] == state["operation"])
        assert demo_op["practice"] is True
        assert demo_op["station_call"] == "K9KSU"
        stations: list[dict[str, Any]] = []
        for _ in range(50):  # the first report is made as soon as it starts
            path = f"/api/v1/operations/{state['operation']}/on-air"
            stations = client.get(path, headers=k9ksu).json()["stations"]
            if len(stations) == 3:
                break
            time.sleep(0.1)
        assert [s["pretend"] for s in stations] == [True, True, True]
        assert all(s["from_radio"] for s in stations)
        again = client.put("/api/v1/demo", headers=k9ksu, json={"on": True}).json()
        assert again["operation"] == state["operation"]  # the same operation, not another
    finally:
        off = client.put("/api/v1/demo", headers=k9ksu, json={"on": False}).json()
    assert off["on"] is False
    assert isinstance(off["erased"], int)
    remaining = client.get(
        f"/api/v1/qsos?operation={state['operation']}&include_deleted=true", headers=k9ksu
    ).json()
    assert remaining == []
    path = f"/api/v1/operations/{state['operation']}/on-air"
    assert client.get(path, headers=k9ksu).json()["stations"] == []
