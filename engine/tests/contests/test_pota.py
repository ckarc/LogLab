# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The POTA activator overlay: dupes by UTC day and park, activations, the fields it writes."""

from __future__ import annotations

import json
import uuid
from datetime import UTC, datetime, timedelta

import pytest

from loglab_engine.adif import Field, Record
from loglab_engine.contests import Earlier, OverlayError, check, overlay, parse
from loglab_engine.contests.dupes import sent_problems
from loglab_engine.contests.overlay import Overlay
from loglab_engine.contests.stamp import Station, stamp
from loglab_engine.contests.stats import Logged, operation_stats

DAY = datetime(2026, 10, 24, 15, 0, tzinfo=UTC)


def _pota() -> Overlay:
    found = overlay("pota-activator")
    assert found is not None
    return found


def _qso(call: str = "W1AW", band: str = "20m", mode: str = "SSB", **more: str) -> dict[str, str]:
    fields = {
        "CALL": call,
        "BAND": band,
        "MODE": mode,
        "STATION_CALLSIGN": "W0CK",
        "MY_SIG": "POTA",
        "MY_SIG_INFO": "US-1562",
    }
    fields.update(more)
    return fields


def _earlier(**more: str) -> Earlier:
    return Earlier(uuid.uuid4(), _qso(**more), DAY, "K9KSU")


def test_the_overlay_is_a_program_not_a_contest() -> None:
    """It writes no CONTEST_ID, checks dupes by UTC day and park, and wants ten QSOs."""
    pota = _pota()
    assert pota.contest_id is None
    assert not pota.is_contest
    assert pota.checks_dupes
    assert pota.program is not None
    assert (pota.program.sig, pota.program.role, pota.program.goal) == ("POTA", "activator", 10)
    assert pota.dupes is not None
    assert pota.dupes.within == "day"
    assert pota.shown_stats[0] == "activation"


def test_a_station_counts_once_per_band_and_mode() -> None:
    """The same band and mode from the same park is a dupe; another band is not."""
    pota = _pota()
    assert check(pota, _qso(), [_earlier()]).status == "dupe"
    assert check(pota, _qso(band="40m"), [_earlier()]).status == "new"
    assert check(pota, _qso(mode="CW"), [_earlier()]).status == "new"


def test_park_to_park_from_another_park_counts_again() -> None:
    """The other station moved to a new park: a new contact."""
    pota = _pota()
    first = _earlier(SIG_INFO="US-0001")
    assert check(pota, _qso(SIG_INFO="US-0002"), [first]).status == "new"
    assert check(pota, _qso(SIG_INFO="US-0001"), [first]).status == "dupe"
    # The earlier contact still shows as worked.
    assert len(check(pota, _qso(SIG_INFO="US-0002"), [first]).worked) == 1


def test_from_another_park_of_mine_it_counts_again() -> None:
    """A rove: the same station worked again from the next park."""
    pota = _pota()
    assert check(pota, _qso(MY_SIG_INFO="US-0003"), [_earlier()]).status == "new"


def test_their_park_may_be_left_out_but_must_look_like_a_park() -> None:
    """Only a park-to-park contact has one."""
    pota = _pota()
    assert check(pota, _qso(), []).problems == ()
    bad = check(pota, _qso(SIG_INFO="K-12"), []).problems
    assert [p.field for p in bad] == ["SIG_INFO"]


def test_the_operation_must_give_its_park_and_may_give_its_state() -> None:
    """The park is required; the state only for a park in several states."""
    pota = _pota()
    assert [p.field for p in sent_problems(pota, {})] == ["park"]
    assert sent_problems(pota, {"park": "US-1562"}) == ()
    assert [p.field for p in sent_problems(pota, {"park": "US-1562", "state": "W1"})] == ["state"]


def test_a_qso_is_stamped_with_the_park_and_park_to_park_with_theirs() -> None:
    """MY_SIG, MY_SIG_INFO and MY_POTA_REF always; SIG and POTA_REF only when they gave a park."""
    pota = _pota()
    station = Station("W0CK", "K9KSU", {"park": "US-1562", "state": ""}, "op-1")
    plain = stamp(Record((Field("CALL", "W1AW"),)), pota, station)
    names = {f.name: f.value for f in plain.fields}
    assert names["MY_SIG"] == "POTA"
    assert names["MY_SIG_INFO"] == "US-1562"
    assert names["MY_POTA_REF"] == "US-1562"
    assert "MY_STATE" not in names
    assert "SIG" not in names
    assert "CONTEST_ID" not in names
    p2p = stamp(Record((Field("CALL", "W1AW"), Field("SIG_INFO", "us-0001"))), pota, station)
    names = {f.name: f.value for f in p2p.fields}
    assert (names["SIG"], names["POTA_REF"]) == ("POTA", "US-0001")


def _logged(call: str, minute: int, **more: str) -> Logged:
    return Logged(_qso(call, **more), DAY + timedelta(minutes=minute), "K9KSU")


def test_ten_qsos_from_a_park_in_a_utc_day_activate_it() -> None:
    """Dupes don't count; every operator of the club call counts together."""
    qsos = [_logged(f"W{i}AA", i, OPERATOR="K9KSU" if i % 2 else "W0XYZ") for i in range(9)]
    qsos.append(_logged("W1AA", 20, OPERATOR="W0XYZ"))  # a dupe of W1AA
    stats = operation_stats(
        _pota(), qsos, starts_at=None, ends_at=None, now=DAY + timedelta(hours=1)
    )
    (today,) = stats.activations
    assert (today.station_call, today.park, today.day, today.qsos, today.done) == (
        "W0CK",
        "US-1562",
        DAY.date(),
        9,
        False,
    )
    assert stats.dupes == 1
    qsos.append(_logged("W9ZZ", 30, SIG_INFO="US-0001"))
    stats = operation_stats(
        _pota(), qsos, starts_at=None, ends_at=None, now=DAY + timedelta(hours=1)
    )
    (today,) = stats.activations
    assert (today.qsos, today.done, today.p2p) == (10, True, 1)
    assert stats.p2p == 1


def test_each_utc_day_and_each_call_is_its_own_activation() -> None:
    """After 0000 UTC the count starts over; an operator's own call counts apart."""
    qsos = [
        _logged("W1AA", 0),
        _logged("W1AA", 10 * 60),  # 0100 UTC the next day: not a dupe
        _logged("W2BB", 10 * 60, STATION_CALLSIGN="K9KSU"),
    ]
    stats = operation_stats(
        _pota(), qsos, starts_at=None, ends_at=None, now=DAY + timedelta(days=1)
    )
    assert stats.dupes == 0
    assert [(a.day.isoformat(), a.station_call, a.qsos) for a in stats.activations] == [
        ("2026-10-25", "W0CK", 1),
        ("2026-10-25", "K9KSU", 1),
        ("2026-10-24", "W0CK", 1),
    ]


def _raw() -> dict[str, object]:
    found = overlay("pota-activator")
    assert found is not None
    return json.loads(found.model_dump_json(exclude_none=True))  # type: ignore[no-any-return]


@pytest.mark.parametrize(
    ("change", "why"),
    [
        ({"contest_id": "POTA"}, "no contest_id"),
        ({"exchange": None}, "needs rules, bands, modes and an exchange"),
    ],
)
def test_a_program_overlay_that_is_not_whole_is_refused(
    change: dict[str, object], why: str
) -> None:
    """A program has no contest ID, and needs the parts a contest has."""
    raw = _raw()
    raw.update({"id": "pota-copy", **change})
    raw = {k: v for k, v in raw.items() if v is not None}
    with pytest.raises(OverlayError, match=why):
        parse(json.dumps(raw).encode(), "pota-copy.json")


def test_only_a_program_checks_dupes_by_day_or_shows_activations() -> None:
    """A contest's dupes stay yearly; activations need a program."""
    fd = overlay("arrl-fd")
    assert fd is not None
    raw = json.loads(fd.model_dump_json(exclude_none=True))
    raw["id"] = "fd-copy"
    raw["dupes"]["within"] = "day"
    with pytest.raises(OverlayError, match="only a program overlay"):
        parse(json.dumps(raw).encode(), "fd-copy.json")
    raw["dupes"]["within"] = "year"
    raw["stats"]["show"] = ["activation"]
    with pytest.raises(OverlayError, match="shown only for a program"):
        parse(json.dumps(raw).encode(), "fd-copy.json")
