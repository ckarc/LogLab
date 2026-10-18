# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""``loglab-engine``: run the Engine's API and web client.

``loglab-engine serve`` listens on this machine only by default. To reach it
from other computers on the local network, give ``--host 0.0.0.0``; it needs
no internet connection (Principle V).

Exit codes: 0 stopped normally, 2 refused or failed to start.
"""

from __future__ import annotations

import argparse
import sys
from typing import TYPE_CHECKING, Final, TextIO

import psycopg
import uvicorn

from loglab_engine import __version__
from loglab_engine.api import create_app
from loglab_engine.cli.operator import RefusedError, check_schema
from loglab_engine.store import connect, default_database

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
    return parser


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
    out.write(f"LogLab Engine {__version__}: http://{args.host}:{args.port}/\n")
    out.flush()
    uvicorn.run(create_app(database), host=args.host, port=args.port, log_level="info")
    return EXIT_OK


def run() -> None:
    """Console-script entry point."""
    sys.exit(main())
