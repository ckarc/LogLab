# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading pota.app's park list: untrusted input, read carefully (Principle XII)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from loglab_engine.reference.licenses import Tally
from loglab_engine.reference.parks import ParkFileError, park_reference, read_parks

if TYPE_CHECKING:
    from pathlib import Path

HEADER = '"reference","name","active","entityId","locationDesc","latitude","longitude","grid"'


def parks_csv(path: Path, *rows: str, header: str = HEADER) -> Path:
    """A park list like pota.app's ``all_parks_ext.csv``."""
    path.write_text("\n".join([header, *rows]) + "\n", encoding="utf-8")
    return path


def test_parks_read_with_their_location_and_place(tmp_path: Path) -> None:
    """Reference, name, active, locations, position and grid are kept."""
    path = parks_csv(
        tmp_path / "all_parks_ext.csv",
        '"US-0001","Acadia National Park","1","291","US-ME","44.31","-68.2034","FN54vh"',
        '"US-4567","Lake ""Two"" State Trail","0","291","US-WI,US-IL","42.5","-89.1","en52"',
    )
    tally = Tally()
    acadia, trail = read_parks(path, tally)
    assert (acadia.reference, acadia.name, acadia.active) == (
        "US-0001",
        "Acadia National Park",
        True,
    )
    assert (acadia.locations, acadia.latitude, acadia.longitude, acadia.grid) == (
        "US-ME",
        44.31,
        -68.2034,
        "FN54VH",
    )
    assert (trail.name, trail.active, trail.locations, trail.grid) == (
        'Lake "Two" State Trail',
        False,
        "US-WI,US-IL",
        "EN52",
    )
    assert (tally.read, tally.skipped) == (2, 0)


def test_columns_are_found_by_name(tmp_path: Path) -> None:
    """A reordered file with only the required columns still reads."""
    path = parks_csv(tmp_path / "p.csv", "Yellowstone,us-0006", header="name,reference")
    (park,) = read_parks(path, Tally())
    assert (park.reference, park.name, park.active, park.grid) == (
        "US-0006",
        "Yellowstone",
        True,
        None,
    )


def test_bad_parks_are_skipped_and_bad_places_dropped(tmp_path: Path) -> None:
    """A park needs a reference and a name; a nonsense position is not kept."""
    path = parks_csv(
        tmp_path / "p.csv",
        '"not a ref","Somewhere","1","1","US-ME","1","1",""',
        '"US-0002","","1","1","US-ME","1","1",""',
        '"US-0003","Good Park","1","1","<script>","95","10","ZZ99"',
        "x" * 5000,
    )
    tally = Tally()
    (park,) = read_parks(path, tally)
    assert park.reference == "US-0003"
    assert (park.locations, park.latitude, park.longitude, park.grid) == (None, None, None, None)
    assert (tally.read, tally.skipped) == (4, 3)


def test_a_file_that_is_not_a_park_list_is_refused(tmp_path: Path) -> None:
    """A maintenance page or an empty file is not loaded."""
    page = tmp_path / "page.csv"
    page.write_text("<html>down for maintenance</html>")
    with pytest.raises(ParkFileError, match="not a park list"):
        list(read_parks(page, Tally()))
    empty = tmp_path / "empty.csv"
    empty.write_text("")
    with pytest.raises(ParkFileError, match="empty"):
        list(read_parks(empty, Tally()))


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("us-1234", "US-1234"),
        (" VE-0042 ", "VE-0042"),
        ("K-12", None),
        ("US1234", None),
        (None, None),
    ],
)
def test_park_references(text: str | None, expected: str | None) -> None:
    """References look like US-1234: a prefix, a hyphen, four to six digits."""
    assert park_reference(text) == expected
