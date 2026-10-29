# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reference data the Engine owns and serves itself (Principle VI)."""

from loglab_engine.reference.locator import (
    Place,
    distance_km,
    grid_place,
    place_grid,
    record_place,
)
from loglab_engine.reference.sections import (
    SECTIONS,
    SOURCE,
    Section,
    SectionTally,
    section_for,
    tally_sections,
)
from loglab_engine.reference.zones import FIELDS as ZONE_FIELDS
from loglab_engine.reference.zones import ZONES, ZoneKind, ZoneTally, tally_zones, zone_for

__all__ = [
    "SECTIONS",
    "SOURCE",
    "ZONES",
    "ZONE_FIELDS",
    "Place",
    "Section",
    "SectionTally",
    "ZoneKind",
    "ZoneTally",
    "distance_km",
    "grid_place",
    "place_grid",
    "record_place",
    "section_for",
    "tally_sections",
    "tally_zones",
    "zone_for",
]
