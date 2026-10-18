# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The ``loglab-log`` command line: the demonstration Brian runs, end to end."""

from __future__ import annotations

import io
import re
from typing import TYPE_CHECKING

from loglab_engine.adif import parse_adi
from loglab_engine.adif.adi import write_adi
from loglab_engine.adif.ghosts import generate
from loglab_engine.cli.log import EXIT_DIFFERENT, EXIT_OK, EXIT_REFUSED, main

from .conftest import conninfo

if TYPE_CHECKING:
    from pathlib import Path

ME = ("--operator", "k9ksu", "--station", "shack")


class _Cli:
    def __init__(self, database: str) -> None:
        self.database = conninfo(database)

    def __call__(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        code = main(["--database", self.database, *argv], out, err)
        return code, out.getvalue(), err.getvalue()


def _ids(listing: str) -> list[str]:
    return re.findall(r"^([0-9a-f-]{36})  v", listing, flags=re.MULTILINE)


def test_migrate_command(empty_database: str) -> None:
    """Nothing runs on an unmigrated database; migrate applies once and then has nothing to do."""
    cli = _Cli(empty_database)
    code, _, err = cli("find")
    assert code == EXIT_REFUSED
    assert "run 'loglab-log migrate' first" in err
    assert cli("migrate", "--check")[0] == EXIT_DIFFERENT
    code, out, _ = cli("migrate")
    assert code == EXIT_OK
    assert "applied  0002_log" in out
    assert cli("migrate")[1].endswith("(0 applied now)\n")
    assert cli("migrate", "--check")[0] == EXIT_OK


def test_the_demo(database: str, tmp_path: Path) -> None:
    """Import ghosts, edit, delete, read history, restore, and round-trip."""
    cli = _Cli(database)
    ghosts = tmp_path / "ghosts.adi"
    ghosts.write_bytes(write_adi(generate(200)))

    code, out, _ = cli("import", str(ghosts), *ME)
    assert code == EXIT_OK
    assert "Imported 200 QSOs" in out
    assert "as import 1, by K9KSU at shack" in out
    assert "reproduces it exactly" in out
    assert "not yet logged" in out
    assert " logged." not in out  # Principle XV is not claimed.
    code, _, err = cli("import", str(ghosts), *ME)
    assert code == EXIT_REFUSED
    assert "already imported, as import 1" in err

    code, out, _ = cli("find", "-n", "3")
    first, second, _third = _ids(out)
    assert "3 QSO(s) shown" in out

    original = cli("show", first[:8])[1]
    rst = re.search(r"<RST_RCVD> '([^']*)'", original)
    assert rst is not None
    code, out, _ = cli("edit", first[:8], "--set", "RST_RCVD=339", *ME, "--note", "typo")
    assert code == EXIT_OK
    assert "now version 2" in out
    assert f"- <RST_RCVD> '{rst[1]}'" in out
    code, out, _ = cli("delete", second, "--operator", "W0XYZ", "--station", "fd-2")
    assert code == EXIT_OK
    assert "restore' brings it back" in out

    code, out, _ = cli("history", first[:8])
    assert "2 version(s)" in out
    assert "edit     by K9KSU at shack  note: typo" in out
    assert f"- <RST_RCVD> '{rst[1]}'" in out
    assert "+ <RST_RCVD> '339'" in out
    assert "create   by K9KSU at shack  (import 1: ghosts.adi)" in out
    code, out, _ = cli("history", second)
    assert "delete   by W0XYZ at fd-2" in out

    copy = tmp_path / "copy.adi"
    code, out, _ = cli("export", "--import", "1", "-o", str(copy))
    assert code == EXIT_DIFFERENT
    assert "(199 QSOs)" in out

    assert cli("restore", second, *ME)[0] == EXIT_OK
    assert cli("edit", first[:8], "--set", f"RST_RCVD={rst[1]}", *ME)[0] == EXIT_OK
    code, out, _ = cli("export", "--import", "1", "-o", str(copy))
    assert code == EXIT_OK
    assert "Identical to the imported file ghosts.adi" in out
    assert copy.read_bytes() == ghosts.read_bytes()


def test_whole_log_export_has_a_loglab_header(database: str, tmp_path: Path) -> None:
    """Exporting the whole log writes LogLab's own header over the stored QSOs."""
    cli = _Cli(database)
    ghosts = tmp_path / "ghosts.adi"
    ghosts.write_bytes(write_adi(generate(20)))
    assert cli("import", str(ghosts), *ME)[0] == EXIT_OK
    whole = tmp_path / "whole.adi"
    code, out, _ = cli("export", "-o", str(whole))
    assert code == EXIT_OK
    assert "Wrote the whole log (20 QSOs" in out
    exported = parse_adi(whole.read_bytes()).document
    assert exported.header is not None
    assert exported.header.get("PROGRAMID") == "LogLab"
    assert exported.header.preamble.startswith("LogLab export of the whole log")
    assert exported.records == parse_adi(ghosts.read_bytes()).document.records


def test_refusals_are_explained(database: str, tmp_path: Path) -> None:
    """Bad input and impossible changes exit 2 with the reason; nothing is written."""
    cli = _Cli(database)
    bad = tmp_path / "bad.adi"
    bad.write_bytes(b"<CALL:3>Q1A")
    code, _, err = cli("import", str(bad), *ME)
    assert code == EXIT_REFUSED
    assert "not terminated by <EOR>" in err
    code, _, err = cli("import", str(tmp_path / "missing.adi"), *ME)
    assert code == EXIT_REFUSED

    good = tmp_path / "one.adi"
    good.write_bytes(b"<CALL:3>Q1A <QSO_DATE:8>20260627 <EOR>")
    code, out, _ = cli("import", str(good), *ME)
    assert "not canonical ADI" in out
    qso = _ids(cli("find")[1])[0]
    for argv, reason in [
        (("edit", qso, *ME), "changes nothing"),
        (("edit", qso, "--set", "NOEQUALS", *ME), "not NAME=VALUE"),
        (("edit", qso, "--set", "BAD<NAME=1", *ME), "forbidden character"),
        (("edit", qso, "--remove", "FREQ", *ME), "no such field"),
        (("edit", qso, "--remove", "CALL", "--remove", "QSO_DATE", *ME), "at least one field"),
        (("restore", qso, *ME), "is not deleted"),
        (("show", "00000000"), "no QSO ID starts"),
        (("edit", qso, "--set", "A=1", "--operator", "K9 KSU", "--station", "shack"), "callsign"),
        (("export", "--import", "7", "-o", str(tmp_path / "x.adi")), "no import 7"),
    ]:
        code, _, err = cli(*argv)
        assert code == EXIT_REFUSED, argv
        assert reason in err, (argv, err)
    assert not (tmp_path / "x.adi").exists()
    assert "1 version(s)" in cli("history", qso)[1]


def test_unreachable_database_is_reported(tmp_path: Path) -> None:
    """A database that isn't there is an error message, not a traceback."""
    out, err = io.StringIO(), io.StringIO()
    code = main(["--database", f"host={tmp_path} dbname=nowhere", "find"], out, err)
    assert code == EXIT_REFUSED
    assert err.getvalue().startswith("database error:")
