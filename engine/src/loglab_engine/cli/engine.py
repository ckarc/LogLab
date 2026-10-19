# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""``loglab-engine``: run the Engine's API and web client.

``loglab-engine serve`` listens on this machine only by default. To reach it
from other computers on the local network, give ``--host 0.0.0.0``; it needs
no internet connection (Principle V). While it runs, it keeps the US and
Canadian license lists current whenever there is internet (unless given
``--no-license-refresh``).

``loglab-engine licenses status`` says how current those lists are;
``loglab-engine licenses import`` loads files downloaded elsewhere, for a
station that is never online; ``loglab-engine licenses refresh`` downloads
whatever is newer now.

Exit codes: 0 stopped normally, 2 refused or failed to start.
"""

from __future__ import annotations

import argparse
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Final, TextIO

import psycopg
import uvicorn

from loglab_engine import __version__
from loglab_engine.api import create_app
from loglab_engine.cli.operator import RefusedError, check_schema
from loglab_engine.reference.licenses import LicenseFileError
from loglab_engine.reference.refresh import HttpDownloads, Refresher
from loglab_engine.store import connect, default_database
from loglab_engine.store.licenses import COUNTRY, ImportResult, Kind, LicenseStore

if TYPE_CHECKING:
    from collections.abc import Sequence

EXIT_OK: Final = 0
EXIT_REFUSED: Final = 2
DEFAULT_PORT: Final = 8073


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="loglab-engine",
        description="Run the LogLab Engine: its API and the web client.",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument(
        "--database",
        default=None,
        help="libpq connection string or URL (default: $LOGLAB_DATABASE, else dbname=loglab)",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    serve = commands.add_parser("serve", help="serve the API and web client")
    serve.add_argument(
        "--host",
        default="127.0.0.1",
        help="address to listen on (default 127.0.0.1, this machine only)",
    )
    serve.add_argument("--port", type=int, default=DEFAULT_PORT, help="default 8073")
    serve.add_argument(
        "--no-license-refresh",
        action="store_true",
        help="never download the FCC and ISED license lists (load them with 'licenses import')",
    )
    licenses = commands.add_parser("licenses", help="the US and Canadian license lists")
    actions = licenses.add_subparsers(dest="action", required=True)
    actions.add_parser("status", help="how current the lists are")
    actions.add_parser("refresh", help="download and load whatever is newer, now")
    load = actions.add_parser("import", help="load files downloaded elsewhere")
    load.add_argument("--fcc", type=Path, help="the FCC's weekly l_amat.zip")
    load.add_argument(
        "--fcc-daily",
        type=Path,
        action="append",
        default=[],
        help="an FCC daily change file, l_am_<day>.zip (repeat, oldest first)",
    )
    load.add_argument("--canada", type=Path, help="ISED's amateur_delim.zip")
    return parser


def _said(result: ImportResult) -> str:
    what = "changes to" if result.kind is Kind.DAILY else "licenses in"
    line = f"loaded {result.records} {what} the {COUNTRY[result.source]} list"
    if result.skipped:
        line += f" ({result.skipped} unreadable records skipped"
        line += "".join(f"; {e}" for e in result.examples) + ")"
    return line


def _refresh(database: str, out: TextIO) -> None:
    report = Refresher(database, HttpDownloads(f"LogLab/{__version__}")).run_once()
    for result in report.imported:
        out.write(_said(result) + "\n")
    for problem in report.problems:
        out.write(f"not loaded: {problem}\n")
    if not report.imported and not report.problems:
        out.write("the lists are already current\n")


def _import(args: argparse.Namespace, store: LicenseStore, out: TextIO) -> None:
    if args.fcc is None and not args.fcc_daily and args.canada is None:
        msg = "give --fcc, --fcc-daily or --canada"
        raise RefusedError(msg)
    today = datetime.now(UTC).date()
    if args.fcc is not None:
        out.write(_said(store.import_fcc(args.fcc, Kind.FULL, today=today)) + "\n")
    for daily in args.fcc_daily:
        out.write(_said(store.import_fcc(daily, Kind.DAILY, today=today)) + "\n")
    if args.canada is not None:
        out.write(_said(store.import_ised(args.canada)) + "\n")


def _status(store: LicenseStore, out: TextIO) -> None:
    for state in store.states():
        line = f"{COUNTRY[state.source]}: {state.calls} calls"
        if state.full_imported is None:
            line += ", never loaded"
        else:
            line += f", loaded {state.full_imported:%Y-%m-%d %H:%M} UTC"
            if state.full_published is not None:
                line += f" from the list published {state.full_published:%Y-%m-%d}"
        if state.daily_imported is not None:
            line += f"; changes applied {state.daily_imported:%Y-%m-%d %H:%M} UTC"
        out.write(line + "\n")


def _licenses(args: argparse.Namespace, database: str, out: TextIO) -> None:
    if args.action == "refresh":
        _refresh(database, out)
        return
    with connect(database) as conn:
        store = LicenseStore(conn)
        if args.action == "import":
            _import(args, store, out)
        else:
            _status(store, out)


def main(
    argv: Sequence[str] | None = None,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Run the command line and return its exit code."""
    args = _parser().parse_args(argv)
    database = args.database or default_database()
    try:
        with connect(database) as conn:
            check_schema(conn)
    except RefusedError as error:
        err.write(f"refused: {error}\n")
        return EXIT_REFUSED
    except psycopg.Error as error:
        err.write(f"database error: {str(error).strip()}\n")
        return EXIT_REFUSED
    if args.command == "licenses":
        try:
            _licenses(args, database, out)
        except (RefusedError, LicenseFileError) as error:
            err.write(f"refused: {error}\n")
            return EXIT_REFUSED
        except psycopg.Error as error:
            err.write(f"database error: {str(error).strip()}\n")
            return EXIT_REFUSED
        return EXIT_OK
    out.write(f"LogLab Engine {__version__}: http://{args.host}:{args.port}/\n")
    out.flush()
    app = create_app(database)
    app.state.logbook.start()  # sends QSOs to QRZ logbooks, when operators set that up
    if not args.no_license_refresh:
        app.state.licenses.start()  # keeps the FCC and ISED lists current, when online
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")
    return EXIT_OK


def run() -> None:
    """Console-script entry point."""
    sys.exit(main())
