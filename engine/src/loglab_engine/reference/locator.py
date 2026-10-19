# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Where a QSO was made from and to, from the fields the log already has.

A place comes from the ADIF ``LAT`` and ``LON`` fields when both are there,
or else from the Maidenhead grid square (``GRIDSQUARE``); the station's own
end uses ``MY_LAT``, ``MY_LON`` and ``MY_GRIDSQUARE``. A grid square stands
for its centre. Nothing is looked up anywhere (Principle V).
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from loglab_engine.adif import Record

EARTH_KM: Final = 6371.0088  # mean radius
_GRID = re.compile(r"^[A-R]{2}(?:[0-9]{2}(?:[A-X]{2}(?:[0-9]{2})?)?)?$")
# ADIF Location: a hemisphere letter, three-digit degrees, and minutes, like "N041 30.123".
_LOCATION = re.compile(r"^([NSEW])([0-9]{3}) ([0-9]{2}(?:\.[0-9]+)?)$")


@dataclass(frozen=True, slots=True)
class Place:
    """A point on the Earth, in degrees: latitude north, longitude east."""

    lat: float
    lon: float


def grid_place(text: str | None) -> Place | None:
    """The centre of a Maidenhead grid square of 2, 4, 6 or 8 characters, or ``None``."""
    grid = (text or "").strip().upper()
    if not _GRID.match(grid):
        return None
    lon, lat = -180.0, -90.0
    width, height = 20.0, 10.0
    lon += (ord(grid[0]) - ord("A")) * width
    lat += (ord(grid[1]) - ord("A")) * height
    steps = ((10.0, "0"), (24.0, "A"), (10.0, "0"))
    for at, (parts, first) in zip(range(2, len(grid), 2), steps, strict=False):
        width, height = width / parts, height / parts
        lon += (ord(grid[at]) - ord(first)) * width
        lat += (ord(grid[at + 1]) - ord(first)) * height
    return Place(lat=lat + height / 2, lon=lon + width / 2)


def adif_degrees(text: str | None, *, latitude: bool) -> float | None:
    """An ADIF Location (``N041 30.123``) in degrees, or ``None`` if it is not one."""
    found = _LOCATION.match((text or "").strip().upper())
    if not found:
        return None
    hemisphere, degrees, minutes = found.groups()
    if (hemisphere in "NS") != latitude:
        return None
    value = int(degrees) + float(minutes) / 60
    if float(minutes) >= 60 or value > (90 if latitude else 180):  # noqa: PLR2004 - minutes, degrees
        return None
    return -value if hemisphere in "SW" else value


def record_place(record: Record, *, mine: bool = False) -> Place | None:
    """Where one end of a QSO was: the other station's, or with ``mine`` the station's own."""
    prefix = "MY_" if mine else ""
    lat = adif_degrees(record.get(f"{prefix}LAT"), latitude=True)
    lon = adif_degrees(record.get(f"{prefix}LON"), latitude=False)
    if lat is not None and lon is not None:
        return Place(lat=lat, lon=lon)
    return grid_place(record.get(f"{prefix}GRIDSQUARE"))


def distance_km(a: Place, b: Place) -> float:
    """The great-circle distance between two places, in kilometres."""
    p1, p2 = math.radians(a.lat), math.radians(b.lat)
    dp, dl = p2 - p1, math.radians(b.lon - a.lon)
    h = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * EARTH_KM * math.asin(min(1.0, math.sqrt(h)))
