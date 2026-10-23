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
