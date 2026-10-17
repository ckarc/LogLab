# LogLab

An original, open-source amateur radio logging system built on an API engine.

- **One log, many templates** — contests are definition files, not separate programs.
- **Engine** (Python + PostgreSQL) owns the log, reference data, and operator identities.
- **Station Agent** (Rust) owns the radios and shares them with other software.
- **Web client** served by the Engine — works on any device, with no internet connection.

LogLab is in early development. Start with [`CONSTITUTION.md`](CONSTITUTION.md), the principles every contribution must uphold.

A project of the Central Kansas Amateur Radio Club (W0CY). Maintained by Brian Krenzin, K9KSU.

Licensed under the [GNU Affero General Public License v3.0](LICENSE).
