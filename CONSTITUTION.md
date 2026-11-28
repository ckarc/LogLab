# LabLog Constitution

| Field          | Value                                                                                  |
| -------------- | -------------------------------------------------------------------------------------- |
| **Version**    | 6.1.0                                                                                  |
| **Status**     | `STABLE`                                                                               |
| **Maintainer** | Brian Krenzin, K9KSU                                                                   |
| **License**    | GNU Affero General Public License v3.0 (AGPL-3.0)                                      |
| **Home**       | github.com/ckarc/loglab (Central Kansas Amateur Radio Club)                            |
| **Applies to** | All contributions: engine, station agent, clients, contest templates, rig definitions, documentation, automation, releases |

## Purpose

This constitution records the principles every contribution to LabLog must
uphold, regardless of who or what produces it — a human, an AI coding
assistant, or an automated agent. These are not style preferences. Each one
protects a property the project depends on; violating any of them breaks that
property for every operator who relies on the log.

LabLog is an original, open-source amateur radio logging system built on an
API engine. It is not a clone, port, or reimplementation of any existing
logger. Its design is its own: one log, one engine, contests defined as
templates, radios owned by a station agent, and every user interface a client
of the same APIs.

The project is licensed under the AGPL-3.0. Anyone may use, study, modify, and
share it. Anyone who distributes it — or runs a modified version as a network
service — must make their complete source available under the same license.
The code stays open, wherever it goes.

## System Components

- **The Engine** — the server. It owns the log, contest templates, reference
  data, operator identities, and the event stream. It exposes the API every
  client uses. It never touches radio hardware.
- **The Station Agent** — runs at each operating position (or wherever the
  radios are). It owns the radios: serial, USB, HID, and network-native radios
  such as FlexRadio. It presents every radio through one common rig model,
  reports radio state to the Engine, and shares radios with other software.
- **Clients** — user interfaces. The default client is a web application
  served by the Engine. Any other client is equally valid as long as it uses
  only the published APIs.

---

## Core Principles

### I. The API Is The Product

Every capability exists first as an API. Logging a QSO, editing it, checking a
dupe, scoring a contest, looking up a callsign, exporting a log — each is an
Engine API operation before it is a button. Tuning a radio, reading its meters,
or sending a CW macro is a Station Agent API operation before it is a button.
User interfaces are clients of those APIs and have no private path into the
Engine, the Agent, or the database.

A feature that only works through one particular UI is incomplete. A UI that
reaches around an API to touch the database or the hardware directly is a
defect.

*Why this is inviolable: the moment one client gets a private path, the API
stops being the whole truth about what the system can do. Other clients fall
behind, automation can't reach the feature, and the system can no longer
guarantee its own rules (dupes, validation, audit, authorization, transmit
safety) because something is working around them.*

### II. The Engine Is Authoritative For The Log

The Engine holds the log; clients mirror it. A client action is a *request* —
the Engine validates it, applies it, and announces the result on the event
stream. Clients render what the Engine reports, not what they assumed would
happen. Where a client updates optimistically for responsiveness, the Engine's
event supersedes the optimistic value if they differ.

Requests flow client → Engine. Truth flows Engine → clients. The two never form
a loop.

*Why this is inviolable: in a multi-station event, every operating position
must agree on the log — what's a dupe, what the score is, who worked what. A
client that trusts its own copy over the Engine's drifts out of agreement with
every other position, and nobody notices until the dupe sheet is wrong.*

### III. One Log, Many Templates

There is one log and one Engine. Contests and special events are templates —
declarative definition files that describe exchange fields and validation,
dupe rules, scoring and multipliers, Cabrillo mapping, and entry-screen layout.
A contest is never a separate program, a fork, or a branch of Engine code.

Adding a contest means adding a template file. Adding a contest never requires
a database schema change. Where a template genuinely needs logic a declaration
cannot express (an unusual scoring rule), that logic goes through a single,
documented, sandboxed extension point — never a special case in the Engine.

*Why this is inviolable: separate programs per contest is the design this
project exists to replace. Every special case in the Engine is a step back
toward it, and every schema change for one contest is a migration every
operator pays for.*

### IV. Open Standards At The Edges

The Engine's QSO field model is ADIF. Import and export are lossless: an ADIF
file round-trips through the Engine without dropping or altering fields,
including fields the Engine does not use. Contest submissions export as
Cabrillo. Interoperability with other software happens through open,
documented formats and protocols — ADIF, Cabrillo, the WSJT-X UDP protocol,
the Hamlib `rigctld` network protocol, TCI — not by imitating another product.

A radio maker's published command set is an open protocol. The Station Agent
may offer a virtual radio that speaks one (a Kenwood command set, for
example), so programs that only know how to talk to radios can share a radio
through the Agent. It is built only from the maker's published reference, and
the documentation says which command set it speaks. This permits speaking a
published command set; it never permits copying another program's code, look,
or workflow.

*Why this is inviolable: an operator's log is decades of their history. A
logger that loses fields on import, or that can only exchange data with
itself, holds that history hostage. Open formats at the edges are what let an
operator leave — which is what lets them trust the project enough to stay.*

### V. Offline First

Every core function works with no internet connection: logging, editing, dupe
checking, scoring, multi-station sync on a local network, radio control, US
callsign lookup, operator sign-in, and export. Internet services — online
callbooks, LoTW, cluster spots, sync to a hosted copy — are enhancements. When
they are unavailable, the system degrades gracefully and says so; it never
blocks logging.

Network use on metered or satellite links is deliberate and minimal: fetch on
events rather than polling, cache every result, and fetch each thing once.

*Why this is inviolable: the places this logger matters most — Field Day,
emergency communications, portable and trailer stations — are exactly the
places the internet is slow, metered, or gone. A logger that stops logging
when the link drops has failed at the one job it has.*

### VI. The Engine Owns Its Reference Data

Reference data the Engine depends on — the FCC amateur license database, DXCC
and prefix tables, contest section lists — is stored and served by the Engine
itself, with its source and refresh date recorded. Online lookups supplement
the local data; they never replace it. Results fetched from outside services
are cached so the same call is never fetched twice.

Reference data is kept separate from the log, so a reference refresh can never
touch, lock, or corrupt a QSO record.

*Why this is inviolable: if callsign lookup depends on a third-party service,
the logger's speed, availability, and cost depend on that service too. Owning
the data keeps lookup fast, offline-capable, and free — and keeping it apart
from the log means a bad refresh is an inconvenience, not data loss.*

### VII. A QSO Is Never Silently Lost Or Altered

Every change to a QSO record is versioned: which operator changed it, when,
from which station, and what it was before. Deletion is a recorded action, not
an erasure, and a deleted QSO can be recovered. No import, migration, template
change, or sync operation may modify existing QSOs except through this same
audited path.

A pretend QSO is the one exception. A pretend QSO is marked as made with a
pretend station (`APP_LOGLAB_PRETEND`), is accepted only in a practice
operation, and is never sent to any outside service. It is not a contact, so
a signed-in administrator may erase the pretend QSOs of a practice operation
entirely, history and all, as when demo mode is turned off. The erasure
itself is recorded: who did it, when, in which operation, and how many QSOs it
removed. A QSO with any version that is not pretend is never erased.

*Why this is inviolable: a contact can't be re-made. Contest results, awards,
and QSL confirmations all rest on the log being exactly what the operators
entered. An unrecorded change — from a bug, a bad import, or a sync conflict —
is indistinguishable from the truth until it's too late to fix.*

### VIII. UTC Is The Only Clock

Every timestamp the system stores or emits is UTC. Local time exists only in
client display, and only when the operator asks for it. The Engine records its
time source and refuses to present a station's clock as trustworthy when it
cannot verify it.

*Why this is inviolable: contest logs are cross-checked against other stations'
logs by time. A QSO stamped in local time, or by a drifting clock, fails those
checks — and in a multi-station event, one bad clock corrupts the order of the
whole log.*

### IX. One Owner Per Radio

Each radio has exactly one owner: the Station Agent. Only the Agent opens a
radio's serial port, USB or HID device, or network control connection. The
Engine, the clients, and third-party programs never talk to radio hardware
directly — they are clients of the Agent.

The Agent presents every radio through one common rig model (receivers or
slices, frequency, mode, filter, PTT, power, meters, antenna), so nothing above
it needs to know what make or model the radio is. The rig model records the
radio's own identity separately from the path used to reach it, so every QSO
names the actual equipment that made it, whether control arrived over a serial
port, a vendor API, or an intermediary application. Command-driven radios are
described by declarative rig definition files; adding a radio means adding a
definition, not changing Agent code. Network-native radios whose control
protocols are too rich for a command table (FlexRadio, for example) get a
dedicated driver behind the same rig model. The Agent shares each radio with
other software through open protocols (Principle IV), so a logger, WSJT-X,
and fldigi can all use one radio at once.

*Why this is inviolable: two programs fighting over one radio port is the
oldest problem in station software — "port in use," commands interleaving,
each program undoing the other's changes. One owner removes the fight. And
because only the Agent touches hardware, every command that reaches a radio
passes through one place.*

### X. The Radio Is Authoritative On Live State

The radio holds its live state; the Agent mirrors it. Whenever the Agent's
model and the radio's reported state disagree about what is live — a
frequency, a mode, a filter, the selected antenna — the radio wins and the
Agent reconciles to it. A command to the radio is a *request*; the radio's
reported state afterward is the truth. The Agent never writes a remembered
value back over the radio's current one.

If the radio can store and recall a setting itself, the radio keeps it. The
Agent persists only what the radio cannot store.

Three exceptions. First, the Agent may record a radio's settings when it first
connects, and an operator may explicitly ask the Agent to put recorded
settings back; the Agent then restores only after the operator confirms it.
Second, when a program starts using a radio's audio through the Agent, the
Agent may put back, without asking, the setup kept for that radio and that
program. It does so once, as the program starts, and only for a radio that
already has a setup kept for that program, so a radio nobody has set up for
it, such as a visitor's, is never changed this way. While the program runs,
the operator's changes on the radio stand. Third, an operator may turn on
auto mode on a station computer. While it is on, the Agent may make, without
asking, the changes it would otherwise offer for the operator's yes: setting
what stops a job working, such as the audio source a data mode transmits
from, and lowering a transmit drive level that the radio's own meters show
is too high. It changes nothing while a program has the radio keyed, and it
never keys a radio to try a change: it adjusts only between transmissions a
program asked for. Auto mode is off until an operator turns it on, it applies
only on the computer where it was turned on, and while it is off the Agent
asks first. Every change of these kinds is shown to the operator and recorded
— what was changed, from what, and when — like any other change the Agent
makes to a radio. Apart from these three exceptions, the Agent never changes
a radio's settings on its own.

Where a radio can measure a value the log records — transmit power, most
importantly — the measured value is logged and marked as measured. A value an
operator typed or a default filled in is marked as entered. The log never
presents an entered value as a measured one.

Commands flow Agent → radio. Truth flows radio → Agent → Engine. The two never
form a loop.

*Why this is inviolable: when software trusts its own copy of the radio's state
over what the radio reports, the two fight — values flicker, the operator's
settings jump back to stale ones on reconnect, and QSOs get stamped with a
frequency the radio wasn't actually on. The log is only as accurate as the
radio state behind it.*

### XI. Repealed

Repealed in version 3.0.0. LabLog is a logging application; transmitting is
done by the programs the operator runs. The number is kept so that later
principles keep their numbers.

### XII. Untrusted Input Is Validated At The Boundary

The system consumes many external inputs: API requests, ADIF and Cabrillo
files, WSJT-X UDP datagrams, TCI messages, contest templates, rig definitions,
reference-data downloads, online lookup responses, and the byte streams radios
send back to the Agent. None is trusted to be well-formed. Every parser
bounds-checks lengths, caps allocations, validates ranges and encodings, and
fails closed on malformed input. Validation happens once, where the data
enters, so the interior can treat parsed values as sound.

Contest templates and rig definitions are untrusted input. They are data,
never executable code, except through the sandboxed extension point of
Principle III.

*Why this is inviolable: the Engine and the Agent listen on the network and
accept files and hardware data from anywhere. One oversized field or malformed
packet that a parser trusts becomes a crash in the middle of a contest — or a
way in.*

### XIII. Repealed

Repealed in version 3.0.0. Which programs may control or key a radio is decided
at the computer the radio is connected to, not by LabLog. The number is kept so
that later principles keep their numbers.

### XIV. Persist Atomically

No reader ever observes a partially written state. Database changes that belong
together commit in one transaction. Reference-data refreshes load into a
staging area and swap in atomically. Configuration, template, and rig
definition files are written in full and renamed into place, never truncated
and rewritten.

*Why this is inviolable: "delete the old, then write the new" with a crash or
power loss between the steps leaves every reader with nothing — and field
stations lose power. Write-then-swap makes that window disappear.*

### XV. A Logged QSO Is Durably Stored And Recoverable

A QSO counts as logged once the Engine has committed it to durable storage
(Principle XIV). Logging never waits on a second machine: a tablet, a
workstation that is switched off, or a missing network link never holds a QSO
back from being logged.

Every client keeps a local journal of each QSO it submits until the Engine
confirms the QSO is logged, and the client shows the operator which entries are
still unconfirmed, so a QSO entered while the Engine is out of reach is sent
when it is back. Every QSO carries a unique identifier assigned when it is
first entered, so replaying a journal or restoring a backup never creates a
duplicate.

The Engine can keep further copies of the log — on another machine at the
station, or off site — and recovery from them is a documented, tested
procedure. Further copies protect the log; they are never a condition for
logging. Like everything else, this works with no internet connection
(Principle V).

*Why this is inviolable: a logger that will not call a contact logged because
a tablet or a switched-off workstation has not answered fails operators at the
moment they need it. But field stations run on generators, folding tables, and
laptops people trip over, and a contact, once lost, can't be re-made — so the
log is committed durably before it is called logged, nothing an operator
entered is dropped on the way, and the log can be rebuilt when hardware dies.*

### XVI. Original Work, Clean Sources

Every contribution is original or comes from clean sources: public
specifications, published vendor documentation and source code, open-source
code under a license compatible with the AGPL-3.0 (credited and recorded in
the third-party licenses file), behavior observed on the wire, and the
contributor's own design. Code, protocol knowledge, or interface designs
obtained by decompiling, disassembling, or reverse-engineering proprietary
software never enter the project — nor does anything transcribed, translated,
or paraphrased from such sources.

The project does not reproduce another logger's look, workflow, or trade dress.
It solves the same problems in its own way.

*Why this is inviolable: contaminated code carries someone else's copyright
into this project's license, and the contamination spreads to everything
written from it. The only remedy is to tear it all out. It is trivial to refuse
at the door and ruinous to undo afterward.*

---

> **Principles XVII–XIX govern how changes are made.** This project is written
> primarily by AI coding assistants under the direction of the maintainer, who
> manages outcomes rather than reviewing every line. These principles apply to
> every contributor, human or AI, and they are enforced by the repository's
> infrastructure, not by trust.

### XVII. Evidence Over Assertion

A change is verified by the compiler, the type checker, tests, and observed
behavior — not by its author's confidence. No contributor, human or AI, may
merge or recommend merging a change on the strength of its description. A claim
whose verification step was not actually run is a hypothesis, however
confident it reads.

Changes to dupe checking, scoring, ADIF handling, persistence, authorization,
rig control, or any transmit path require tests that exercise the specific
behavior changed. Every feature is delivered with a demonstration the
maintainer can run and observe.

*Why this is inviolable: AI tools produce fluent, plausible, confident
descriptions of fixes that are wrong at a rate that makes unverified changes
dangerous. When the maintainer is not reading every line, the gates are the
review. Only a passing build and an observed result are evidence.*

### XVIII. Sandbox By Infrastructure, Not By Prompt

Contributor and agent permissions are enforced by infrastructure — branch
protection, required status checks, signed commits — never by instructions in
a prompt alone. AI agents read untrusted content as part of
their work (issues, pull requests, log files, ADIF files), and any of it may
contain instructions. The enforcement layer must be somewhere an agent cannot
be talked out of.

*Why this is inviolable: an agent that can be instructed past a rule doesn't
have a rule. A gate it cannot argue with does.*

### XIX. The Maintainer Outranks Every Agent

The maintainer's direct decisions — accepting an issue, approving a change,
merging, stopping or reverting a change — are authoritative. Suggestions from
AI tools, notes left by earlier agent sessions, and agreement among several AI
tools are hints, not authority.
An agent does not treat another agent's "this is done" as fact, and does not
treat its own conclusion as approval.

*Why this is inviolable: agents talk each other into believing work is finished.
The cycle breaks only when human intent ranks above agent consensus, every
time.*

### XX. Received Is Never Confused With Inferred

LabLog keeps a hard line between what a station was heard to send and what it
probably sent. Every decode, from any mode or decoder, is one of three kinds,
and it carries its kind wherever it goes: on screen, in the event stream, in
the log, and in exports.

- **Received**: the whole message came from the signal and passed the mode's
  own integrity checks (for FT8, every parity check and the CRC). Nothing about
  its content was assumed.
- **Assisted**: part of the message was assumed rather than received (an
  expected callsign, a CQ, the next step of a QSO in progress, or the same
  message combined across repeats), and the rest came from the signal and
  passed the integrity checks. An assisted decode records what was assumed and
  is always shown as assisted, never as received.
- **Predicted**: anything that did not pass the integrity checks, or that was
  filled in from expectation, history, or reference data. A prediction may be
  offered to the operator as a suggestion, visibly distinct from a decode. It is
  never shown, stored, or exported as something the station transmitted.

A decode is never silently upgraded from one kind to another. A QSO whose
details rest on assisted decodes records that, so the log never presents
inference as reception. Decoder benchmarks report received and assisted
results separately, and any claim that LabLog decodes better than another
program states which kind it counts.

*Why this is inviolable: a log is a record of what was actually heard. Once a
guess about what a station probably sent is mixed in with what it did send,
nobody can tell them apart afterward: not the operator, not a contest
adjudicator, not a confirmation service. Decoding more weak signals is only
worth something if every decode can be trusted to be what it says it is.*

### XXI. The Log Knows Who Is Asking

Every request to the Engine is made by an identified operator at an identified
station. Anonymous requests are refused, except the small set needed to sign
in and to discover the system on the local network.

Permissions are explicit and separated by kind:

- **Viewing** the log and the event stream.
- **Logging**: entering and editing QSOs.
- **Administration**: managing operators, stations, templates, and reference
  data.

Being able to log never by itself grants administration. An operator's own
credentials (an online callbook login, a LoTW certificate, a logbook key)
belong to that operator: only they, signed in, can use, change, or remove
them.

Identity works offline (Principle V). Operators are local accounts held by the
Engine; signing in needs no internet connection and no outside identity
provider, and is quick enough for a visiting operator to join a Field Day
position in under a minute.

A Station Agent proves which station it is with a key an administrator
approved. Protocols that carry no sign-in of their own (the WSJT-X UDP
protocol and the Hamlib `rigctld` protocol, for example) are accepted only
from the computer they run on, unless an administrator explicitly opens one to
named hosts.

This principle is about the log. It grants nothing about controlling a radio:
which program controls a radio is decided at the computer the radio is
connected to.

*Why this is inviolable: the log promises to record who did what (Principle
VII), and that promise means nothing if the system can't tell who is asking.
At Field Day the network is full of laptops the station owner doesn't
control; a visitor must be able to log in a minute, and must never be able to
read another operator's credentials, change the club's templates, or edit the
log without signing in.*

---

## Technology Constraints

### Engine

- **Language**: Python, with strict static typing. Every function is fully
  type-annotated, and the strict type checker runs in CI as a merge gate — a
  type error fails the build.
- **API inputs**: every request body and message is parsed into a typed,
  validated model at the boundary (Principle XII). Unvalidated dictionaries do
  not travel through the Engine.
- **Database**: PostgreSQL. The log and the reference data live in separate
  schemas.
- **Platform**: Linux.
- **Deployment**: the Engine must run fully on a single small Linux machine on
  a local network with no internet connection (Principle V).

### Station Agent

- **Language**: Rust. The compiler's guarantees — memory safety, no data races,
  every error handled — are the Agent's first line of review.
- **Distribution**: a single self-contained executable for each supported
  platform (Windows, macOS, Linux on x86-64 and ARM). No runtime, interpreter,
  or separate install required.
- **`unsafe` code**: forbidden except where a hardware or platform interface
  requires it, and then isolated, documented, and justified in the pull
  request.
- **Rig sharing**: the Agent speaks the Hamlib `rigctld` network protocol so
  third-party programs can share radios through it (Principle IX).
- **Hardware drivers**: rig definition files for command-driven radios;
  dedicated drivers for network-native radios, built from vendor-published
  documentation and source (Principle XVI). A TCI driver reaches SDR
  applications that serve TCI.

### Clients

- **Default client**: a web application served by the Engine, installable as a
  progressive web app, usable on any device on the local network with no
  internet connection.
- **Other clients**: any platform, any language, reaching the Engine and Agent
  only through their published APIs (Principle I).

### Licensing

- **Project license**: AGPL-3.0. Every source file carries the license notice.
- **Third-party code**: only under licenses compatible with the AGPL-3.0 —
  for example GPL-3.0, LGPL-3.0, Apache-2.0, MIT, and BSD. Code under
  GPL-2.0-only, non-commercial, or proprietary licenses never enters the
  project. Every third-party component is recorded in the third-party licenses
  file.
- **Network use**: anyone running a modified version of the Engine or Agent as
  a network service must offer its users the corresponding source, as the
  AGPL-3.0 requires. The project's own clients make the source location
  visible to their users.

## Development Workflow

- **Branch protection on `main`**: no direct pushes. Every change arrives by
  pull request.
- **Merge gates** — all required, none optional:
  - Engine: strict type check, linting, and tests pass.
  - Agent: builds with no compiler warnings, linting passes, and tests pass on
    every supported platform.
  - License check: no third-party dependency with an incompatible license.
- **Automatic merge**: a pull request merges itself once every required gate
  passes. The gates are the approval; no per-change maintainer approval is
  required. The maintainer may stop any pull request before it merges, revert
  any merged change, and turn automatic merging off at any time.
- **Governance changes**: a pull request that changes this constitution,
  `CLAUDE.md` / `AGENTS.md`, the CI workflows, or the license allowlist merges
  automatically like any other once every required gate passes. Such a change
  never removes or weakens a gate, and an amendment to this constitution still
  records the maintainer's decision in its Change History row (Amendment).
- **Signed commits**: required on `main`.
- **Demonstrations**: every feature pull request states how the maintainer can
  see it work (Principle XVII).
- **AI-assisted work**: AI tools read `CLAUDE.md` / `AGENTS.md` first. Those
  files carry working conventions and must point to this constitution; they
  never contradict it.
- **Principle citations**: when a change is principle-relevant, its merge
  commit message cites the principle by Roman numeral (`Principle VII.`).

---

## Governance

### Amendment

A principle may be amended or removed only by:

1. Documenting the specific situation in which the principle, as written,
   produces a worse outcome than violating it; and
2. Recording the amendment in this file with a version bump, date, and
   rationale.

"It is inconvenient" is not grounds for amendment. Amendments arrive by pull
request and are reviewed like any other change. When an amendment would change
the expected behavior of work in progress, that work pauses until the
amendment is decided.

### Precedence

Where this constitution conflicts with any other document, plan, template, rig
definition, or generated artifact, this constitution wins and the other
artifact is in error.

### Scope Of Authority

This constitution constrains the project's design and the behavior of
contributors and automated agents. It does not constrain the maintainer's
runtime decisions: the maintainer may override an automated verdict or disable
a workflow. The override is recorded; it is not refused.

### Versioning

This file is versioned independently of the software. Before version 1.0.0,
the constitution is a draft and changes bump the minor version. From 1.0.0 on:

- **MAJOR** — a principle is added or removed, or its direction is inverted (a
  "never" becomes a "may").
- **MINOR** — a principle's scope is widened or narrowed without inverting it,
  or a governance section is added or removed.
- **PATCH** — wording and formatting fixes that change nothing a principle
  requires.

### Compliance Review

- **On every pull request**: the reviewer checks principle-relevant changes
  against this file.
- **On every minor release**: the maintainer reviews whether the principles
  still describe the shipping reality, and amends them if not.
- **When an AI agent finds that a requested change would violate a
  principle**: it stops and asks for a decision or an amendment rather than
  working around the principle.

### Change History

| Version | Date       | Change |
| ------- | ---------- | ------ |
| 0.1.0   | 2026-10-17 | Initial draft: fifteen principles. |
| 0.2.0   | 2026-10-17 | Added the Station Agent as a system component. Added Principle IX (One Owner Per Radio) and Principle X (The Radio Is Authoritative On Live State); renumbered later principles. Set languages: strictly typed Python for the Engine, Rust for the Agent. Made type checking, warning-free builds, tests, and demonstrations merge gates. |
| 0.3.0   | 2026-10-17 | Adopted the AGPL-3.0 as the project license, with a Licensing section and a license-check merge gate. Added Principle XIII (Every Action Has An Identified, Authorized Operator); renumbered later principles. Rig model now records radio identity separately from the connection path. Measured values are logged as measured, entered values as entered. Added TCI to the open protocols. |
| 0.4.0   | 2026-10-17 | Added Principle XV (The Log Survives The Loss Of Any One Machine): two independent durable copies before a QSO counts as logged, client journals, continuous replication, unique QSO identifiers, tested recovery. Renumbered later principles. |
| 0.5.0   | 2026-10-17 | Set the project's home: the `ckarc` GitHub organization. |
| 0.6.0   | 2026-10-17 | Named the project LogLab, at github.com/ckarc/loglab. All open items closed. |
| 1.0.0   | 2026-10-17 | Ratified by the maintainer, Brian Krenzin, K9KSU. Status `STABLE`. No principle changed from 0.6.0. The post-1.0.0 versioning rules now apply. |
| 1.1.0   | 2026-10-18 | Dropped per-change maintainer approval as a merge gate: pull requests merge automatically once every required gate passes. Governance changes are still merged by the maintainer. Principle XVIII no longer lists required review; Principle XIX names stopping and reverting as maintainer decisions. Rationale: the maintainer manages outcomes and does not read code, so a per-change approval added delay without adding review; the gates are the review (Principle XVII). |
| 1.2.0   | 2026-10-18 | Narrowed Principle XV: a QSO counts as logged once the Engine has committed it to durable storage; a second independent copy is no longer a condition for logging. Client journals, unique QSO identifiers, further copies, and tested recovery remain. Situation: the Field Lab station runs with no internet, and its operating positions include tablets and workstations that are not always on; requiring a second copy before a QSO counts as logged would leave every QSO unconfirmed whenever the second machine is a tablet, switched off, or out of reach, a worse outcome than logging on the Engine's durable commit. Decided by the maintainer. |
| 2.0.0   | 2026-10-19 | Added Principle XX (Received Is Never Confused With Inferred): every decode is received, assisted, or predicted, carries that kind everywhere, and is never silently upgraded; benchmarks report received and assisted results separately. Added at the end so no principle is renumbered. Situation: LogLab's own FT8 decoder will reach weaker signals with help such as expected messages and combined repeats; without this rule, what a station probably sent could be logged and shown as what it did send, and the log could no longer be trusted as a record of what was heard. Requested by the maintainer. |
| 2.0.1   | 2026-10-19 | Renamed the software LabLog wherever people read its name. The repository, code and database keep the name loglab. Wording only; no principle changed. Decided by the maintainer. |
| 2.1.0   | 2026-10-23 | Narrowed Principle VII: an administrator may erase the pretend QSOs of a practice operation entirely, and the erasure itself is recorded; every other QSO keeps its full history. Situation: demo mode fills the boxes with pretend QSOs, using real calls marked pretend, to show LabLog to people. Keeping every one of them forever as deleted versions would fill the log's history with contacts that never happened, mixed in with the real ones, which is a worse outcome than erasing what was never a contact. Decided by the maintainer. |
| 3.0.0   | 2026-10-24 | Repealed Principle XI (The Operator Keys The Transmitter) and Principle XIII (Every Action Has An Identified, Authorized Operator); their numbers are kept so no principle is renumbered. Principle VII now names the operator who made a change directly instead of citing XIII; Principle IX no longer cites XI. Narrowed Principle X: the Agent may record a radio's settings on first connect and restore them on an operator's explicit, confirmed request, recorded like any other change. Widened Principle IV: a radio maker's published command set is an open protocol the Agent may speak through a virtual radio. Situation: LabLog is now purely a logging application with no built-in operating modes. Transmitting is done by programs such as WSJT-X, VarAC and Winlink Express, which reach the radio through the Station Agent; the person at the computer chooses in the Agent which program controls each radio. Requiring LabLog operator accounts to authorize transmitting would put a logger between operators and their own stations, and would leave those programs unable to key a radio at a workstation where nobody has signed in to LabLog, a worse outcome than leaving that choice at the computer. Restoring recorded settings lets a visitor's radio go home set up the way it arrived, which Principle X as written forbade. Programs such as Winlink Express and N1MM Logger+ can only reach a radio through a maker's command set, so without the Principle IV clause they could not share a radio through the Agent at all. Decided by the maintainer. |
| 4.0.0   | 2026-10-24 | Added Principle XXI (The Log Knows Who Is Asking): every Engine request is made by an identified operator at an identified station; viewing, logging and administration are separate permissions; an operator's own credentials are theirs alone; sign-in works offline; a Station Agent proves its station with an approved key; protocols with no sign-in are accepted only from their own computer. It grants nothing about controlling a radio. Added at the end so no principle is renumbered. Situation: repealing Principle XIII in 3.0.0 removed the radio permissions it held, as intended, but also left the Engine's sign-in, its separate permissions, and each operator's private credentials with no principle behind them; without one, a change could let anyone on a Field Day network read or edit the log, or use another operator's LoTW certificate, with no rule to stop it. Decided by the maintainer. |
| 5.0.0   | 2026-10-27 | Widened Principle X: when a program starts using a radio's audio through the Agent, the Agent may put back the setup kept for that radio and that program without asking, once, as the program starts, and only for a radio that already has a setup kept for that program; every restore is shown and recorded. Restoring on an operator's explicit, confirmed request is unchanged. Situation: FieldLab's radios move between computers and between modes (FT8, VARA, SSTV for youth operators), and each move costs about 30 minutes of resetting filters, noise reduction, RF gain and audio levels by hand. Asking for a confirm every time a program starts turns a radio that should simply work into a step operators skip or get wrong, a worse outcome than applying a setup the station already chose for that radio and program. Limiting it to radios with a kept setup still leaves a visitor's radio untouched, the reason the confirm was required. Decided by the maintainer. |
| 6.0.0   | 2026-10-28 | Widened Principle X with a third exception, auto mode: an operator may turn it on for a station computer, and while it is on the Agent makes, without asking, the changes it would otherwise offer for a yes (settings that stop a job working, and lowering a transmit drive level the radio's meters show is too high). Never while a program has the radio keyed, never by keying the radio itself, only between transmissions a program asked for; off until turned on, per computer; every change shown and recorded. Major, following 5.0.0, because a case where the Agent asked first becomes one where it may act. Situation: at FieldLab's first VARA contact on 2026-10-28 the Agent measured ALC at 70 to 92 percent of its scale, where data modes want little or none; bringing it down meant a confirm after every test transmission, and a youth operator at an unattended position would leave a radio overdriving rather than answer each one, a worse outcome than letting the Agent make a measured, recorded change the station chose to allow. Changes made to a visitor's radio stay recorded against the settings it arrived with, so it can still be put back as found. Decided by the maintainer. |
| 6.1.0   | 2026-10-29 | Governance changes merge automatically: a pull request that changes this constitution, `CLAUDE.md` / `AGENTS.md`, the CI workflows, or the license allowlist now merges itself once every required gate passes, like any other change, instead of waiting for the maintainer to merge it. Such a change still never removes or weakens a gate, and an amendment still records the maintainer's decision. The maintainer may still stop, revert, or turn off automatic merging of any change. Minor, because a governance rule is removed and no principle changes. Situation: changes by AI coding assistants now arrive from several threads at once, and many of them must touch CI (for example, building and checking a new Agent component on every platform, as the MMSSTV transmit plugin in pull request 159 does). Each one then waited, green, for a manual merge, and a waiting change goes stale as others merge ahead of it, a worse outcome than letting the gates that already approve every other change approve these too. Decided by the maintainer. |

---

## Ratification

Ratified by the maintainer, Brian Krenzin, K9KSU, on 2026-10-17 as version
1.0.0, status `STABLE`. The post-1.0.0 versioning rules now apply: every
further change is an amendment under the Governance section above.
