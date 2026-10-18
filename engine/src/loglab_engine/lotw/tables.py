# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""What Logbook of the World calls modes, bands and places, shipped with the Engine.

The tables come from TrustedQSL's published configuration
(``tools/build_lotw_tables.py``) and are read from the package, so signing a
QSO needs no internet connection (Principle V).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from functools import cache
from importlib.resources import files
from typing import Final, cast

ZonePair = tuple[int, int]


@dataclass(frozen=True, slots=True)
class Place:
    """A state or province a station location can name, with its ITU and CQ zones."""

    code: str
    name: str
    zones: tuple[ZonePair, ...]
    dxcc: int
    counties: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True)
class Tables:
    """The LoTW tables: modes, mode groups, bands, DXCC entities, states and provinces."""

    source: str
    modes: dict[str, str]
    mode_groups: dict[str, str]
    bands: frozenset[str]
    dxcc: dict[int, str]
    us_states: dict[str, Place]
    ca_provinces: dict[str, Place]

    def lotw_mode(self, mode: str, submode: str | None) -> str | None:
        """The mode LoTW knows an ADIF ``mode`` (and ``submode``) as, or ``None``.

        As TrustedQSL does: the submode's own entry first, then the mode's, then
        the mode itself if LoTW knows it by that name.
        """
        mode = mode.strip().upper()
        sub = (submode or "").strip().upper()
        found = self.modes.get(f"{mode}/{sub}") if sub else None
        if found is None:
            found = self.modes.get(mode)
        if found is None and mode in self.mode_groups:
            found = mode
        return found

    def group(self, lotw_mode: str) -> str | None:
        """The group (CW, PHONE, DATA or IMAGE) a LoTW mode belongs to."""
        return self.mode_groups.get(lotw_mode.upper())


US_DXCC: Final = frozenset({291, 6, 110})  # the USA, Alaska, Hawaii
CANADA_DXCC: Final = 1


def _zones(raw: object) -> tuple[ZonePair, ...]:
    pairs = cast("list[list[int]]", raw)
    return tuple((int(itu), int(cq)) for itu, cq in pairs)


@cache
def tables() -> Tables:
    """The tables shipped in ``tqsl-tables.json``."""
    text = (files("loglab_engine.lotw") / "tqsl-tables.json").read_text(encoding="utf-8")
    raw = cast("dict[str, object]", json.loads(text))
    states = cast("dict[str, dict[str, object]]", raw["us_states"])
    provinces = cast("dict[str, dict[str, object]]", raw["ca_provinces"])
    return Tables(
        source=str(raw["source"]),
        modes=cast("dict[str, str]", raw["modes"]),
        mode_groups=cast("dict[str, str]", raw["mode_groups"]),
        bands=frozenset(cast("list[str]", raw["bands"])),
        dxcc={int(k): str(v) for k, v in cast("dict[str, str]", raw["dxcc"]).items()},
        us_states={
            code: Place(
                code,
                str(s["name"]),
                _zones(s["zones"]),
                int(cast("int", s["dxcc"])),
                tuple(cast("list[str]", s["counties"])),
            )
            for code, s in states.items()
        },
        ca_provinces={
            code: Place(code, str(p["name"]), _zones(p["zones"]), CANADA_DXCC)
            for code, p in provinces.items()
        },
    )
