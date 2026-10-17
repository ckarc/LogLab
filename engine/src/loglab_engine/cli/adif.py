# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""``loglab-adif``: generate ghost QSOs, check ADI files, and prove round trips.

Exit codes: 0 success, 1 round trip not identical, 2 input rejected or not found.
"""

from __future__ import annotations

import argparse
import os
import stat
import sys
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, TextIO

from loglab_engine import __version__
from loglab_engine.adif import AdiParseError, differences, parse_adi, write_adi
from loglab_engine.adif.ghosts import DEFAULT_START, generate

if TYPE_CHECKING:
    from collections.abc import Sequence

    from loglab_engine.adif import ParseResult

EXIT_OK = 0
EXIT_DIFFERENT = 1
EXIT_REJECTED = 2
_MAX_LISTED = 20


def _mode_for(path: Path) -> int:
    """Permission bits for ``path``: its current ones, or the default for new files."""
    try:
        return stat.S_IMODE(path.stat().st_mode)
    except FileNotFoundError:
        umask = os.umask(0)
        os.umask(umask)
        return 0o666 & ~umask


def write_atomically(path: Path, data: bytes) -> None:
    """Write ``data`` to a temporary file beside ``path``, then rename it into place.

    A reader never sees a half-written file, even after a crash (Principle XIV).
    The result gets the permissions an ordinary new file would, or keeps those of
    the file it replaces.
    """
    path = path.resolve()
    fd, temp_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    temp = Path(temp_name)
    try:
        temp.chmod(_mode_for(path))
        with os.fdopen(fd, "wb") as file:
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        temp.replace(path)
    except BaseException:
        temp.unlink(missing_ok=True)
        raise


def _utc(text: str) -> datetime:
    """Parse an ISO 8601 time that must say it is UTC, e.g. 2026-06-27T18:00Z."""
    parsed = datetime.fromisoformat(text)
    if parsed.tzinfo is None or parsed.utcoffset() != UTC.utcoffset(None):
        msg = f"{text!r} is not UTC; add 'Z' (Principle VIII)"
        raise argparse.ArgumentTypeError(msg)
    return parsed.astimezone(UTC)


def _read(path: Path, err: TextIO) -> ParseResult | None:
    try:
        return parse_adi(path.read_bytes())
    except OSError as error:
        err.write(f"{path}: {error.strerror or error}\n")
    except AdiParseError as error:
        err.write(f"{path}: rejected: {error}\n")
    return None


def _report_warnings(result: ParseResult, out: TextIO) -> None:
    for warning in result.warnings[:_MAX_LISTED]:
        where = "header" if warning.record is None else f"record {warning.record}"
        field = f" {warning.field}" if warning.field else ""
        out.write(f"  warning: {where}{field}: {warning.message}\n")
    if len(result.warnings) > _MAX_LISTED:
        out.write(f"  ... and {len(result.warnings) - _MAX_LISTED} more warnings\n")


def _ghosts(args: argparse.Namespace, out: TextIO) -> int:
    document = generate(args.count, seed=args.seed, start=args.start)
    write_atomically(args.output, write_adi(document))
    out.write(
        f"Wrote {len(document.records)} ghost QSOs ({document.field_count} fields) "
        f"to {args.output}\n",
    )
    return EXIT_OK


def _check(args: argparse.Namespace, out: TextIO, err: TextIO) -> int:
    result = _read(args.file, err)
    if result is None:
        return EXIT_REJECTED
    document = result.document
    header = "a header" if document.header else "no header"
    out.write(
        f"{args.file}: valid ADI with {header}, {len(document.records)} records, "
        f"{document.field_count} fields, {len(result.warnings)} warnings\n",
    )
    _report_warnings(result, out)
    return EXIT_OK


def _roundtrip(args: argparse.Namespace, out: TextIO, err: TextIO) -> int:
    first = _read(args.file, err)
    if first is None:
        return EXIT_REJECTED
    write_atomically(args.output, write_adi(first.document))
    second = _read(args.output, err)
    if second is None:
        return EXIT_REJECTED
    found = differences(first.document, second.document)
    if found:
        out.write(f"NOT identical: {len(found)} differences\n")
        for line in found[:_MAX_LISTED]:
            out.write(f"  {line}\n")
        return EXIT_DIFFERENT
    document = first.document
    out.write(
        f"Round trip identical: {len(document.records)} records, "
        f"{document.field_count} fields, every name, value, and type unchanged.\n"
        f"Wrote {args.output}\n",
    )
    _report_warnings(first, out)
    return EXIT_OK


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="loglab-adif",
        description="LogLab ADIF tools: ghost QSOs, validation, and lossless round trips.",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    commands = parser.add_subparsers(dest="command", required=True)

    ghosts = commands.add_parser("ghosts", help="write synthetic ghost QSOs as ADI")
    ghosts.add_argument("-o", "--output", type=Path, required=True, help="ADI file to write")
    ghosts.add_argument("-n", "--count", type=int, default=1000, help="number of QSOs")
    ghosts.add_argument("--seed", type=int, default=1, help="same seed, same ghosts")
    ghosts.add_argument(
        "--start",
        type=_utc,
        default=DEFAULT_START,
        help="UTC time of the first QSO, e.g. 2026-06-27T18:00Z",
    )

    check = commands.add_parser("check", help="validate an ADI file and summarize it")
    check.add_argument("file", type=Path)

    roundtrip = commands.add_parser(
        "roundtrip",
        help="read an ADI file, write it back out, and prove nothing changed",
    )
    roundtrip.add_argument("file", type=Path)
    roundtrip.add_argument("-o", "--output", type=Path, required=True, help="ADI file to write")
    return parser


def main(
    argv: Sequence[str] | None = None,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Run the command line and return its exit code."""
    args = _parser().parse_args(argv)
    if args.command == "ghosts":
        if args.count < 0:
            err.write("--count must not be negative\n")
            return EXIT_REJECTED
        return _ghosts(args, out)
    if args.command == "check":
        return _check(args, out, err)
    return _roundtrip(args, out, err)


def run() -> None:
    """Console-script entry point."""
    sys.exit(main())
