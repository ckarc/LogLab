# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Build the ARRL and RAC section map the web client draws.

This is a developer tool, not part of the Engine: its output,
``src/loglab_engine/web/maps/arrl-sections.json``, is committed, so LogLab
draws the map with no internet connection (Principle V) and nothing here runs
in the field. Run it only to rebuild the map, in a scratch environment with
Shapely installed (``pip install shapely``)::

    python tools/build_section_map.py ADMIN_1.geojson COUNTIES.geojson LAKES.geojson

Sources (THIRD_PARTY_LICENSES.md):

* Natural Earth 1:10m admin-1 states and provinces, admin-2 counties and lakes,
  public domain, from github.com/nvkelso/natural-earth-vector at commit
  ``ca96624a56bd078437bca8184e78163e5039ad19`` (``geojson/`` folder). The
  SHA-256 of each file is checked below.
* Which county is in which section, for the states ARRL splits, from
  ``@ham2k/lib-arrl-data`` 1.2.0 (MIT), kept unchanged in ``tools/ham2k/``,
  with the corrections in ``CORRECTIONS`` below.

Each US section is the union of its counties (Puerto Rico, which the
county file leaves out, comes from admin-1), so split states (California,
Florida, New York, Pennsylvania, Texas, and the rest) are drawn section by
section. Canada is drawn by province. Natural Earth does not divide Ontario,
so Ontario is one shape that stands for its four sections (GH, ONE, ONN, ONS).
Hawaii (PAC) and Puerto Rico with the Virgin Islands are drawn in insets.
"""

from __future__ import annotations

import hashlib
import json
import math
import sys
from collections import defaultdict
from collections.abc import Callable
from pathlib import Path
from typing import Final

import numpy as np
from numpy.typing import NDArray
from shapely import coverage_simplify, set_precision, transform
from shapely.affinity import affine_transform
from shapely.geometry import MultiPolygon, Polygon, shape
from shapely.geometry.base import BaseGeometry
from shapely.ops import polylabel, unary_union

HERE: Final = Path(__file__).resolve().parent
OUT: Final = HERE.parent / "src" / "loglab_engine" / "web" / "maps" / "arrl-sections.json"
SHA256: Final = {
    "admin_1": "22d0e3ad85eb3e27f17cabf8ba2d50e554fbc27a87796ff891d958185da62fb5",
    "counties": "9a80308d80eef51a0c1ade49aeb9ca877957a346f880e177a85527ef6cfdd1bb",
    "lakes": "2d036f53dedec578001c5c30c2959ee7d4eebc1306900fa4367c49929ec8f2d9",
}
WIDTH: Final = 1000.0
TOLERANCE: Final = 0.6  # in map units: about half a pixel on a 1000-wide map
MIN_AREA: Final = 1.5  # islands smaller than this (square map units) are left out
CANADA: Final = {
    "AB": "AB",
    "BC": "BC",
    "MB": "MB",
    "NB": "NB",
    "NL": "NL",
    "NS": "NS",
    "PE": "PE",
    "QC": "QC",
    "SK": "SK",
    "NT": "TER",
    "NU": "TER",
    "YT": "TER",
    "ON": "ON",
}
ONTARIO: Final = ("GH", "ONE", "ONN", "ONS")
INSET_SHAPES: Final = ("PAC", "PR", "VI")
# Two Texas counties the ham2k table puts in West Texas, though each is
# surrounded by another section: Anderson (Palestine) is North Texas, and
# Bastrop, beside Austin, is South Texas.
CORRECTIONS: Final = {("TX", "Anderson"): "NTX", ("TX", "Bastrop"): "STX"}
SPELLINGS: Final = {
    "Desoto": "DeSoto",
    "St. Johns": "Saint Johns",
    "St. Lawrence": "Saint Lawrence",
    "St. Lucie": "Saint Lucie",
}

Projection = Callable[[NDArray[np.float64]], NDArray[np.float64]]


def _lambert(lon0: float, lat0: float, lat1: float, lat2: float) -> Projection:
    """A spherical Lambert conformal conic projection of (longitude, latitude) rows."""
    p1, p2, p0 = (math.radians(v) for v in (lat1, lat2, lat0))
    n = math.log(math.cos(p1) / math.cos(p2)) / math.log(
        math.tan(math.pi / 4 + p2 / 2) / math.tan(math.pi / 4 + p1 / 2),
    )
    f = math.cos(p1) * math.tan(math.pi / 4 + p1 / 2) ** n / n
    rho0 = f / math.tan(math.pi / 4 + p0 / 2) ** n

    def project(lonlat: NDArray[np.float64]) -> NDArray[np.float64]:
        rho = f / np.tan(np.pi / 4 + np.radians(lonlat[:, 1]) / 2) ** n
        theta = n * np.radians(lonlat[:, 0] - lon0)
        # x grows eastward; y grows downward, as on screen.
        return np.column_stack((rho * np.sin(theta), rho * np.cos(theta) - rho0))

    return project


def _load(path: Path, key: str) -> list[dict[str, object]]:
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256[key]:
        sys.exit(f"{path}: SHA-256 {digest} is not the pinned Natural Earth file")
    features: list[dict[str, object]] = json.loads(data)["features"]
    return features


def _sections_by_county() -> dict[str, str | dict[str, str]]:
    table: dict[str, str | dict[str, str]] = json.loads(
        (HERE / "ham2k" / "sections-for-states.json").read_text(encoding="utf-8"),
    )
    return table


def _gather(admin_1: Path, counties: Path, lakes: Path) -> dict[str, BaseGeometry]:
    table = _sections_by_county()
    parts: dict[str, list[BaseGeometry]] = defaultdict(list)
    for feature in _load(counties, "counties"):
        props: dict[str, str] = feature["properties"]  # type: ignore[assignment]
        state, county = props["REGION"], props["NAME"]
        entry = table[state]
        if isinstance(entry, str):
            parts[entry].append(shape(feature["geometry"]))
            continue
        names = {SPELLINGS.get(k, k): v for k, v in entry.items()}
        if county not in names:
            sys.exit(f"{state} county {county!r} is in no section")
        section = CORRECTIONS.get((state, county), names[county])
        parts[section].append(shape(feature["geometry"]))
    for feature in _load(admin_1, "admin_1"):
        props = feature["properties"]  # type: ignore[assignment]
        if props["adm0_a3"] == "CAN":
            parts[CANADA[props["postal"]]].append(shape(feature["geometry"]))
        elif props["adm0_a3"] == "PRI":
            parts["PR"].append(shape(feature["geometry"]))
    water = unary_union(
        [
            shape(f["geometry"])
            for f in _load(lakes, "lakes")
            if f["properties"]["scalerank"] == 0  # type: ignore[index]
        ],
    )
    return {code: unary_union(pieces).difference(water) for code, pieces in parts.items()}


def _polygons(geometry: BaseGeometry) -> list[Polygon]:
    if isinstance(geometry, Polygon):
        return [geometry]
    if isinstance(geometry, MultiPolygon):
        return list(geometry.geoms)
    return [g for part in getattr(geometry, "geoms", []) for g in _polygons(part)]


def _fit(shapes: dict[str, BaseGeometry], box: tuple[float, float, float, float]) -> None:
    """Scale and move ``shapes`` together so their bounds fill ``box`` (x, y, w, h)."""
    bounds = unary_union(list(shapes.values())).bounds
    scale = min(box[2] / (bounds[2] - bounds[0]), box[3] / (bounds[3] - bounds[1]))
    dx = box[0] + (box[2] - (bounds[2] - bounds[0]) * scale) / 2 - bounds[0] * scale
    dy = box[1] + (box[3] - (bounds[3] - bounds[1]) * scale) / 2 - bounds[1] * scale
    for key, geometry in shapes.items():
        shapes[key] = affine_transform(geometry, [scale, 0, 0, scale, dx, dy])


def _path(geometry: BaseGeometry) -> str:
    out: list[str] = []
    for polygon in _polygons(geometry):
        if polygon.area < MIN_AREA:
            continue
        for ring in (polygon.exterior, *polygon.interiors):
            coords = list(ring.coords)[:-1]
            if len(coords) < 3:  # noqa: PLR2004 - a ring needs three corners
                continue
            first, *rest = coords
            out.append(f"M{first[0]:.1f} {first[1]:.1f}")
            out.extend(f"L{x:.1f} {y:.1f}" for x, y in rest)
            out.append("Z")
    return "".join(out)


def build(admin_1: Path, counties: Path, lakes: Path) -> dict[str, object]:
    """The map: its size and one shape per section (Ontario: one for four)."""
    parts = _gather(admin_1, counties, lakes)
    main_projection = _lambert(-100, 50, 40, 62)
    pac_projection = _lambert(-157, 20.5, 19, 22)
    car_projection = _lambert(-65.8, 18.2, 17.5, 18.8)
    main: dict[str, BaseGeometry] = {}
    pac: dict[str, BaseGeometry] = {}
    car: dict[str, BaseGeometry] = {}
    hawaii = Polygon([(-161, 18), (-154, 18), (-154, 23), (-161, 23)])
    below_arctic = Polygon([(-180, 24), (-50, 24), (-50, 72), (-180, 72)])
    for code, merged in parts.items():
        if code == "PAC":
            # Hawaii's main islands; the far Northwestern Islands would shrink the inset.
            pac[code] = transform(merged.intersection(hawaii), pac_projection)
        elif code in INSET_SHAPES:
            car[code] = transform(merged, car_projection)
        else:
            # Trim the high Arctic so the provinces and states get the room.
            main[code] = transform(merged.intersection(below_arctic), main_projection)
    height = 760.0
    _fit(main, (0, 0, WIDTH, height))
    _fit(pac, (150, height - 140, 200, 120))
    _fit(car, (870, height - 110, 120, 90))
    shapes = {**main, **pac, **car}
    codes = sorted(shapes)
    simplified = coverage_simplify([shapes[c] for c in codes], TOLERANCE)
    out: list[dict[str, object]] = []
    for code, geometry in zip(codes, simplified, strict=True):
        clean = set_precision(geometry.buffer(0), 0.1)
        biggest = max(_polygons(clean), key=lambda p: p.area)
        label = polylabel(biggest, tolerance=0.5)
        out.append(
            {
                "id": code,
                "sections": list(ONTARIO) if code == "ON" else [code],
                "label": [round(label.x, 1), round(label.y, 1)],
                "d": _path(clean),
            },
        )
    return {
        "width": WIDTH,
        "height": height,
        "insets": [
            {"x": 140, "y": height - 150, "width": 220, "height": 140, "title": "Hawaii"},
            {"x": 860, "y": height - 120, "width": 140, "height": 110, "title": "PR and VI"},
        ],
        "source": (
            "Natural Earth (public domain); county sections from @ham2k/lib-arrl-data 1.2.0 (MIT)"
        ),
        "shapes": out,
    }


def main() -> None:
    """Build the map from the three Natural Earth files named on the command line."""
    if len(sys.argv) != 4:  # noqa: PLR2004 - the program name and three files
        sys.exit(__doc__)
    result = build(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(result, separators=(",", ":")) + "\n", encoding="utf-8")
    sys.stdout.write(f"wrote {OUT} ({OUT.stat().st_size // 1024} KB)\n")


if __name__ == "__main__":
    main()
