<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# The Station Agent

The Station Agent runs on the computer the radio is plugged into. It owns the
radio (Principle IX), reads it, and streams what the radio reports to the
Engine: frequency, band, mode, filter width, and whether it is transmitting
(Principle X). The web client's New QSO box fills in from it, and the On air
box shows it.

This version only reads the radio. It never tunes it and never transmits.

## How it reaches a radio

- **Hamlib.** The Agent starts Hamlib's `rigctld` for the radio, listening
  only on this computer, and is its only user. Hamlib must be installed
  (`sudo apt install libhamlib-utils` on Debian or Ubuntu). Hamlib numbers
  each radio model; `rigctl --list` shows them (3073 is the Icom IC-7300).
- **FlexRadio.** A FlexRadio 6000- or 8000-series radio (the FLEX-8400, for
  example) is reached over the network through its own published API, on TCP
  port 4992, with `--flex ADDRESS`. Nothing needs installing. The radio lets
  several programs connect at once, so SmartSDR keeps working beside the
  Agent. The Agent reports the transmit slice (or the active slice when none
  transmits) and names it, such as "Slice A". SmartSDR's radio chooser shows
  the radio's IP address; giving the radio a fixed address on the network
  keeps the Agent's command line right.
- **The Agent's own CI-V driver (Icom).** Built in, and read-only. It tries
  each speed its rig definitions list, asks each known radio "who are you?"
  (CI-V `19 00`), and reads the one that answers: frequency, mode, data mode,
  and whether it is transmitting. It is not yet connected to a serial port:
  `--civ-pretend` runs it against a pretend IC-7300, and the serial port comes
  once there is a radio to test it on. LabLog shows the connection as
  "Icom CI-V".
- **The pretend radio.** Built in, for trying LabLog with no radio. It visits
  a few spots on the bands and "transmits" for a few seconds at each.

## Setting up a station

1. On the radio's computer, start the Agent, naming the station operators
   sign in at and a file to keep its key in:

   ```sh
   loglab-agent run --engine http://127.0.0.1:8073 --station shack \
       --key-file ~/.config/loglab/agent-key \
       --hamlib-model 3073 --device /dev/ttyUSB0 --baud 19200
   ```

   Use `--flex 192.168.1.20` (the radio's address) in place of the Hamlib
   options for a FlexRadio, `--pretend` to try it without a radio, or
   `--rigctld 127.0.0.1:4532` to use a `rigctld` already running there.
   `loglab-agent read RADIO` reads the radio once and prints it, without the
   Engine.

2. The first time, the Agent has no key, so it asks to join and prints a
   short code. An administrator signed in to the web client sees "1 Agent
   asking to join" in the top bar (or Menu > Admin > Station Agents), checks
   that the code matches, and clicks Approve. The Agent saves its key, readable
   only by its account, and starts sending the radio. After that it starts
   straight away.

   Station Agents also lists the stations that have an Agent; Disconnect ends
   one's key. An Agent whose key stops working asks to join again. From the
   command line, `loglab-operator agent-key STATION --write FILE` makes a key
   directly, `--end` ends it, and `loglab-operator agent-keys` lists them.

3. Sign in to the web client at the same station name (`shack` above). New
   QSO follows the radio until you type in Freq, Band, or Mode yourself; the
   next QSO follows it again.

## Logging from WSJT-X

The Agent can hear WSJT-X (or JTDX) log a QSO on the same computer and log
it in LabLog, as the operator signed in at that station. Nobody types it
twice.

1. In WSJT-X, open Settings > Reporting. Set **UDP Server** to `127.0.0.1`
   and **UDP Server port number** to `2237`. The Agent only listens; it sends
   nothing to WSJT-X.
2. Start the Agent with `--wsjtx 127.0.0.1:2237` added:

   ```sh
   loglab-agent run --engine http://127.0.0.1:8073 --station shack \
       --key-file ~/.config/loglab/agent-key --wsjtx 127.0.0.1:2237 \
       --hamlib-model 3073 --device /dev/ttyUSB0 --baud 19200
   ```

3. Sign in to LabLog at that station (`shack`) and pick the operation, as
   for typing QSOs. When you click **Log QSO** in WSJT-X, the QSO appears in
   the Log a moment later, logged by you at `shack`, in your operation. Its
   history says it came from WSJT-X through the Station Agent.

How it behaves:

- **Every field WSJT-X wrote is kept** as it wrote it: call, grid, reports,
  frequency and the rest. LabLog adds only what typing would add (the
  operation's call and exchange, when you log in an operation), never
  something it guessed (Principle XX).
- **Nothing is lost.** Each QSO goes into the Agent's journal on disk first
  (the key file's name plus `.qsos`, or `--journal FILE`). It stays there
  until the Engine has saved it, so a QSO logged while the Engine is down,
  the network is out, or nobody is signed in at the station is sent later,
  once, under the ID it got when it arrived (Principle XV).
- **No duplicates.** A QSO already in the log (the same call, band and mode
  in the same UTC minute, for instance one brought in earlier with Menu >
  Operation > Import an ADIF file) is not logged again.
- **A QSO the Engine refuses** (one made outside the operation's hours, say)
  is kept in an ADIF file beside the journal (`...qsos.refused.adi`). The
  Agent says so; fix what was wrong and import that file from the Menu.
- **Only this computer.** WSJT-X's messages carry no password, so the Agent
  listens only on `127.0.0.1` and ignores anything from elsewhere
  (Principle XXI).
- **One listener per address.** WSJT-X sends to one address. If another
  program on the computer (GridTracker, JTAlert) also wants WSJT-X's
  messages, it must use a different port; that program's own settings may
  let it pass them on.

## Checking a rig definition

Command-driven radios are described by rig definition files in `agent/rigs/`
(see [station-agent-usb-radios.md](station-agent-usb-radios.md)). To check
one, or every file in a folder:

```sh
loglab-agent check-rig agent/rigs
```

Each file is listed as `ok` with a one-line summary, or as `REFUSED` with
every problem found in it. A misspelled key is shown with its line and column.
The command exits with an error if any file is refused, and the Agent's tests
check every file in `agent/rigs/`, so a broken definition cannot be merged
(Principle XII).

## Sharing a radio with WSJT-X and other programs

The Agent can be a virtual radio for programs that speak Hamlib's `rigctld`
network protocol, such as WSJT-X, VarAC and fldigi. They connect to the
Agent, and the Agent stays the radio's one owner (Principle IX).

```sh
loglab-agent serve RADIO --listen 127.0.0.1:4532
```

In WSJT-X, open Settings > Radio and set **Rig** to `Hamlib NET rigctl` and
**Network Server** to `127.0.0.1:4532` (the default if `--listen` is left
out). WSJT-X then follows the radio's frequency and mode.

How it behaves:

- **Read-only, for now.** Every request to tune, change mode, or transmit is
  refused with Hamlib's "command rejected by the rig" (`RPRT -9`), and
  nothing reaches the radio. A band change in WSJT-X shows an error and the
  radio stays where it is.
- **This computer only.** The protocol has no sign-in, so the Agent listens
  only on `127.0.0.1` and refuses connections from anywhere else
  (Principle XXI).
- **Several programs at once.** Up to eight can connect; readings are
  shared, so they don't crowd the radio.
- Any radio the Agent can read can be served: `--civ-pretend`, `--pretend`,
  `--flex`, or a Hamlib radio. Hamlib's own network client, the one WSJT-X
  uses, has been checked against it with Hamlib 4.5.5.

## A radio's settings record

The first time the Agent meets a radio, it can read every menu setting the
radio's rig definition lists and keep them as the radio's **as-received
record**: when (UTC), on which computer, and every value. It never
overwrites that record. Reading the radio again compares it against the
record and lists what has changed since the radio arrived.

```sh
loglab-agent record --civ-pretend --records FOLDER
```

The report also names any setting that stops a job from working, in plain
words. For example, with DATA MOD on MIC, WSJT-X and VARA would transmit
silence. It never changes a setting; offering a change, with your OK, comes
later. Records are JSON files in FOLDER, one per radio, written whole and
renamed into place. For now only `--civ-pretend` can be recorded: the
Agent's CI-V driver is not yet connected to a serial port.

## Watching CI-V

`loglab-agent civ-demo` holds a short conversation with a pretend IC-7300 and
prints every message both ways, byte by byte, with what each one means:
asking who it is, reading and tuning the frequency, reading the mode and two
menu settings, a tuning request it refuses, and a report it sends on its own
when someone turns its dial. Nothing in it talks to a real radio. The same
pretend radio is what the Agent's CI-V code is tested against.

## Notes

- `rigctld` has no sign-in, so the Agent runs it on this computer only
  (Principle XXI). Stopping the Agent stops it too.
- The FlexRadio driver only asks: it subscribes to slice and transmitter
  status, asks the radio's model, and pings it every 5 seconds. It sends no
  command that changes the radio. If the radio says nothing for 15 seconds,
  the Agent reconnects.
- The Engine forgets a station's radio 15 seconds after its Agent goes quiet.
- A request to join grants nothing until an administrator approves it, and
  expires after 10 minutes (the Agent then asks again with a new code).
- A data mode on the radio (`DATA-USB`) leaves the QSO's mode alone, since
  only the operator knows whether it is FT8 or PSK31.
