# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Places from grid squares and ADIF locations, and the world map the page draws."""

from __future__ import annotations

import json
from importlib.resources import files

import pytest

from loglab_engine.adif import Field, Record
from loglab_engine.reference import Place, distance_km, grid_place, record_place
from loglab_engine.reference.locator import adif_degrees

MAP = files("loglab_engine") / "web" / "maps" / "world.json"


@pytest.mark.parametrize(
    ("grid", "lat", "lon"),
    [
        ("EM28", 38.5, -95.0),  # Lawrence, Kansas
        ("em28qx", 38.979167, -94.625),
        ("JO01", 51.5, 1.0),
        ("FN31pr", 41.729167, -72.708333),  # Newington, Connecticut
        ("FN31pr44", 41.727083, -72.7125),
        ("AA", -85.0, -170.0),
        ("RR99XX99", 89.997917, 179.995833),
    ],
)
def test_a_grid_square_is_its_centre(grid: str, lat: float, lon: float) -> None:
    """Field, square, subsquare and extended square, any case."""
    place = grid_place(grid)
    assert place is not None
    assert place.lat == pytest.approx(lat, abs=1e-5)
    assert place.lon == pytest.approx(lon, abs=1e-5)


@pytest.mark.parametrize("grid", ["", None, "E", "EM2", "SS00", "EM28YY", "EM28qx4", "EM 28"])
def test_a_bad_grid_square_has_no_place(grid: str | None) -> None:
    """Anything that is not a Maidenhead locator is ignored, not guessed at."""
    assert grid_place(grid) is None


def test_adif_locations() -> None:
    """``XDDD MM.MMM``, with the hemisphere checked against the axis."""
    assert adif_degrees("N041 30.000", latitude=True) == pytest.approx(41.5)
    assert adif_degrees("S033 52.000", latitude=True) == pytest.approx(-33.866667)
    assert adif_degrees("W094 37.500", latitude=False) == pytest.approx(-94.625)
    assert adif_degrees("E151 12.5", latitude=False) == pytest.approx(151.208333)
    assert adif_degrees("E041 30.000", latitude=True) is None
    assert adif_degrees("N041 60.000", latitude=True) is None
    assert adif_degrees("N091 00.000", latitude=True) is None
    assert adif_degrees("41.5", latitude=True) is None


def test_a_qso_uses_lat_and_lon_before_its_grid() -> None:
    """Each end of the QSO from its own fields."""
    record = Record(
        (
            Field("GRIDSQUARE", "JO01"),
            Field("LAT", "N051 30.000"),
            Field("LON", "W000 07.500"),
            Field("MY_GRIDSQUARE", "EM28"),
        )
    )
    assert record_place(record) == Place(lat=51.5, lon=-0.125)
    assert record_place(record, mine=True) == Place(lat=38.5, lon=-95.0)
    assert record_place(Record((Field("CALL", "K1A"),))) is None


def test_distance() -> None:
    """Great-circle distance, about 7,060 km from Kansas to London."""
    kansas, london = Place(38.5, -95.0), Place(51.5, -0.125)
    assert distance_km(kansas, kansas) == 0
    assert 7000 < distance_km(kansas, london) < 7100
    assert distance_km(Place(0, 0), Place(0, 180)) == pytest.approx(20015, abs=1)


def test_the_world_map_has_every_continent_but_antarctica() -> None:
    """One shape per country, drawn in degrees, with no Antarctica."""
    world = json.loads(MAP.read_text(encoding="utf-8"))
    ids = [s["id"] for s in world["shapes"]]
    assert len(ids) == len(set(ids)) > 150
    assert {"USA", "CAN", "GBR", "JPN", "AUS", "BRA", "ZAF"} <= set(ids)
    assert "ATA" not in ids
    for shape in world["shapes"]:
        assert shape["d"].startswith("M")
        numbers = [
            float(n)
            for n in shape["d"].replace("M", " ").replace("L", " ").replace("Z", " ").split()
        ]
        xs, ys = numbers[0::2], numbers[1::2]
        assert min(xs) >= -180
        assert max(xs) <= 180
        assert min(ys) >= -90
        assert max(ys) <= -world["south"]
