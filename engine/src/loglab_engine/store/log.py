# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The log in PostgreSQL: QSOs as versioned, audited ADIF snapshots.

:class:`LogStore` is the only way the Engine writes QSOs. Every write is one
transaction (Principle XIV) that records a changeset (operator, station, UTC
time, kind) and appends complete snapshots; nothing is updated in place or
erased (Principle VII). The database enforces the same rules, so a bug here
fails instead of silently changing the log.

A QSO is *logged* once its transaction commits (Principle XV): the database
has it on disk.
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

# What the log can be sorted by: a fixed column, or ``field:NAME`` for any ADIF
# field. Each maps to a constant SQL expression; a field's name is passed as a
# parameter, never written into the SQL.
_SORT_SQL: Final = {
    "entered": "q.entry_seq",
    "qso_start": "v.qso_start",
    "call": "v.call",
    # Bands by frequency, lowest first: by wavelength, longest first.
    "band": (
        "-(CASE WHEN v.band ~ '^[0-9]+([.][0-9]+)?mm$' THEN rtrim(v.band, 'm')::numeric / 1000"
        " WHEN v.band ~ '^[0-9]+([.][0-9]+)?cm$' THEN rtrim(v.band, 'cm')::numeric / 100"
        " WHEN v.band ~ '^[0-9]+([.][0-9]+)?m$' THEN rtrim(v.band, 'm')::numeric END)"
    ),
    "mode": "v.mode",
    "operator": "o.callsign",
    "station": "s.name",
    "version": "q.current_version",
    "changed_at": "c.changed_at",
}
# Numbers (such as FREQ) sort as numbers; everything else as text.
_FIELD_SORT_SQL: Final = (
    "(CASE WHEN log.field_value(v.fields, %(field)s) ~ '^-?[0-9]+([.][0-9]+)?$'"
    " THEN log.field_value(v.fields, %(field)s)::numeric END) {dir} NULLS LAST,"
    " upper(log.field_value(v.fields, %(field)s)) {dir} NULLS LAST"
)
_SORT_FIELD: Final = re.compile(r"^field:([A-Za-z][A-Za-z0-9_]{0,63})$")


def _order_by(sort: str, *, descending: bool) -> tuple[str, str | None]:
    """The ORDER BY clause for ``sort``, and the ADIF field name it sorts by, if any.

    Ties keep the order the QSOs were entered.

    Raises:
        StoreError: ``sort`` is not something the log can be sorted by.
    """
    direction = "DESC" if descending else "ASC"
    tiebreak = f"q.entry_seq {direction}"
    if sort in _SORT_SQL:
        if sort == "entered":
            return tiebreak, None
        return f"{_SORT_SQL[sort]} {direction} NULLS LAST, {tiebreak}", None
    match = _SORT_FIELD.match(sort)
    if match is None:
        raise StoreError(f"the log cannot be sorted by {sort!r}")
    return f"{_FIELD_SORT_SQL.format(dir=direction)}, {tiebreak}", match.group(1).upper()


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


# The call a QSO was made as: its STATION_CALLSIGN, else its OPERATOR, else the
# operator who entered it.
_STATION_CALL: Final = """
    coalesce(
        nullif(upper(btrim(log.field_value(v.fields, 'STATION_CALLSIGN'))), ''),
        nullif(upper(btrim(log.field_value(v.fields, 'OPERATOR'))), ''),
        (SELECT o1.callsign FROM log.qso_version v1
         JOIN log.changeset c1 ON c1.id = v1.changeset_id
         JOIN log.operator o1 ON o1.id = c1.operator_id
         WHERE v1.qso_id = q.id AND v1.version = 1)
    )
"""
_QSL_WAITING: Final = f"""
    NOT q.deleted
    AND coalesce(upper(btrim(log.field_value(v.fields, %(sent)s))), '') NOT IN ('Y', 'N', 'I')
    AND {_STATION_CALL} = %(call)s
"""
_SERVICE: Final = re.compile(r"^[A-Z]{2,12}$")


class StoreError(Exception):
    """A request to the log cannot be carried out; the log is unchanged."""


class QsoNotFoundError(StoreError):
    """No QSO has the given ID."""


class StaleVersionError(StoreError):
    """The QSO changed since the version the caller based its edit on."""


class NotAuthorizedError(StoreError):
    """The operator is not signed in, or does not hold the permission this needs."""


class QsoExistsError(StoreError):
    """A new QSO was given an ID that a different QSO already has."""


class DuplicateImportError(StoreError):
    """This exact file has been imported before."""


class Action(StrEnum):
    """What a QSO version did."""

    CREATE = "create"
    EDIT = "edit"
    DELETE = "delete"
    RESTORE = "restore"


def check_callsign(callsign: str) -> None:
    """Refuse a callsign the log could not record.

    Raises:
        StoreError: Not an upper-case callsign of at most 20 characters.
    """
    if len(callsign) > _MAX_CALLSIGN or not _CALLSIGN.match(callsign):
        msg = f"{callsign!r} is not an upper-case callsign"
        raise StoreError(msg)


def check_station(station: str) -> None:
    """Refuse a station name the log could not record.

    Raises:
        StoreError: Not 1-64 letters, digits, '.', '_' or '-'.
    """
    if not _STATION.match(station):
        msg = f"station {station!r} must be 1-64 letters, digits, '.', '_' or '-'"
        raise StoreError(msg)


@dataclass(frozen=True, slots=True)
class Actor:
    """A signed-in operator at a station, making a change in their session (Principle XIII).

    Get one from :meth:`loglab_engine.store.IdentityStore.authenticate`. The
    database checks every change against the session, so an actor that was
    not signed in is refused.
    """

    operator: str
    station: str
    session_id: uuid.UUID

    def __post_init__(self) -> None:
        """Refuse identities the log could not record."""
        check_callsign(self.operator)
        check_station(self.station)


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
        try:
            row = self._conn.execute(
                """
                INSERT INTO log.changeset (operator_id, station_id, session_id, kind, note)
                SELECT o.id, s.id, %s, %s, %s
                FROM log.operator o, log.station s
                WHERE o.callsign = %s AND s.name = %s
                RETURNING id
                """,
                (actor.session_id, kind, note, actor.operator, actor.station),
            ).fetchone()
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error
        if row is None:
            msg = f"{actor.operator} at {actor.station} is not signed in (Principle XIII)"
            raise NotAuthorizedError(msg)
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
            raise QsoExistsError(msg) from error
        return self.current(new_id)

    def enter_qso(
        self,
        record: Record,
        *,
        actor: Actor,
        qso_id: uuid.UUID,
        note: str | None = None,
    ) -> tuple[QsoVersion, bool]:
        """Enter a QSO under the ID its client gave it, or recognise a re-send of it.

        A client keeps each QSO in its journal until it is logged and sends it
        again if it is unsure the Engine got it. Re-sending the same ID with the
        same fields creates nothing (Principle XV).

        Returns:
            The QSO as it stands now, and whether this call created it.

        Raises:
            QsoExistsError: A different QSO already has ``qso_id``.
            StoreError: The record cannot be stored.
        """
        try:
            return self.create_qso(record, actor=actor, qso_id=qso_id, note=note), True
        except QsoExistsError:
            first = self.history(qso_id)[0]
            if first.action is not Action.CREATE or _encode_fields(
                first.record.fields,
                "record",
            ) != _encode_fields(record.fields, "record"):
                msg = f"a different QSO already has the ID {qso_id}"
                raise QsoExistsError(msg) from None
            return self.current(qso_id), False

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

    def edit_many(
        self,
        edits: Iterable[tuple[uuid.UUID, int, Record]],
        *,
        actor: Actor,
        note: str | None = None,
    ) -> tuple[uuid.UUID, ...]:
        """Replace several QSOs' fields as new versions, in one changeset and transaction.

        Each edit is a QSO's ID, the version it was made from, and its new
        record. A QSO that has moved on from that version, is deleted, or would
        not change is left as it is, so a background update never overwrites
        an operator's edit (Principle VII).

        Returns:
            The IDs of the QSOs that were changed.

        Raises:
            StoreError: A record cannot be stored; nothing was changed.
        """
        encoded = [(qso_id, base, _encode_fields(r.fields, "record")) for qso_id, base, r in edits]
        if not encoded:
            return ()
        conn = self._conn
        changed: list[uuid.UUID] = []
        with conn.transaction():
            rows = conn.execute(
                """
                SELECT q.id, q.current_version, q.deleted, v.fields
                FROM log.qso q
                JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
                WHERE q.id = ANY(%s)
                FOR UPDATE OF q
                """,
                ([qso_id for qso_id, _, _ in encoded],),
            ).fetchall()
            state = {uuid.UUID(str(r[0])): (int(r[1]), bool(r[2]), r[3]) for r in rows}
            todo = [
                (qso_id, base, fields)
                for qso_id, base, fields in encoded
                if qso_id in state
                and state[qso_id][0] == base
                and not state[qso_id][1]
                and state[qso_id][2] != fields
            ]
            if not todo:
                return ()
            changeset_id = self._changeset(actor, Action.EDIT.value, note)
            with conn.cursor() as cur:
                cur.executemany(
                    """
                    INSERT INTO log.qso_version
                        (qso_id, version, changeset_id, action, deleted, fields)
                    VALUES (%s, %s, %s, 'edit', false, %s)
                    """,
                    [(qso_id, base + 1, changeset_id, Jsonb(f)) for qso_id, base, f in todo],
                )
            changed = [qso_id for qso_id, _, _ in todo]
        return tuple(changed)

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

    def current_versions(self, qso_ids: Iterable[uuid.UUID]) -> dict[uuid.UUID, int]:
        """The current version number of each of ``qso_ids`` this log holds."""
        rows = self._conn.execute(
            "SELECT id, current_version FROM log.qso WHERE id = ANY(%s)",
            (list(qso_ids),),
        ).fetchall()
        return {uuid.UUID(str(qso_id)): int(version) for qso_id, version in rows}

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
        newest_first: bool = False,
        sort: str = "entered",
    ) -> tuple[QsoVersion, ...]:
        """Current QSOs in the order they were entered, optionally only those with ``call``.

        With ``newest_first``, the most recently entered come first: the
        ``limit`` latest QSOs rather than the earliest. ``sort`` orders them by
        another column instead (``call``, ``band``, ``qso_start``, ...) or by
        any ADIF field (``field:FREQ``); ``newest_first`` then reverses it.

        Raises:
            StoreError: ``sort`` is not something the log can be sorted by.
        """
        order_by, sort_field = _order_by(sort, descending=newest_first)
        rows = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            {_VERSION_JOINS}
            WHERE (%(call)s::text IS NULL OR v.call = upper(btrim(%(call)s::text)))
              AND (%(deleted)s OR NOT q.deleted)
            ORDER BY {order_by}
            LIMIT %(limit)s
            """,  # noqa: S608 - constant fragments only; values are parameters
            {
                "call": call,
                "deleted": include_deleted,
                "limit": max(limit, 0),
                "field": sort_field,
            },
        ).fetchall()
        return tuple(_version(row) for row in rows)

    def qsl_queue(self, service: str, station_call: str, limit: int) -> tuple[QsoVersion, ...]:
        """Current QSOs made as ``station_call`` that have not been sent to a QSL ``service``.

        ``service`` is the prefix of the ADIF ``*_QSL_SENT`` field, such as
        ``LOTW``. A QSO is waiting unless that field says it was sent (Y), is
        not to be sent (N), or is to be ignored (I). The call a QSO was made as
        is its ``STATION_CALLSIGN``, else its ``OPERATOR``, else the operator
        who entered it. The oldest-entered come first.

        Raises:
            StoreError: ``service`` is not a QSL service name.
        """
        if not _SERVICE.match(service):
            raise StoreError(f"{service!r} is not a QSL service")
        rows = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            {_VERSION_JOINS}
            WHERE {_QSL_WAITING}
            ORDER BY q.entry_seq
            LIMIT %(limit)s
            """,  # noqa: S608 - constant fragments only; values are parameters
            {"sent": f"{service}_QSL_SENT", "call": station_call, "limit": max(limit, 0)},
        ).fetchall()
        return tuple(_version(row) for row in rows)

    def qsl_waiting(self, service: str, station_call: str) -> int:
        """How many QSOs :meth:`qsl_queue` would list with no limit.

        Raises:
            StoreError: ``service`` is not a QSL service name.
        """
        if not _SERVICE.match(service):
            raise StoreError(f"{service!r} is not a QSL service")
        row = self._conn.execute(
            f"""
            SELECT count(*)
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            WHERE {_QSL_WAITING}
            """,  # noqa: S608 - constant fragments only; values are parameters
            {"sent": f"{service}_QSL_SENT", "call": station_call},
        ).fetchone()
        return 0 if row is None else int(row[0])

    def station_qsos(self, station_call: str, calls: Iterable[str]) -> tuple[QsoVersion, ...]:
        """Current QSOs made as ``station_call`` with any of ``calls`` (see :meth:`qsl_queue`)."""
        rows = self._conn.execute(
            f"""
            SELECT {_VERSION_COLUMNS}
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            {_VERSION_JOINS}
            WHERE NOT q.deleted AND v.call = ANY(%(calls)s) AND {_STATION_CALL} = %(call)s
            ORDER BY q.entry_seq
            """,  # noqa: S608 - constant fragments only; values are parameters
            {"calls": sorted(set(calls)), "call": station_call},
        ).fetchall()
        return tuple(_version(row) for row in rows)

    def field_counts(self, name: str) -> dict[str, int]:
        """How many current QSOs carry each value of the ADIF field ``name``.

        Values are compared ignoring case and surrounding spaces, and are
        returned in capitals. QSOs without the field (or with it empty) are
        counted under ``""``. Deleted QSOs are left out.
        """
        rows = self._conn.execute(
            """
            SELECT coalesce(upper(btrim(log.field_value(v.fields, %s))), ''), count(*)
            FROM log.qso q
            JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
            WHERE NOT q.deleted
            GROUP BY 1
            """,
            (name,),
        ).fetchall()
        return {str(value): int(count) for value, count in rows}

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
