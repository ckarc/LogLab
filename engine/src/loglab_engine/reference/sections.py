# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The ARRL and RAC sections: the places ARRL Field Day counts.

The Engine keeps this list itself, so the section map and anything else that
needs it work with no internet connection (Principles V and VI). The list is
data, not code: it changes when the ARRL or RAC changes its sections, and the
date it was last checked is recorded in :data:`SOURCE`.

A QSO's section is its ADIF ``ARRL_SECT`` field. Logs from other programs
sometimes use a section's old abbreviation; the few whose meaning is
unambiguous are read as today's section (``PQ`` is Quebec). An old
abbreviation that now covers several sections (``MAR``, the Maritimes, now
NB, NS and PE) cannot be placed and is reported as not recognized.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Final


@dataclass(frozen=True)
class Section:
    """One ARRL or RAC section."""

    code: str
    name: str
    division: str
    country: str


SOURCE: Final = (
    "ARRL and RAC section list as published in @ham2k/lib-arrl-data 1.2.0 (MIT), checked 2026-10-18"
)

SECTIONS: Final[tuple[Section, ...]] = (
    Section("AB", "Alberta", "RAC", "Canada"),
    Section("AK", "Alaska", "Northwestern", "United States"),
    Section("AL", "Alabama", "Southeastern", "United States"),
    Section("AR", "Arkansas", "Delta", "United States"),
    Section("AZ", "Arizona", "Southwestern", "United States"),
    Section("BC", "British Columbia", "RAC", "Canada"),
    Section("CO", "Colorado", "Rocky Mountain", "United States"),
    Section("CT", "Connecticut", "New England", "United States"),
    Section("DE", "Delaware", "Atlantic", "United States"),
    Section("EB", "East Bay", "Pacific", "United States"),
    Section("EMA", "Eastern Massachusetts", "New England", "United States"),
    Section("ENY", "Eastern New York", "Hudson", "United States"),
    Section("EPA", "Eastern Pennsylvania", "Atlantic", "United States"),
    Section("EWA", "Eastern Washington", "Northwestern", "United States"),
    Section("GA", "Georgia", "Southeastern", "United States"),
    Section("GH", "Golden Horseshoe", "RAC", "Canada"),
    Section("IA", "Iowa", "Midwest", "United States"),
    Section("ID", "Idaho", "Northwestern", "United States"),
    Section("IL", "Illinois", "Central", "United States"),
    Section("IN", "Indiana", "Central", "United States"),
    Section("KS", "Kansas", "Midwest", "United States"),
    Section("KY", "Kentucky", "Great Lakes", "United States"),
    Section("LA", "Louisiana", "Delta", "United States"),
    Section("LAX", "Los Angeles", "Southwestern", "United States"),
    Section("MB", "Manitoba", "RAC", "Canada"),
    Section("MDC", "Maryland-DC", "Atlantic", "United States"),
    Section("ME", "Maine", "New England", "United States"),
    Section("MI", "Michigan", "Great Lakes", "United States"),
    Section("MN", "Minnesota", "Dakota", "United States"),
    Section("MO", "Missouri", "Midwest", "United States"),
    Section("MS", "Mississippi", "Delta", "United States"),
    Section("MT", "Montana", "Northwestern", "United States"),
    Section("NB", "New Brunswick", "RAC", "Canada"),
    Section("NC", "North Carolina", "Roanoke", "United States"),
    Section("ND", "North Dakota", "Dakota", "United States"),
    Section("NE", "Nebraska", "Midwest", "United States"),
    Section("NFL", "Northern Florida", "Southeastern", "United States"),
    Section("NH", "New Hampshire", "New England", "United States"),
    Section("NL", "Newfoundland/Labrador", "RAC", "Canada"),
    Section("NLI", "New York City-Long Island", "Hudson", "United States"),
    Section("NM", "New Mexico", "Rocky Mountain", "United States"),
    Section("NNJ", "Northern New Jersey", "Hudson", "United States"),
    Section("NNY", "Northern New York", "Atlantic", "United States"),
    Section("NS", "Nova Scotia", "RAC", "Canada"),
    Section("NTX", "North Texas", "West Gulf", "United States"),
    Section("NV", "Nevada", "Pacific", "United States"),
    Section("OH", "Ohio", "Great Lakes", "United States"),
    Section("OK", "Oklahoma", "West Gulf", "United States"),
    Section("ONE", "Ontario East", "RAC", "Canada"),
    Section("ONN", "Ontario North", "RAC", "Canada"),
    Section("ONS", "Ontario South", "RAC", "Canada"),
    Section("OR", "Oregon", "Northwestern", "United States"),
    Section("ORG", "Orange", "Southwestern", "United States"),
    Section("PAC", "Pacific", "Pacific", "United States"),
    Section("PE", "Prince Edward Island", "RAC", "Canada"),
    Section("PR", "Puerto Rico", "Southeastern", "United States"),
    Section("QC", "Quebec", "RAC", "Canada"),
    Section("RI", "Rhode Island", "New England", "United States"),
    Section("SB", "Santa Barbara", "Southwestern", "United States"),
    Section("SC", "South Carolina", "Roanoke", "United States"),
    Section("SCV", "Santa Clara Valley", "Pacific", "United States"),
    Section("SD", "South Dakota", "Dakota", "United States"),
    Section("SDG", "San Diego", "Southwestern", "United States"),
    Section("SF", "San Francisco", "Pacific", "United States"),
    Section("SFL", "Southern Florida", "Southeastern", "United States"),
    Section("SJV", "San Joaquin Valley", "Pacific", "United States"),
    Section("SK", "Saskatchewan", "RAC", "Canada"),
    Section("SNJ", "Southern New Jersey", "Atlantic", "United States"),
    Section("STX", "South Texas", "West Gulf", "United States"),
    Section("SV", "Sacramento Valley", "Pacific", "United States"),
    Section("TER", "Territories", "RAC", "Canada"),
    Section("TN", "Tennessee", "Delta", "United States"),
    Section("UT", "Utah", "Rocky Mountain", "United States"),
    Section("VA", "Virginia", "Roanoke", "United States"),
    Section("VI", "Virgin Islands", "Southeastern", "United States"),
    Section("VT", "Vermont", "New England", "United States"),
    Section("WCF", "West Central Florida", "Southeastern", "United States"),
    Section("WI", "Wisconsin", "Central", "United States"),
    Section("WMA", "Western Massachusetts", "New England", "United States"),
    Section("WNY", "Western New York", "Atlantic", "United States"),
    Section("WPA", "Western Pennsylvania", "Atlantic", "United States"),
    Section("WTX", "West Texas", "West Gulf", "United States"),
    Section("WV", "West Virginia", "Roanoke", "United States"),
    Section("WWA", "Western Washington", "Northwestern", "United States"),
    Section("WY", "Wyoming", "Rocky Mountain", "United States"),
)

_BY_CODE: Final = {s.code: s for s in SECTIONS}
_OLD_NAMES: Final = {"PQ": "QC", "NWT": "TER", "GTA": "GH"}


def section_for(value: str) -> Section | None:
    """The section an ``ARRL_SECT`` value names, ignoring case and spaces, if any."""
    code = value.strip().upper()
    return _BY_CODE.get(_OLD_NAMES.get(code, code))


@dataclass(frozen=True)
class SectionTally:
    """How many QSOs the log has in each section, and what could not be counted.

    ``worked`` maps each section code to its number of QSOs (sections with
    none are left out). ``unrecognized`` maps each ``ARRL_SECT`` value that
    names no section to its number of QSOs. ``without_section`` counts the QSOs
    with no ``ARRL_SECT`` at all.
    """

    worked: Mapping[str, int]
    unrecognized: Mapping[str, int]
    without_section: int


def tally_sections(values: Mapping[str, int]) -> SectionTally:
    """Tally sections from ``ARRL_SECT`` values and how many QSOs carry each.

    The empty string stands for QSOs with no ``ARRL_SECT``.
    """
    worked: dict[str, int] = {}
    unrecognized: dict[str, int] = {}
    without = 0
    for value, count in values.items():
        if not value.strip():
            without += count
            continue
        section = section_for(value)
        if section is None:
            key = value.strip().upper()
            unrecognized[key] = unrecognized.get(key, 0) + count
        else:
            worked[section.code] = worked.get(section.code, 0) + count
    return SectionTally(worked, unrecognized, without)
