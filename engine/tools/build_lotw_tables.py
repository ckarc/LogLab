# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Build the tables LabLog needs to sign QSOs for Logbook of the World.

This is a developer tool, not part of the Engine: its output,
``src/loglab_engine/lotw/tqsl-tables.json``, is committed, so LabLog signs
QSOs with no internet connection (Principle V) and nothing here runs in the
field. Run it only to refresh the tables from a newer TrustedQSL release::

    python tools/build_lotw_tables.py config.xml

Source (THIRD_PARTY_LICENSES.md): ``src/config.xml`` from the TrustedQSL
source code (sourceforge.net/p/trustedqsl/tqsl), published by the ARRL and
the TrustedQSL developers under the license kept in ``tools/trustedqsl/``.
Only facts are taken from it: which LoTW mode each ADIF mode and submode is
sent as and its group (CW, PHONE, DATA or IMAGE), the band names, the DXCC
entity names, and the US states and counties and Canadian provinces a station
location names, with the ITU and CQ zones of each.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from xml.etree import ElementTree

OUT = Path(__file__).resolve().parent.parent / "src/loglab_engine/lotw/tqsl-tables.json"
# DXCC entities whose station locations name a US state and county.
US_ENTITIES = ("291", "6", "110")


def _zones(zonemap: str | None) -> list[list[int]]:
    """``"07:04,08:04"`` (ITU:CQ pairs) as ``[[7, 4], [8, 4]]``."""
    if not zonemap:
        return []
    return [[int(z) for z in pair.split(":")] for pair in zonemap.split(",")]


def build(config: bytes) -> dict[str, object]:
    """The tables, from the bytes of TrustedQSL's ``config.xml``."""
    root = ElementTree.fromstring(config)  # noqa: S314 - a trusted file, read by hand
    modes: dict[str, str] = {}
    for item in root.iterfind("adifmap/adifmode"):
        key = item.get("adif-mode", "")
        if item.get("adif-submode"):
            key += "/" + item.get("adif-submode", "")
        modes[key] = item.get("mode", "")
    fields = {f.get("Id"): f for f in root.iterfind("locfields/field")}
    states: dict[str, dict[str, object]] = {}
    for enums in fields["US_STATE"].iterfind("enums"):
        if enums.get("dependency") in US_ENTITIES:
            for e in enums.iterfind("enum"):
                states[e.get("value", "")] = {
                    "dxcc": int(enums.get("dependency", "0")),
                    "name": e.text,
                    "zones": _zones(e.get("zonemap")),
                    "counties": [],
                }
    for enums in fields["US_COUNTY"].iterfind("enums"):
        state = states.get(enums.get("dependency", ""))
        if state is not None:
            state["counties"] = sorted(e.get("value", "") for e in enums.iterfind("enum"))
    provinces = {
        e.get("value", ""): {"name": e.text, "zones": _zones(e.get("zonemap"))}
        for e in fields["CA_PROVINCE"].iterfind("enums/enum")
    }
    return {
        "source": "TrustedQSL src/config.xml, version "
        f"{root.get('majorversion')}.{root.get('minorversion')}, "
        f"sha256 {hashlib.sha256(config).hexdigest()}",
        "modes": dict(sorted(modes.items())),
        "dxcc": {e.get("arrlId", ""): e.text for e in root.iterfind("dxcc/entity")},
        "mode_groups": {m.text or "": m.get("group", "") for m in root.iterfind("modes/mode")},
        "bands": sorted(b.text or "" for b in root.iterfind("bands/band")),
        "us_states": dict(sorted(states.items())),
        "ca_provinces": dict(sorted(provinces.items())),
    }


if __name__ == "__main__":
    tables = build(Path(sys.argv[1]).read_bytes())
    OUT.write_text(json.dumps(tables, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
