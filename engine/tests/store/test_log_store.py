# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The log store: lossless (IV), UTC (VIII), versioned and audited (VII), atomic (XIV)."""

from __future__ import annotations

import hashlib
import uuid
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import psycopg
import pytest
from hypothesis import HealthCheck, given, settings

from loglab_engine.adif import AdifDocument, Field, Header, Record, write_adi
from loglab_engine.adif.ghosts import generate
from loglab_engine.store import (
    Action,
    Actor,
    DuplicateImportError,
    ImportSummary,
    LogStore,
    QsoExistsError,
    QsoFilter,
    QsoNotFoundError,
    StaleVersionError,
    StoreError,
)

from ..adif.test_roundtrip import documents

if TYPE_CHECKING:
    from psycopg.rows import TupleRow


def _import(
    store: LogStore,
    document: AdifDocument,
    actor: Actor,
    name: str = "ghosts.adi",
) -> ImportSummary:
    data = write_adi(document)
    return store.import_document(
        document,
        file_name=name,
        file_sha256=hashlib.sha256(data).hexdigest(),
        actor=actor,
    )


def _has_nul(document: AdifDocument) -> bool:
    texts = [f.value for r in document.records for f in r.fields]
    if document.header is not None:
        texts += [document.header.preamble, *(f.value for f in document.header.fields)]
    return any("\x00" in text for text in texts)


def _count(conn: psycopg.Connection[TupleRow], table: str) -> int:
    row = conn.execute(f"SELECT count(*) FROM log.{table}").fetchone()  # noqa: S608
    assert row is not None
    count: int = row[0]
    return count


def _qso(call: str = "Q1ABC", **extra: str) -> Record:
    fields = [
        Field("CALL", call),
        Field("QSO_DATE", "20260627"),
        Field("TIME_ON", "1805"),
        Field("BAND", "20M"),
        Field("MODE", "SSB"),
    ]
    fields += [Field(name, value) for name, value in extra.items()]
    return Record(tuple(fields))


# -- Lossless storage (Principle IV) -----------------------------------------


def test_log_and_reference_are_separate_schemas(
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Principle VI: the log's tables are in ``log``; ``reference`` holds the callbook cache."""
    rows = conn.execute(
        "SELECT table_schema, table_name FROM information_schema.tables "
        "WHERE table_schema IN ('log', 'reference')",
    ).fetchall()
    tables = {(str(schema), str(name)) for schema, name in rows}
    assert ("log", "qso") in tables
    assert ("log", "qso_version") in tables
    assert {t for t in tables if t[0] == "reference"} == {
        ("reference", "callbook_entry"),
        ("reference", "callbook_media"),
    }
    links = conn.execute(
        "SELECT count(*) FROM information_schema.referential_constraints r"
        " JOIN information_schema.table_constraints t ON t.constraint_name = r.constraint_name"
        " WHERE t.table_schema = 'reference' OR r.unique_constraint_schema = 'reference'",
    ).fetchone()
    assert links == (0,)  # nothing ties reference data to the log


def test_ghosts_import_and_export_back_byte_for_byte(store: LogStore, shack: Actor) -> None:
    """1000 ghosts, with every ADI edge case, come back out as the identical file."""
    document = generate(1000)
    ids = _import(store, document, shack).qso_ids
    assert len(set(ids)) == 1000
    exported = store.export_import(1)
    assert exported == document
    assert write_adi(exported) == write_adi(document)


@settings(
    max_examples=60,
    deadline=None,
    suppress_health_check=[HealthCheck.function_scoped_fixture],
)
@given(document=documents)
def test_any_document_survives_storage(
    store: LogStore, document: AdifDocument, shack: Actor
) -> None:
    """Random names, case, order, duplicates, indicators, and Unicode all survive storage."""
    if _has_nul(document):
        with pytest.raises(StoreError, match="NUL"):
            _import(store, document, shack, name="random.adi")
        return
    try:
        summary = _import(store, document, shack, name="random.adi")
    except DuplicateImportError:
        return  # Hypothesis replayed a document this test's database already has.
    assert store.export_import(summary.import_id) == document
    for qso_id, record in zip(summary.qso_ids, document.records, strict=True):
        assert store.current(qso_id).record == record


def test_nul_is_refused_with_the_record_number_and_nothing_is_stored(
    store: LogStore,
    conn: psycopg.Connection[TupleRow],
    shack: Actor,
) -> None:
    """PostgreSQL cannot store U+0000, so the import is refused instead of altered."""
    document = AdifDocument(None, (_qso(), _qso(COMMENT="bad\x00byte")))
    with pytest.raises(StoreError, match="record 2: COMMENT contains a NUL"):
        _import(store, document, shack)
    assert _count(conn, "changeset") == 0
    assert _count(conn, "qso") == 0


def test_whole_log_export_uses_the_given_header_and_entry_order(
    store: LogStore, shack: Actor
) -> None:
    """A whole-log export carries LogLab's header, then every live QSO in the order entered."""
    first = generate(3, seed=1)
    second = generate(2, seed=2)
    _import(store, first, shack, "one.adi")
    _import(store, second, shack, "two.adi")
    header = Header("LogLab test export\n", (Field("PROGRAMID", "LogLab"),))
    exported = store.export_log(header)
    assert exported.header == header
    assert exported.records == first.records + second.records


def test_reimporting_the_same_file_is_refused(
    store: LogStore,
    conn: psycopg.Connection[TupleRow],
    shack: Actor,
) -> None:
    """The same bytes again would duplicate every QSO; it is refused and nothing changes."""
    document = generate(5)
    _import(store, document, shack)
    with pytest.raises(DuplicateImportError, match="already imported, as import 1"):
        _import(store, document, shack, name="renamed.adi")
    assert _count(conn, "qso") == 5
    assert _count(conn, "changeset") == 1


def test_import_keeps_the_header_and_file_identity(store: LogStore, shack: Actor) -> None:
    """The import batch records the file name, SHA-256, header, count, and who imported it."""
    document = generate(4)
    _import(store, document, shack)
    batch = store.import_batch(1)
    assert batch.file_name == "ghosts.adi"
    assert batch.file_sha256 == hashlib.sha256(write_adi(document)).hexdigest()
    assert batch.header == document.header
    assert batch.record_count == 4
    assert (batch.operator, batch.station) == ("K9KSU", "shack")
    with pytest.raises(StoreError, match="no import 9"):
        store.import_batch(9)


def test_headerless_file_round_trips(store: LogStore, shack: Actor) -> None:
    """A file with no header exports with no header."""
    document = AdifDocument(None, (_qso(),))
    _import(store, document, shack, "bare.adi")
    assert store.export_import(1) == document


# -- Identity and time (Principles XV, VIII) ---------------------------------


def test_each_qso_gets_a_unique_id_when_entered(store: LogStore, shack: Actor) -> None:
    """IDs are assigned on entry; a supplied ID is kept; reusing one is refused."""
    entered = store.create_qso(_qso(), actor=shack)
    assert entered.version == 1
    assert entered.action is Action.CREATE
    chosen = uuid.UUID("12345678-1234-4234-8234-123456789abc")
    assert store.create_qso(_qso("Q2B"), actor=shack, qso_id=chosen).qso_id == chosen
    with pytest.raises(StoreError, match="already exists"):
        store.create_qso(_qso("Q3C"), actor=shack, qso_id=chosen)
    assert store.current(chosen).call == "Q2B"


def test_entering_under_a_client_id_is_safe_to_repeat(store: LogStore, shack: Actor) -> None:
    """A journal re-send is recognised, even after an edit; a different QSO is refused (XV)."""
    chosen = uuid.uuid4()
    first, created = store.enter_qso(_qso("Q4D"), actor=shack, qso_id=chosen)
    assert created
    store.edit_qso(chosen, _qso("Q4E"), actor=shack, base_version=1)
    again, created = store.enter_qso(_qso("Q4D"), actor=shack, qso_id=chosen)
    assert not created
    assert (again.qso_id, again.version, again.call) == (first.qso_id, 2, "Q4E")
    with pytest.raises(QsoExistsError, match="different QSO"):
        store.enter_qso(_qso("Q4E"), actor=shack, qso_id=chosen)
    assert len(store.history(chosen)) == 2


def test_times_are_utc_and_derived_columns_follow_the_snapshot(
    store: LogStore, shack: Actor
) -> None:
    """Change time and QSO start come back as UTC; call, band, and mode are normalized."""
    before = datetime.now(UTC)
    version = store.create_qso(_qso("q1abc"), actor=shack)
    assert version.changed_at.utcoffset() == timedelta(0)
    assert before - timedelta(seconds=5) <= version.changed_at <= datetime.now(UTC)
    assert version.qso_start == datetime(2026, 6, 27, 18, 5, tzinfo=UTC)
    assert version.qso_start is not None
    assert version.qso_start.utcoffset() == timedelta(0)
    assert (version.call, version.band, version.mode) == ("Q1ABC", "20m", "SSB")


@pytest.mark.parametrize(
    ("date", "time", "expected"),
    [
        ("20260627", "180559", datetime(2026, 6, 27, 18, 5, 59, tzinfo=UTC)),
        ("20240229", "0000", datetime(2024, 2, 29, 0, 0, tzinfo=UTC)),
        ("20260230", "1200", None),  # Not a real date.
        ("20260627", "2500", None),  # Not a real time.
        ("2026-06-27", "1200", None),  # Not ADIF's format.
        ("20260627", "12", None),
    ],
)
def test_qso_start_is_derived_only_from_real_utc_dates(
    store: LogStore,
    date: str,
    time: str,
    expected: datetime | None,
    shack: Actor,
) -> None:
    """Malformed dates and times are stored as written, with no derived start."""
    record = Record((Field("CALL", "Q1A"), Field("qso_date", date), Field("time_on", time)))
    version = store.create_qso(record, actor=shack)
    assert version.qso_start == expected
    assert version.record == record


def test_missing_fields_derive_nothing(store: LogStore, shack: Actor) -> None:
    """A QSO with none of the derived fields still stores, with empty derived columns."""
    version = store.create_qso(Record((Field("COMMENT", "only a comment"),)), actor=shack)
    assert (version.call, version.qso_start, version.band, version.mode) == (None,) * 4


# -- Versioned, audited changes (Principle VII) ------------------------------


def test_edit_adds_a_version_and_keeps_what_it_was_before(
    store: LogStore, shack: Actor, visitor: Actor
) -> None:
    """An edit is a new version by its operator and station; the old one is unchanged."""
    qso_id = store.create_qso(_qso(), actor=shack).qso_id
    edited = _qso("Q9ZZZ", COMMENT="call corrected")
    after = store.edit_qso(qso_id, edited, actor=visitor, base_version=1, note="busted call")
    assert after.version == 2
    assert after.call == "Q9ZZZ"
    history = store.history(qso_id)
    assert [v.version for v in history] == [1, 2]
    assert history[0].record == _qso()
    assert history[0].call == "Q1ABC"
    assert (history[1].operator, history[1].station, history[1].note) == (
        "W0XYZ",
        "field-day-2",
        "busted call",
    )
    assert history[1].action is Action.EDIT
    assert history[1].changed_at >= history[0].changed_at


def test_a_stale_edit_is_refused(store: LogStore, shack: Actor, visitor: Actor) -> None:
    """Editing from an old version would silently undo someone else's change."""
    qso_id = store.create_qso(_qso(), actor=shack).qso_id
    store.edit_qso(qso_id, _qso(RST_RCVD="59"), actor=shack, base_version=1)
    with pytest.raises(StaleVersionError, match="at version 2, not 1"):
        store.edit_qso(qso_id, _qso(RST_RCVD="57"), actor=visitor, base_version=1)
    assert store.current(qso_id).record == _qso(RST_RCVD="59")


def test_an_edit_that_changes_nothing_is_refused(store: LogStore, shack: Actor) -> None:
    """Every version records a real change."""
    qso_id = store.create_qso(_qso(), actor=shack).qso_id
    with pytest.raises(StoreError, match="changes nothing"):
        store.edit_qso(qso_id, _qso(), actor=shack, base_version=1)
    assert len(store.history(qso_id)) == 1


def test_delete_is_recorded_and_recoverable(store: LogStore, shack: Actor, visitor: Actor) -> None:
    """A deleted QSO leaves exports and searches, keeps its history, and can be restored."""
    document = generate(10)
    victim = _import(store, document, shack).qso_ids[3]

    deleted = store.delete_qso(victim, actor=visitor, note="dupe")
    assert deleted.deleted
    assert deleted.record == document.records[3]
    assert store.export_import(1).records == document.records[:3] + document.records[4:]
    assert victim not in {v.qso_id for v in store.find(limit=100)}
    assert victim in {v.qso_id for v in store.find(QsoFilter(include_deleted=True), limit=100)}

    with pytest.raises(StoreError, match="already deleted"):
        store.delete_qso(victim, actor=shack)
    with pytest.raises(StoreError, match="restore it before editing"):
        store.edit_qso(victim, _qso(), actor=shack, base_version=2)

    restored = store.restore_qso(victim, actor=shack)
    assert (restored.version, restored.deleted) == (3, False)
    assert [v.action for v in store.history(victim)] == [
        Action.CREATE,
        Action.DELETE,
        Action.RESTORE,
    ]
    assert store.export_import(1) == document
    with pytest.raises(StoreError, match="is not deleted"):
        store.restore_qso(victim, actor=shack)


def test_unknown_qsos_are_reported(store: LogStore, shack: Actor) -> None:
    """Every operation on a missing QSO says so instead of doing nothing."""
    missing = uuid.uuid4()
    with pytest.raises(QsoNotFoundError):
        store.current(missing)
    with pytest.raises(QsoNotFoundError):
        store.history(missing)
    with pytest.raises(QsoNotFoundError):
        store.delete_qso(missing, actor=shack)
    with pytest.raises(QsoNotFoundError):
        store.edit_qso(missing, _qso(), actor=shack, base_version=1)


def test_ids_resolve_from_a_prefix(store: LogStore, shack: Actor) -> None:
    """Operators can type the first 8 characters of an ID."""
    qso_id = store.create_qso(_qso(), actor=shack).qso_id
    assert store.resolve(str(qso_id)[:8].upper()) == qso_id
    assert store.resolve(str(qso_id)) == qso_id
    with pytest.raises(QsoNotFoundError, match="8\\+ character prefix"):
        store.resolve(str(qso_id)[:7])
    with pytest.raises(QsoNotFoundError, match="no QSO ID starts"):
        store.resolve("ffffffff-ffff")


def test_find_by_call_uses_the_current_version(store: LogStore, shack: Actor) -> None:
    """Search sees a QSO as it is now, not as it was."""
    qso_id = store.create_qso(_qso("Q1OLD"), actor=shack).qso_id
    store.edit_qso(qso_id, _qso("Q1NEW"), actor=shack, base_version=1)
    assert store.find(QsoFilter(call="q1old")) == ()
    assert [v.qso_id for v in store.find(QsoFilter(call=" q1new "))] == [qso_id]


def test_actors_must_be_identified(store: LogStore, shack: Actor) -> None:
    """Identities are validated; there is no anonymous change."""
    with pytest.raises(StoreError, match="upper-case callsign"):
        Actor("", "shack", shack.session_id)
    with pytest.raises(StoreError, match="upper-case callsign"):
        Actor("k9ksu", "shack", shack.session_id)
    with pytest.raises(StoreError, match="station"):
        Actor("K9KSU", "", shack.session_id)
    with pytest.raises(StoreError, match="at most 1000"):
        store.create_qso(_qso(), actor=shack, note="x" * 1001)


def test_store_needs_autocommit(database: str) -> None:
    """The store owns its transactions; an implicit outer one would hide partial writes."""
    with psycopg.connect(f"dbname={database}") as conn:
        with pytest.raises(StoreError, match="autocommit"):
            LogStore(conn)
        conn.rollback()


# -- Atomicity (Principle XIV) -----------------------------------------------


def test_a_failed_import_stores_nothing(
    store: LogStore,
    conn: psycopg.Connection[TupleRow],
    shack: Actor,
) -> None:
    """If the database fails part-way through an import, not one QSO of it remains."""
    conn.execute(
        """
        CREATE FUNCTION public.fail_at_500() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF (SELECT count(*) FROM log.qso_version) >= 500 THEN
                RAISE EXCEPTION 'simulated failure';
            END IF;
            RETURN NEW;
        END $$;
        CREATE TRIGGER fail_at_500 BEFORE INSERT ON log.qso_version
            FOR EACH ROW EXECUTE FUNCTION public.fail_at_500();
        """,
    )
    with pytest.raises(psycopg.errors.RaiseException, match="simulated failure"):
        _import(store, generate(1000), shack)
    for table in ("changeset", "import_batch", "qso", "qso_version"):
        assert _count(conn, table) == 0, table
    assert (_count(conn, "operator"), _count(conn, "station")) == (1, 1)  # just the signed-in one


# -- The database enforces the rules, not just this code (Principle VII) -----


@pytest.fixture
def entered(store: LogStore, shack: Actor) -> uuid.UUID:
    """One QSO, edited once, so it has two versions."""
    qso_id = store.create_qso(_qso(), actor=shack).qso_id
    store.edit_qso(qso_id, _qso(RST_RCVD="59"), actor=shack, base_version=1)
    return qso_id


@pytest.mark.parametrize(
    ("statement", "error"),
    [
        ("UPDATE log.qso_version SET deleted = true", "append-only"),
        ("DELETE FROM log.qso_version", "append-only"),
        ("DELETE FROM log.qso", "append-only"),
        ("UPDATE log.qso SET deleted = true", "only by adding a version"),
        ("UPDATE log.qso SET current_version = current_version + 1", "only by adding a version"),
        ("UPDATE log.changeset SET note = 'rewritten'", "append-only"),
        ("DELETE FROM log.changeset", "append-only"),
        ("UPDATE log.operator SET callsign = 'N0BODY'", "append-only"),
        ("TRUNCATE log.qso_version CASCADE", "append-only"),
        ("TRUNCATE log.qso CASCADE", "append-only"),
        ("UPDATE log.qso_version SET call = 'Q9X'", "generated column"),
    ],
)
@pytest.mark.usefixtures("entered")
def test_history_cannot_be_rewritten_even_with_sql(
    conn: psycopg.Connection[TupleRow],
    statement: str,
    error: str,
) -> None:
    """Direct SQL that would erase or alter the record is refused by the database."""
    with pytest.raises(psycopg.Error, match=error):
        conn.execute(statement.encode())
    assert _count(conn, "qso_version") == 2


def _changeset(conn: psycopg.Connection[TupleRow], kind: str) -> int:
    row = conn.execute(
        """
        INSERT INTO log.changeset (operator_id, station_id, session_id, kind)
        SELECT operator_id, station_id, id, %s FROM log.session LIMIT 1
        RETURNING id
        """,
        (kind,),
    ).fetchone()
    assert row is not None
    changeset_id: int = row[0]
    return changeset_id


def _add_version(
    conn: psycopg.Connection[TupleRow],
    qso_id: uuid.UUID,
    version: int,
    changeset_id: int,
    action: str,
) -> None:
    conn.execute(
        """
        INSERT INTO log.qso_version (qso_id, version, changeset_id, action, deleted, fields)
        VALUES (%s, %s, %s, %s, %s, %s::jsonb)
        """,
        (qso_id, version, changeset_id, action, action == "delete", '[["CALL", "Q1X", null]]'),
    )


def test_versions_must_follow_in_order_from_this_transaction(
    conn: psycopg.Connection[TupleRow],
    entered: uuid.UUID,
) -> None:
    """Skipped versions, reused changesets, and mismatched kinds are refused."""
    with pytest.raises(psycopg.Error, match="the next version is 3, not 5"), conn.transaction():
        _add_version(conn, entered, 5, _changeset(conn, "edit"), "edit")
    with pytest.raises(psycopg.Error, match="changeset from its own transaction"):
        _add_version(conn, entered, 3, 1, "edit")
    with pytest.raises(psycopg.Error, match="in an edit"), conn.transaction():
        _add_version(conn, entered, 3, _changeset(conn, "delete"), "edit")
    with pytest.raises(psycopg.Error, match="keeping its fields"), conn.transaction():
        _add_version(conn, entered, 3, _changeset(conn, "delete"), "delete")
    with pytest.raises(psycopg.Error, match="qso_version_fields_check"), conn.transaction():
        conn.execute(
            """
            INSERT INTO log.qso_version (qso_id, version, changeset_id, action, deleted, fields)
            VALUES (%s, 3, %s, 'edit', false, '[["CALL", 5, null]]')
            """,
            (entered, _changeset(conn, "edit")),
        )
    with conn.transaction():
        _add_version(conn, entered, 3, _changeset(conn, "edit"), "edit")
    assert _count(conn, "qso_version") == 3


def test_a_qso_cannot_exist_without_a_version(conn: psycopg.Connection[TupleRow]) -> None:
    """A QSO row alone fails at commit."""
    with pytest.raises(psycopg.Error, match="without a version"), conn.transaction():
        conn.execute("INSERT INTO log.qso (id) VALUES (gen_random_uuid())")
    assert _count(conn, "qso") == 0


@pytest.mark.usefixtures("entered")
def test_audit_columns_are_required_and_time_is_the_databases(
    conn: psycopg.Connection[TupleRow],
) -> None:
    """A changeset must name its operator and station, and cannot be backdated."""
    session = conn.execute("SELECT id, operator_id, station_id FROM log.session").fetchone()
    assert session is not None
    session_id, operator_id, station_id = session
    with pytest.raises(psycopg.errors.NotNullViolation):
        conn.execute(
            "INSERT INTO log.changeset (station_id, session_id, kind) VALUES (%s, %s, 'edit')",
            (station_id, session_id),
        )
    with pytest.raises(psycopg.errors.NotNullViolation):
        conn.execute(
            "INSERT INTO log.changeset (operator_id, session_id, kind) VALUES (%s, %s, 'edit')",
            (operator_id, session_id),
        )
    row = conn.execute(
        """
        INSERT INTO log.changeset (operator_id, station_id, session_id, kind, changed_at)
        VALUES (%s, %s, %s, 'edit', '2001-01-01T00:00Z') RETURNING changed_at
        """,
        (operator_id, station_id, session_id),
    ).fetchone()
    assert row is not None
    assert row[0] > datetime(2026, 1, 1, tzinfo=UTC)


@pytest.mark.parametrize("sort", ["dxcc", "field:", "field:FREQ' OR 1=1", "q.entry_seq"])
def test_find_refuses_an_unknown_sort(store: LogStore, sort: str) -> None:
    """The store checks a sort itself, so no caller can put SQL into it."""
    with pytest.raises(StoreError, match="cannot be sorted by"):
        store.find(sort=sort)
