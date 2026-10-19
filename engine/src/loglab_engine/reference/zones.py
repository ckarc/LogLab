# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""CQ zones and ITU zones: the places the CQ WW and IARU HF contests count.

There are 40 CQ zones and 90 ITU zones, numbered from 1. A QSO's zone is its
ADIF ``CQZ`` or ``ITUZ`` field. Their boundaries ship with the web client
(``web/maps/cq-zones.json`` and ``itu-zones.json``), so the zone maps work
with no internet connection (Principle V).
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Final, Literal

ZoneKind = Literal["cq", "itu"]

ZONES: Final[Mapping[ZoneKind, int]] = {"cq": 40, "itu": 90}
FIELDS: Final[Mapping[ZoneKind, str]] = {"cq": "CQZ", "itu": "ITUZ"}


@dataclass(frozen=True)
class ZoneTally:
    """How many QSOs are in each zone, and how many name no zone."""

    worked: dict[int, int]
    unrecognized: dict[str, int]
    without_zone: int


def zone_for(kind: ZoneKind, value: str) -> int | None:
    """The zone a ``CQZ`` or ``ITUZ`` value names (``05`` is zone 5), or ``None``."""
    text = value.strip()
    if not text.isascii() or not text.isdigit():
        return None
    zone = int(text)
    return zone if 1 <= zone <= ZONES[kind] else None


def tally_zones(kind: ZoneKind, values: Mapping[str, int]) -> ZoneTally:
    """Tally zones from field values and how many QSOs carry each.

    The empty string stands for QSOs with no zone.
    """
    worked: dict[int, int] = {}
    unrecognized: dict[str, int] = {}
    without = 0
    for value, count in values.items():
        if not value.strip():
            without += count
            continue
        zone = zone_for(kind, value)
        if zone is None:
            key = value.strip().upper()
            unrecognized[key] = unrecognized.get(key, 0) + count
        else:
            worked[zone] = worked.get(zone, 0) + count
    return ZoneTally(worked, unrecognized, without)
