<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Station Agent: USB Radios and Virtual Radios

**Status: design draft.** Nothing here is built yet. The decisions marked
**Decision needed** are the maintainer's to make (Principle XIX); until then,
they are proposals.

This document plans how the Station Agent takes over radios connected to a
workstation by USB (the Icom IC-7300 and IC-9700, the Yaesu FTDX10 and the
like) and shares them with the programs operators actually run: WSJT-X, VARA
with Winlink Express or VarAC, N3FJP, N1MM Logger+, and LabLog itself.
FlexRadio radios already have their own driver and are not covered here.

## The problem

Today a USB radio is a serial port and a sound card that every program opens
for itself. Setting up a station means matching COM port numbers, picking the
right one of several identical "USB Audio CODEC" devices, setting baud rates
and CI-V addresses in every program, and then finding out that two programs
cannot share one COM port. Windows renumbers ports and resets audio levels on
its own, so a working setup breaks without anyone touching it.

The Agent removes all of that by being the radio's one owner (Principle IX):
it finds the radio, opens it once, and hands every program a ready-made
*virtual radio* to connect to.

## What exists today

The Agent ([station-agent.md](station-agent.md)) reads a radio and reports it
to the Engine. It reaches radios three ways: Hamlib's `rigctld` (started by the
Agent as its own child program), a FlexRadio's network API, and a built-in
pretend radio. It never tunes a radio and never transmits (Principle XI). It
also hears WSJT-X log QSOs and logs them in LabLog.

This plan keeps all of that and adds to it.

## Goals

These come from the maintainer:

1. **The Agent always owns the radio.** No program ever opens the radio's
   serial port or sound card. Every program connects to a virtual radio the
   Agent provides, and every command passes through the Agent.
2. **Setup is automatic.** Plugging in the USB cable is enough for the Agent
   to find the radio, work out what it is and how to talk to it, and offer it
   to programs.
3. **The Agent adapts to the radio.** It works with the radio's settings as
   they are. It changes a setting only when the radio cannot work at all
   without the change, and only with the operator's approval.
4. **Each radio is known by its serial number.** The same radio is recognized
   at any workstation.
5. **The radio's settings are documented on first connect.** Before changing
   anything, the Agent records every setting it can read, so the owner has a
   record of the radio as it arrived.
6. **Radios are described by data files.** One rig definition file per radio
   model, written so other clubs can add their own radios without writing Rust
   (Principle IX).
7. **Open source means other people's programs too.** FieldLab does not use
   N1MM, but other clubs do, so the design accommodates it.

## The shape of it

```
 WSJT-X ──┐                                              ┌── IC-7300 (USB)
 VarAC  ──┤  rigctld server          ┌──────────────┐    │
 LabLog ──┘                          │              │    │
 N1MM   ───  CAT-over-TCP server ────┤  Station     ├────┤
 Winlink ──  virtual COM port ───────┤  Agent       │    │
 N3FJP  ───  N3FJP API (Agent calls) │              │    └── FTDX10 (USB)
 VARA   ───  virtual sound device ───┤  one owner,  │
             + PTT through its client│  one rig     │
                                     │  model       │
                                     └──────────────┘
         "personalities"                  core            "drivers" + rig
       (what programs see)                                definition files
```

There are three layers:

- **Drivers** talk to real radios. A driver is Rust code for one *protocol
  family* (Icom CI-V, Yaesu CAT, Kenwood CAT, FlexRadio API). Which radio it is
  talking to, and that radio's details, come from a rig definition file.
- **The core** is the common rig model the Agent already has (`rig.rs`). It
  holds the radio's live state as the radio reports it (Principle X) and
  decides who may change it and who may transmit.
- **Personalities** are the virtual radios programs connect to. Each one
  translates one outside protocol to and from the core. A personality knows
  nothing about which real radio is underneath, and a driver knows nothing
  about which programs are connected.

A contributor who wants their favorite program supported writes one
personality. One who wants their radio supported writes one rig definition
file, and Rust code only when the radio uses a protocol family no driver
speaks yet.

## Drivers: native first, Hamlib as the fallback

**Native drivers** in Rust for the protocol families that matter most: Icom
CI-V first, then Yaesu CAT. These protocols are small and published by the
makers, and the Agent needs more from them than any general library offers:
reading the radio's identity, reading its menu settings, and reacting the
moment the radio reports a change on its own (Icom calls this "transceive").

**Hamlib stays as the fallback** for every radio without a definition file, run
the way the Agent runs it today: as a separate program the Agent talks to over
Hamlib's documented network protocol. A radio reached this way gets basic
control (frequency, mode, PTT, split) but no settings record and no instant
change reports. Running Hamlib as its own program keeps its code out of
LabLog, keeps licensing simple, and means a fault in Hamlib cannot crash the
Agent.

**Hamlib as a reference.** Hamlib's source records years of hard-won knowledge
about how radios really behave: timing, echo, firmware quirks. Rig definition
files may draw on it, since it is open source under a license compatible with
the AGPL-3.0 (Principle XVI), and every definition that does says so in its
`sources` list. The rule for contributors: learn from Hamlib, write your own.
Makers' published documents come first; Hamlib fills in what they leave out.

## Finding a radio

When a USB device appears, the Agent:

1. **Pairs the serial port and the sound card.** A USB radio shows up as a hub
   with a USB-to-serial bridge and a USB sound device behind it. The Agent
   follows Windows' device tree to see which serial port and which sound
   device hang off the same radio, so nobody has to choose either from a list.
2. **Records the USB serial number**, the radio's identity from then on. If
   Windows renumbers COM5 to COM9, nothing changes. Where a radio reports no
   usable USB serial number, the Agent uses the model plus the identity it
   reads from the radio, and asks the operator to name it once.
3. **Finds the speed.** It tries the baud rates the definition file lists until
   the radio answers. (On the IC-7300 the speed is not readable or settable
   over CI-V, so trying is the only way.)
4. **Asks the radio what it is.** Icom radios answer CI-V command `19 00` with
   their address; Yaesu radios answer `ID;`. The answer picks the rig
   definition file. A radio that answers nothing the Agent recognizes falls
   back to Hamlib if the operator chooses its model.

## Adapting to the radio

The Agent reads what it needs and works with it:

- **CI-V address**: taken from the radio's own replies, not assumed.
- **Transceive on or off**: if the radio reports changes on its own, the Agent
  uses them; if not, it asks for state on a schedule.
- **Echo**: whether the radio echoes commands back is read from the radio or
  detected on the wire, and handled either way.

Some settings make the radio unusable for a job no matter how the Agent
adapts. The definition file lists them as **requirements**, each with the job
it is needed for. For example, on the IC-7300, digital modes need setting 0067
(the transmit audio source in data mode) set to USB; otherwise the radio
transmits silence. When a requirement is not met, the Agent:

1. says exactly what is wrong and why, in plain words;
2. offers to change that one setting, and changes it only if the operator
   says yes;
3. records the old value next to the new one, with who approved the change and
   when.

The Agent never makes a blanket change. A visitor who brings their own radio to
FieldLab takes it home set up the way they brought it, apart from the changes
they approved.

## The settings record

On a radio's first connection, before the Agent changes anything, it reads
every setting its rig definition lists and saves the result as the radio's
**as-received record**: the date, the workstation, every value read, and the
list of settings it could not read. The record is shown as a plain report the
owner can read or print.

Records are stored by the Engine, so a radio is recognized at every station,
and the Agent keeps its own copy so this works with no connection to the
Engine (Principle V). Files are written in full and renamed into place
(Principle XIV).

**Decision needed: restoring settings.** Principle X says "the Agent never
writes a remembered value back over the radio's current one." Restoring the
as-received record does exactly that, even when the operator asks for it. The
options:

- **(a) Amend Principle X** to allow one exception: an operator's explicit,
  confirmed request to restore a radio's recorded settings, recorded like any
  other change. *Recommended*: the restore is the operator's deliberate
  action, not the Agent trusting a stale copy over the radio, which is what X
  exists to prevent.
- **(b) Keep the record only.** The Agent documents the radio; restoring is
  done by hand from the printed report.

Until this is decided, the plan builds the record and not the restore.

## Virtual radios (personalities)

| Personality | What it is | Who uses it |
| --- | --- | --- |
| rigctld server | Hamlib's documented network protocol, on `127.0.0.1` only (Principle XIII) | WSJT-X, VarAC, fldigi, JS8Call, LabLog |
| CAT over TCP | A text command set many loggers understand, on `127.0.0.1` | N1MM Logger+ |
| Virtual COM port | The same command set, on a virtual serial port | Winlink Express, N3FJP if its API is not enough, older programs |
| N3FJP API client | The Agent calls N3FJP's own documented TCP API to tell it the frequency and mode; N3FJP's rig setting is "None" | N3FJP |
| Virtual sound device | A Windows sound device per program, routed by the Agent to and from the radio | WSJT-X, VARA, VarAC, fldigi |

What each program supports, briefly:

- **WSJT-X** speaks rigctld ("Hamlib NET rigctl"), so it needs no COM port.
  It needs real split support (Settings > Radio > Split Operation: Rig), which
  the rig model and the rigctld personality must handle.
- **VarAC** speaks rigctld.
- **Winlink Express** controls the radio through its own built-in drivers over
  a COM port only, and keys PTT itself. It gets a virtual COM port.
- **VARA** never tunes the radio; its client (Winlink Express or VarAC) does.
  VARA's own PTT is turned off so only the client keys, through the Agent.
  Only one client connects to VARA at a time.
- **N1MM Logger+** can reach a radio over TCP when told it is a particular
  radio model, so it needs no virtual COM port.
- **N3FJP** is fed through its API, so it never touches the radio at all.

A virtual radio does not have to match the real one: Winlink might see a
radio that speaks one maker's command set while the Agent is running an
IC-7300. That is what lets any program work with any radio. Each personality
offers what these programs actually use: frequency, mode, split, VFO, and
PTT. Maker-specific extras (scope data, filter shapes) are available only
through the Agent's own API.

**Decision needed: whose command set the CAT personality speaks.** N1MM and
Winlink both understand the Kenwood command set (the TS-2000's, which Kenwood
publishes). Principle IV asks for "open, documented formats and protocols
... not by imitating another product." The Kenwood command set is published
and widely used as a common language between programs and radios, which reads
as an open protocol; but the personality would answer to programs as if it
were a Kenwood radio. *Recommended*: allow it, implemented only from Kenwood's
published command reference, and say plainly in the user guide that the
virtual radio speaks Kenwood's command set.

## Transmitting

Principle XI: nothing transmits unless an operator holding transmit
authorization for that radio deliberately caused it. With programs keying the
radio through the Agent, that means:

- **A program may only transmit if an operator granted it.** When an operator
  signs in at a station, they choose which programs may transmit on which
  radio, for that session ("WSJT-X may transmit on the IC-7300"). A program
  without a grant can tune but not key; its PTT requests are refused and the
  refusal is shown. A grant ends when the operator signs out.
- **One transmitter, one program at a time.** While one program holds PTT,
  another program's PTT request is refused, not queued. Pressing the mic's PTT
  on the radio itself always wins; the Agent sees it from the radio's state
  and refuses program PTT until it is released.
- **Fails closed.** If the program that keyed disconnects, the Agent unkeys.
  If the Agent's own transmit watchdog sees PTT held longer than the limit set
  for the radio, it unkeys and reports why. If the Agent loses the radio
  mid-transmission, it reports the radio as possibly keyed and tells the
  operator to check it. The radio's own transmit time-out timer is the last
  line of defense, and the setup report recommends turning it on.

Every transmit path stays in the Agent (Principles IX and XI), and every one
of them has tests (Principle XVII).

## Audio

The Agent owns the radio's sound device too: programs get virtual sound
devices, and the Agent moves audio between them and the radio.

- **One sample rate, 48 kHz, end to end.**
- **Clock drift.** The radio's sound chip and each virtual device run on their
  own clocks and slowly drift apart. The Agent resamples continuously by tiny
  amounts to keep them locked, or audio glitches every few minutes.
- **Latency.** VARA is the most demanding user: its timing between transmit
  and receive is tight. The target is under 20 ms added latency each way,
  measured, not assumed.
- **Levels.** The Agent sets and holds the Windows levels for the radio's
  sound device and puts them back when Windows changes them.

## Windows specifics

- **Runs as a service plus a tray program.** The service starts at boot, owns
  the radios, and keeps running when nobody is signed in to Windows. The small
  tray program shows status and asks the operator the questions above (approve
  a setting change, grant transmit).
- **Drivers.** Windows only loads signed drivers. A virtual COM port and a
  virtual sound device both need one. **Decision needed**: for the first
  versions, use existing signed drivers (VB-Audio's virtual cable for sound, a
  signed build of com0com for serial ports), which are installed separately
  and are not part of LabLog; or write and sign our own, which is a large
  project. *Recommended*: existing drivers first, our own later only if they
  limit us. VB-Audio's licence must be checked before LabLog's installer
  bundles it; until then it is a separate download.
- **The Agent is still one executable** (Technology Constraints); the drivers
  are separate installs, the same way Hamlib is today.

## Rig definition files

One file per radio model, in TOML, at `agent/rigs/<maker>/<model>.toml`.
TOML because a definition needs comments: where a value came from, which
firmware behaves oddly. JSON has no comments.

Each file says:

- **Identity**: maker, model, protocol family, and how the radio identifies
  itself (for Icom, its default CI-V address).
- **USB layout**: which serial ports and sound device the radio presents.
- **Link**: the baud rates to try, the default address, echo behavior.
- **Modes**: the radio's mode codes, mapped to the rig model's modes.
- **Commands**: the protocol commands the driver uses for each rig-model
  operation, where they differ from the family's usual ones.
- **Settings**: every menu setting the Agent may read, with its command, its
  values, and whether it may be written.
- **Requirements**: the settings a job needs (digital modes, CAT PTT), with
  the acceptable values and a plain-words explanation for the operator.
- **Sources**: the documents each part came from.
- **Verification**: every command and setting carries `verified = true` only
  once it has been checked against a real radio. The Agent reads unverified
  settings but **never writes them**.

Rules (Principle XII): a definition file is untrusted data, never code. The
Agent checks every file against the schema when it loads it, refuses one that
fails with a message naming the file and the problem, and runs fine without
it. CI checks every definition file in the repository against the same
schema, so a broken one cannot be merged. The schema is defined in Rust, in
the Agent, so there is one source of truth.

[`agent/rigs/icom/ic-7300.toml`](../agent/rigs/icom/ic-7300.toml) is the first
definition and the model for the rest.

## Working with no internet

Everything here works on a station network with no internet (Principle V):
finding radios, virtual radios, the settings record (kept locally and sent to
the Engine when it is reachable), and transmit grants (made through the
Engine on the local network).

## Order of work

Each step is its own pull request, with tests and a demonstration
(Principle XVII):

1. **Rig definition schema and loader**, with the IC-7300 file checked in CI.
2. **Pretend IC-7300.** A simulated radio that speaks CI-V like the real one,
   so drivers and personalities are built and tested without a radio on the
   bench. The existing pretend radio grows into this.
3. **Icom CI-V driver, reading only:** finding the radio, baud search,
   identity, live state, transceive. This replaces Hamlib for the IC-7300.
4. **Settings record** on first connect.
5. **rigctld personality** on `127.0.0.1`: WSJT-X connects to the Agent
   instead of to the radio. Tuning first, no transmit.
6. **Transmit grants and the PTT path**, with the watchdog and refusal tests.
   WSJT-X transmits through the Agent.
7. **Audio**: the Agent owns the radio's sound device; WSJT-X uses a virtual
   one.
8. **VARA with Winlink Express and VarAC**: the virtual COM port and the
   audio latency work.
9. **N3FJP** through its API.
10. **IC-9700 and FTDX10** definitions (the FTDX10 adds the Yaesu CAT driver).
11. **N1MM Logger+** through the CAT-over-TCP personality.

## Decisions needed

| # | Question | Recommended |
| --- | --- | --- |
| 1 | Restore a radio's recorded settings: amend Principle X, or keep the record only? | Amend X for an explicit, confirmed, recorded operator restore |
| 2 | May the CAT personality speak Kenwood's published command set (Principle IV)? | Yes, from Kenwood's published reference only |
| 3 | Transmit grants per program, per radio, per session, as described above? | Yes |
| 4 | Existing signed drivers (VB-Audio, com0com) first, or our own? | Existing first |
| 5 | Service plus tray program on Windows? | Yes |
