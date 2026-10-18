# Third-Party Licenses

Every third-party component that ships in LogLab is recorded here, with its
license (CONSTITUTION.md, Technology Constraints, Licensing). Only licenses
compatible with the AGPL-3.0 are permitted; CI enforces this with the
**License check** job.

## Shipped components

The Station Agent has no crate dependencies yet. The Engine's runtime
dependencies:

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

psycopg is used unmodified as a separate library, as the LGPL-3.0 permits.
It loads the system's PostgreSQL client library, libpq (PostgreSQL License,
a permissive BSD-style license), which LogLab does not ship.

## Shipped data

| Component | Used for | License |
| --------- | -------- | ------- |
| Natural Earth 1:10m admin-1 states and provinces, admin-2 counties, lakes ([naturalearthdata.com](https://www.naturalearthdata.com), commit `ca96624` of github.com/nvkelso/natural-earth-vector) | The shapes of the section map (`engine/src/loglab_engine/web/maps/arrl-sections.json`) | Public domain |
| [@ham2k/lib-arrl-data](https://github.com/ham2k/ham-js-libs) 1.2.0 | The ARRL and RAC section list, and which county is in which section (`engine/tools/ham2k/`, with its license) | MIT |

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
| Shapely, NumPy | Rebuilding the section map (`engine/tools/build_section_map.py`, run by hand) | BSD-3-Clause |
