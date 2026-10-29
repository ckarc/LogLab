<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Station Agent: USB Radios and Virtual Radios

**Status: design, all decisions made.** Nothing here is built yet. The
maintainer's decisions are recorded where they apply and listed at the end
(Principle XIX).

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
pretend radio. It never tunes a radio and never transmits. It
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
   they are. It changes a setting only to put back a setup kept for the
   program using the radio, or when the radio cannot work at all without the
   change, and then only with the operator's approval.
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

Every record also keeps each setting's bytes exactly as the radio sent them,
and the radio's own serial number (from its USB serial port), so each radio
has its own record, even beside another of the same model. `station` reads
and keeps the record by itself, in `records` beside the Agent, and says what
has changed since the radio arrived.

**Restoring settings.** An operator can ask the Agent to put a radio's
recorded settings back. The Agent then restores only after the operator
confirms it, and records the restore like any other change it makes to a
radio. (Principle X, amended in constitution 3.0.0.)

**Setups per program, applied by themselves.** The lab keeps one known-good
profile per radio model for each program (VARA, FT8, JS8, fldigi, SSTV), in
`agent/profiles/` and built into the Agent, so it is locked: only a new build
replaces it, and the Agent never saves a radio's settings over it (decided by
brian, 2026-10-28). When a program connects to the virtual radio, the Agent
tells which program it is from the process that owns the connection
(Windows' `GetExtendedTcpTable` and the process's executable; `VarAC.exe`
means VARA, since VarAC is the program that drives the radio for VARA) and,
on an enrolled radio, puts that program's profile on without asking, shows
what it changed, and records it. It does this once per program start; while
the program runs, the operator's changes on the radio stand. When two
programs share a radio, the first keeps it. Operators may still keep their
own named setups and put one back by choice (`restore`, after a yes); the
lab's profile names are locked, so `keep VARA` is refused and the tray keeps
"My VARA" instead, while `restore VARA` puts the lab's profile back.
(Principle X, second exception, constitution 5.0.0.)

- **Only enrolled radios.** The first time the Agent sees a radio of a model
  the lab has profiles for, it asks once whether this is a FieldLab radio.
  Yes enrolls it in lab settings control; no leaves it alone for good, as for
  a visitor's radio. `enroll on` or `enroll off` (or the tray's Lab radio
  menu) changes it later. Kept per radio serial number in `records`.
- **Only verified settings.** As with any restore, the Agent writes only
  settings its rig definition marks writable and checked on a real radio,
  and reads each one back afterward.
- **Never mid-transmission.** The program's first transmission waits until
  its profile is on (up to 15 seconds), and nothing is written while a
  program has the radio keyed.

**Lab Stock.** Each radio model also has one everyday baseline, its Lab
Stock: every setting the Agent may write, the way the lab wants a radio of
that model left between users. The per-program profiles sit on top of it.
Lab Stock is kept on the Engine and managed in LabLog, behind a gate (brian,
2026-10-29: "any changes to the labstock settings should be gated"):

- **Proposing.** `stock save` at the Agent's console (or the tray's Lab Stock
  menu, "Send settings as a Lab Stock proposal") sends the radio's settings
  to the Engine as a proposal. A proposal changes nothing.
- **The gate.** In LabLog, Admin then Lab Stock, an administrator sees each
  proposal and exactly which settings it changes against the Lab Stock now,
  and approves or refuses it, once. The newest approval is the model's Lab
  Stock. The database refuses a decision from anyone but a signed-in
  administrator, and a station key can never approve. Approving is refused
  if the Lab Stock changed since the administrator compared it. Going back to
  an earlier one is a new proposal that needs its own approval. Every
  proposal and decision is kept (Principle VII; migration
  `0022_lab_stock`).
- **Putting it on.** `stock` (the tray's "Put Lab Stock on") shows what
  putting the approved Lab Stock on would change and waits for the
  operator's yes, even in auto mode; then the restore path writes it (mode
  and filter first, only checked settings, read back, recorded in
  `records`). (Principle X, first exception.)
- **Only checked settings.** A proposal holds only settings the rig
  definition marks writable and checked on a real radio, and not the clock.
  A model with none checked yet (the IC-9700, until `check-writes` runs on
  one) has nothing to propose.
- **Every filter's width.** The radio reads and sets only the selected
  filter's width (`1A 03`), so `stock save` asks a yes, then chooses FIL1,
  FIL2 and FIL3 in turn on the mode the radio is on, reads each width, and
  puts the radio back on the filter it was on, reading it again to be sure
  (brian, 2026-10-27: back exactly as found). `stock` sets each filter's
  width the same way, last, on the Lab Stock's mode, then selects the Lab
  Stock's filter. In data mode the filter is chosen with `1A 06`, since `06`
  turns data mode off. Widths are kept for the Lab Stock's mode only.

## Virtual radios (personalities)

| Personality | What it is | Who uses it |
| --- | --- | --- |
| rigctld server | Hamlib's documented network protocol, on `127.0.0.1` only | WSJT-X, VarAC, fldigi, JS8Call, LabLog |
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

**The CAT personality speaks Kenwood's command set** (the TS-2000's, which
Kenwood publishes), because N1MM and Winlink both understand it. It is built
only from Kenwood's published command reference, and the user guide says
plainly that the virtual radio speaks Kenwood's command set. (Principle IV,
amended in constitution 3.0.0: a maker's published command set is an open
protocol.)

## Who controls a radio

LabLog is a logging application; it never tunes or transmits. Control of a
radio belongs to the computer it is connected to, and it goes to **the
program using the radio's audio**, with nothing to click: starting WSJT-X,
VarAC or MMSSTV is the choice. (Decided by the maintainer, 2026-10-27.)

- **The program using the radio's audio controls it.** It has full control:
  frequency, mode, split, VFO and PTT, and the Agent sets the radio up for it
  (above). For VARA, that is VARA together with the client connected to the
  virtual radio (VarAC or Winlink Express): VARA never tunes, and its client
  keys for it.
- **One program at a time.** If a second program starts using the same
  radio's audio while one has it, the first keeps control and the Agent says
  so; the second is read-only until the first stops.
- **Nothing in control to begin with.** Until a program uses the radio's
  audio, nothing can tune or key the radio.
- **Other programs are read-only.** They see the frequency and mode and hear
  receive audio, so they log correctly, but the Agent refuses every command
  from them that would change the radio, and their transmit audio never
  reaches it.
- **The radio's own PTT always wins.** Pressing the microphone's PTT on the
  radio overrides the program; the Agent sees it from the radio's state and
  refuses program PTT until it is released.
- **The network only reads.** Programs on other computers, LabLog included,
  can read a radio's state from the Agent. Nothing on the network can tune or
  key a radio.
- **The tray shows it.** The tray program shows each radio, which program has
  it, and the setup it applied. Choosing another program there is possible,
  but never needed.

**How the Agent knows which program is which.** A program does not say its
name, so the Agent asks Windows. Windows lists the programs using each sound
device and their process numbers (Core Audio's `IAudioSessionManager2` and
`IAudioSessionControl2::GetProcessId`, Microsoft Learn), and lists which
process owns each network connection (`GetExtendedTcpTable`), so the Agent
matches the program using the radio's virtual sound device to its
connection to the virtual radio. Microsoft's pages do not say whether
programs that only record from a device are listed the same way, so this is
to be checked on a real computer before it is relied on.

**Setting a program up: the setup card.** Except for the programs listed
below, the Agent never writes another program's settings files. Their formats aren't documented, can change with
any version, and a program like WSJT-X rewrites its own file when it
closes, so a change written while it runs is lost. Instead, the tray shows
a setup card for each program and radio, with exactly what to enter, each
value with a copy button:

```
WSJT-X, for the IC-7300
  Rig:             Hamlib NET rigctl
  Network Server:  127.0.0.1:4532
  PTT Method:      CAT
  Mode:            Data/Pkt
  Bands this radio has: 160 m to 6 m (delete 2 m and 70 cm)
```

The person types it in once, and the program keeps it in its own file.

**The exceptions: VARA, WSJT-X, JS8Call, fldigi and MMSSTV.** None of these
has a command line or network command for choosing its sound devices or its
radio (varanny's README: "VARA doesn't offer command line configuration
options"; for the others, their source code), and a station should work
with nothing typed in. So the maintainer decided that the Agent sets them up
itself: VARA's sound devices (2026-10-26), and WSJT-X, JS8Call, fldigi and
MMSSTV (2026-10-27, "as seamless as it can be"). For each, the Agent changes
only the lines that name the sound devices and the radio, and only while
the program isn't running (each writes its own file when it closes). It
keeps the file as it first found it (`<file>.lablog-original`), and writes
the file whole and renames it into place. No other line, and no other
program's file, is touched.

| Program | File | What the Agent sets |
|---|---|---|
| VARA | `VARA.ini` next to `VARA.exe` | `[Soundcard]` input and output device |
| WSJT-X, JS8Call | `%LOCALAPPDATA%\<app>\<app>.ini` | `[Configuration]` `SoundInName`, `SoundOutName`, `Rig` (Hamlib NET rigctl), `CATNetworkPort`, `PTTMethod` (CAT), `DataMode` (Data/Pkt), `Polling` (1, only if missing or 0) |
| fldigi | `%USERPROFILE%\fldigi.files\fldigi_def.xml` | PortAudio input and output device, by the 31-character name PortAudio gives the cable's MME device (fldigi opens the first device with exactly that name); Hamlib on (model 2, NET rigctl) at the virtual radio, PTT by Hamlib; flrig and RigCAT off |
| MMSSTV | `Mmsstv.ini` next to `Mmsstv.exe` | `[Define]` `SoundDevice` input only, for now |

MMSSTV keys a radio only through a COM port, so it receives through the
Agent but can't transmit through it until the Agent offers it a virtual COM
port (step 8).

**When a program complains: the fix.** The Agent sees every request a
program makes and what became of it, so when one is refused it says why,
in plain words, and how to fix it on the program's side. For example, a
band the radio doesn't have: "The IC-7300 most likely has no band there ...
in WSJT-X, Settings > Frequencies." A program that shows an error and
reconnects usually did so right after a refused request, and the Agent's
log puts the two side by side. `serve` already does this for refused bands,
modes, split, and transmit requests; the tray shows the same.

**Failing safe.** When anything goes wrong, the result is not transmitting:

- If the program in control disconnects while keyed, the Agent unkeys.
- If the Agent's transmit watchdog sees PTT held longer than the limit set for
  the radio, it unkeys and reports why.
- The Agent never keys a radio on its own: not on a timer, not as a retry, not
  after a reconnect.
- If the Agent loses the radio mid-transmission, it reports the radio as
  possibly keyed and tells the operator to check it. The radio's own transmit
  time-out timer is the last line of defense, and the setup report recommends
  turning it on.

Every transmit path is in the Agent (Principle IX), and every one has tests
(Principle XVII).

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

- **Runs as a tray program, not a service.** Windows starts it at sign-in
  (`startup on`, the Run key), with no window; its icon by the clock shows
  the radio's state, its menu does what is typed at `station`, and a Yes/No
  box asks before a change the operator must approve. It is one program in
  the signed-in person's session, because a service can't show anything to
  the person at the computer (Microsoft Learn, "Interactive Services":
  "Services cannot directly interact with a user as of Windows Vista") and
  the radio's audio devices and programs' settings belong to that person.
  So the Agent runs only while someone is signed in. (Decided by the
  maintainer, 2026-10-28.)
- **Drivers: existing signed drivers first.** Windows only loads signed
  drivers, and a virtual COM port and a virtual sound device both need one.
  The first versions use existing signed drivers: VB-Audio's virtual cables
  for sound and a signed build of com0com for COM ports. They are installed
  separately and are not part of LabLog. Each program needs its own virtual
  sound device, so a station running several programs needs VB-Audio's paid
  additional cables; VB-Audio's licence must allow it before any LabLog
  installer bundles their driver, and until then it is a separate download.
  LabLog's own drivers come later, only if these limit us.
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
  A radio with more than one serial port behind its USB names its control
  port by how its USB serial number ends (`number_ends_with = "A"` for the
  IC-9700's USB (A)); the Agent never opens the others, which may carry data
  the radio transmits.
- **Link**: the baud rates to try, the default address, echo behavior.
- **Modes**: the radio's mode codes, mapped to the rig model's modes.
- **Commands**: the protocol commands the driver uses for each rig-model
  operation, where they differ from the family's usual ones.
- **Settings**: every setting the radio exposes, each with what reads it and
  whether it may be written. A menu item is given by its `item` number (read
  with `1A 05` on Icom); a front-panel control, such as the RF gain or the
  noise reduction, by its own `command` (`14 02`, `16 40`). The Agent reads
  settings only with commands that just ask when sent with nothing after
  them; a definition naming any other command (`07 00`, which selects a VFO,
  or anything in `1C`, which keys the radio or starts the tuner) is refused.
  A setting has named `values`, a `range`, or `raw = true` when no source
  says what its values mean: then the Agent keeps, compares and would put
  back the bytes the radio sends, without saying what they mean.
- **Requirements**: the settings a job needs (digital modes, CAT PTT), with
  the acceptable values and a plain-words explanation for the operator.
- **Sources**: the documents each part came from.
- **Verification**: every command and setting carries `verified = true` only
  once its values have been checked against a real radio. A setting also
  carries `write_checked = "SOURCE"` once a real radio took its own value
  written back with the command that sets it (`loglab-agent check-writes`).
  The Agent reads every setting, but **writes one only if it is
  `write_checked`**, and then only in a restore the operator confirmed.
  `moves = true` marks a setting that changes by itself, such as the clock:
  never compared, never put back.

Rules (Principle XII): a definition file is untrusted data, never code. The
Agent checks every file against the schema when it loads it, refuses one that
fails with a message naming the file and the problem, and runs fine without
it. CI checks every definition file in the repository against the same
schema, so a broken one cannot be merged. The schema is defined in Rust, in
the Agent, so there is one source of truth.

[`agent/rigs/icom/ic-7300.toml`](../agent/rigs/icom/ic-7300.toml) is the first
definition and the model for the rest.
[`agent/rigs/icom/ic-9700.toml`](../agent/rigs/icom/ic-9700.toml) uses the same
CI-V driver at address A2h; nothing in it is checked on a real IC-9700 yet,
so the Agent records its settings but writes none until `check-writes` has
been run on one.

[`agent/rigs/yaesu/ftdx10.toml`](../agent/rigs/yaesu/ftdx10.toml) is the first
Yaesu CAT definition (`family = "yaesu-cat"`), driven by the Agent's own CAT
driver (`agent/src/cat_radio.rs`). CAT is text, so a CAT definition writes a
command as it is sent without its semicolon (`RG0`), a menu item as its group,
page and item (`01 04 17` is `EX010417`), and a value's length in `digits`.
The Agent keeps a value of digits as BCD bytes, as an Icom radio sends its
values, so ranges, records and profiles mean the same for both. A CAT link has
no addresses; it says its `stop_bits`, whether RTS is held on (`rts = "on"`,
refused for CI-V), and what the radio answers to `ID` (`identity.answer`).
Settings are read only with commands on the Agent's list of CAT commands that
only ask, and Yaesu's menu group 05 (clock, loads, saves and resets) is
refused. A radio whose one USB bridge has more ports than its control port
names its control port's USB `interface` (the FTDX10's Enhanced COM Port is
0); the Agent never opens the other, whose RTS and DTR key the radio. Nothing
in the FTDX10 file is checked on a real radio yet.

## Working with no internet

Everything here works on a station network with no internet (Principle V):
finding radios, virtual radios, the settings record (kept locally and sent to
the Engine when it is reachable), and which program controls a radio (decided on the computer itself, from
the program using the radio's audio, with no network at all).

## Order of work

Each step is its own pull request, with tests and a demonstration
(Principle XVII):

1. **Rig definition schema and loader**, with the IC-7300 file checked in CI.
2. **Pretend IC-7300.** A simulated radio that speaks CI-V like the real one,
   so drivers and personalities are built and tested without a radio on the
   bench. The existing pretend radio grows into this.
3. **Icom CI-V driver, reading only:** finding the radio, baud search,
   identity, live state, transceive. This replaces Hamlib for the IC-7300.
   *Done, on a real serial port too (`--civ COM5`).*
4. **Settings record** on first connect.
5. **rigctld personality** on `127.0.0.1`: WSJT-X connects to the Agent
   instead of to the radio. Tuning first, no transmit.
6. **The tray program and the PTT path**: control by the program using the
   radio's audio, read-only other programs, the watchdog, and refusal tests. WSJT-X
   transmits through the Agent. *The PTT path is done, chosen on the command
   line until control follows the audio (`serve --allow-transmit`): one program at a
   time, unkey on disconnect, a time limit, the radio's own PTT wins, with
   tests.*
7. **Audio**: the Agent owns the radio's sound device; WSJT-X uses a virtual
   one. *Receiving is done (`rx-audio`): the radio's audio into a VB-Audio
   cable, with clock-drift correction. Sending comes with transmitting.*
8. **VARA with Winlink Express and VarAC**: the virtual COM port and the
   audio latency work.
9. **N3FJP** through its API.
10. **IC-9700 and FTDX10** definitions (the FTDX10 adds the Yaesu CAT driver).
11. **N1MM Logger+** through the CAT-over-TCP personality.

## Decisions

| # | Question | Status |
| --- | --- | --- |
| 1 | Restore a radio's recorded settings? | **Decided**: yes, on the operator's explicit, confirmed request (Principle X, constitution 3.0.0) |
| 2 | May the CAT personality speak Kenwood's published command set? | **Decided**: yes (Principle IV, constitution 3.0.0) |
| 3 | Who decides which program may control and key a radio? | **Decided** (changed 2026-10-27): the program using the radio's audio, with nothing to click; the tray only shows it |
| 4 | Existing signed drivers (VB-Audio, com0com) first, or our own? | **Decided**: existing signed drivers first |
| 5 | Tray program only, or a service with the tray program as its face? | **Decided** (changed 2026-10-28): tray program only; the Agent runs while someone is signed in |
| 6 | Must the operator confirm each setup the Agent applies? | **Decided**: no; a radio's kept setup for a program is applied when that program starts using the radio (Principle X, constitution 5.0.0) |
