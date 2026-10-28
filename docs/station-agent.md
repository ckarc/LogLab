<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# The Station Agent

The Station Agent runs on the computer the radio is plugged into. It owns the
radio (Principle IX), reads it, and streams what the radio reports to the
Engine: frequency, band, mode, filter width, and whether it is transmitting
(Principle X). The web client's New QSO box fills in from it, and the On air
box shows it.

On its own it only reads the radio. It tunes or keys a radio only when the
operator starts `serve` with `--allow-tuning` or `--allow-transmit`, for the
program that asks (see "Sharing a radio" below).

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
  and whether it is transmitting. `--civ COM5` (or `--civ /dev/ttyUSB0`) uses
  the radio's own USB serial port; `--civ-pretend` runs it against a pretend
  IC-7300. On Windows the IC-7300's port needs Silicon Labs' CP210x driver
  (Device Manager shows "CP2102 USB to UART Bridge Controller" with an error
  until it is installed). The Agent holds the port's DTR and RTS lines off, so
  a radio set to key from them (USB SEND) is never keyed by opening it. LabLog
  shows the connection as "Icom CI-V".
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
   command line, `lablog-operator agent-key STATION --write FILE` makes a key
   directly, `--end` ends it, and `lablog-operator agent-keys` lists them.

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

## One command for the whole station: `station`

```sh
loglab-agent station                 # finds the radio by itself
loglab-agent station --civ COM5      # or names its port
loglab-agent station --engine http://192.168.1.10:8073 --key-file station.key
                                     # and keeps its settings on the Engine
```

`station` does everything programs on this computer need to use the radio:

1. **Finds the radio.** It looks for a serial port whose USB IDs match a
   radio it knows (an IC-7300's Silicon Labs CP210x bridge, 10C4:EA60, with a
   USB serial number like `IC-7300_02065962` tried first), and takes the one
   whose radio answers "who are you?", at whatever speed it answers at.
   If the radio is plugged in but Windows has no driver for its USB serial
   bridge (the IC-7300's Silicon Labs CP210x has none built in), the Agent
   gets it: from the copy in `drivers` beside the Agent or in Downloads,
   else from Silicon Labs with Windows' own `curl.exe`, else by opening
   Silicon Labs' download page in your browser and waiting for the file to
   arrive in Downloads. It unpacks it and installs it; Windows asks once
   whether to allow that. It keeps the file in `drivers`, so copying that
   folder to the next computer lets it install with no internet. (The
   driver's license allows that copy, not shipping the driver with the
   Agent.)
2. **Reads all the radio's settings** (259 on the IC-7300) and keeps the
   first reading of each radio, by its serial number, as its as-received
   record in `records` beside the Agent. On later starts it lists what has
   changed since. It checks the settings digital modes need, such as DATA
   MOD, and says what to set on the radio if one is wrong. It changes nothing
   on the radio by itself: settings are only put back with `restore`
   (below). The clock is never listed as a change.
3. **Carries the radio's audio both ways** (Windows). Receive audio plays into
   VB-CABLE ("CABLE Input"), so programs record it from "CABLE Output".
   Transmit audio goes from a second VB-Audio virtual cable to the radio's
   USB sound device, **only while the Agent has the radio keyed for a
   program**; the rest of the time the radio gets silence and the program's
   audio is thrown away. Without a second cable, receive still works and the
   window says transmit audio doesn't go through the Agent. VB-Audio's
   Hi-Fi Cable is "free to use" for end users (vb-audio.com/Cable); its
   cables A+B are donationware.
4. **Sets VARA's sound devices** to those cables in `VARA.ini`, if VARA is
   installed in `C:\VARA` or Program Files and isn't running (see the plan,
   "The exception: VARA's sound devices"). With no transmit cable, VARA's
   output is set to the radio's own USB sound device, so VARA plays straight
   into the radio. If VARA is running, it says so and leaves the file alone.
5. **Serves the radio** at `127.0.0.1:4532`, as `serve --allow-transmit`:
   programs may tune and key it, one at a time, with the same failing-safe
   rules (below). After each transmission it shows how loud the program's
   transmit audio was, or that it was silent and which device the program
   should play into.
6. **Keeps the radio's settings on the Engine** (with `--engine` and
   `--key-file`), so every computer the radio is plugged into knows them.
   See "Setups kept on the Engine" below.

Starting `station` is the operator's choice that programs on this computer
may key the radio (Principle IX).

If the radio isn't there yet (off, or its USB cable out), `station` says why
once and keeps looking every 5 seconds until it is, so it can be started
before the radio is.

**Starting with Windows.** `loglab-agent startup on` adds the Agent to the
programs Windows runs each time you sign in (the `Run` key under
`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, as
Microsoft Learn's "Run and RunOnce Registry Keys" describes), running
`station` from wherever the Agent is. `startup off` removes it, and
`startup status` says whether it is there. Windows doesn't promise how soon
after sign-in it starts the programs listed there, or in what order, so the
Agent may start after VARA the first time. Then it leaves VARA's file alone
and says so; once set, VARA keeps the setting. A Windows service, which
runs before anyone signs in, comes with the tray program.

## Sharing a radio with WSJT-X and other programs

The Agent can be a virtual radio for programs that speak Hamlib's `rigctld`
network protocol, such as WSJT-X, VarAC and fldigi. They connect to the
Agent, and the Agent stays the radio's one owner (Principle IX).

```sh
loglab-agent serve RADIO --listen 127.0.0.1:4532
loglab-agent serve --civ COM5 --allow-tuning      # a real IC-7300: tune, no transmit
loglab-agent serve --civ COM5 --allow-transmit    # a real IC-7300: tune and key it
loglab-agent serve --civ-pretend --allow-transmit # the pretend IC-7300
```

In VarAC, open Settings > RIG and choose **Hamlib** for both PTT and
Frequency Control, with Host `localhost` and Port `4532`. Close any other
program that has the radio's COM port open first: the Agent is its one owner.

In WSJT-X, open Settings > Radio and set **Rig** to `Hamlib NET rigctl` and
**Network Server** to `127.0.0.1:4532` (the default if `--listen` is left
out). WSJT-X then follows the radio's frequency and mode.

How it behaves:

- **Read-only unless tuning is allowed.** Without `--allow-tuning`, every
  request to tune or change mode is refused with Hamlib's "command rejected
  by the rig" (`RPRT -9`). WSJT-X needs to tune: it sets the frequency as
  soon as it connects, and stops with a rig error when refused. So for
  WSJT-X, use `--allow-tuning`, which works only with the Agent's own CI-V
  driver (`--civ PORT` or `--civ-pretend`). Allowing it is the operator's
  choice at the radio's computer, on the command line, until the tray program
  decides which program controls each radio.
- **Transmits only with `--allow-transmit`.** Without it, every request to
  transmit is refused. With it (CI-V driver only, and it allows tuning too),
  a program's PTT request keys the radio (CI-V `1C 00 01`), and in WSJT-X or
  VarAC the PTT method is then CAT or Hamlib. A request to **stop**
  transmitting (`T 0`, which WSJT-X sends whenever it closes its connection)
  is always honored, never refused. Failing safe means not transmitting
  (Principle IX):
  - **One program at a time.** While one program has the radio keyed, a
    request to transmit from another is refused; any program may stop it.
  - **The radio's own PTT wins.** If the radio is already transmitting when
    a program asks (the microphone's PTT is pressed), the request is refused.
    A request to stop is never passed to a radio the Agent didn't key, so it
    never cuts the operator off.
  - **A program that disconnects while keyed is unkeyed** at once, crashed or
    closed.
  - **A time limit.** After 60 seconds keyed at once (`--tx-limit SECONDS`,
    1 to 600), the Agent unkeys the radio and says why.
  - **If unkeying fails,** the Agent says the radio may still be
    transmitting, refuses every request to transmit, and tries again every
    second until it is unkeyed. Stopping the Agent unkeys too.
  - **Never on its own.** The Agent keys a radio only when a program asks:
    never on a timer, as a retry, or after a reconnect.
  - It also refuses to key a radio that can't say whether it is
    transmitting.

  The radio's own transmit time-out timer is still the last line of defense:
  turn it on. Transmit audio does not go through the Agent yet, so the
  program's sound output goes straight to the radio's USB sound device.
- **A band the radio doesn't have** (2 m or 70 cm on an IC-7300) is refused
  by the radio itself. WSJT-X then shows "Rig Control Error" and reconnects,
  as it does with any radio. Delete those bands from WSJT-X's Settings >
  Frequencies for this radio's configuration.
- **When something is refused, the window says how to fix it,** on a line
  starting `->` under the refusal, such as which WSJT-X setting to change.
- **This computer only.** The protocol has no sign-in, so the Agent listens
  only on `127.0.0.1` and refuses connections from anywhere else
  (Principle XXI).
- **What it shows.** While it runs, `serve` prints what the radio says,
  whenever its frequency, mode or PTT changes, and beside it every change a
  program asked for and what became of it, each with the UTC time:

  ```
  21:38:26Z  a program connected (127.0.0.1:55526)
  21:38:26Z  asked to tune to 7.074000 MHz: done
  21:38:26Z  radio: 7.074000 MHz DATA-USB, receiving
  21:38:28Z  asked to transmit: transmitting (127.0.0.1:55526)
  21:38:28Z  radio: 7.074000 MHz DATA-USB, transmitting
  21:38:41Z  asked to stop transmitting: unkeyed after 12.7 s; peak Po 50%, ALC 16% of its scale, SWR 1.3
  ```

  The `radio:` lines are read from the radio itself, so if a program's
  display ever disagrees with the latest one, the program is showing what it
  asked for rather than what the radio did (Principle X).
- **Transmit meters.** While a program has the radio keyed, the Agent reads
  its Po, SWR and ALC meters four times a second and, when it unkeys, says
  the highest of each. Po and ALC are percent of the meter's scale; SWR is
  the ratio. Over a third of the ALC scale (data modes want little or no ALC)
  or SWR over 2.0, it adds a `->` line saying what to change. Only radios
  whose rig definition has a `[meters]` table are read; the IC-7300's comes
  from Icom's CI-V guide (`15 11`, `15 12`, `15 13`). Reading the meters only
  reads: it never keys, unkeys or changes anything (Principle X). A
  transmission shorter than a quarter second has no meter line.

  Under `station`, the Agent also offers to move the radio's drive level
  (on the IC-7300, USB MOD Level, since the Agent's transmit audio reaches
  the radio over USB; the rig definition's `meters.drive` names it) one
  step of 10 points:

  - **lower** when ALC ran over a third of its scale;
  - **raise** when ALC stayed at 10% or under and Po fell short of 70% of
    the power the radio is set to (`meters.power_setting`, RF power on the
    IC-7300). It never raises for a transmission under a second, or for one
    whose Po stayed under 5%, which is silence rather than too little drive.

  ```
  ALC peaked at 74% of its scale: lower USB MOD Level from 128 of 255 (50%) to 102 of 255 (40%).
  -> Type yes to set it, then transmit again to see the new meters; anything else leaves the radio as it is. (question 1)
  ```

  A step never goes back past a level already tried while the Agent runs:
  if 40% ran ALC too high and 30% fell short, the next step is halfway, 35%.

  **The radio's own tuner.** While the radio's antenna tuner is tuning (a
  frequency change with PTT tune on, or the TUNE button), its carrier, not
  the program's audio, moves the meters. The Agent asks the tuner's state
  with each reading (`1C 01` on Icom radios: 02 while tuning, per Hamlib
  and wfview), says so on the unkey line, and moves nothing for that
  transmission.

  `yes` writes it and reads it back; anything else leaves the radio as it
  is (Principle X). With auto mode on (below), the Agent sets it without
  asking and says so. Transmit again and repeat until neither `->` line
  appears. The Agent never transmits to test it: the next transmission is
  always one a program asked for.
- **Several programs at once.** Up to eight can connect; readings are
  shared, so they don't crowd the radio.
- Any radio the Agent can read can be served: `--civ PORT`, `--civ-pretend`, `--pretend`,
  `--flex`, or a Hamlib radio. Hamlib's own network client, the one WSJT-X
  uses, has been checked against it with Hamlib 4.5.5.

## Receiving a radio's audio (Windows)

The Agent can take a radio's receive audio from its USB sound device and
play it into a virtual sound cable, so programs like WSJT-X take their
audio from the cable and never open the radio's sound device themselves.
The cable is VB-Audio's free **VB-CABLE**, installed separately from
<https://vb-audio.com/Cable/>. It adds two devices: "CABLE Input", which the
Agent plays into, and "CABLE Output", which programs record from.

1. See the sound devices, and find the radio's (an IC-7300's is
   "Microphone (USB Audio CODEC)", sometimes with a number):

   ```sh
   loglab-agent audio-devices
   ```

2. Start receiving, naming part of the radio's sound device:

   ```sh
   loglab-agent rx-audio --from "USB Audio CODEC"
   ```

3. In WSJT-X, Settings > Audio > **Input**: `CABLE Output (VB-Audio Virtual
   Cable)`.

Every two seconds the Agent shows how the audio is doing:

```
11:02:14Z  radio audio  -18 dBFS peak | buffer 96 ms | no gaps
```

- **dBFS peak** is how loud the radio's audio is, 0 being the loudest a
  sound card can carry. Somewhere around -30 to -10 decodes well. When it's
  silent, close to clipping, or very quiet, a line starting `->` says what to
  change on the radio (on an IC-7300, ACC/USB AF Output Level and ACC/USB
  Output Select).
- **buffer** is how much audio is waiting between the radio and the cable.
  The radio's sound chip and the cable keep slightly different time, and
  the Agent keeps this near 100 ms by reading very slightly faster or slower,
  so there are no glitches.
- **gaps** are moments the cable had nothing to play. A few at the start are
  normal; steady gaps mean the radio's audio is stalling.

**Receive only.** Audio played into a radio can key it (through VOX, or data
mode with USB audio), so `rx-audio` plays only into a VB-Audio virtual
cable and refuses every other device, the radio's own "Speakers (USB Audio
CODEC)" included. Sending a program's audio to the radio comes with
transmitting, for the program checked in the tray program. If a name matches
more than one device (two radios are both "USB Audio CODEC"), it lists them
and asks for more of the name.

## A radio's settings record

The first time the Agent meets a radio, it reads every setting the radio's
rig definition lists and keeps them as the radio's **as-received record**:
when (UTC), on which computer, the radio's serial number, and every value,
with the exact bytes the radio sent. For the IC-7300 that is every menu item
(0001 to 0216) and the front-panel controls: filters, NR, NB, RF gain, AGC,
preamp, attenuator, RF power and the rest, 259 settings in all. It never
overwrites that record. Reading the radio again compares it against the
record and lists what has changed since the radio arrived.

`station` does this by itself, keeping records in `records` beside the
Agent, one per radio by its serial number. To do it on its own:

```sh
loglab-agent record --civ COM5 --records FOLDER
loglab-agent record --civ-pretend --records FOLDER
```

The report also names any setting that stops a job from working, in plain
words. For example, with DATA MOD on MIC, WSJT-X and VARA would transmit
silence. It never changes a setting: nothing in the definition is verified on
a radio yet. Records are JSON files, one per radio, written whole and renamed
into place.

**Fixing what stops a job working.** When a setting stops a job working,
such as DATA MOD on MIC so that VARA would transmit silence, `station` says
so. If the definition offers a value for it, and the Agent may change that
setting and a real radio took it written back, the Agent lists the fixes and
waits: type `yes` on the next line to set them, and anything else leaves the
radio as it is (Principle X). Each fix is read again after it is written and
recorded like a restore (`restore-...json`). Value codes not yet checked on a
real radio are said to be so, so you can look at the radio's menu afterwards.

**Reading one setting.** Type `show DATA MOD` (any setting's menu name, or
part of one: `show PBT`) to read it from the radio now. Type `fix` to see
again what the Agent can fix, and say `yes` to it.

**Auto mode.** Type `auto on` to let the Agent make its fixes without
asking: what stops a job working (at start-up, or when you type `fix`), and
lowering the drive level when ALC ran high. Each change is shown, starting
`Auto mode:`, and recorded like any other. It changes nothing while a
program has the radio keyed and never keys the radio to try a change.
`auto off` goes back to asking; `auto` says which it is. Auto mode is off
until turned on, and kept for this computer only, in `auto-mode.txt` in the
records folder (Principle X, third exception, constitution 6.0.0).

### Setups kept on the Engine

With `--engine URL --key-file FILE`, `station` also keeps the radio's
settings on the Engine, by the radio's maker, model and USB serial number,
so it doesn't matter which FieldLab computer the radio is plugged into:

- It sends the radio's as-received record. The Engine keeps the first one
  any computer sends for that radio and never replaces it.
- It lists, for each **setup** kept for the radio, every setting where the
  radio differs, in plain words where the definition says what the bytes
  mean.
- Once the radio is set up for a job, type `keep FT8` (any name up to 40
  characters) in the Agent's window. It reads every setting again and keeps
  them on the Engine as setup FT8, with who was signed in at the station.
  Keeping a name again keeps a newer version; the Engine keeps the old one
  too. `compare` reads the radio again and lists what differs from each
  setup. `help` says this.

**With no Engine.** Without `--engine`, `keep`, `compare` and `restore` work
the same way with setups kept on this computer, in the `records` folder
beside the Agent, one file per version (`setup-…json`), written whole and
never overwritten; the newest of each name is used (Principle V). The Agent
says at start-up where setups are kept. Setups kept this way stay on this
computer: for a radio's setups to follow it to another computer, run with
`--engine`.

**Putting a setup back.** Type `restore FT8`. The Agent reads the radio
again and lists what putting setup FT8 back would change, from what to what,
and which settings that differ it would leave alone, and why. Nothing changes
until you type `yes` on the next line; anything else cancels (Principle X:
the Agent restores only when asked, and only after the operator confirms).
It then reads each setting just before writing it and again after: what the
radio says afterward is what counts. It records the restore, each setting
from what, to what, and what the radio then had, in a `restore-...json` file
in `records`, one per restore, never overwritten.

**Mode and filter first.** Some settings belong to whichever mode and filter
the radio is on: on the IC-7300 the selected filter's width, its AGC time
constant, the SSB TX bandwidth and Twin PBT (`per_selection` in the
definition). A setup keeps the mode and filter (FIL1, FIL2 or FIL3, read with
CI-V command 04), and a restore puts them back first (command 06), then reads
those settings again and puts them back. If the mode and filter can't be put
back, or the setup doesn't say which they were (a setup kept before this),
those settings are left alone: written now, they would land on another filter.
Any setting that comes out different from the setup is written once more at
the end, since one setting can depend on another; if it still differs, the
Agent says so.

It only ever writes a setting its rig definition says may be changed and
that a **real radio took** when `check-writes` wrote its own value back
(`write_checked` in the definition). On the IC-7300 that is 247 of the 248
changeable settings, the operating mode and filter among them, from the
FieldLab runs on 2026-10-27; the one left, TX delay for 70 MHz, the radio
won't read. Settings the Agent never changes
(the CI-V and USB link settings, the clock) stay as they are.

**Finding out which settings can be set.** `loglab-agent check-writes`
(with `station` stopped, since only one program may own the radio) finds the
radio, asks for a `yes`, then for each setting the Agent may change it reads
the value, writes exactly that value back, and reads it again. The radio ends
as it began: nothing remembered is written (Principle X). It reports how many
the radio took and lists the ones it refused or changed, and keeps the
report as a `write-check-...json` file in `records`. A setting whose own
value the radio took is one whose command works, which is the evidence for
marking it verified in the rig definition.

Reading every setting takes a moment, and is refused while a program has the
radio keyed. The radio's serial number comes with its USB serial port, so
this needs the Agent to find the port itself (no `--civ`).

The station needs a key, as for `run`: an administrator makes one with
`lablog-operator agent-key STATION --write FILE`, or add `--station NAME`
and the Agent asks to join, as `run` does. `startup on` doesn't pass
`--engine` yet.

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
