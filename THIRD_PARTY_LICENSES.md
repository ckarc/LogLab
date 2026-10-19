# Third-Party Licenses

Every third-party component that ships in LogLab is recorded here, with its
license (CONSTITUTION.md, Technology Constraints, Licensing). Only licenses
compatible with the AGPL-3.0 are permitted; CI enforces this with the
**License check** job.

## Shipped components

The Station Agent's crates, compiled into its single executable:

| Component        | Used for                                  | License                          |
| ---------------- | ----------------------------------------- | -------------------------------- |
| ureq, ureq-proto | Sending the radio's state to the Engine   | MIT OR Apache-2.0                |
| http, httparse   | ureq dependencies (HTTP types and parser) | MIT OR Apache-2.0                |
| base64, log, percent-encoding, utf8-zero | ureq dependencies | MIT OR Apache-2.0           |
| bytes            | http dependency                           | MIT                              |
| itoa             | http and serde_json dependency            | MIT OR Apache-2.0                |
| serde, serde_core, serde_json | Writing the Agent's reports as JSON | MIT OR Apache-2.0           |
| memchr           | serde_json dependency                     | Unlicense OR MIT                 |
| zmij             | serde_json dependency (number formatting) | MIT                              |
| ctrlc            | Stopping Hamlib when the Agent is stopped | MIT OR Apache-2.0                |
| rustfft          | FFTs for the FT8 decoder                  | MIT OR Apache-2.0                |
| num-complex, num-traits, num-integer, primal-check, strength_reduce, transpose | rustfft dependencies | MIT OR Apache-2.0 |
| nix, libc, bitflags, cfg-if | ctrlc dependencies (Linux and macOS) | MIT (nix); MIT OR Apache-2.0 |
| windows-sys, windows-link | ctrlc dependencies (Windows)     | MIT OR Apache-2.0                |
| dispatch2, block2, objc2, objc2-encode | ctrlc dependencies (macOS) | MIT                    |
| serde_derive, syn, proc-macro2, quote | Build time only (serde's derive) | MIT OR Apache-2.0  |
| cfg_aliases      | Build time only (nix dependency)          | MIT                              |
| unicode-ident    | Build time only (syn dependency)          | (MIT OR Apache-2.0) AND Unicode-3.0 |
| autocfg          | Build time only (num-traits dependency)   | MIT OR Apache-2.0                |

The Agent can also run Hamlib's `rigctld` (https://hamlib.github.io; the
library is LGPL-2.1-or-later, its programs GPL-2.0-or-later) to reach a
radio. Hamlib is installed separately and runs as its own program, which the
Agent talks to over Hamlib's documented network protocol; none of its code is
in LogLab, and LogLab does not ship it.

The Engine's runtime dependencies:

| Component         | Used for                           | License       |
| ----------------- | ---------------------------------- | ------------- |
| psycopg           | Engine PostgreSQL driver           | LGPL-3.0-only |
| typing_extensions | psycopg dependency (Python < 3.13) | PSF-2.0       |
| tzdata            | psycopg dependency (Windows only)  | Apache-2.0    |
| FastAPI           | Engine HTTP API                    | MIT           |
| Starlette         | FastAPI dependency (web framework) | BSD-3-Clause  |
| Pydantic, pydantic-core | API input validation (Principle XII) | MIT  |
| annotated-types   | Pydantic dependency                | MIT           |
| typing-inspection | Pydantic dependency                | MIT           |
| annotated-doc     | FastAPI dependency                 | MIT           |
| opentelemetry-api | FastAPI dependency (unused hooks)  | Apache-2.0    |
| AnyIO             | Starlette dependency               | MIT           |
| idna              | AnyIO dependency                   | BSD-3-Clause  |
| Uvicorn           | Engine HTTP server                 | BSD-3-Clause  |
| Click             | Uvicorn dependency                 | BSD-3-Clause  |
| h11               | Uvicorn dependency (HTTP/1.1)      | MIT           |
| cryptography      | Reading LoTW certificates, signing QSOs | Apache-2.0 OR BSD-3-Clause |
| cffi              | cryptography dependency            | MIT           |
| pycparser         | cffi dependency                    | BSD-3-Clause  |

psycopg is used unmodified as a separate library, as the LGPL-3.0 permits.
It loads the system's PostgreSQL client library, libpq (PostgreSQL License,
a permissive BSD-style license), which LogLab does not ship.

## Shipped data

| Component | Used for | License |
| --------- | -------- | ------- |
| Natural Earth 1:10m admin-1 states and provinces, admin-2 counties, lakes ([naturalearthdata.com](https://www.naturalearthdata.com), commit `ca96624` of github.com/nvkelso/natural-earth-vector) | The shapes of the section map (`engine/src/loglab_engine/web/maps/arrl-sections.json`) | Public domain |
| Natural Earth 1:110m admin-0 countries (same source and commit) | The countries of the world map (`engine/src/loglab_engine/web/maps/world.json`, built by `engine/tools/build_world_map.py`) | Public domain |
| [HB9HIL/hamradio-zones-geojson](https://github.com/HB9HIL/hamradio-zones-geojson), commit `091b813` | The shapes of the CQ zone and ITU zone maps (`engine/src/loglab_engine/web/maps/cq-zones.json` and `itu-zones.json`, built by `engine/tools/build_zone_maps.py`; the license is kept in `engine/tools/hb9hil-zones/LICENSE`) | MIT |
| [@ham2k/lib-arrl-data](https://github.com/ham2k/ham-js-libs) 1.2.0 | The ARRL and RAC section list, and which county is in which section (`engine/tools/ham2k/`, with its license) | MIT |

| [ft8_lib](https://github.com/kgoba/ft8_lib) by Kārlis Goba, YL3JG, commit `9fec6ca` | The generator matrix and parity checks of FT8's (174,91) LDPC code (`agent/src/ft8/ldpc_generator.rs` and `ldpc_checks.rs`, converted from its tables; the license is kept in `agent/src/ft8/ft8_lib-LICENSE.txt`) | MIT |

| [TrustedQSL](https://sourceforge.net/p/trustedqsl/tqsl/) configuration 11.34 (`src/config.xml`) | Which LoTW mode each ADIF mode is sent as, band and DXCC entity names, US states and counties and Canadian provinces with their zones (`engine/src/loglab_engine/lotw/tqsl-tables.json`, built by `engine/tools/build_lotw_tables.py`; the license is kept in `engine/tools/trustedqsl/LICENSE.txt`) | TrustedQSL license (BSD-style) |

TrustedQSL is Copyright (C) 2001-2015 American Radio Relay League, Inc.,
portions (C) 2003-2026 The TrustedQSL Developers, under a BSD-style license
whose notice and disclaimer travel with the data in
`engine/tools/trustedqsl/LICENSE.txt`. Its conditions are to keep the notice
and disclaimer, not to name a product after ARRL's marks ("Logbook of the
World", "LoTW"), and not to claim ARRL's endorsement. LogLab is not named
after them and claims no endorsement. How LogLab builds and signs LoTW
upload files was learned from TrustedQSL's published source (Principle XVI);
no TrustedQSL code is copied.

Natural Earth's terms place its data in the public domain, with no
conditions. The ham2k section table is MIT-licensed; its copyright and
permission notice ship with it in `engine/tools/ham2k/LICENSE.md` and are
repeated here: Copyright ©2023 Sebastian Delmont. Permission is hereby
granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software
without restriction, including without limitation the rights to use, copy,
modify, merge, publish, distribute, sublicense, and/or sell copies of the
Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions: The above copyright notice and this
permission notice (including the next paragraph) shall be included in all
copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS
IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

The CQ and ITU zone shapes are MIT-licensed under the same terms as above;
their notice ships in `engine/tools/hb9hil-zones/LICENSE` and in each map
file's `source`: Copyright (c) 2024 HB9HIL.

The FT8 LDPC generator matrix and parity checks are published protocol constants,
the same in every FT8 implementation (FT8 itself is by K1JT, K9AN and G4WJS).
LogLab's copy was converted from ft8_lib's table; its MIT copyright and
permission notice travel with it in `agent/src/ft8/ft8_lib-LICENSE.txt`.
No ft8_lib code is copied. LogLab's FT8 message packing follows the
published protocol and WSJT-X's published source (Principle XVI) and is
checked tone for tone against WSJT-X's own output.

## Development and CI tooling (not shipped)

These tools build, check, and test LogLab but are not distributed with it.
They are listed for transparency and are held to the same license policy.

| Component      | Used for                    | License                    |
| -------------- | --------------------------- | -------------------------- |
| uv, uv_build   | Engine environment, build   | MIT OR Apache-2.0          |
| mypy           | Engine strict type check    | MIT                        |
| Ruff           | Engine lint and format      | MIT                        |
| pytest         | Engine tests                | MIT                        |
| Hypothesis     | Engine property-based tests | MPL-2.0                    |
| httpx2, httpcore2, truststore | Engine API tests (test client) | BSD-3-Clause, MIT |
| PostgreSQL 16  | CI test database (service container) | PostgreSQL License |
| Rust toolchain | Agent build, Clippy, rustfmt | MIT OR Apache-2.0         |
| cargo-deny     | Agent license check         | MIT OR Apache-2.0          |
| WSJT-X 3.0.2 (`jt9`, `ft8code`), with gfortran, FFTW3, Qt 5, Boost, Hamlib and PortAudio to build it | The reference FT8 decoder and encoder the FT8 bench compares against (`agent/tools/ft8-reference/`, built and run by hand); `ft8code`'s output is the expected tones in `agent/tests/data/ft8-wsjtx-tones.tsv` | GPL-3.0 (WSJT-X); GPL-2.0-or-later, LGPL and permissive (its build dependencies) |
| Shapely, NumPy | Rebuilding the section map (`engine/tools/build_section_map.py`, run by hand) | BSD-3-Clause |
