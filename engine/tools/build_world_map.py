# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Build the world map the web client draws QSO paths on.

This is a developer tool, not part of the Engine: its output,
``src/loglab_engine/web/maps/world.json``, is committed, so LogLab draws the
map with no internet connection (Principle V) and nothing here runs in the
field. Run it only to rebuild the map, in a scratch environment with Shapely
installed (``pip install shapely``)::

    python tools/build_world_map.py COUNTRIES.geojson

Source (THIRD_PARTY_LICENSES.md): Natural Earth 1:110m admin-0 countries,
public domain, from github.com/nvkelso/natural-earth-vector at commit
``ca96624a56bd078437bca8184e78163e5039ad19`` (``geojson/`` folder). Its
SHA-256 is checked below.

Shapes are kept in degrees, x the longitude and y the latitude negated (so it
grows downward, as on screen). The page draws them in an equirectangular
projection centred on the station's longitude, so paths seldom cross the
map's edge. Antarctica is left out.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Final

from shapely import coverage_simplify, set_precision
from shapely.geometry import MultiPolygon, Polygon, shape
from shapely.geometry.base import BaseGeometry

HERE: Final = Path(__file__).resolve().parent
OUT: Final = HERE.parent / "src" / "loglab_engine" / "web" / "maps" / "world.json"
SHA256: Final = "6866c877d39cba9c357620878839b336d569f8c662d3cfab4cb1dbe2d39c977f"
TOLERANCE: Final = 0.15  # degrees
GRID: Final = 0.1  # coordinates are rounded to this many degrees
MIN_AREA: Final = 0.3  # islands smaller than this (square degrees) are left out
SOUTH: Final = -60.0  # nothing south of this is drawn


def _rings(geometry: BaseGeometry) -> list[Polygon]:
    if isinstance(geometry, Polygon):
        return [geometry]
    if isinstance(geometry, MultiPolygon):
        return list(geometry.geoms)
    return []


def _path(geometry: BaseGeometry) -> str:
    """An SVG path in degrees: x the longitude, y the latitude negated."""
    parts: list[str] = []
    for polygon in _rings(geometry):
        if polygon.area < MIN_AREA:
            continue
        for ring in (polygon.exterior, *polygon.interiors):
            points = [f"{x:g} {-y:g}" for x, y in list(ring.coords)[:-1]]
            if len(points) >= 3:  # noqa: PLR2004 - a ring needs three corners
                parts.append("M" + "L".join(points) + "Z")
    return "".join(parts)


def build(countries: Path) -> dict[str, object]:
    """The map file's contents, from the Natural Earth countries file."""
    data = countries.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256:
        sys.exit(f"{countries}: SHA-256 {digest} is not the pinned Natural Earth file")
    features: list[dict[str, dict[str, object]]] = json.loads(data)["features"]
    kept = [f for f in features if f["properties"]["ADM0_A3"] != "ATA"]
    geometries = [shape(f["geometry"]) for f in kept]
    simple = coverage_simplify(geometries, TOLERANCE)
    shapes = []
    for feature, geometry in zip(kept, simple, strict=True):
        rounded = set_precision(geometry, GRID)
        d = _path(rounded)
        if d:
            shapes.append(
                {
                    "id": str(feature["properties"]["ADM0_A3"]),
                    "name": str(feature["properties"]["NAME"]),
                    "d": d,
                }
            )
    shapes.sort(key=lambda s: s["id"])
    return {
        "source": "Natural Earth 1:110m admin-0 countries (public domain)",
        "south": SOUTH,
        "shapes": shapes,
    }


def main() -> None:
    """Write the map file."""
    if len(sys.argv) != 2:  # noqa: PLR2004 - the program name and one file
        sys.exit(__doc__)
    result = build(Path(sys.argv[1]))
    OUT.write_text(json.dumps(result, separators=(",", ":")) + "\n", encoding="utf-8")
    sys.stderr.write(f"{OUT}: {OUT.stat().st_size} bytes\n")


if __name__ == "__main__":
    main()
