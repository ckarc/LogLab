<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# ham2k ARRL section table

`sections-for-states.json` is `data/sections-for-states.json` from the npm
package `@ham2k/lib-arrl-data` 1.2.0, copied unchanged. It says which ARRL or
RAC section each state, province, and (where ARRL splits a state) county is
in. `LICENSE.md` is that package's MIT license, which must travel with it.

`../build_section_map.py` reads it to draw the section map, correcting the two
Texas counties listed in that script's `CORRECTIONS`.
