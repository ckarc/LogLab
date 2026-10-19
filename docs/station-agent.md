<!-- SPDX-FileCopyrightText: 2026 LogLab contributors -->
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
- **The pretend radio.** Built in, for trying LogLab with no radio. It visits
  a few spots on the bands and "transmits" for a few seconds at each.

## Setting up a station

1. On the radio's computer, start the Agent, naming the station operators
   sign in at and a file to keep its key in:

   ```sh
   loglab-agent run --engine http://127.0.0.1:8073 --station shack \
       --key-file ~/.config/loglab/agent-key \
       --hamlib-model 3073 --device /dev/ttyUSB0 --baud 19200
   ```

   Use `--pretend` in place of the Hamlib options to try it without a radio,
   or `--rigctld 127.0.0.1:4532` to use a `rigctld` already running there.
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
- The Engine forgets a station's radio 15 seconds after its Agent goes quiet.
- A request to join grants nothing until an administrator approves it, and
  expires after 10 minutes (the Agent then asks again with a new code).
- A data mode on the radio (`DATA-USB`) leaves the QSO's mode alone, since
  only the operator knows whether it is FT8 or PSK31.
