# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""``loglab-log``: the log in PostgreSQL, from the command line.

Migrate the schema, import ADI files, look at QSOs and their history, edit,
delete, and restore them through the audited path (Principle VII), and export
ADI. Every change is made by the operator signed in with ``loglab-operator
sign-in``, at their station (Principle XIII). All times are shown in UTC
(Principle VIII).

A QSO is logged once the Engine has committed it to the database (Principle XV).

Exit codes: 0 success, 1 export differs from the imported file, 2 refused or failed.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Final, TextIO

import psycopg

from loglab_engine import __version__
from loglab_engine.adif import (
    AdifModelError,
    AdiParseError,
    Field,
    Header,
    Record,
    parse_adi,
    write_adi,
)
from loglab_engine.cli.adif import write_atomically
from loglab_engine.cli.operator import RefusedError, check_schema, signed_in
from loglab_engine.store import (
    Actor,
    IdentityStore,
    LogStore,
    MigrationError,
    Permission,
    QsoVersion,
    StoreError,
    connect,
    default_database,
    load_migrations,
    migrate,
    pending_migrations,
)

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from psycopg.rows import TupleRow

EXIT_OK: Final = 0
EXIT_DIFFERENT: Final = 1
EXIT_REFUSED: Final = 2
ADIF_VERSION: Final = "3.1.7"
_MAX_LISTED: Final = 20


class _RefusedError(Exception):
    """A command cannot go ahead; the message says why."""


def _utc(moment: datetime) -> str:
    return moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _describe(field: Field) -> str:
    indicator = f":{field.type_indicator}" if field.type_indicator else ""
    return f"<{field.name}{indicator}> {field.value!r}"


def _summary(version: QsoVersion) -> str:
    start = _utc(version.qso_start) if version.qso_start else "(no date/time)"
    flag = "  DELETED" if version.deleted else ""
    return (
        f"{version.qso_id}  v{version.version}  {start}  {version.call or '?':<12} "
        f"{version.band or '?':<6} {version.mode or '?'}{flag}"
    )


def _audit(version: QsoVersion) -> str:
    source = f"  (import {version.import_id}: {version.import_file})" if version.import_id else ""
    note = f"  note: {version.note}" if version.note else ""
    return (
        f"v{version.version}  {_utc(version.changed_at)}  {version.action.value:<7}  "
        f"by {version.operator} at {version.station}{source}{note}"
    )


def _changes(before: Record, after: Record) -> list[str]:
    """Fields removed and added between two versions, as a multiset, in field order."""
    removed = Counter(before.fields) - Counter(after.fields)
    added = Counter(after.fields) - Counter(before.fields)
    lines = []
    for field in before.fields:
        if removed[field] > 0:
            removed[field] -= 1
            lines.append(f"      - {_describe(field)}")
    for field in after.fields:
        if added[field] > 0:
            added[field] -= 1
            lines.append(f"      + {_describe(field)}")
    return lines


def _actor(args: argparse.Namespace) -> Actor:
    actor: Actor = args.actor
    return actor


def _edited(record: Record, sets: Sequence[str], removes: Sequence[str]) -> Record:
    """Apply ``--set NAME=VALUE`` and ``--remove NAME`` to a copy of ``record``.

    ``--set`` replaces the data of the first field with that name, keeping its
    position, spelling, and type indicator, or adds the field at the end.
    ``--remove`` takes out every field with that name.
    """
    fields = list(record.fields)
    for item in sets:
        name, sep, value = item.partition("=")
        if not sep:
            raise _RefusedError(f"--set {item!r} is not NAME=VALUE")
        key = name.upper()
        index = next((i for i, f in enumerate(fields) if f.key == key), None)
        if index is None:
            fields.append(Field(name, value))
        else:
            fields[index] = Field(fields[index].name, value, fields[index].type_indicator)
    for name in removes:
        key = name.upper()
        if not any(f.key == key for f in fields):
            raise _RefusedError(f"--remove {name}: the QSO has no such field")
        fields = [f for f in fields if f.key != key]
    if not fields:
        raise _RefusedError("a QSO must keep at least one field")
    return Record(tuple(fields))


# -- Commands ----------------------------------------------------------------


def _migrate(conn: psycopg.Connection[TupleRow], args: argparse.Namespace, out: TextIO) -> int:
    migrations = load_migrations()
    if args.check:
        pending = pending_migrations(conn, migrations)
        for migration in pending:
            out.write(f"pending  {migration.label}\n")
        out.write(f"{len(pending)} pending of {len(migrations)} migrations\n")
        return EXIT_DIFFERENT if pending else EXIT_OK
    applied = migrate(conn, migrations)
    for migration in applied:
        out.write(f"applied  {migration.label}  sha256 {migration.sha256}\n")
    out.write(f"Schema is at version {migrations[-1].version if migrations else 0}")
    out.write(f" ({len(applied)} applied now)\n")
    return EXIT_OK


def _import(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    path: Path = args.file
    try:
        data = path.read_bytes()
    except OSError as error:
        raise _RefusedError(f"{path}: {error.strerror or error}") from error
    try:
        result = parse_adi(data)
    except AdiParseError as error:
        raise _RefusedError(f"{path}: rejected: {error}") from error
    document = result.document
    summary = store.import_document(
        document,
        file_name=path.name,
        file_sha256=hashlib.sha256(data).hexdigest(),
        actor=_actor(args),
        note=args.note,
    )
    out.write(
        f"Imported {len(summary.qso_ids)} QSOs ({document.field_count} fields) from {path.name} "
        f"as import {summary.import_id}, by {args.actor.operator} at {args.actor.station}.\n",
    )
    if write_adi(document) == data:
        out.write("The file is canonical ADI, so exporting this import reproduces it exactly.\n")
    else:
        out.write(
            "The file's layout is not canonical ADI (spacing, line breaks, or leading zeros),\n"
            "so exporting this import reproduces every field exactly but not every byte.\n",
        )
    for warning in result.warnings[:_MAX_LISTED]:
        where = "header" if warning.record is None else f"record {warning.record}"
        out.write(f"  warning: {where} {warning.field or ''}: {warning.message}\n")
    return EXIT_OK


def _export(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    if args.import_id is not None:
        batch = store.import_batch(args.import_id)
        document = store.export_import(args.import_id)
        data = write_adi(document)
        write_atomically(args.output, data)
        out.write(
            f"Wrote import {batch.import_id} ({len(document.records)} QSOs) to {args.output}\n"
        )
        if hashlib.sha256(data).hexdigest() == batch.file_sha256:
            out.write(
                f"Identical to the imported file {batch.file_name}: SHA-256 {batch.file_sha256}\n",
            )
            return EXIT_OK
        out.write(
            f"Differs from the imported file {batch.file_name}: some of its QSOs were edited or "
            "deleted since, or the file was not canonical ADI.\n",
        )
        return EXIT_DIFFERENT
    now = datetime.now(UTC)
    header = Header(
        f"LogLab export of the whole log, {_utc(now)}.\n",
        (
            Field("ADIF_VER", ADIF_VERSION),
            Field("CREATED_TIMESTAMP", now.strftime("%Y%m%d %H%M%S")),
            Field("PROGRAMID", "LogLab"),
            Field("PROGRAMVERSION", __version__),
        ),
    )
    document = store.export_log(header)
    write_atomically(args.output, write_adi(document))
    out.write(
        f"Wrote the whole log ({len(document.records)} QSOs, deleted ones left out) "
        f"to {args.output}\n",
    )
    return EXIT_OK


def _find(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    found = store.find(call=args.call, limit=args.limit, include_deleted=args.deleted)
    for version in found:
        out.write(_summary(version) + "\n")
    out.write(f"{len(found)} QSO(s) shown\n")
    return EXIT_OK


def _show(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    version = store.current(store.resolve(args.qso))
    out.write(_summary(version) + "\n")
    out.write(_audit(version) + "\n")
    for field in version.record.fields:
        out.write(f"  {_describe(field)}\n")
    return EXIT_OK


def _history(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    versions = store.history(store.resolve(args.qso))
    out.write(f"QSO {versions[0].qso_id}: {len(versions)} version(s), oldest first\n")
    previous: Record | None = None
    for version in versions:
        out.write(_audit(version) + "\n")
        if previous is None:
            out.write(f"      {len(version.record.fields)} fields as first entered\n")
        else:
            lines = _changes(previous, version.record)
            out.write("\n".join(lines) + "\n" if lines else "      fields unchanged\n")
        previous = version.record
    return EXIT_OK


def _edit(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    qso_id = store.resolve(args.qso)
    before = store.current(qso_id)
    record = _edited(before.record, args.set or [], args.remove or [])
    after = store.edit_qso(
        qso_id,
        record,
        actor=_actor(args),
        base_version=before.version,
        note=args.note,
    )
    out.write(f"Edited QSO {qso_id}: now version {after.version}\n")
    for line in _changes(before.record, after.record):
        out.write(line + "\n")
    return EXIT_OK


def _delete(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    after = store.delete_qso(store.resolve(args.qso), actor=_actor(args), note=args.note)
    out.write(
        f"Deleted QSO {after.qso_id}: version {after.version} marks it deleted; "
        "its fields and history are kept and 'restore' brings it back.\n",
    )
    return EXIT_OK


def _restore(store: LogStore, args: argparse.Namespace, out: TextIO) -> int:
    after = store.restore_qso(store.resolve(args.qso), actor=_actor(args), note=args.note)
    out.write(f"Restored QSO {after.qso_id}: version {after.version}\n")
    return EXIT_OK


# Changes need the log permission (the database checks it too); reading needs view.
_NEEDS: Final = {
    "import": Permission.LOG,
    "edit": Permission.LOG,
    "delete": Permission.LOG,
    "restore": Permission.LOG,
}

_STORE_COMMANDS: Final[dict[str, Callable[[LogStore, argparse.Namespace, TextIO], int]]] = {
    "import": _import,
    "export": _export,
    "find": _find,
    "show": _show,
    "history": _history,
    "edit": _edit,
    "delete": _delete,
    "restore": _restore,
}


def _add_actor(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--note", help="why, recorded with the change")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="loglab-log",
        description="LogLab's log in PostgreSQL: versioned, audited, never erased.",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument(
        "--database",
        default=None,
        help="libpq connection string or URL (default: $LOGLAB_DATABASE, else dbname=loglab)",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    migrate_cmd = commands.add_parser("migrate", help="apply pending schema migrations")
    migrate_cmd.add_argument(
        "--check",
        action="store_true",
        help="only list pending migrations; exit 1 if there are any",
    )

    import_cmd = commands.add_parser("import", help="store every QSO of an ADI file")
    import_cmd.add_argument("file", type=Path)
    _add_actor(import_cmd)

    export = commands.add_parser("export", help="write QSOs that are not deleted as ADI")
    export.add_argument("-o", "--output", type=Path, required=True, help="ADI file to write")
    export.add_argument(
        "--import",
        dest="import_id",
        type=int,
        help="export only this import, with its original header",
    )

    find = commands.add_parser("find", help="list QSOs in the order entered")
    find.add_argument("--call", help="only QSOs with this CALL")
    find.add_argument("-n", "--limit", type=int, default=20)
    find.add_argument("--deleted", action="store_true", help="include deleted QSOs")

    for name, text in (("show", "show a QSO's fields"), ("history", "show every version")):
        command = commands.add_parser(name, help=text)
        command.add_argument("qso", help="QSO ID, or its first 8+ characters")

    edit = commands.add_parser("edit", help="change a QSO's fields, as a new version")
    edit.add_argument("qso", help="QSO ID, or its first 8+ characters")
    edit.add_argument("--set", action="append", metavar="NAME=VALUE", help="set a field")
    edit.add_argument("--remove", action="append", metavar="NAME", help="remove a field")
    _add_actor(edit)

    for name, text in (
        ("delete", "mark a QSO deleted (recoverable)"),
        ("restore", "recover a deleted QSO"),
    ):
        command = commands.add_parser(name, help=text)
        command.add_argument("qso", help="QSO ID, or its first 8+ characters")
        _add_actor(command)
    return parser


def main(
    argv: Sequence[str] | None = None,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Run the command line and return its exit code."""
    args = _parser().parse_args(argv)
    try:
        with connect(args.database or default_database()) as conn:
            if args.command == "migrate":
                return _migrate(conn, args, out)
            check_schema(conn)
            identity = IdentityStore(conn)
            args.actor = signed_in(identity)
            identity.require(args.actor, _NEEDS.get(args.command, Permission.VIEW))
            return _STORE_COMMANDS[args.command](LogStore(conn), args, out)
    except (_RefusedError, RefusedError, StoreError, MigrationError, AdifModelError) as error:
        err.write(f"refused: {error}\n")
    except psycopg.Error as error:
        err.write(f"database error: {str(error).strip()}\n")
    return EXIT_REFUSED


def run() -> None:
    """Console-script entry point."""
    sys.exit(main())
