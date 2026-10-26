# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The ARRL and RAC section list, and the map the web client draws from it."""

from __future__ import annotations

import json
import re
from importlib.resources import files

from loglab_engine.reference import SECTIONS, section_for, tally_sections

MAP = files("loglab_engine") / "web" / "maps" / "arrl-sections.json"


def test_every_section_once() -> None:
    """71 ARRL sections in the US and its territories, 14 RAC sections in Canada."""
    codes = [s.code for s in SECTIONS]
    assert len(codes) == len(set(codes)) == 85
    assert sum(s.country == "Canada" for s in SECTIONS) == 14
    assert {"KS", "NLI", "WTX", "PAC", "MDC", "GH", "ONN", "TER"} <= set(codes)


def test_the_map_draws_every_section_and_nothing_else() -> None:
    """Each section is on the bundled map exactly once, so every one can be coloured in."""
    drawn = json.loads(MAP.read_text(encoding="utf-8"))
    on_map = [code for shape in drawn["shapes"] for code in shape["sections"]]
    assert sorted(on_map) == sorted(s.code for s in SECTIONS)
    for shape in drawn["shapes"]:
        assert re.fullmatch(r"(M[-\d.]+ [-\d.]+(L[-\d.]+ [-\d.]+)*Z)+", shape["d"]), shape["id"]
        x, y = shape["label"]
        assert 0 <= x <= drawn["width"]
        assert 0 <= y <= drawn["height"]


def test_section_values_are_read_as_logs_write_them() -> None:
    """Case and spaces do not matter; a few unambiguous old names still count."""
    assert section_for(" ks ") == section_for("KS")
    found = section_for("pq")
    assert found is not None
    assert found.code == "QC"
    assert section_for("MAR") is None  # the Maritimes: now three sections
    assert section_for("") is None
    assert section_for("DX") is None


def test_tally_counts_sections_and_says_what_it_could_not_place() -> None:
    """Worked sections with their QSOs; unknown values and missing sections reported."""
    tally = tally_sections({"KS": 3, "ks ": 1, "PQ": 2, "MAR": 1, "": 5, "  ": 1})
    assert tally.worked == {"KS": 4, "QC": 2}
    assert tally.unrecognized == {"MAR": 1}
    assert tally.without_section == 6
