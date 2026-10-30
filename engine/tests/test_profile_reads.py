# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Requests for a station's Agent to read its radio, kept in memory for a while."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from loglab_engine.profile_reads import LIFETIME, ReadDesk, ReadError

NOW = datetime(2026, 10, 30, 0, 30, tzinfo=UTC)


def test_a_new_request_for_a_station_replaces_the_one_still_waiting() -> None:
    """Asking again doesn't leave the Agent two readings to make."""
    desk = ReadDesk()
    first = desk.ask("fl-1", "VARA", "K9KSU", NOW)
    second = desk.ask("fl-1", "FT8", "K9KSU", NOW)
    other = desk.ask("fl-2", "VARA", "K9KSU", NOW)
    assert desk.waiting_for("fl-1", NOW) == (second,)
    assert desk.waiting_for("fl-2", NOW) == (other,)
    with pytest.raises(ReadError):
        desk.get(first.id, NOW)


def test_a_request_is_forgotten_after_its_lifetime() -> None:
    """A restart or a while clears requests; the administrator asks again."""
    desk = ReadDesk()
    asked = desk.ask("fl-1", "VARA", "K9KSU", NOW)
    later = NOW + LIFETIME * 2
    assert desk.waiting_for("fl-1", later) == ()
    with pytest.raises(ReadError):
        desk.answer("fl-1", asked.id, later, problem="late")
    with pytest.raises(ReadError):
        desk.get(asked.id, later)
