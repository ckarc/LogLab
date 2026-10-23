# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading the Parks on the Air park list, as pota.app publishes it.

pota.app publishes every park as one CSV file, ``all_parks_ext.csv``, column
names first: ``reference``, ``name``, ``active``, ``entityId``,
``locationDesc``, ``latitude``, ``longitude`` and ``grid``. Columns are found
by name, so a reordered or extended file still reads; only ``reference`` and
``name`` must be there.

The download is untrusted (Principle XII). A file larger than
:data:`MAX_FILE` is refused; a line longer than :data:`MAX_LINE` is skipped
and counted; every value kept is cleaned and length-capped; a park without a
valid reference or a name is skipped and counted, so the importer can refuse
a file that is mostly unreadable. A position or grid that does not make sense
is dropped, keeping the park.
"""

from __future__ import annotations

import csv
import io
import math
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

from loglab_engine.reference.licenses import LicenseFileError, Tally, clean_text

if TYPE_CHECKING:
    from collections.abc import Iterator
    from pathlib import Path

PARKS_CREDIT: Final = "Park list: Parks on the Air (pota.app)."
MAX_FILE: Final = 64 * 1024 * 1024
MAX_LINE: Final = 4096

_REFERENCE: Final = re.compile(r"^[A-Z0-9]{1,4}-[0-9]{4,6}$")
_GRID: Final = re.compile(r"^[A-R]{2}[0-9]{2}(?:[A-X]{2}(?:[0-9]{2})?)?$")
_LOCATION: Final = re.compile(r"^[A-Z0-9]{1,4}-[A-Z0-9]{1,4}$")
_REQUIRED: Final = ("reference", "name")


class ParkFileError(LicenseFileError):
    """A park list download is not the file it should be."""


@dataclass(frozen=True, slots=True)
class Park:
    """One park in the list."""

    reference: str
    name: str
    active: bool
    locations: str | None
    latitude: float | None
    longitude: float | None
    grid: str | None


def park_reference(text: str | None) -> str | None:
    """``text`` as a park reference, like ``US-1234``, or ``None`` if it is not one."""
    value = (text or "").strip().upper()
    return value if _REFERENCE.match(value) else None


def _degrees(text: str | None, limit: float) -> float | None:
    try:
        value = float((text or "").strip())
    except ValueError:
        return None
    return value if math.isfinite(value) and -limit <= value <= limit else None


def _locations(text: str | None) -> str | None:
    codes = [c.strip().upper() for c in (text or "").split(",")]
    kept = [c for c in codes if _LOCATION.match(c)]
    return ",".join(kept)[:200] or None


def _grid(text: str | None) -> str | None:
    value = (text or "").strip().upper()
    return value if _GRID.match(value) else None


def _active(text: str | None) -> bool:
    return (text or "").strip().lower() not in {"0", "false", "no", "n", "inactive"}


def _rows(path: Path, tally: Tally) -> Iterator[dict[str, str]]:
    try:
        if path.stat().st_size > MAX_FILE:
            raise ParkFileError(f"{path.name} is larger than {MAX_FILE} bytes")
        raw = path.read_bytes()
    except OSError as error:
        raise ParkFileError(f"{path.name} could not be read ({error})") from error
    text = raw.decode("utf-8-sig", errors="replace")
    lines = [line for line in text.splitlines() if line.strip()]
    if not lines:
        raise ParkFileError(f"{path.name} is empty")
    header = next(csv.reader([lines[0]]))
    names = [h.strip().lower() for h in header]
    if any(required not in names for required in _REQUIRED):
        raise ParkFileError(f"{path.name} is not a park list (no reference and name columns)")
    for line in lines[1:]:
        tally.read += 1
        if len(line) > MAX_LINE:
            tally.skip(f"a line longer than {MAX_LINE} characters")
            continue
        try:
            values = next(csv.reader(io.StringIO(line)))
        except (csv.Error, StopIteration):
            tally.skip("a line that is not CSV")
            continue
        yield dict(zip(names, values, strict=False))


def read_parks(path: Path, tally: Tally) -> Iterator[Park]:
    """Every usable park in a pota.app park list, counting the ones skipped in ``tally``.

    Raises:
        ParkFileError: The file is too large, unreadable, or not a park list.
    """
    for row in _rows(path, tally):
        reference = park_reference(row.get("reference"))
        name = clean_text(row.get("name"), 200)
        if reference is None or name is None:
            tally.skip(
                f"a park without a valid reference or name ({row.get('reference', '')[:20]!r})"
            )
            continue
        latitude = _degrees(row.get("latitude"), 90)
        longitude = _degrees(row.get("longitude"), 180)
        if latitude is None or longitude is None:
            latitude = longitude = None
        yield Park(
            reference=reference,
            name=name,
            active=_active(row.get("active")),
            locations=_locations(row.get("locationdesc")),
            latitude=latitude,
            longitude=longitude,
            grid=_grid(row.get("grid")),
        )
