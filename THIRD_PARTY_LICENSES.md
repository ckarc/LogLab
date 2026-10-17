# Third-Party Licenses

Every third-party component that ships in LogLab is recorded here, with its
license (CONSTITUTION.md, Technology Constraints, Licensing). Only licenses
compatible with the AGPL-3.0 are permitted; CI enforces this with the
**License check** job.

## Shipped components

None yet. The Engine has no runtime dependencies and the Station Agent has no
crate dependencies.

## Development and CI tooling (not shipped)

These tools build, check, and test LogLab but are not distributed with it.
They are listed for transparency and are held to the same license policy.

| Component      | Used for                    | License                    |
| -------------- | --------------------------- | -------------------------- |
| uv, uv_build   | Engine environment, build   | MIT OR Apache-2.0          |
| mypy           | Engine strict type check    | MIT                        |
| Ruff           | Engine lint and format      | MIT                        |
| pytest         | Engine tests                | MIT                        |
| Rust toolchain | Agent build, Clippy, rustfmt | MIT OR Apache-2.0         |
| cargo-deny     | Agent license check         | MIT OR Apache-2.0          |
