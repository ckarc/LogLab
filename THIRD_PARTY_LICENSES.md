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
