# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Environment settings: LabLog's names, and LogLab's from before the rename."""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import TYPE_CHECKING

from loglab_engine.environment import setting
from loglab_engine.store import default_database

if TYPE_CHECKING:
    import pytest

PYPROJECT = Path(__file__).resolve().parent.parent / "pyproject.toml"


def test_the_new_name_is_read(monkeypatch: pytest.MonkeyPatch) -> None:
    """$LABLOG_DATABASE is read."""
    monkeypatch.setenv("LABLOG_DATABASE", "dbname=new")
    monkeypatch.delenv("LOGLAB_DATABASE", raising=False)
    assert setting("DATABASE") == "dbname=new"


def test_the_old_name_still_works(monkeypatch: pytest.MonkeyPatch) -> None:
    """A station set up before the rename keeps its $LOGLAB_DATABASE."""
    monkeypatch.delenv("LABLOG_DATABASE", raising=False)
    monkeypatch.setenv("LOGLAB_DATABASE", "dbname=old")
    assert setting("DATABASE") == "dbname=old"
    assert default_database() == "dbname=old"


def test_the_new_name_wins_over_the_old(monkeypatch: pytest.MonkeyPatch) -> None:
    """With both set, the LabLog name is the one used."""
    monkeypatch.setenv("LABLOG_MQTT", "kiosk")
    monkeypatch.setenv("LOGLAB_MQTT", "old-kiosk")
    assert setting("MQTT") == "kiosk"


def test_empty_counts_as_not_set(monkeypatch: pytest.MonkeyPatch) -> None:
    """An empty value is skipped, as an unset one is."""
    monkeypatch.setenv("LABLOG_MQTT", "")
    monkeypatch.setenv("LOGLAB_MQTT", "old-kiosk")
    assert setting("MQTT") == "old-kiosk"
    monkeypatch.setenv("LOGLAB_MQTT", "")
    assert setting("MQTT") is None


def test_neither_set_gives_the_default_database(monkeypatch: pytest.MonkeyPatch) -> None:
    """With no setting at all, the database is still the one named loglab."""
    monkeypatch.delenv("LABLOG_DATABASE", raising=False)
    monkeypatch.delenv("LOGLAB_DATABASE", raising=False)
    assert default_database() == "dbname=loglab"


def test_every_command_answers_to_both_names() -> None:
    """Each command runs under its LabLog name and its old LogLab name alike."""
    scripts = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["project"]["scripts"]
    for command in ("engine", "log", "operator", "adif"):
        assert scripts[f"lablog-{command}"] == scripts[f"loglab-{command}"]
