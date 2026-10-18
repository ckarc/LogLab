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
(Principle VII). A QSO is *logged* once its transaction commits (Principle XV).

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

## The web page and the API

```sh
uv run loglab-engine serve                        # http://127.0.0.1:8073
```

The Engine serves LogLab's HTTP API at `/api/v1` and a web page at `/` that
uses only that API (Principle I). Open the page, sign in with your callsign,
PIN, and station name, and you can enter QSOs, search the log, edit a QSO
(each save is a new version), delete and restore one, and read its full
history. The page loads nothing from the internet (Principle V). The API's
own reference is at `/api/docs`.

- Every request except `/api/v1/status` needs a signed-in operator; reads need
  `view` and changes need `log`. Every request body is checked against a
  strict model before it reaches the log (Principle XII).
- Times go in and come out in UTC (Principle VIII).
- A QSO is logged as soon as the Engine commits it (Principle XV). The page
  gives each QSO its unique ID when you store it and keeps it in the
  browser's journal until the Engine has logged it. If the Engine cannot be
  reached, the QSO waits under "Not logged yet" and is sent again every 15
  seconds, or when you press **Send now** or sign back in. Sending it again
  never logs it twice: the Engine recognises the ID. A QSO that is logged
  simply appears in the Log; the page shows a notice only when something
  needs your attention.
- A QSO waiting in the journal is sent only by the operator who typed it, at
  the same station (Principle XIII).
- Each box on the page is a module. The lock in the top bar is **Locked**
  normally; press it to unlock, and every box shows a handle bar. Drag a box
  by its handle like a window: let go on the centre line (the middle fifth
  of the page) for full width, or left or right of it for half width on that
  side; how far down you let go sets its place. From the keyboard, focus a
  handle: up and down move it, left and right step between left half, full
  width and right half (L, C, R jump there). New QSO and Log can be hidden;
  "Not logged yet" can be moved but never hidden. On a phone every box is
  full width. The Engine keeps each operator's layout in its own
  `workspace` schema, apart from the log, so it follows them to any
  workstation or tablet (`GET`/`PUT /api/v1/layout`).
- Every table can be shaped the same way, and its shape is saved with the
  layout. **Columns** picks which columns show (any ADIF field can be added
  by name) and their order; a heading can also be dragged to move it.
  Clicking a heading sorts by it, a second click reverses, and a third goes
  back to the order entered. The Engine does the sorting, over the whole log
  (`GET /api/v1/qsos?sort=call`, `sort=band` by frequency, or
  `sort=field:FREQ`; `newest_first` sets the direction).
- An open QSO shows each field by its plain name with its ADIF name beside
  it. **Fields** picks the fields every QSO always shows, in order, even when
  empty; it searches all the ADIF 3.1 QSO fields by either name, grouped
  (contact, station worked, my station, contest, QSL and awards,
  propagation), and an APP_ or user-defined field can be named exactly. Any
  other field a QSO has is still shown below them. **Add field** searches the
  same list. A field left empty is not saved. The choice is saved with the
  layout, per operator (`detail.fields`).
- **QRZ.com lookup** is optional. Each operator saves their own QRZ.com login
  from the **QRZ.com lookup** link on the New QSO form
  (`GET`/`PUT`/`DELETE /api/v1/callbook/qrz`; the password is never sent
  back). Leaving the Call box then fills in Name, QTH, State, and Grid from
  QRZ.com, never over anything already typed (`GET /api/v1/lookup?call=W1AW`).
  Every answer is kept in the `reference` schema, apart from the log, so a
  call is fetched once (Principle VI); `refresh=true` fetches it again. With
  no login, or no internet, nothing is filled in and logging works exactly
  the same (Principle V); after QRZ.com cannot be reached the Engine leaves
  it alone for a minute. The Engine has to send the password to QRZ.com, so
  it is stored as typed: anyone who can read the database can read it.
- **Logbook of the World** is optional. The **LoTW** link in the Log box
  says how many QSOs wait to be sent and opens the set-up: each operator
  saves their own callsign certificate (the `.p12` file TrustedQSL saves,
  with its password), their station location (grid, zones, and a state and
  county or province from LoTW's own lists), and their LoTW website login
  (`GET`/`DELETE /api/v1/lotw`, `PUT /api/v1/lotw/certificate`, `/location`,
  `/login`; no password or key is ever sent back). **Send to LoTW**
  (`POST /api/v1/lotw/upload`) signs every waiting QSO made as the
  certificate's call (its `STATION_CALLSIGN`, else `OPERATOR`, else whoever
  entered it) and sends them; the Engine signs the `.tq8` file itself, as
  TrustedQSL's published source does, so TrustedQSL need not be installed.
  QSOs LoTW cannot take (an unknown band, a date the certificate does not
  cover, `MY_` fields that name another place) stay waiting, each with the
  reason. **Fetch confirmations** (`POST /api/v1/lotw/download`) asks LoTW
  for QSLs recorded since the last fetch and matches each to a QSO by call,
  band and mode within 30 minutes. What happened is written to each QSO's
  own ADIF fields (`LOTW_QSL_SENT`, `LOTW_QSLSDATE`, `LOTW_QSL_RCVD`,
  `LOTW_QSLRDATE`) as a new audited version by the operator who asked
  (Principles IV and VII); the Log's **LoTW** column shows Sent or
  Confirmed. Logging never waits for LoTW: with no internet a send or fetch
  changes nothing and says so (Principle V). The certificate and its
  password are stored so sending needs no typing: anyone who can read the
  database can sign QSOs with it.
- **QRZ logbook** sending is optional. An operator saves their QRZ logbook's
  API key (and, for a club logbook, its callsign) from the **QRZ.com** link
  (`GET`/`PUT`/`DELETE /api/v1/logbook/qrz`; the key is never sent back).
  QSOs entered from then on whose STATION_CALLSIGN, or else whose operator,
  is that callsign are copied to the logbook in the background; edits replace
  the copy, deletes remove it, and a restore sends it again. Logging never
  waits on this (Principle V): with no internet, QSOs wait and go out once
  QRZ.com can be reached, retried every five minutes and whenever a QSO
  changes. Each callsign has one key, so nothing is sent twice. What was sent
  is kept in the `workspace` schema, apart from the log.
- The server listens only on this computer by default. `--host` and `--port`
  change that; there is no HTTPS yet, so do not open it to the internet.

Migrations are numbered SQL files in `src/loglab_engine/store/migrations`.
Each runs in its own transaction and is recorded with its SHA-256; if an
applied file is ever changed, every later run refuses to start. Never edit an
applied migration. Add a new one.
