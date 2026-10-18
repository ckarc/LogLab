# LogLab Engine

The Engine is LogLab's server: it owns the log, contest templates, reference
data, operator identities, and the event stream. It is strictly typed Python
on PostgreSQL. See [`../CONSTITUTION.md`](../CONSTITUTION.md).

## Run the gates locally

Requires [uv](https://docs.astral.sh/uv/) and a PostgreSQL 16 server the
current user can create databases on. The storage tests create and drop their
own `loglab_test_*` databases; point libpq at the server with the usual
`PGHOST`/`PGUSER`/... variables if it isn't the local default.

```sh
cd engine
uv sync --locked
uv run ruff format --check
uv run ruff check
uv run mypy
uv run pytest
```

## ADIF tools

`loglab-adif` reads and writes ADI files (ADIF 3.1.7) losslessly and generates
ghost QSOs, synthetic test contacts that can never pass for real ones.

```sh
uv run loglab-adif ghosts -o ghosts.adi          # 1000 ghost QSOs (-n to change)
uv run loglab-adif check ghosts.adi              # validate and summarize
uv run loglab-adif roundtrip ghosts.adi -o copy.adi   # read, rewrite, prove nothing changed
```

Ghost files are test data. Never import them into a real log or upload them to
any service.

## The log in PostgreSQL

`loglab-log` stores QSOs in PostgreSQL. The log lives in the `log` schema and
reference data in `reference` (Principle VI). Every QSO is a series of complete
ADIF snapshots: an edit, a delete, and a restore each add a version that
records the operator, station, and UTC time, and nothing is ever erased
(Principle VII). Storing a QSO is not yet *logging* it: that needs a second
independent copy (Principle XV), which comes later.

```sh
createdb loglab                                   # once
uv run loglab-log migrate                         # apply schema migrations
uv run loglab-operator setup K9KSU                # once: the first administrator
uv run loglab-operator sign-in K9KSU --station shack
uv run loglab-log import ghosts.adi
uv run loglab-log find -n 5                       # QSO IDs, newest last
uv run loglab-log edit 1a2b3c4d --set RST_RCVD=339 --note "typo"
uv run loglab-log delete 1a2b3c4d
uv run loglab-log history 1a2b3c4d                # every version, who, when, what changed
uv run loglab-log restore 1a2b3c4d
uv run loglab-log export --import 1 -o copy.adi   # that import, with its own header
uv run loglab-log export -o log.adi               # the whole log, with a LogLab header
uv run loglab-operator sign-out
```

`--database` (or `$LOGLAB_DATABASE`) takes any libpq connection string; the
default is `dbname=loglab`.

## Operators and signing in

Every request is made by an identified operator at an identified station
(Principle XIII). Operators are local accounts in the Engine's database, so
signing in works with no internet connection (Principle V).

```sh
uv run loglab-operator add W0XYZ                  # administrators; default view,log
uv run loglab-operator add N0ABC --grant view,log,radio
uv run loglab-operator grant W0XYZ radio          # or revoke
uv run loglab-operator disable W0XYZ              # ends their sessions; enable undoes it
uv run loglab-operator set-pin                    # change your own PIN or passphrase
uv run loglab-operator list                       # everyone and what they may do
uv run loglab-operator whoami
```

- A secret is a PIN or passphrase of 6 to 128 characters, typed at a prompt.
  Only a salted scrypt hash is stored.
- Signing in keeps a session token in `~/.local/state/loglab/session`
  (or `$LOGLAB_SESSION_FILE`), readable only by you. Sessions last 24 hours.
  Only the token's SHA-256 is in the database.
- Five wrong secrets for a callsign within 15 minutes lock it for 15 minutes.
- Permissions are `view`, `log`, `radio`, and `admin`, each granted on its own.
  Transmit permission is per radio and arrives with the Station Agent.
- Account changes, sign-ins, and sign-outs are kept forever, with who made them
  and when (Principle VII). The database enforces all of this, so no code path
  can skip it.

Migrations are numbered SQL files in `src/loglab_engine/store/migrations`.
Each runs in its own transaction and is recorded with its SHA-256; if an
applied file is ever changed, every later run refuses to start. Never edit an
applied migration. Add a new one.
