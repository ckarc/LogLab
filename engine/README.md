# LabLog Engine

The Engine is LabLog's server: it owns the log, contest templates, reference
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
uv run loglab-log export -o log.adi               # the whole log, with a LabLog header
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

The Engine serves LabLog's HTTP API at `/api/v1` and a web page at `/` that
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
- The top bar holds only LabLog, who is signed in, and **Menu**. Menu opens
  a box with a few headings: **Operation**, **Appearance and layout**,
  **Connections** (QRZ.com, LoTW, the US and Canada call lists) and
  **Admin** (shown only to admins), with who is signed in and **Sign out**
  at the bottom. A heading opens to show its settings, one at a time; the
  one used last opens again next time, and a heading shows what is set under
  it (the operation, Auto/Light/Dark). New settings go under the heading
  they belong to, so the bar never fills up.
- Each box on the page is a module. The layout is locked normally; **Menu →
  Unlock to move boxes** unlocks it, and every box shows a handle bar
  (**Lock** in the bar above the boxes locks it again). Drag a box
  by its handle like a window: let go on the centre line (the middle fifth
  of the page) for full width, or left or right of it for half width on that
  side; how far down you let go sets its place. From the keyboard, focus a
  handle: up and down move it, left and right step between left half, full
  width and right half (L, C, R jump there). New QSO and Log can be hidden;
  "Not logged yet" can be moved but never hidden. On a phone every box is
  full width. The Engine keeps each operator's layout in its own
  `workspace` schema, apart from the log, so it follows them to any
  workstation or tablet (`GET`/`PUT /api/v1/layout`).
- The page is light or dark: **Menu → Appearance** picks **Auto** (follow
  the computer's own setting), **Light** or **Dark**. The choice is saved
  with the layout, per operator (`theme`).
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
  other field a QSO has is still shown below them. Fields are never added
  QSO by QSO: **Fields** is the one place that sets them. A field left empty
  is not saved. The choice is saved with the layout, per operator
  (`detail.fields`).
- **US and Canada call lists** fill in US and Canadian calls with no
  internet at all (Principles V and VI). The Engine keeps its own copy of the
  FCC's amateur license list (public domain) and ISED Canada's (Open
  Government Licence - Canada) in `reference.license`: only the call, name,
  city, state or province, license class, and for US calls the expiry date.
  No street address, email, or phone is kept, nor cancelled licenses or ones
  expired past the two-year renewal grace. While `serve` runs it checks every
  six hours, by HEAD request, whether the FCC's weekly `l_amat.zip`, its
  daily change files `l_am_<day>.zip`, or ISED's `amateur_delim.zip` are
  newer than what is loaded, and loads only those (`--no-license-refresh`
  turns this off). In FieldLab (`serve --mqtt`), nothing is fetched while the
  gateway says the internet is Starlink or the phone tether: the full US list
  is about 143 MB, and Starlink has a data budget. `licenses refresh` by hand
  still fetches on any link. Each load happens in one transaction, so lookups see the
  old list until the new one is complete, and a file that is mostly
  unreadable is refused (Principles XII and XIV). For a station that is never
  online, download the files elsewhere and load them by hand:

  ```sh
  uv run loglab-engine licenses import --fcc l_amat.zip --canada amateur_delim.zip
  uv run loglab-engine licenses import --fcc-daily l_am_mon.zip   # oldest first
  uv run loglab-engine licenses status
  uv run loglab-engine licenses refresh   # download whatever is newer, now
  ```

  A lookup answers from these lists first; QRZ.com, when set up, only fills
  in what they lack, such as the grid square, and answers for other
  countries' calls. Menu → **US and Canada call lists** shows how current
  each list is, with its credit (`GET /api/v1/licenses`); an admin can ask
  for a check now (`POST /api/v1/licenses/refresh`).
- **QRZ.com lookup** is optional. Each operator saves their own QRZ.com login
  from the **QRZ.com lookup** link on the New QSO form
  (`GET`/`PUT`/`DELETE /api/v1/callbook/qrz`; the password is never sent
  back). The username is the operator's callsign, not their email address.
  Saving tries the login at once and shows what QRZ.com said, whether it
  refused the login or accepted it with a notice (such as no XML
  subscription). Leaving the Call box then fills in Name, QTH, State, and Grid from
  QRZ.com, never over anything already typed (`GET /api/v1/lookup?call=W1AW`).
  Every answer is kept in the `reference` schema, apart from the log, so a
  call is fetched once (Principle VI); `refresh=true` fetches it again. With
  no login, or no internet, nothing is filled in and logging works exactly
  the same (Principle V); after QRZ.com cannot be reached the Engine leaves
  it alone for a minute. The Engine has to send the password to QRZ.com, so
  it is stored as typed: anyone who can read the database can read it.
- **Kept QRZ.com profiles stay current** on their own. Once an hour,
  `loglab-engine serve` looks up again (at most 60, three seconds apart)
  the calls whose biography or photo it keeps and whose entry is over 30
  days old. That is QRZ.com's small callsign answer; the biography and photo
  are downloaded again only when it shows a newer biography date or a
  different photo. It uses the earliest saved QRZ.com login that QRZ.com
  accepted, with a session of its own. By default it pauses while the
  internet comes through Starlink, told by the public address's network
  (SpaceX's AS14593, or a `starlinkisp.net` name, asked of ipinfo.io and
  api.ipify.org); an admin can set it to run on any internet, or never, in
  the **QRZ.com** window (`GET`/`PUT /api/v1/callbook/refresh`,
  `{"mode": "auto" | "always" | "off"}`). With no internet it tries nothing.
  In FieldLab, give `loglab-engine serve --mqtt HOST[:PORT]` (or set
  `$LOGLAB_MQTT`) to the kiosk's MQTT broker: the gateway's own word there
  (`fieldlab/facts/host`, from github.com/ckarc/fieldlab-kiosk) decides
  instead, and re-checking runs only on `wan1`, the shop link, never on
  Starlink or the phone tether. The Engine only reads the bus, with an MQTT
  client of its own; with no fresh word from it, the address check decides.
  The same reader, every 30 seconds, takes the trailer's GPS fix
  (`fieldlab/location`): while it is under an hour old it is the station's
  home on the world map and for the Station profile's distance and heading
  (`GET /api/v1/paths` gives `home.source` `"gps"`), and the box says
  "from GPS to their grid square".
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
  is that callsign are copied to the logbook in the background; an edit swaps
  the copy, deletes remove it, and a restore sends it again. Logging never
  waits on this (Principle V): with no internet, QSOs wait and go out once
  QRZ.com can be reached, retried every five minutes and whenever a QSO
  changes. Each callsign has one key, so nothing is sent twice. What was sent
  is kept in the `workspace` schema, apart from the log. LabLog only ever
  removes its own copies, by the id QRZ.com gave them, and never asks QRZ.com
  to replace a QSO, so QSOs QRZ.com got another way (from LoTW, say) are
  never changed or removed.
- The server listens only on this computer by default. `--host` and `--port`
  change that; there is no HTTPS yet, so do not open it to the internet.

Migrations are numbered SQL files in `src/loglab_engine/store/migrations`.
Each runs in its own transaction and is recorded with its SHA-256; if an
applied file is ever changed, every later run refuses to start. Never edit an
applied migration. Add a new one.
