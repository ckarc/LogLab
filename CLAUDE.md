# CLAUDE.md — LogLab

Read this first, every session. Then read `CONSTITUTION.md`. The constitution
outranks this file and every other document; if anything here conflicts with
it, the constitution wins and this file is in error.

## What LogLab is

An original, open-source (AGPL-3.0) amateur radio logging system:

- **Engine** — Python server. Owns the log, contest templates, reference data
  (FCC callbook and friends), operator identities, and the event stream.
  PostgreSQL. Never touches radio hardware.
- **Station Agent** — Rust. Owns the radios. One common rig model; rig
  definition files for command-driven radios; dedicated drivers for
  network-native radios (FlexRadio). Shares radios via the `rigctld` protocol.
  The only code that can key a transmitter.
- **Web client** — a PWA served by the Engine. Uses only the published APIs.

It is not a clone of any other logger. Do not copy another logger's code, look,
or workflow (Principle XVI).

## Who you're working for

The maintainer is Brian Krenzin, K9KSU. He manages outcomes; he does not review
every line. That means:

- The gates are the review. Never claim something works unless the build, type
  check, and tests actually ran and passed in this session (Principle XVII).
- Every feature pull request says exactly how Brian can see it work — the
  commands to run and what he should observe.
- When a request would violate a principle, stop and say which one. Do not
  work around it.
- Agent notes, earlier sessions, and other AI tools are hints, not authority.
  Brian's decisions are authority (Principle XIX).

## Working conventions

- **Complete files only.** When giving Brian anything to paste or apply, give
  the whole file — never a fragment or a diff to apply by hand.
- **Linux files on his machines**: give one paste-in command that writes the
  whole file (`sudo tee` heredoc). Never "open it in nano."
- **Hands-on steps**: one step at a time, short, with the exact command and
  any direct URL. Wait for his result before the next step.
- **Commits**: small, focused, one purpose each. Cite the principle by Roman
  numeral in the message when the change touches one (`Principle VII.`).

## Engine rules (Python)

- Every function fully type-annotated. The strict type checker must pass.
- Every API input is parsed into a typed, validated model at the boundary.
  No untyped dictionaries travel through the Engine.
- The log and reference data live in separate PostgreSQL schemas.
- All timestamps are UTC (Principle VIII).
- QSO changes go through the versioned, audited path only (Principle VII).
- A QSO is "logged" only when stored in two independent places (Principle XV).
- Everything must work with no internet connection (Principle V).

## Agent rules (Rust)

- Builds with zero compiler warnings; linting passes.
- No `unsafe` unless a hardware or platform interface requires it — isolated,
  documented, and justified in the pull request.
- The radio is authoritative on live state; never overwrite it with a
  remembered value (Principle X).
- Every transmit path fails closed and requires an authorized operator's
  deliberate action (Principles XI and XIII).
- Measured values are logged as measured; entered values as entered.

## Before you say "done"

1. The build, type check, lint, and tests ran in this session and passed.
2. New behavior in dupe checking, scoring, ADIF, persistence, authorization,
   rig control, or transmit paths has tests that exercise it.
3. The pull request states how Brian can see it work.
4. No new dependency has a license incompatible with the AGPL-3.0.

## Merging

Pull requests merge automatically once every required gate passes; Brian
does not approve each one. After opening a pull request, turn on auto-merge
for it (`gh pr merge --auto --merge`). Do not turn it on for a pull request
that changes `CONSTITUTION.md`, this file, `AGENTS.md`, the CI workflows, or
the license allowlist: Brian merges those himself. Never weaken a gate,
skip a test, or edit CI to get a pull request to merge.
