# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""CQ and ITU zones: reading a QSO's zone, tallying them, and the maps' shapes."""

from __future__ import annotations

import json
from importlib.resources import files

import pytest

from loglab_engine.reference import ZONES, ZoneKind, tally_zones, zone_for


@pytest.mark.parametrize(
    ("kind", "value", "zone"),
    [
        ("cq", "5", 5),
        ("cq", "05", 5),
        ("cq", " 40 ", 40),
        ("cq", "41", None),
        ("cq", "0", None),
        ("itu", "90", 90),
        ("itu", "91", None),
        ("itu", "DARC", None),
        ("itu", "-3", None),
        ("itu", "٣", None),  # an Arabic-Indic digit is not an ADIF number
    ],
)
def test_a_zone_from_its_field(kind: ZoneKind, value: str, zone: int | None) -> None:
    """Numbers from 1 to the last zone, with or without a leading zero."""
    assert zone_for(kind, value) == zone


def test_tallying_zones() -> None:
    """Values that name the same zone add up; the rest are reported, not dropped."""
    tally = tally_zones("cq", {"4": 2, "04": 1, "5": 1, "": 3, "dx": 1, "99": 2})
    assert tally.worked == {4: 3, 5: 1}
    assert tally.unrecognized == {"DX": 1, "99": 2}
    assert tally.without_zone == 3


@pytest.mark.parametrize("kind", ["cq", "itu"])
def test_every_zone_is_on_its_map(kind: ZoneKind) -> None:
    """The map that ships with the Engine has one shape for each zone, and a label inside it."""
    data = json.loads(
        (files("loglab_engine") / "web" / "maps" / f"{kind}-zones.json").read_text("utf-8")
    )
    assert data["kind"] == kind
    assert "MIT" in data["source"]
    assert [s["zone"] for s in data["shapes"]] == list(range(1, ZONES[kind] + 1))
    for s in data["shapes"]:
        assert s["d"].startswith("M"), s["zone"]
        assert s["name"], s["zone"]
        lon, y = s["label"]
        assert -200 <= lon <= 200, s["zone"]  # zones may run past 180
        assert -90 <= y <= 90, s["zone"]
