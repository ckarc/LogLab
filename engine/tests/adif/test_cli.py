# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The ``loglab-adif`` command line: the demonstration Brian runs."""

from __future__ import annotations

import io
import os
from typing import TYPE_CHECKING

import pytest

from loglab_engine.cli.adif import EXIT_OK, EXIT_REJECTED, main, write_atomically

if TYPE_CHECKING:
    from pathlib import Path


def run(*argv: str) -> tuple[int, str, str]:
    """Run the CLI and capture its exit code, stdout, and stderr."""
    out, err = io.StringIO(), io.StringIO()
    code = main(list(argv), out, err)
    return code, out.getvalue(), err.getvalue()


def test_ghosts_then_roundtrip_is_identical_byte_for_byte(tmp_path: Path) -> None:
    """The demo: generate ghosts, round-trip them, and compare files."""
    ghosts, copy = tmp_path / "ghosts.adi", tmp_path / "copy.adi"
    code, out, _ = run("ghosts", "-n", "250", "-o", str(ghosts))
    assert code == EXIT_OK
    assert "Wrote 250 ghost QSOs" in out
    code, out, _ = run("roundtrip", str(ghosts), "-o", str(copy))
    assert code == EXIT_OK
    assert "Round trip identical: 250 records" in out
    assert copy.read_bytes() == ghosts.read_bytes()


def test_check_summarizes_and_reports_warnings(tmp_path: Path) -> None:
    """Check reports counts and any import-only findings."""
    path = tmp_path / "old.adi"
    path.write_bytes(b"<CALL:003>Q1A<EOR>")
    code, out, _ = run("check", str(path))
    assert code == EXIT_OK
    assert "valid ADI with no header, 1 records, 1 fields, 1 warnings" in out
    assert "leading zeros" in out


def test_malformed_file_is_rejected_and_nothing_is_written(tmp_path: Path) -> None:
    """A bad file exits 2 with the reason, and no output file appears."""
    bad, target = tmp_path / "bad.adi", tmp_path / "out.adi"
    bad.write_bytes(b"<CALL:3>Q1A")
    code, _, err = run("roundtrip", str(bad), "-o", str(target))
    assert code == EXIT_REJECTED
    assert "not terminated by <EOR>" in err
    assert not target.exists()


def test_missing_file_is_reported(tmp_path: Path) -> None:
    """A path that doesn't exist is an error, not a crash."""
    code, _, err = run("check", str(tmp_path / "nope.adi"))
    assert code == EXIT_REJECTED
    assert "nope.adi" in err


def test_start_must_be_utc(tmp_path: Path) -> None:
    """A local time for --start is refused (Principle VIII)."""
    with pytest.raises(SystemExit):
        run("ghosts", "-o", str(tmp_path / "g.adi"), "--start", "2026-06-27T13:00-05:00")
    code, _, _ = run("ghosts", "-o", str(tmp_path / "g.adi"), "--start", "2026-06-27T18:00Z")
    assert code == EXIT_OK


def test_negative_count_is_refused(tmp_path: Path) -> None:
    """A negative ghost count is an error."""
    code, _, err = run("ghosts", "-n", "-5", "-o", str(tmp_path / "g.adi"))
    assert code == EXIT_REJECTED
    assert "negative" in err


def test_atomic_write_replaces_and_leaves_no_temp_files(tmp_path: Path) -> None:
    """Writes land whole, replace the old file, and clean up (Principle XIV)."""
    target = tmp_path / "log.adi"
    target.write_bytes(b"old")
    write_atomically(target, b"new")
    assert target.read_bytes() == b"new"
    assert [p.name for p in tmp_path.iterdir()] == ["log.adi"]


@pytest.mark.skipif(os.name != "posix", reason="POSIX permission bits")
def test_atomic_write_uses_normal_permissions(tmp_path: Path) -> None:
    """New files get umask-default permissions; replaced files keep theirs."""
    umask = os.umask(0o022)
    try:
        fresh = tmp_path / "fresh.adi"
        write_atomically(fresh, b"x")
        assert fresh.stat().st_mode & 0o777 == 0o644
        kept = tmp_path / "kept.adi"
        kept.write_bytes(b"old")
        kept.chmod(0o600)
        write_atomically(kept, b"new")
        assert kept.stat().st_mode & 0o777 == 0o600
    finally:
        os.umask(umask)


def test_atomic_write_failure_keeps_the_old_file(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """If the write fails, the previous file is untouched and no temp remains."""
    target = tmp_path / "log.adi"
    target.write_bytes(b"old")

    def disk_failure(_fd: int) -> None:
        raise OSError(5, "simulated disk failure")

    monkeypatch.setattr(os, "fsync", disk_failure)
    with pytest.raises(OSError, match="simulated disk failure"):
        write_atomically(target, b"new")
    assert target.read_bytes() == b"old"
    assert [p.name for p in tmp_path.iterdir()] == ["log.adi"]
