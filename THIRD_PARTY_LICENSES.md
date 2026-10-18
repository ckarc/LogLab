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
| PostgreSQL 16  | CI test database (service container) | PostgreSQL License |
| Rust toolchain | Agent build, Clippy, rustfmt | MIT OR Apache-2.0         |
| cargo-deny     | Agent license check         | MIT OR Apache-2.0          |
