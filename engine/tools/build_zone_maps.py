# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Build the CQ zone and ITU zone maps the web client draws.

This is a developer tool, not part of the Engine: its output,
``src/loglab_engine/web/maps/cq-zones.json`` and ``itu-zones.json``, is
committed, so LabLog draws the maps with no internet connection
(Principle V) and nothing here runs in the field. Run it only to rebuild the
maps, in a scratch environment with Shapely installed
(``pip install shapely``)::

    python tools/build_zone_maps.py CQZONES.geojson ITUZONES.geojson

Source (THIRD_PARTY_LICENSES.md): the CQ and ITU zone boundaries of
github.com/HB9HIL/hamradio-zones-geojson (MIT licence, copyright 2024 HB9HIL)
at commit ``091b813f35c899af194d028d3841fc10b16fbd45``. The SHA-256 of each
file is checked below.

Shapes are kept in degrees, x the longitude and y the latitude negated, as
the world map's are. Zones that cross the 180th meridian run past it (a
longitude of 190 is 170 W), so the page can draw them unbroken.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Final

from shapely import make_valid, set_precision
from shapely.geometry import MultiPolygon, Polygon, shape
from shapely.geometry.base import BaseGeometry

HERE: Final = Path(__file__).resolve().parent
MAPS: Final = HERE.parent / "src" / "loglab_engine" / "web" / "maps"
SOURCE: Final = (
    "CQ and ITU zones, Copyright (c) 2024 HB9HIL, from HB9HIL/hamradio-zones-geojson"
    " (MIT licence: engine/tools/hb9hil-zones/LICENSE)"
)
SHA256: Final = {
    "cq": "1764e65acbbb82a6b3b6e8ba31c69fbc271965772e73ad7ad02b14bc2bae5ddf",
    "itu": "79c6e5c723bfa0c27e527e79a9e12a52bd19669faec31b9fb27bcd71da7b61b7",
}
TOLERANCE: Final = 0.2  # degrees
GRID: Final = 0.1  # coordinates are rounded to this many degrees


def _polygons(geometry: BaseGeometry) -> list[Polygon]:
    if isinstance(geometry, Polygon):
        return [geometry]
    if isinstance(geometry, MultiPolygon):
        return list(geometry.geoms)
    return [p for part in getattr(geometry, "geoms", ()) for p in _polygons(part)]


def _path(geometry: BaseGeometry) -> str:
    """An SVG path in degrees: x the longitude, y the latitude negated."""
    parts: list[str] = []
    for polygon in _polygons(geometry):
        for ring in (polygon.exterior, *polygon.interiors):
            points = [f"{x:g} {-y:g}" for x, y in list(ring.coords)[:-1]]
            if len(points) >= 3:  # noqa: PLR2004 - a ring needs three corners
                parts.append("M" + "L".join(points) + "Z")
    return "".join(parts)


def _places(text: str) -> str:
    """The places an ITU zone's name lists, without the source's markup.

    The source writes ``!:KL:**Alaska, west of 141º W#VE:**Canada#VE7:British
    Columbia``: ``!`` marks nesting, ``#`` separates entries, and ``**`` marks
    a country or area (the entries without it are its parts).
    """
    names = []
    for entry in text.split("#"):
        found = re.search(r"\*\*(.+)$", entry)
        if found:
            name = found.group(1).replace("+", " ").strip()
            if name not in names:
                names.append(name)
    return "; ".join(names)


def build(kind: str, zones: Path) -> dict[str, object]:
    """One map file's contents, from that kind's zones file."""
    data = zones.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256[kind]:
        sys.exit(f"{zones}: SHA-256 {digest} is not the pinned {kind} zones file")
    shapes = []
    for feature in json.loads(data)["features"]:
        properties = feature["properties"]
        zone = int(properties[f"{kind}_zone_number"])
        lat, lon = properties[f"{kind}_zone_name_loc"]
        raw = str(properties[f"{kind}_zone_name"])
        geometry = make_valid(shape(feature["geometry"]))
        simple = set_precision(geometry.simplify(TOLERANCE), GRID)
        shapes.append(
            {
                "zone": zone,
                "name": raw if kind == "cq" else _places(raw),
                "label": [float(lon), -float(lat)],
                "d": _path(simple),
            }
        )
    shapes.sort(key=lambda s: int(str(s["zone"])))
    return {"source": SOURCE, "kind": kind, "shapes": shapes}


def main() -> None:
    """Write both map files."""
    if len(sys.argv) != 3:  # noqa: PLR2004 - the program name and two files
        sys.exit(__doc__)
    for kind, name in (("cq", sys.argv[1]), ("itu", sys.argv[2])):
        out = MAPS / f"{kind}-zones.json"
        result = build(kind, Path(name))
        out.write_text(
            json.dumps(result, separators=(",", ":"), ensure_ascii=False) + "\n", encoding="utf-8"
        )
        sys.stderr.write(f"{out}: {out.stat().st_size} bytes\n")


if __name__ == "__main__":
    main()
