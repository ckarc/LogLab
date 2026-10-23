<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# The Station Agent

The Station Agent runs on the computer the radio is plugged into. It owns the
radio (Principle IX), reads it, and streams what the radio reports to the
Engine: frequency, band, mode, filter width, and whether it is transmitting
(Principle X). The web client's New QSO box fills in from it, and the On air
box shows it.

This version only reads the radio. It never tunes it and never transmits
(Principle XI).

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
  (Principle XIII).
- **One listener per address.** WSJT-X sends to one address. If another
  program on the computer (GridTracker, JTAlert) also wants WSJT-X's
  messages, it must use a different port; that program's own settings may
  let it pass them on.

## Notes

- `rigctld` has no sign-in, so the Agent runs it on this computer only
  (Principle XIII). Stopping the Agent stops it too.
- The FlexRadio driver only asks: it subscribes to slice and transmitter
  status, asks the radio's model, and pings it every 5 seconds. It sends no
  command that changes the radio. If the radio says nothing for 15 seconds,
  the Agent reconnects.
- The Engine forgets a station's radio 15 seconds after its Agent goes quiet.
- A request to join grants nothing until an administrator approves it, and
  expires after 10 minutes (the Agent then asks again with a new code).
- A data mode on the radio (`DATA-USB`) leaves the QSO's mode alone, since
  only the operator knows whether it is FT8 or PSK31.
