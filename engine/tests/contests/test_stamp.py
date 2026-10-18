# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""What an operation adds to each QSO: never replacing what the operator typed (VII)."""

from __future__ import annotations

from loglab_engine.adif import Field, Record
from loglab_engine.contests import EVERYDAY, Overlay, overlay
from loglab_engine.contests.stamp import Station, stamp


def _overlay(overlay_id: str) -> Overlay:
    found = overlay(overlay_id)
    assert found is not None
    return found


CLUB = Station(station_call="W0CK", operator="K9KSU", sent={"class": "3a", "section": "ks"})


def _values(record: Record) -> dict[str, str]:
    return {f.name: f.value for f in record.fields}


def test_a_field_day_qso_gets_the_club_call_operator_contest_and_exchange() -> None:
    """Made as W0CK, by K9KSU, in ARRL-FD, having sent 3A KS."""
    record = Record((Field("CALL", "W1AW"), Field("BAND", "20m")))
    values = _values(stamp(record, _overlay("arrl-fd"), CLUB))
    assert values == {
        "CALL": "W1AW",
        "BAND": "20m",
        "STATION_CALLSIGN": "W0CK",
        "OPERATOR": "K9KSU",
        "CONTEST_ID": "ARRL-FD",
        "MY_ARRL_SECT": "KS",
        "STX_STRING": "3A KS",
    }


def test_fields_the_qso_already_has_are_kept() -> None:
    """A logger typing for another operator names them; the stamp never overwrites it."""
    record = Record((Field("CALL", "W1AW"), Field("operator", "N0FD")))
    values = _values(stamp(record, _overlay("arrl-fd"), CLUB))
    assert values["operator"] == "N0FD"
    assert "OPERATOR" not in values


def test_everyday_adds_only_the_calls() -> None:
    """An everyday operation (a club station's daily log) has no contest or exchange."""
    record = Record((Field("CALL", "W1AW"),))
    values = _values(stamp(record, _overlay(EVERYDAY), CLUB))
    assert values == {"CALL": "W1AW", "STATION_CALLSIGN": "W0CK", "OPERATOR": "K9KSU"}
