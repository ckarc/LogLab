# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The log in PostgreSQL: QSOs as versioned, audited ADIF snapshots.

:class:`LogStore` is the only way the Engine writes QSOs. Every write is one
transaction (Principle XIV) that records a changeset (operator, station, UTC
time, kind) and appends complete snapshots; nothing is updated in place or
erased (Principle VII). The database enforces the same rules, so a bug here
fails instead of silently changing the log.

Storing a QSO here does not make it *logged*: that needs a second, independent
copy (Principle XV), which this module does not provide.
"""

from __future__ import annotations

import re
import uuid
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import TYPE_CHECKING, Final, cast

import psycopg
from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.adif import AdifDocument, AdifModelError, Field, Header, Record

if TYPE_CHECKING:
    from collections.abc import Iterable

    from psycopg.rows import TupleRow

_CALLSIGN: Final = re.compile(r"^[A-Z0-9]+(/[A-Z0-9]+)*$")
_STATION: Final = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
_HEX_PREFIX: Final = re.compile(r"^[0-9a-f-]{8,36}$")
_MAX_CALLSIGN: Final = 20
_MAX_NOTE: Final = 1000

_VERSION_COLUMNS: Final = """
    v.qso_id, v.version, v.action, v.deleted, v.fields, v.call, v.qso_start, v.band, v.mode,
    c.changed_at, o.callsign, s.name, c.note, b.id, b.file_name
"""
_VERSION_JOINS: Final = """
    JOIN log.changeset c ON c.id = v.changeset_id
    JOIN log.operator o ON o.id = c.operator_id
    JOIN log.station s ON s.id = c.station_id
    LEFT JOIN log.import_batch b ON b.changeset_id = c.id
"""


class StoreError(Exception):
    """A request to the log cannot be carried out; the log is unchanged."""


class QsoNotFoundError(StoreError):
    """No QSO has the given ID."""


class StaleVersionError(StoreError):
    """The QSO changed since the version the caller based its edit on."""


class DuplicateImportError(StoreError):
    """This exact file has been imported before."""


class Action(StrEnum):
    """What a QSO version did."""

    CREATE = "create"
    EDIT = "edit"
    DELETE = "delete"
    RESTORE = "restore"


@dataclass(frozen=True, slots=True)
class Actor:
    """Who is making a change, and from where (placeholders until operator sign-in)."""

    operator: str
    station: str

    def __post_init__(self) -> None:
        """Refuse identities the log could not record."""
        if len(self.operator) > _MAX_CALLSIGN or not _CALLSIGN.match(self.operator):
            msg = f"operator {self.operator!r} is not an upper-case callsign"
            raise StoreError(msg)
        if not _STATION.match(self.station):
            msg = f"station {self.station!r} must be 1-64 letters, digits, '.', '_' or '-'"
            raise StoreError(msg)


@dataclass(frozen=True, slots=True)
class QsoVersion:
    """One version of a QSO: its complete fields and the audit record of the change."""

    qso_id: uuid.UUID
    version: int
    action: Action
    deleted: bool
    record: Record
    changed_at: datetime
    operator: str
    station: str
    note: str | None
    call: str | None
    qso_start: datetime | None
    band: str | None
    mode: str | None
    import_id: int | None
    import_file: str | None


@dataclass(frozen=True, slots=True)
class ImportBatch:
    """An imported file: its identity and the header it arrived with."""

    import_id: int
    file_name: str
    file_sha256: str
    header: Header | None
    record_count: int
    imported_at: datetime
    operator: str
    station: str


@dataclass(frozen=True, slots=True)
class ImportSummary:
    """The result of an import."""

    import_id: int
    qso_ids: tuple[uuid.UUID, ...]


def _encode_fields(fields: Iterable[Field], where: str) -> list[list[str | None]]:
    out: list[list[str | None]] = []
    for field in fields:
        if "\x00" in field.value:
            msg = (
                f"{where}: {field.name} contains a NUL character, which ADIF does not "
                "allow and PostgreSQL cannot store"
            )
            raise StoreError(msg)
        out.append([field.name, field.value, field.type_indicator])
    return out


def _decode_fields(raw: object) -> tuple[Field, ...]:
    """Turn stored JSON back into fields, checking its shape (the database is a boundary too)."""
    if not isinstance(raw, list):
        msg = "stored fields are not a list"
        raise StoreError(msg)
    fields: list[Field] = []
    for item in cast("list[object]", raw):
        if not isinstance(item, list) or len(cast("list[object]", item)) != 3:  # noqa: PLR2004
            msg = "a stored field is not [name, value, type]"
            raise StoreError(msg)
        name, value, indicator = cast("list[object]", item)
        if not (
            isinstance(name, str)
            and isinstance(value, str)
            and (indicator is None or isinstance(indicator, str))
        ):
            msg = "a stored field has the wrong types"
            raise StoreError(msg)
        try:
            fields.append(Field(name, value, indicator))
        except AdifModelError as error:
            raise StoreError(f"a stored field is invalid: {error}") from error
    return tuple(fields)


def _encode_header(header: Header | None) -> Jsonb | None:
    if header is None:
        return None
    if "\x00" in header.preamble:
        msg = "header text contains a NUL character, which PostgreSQL cannot store"
        raise StoreError(msg)
    return Jsonb({"preamble": header.preamble, "fields": _encode_fields(header.fields, "header")})


def _decode_header(raw: object) -> Header | None:
    if raw is None:
        return None
    if not isinstance(raw, dict):
        msg = "stored header is not an object"
        raise StoreError(msg)
    data = cast("dict[str, object]", raw)
    preamble = data.get("preamble")
    if not isinstance(preamble, str):
        msg = "stored header has no text"
        raise StoreError(msg)
    try:
        return Header(preamble, _decode_fields(data.get("fields")))
    except AdifModelError as error:
        raise StoreError(f"stored header is invalid: {error}") from error


def _version(row: TupleRow) -> QsoVersion:
    (qso_id, version, action, deleted, fields, call, qso_start, band, mode) = row[:9]
    (changed_at, operator, station, note, import_id, import_file) = row[9:]
    return QsoVersion(
        qso_id=qso_id,
        version=version,
        action=Action(action),
        deleted=deleted,
        record=Record(_decode_fields(fields)),
        changed_at=changed_at,
        operator=operator,
        station=station,
        note=note,
        call=call,
        qso_start=qso_start,
        band=band,
        mode=mode,
        import_id=import_id,
        import_file=import_file,
    )


class LogStore:
    """The versioned, audited log (Principle VII) on one PostgreSQL connection.

    The connection must be in autocommit mode and use UTC (see
    :func:`loglab_engine.store.db.connect`); each method is one transaction.
    """

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation."""
        if not conn.autocommit:
            msg = "LogStore needs an autocommit connection; it manages its own transactions"
            raise StoreError(msg)
        self._conn = conn

    # -- Writes: each is one transaction ------------------------------------

    def _changeset(self, actor: Actor, kind: str, note: str | None) -> int:
        if note is not None and (len(note) > _MAX_NOTE or "\x00" in note):
            msg = f"a note is at most {_MAX_NOTE} characters, without NUL"
            raise StoreError(msg)
        conn = self._conn
        conn.execute(
            "INSERT INTO log.operator (callsign) VALUES (%s) ON CONFLICT DO NOTHING",
            (actor.operator,),
        )
        conn.execute(
            "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING",
            (actor.station,),
        )
        row = conn.execute(
            """
            INSERT INTO log.changeset (operator_id, station_id, kind, note)
            SELECT o.id, s.id, %s, %s
            FROM log.operator o, log.station s
            WHERE o.callsign = %s AND s.name = %s
            RETURNING id
            """,
            (kind, note, actor.operator, actor.station),
        ).fetchone()
        if row is None:  # pragma: no cover - both rows were just ensured
            msg = "could not record the changeset"
            raise StoreError(msg)
        changeset_id: int = row[0]
        return changeset_id

    def import_document(
        self,
        document: AdifDocument,
        *,
        file_name: str,
        file_sha256: str,
        actor: Actor,
        note: str | None = None,
    ) -> ImportSummary:
        """Store every record of ``document`` as a new QSO, all or nothing.

        Each QSO gets a new unique ID (Principle XV). The file's name, SHA-256,
        and header are kept so the import can be exported back exactly.

        Raises:
            DuplicateImportError: A file with the same SHA-256 was imported before.
            StoreError: A record cannot be stored; nothing was stored.
        """
        encoded = [
            _encode_fields(record.fields, f"record {number}")
            for number, record in enumerate(document.records, start=1)
        ]
        header = _encode_header(document.header)
        qso_ids = tuple(uuid.uuid4() for _ in encoded)
        conn = self._conn
        try:
            with conn.transaction():
                existing = conn.execute(
                    "SELECT id FROM log.import_batch WHERE file_sha256 = %s",
                    (file_sha256,),
                ).fetchone()
                if existing is not None:
                    msg = f"this file was already imported, as import {existing[0]}"
                    raise DuplicateImportError(msg)
                changeset_id = self._changeset(actor, "import", note)
                batch = conn.execute(
                    """
                    INSERT INTO log.import_batch
                        (changeset_id, file_name, file_sha256, header, record_count)
                    VALUES (%s, %s, %s, %s, %s)
                    RETURNING id
                    """,
                    (changeset_id, file_name, file_sha256, header, len(encoded)),
                ).fetchone()
                if batch is None:  # pragma: no cover - RETURNING always yields a row
                    msg = "could not record the import"
                    raise StoreError(msg)
                import_id: int = batch[0]
                with conn.cursor() as cur:
                    cur.executemany(
                        """
                        INSERT INTO log.qso (id, import_batch_id, import_position)
                        VALUES (%s, %s, %s)
                        """,
                        [(qso_id, import_id, n) for n, qso_id in enumerate(qso_ids, start=1)],
                    )
                    cur.executemany(
                        """
                        INSERT INTO log.qso_version
                            (qso_id, version, changeset_id, action, deleted, fields)
                        VALUES (%s, 1, %s, 'create', false, %s)
                        """,
                        [
                            (qso_id, changeset_id, Jsonb(fields))
                            for qso_id, fields in zip(qso_ids, encoded, strict=True)
                        ],
                    )
        except errors.UniqueViolation as error:
            msg = "this file was already imported"
            raise DuplicateImportError(msg) from error
        return ImportSummary(import_id, qso_ids)

    def create_qso(
        self,
        record: Record,
        *,
        actor: Actor,
        qso_id: uuid.UUID | None = None,
        note: str | None = None,
    ) -> QsoVersion:
        """Enter one new QSO; it gets its unique ID now unless one is supplied.

        Raises:
            StoreError: The record cannot be stored, or ``qso_id`` is already used.
        """
        fields = _encode_fields(record.fields, "record")
        new_id = qso_id if qso_id is not None else uuid.uuid4()
        conn = self._conn
        try:
            with conn.transaction():
                changeset_id = self._changeset(actor, "create", note)
                conn.execute("INSERT INTO log.qso (id) VALUES (%s)", (new_id,))
                conn.execute(
                    """
                    INSERT INTO log.qso_version
                        (qso_id, version, changeset_id, action, deleted, fields)
                    VALUES (%s, 1, %s, 'create', false, %s)
                    """,
                    (new_id, changeset_id, Jsonb(fields)),
                )
        except errors.UniqueViolation as error:
            msg = f"QSO {new_id} already exists"
            raise StoreError(msg) from error
        return self.current(new_id)

    def _append(
        self,
        qso_id: uuid.UUID,
        action: Action,
        actor: Actor,
        note: str | None,
        edit: tuple[Record, int] | None = None,
    ) -> QsoVersion:
        """Append the next version; ``edit`` is the new record and the version it was based on."""
        conn = self._conn
        with conn.transaction():
            row = conn.execute(
                """
                SELECT q.current_version, q.deleted, v.fields
                FROM log.qso q
                JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
                WHERE q.id = %s
                FOR UPDATE OF q
                """,
                (qso_id,),
            ).fetchone()
            if row is None:
                raise QsoNotFoundError(f"no QSO has ID {qso_id}")
            current, deleted, previous = row
            if edit is not None and edit[1] != current:
                msg = (
                    f"QSO {qso_id} is at version {current}, not {edit[1]}; "
                    "someone changed it first, so look again before editing"
                )
                raise StaleVersionError(msg)
            if action is Action.EDIT and deleted:
                raise StoreError(f"QSO {qso_id} is deleted; restore it before editing")
            if action is Action.DELETE and deleted:
                raise StoreError(f"QSO {qso_id} is already deleted")
            if action is Action.RESTORE and not deleted:
                raise StoreError(f"QSO {qso_id} is not deleted")
            fields = previous
            if edit is not None:
                fields = _encode_fields(edit[0].fields, "record")
                if fields == previous:
                    raise StoreError("the edit changes nothing")
            changeset_id = self._changeset(actor, action.value, note)
            conn.execute(
                """
                INSERT INTO log.qso_version
                    (qso_id, version, changeset_id, action, deleted, fields)
                VALUES (%s, %s, %s, %s, %s, %s)
                """,
                (
                    qso_id,
                    current + 1,
                    changeset_id,
                    action.value,
                    action is Action.DELETE,
                    Jsonb(fields),
                ),
            )
        return self.current(qso_id)

    def edit_qso(
        self,
        qso_id: uuid.UUID,
        record: Record,
        *,
        actor: Actor,
        base_version: int,
        note: str | None = None,
    ) -> QsoVersion:
        """Replace a QSO's fields with ``record``, as a new version.

        ``base_version`` is the version the edit was made from; if the QSO has
        moved on since, nothing is changed.

        Raises:
            QsoNotFoundError: No such QSO.
            StaleVersionError: The QSO is no longer at ``base_version``.
            StoreError: The QSO is deleted, or the edit changes nothing.
        """
        return self._append(qso_id, Action.EDIT, actor, note, (record, base_version))

    def delete_qso(
        self,
        qso_id: uuid.UUID,
        *,
        actor: Actor,
        note: str | None = None,
    ) -> QsoVersion:
        """Mark a QSO deleted, as a new version. Its fields and history are kept.

        Raises:
            QsoNotFoundError: No such QSO.
            StoreError: The QSO is already deleted.
        """
        return self._append(qso_id, Action.DELETE, actor, note)

    def restore_qso(
        self,
        qso_id: uuid.UUID,
        *,
        actor: Actor,
        note: str | None = None,
    ) -> QsoVersion:
        """Recover a deleted QSO, as a new version with the same fields.

        Raises:
            QsoNotFoundError: No such QSO.
            StoreError: The QSO is not deleted.
        """
        return self._append(qso_id, Action.RESTORE, actor, note)

    # -- Reads ----------------------------------------------------------------

    def resolve(self, text: str) -> uuid.UUID:
        """Turn a full QSO ID, or a unique prefix of at least 8 characters, into an ID.

        Raises:
            QsoNotFoundError: Nothing matches, or the prefix matches several QSOs.
        """
        prefix = text.strip().lower()
        if not _HEX_PREFIX.match(prefix):
            raise QsoNotFoundError(f"{text!r} is not a QSO ID or an 8+ character prefix of one")
        rows = self._conn.execute(
            "SELECT id FROM log.qso WHERE id::text LIKE %s ORDER BY entry_seq LIMIT 2",
            (prefix + "%",),
        ).fetchall()
        if not rows:
            raise QsoNotFoundError(f"no QSO ID starts with {prefix}")
        if len(rows) > 1:
            raise QsoNotFoundError(f"more than one QSO ID starts with {prefix}; give more of it")
        found: uuid.UUID = rows[0][0]
        return found

    def current(self, qso_id: uuid.UUID) -> QsoVersion:
        """The QSO as it stands now (which may be deleted).

        Raises:
            QsoNotFoundError: No such QSO.
        """
        row = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            {_VERSION_JOINS}
            WHERE q.id = %s
            """,  # noqa: S608 - constant fragments only; values are parameters
            (qso_id,),
        ).fetchone()
        if row is None:
            raise QsoNotFoundError(f"no QSO has ID {qso_id}")
        return _version(row)

    def history(self, qso_id: uuid.UUID) -> tuple[QsoVersion, ...]:
        """Every version of a QSO, oldest first.

        Raises:
            QsoNotFoundError: No such QSO.
        """
        rows = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso_version v
            {_VERSION_JOINS}
            WHERE v.qso_id = %s
            ORDER BY v.version
            """,  # noqa: S608 - constant fragments only; values are parameters
            (qso_id,),
        ).fetchall()
        if not rows:
            raise QsoNotFoundError(f"no QSO has ID {qso_id}")
        return tuple(_version(row) for row in rows)

    def find(
        self,
        *,
        call: str | None = None,
        limit: int = 20,
        include_deleted: bool = False,
    ) -> tuple[QsoVersion, ...]:
        """Current QSOs in the order they were entered, optionally only those with ``call``."""
        rows = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            {_VERSION_JOINS}
            WHERE (%(call)s::text IS NULL OR v.call = upper(btrim(%(call)s::text)))
              AND (%(deleted)s OR NOT q.deleted)
            ORDER BY q.entry_seq
            LIMIT %(limit)s
            """,  # noqa: S608 - constant fragments only; values are parameters
            {"call": call, "deleted": include_deleted, "limit": max(limit, 0)},
        ).fetchall()
        return tuple(_version(row) for row in rows)

    def import_batch(self, import_id: int) -> ImportBatch:
        """An import's identity and original header.

        Raises:
            StoreError: No such import.
        """
        row = self._conn.execute(
            """
            SELECT b.id, b.file_name, b.file_sha256, b.header, b.record_count,
                   c.changed_at, o.callsign, s.name
            FROM log.import_batch b
            JOIN log.changeset c ON c.id = b.changeset_id
            JOIN log.operator o ON o.id = c.operator_id
            JOIN log.station s ON s.id = c.station_id
            WHERE b.id = %s
            """,
            (import_id,),
        ).fetchone()
        if row is None:
            raise StoreError(f"there is no import {import_id}")
        batch_id, file_name, sha256, header, count, imported_at, operator, station = row
        return ImportBatch(
            batch_id,
            file_name,
            sha256,
            _decode_header(header),
            count,
            imported_at,
            operator,
            station,
        )

    def _current_records(self, where: str, params: tuple[object, ...]) -> tuple[Record, ...]:
        rows = self._conn.execute(
            f"""
            SELECT v.fields
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            WHERE NOT q.deleted AND {where}
            ORDER BY q.import_position, q.entry_seq
            """,  # noqa: S608 - callers pass constant conditions; values are parameters
            params,
        ).fetchall()
        return tuple(Record(_decode_fields(row[0])) for row in rows)

    def export_import(self, import_id: int) -> AdifDocument:
        """An import as it stands now: its original header and its QSOs in file order.

        If none of its QSOs has been edited or deleted, this is the document that
        was imported, field for field (Principle IV). Deleted QSOs are left out.

        Raises:
            StoreError: No such import.
        """
        with self._conn.transaction():
            # One snapshot for the header and the records.
            self._conn.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            batch = self.import_batch(import_id)
            records = self._current_records("q.import_batch_id = %s", (import_id,))
        return AdifDocument(batch.header, records)

    def export_log(self, header: Header) -> AdifDocument:
        """Every QSO that is not deleted, in the order entered, under ``header``."""
        rows = self._conn.execute(
            """
            SELECT v.fields
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            WHERE NOT q.deleted
            ORDER BY q.entry_seq
            """,
        ).fetchall()
        return AdifDocument(header, tuple(Record(_decode_fields(row[0])) for row in rows))
