// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LabLog Station Agent.
//!
//! The Agent owns the radios: it is the only component that opens a radio's
//! port or control connection, it presents every radio through one common rig
//! model, and it is the only code that can key a transmitter. See
//! `CONSTITUTION.md` at the repository root.
//!
//! This version reads one radio, through Hamlib, a FlexRadio's own API or
//! the built-in pretend radio, and streams its live state to the Engine. It never tunes the radio
//! and never keys it. With `--wsjtx`, it also hears WSJT-X log QSOs on this
//! computer and hands them to the Engine, through its journal.

// The resampler is used on Windows, and tested on every platform.
#[cfg(any(windows, test))]
mod audio;
#[cfg(windows)]
mod audio_windows;
mod auto_mode;
mod carrier;
mod cat_pretend;
mod cat_radio;
mod civ;
mod civ_demo;
mod civ_pretend;
mod civ_radio;
mod civ_serial;
#[cfg(any(windows, test))]
mod com0com;
#[cfg(any(windows, test))]
mod driver;
mod engine;
mod enrollment;
mod filter_widths;
#[cfg(any(windows, test))]
mod fldigi;
mod flex;
mod flex_pretend;
mod flex_settings;
mod hamlib;
#[cfg(any(windows, test))]
mod ini;
mod journal;
#[cfg(any(windows, test))]
mod kenwood_cat;
mod lab_stock;
#[cfg(any(windows, test))]
mod mmsstv;
mod mqtt;
mod pairing;
#[cfg(test)]
mod pe_exports;
mod pretend;
mod profiles;
mod program_id;
#[cfg(windows)]
mod programs;
mod qso_logs;
#[cfg(any(windows, test))]
mod qt_settings;
mod restore;
mod rig;
mod rigctld_server;
mod rigdef;
mod sends_as;
mod serial_radio;
#[cfg(any(windows, test))]
mod settings_file;
mod settings_record;
mod setups;
mod station;
mod tray;
#[cfg(windows)]
mod tray_windows;
#[cfg(any(windows, test))]
mod vara;
#[cfg(any(windows, test))]
mod vara_proxy;
#[cfg(any(windows, test))]
mod winlink;
mod write_check;
mod wsjtx;

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use cat_pretend::{PretendCat, PretendCatPort};
use cat_radio::CatRadio;
use civ_pretend::PretendCiv;
use civ_radio::{CivRadio, PretendPort};
use engine::{Handed, RadioReport, Report, SendError, Uplink};
use flex::FlexRadio;
use flex_pretend::PretendFlex;
use hamlib::{HamlibRadio, Launch, Source};
use journal::Journal;
use pretend::PretendRadio;
use rig::{Radio, Reading};
use wsjtx::Listener;

/// The Agent's version, from `Cargo.toml`.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How often the radio is read.
const POLL: Duration = Duration::from_millis(250);
/// How often the Engine hears from the Agent when nothing changes.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// How long to wait before trying an unreachable radio again.
const RETRY: Duration = Duration::from_secs(3);
/// How long QSOs wait in the journal before the Engine is tried again.
const QSO_RETRY: Duration = Duration::from_secs(15);
/// How long a program may keep the radio keyed at once, unless
/// `--tx-limit` says otherwise.
const TX_LIMIT: Duration = Duration::from_mins(1);
/// The longest `--tx-limit` accepted, in seconds.
const MAX_TX_LIMIT: u64 = 600;

const USAGE: &str = "\
Usage:
  loglab-agent run  --engine URL --key-file FILE [--station NAME]
                    [--wsjtx 127.0.0.1:2237 [--journal FILE]] RADIO
  loglab-agent read RADIO              read the radio once and print it
  loglab-agent record RADIO --records FOLDER
                                       read every menu setting the radio's
                                       definition lists; keep the first
                                       reading as its as-received record,
                                       and compare later ones against it
                                       (--civ, --cat, --flex, or their
                                       -pretend, only)
  loglab-agent serve RADIO [--listen 127.0.0.1:4532] [--allow-tuning]
                    [--allow-transmit [--tx-limit SECONDS]]
                                       be a virtual radio for programs like
                                       WSJT-X (Rig: Hamlib NET rigctl), on
                                       this computer only; read-only unless
                                       --allow-tuning; --allow-transmit also
                                       lets a program key the radio, one at
                                       a time, for at most 60 seconds at
                                       once (--tx-limit, up to 600); both
                                       --civ, --cat, --flex, or their
                                       -pretend, only
  loglab-agent station [--civ PORT | --civ-pretend] [--listen 127.0.0.1:4532]
                    [--tx-limit SECONDS]
                    [--engine URL --key-file FILE [--station NAME]]
                    [--mqtt HOST[:PORT]] [--grid GRID] [--call CALL]
                                       everything programs on this computer
                                       need to use the radio: finds it and
                                       its port, checks its settings, carries
                                       its audio both ways (Windows), sets
                                       VARA's sound devices, and lets
                                       programs like VarAC and WSJT-X tune
                                       and key it (as serve --allow-transmit)
                                       (it waits for the radio if it is off);
                                       with --engine, keeps the radio's
                                       settings there for every computer:
                                       type keep NAME to keep a setup;
                                       sets the call and grid square
                                       WSJT-X, JS8Call and fldigi send:
                                       the grid from FieldLab's GPS on its
                                       MQTT broker (--mqtt, or
                                       $LABLOG_MQTT, else 10.73.10.10;
                                       --mqtt none reads none), else
                                       --grid, else
                                       LabLog's; the call from LabLog,
                                       else --call;
                                       quit, or its input closing, stops it
  loglab-agent tray [station's options]
                                       station in the background, with an
                                       icon by the clock instead of a
                                       window: its menu does what is typed
                                       at station, and a Yes/No box asks
                                       before the radio is changed
                                       (Windows only)
  loglab-agent check-writes [--civ PORT | --flex ADDRESS]
                                       find out which of the radio's
                                       settings the Agent can set: writes
                                       each one's own value back and reads
                                       it again, so nothing changes; asks
                                       first
  loglab-agent startup on [station's options] | off | status
                                       whether Windows starts the tray each
                                       time you sign in, and with which
                                       options (--engine, say)
  loglab-agent audio-devices           list this computer's sound devices
  loglab-agent rx-audio --from RADIO-SOUND [--to CABLE]
                                       receive: play the radio's audio into
                                       a VB-Audio virtual cable for programs
                                       like WSJT-X (Windows only; nothing is
                                       ever played to the radio)
  loglab-agent check-rig PATH...       check rig definition files (a file,
                                       or every .toml file in a folder)
  loglab-agent civ-demo                show a CI-V conversation with the
                                       pretend IC-7300, byte by byte

RADIO is one of:
  --pretend                            the built-in pretend radio
  --civ PORT                           an Icom radio on a serial port, such
                                       as COM5 or /dev/ttyUSB0, through the
                                       Agent's own CI-V driver; it finds the
                                       speed and the radio by itself
  --civ-pretend                        the same driver, talking to a pretend
                                       IC-7300
  --cat PORT                           a Yaesu radio (FTDX10) on a serial
                                       port, through the Agent's own CAT
                                       driver; RTS is held on, as Yaesu's
                                       CAT port uses it for flow control
  --cat-pretend                        the same driver, talking to a pretend
                                       FTDX10
  --hamlib-model N --device PORT [--baud N] [--rigctld-program PATH]
                                       start Hamlib's rigctld for a radio
                                       (3073 is the Icom IC-7300)
  --hamlib-model N --rigctld 127.0.0.1:PORT
                                       use a rigctld already running here
  --flex ADDRESS                       a FlexRadio on the network, by its IP
                                       address or name (port 4992 unless
                                       given, as in 192.168.1.20:4992),
                                       through its SmartSDR API; serve may
                                       tune and key its transmit slice
  --flex-pretend                       the same driver, talking to a pretend
                                       FLEX-6600 on this computer

With --station, an Agent whose key file is empty or missing asks to join as
that station, shows a code, and waits for an administrator to approve it in
LabLog (Menu > Station Agents); the key is then saved in the key file.

With --wsjtx, the Agent listens on this computer for WSJT-X (or JTDX) and
logs each QSO it logs, as the operator signed in to LabLog at this station.
Set WSJT-X's Settings > Reporting > UDP Server to the same address. QSOs wait
in the journal (the key file's name plus .qsos unless --journal says) until
the Engine has them.

The Agent only reads the radio. It never tunes it and never transmits.";

/// Returns the line the Agent prints to identify itself.
fn banner() -> String {
    format!("loglab-agent {VERSION}")
}

/// Which radio to read.
#[derive(Debug, PartialEq, Eq)]
enum RadioChoice {
    Pretend,
    CivPretend,
    /// The Agent's own CI-V driver, on a real serial port (`COM5`).
    Civ(String),
    /// The Agent's own CAT driver, talking to a pretend FTDX10.
    CatPretend,
    /// The Agent's own CAT driver, on a real serial port.
    Cat(String),
    Hamlib(HamlibChoice),
    Flex(String),
    /// The FlexRadio driver, talking to a pretend FlexRadio.
    FlexPretend,
}

#[derive(Debug, PartialEq, Eq)]
enum HamlibChoice {
    Start(Launch),
    Running { address: SocketAddr, model: u32 },
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Read(RadioChoice),
    CheckRig(Vec<String>),
    CivDemo,
    AudioDevices,
    RxAudio {
        from: String,
        to: String,
    },
    Serve {
        radio: RadioChoice,
        listen: SocketAddr,
        allow_tuning: bool,
        /// Programs may key the radio, for at most this long at once.
        transmit_limit: Option<Duration>,
    },
    Record {
        radio: RadioChoice,
        records: String,
    },
    Station {
        port: Option<String>,
        /// The pretend IC-7300 in place of a radio.
        pretend: bool,
        listen: SocketAddr,
        limit: Duration,
        /// The Engine's address, the station key's file, and the station to
        /// ask to join as if it has no key, to keep the radio's settings
        /// there.
        engine: Option<(String, String, Option<String>)>,
        /// A COM port to answer on as a Kenwood TS-2000, for Winlink
        /// Express: one end of a com0com pair. Without it, where Winlink
        /// Express is installed, the Agent finds a com0com pair itself.
        kenwood: Option<String>,
        /// FieldLab's bus, and the call and grid the programs send where
        /// nothing better is known (see `sends_as`).
        stated: sends_as::Stated,
    },
    /// `on`, `off` or `status`, and for `on`, the `station` options the
    /// tray runs it with.
    Startup(String, Vec<String>),
    /// The tray: `station`, with these options, in the background.
    Tray(Vec<String>),
    /// Write each changeable setting's own value back, on this port or
    /// whichever the radio is on.
    CheckWrites(Option<String>),
    /// `check-writes --flex ADDRESS`.
    CheckWritesFlex(String),
    Run {
        engine: String,
        key_file: String,
        station: Option<String>,
        wsjtx: Option<Wsjtx>,
        radio: RadioChoice,
    },
}

/// Where to hear WSJT-X, and where to keep its QSOs until the Engine has them.
#[derive(Debug, PartialEq, Eq)]
struct Wsjtx {
    address: SocketAddr,
    journal: String,
}

/// What programs connected to `serve` may do to the radio, beyond reading it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Allowed {
    #[default]
    Reading,
    Tuning,
    Transmitting,
}

impl Allowed {
    fn flag(self) -> &'static str {
        match self {
            Self::Reading => "",
            Self::Tuning => "--allow-tuning",
            Self::Transmitting => "--allow-transmit",
        }
    }
}

#[derive(Default)]
struct Options {
    engine: Option<String>,
    key_file: Option<String>,
    station: Option<String>,
    /// The pretend radios asked for.
    pretends: Vec<RadioChoice>,
    civ: Option<String>,
    cat: Option<String>,
    model: Option<u32>,
    device: Option<String>,
    baud: Option<u32>,
    program: Option<String>,
    rigctld: Option<SocketAddr>,
    flex: Option<String>,
    wsjtx: Option<SocketAddr>,
    journal: Option<String>,
    records: Option<String>,
    listen: Option<SocketAddr>,
    allowed: Allowed,
    tx_limit: Option<u64>,
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} needs a number, not {value:?}"))
}

fn parse(args: &[String]) -> Result<Command, String> {
    let Some((verb, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    if matches!(verb.as_str(), "help" | "--help" | "-h" | "--version") {
        return Ok(Command::Help);
    }
    if verb == "audio-devices" {
        return if rest.is_empty() {
            Ok(Command::AudioDevices)
        } else {
            Err("audio-devices takes no options".to_owned())
        };
    }
    if verb == "rx-audio" {
        return rx_audio_command(rest);
    }
    if verb == "station" {
        return station_command(rest);
    }
    if verb == "tray" {
        station_command(rest)?;
        return Ok(Command::Tray(rest.to_vec()));
    }
    if matches!(verb.as_str(), "check-writes" | "startup") {
        return station_tool_command(verb, rest);
    }
    if verb == "civ-demo" {
        return if rest.is_empty() {
            Ok(Command::CivDemo)
        } else {
            Err("civ-demo takes no options".to_owned())
        };
    }
    if verb == "check-rig" {
        if rest.is_empty() {
            return Err("check-rig needs a definition file or a folder of them".to_owned());
        }
        return Ok(Command::CheckRig(rest.to_vec()));
    }
    let mut options = Options::default();
    let mut words = rest.iter();
    while let Some(flag) = words.next() {
        if let Some(pretend) = pretend_flag(flag) {
            if !options.pretends.contains(&pretend) {
                options.pretends.push(pretend);
            }
            continue;
        }
        if flag == "--allow-tuning" {
            options.allowed = options.allowed.max(Allowed::Tuning);
            continue;
        }
        if flag == "--allow-transmit" {
            options.allowed = Allowed::Transmitting;
            continue;
        }
        let value = words
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag.as_str() {
            "--engine" => options.engine = Some(value),
            "--key-file" => options.key_file = Some(value),
            "--station" => options.station = Some(value),
            "--hamlib-model" => options.model = Some(number(flag, &value)?),
            "--device" => options.device = Some(value),
            "--baud" => options.baud = Some(number(flag, &value)?),
            "--rigctld-program" => options.program = Some(value),
            "--flex" => options.flex = Some(value),
            "--civ" => options.civ = Some(value),
            "--cat" => options.cat = Some(value),
            "--journal" => options.journal = Some(value),
            "--records" => options.records = Some(value),
            "--tx-limit" => options.tx_limit = Some(number(flag, &value)?),
            "--listen" => {
                options.listen = Some(value.parse().map_err(|_| {
                    format!("--listen needs an address like 127.0.0.1:4532, not {value:?}")
                })?);
            }
            "--wsjtx" => {
                let address: SocketAddr = value.parse().map_err(|_| {
                    format!("--wsjtx needs an address like 127.0.0.1:2237, not {value:?}")
                })?;
                wsjtx::check_local(address)?;
                options.wsjtx = Some(address);
            }
            "--rigctld" => {
                options.rigctld = Some(value.parse().map_err(|_| {
                    format!("--rigctld needs an address like 127.0.0.1:4532, not {value:?}")
                })?);
            }
            _ => return Err(format!("unknown option {flag:?}")),
        }
    }
    let radio = radio_choice(&mut options)?;
    command(verb, options, radio)
}

/// `station [--civ PORT] [--listen ADDRESS] [--tx-limit SECONDS] [--kenwood PORT]
/// [--mqtt HOST[:PORT]] [--grid GRID] [--call CALL]`.
fn station_command(rest: &[String]) -> Result<Command, String> {
    let mut port = None;
    let mut pretend = false;
    let mut listen = SocketAddr::from(([127, 0, 0, 1], 4532));
    let mut limit = TX_LIMIT;
    let mut engine = None;
    let mut key_file = None;
    let mut station = None;
    let mut kenwood = None;
    let mut stated = sends_as::Stated::default();
    let mut words = rest.iter();
    while let Some(flag) = words.next() {
        if flag == "--civ-pretend" {
            pretend = true;
            continue;
        }
        let value = words
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag.as_str() {
            "--civ" => port = Some(value),
            "--listen" => {
                listen = value.parse().map_err(|_| {
                    format!("--listen needs an address like 127.0.0.1:4532, not {value:?}")
                })?;
            }
            "--tx-limit" => limit = tx_limit(number(flag, &value)?)?,
            "--engine" => engine = Some(value),
            "--key-file" => key_file = Some(value),
            "--station" => station = Some(value),
            "--kenwood" => kenwood = Some(value),
            "--mqtt" if value == "none" => stated.no_bus = true,
            "--mqtt" => stated.mqtt = Some(sends_as::broker(&value)?),
            "--grid" => {
                stated.grid = Some(sends_as::checked_grid(&value).ok_or_else(|| {
                    format!(
                        "--grid needs a 4- or 6-character grid square like EM18ct, not {value:?}"
                    )
                })?);
            }
            "--call" => {
                stated.call = Some(
                    sends_as::checked_call(&value)
                        .ok_or_else(|| format!("--call needs a call like W0CY, not {value:?}"))?,
                );
            }
            _ => return Err(format!("unknown option {flag:?} for station")),
        }
    }
    let engine = match (engine, key_file, station) {
        (Some(engine), Some(key_file), station) => Some((engine, key_file, station)),
        (None, None, None) => None,
        _ => {
            return Err(
                "station takes --engine and --key-file together, and --station only with them"
                    .to_owned(),
            );
        }
    };
    if pretend && port.is_some() {
        return Err("station takes --civ PORT or --civ-pretend, not both".to_owned());
    }
    Ok(Command::Station {
        port,
        pretend,
        listen,
        limit,
        engine,
        kenwood,
        stated,
    })
}

/// `check-writes [--civ PORT]` and `startup on|off|status`.
fn station_tool_command(verb: &str, rest: &[String]) -> Result<Command, String> {
    match (verb, rest) {
        ("check-writes", []) => Ok(Command::CheckWrites(None)),
        ("check-writes", [flag, address]) if flag == "--flex" => {
            Ok(Command::CheckWritesFlex(address.clone()))
        }
        ("check-writes", [flag, port]) if flag == "--civ" => {
            Ok(Command::CheckWrites(Some(port.clone())))
        }
        ("check-writes", _) => {
            Err("check-writes takes only --civ PORT or --flex ADDRESS".to_owned())
        }
        ("startup", [what]) if matches!(what.as_str(), "on" | "off" | "status") => {
            Ok(Command::Startup(what.clone(), Vec::new()))
        }
        ("startup", [what, options @ ..]) if what == "on" => {
            station_command(options)?;
            Ok(Command::Startup(what.clone(), options.to_vec()))
        }
        _ => Err("startup takes one of: on, off, status (on may add station's options)".to_owned()),
    }
}

/// A `--tx-limit` in seconds, checked.
fn tx_limit(seconds: u64) -> Result<Duration, String> {
    if (1..=MAX_TX_LIMIT).contains(&seconds) {
        Ok(Duration::from_secs(seconds))
    } else {
        Err(format!(
            "--tx-limit is in seconds, 1 to {MAX_TX_LIMIT}, not {seconds}"
        ))
    }
}

/// `rx-audio --from NAME [--to NAME]`.
fn rx_audio_command(rest: &[String]) -> Result<Command, String> {
    let mut from = None;
    let mut to = None;
    let mut words = rest.iter();
    while let Some(flag) = words.next() {
        let value = words
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag.as_str() {
            "--from" => from = Some(value),
            "--to" => to = Some(value),
            _ => return Err(format!("unknown option {flag:?} for rx-audio")),
        }
    }
    Ok(Command::RxAudio {
        from: from.ok_or(
            "rx-audio needs --from, part of the radio's sound device name, such as \"USB Audio CODEC\"",
        )?,
        to: to.unwrap_or_else(|| "CABLE Input".to_owned()),
    })
}

/// The command a verb names, from options already read and a radio already
/// chosen.
fn command(verb: &str, options: Options, radio: RadioChoice) -> Result<Command, String> {
    if options.journal.is_some() && options.wsjtx.is_none() {
        return Err("--journal goes with --wsjtx".to_owned());
    }
    if options.records.is_some() && verb != "record" {
        return Err("--records goes with record".to_owned());
    }
    if options.listen.is_some() && verb != "serve" {
        return Err("--listen goes with serve".to_owned());
    }
    let allowed = options.allowed;
    if allowed != Allowed::Reading && verb != "serve" {
        return Err(format!("{} goes with serve", allowed.flag()));
    }
    if options.tx_limit.is_some() && allowed != Allowed::Transmitting {
        return Err("--tx-limit goes with --allow-transmit".to_owned());
    }
    if allowed != Allowed::Reading && !radio.is_own_driver() {
        return Err(format!(
            "{} works only with the Agent's own drivers (--civ PORT, --cat PORT, --flex ADDRESS, or their -pretend) for now",
            allowed.flag()
        ));
    }
    let transmitting = allowed == Allowed::Transmitting;
    let transmit_limit = match (transmitting, options.tx_limit) {
        (false, _) => None,
        (true, None) => Some(TX_LIMIT),
        (true, Some(seconds)) => Some(tx_limit(seconds)?),
    };
    match verb {
        "read" | "record" | "serve" if options.wsjtx.is_some() => {
            Err("--wsjtx goes with run".to_owned())
        }
        "serve" => Ok(Command::Serve {
            radio,
            listen: options
                .listen
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 4532))),
            // A program that may key the radio may tune it too.
            allow_tuning: allowed >= Allowed::Tuning,
            transmit_limit,
        }),
        "record" => Ok(Command::Record {
            radio,
            records: options
                .records
                .ok_or("record needs --records, the folder settings records are kept in")?,
        }),
        "read" => Ok(Command::Read(radio)),
        "run" => {
            let key_file = options
                .key_file
                .ok_or("run needs --key-file, the file holding this station's key")?;
            let wsjtx = options.wsjtx.map(|address| Wsjtx {
                address,
                journal: options
                    .journal
                    .unwrap_or_else(|| format!("{key_file}.qsos")),
            });
            Ok(Command::Run {
                engine: options
                    .engine
                    .ok_or("run needs --engine, such as --engine http://127.0.0.1:8073")?,
                key_file,
                station: options.station,
                wsjtx,
                radio,
            })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// The pretend radio a flag names, if it names one.
fn pretend_flag(flag: &str) -> Option<RadioChoice> {
    Some(match flag {
        "--pretend" => RadioChoice::Pretend,
        "--civ-pretend" => RadioChoice::CivPretend,
        "--cat-pretend" => RadioChoice::CatPretend,
        "--flex-pretend" => RadioChoice::FlexPretend,
        _ => return None,
    })
}

fn radio_choice(options: &mut Options) -> Result<RadioChoice, String> {
    let hamlib = options.model.is_some() || options.device.is_some() || options.rigctld.is_some();
    let chosen = [
        options.civ.is_some(),
        options.cat.is_some(),
        hamlib,
        options.flex.is_some(),
    ];
    if chosen.iter().filter(|c| **c).count() + options.pretends.len() > 1 {
        return Err(
            "choose one radio: --pretend, --civ-pretend, --civ, --cat-pretend, --cat, a Hamlib radio, --flex or --flex-pretend"
                .to_owned(),
        );
    }
    if let Some(pretend) = options.pretends.pop() {
        return Ok(pretend);
    }
    if let Some(port) = options.civ.take() {
        return Ok(RadioChoice::Civ(port));
    }
    if let Some(port) = options.cat.take() {
        return Ok(RadioChoice::Cat(port));
    }
    if let Some(address) = options.flex.take() {
        return Ok(RadioChoice::Flex(address));
    }
    let model = options
        .model
        .ok_or("choose a radio: --pretend, --civ PORT, --flex, or --hamlib-model with --device or --rigctld")?;
    match (options.device.take(), options.rigctld) {
        (Some(device), None) => Ok(RadioChoice::Hamlib(HamlibChoice::Start(Launch {
            program: options
                .program
                .take()
                .unwrap_or_else(|| "rigctld".to_owned()),
            model,
            device,
            baud: options.baud,
        }))),
        (None, Some(address)) => Ok(RadioChoice::Hamlib(HamlibChoice::Running {
            address,
            model,
        })),
        _ => Err("a Hamlib radio needs exactly one of --device or --rigctld".to_owned()),
    }
}

/// The CI-V driver, finding a pretend IC-7300 that listens at 19200 baud,
/// so the speed search has to work to find it.
fn civ_pretend_radio() -> Result<CivRadio<PretendPort>, rig::RadioError> {
    let definitions = rigdef::built_in().map_err(rig::RadioError)?;
    let ic_7300 = definitions
        .iter()
        .find(|d| d.model == "IC-7300")
        .ok_or_else(|| rig::RadioError("the IC-7300 definition is not built in".to_owned()))?;
    let port = PretendPort::new(PretendCiv::new(ic_7300), 19_200);
    CivRadio::find(port, &definitions)
}

/// The CI-V driver on a real serial port, finding which known radio answers
/// and at what speed.
fn civ_serial_radio(port: &str) -> Result<CivRadio<civ_serial::SerialPort>, rig::RadioError> {
    let definitions = rigdef::built_in().map_err(rig::RadioError)?;
    let port = civ_serial::SerialPort::open(port).map_err(rig::RadioError)?;
    CivRadio::find(port, &definitions)
}

/// The CAT driver, finding a pretend FTDX10 that listens at 9600 baud, so
/// the speed search has to work to find it.
fn cat_pretend_radio() -> Result<CatRadio<PretendCatPort>, rig::RadioError> {
    let definitions = rigdef::built_in().map_err(rig::RadioError)?;
    let ftdx10 = definitions
        .iter()
        .find(|d| d.model == "FTDX10")
        .ok_or_else(|| rig::RadioError("the FTDX10 definition is not built in".to_owned()))?;
    let port = PretendCatPort::new(PretendCat::new(ftdx10), 9600);
    CatRadio::find(port, &definitions)
}

/// The CAT driver on a real serial port: 2 stop bits and RTS on, as the
/// Yaesu definitions ask.
fn cat_serial_radio(port: &str) -> Result<CatRadio<civ_serial::SerialPort>, rig::RadioError> {
    let definitions = rigdef::built_in().map_err(rig::RadioError)?;
    let yaesu = definitions
        .iter()
        .find(|d| d.family == rigdef::YAESU_CAT)
        .ok_or_else(|| rig::RadioError("no Yaesu definition is built in".to_owned()))?;
    let port = civ_serial::SerialPort::open_for(port, yaesu).map_err(rig::RadioError)?;
    CatRadio::find(port, &definitions)
}

impl RadioChoice {
    /// Whether the radio is reached through one of the Agent's own drivers,
    /// the only ones that can tune or key a radio or read its settings: its
    /// serial drivers, and the FlexRadio's.
    fn is_own_driver(&self) -> bool {
        matches!(
            self,
            Self::Civ(_)
                | Self::CivPretend
                | Self::Cat(_)
                | Self::CatPretend
                | Self::Flex(_)
                | Self::FlexPretend
        )
    }
}

/// The FlexRadio driver, talking to a pretend FLEX-6600 on this computer.
fn flex_pretend_radio() -> Result<FlexRadio, rig::RadioError> {
    let pretend = PretendFlex::start("FLEX-6600", true)
        .map_err(|error| rig::RadioError(format!("can't start the pretend FlexRadio: {error}")))?;
    FlexRadio::open(pretend.address())
}

/// The Agent's own driver for the radio chosen, if it is one of them.
fn serial_radio(
    choice: &RadioChoice,
) -> Result<Box<dyn serial_radio::SerialRadio>, rig::RadioError> {
    Ok(match choice {
        RadioChoice::Civ(port) => Box::new(civ_serial_radio(port)?),
        RadioChoice::CivPretend => Box::new(civ_pretend_radio()?),
        RadioChoice::Cat(port) => Box::new(cat_serial_radio(port)?),
        RadioChoice::CatPretend => Box::new(cat_pretend_radio()?),
        RadioChoice::Flex(address) => Box::new(FlexRadio::open(address)?),
        RadioChoice::FlexPretend => Box::new(flex_pretend_radio()?),
        _ => {
            return Err(rig::RadioError(
                "only the Agent's own drivers (CI-V, CAT and FlexRadio) can tune or key a radio or read its settings"
                    .to_owned(),
            ));
        }
    })
}

/// The Agent's own drivers, the only ones that can tune or key a radio.
fn keyable(choice: &RadioChoice) -> Result<Box<dyn rig::Keyable + Send>, rig::RadioError> {
    Ok(serial_radio(choice)?)
}

fn open(choice: &RadioChoice) -> Result<Box<dyn Radio + Send>, rig::RadioError> {
    Ok(match choice {
        RadioChoice::Pretend => Box::new(PretendRadio::new()),
        RadioChoice::CivPretend => Box::new(civ_pretend_radio()?),
        RadioChoice::Civ(port) => Box::new(civ_serial_radio(port)?),
        RadioChoice::CatPretend => Box::new(cat_pretend_radio()?),
        RadioChoice::Cat(port) => Box::new(cat_serial_radio(port)?),
        RadioChoice::Flex(address) => Box::new(FlexRadio::open(address)?),
        RadioChoice::FlexPretend => Box::new(flex_pretend_radio()?),
        RadioChoice::Hamlib(HamlibChoice::Start(launch)) => {
            Box::new(HamlibRadio::open(&Source::Started(launch.clone()))?)
        }
        RadioChoice::Hamlib(HamlibChoice::Running { address, model }) => {
            Box::new(HamlibRadio::open(&Source::Running {
                address: *address,
                model: *model,
            })?)
        }
    })
}

fn describe(radio: &dyn Radio, reading: &Reading) -> String {
    let identity = radio.identity();
    format!(
        "{} {}: {}",
        identity.maker,
        identity.model,
        reading_text(reading)
    )
}

/// A reading in plain words: "14.074000 MHz DATA-USB, receiving".
fn reading_text(reading: &Reading) -> String {
    let mhz = format!(
        "{}.{:06}",
        reading.frequency_hz / 1_000_000,
        reading.frequency_hz % 1_000_000
    );
    let mode = reading
        .mode
        .and_then(|mode| serde_json::to_value(mode).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "no mode".to_owned());
    let ptt = match reading.ptt {
        Some(true) => "transmitting",
        Some(false) => "receiving",
        None => "PTT unknown",
    };
    format!("{mhz} MHz {mode}, {ptt}")
}

fn read_once(choice: &RadioChoice) -> Result<(), String> {
    let mut radio = open(choice).map_err(|error| error.to_string())?;
    let reading = radio.read().map_err(|error| error.to_string())?;
    println!("{}", describe(radio.as_ref(), &reading));
    Ok(())
}

/// What was last sent, so a change is sent at once and nothing else is resent
/// until the heartbeat.
#[derive(PartialEq, Eq)]
enum Sent {
    Nothing,
    Reading(Reading),
    Problem(String),
}

/// The Agent's link to the Engine, and how its last send went.
struct Link {
    uplink: Uplink,
    answering: bool,
    refused: bool,
}

impl Link {
    /// Send a report, saying once when the Engine stops or starts answering.
    fn deliver(&mut self, report: &Report<'_>) -> bool {
        match self.uplink.send(report) {
            Ok(()) => {
                if !self.answering {
                    println!("the Engine is answering again");
                }
                self.answering = true;
                true
            }
            Err(error) => {
                if self.answering || matches!(error, SendError::Refused(_)) {
                    println!("{error}");
                }
                self.refused = matches!(error, SendError::Refused(_));
                self.answering = false;
                false
            }
        }
    }

    /// Say why the radio can't be read, unless that was just said.
    fn send_problem(
        &mut self,
        agent: &str,
        problem: &str,
        sent: &Sent,
        sent_at: &mut Instant,
    ) -> Sent {
        let already = *sent == Sent::Problem(problem.to_owned());
        if already && sent_at.elapsed() < HEARTBEAT {
            return Sent::Problem(problem.to_owned());
        }
        let report = Report {
            agent,
            radio: None,
            problem: Some(problem),
        };
        if self.deliver(&report) {
            *sent_at = Instant::now();
            Sent::Problem(problem.to_owned())
        } else {
            Sent::Nothing
        }
    }
}

/// The station's key: the one in the key file, or a new one from joining.
fn station_key(
    engine: &str,
    key_file: &str,
    station: Option<&str>,
    running: &AtomicBool,
    fresh: bool,
) -> Result<String, String> {
    if !fresh && let Ok(text) = fs::read_to_string(key_file) {
        let key = text.trim();
        if !key.is_empty() {
            return Ok(key.to_owned());
        }
    }
    let Some(station) = station else {
        return Err(format!(
            "no station key in {key_file}; add --station NAME to ask to join as that station"
        ));
    };
    let key = pairing::join(engine.trim_end_matches('/'), station, &banner(), running)
        .map_err(|error| error.to_string())?;
    pairing::save_key(Path::new(key_file), &key)?;
    println!("joined as station {station}; the key is saved in {key_file}");
    Ok(key)
}

/// What the WSJT-X listener heard, for the main loop.
enum Heard {
    Message(wsjtx::Message),
    Malformed(wsjtx::Malformed),
}

/// Listen for WSJT-X on its own thread until the Agent stops.
fn listen(address: SocketAddr, running: &Arc<AtomicBool>) -> Result<Receiver<Heard>, String> {
    let mut listener = Listener::bind(address)?;
    let (tx, rx) = mpsc::channel();
    let running = Arc::clone(running);
    thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            let heard = match listener.next() {
                None => continue,
                Some(Ok(message)) => Heard::Message(message),
                Some(Err(malformed)) => Heard::Malformed(malformed),
            };
            if tx.send(heard).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

/// The QSOs a program logged: heard, journaled, and handed to the Engine.
struct Qsos {
    heard: Receiver<Heard>,
    journal: Journal,
    retry_at: Instant,
    waiting_said: Option<String>,
    malformed_said: bool,
    talking: Option<String>,
}

impl Qsos {
    /// Journal what WSJT-X logged since last time, then hand the Engine
    /// whatever waits, unless it said "not yet" a moment ago.
    fn step(&mut self, link: &mut Link) -> Result<(), String> {
        while let Ok(heard) = self.heard.try_recv() {
            match heard {
                Heard::Message(wsjtx::Message::LoggedAdif { id, adif }) => {
                    let entry = self.journal.add(&adif, &id)?;
                    println!("{id} logged a QSO; journaled as {}", entry.id);
                    self.retry_at = Instant::now();
                }
                Heard::Message(wsjtx::Message::Heartbeat { id, version }) => {
                    if self.talking.as_deref() != Some(id.as_str()) {
                        println!("{id} {version} is talking to the Agent");
                        self.talking = Some(id);
                    }
                }
                Heard::Message(wsjtx::Message::Close { id }) => {
                    println!("{id} closed");
                    self.talking = None;
                }
                Heard::Message(wsjtx::Message::Other) => {}
                Heard::Malformed(why) => {
                    if !self.malformed_said {
                        println!("ignored a message: {why}");
                        self.malformed_said = true;
                    }
                }
            }
        }
        if Instant::now() < self.retry_at {
            return Ok(());
        }
        for entry in self.journal.waiting() {
            match link.uplink.log_qso(&entry) {
                Handed::Logged(message) => {
                    println!("{message}");
                    self.journal.logged(&entry.id)?;
                    self.waiting_said = None;
                }
                Handed::Refused(why) => {
                    let file = self.journal.refused(&entry, &why)?;
                    println!(
                        "the Engine refused a QSO from {}: {why}. It is kept in {}; fix it and import that file in LabLog's Menu.",
                        entry.program,
                        file.display()
                    );
                }
                Handed::NotYet(why) => {
                    if self.waiting_said.as_deref() != Some(why.as_str()) {
                        println!("QSOs wait in the journal: {why}");
                        self.waiting_said = Some(why);
                    }
                    self.retry_at = Instant::now() + QSO_RETRY;
                    return Ok(());
                }
                Handed::KeyRefused(code) => {
                    println!("{}", SendError::Refused(code));
                    link.refused = true;
                    self.retry_at = Instant::now() + QSO_RETRY;
                    return Ok(());
                }
            }
        }
        self.retry_at = Instant::now() + QSO_RETRY;
        Ok(())
    }
}

fn run(
    engine: &str,
    key_file: &str,
    station: Option<&str>,
    wsjtx: Option<&Wsjtx>,
    choice: &RadioChoice,
) -> Result<(), String> {
    // Check the address before anything waits on it.
    Uplink::new(engine, "check")?;
    let agent = banner();
    // Stopping the Agent (Ctrl-C, or the service manager) stops its Hamlib
    // too, so the radio's port is free for the next start.
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("cannot watch for being stopped: {error}"))?;
    let key = station_key(engine, key_file, station, &running, false)?;
    let mut link = Link {
        uplink: Uplink::new(engine, &key)?,
        answering: true,
        refused: false,
    };
    println!("{agent}: sending this station's radio to {engine}");
    let mut qsos = match wsjtx {
        None => None,
        Some(wsjtx) => {
            let journal = Journal::open(Path::new(&wsjtx.journal))?;
            let waiting = journal.waiting().len();
            let heard = listen(wsjtx.address, &running)?;
            println!(
                "listening for WSJT-X at {}; {waiting} QSO(s) waiting in {}",
                wsjtx.address, wsjtx.journal
            );
            Some(Qsos {
                heard,
                journal,
                retry_at: Instant::now(),
                waiting_said: None,
                malformed_said: false,
                talking: None,
            })
        }
    };
    let mut radio: Option<Box<dyn Radio>> = None;
    let mut sent = Sent::Nothing;
    let mut sent_at = Instant::now();
    let mut retry_at = Instant::now();
    while running.load(Ordering::SeqCst) {
        if link.refused && station.is_some() {
            // The key was replaced or ended: ask to join again.
            let key = station_key(engine, key_file, station, &running, true)?;
            link = Link {
                uplink: Uplink::new(engine, &key)?,
                answering: true,
                refused: false,
            };
            sent = Sent::Nothing;
        }
        if let Some(qsos) = qsos.as_mut() {
            qsos.step(&mut link)?;
        }
        if radio.is_none() && Instant::now() >= retry_at {
            match open(choice) {
                Ok(opened) => {
                    let identity = opened.identity();
                    println!("connected to {} {}", identity.maker, identity.model);
                    radio = Some(opened);
                }
                Err(error) => {
                    retry_at = Instant::now() + RETRY;
                    let problem = error.to_string();
                    if sent != Sent::Problem(problem.clone()) {
                        println!("radio: {problem}");
                    }
                    sent = link.send_problem(&agent, &problem, &sent, &mut sent_at);
                }
            }
        }
        if let Some(current) = radio.as_mut() {
            match current.read() {
                Ok(reading) => {
                    let changed = sent != Sent::Reading(reading.clone());
                    if changed || sent_at.elapsed() >= HEARTBEAT {
                        let report = Report {
                            agent: &agent,
                            radio: Some(RadioReport {
                                identity: current.identity(),
                                reading: &reading,
                            }),
                            problem: None,
                        };
                        if link.deliver(&report) {
                            sent = Sent::Reading(reading);
                            sent_at = Instant::now();
                        }
                    }
                }
                Err(error) => {
                    let problem = error.to_string();
                    println!("radio: {problem}");
                    radio = None;
                    retry_at = Instant::now() + RETRY;
                    sent = link.send_problem(&agent, &problem, &sent, &mut sent_at);
                }
            }
        }
        thread::sleep(POLL);
    }
    drop(radio);
    println!("stopped");
    Ok(())
}

/// How often `serve` looks at the radio to show its status.
const STATUS_EVERY: Duration = Duration::from_millis(500);

/// The time of day in UTC, as `21:35:27Z` (Principle VIII).
fn utc_clock() -> String {
    let text = settings_record::utc_text(std::time::SystemTime::now());
    text.split_once('T')
        .map_or(text.clone(), |(_, time)| time.to_owned())
}

/// Be a virtual radio for other programs until stopped: read-only, or one
/// they may tune, or tune and key (Principles IX and XXI).
fn serve(
    choice: &RadioChoice,
    listen: SocketAddr,
    allow_tuning: bool,
    transmit_limit: Option<Duration>,
) -> Result<(), String> {
    let (notes, heard) = mpsc::channel();
    let shared: Arc<dyn rigctld_server::Source> = if let Some(limit) = transmit_limit {
        let radio = keyable(choice).map_err(|error| error.to_string())?;
        Arc::new(rigctld_server::Shared::with_transmit(radio, limit).with_notes(notes))
    } else if allow_tuning {
        let radio = keyable(choice).map_err(|error| error.to_string())?;
        Arc::new(rigctld_server::Shared::with_tuning(radio).with_notes(notes))
    } else {
        let radio = open(choice).map_err(|error| error.to_string())?;
        Arc::new(rigctld_server::Shared::new(radio).with_notes(notes))
    };
    let identity = shared.identity();
    let server = rigctld_server::Server::start(listen, Arc::clone(&shared))?;
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    println!(
        "{} {} is a virtual radio at {}, {}.",
        identity.maker,
        identity.model,
        server.address(),
        if transmit_limit.is_some() {
            "and programs may tune it and key it"
        } else if allow_tuning {
            "and programs may tune it"
        } else {
            "read-only"
        }
    );
    println!(
        "In WSJT-X: Settings > Radio > Rig: Hamlib NET rigctl, Network Server: {}",
        server.address()
    );
    if let Some(limit) = transmit_limit {
        println!(
            "One program at a time may key it, for at most {} s at once; the radio's own PTT wins. Ctrl-C stops.",
            limit.as_secs()
        );
    } else if allow_tuning {
        println!("Requests to transmit are refused. Ctrl-C stops.");
    } else {
        println!("Requests to tune or transmit are refused. Ctrl-C stops.");
    }
    show_until_stopped(shared.as_ref(), &heard, &running, || {});
    drop(server);
    println!("stopped");
    Ok(())
}

/// Prints what the radio says, whenever it changes, beside what programs
/// asked for, so the two can be compared at a glance (Principle X), until
/// `running` turns false. `also` runs every moment, for more to show.
fn show_until_stopped(
    shared: &dyn rigctld_server::Source,
    heard: &Receiver<String>,
    running: &AtomicBool,
    mut also: impl FnMut(),
) {
    let mut shown = String::new();
    let mut last_look: Option<Instant> = None;
    while running.load(Ordering::SeqCst) {
        for event in heard.try_iter() {
            print_event(&event);
        }
        if last_look.is_none_or(|at| at.elapsed() >= STATUS_EVERY) {
            last_look = Some(Instant::now());
            let now = match shared.reading() {
                Ok(reading) => format!("radio: {}", reading_text(&reading)),
                Err(error) => format!("radio: can't be read ({error})"),
            };
            if now != shown {
                println!("{}  {now}", utc_clock());
                shown = now;
            }
        }
        also();
        thread::sleep(Duration::from_millis(100));
    }
}

/// Prints one event with the time. A hint on how to fix something goes on
/// its own lines, indented under the event it explains.
fn print_event(event: &str) {
    let mut lines = event.lines();
    if let Some(first) = lines.next() {
        println!("{}  {first}", utc_clock());
    }
    for line in lines {
        println!("           {line}");
    }
}

/// Read a radio's menu settings, keep the first reading as its as-received
/// record, and report (Principles X and XIV).
fn record(choice: &RadioChoice, dir: &Path) -> Result<(), String> {
    let mut radio = serial_radio(choice).map_err(|error| error.to_string())?;
    record_from(radio.as_mut(), dir)
}

fn record_from(radio: &mut dyn serial_radio::SerialRadio, dir: &Path) -> Result<(), String> {
    let now = settings_record::read(
        radio,
        None,
        std::time::SystemTime::now(),
        &settings_record::computer_name(),
    );
    let path = settings_record::path_for(dir, &now);
    let first = settings_record::load(&path)?;
    let found = settings_record::findings(&now, radio.definition());
    for line in settings_record::report(&now, &found, first.as_ref(), radio.definition()) {
        println!("{line}");
    }
    if first.is_none() {
        settings_record::save(&path, &now)?;
        println!(
            "Saved as this radio's as-received record: {}",
            path.display()
        );
    }
    Ok(())
}

/// List the sound devices, so the right ones can be named to `rx-audio`.
#[cfg(windows)]
fn audio_devices() -> Result<(), String> {
    let (inputs, outputs) = audio_windows::devices()?;
    println!("Capturing (a radio's receive audio is one of these):");
    for device in &inputs {
        println!("  {}  [{}]", device.name, device.format);
    }
    println!("Playing (rx-audio only plays into a VB-Audio \"CABLE Input\"):");
    for device in &outputs {
        println!("  {}  [{}]", device.name, device.format);
    }
    Ok(())
}

#[cfg(not(windows))]
fn audio_devices() -> Result<(), String> {
    Err("routing a radio's audio is Windows-only for now".to_owned())
}

/// Receive a radio's audio into a virtual cable until stopped.
#[cfg(windows)]
fn rx_audio(from: &str, to: &str) -> Result<(), String> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    audio_windows::receive(from, to, &running)
}

#[cfg(not(windows))]
fn rx_audio(from: &str, to: &str) -> Result<(), String> {
    let _ = (from, to);
    Err("routing a radio's audio is Windows-only for now".to_owned())
}

/// Check rig definition files and say what was found in each
/// (Principle XII).
fn check_rigs(paths: &[String]) -> Result<(), String> {
    let mut results = Vec::new();
    for path in paths {
        let path = Path::new(path);
        if path.is_dir() {
            results.extend(rigdef::load_dir(path)?);
        } else {
            results.push((path.to_path_buf(), rigdef::load(path)));
        }
    }
    if results.is_empty() {
        return Err("no rig definition files found".to_owned());
    }
    let mut refused = 0;
    for (file, result) in &results {
        match result {
            Ok(definition) => println!("ok      {}: {}", file.display(), definition.summary()),
            Err(error) => {
                refused += 1;
                println!("REFUSED {error}");
            }
        }
    }
    if refused == 0 {
        Ok(())
    } else {
        Err(format!(
            "{refused} of {} definition files refused",
            results.len()
        ))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match parse(&args) {
        Ok(Command::Help) => {
            println!("{}\n\n{USAGE}", banner());
            Ok(())
        }
        Ok(Command::Read(choice)) => read_once(&choice),
        Ok(Command::CheckRig(paths)) => check_rigs(&paths),
        Ok(Command::Serve {
            radio,
            listen,
            allow_tuning,
            transmit_limit,
        }) => serve(&radio, listen, allow_tuning, transmit_limit),
        Ok(Command::Record { radio, records }) => record(&radio, Path::new(&records)),
        Ok(Command::Station {
            port,
            pretend,
            listen,
            limit,
            engine,
            kenwood,
            stated,
        }) => station::run(
            port.as_deref(),
            pretend,
            listen,
            limit,
            engine.as_ref().map(|(engine, key_file, station)| {
                (engine.as_str(), key_file.as_str(), station.as_deref())
            }),
            kenwood.as_deref(),
            &stated,
        ),
        Ok(Command::Startup(what, options)) => station::startup(&what, &options),
        Ok(Command::Tray(options)) => tray::run(&options),
        Ok(Command::CheckWrites(port)) => station::check_writes(port.as_deref()),
        Ok(Command::CheckWritesFlex(address)) => FlexRadio::open(&address)
            .map_err(|error| error.to_string())
            .and_then(|radio| station::check_writes_on(Box::new(radio), &address, None)),
        Ok(Command::AudioDevices) => audio_devices(),
        Ok(Command::RxAudio { from, to }) => rx_audio(&from, &to),
        Ok(Command::CivDemo) => civ_demo::run().map(|lines| {
            for line in lines {
                println!("{line}");
            }
        }),
        Ok(Command::Run {
            engine,
            key_file,
            station,
            wsjtx,
            radio,
        }) => run(
            &engine,
            &key_file,
            station.as_deref(),
            wsjtx.as_ref(),
            &radio,
        ),
        Err(error) => Err(format!("{error}\n\n{USAGE}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("loglab-agent: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, HamlibChoice, RadioChoice, VERSION, Wsjtx, banner, parse};
    use crate::sends_as;
    use std::time::Duration;

    use crate::hamlib::Launch;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn banner_names_the_agent_and_its_version() {
        assert_eq!(banner(), format!("loglab-agent {VERSION}"));
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn no_arguments_means_help() {
        assert_eq!(parse(&[]), Ok(Command::Help));
    }

    #[test]
    fn reads_a_run_with_hamlib() {
        let command = parse(&args(
            "run --engine http://127.0.0.1:8073 --key-file key --hamlib-model 3073 --device /dev/ttyUSB0 --baud 19200",
        ));
        assert_eq!(
            command,
            Ok(Command::Run {
                engine: "http://127.0.0.1:8073".to_owned(),
                key_file: "key".to_owned(),
                station: None,
                wsjtx: None,
                radio: RadioChoice::Hamlib(HamlibChoice::Start(Launch {
                    program: "rigctld".to_owned(),
                    model: 3073,
                    device: "/dev/ttyUSB0".to_owned(),
                    baud: Some(19200),
                })),
            })
        );
    }

    #[test]
    fn reads_a_run_that_listens_for_wsjtx() {
        let command = parse(&args(
            "run --engine http://127.0.0.1:8073 --key-file key --wsjtx 127.0.0.1:2237 --pretend",
        ));
        let Ok(Command::Run { wsjtx, .. }) = command else {
            unreachable!("{command:?}");
        };
        assert_eq!(
            wsjtx,
            Some(Wsjtx {
                address: "127.0.0.1:2237".parse().unwrap_or_else(|_| unreachable!()),
                journal: "key.qsos".to_owned(),
            })
        );
    }

    #[test]
    fn reads_the_pretend_radio() {
        assert_eq!(
            parse(&args("read --pretend")),
            Ok(Command::Read(RadioChoice::Pretend))
        );
    }

    #[test]
    fn reads_the_civ_pretend_radio() {
        assert_eq!(
            parse(&args("read --civ-pretend")),
            Ok(Command::Read(RadioChoice::CivPretend))
        );
        assert!(parse(&args("read --civ-pretend --pretend")).is_err());
    }

    #[test]
    fn reads_a_civ_radio_on_a_serial_port() {
        assert_eq!(
            parse(&args("read --civ COM5")),
            Ok(Command::Read(RadioChoice::Civ("COM5".to_owned())))
        );
        assert_eq!(
            parse(&args("record --civ /dev/ttyUSB0 --records r")),
            Ok(Command::Record {
                radio: RadioChoice::Civ("/dev/ttyUSB0".to_owned()),
                records: "r".to_owned()
            })
        );
        assert!(parse(&args("read --civ")).is_err(), "needs a port");
        assert!(parse(&args("read --civ COM5 --civ-pretend")).is_err());
        assert_eq!(
            parse(&args("read --cat COM7")),
            Ok(Command::Read(RadioChoice::Cat("COM7".to_owned())))
        );
        assert_eq!(
            parse(&args("read --cat-pretend")),
            Ok(Command::Read(RadioChoice::CatPretend))
        );
        assert!(parse(&args("read --cat COM7 --civ COM5")).is_err());
        assert!(parse(&args("serve --cat COM7 --allow-transmit")).is_ok());
        assert!(parse(&args("read --civ COM5 --hamlib-model 3073 --device COM5")).is_err());
    }

    #[test]
    fn reads_a_flexradio() {
        assert_eq!(
            parse(&args("read --flex 192.168.1.20")),
            Ok(Command::Read(RadioChoice::Flex("192.168.1.20".to_owned())))
        );
        assert_eq!(
            parse(&args("read --flex-pretend")),
            Ok(Command::Read(RadioChoice::FlexPretend))
        );
        assert_eq!(
            parse(&args("read --flex-pretend --flex-pretend")),
            Ok(Command::Read(RadioChoice::FlexPretend))
        );
        // A FlexRadio may be tuned and keyed, through its API.
        assert!(matches!(
            parse(&args("serve --flex 192.168.1.20 --allow-transmit")),
            Ok(Command::Serve {
                radio: RadioChoice::Flex(_),
                transmit_limit: Some(_),
                ..
            })
        ));
        assert!(parse(&args("serve --flex-pretend --allow-tuning")).is_ok());
        assert!(parse(&args("read --flex-pretend --cat-pretend")).is_err());
        assert!(parse(&args("read --flex-pretend --flex 10.0.0.2")).is_err());
    }

    #[test]
    fn reads_check_rig() {
        assert_eq!(
            parse(&args("check-rig rigs rigs/icom/ic-7300.toml")),
            Ok(Command::CheckRig(vec![
                "rigs".to_owned(),
                "rigs/icom/ic-7300.toml".to_owned()
            ]))
        );
        assert!(parse(&args("check-rig")).is_err());
    }

    #[test]
    fn reads_serve() {
        assert_eq!(
            parse(&args("serve --civ-pretend")),
            Ok(Command::Serve {
                radio: RadioChoice::CivPretend,
                listen: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
                allow_tuning: false,
                transmit_limit: None,
            })
        );
        assert!(parse(&args("serve --civ-pretend --listen 127.0.0.1:5000")).is_ok());
        assert!(matches!(
            parse(&args("serve --civ-pretend --allow-tuning")),
            Ok(Command::Serve {
                allow_tuning: true,
                ..
            })
        ));
        assert!(
            parse(&args("serve --pretend --allow-tuning")).is_err(),
            "only the CI-V driver may tune"
        );
        assert_eq!(
            parse(&args("serve --civ COM5 --allow-tuning")),
            Ok(Command::Serve {
                radio: RadioChoice::Civ("COM5".to_owned()),
                listen: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
                allow_tuning: true,
                transmit_limit: None,
            })
        );
        // Transmitting is the operator's choice on the command line, and
        // means tuning too.
        assert_eq!(
            parse(&args("serve --civ COM5 --allow-transmit")),
            Ok(Command::Serve {
                radio: RadioChoice::Civ("COM5".to_owned()),
                listen: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
                allow_tuning: true,
                transmit_limit: Some(Duration::from_mins(1)),
            })
        );
        assert!(matches!(
            parse(&args("serve --civ COM5 --allow-tuning --allow-transmit --tx-limit 15")),
            Ok(Command::Serve {
                allow_tuning: true,
                transmit_limit: Some(limit),
                ..
            }) if limit == Duration::from_secs(15)
        ));
        assert!(matches!(
            parse(&args("serve --civ-pretend --allow-transmit --allow-tuning")),
            Ok(Command::Serve {
                transmit_limit: Some(_),
                ..
            })
        ));
        for refused in [
            "serve --pretend --allow-transmit",
            "serve --hamlib-model 3073 --device COM5 --allow-transmit",
            "serve --civ COM5 --tx-limit 15",
            "serve --civ COM5 --allow-transmit --tx-limit 0",
            "serve --civ COM5 --allow-transmit --tx-limit 601",
            "serve --civ COM5 --allow-transmit --tx-limit soon",
            "read --civ COM5 --allow-transmit",
        ] {
            assert!(parse(&args(refused)).is_err(), "{refused}");
        }
        assert!(parse(&args("read --civ-pretend --allow-tuning")).is_err());
        assert!(parse(&args("read --civ-pretend --listen 127.0.0.1:5000")).is_err());
    }

    #[test]
    fn reads_where_the_station_is() {
        assert!(matches!(
            parse(&args("station --mqtt 10.73.10.10 --grid em18ct --call w0cy")),
            Ok(Command::Station { stated, .. }) if stated == sends_as::Stated {
                mqtt: Some(("10.73.10.10".to_owned(), 1883)),
                no_bus: false,
                call: Some("W0CY".to_owned()),
                grid: Some("EM18CT".to_owned()),
            }
        ));
        assert!(parse(&args("station --grid EM1")).is_err());
        assert!(parse(&args("station --call 12")).is_err());
        assert!(parse(&args("station --mqtt kiosk:x")).is_err());
        assert!(matches!(
            parse(&args("station --mqtt none")),
            Ok(Command::Station { stated, .. }) if stated.no_bus && stated.mqtt.is_none()
        ));
    }

    #[test]
    fn reads_station() {
        assert_eq!(
            parse(&args("station")),
            Ok(Command::Station {
                port: None,
                pretend: false,
                listen: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
                limit: Duration::from_mins(1),
                engine: None,
                kenwood: None,
                stated: sends_as::Stated::default(),
            })
        );
        assert_eq!(
            parse(&args(
                "station --civ COM5 --listen 127.0.0.1:4533 --tx-limit 30"
            )),
            Ok(Command::Station {
                port: Some("COM5".to_owned()),
                pretend: false,
                listen: "127.0.0.1:4533".parse().unwrap_or_else(|_| unreachable!()),
                limit: Duration::from_secs(30),
                engine: None,
                kenwood: None,
                stated: sends_as::Stated::default(),
            })
        );
        assert_eq!(
            parse(&args(
                "station --engine http://192.168.1.10:8073 --key-file station.key"
            )),
            Ok(Command::Station {
                port: None,
                pretend: false,
                listen: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
                limit: Duration::from_mins(1),
                engine: Some((
                    "http://192.168.1.10:8073".to_owned(),
                    "station.key".to_owned(),
                    None
                )),
                kenwood: None,
                stated: sends_as::Stated::default(),
            })
        );
        assert_eq!(
            parse(&args("startup on")),
            Ok(Command::Startup("on".to_owned(), Vec::new()))
        );
        assert_eq!(
            parse(&args("startup on --engine http://x:8073 --key-file k")),
            Ok(Command::Startup(
                "on".to_owned(),
                args("--engine http://x:8073 --key-file k")
            ))
        );
        assert!(parse(&args("startup on --engine http://x:8073")).is_err());
        assert!(parse(&args("startup off --civ COM5")).is_err());
        assert_eq!(
            parse(&args("tray --civ-pretend")),
            Ok(Command::Tray(args("--civ-pretend")))
        );
        assert!(parse(&args("tray --nonsense")).is_err());
        assert!(matches!(
            parse(&args("station --civ-pretend")),
            Ok(Command::Station {
                pretend: true,
                port: None,
                ..
            })
        ));
        assert!(parse(&args("station --civ-pretend --civ COM5")).is_err());
        assert!(matches!(
            parse(&args("station --civ COM5 --kenwood CNCB0")),
            Ok(Command::Station { kenwood: Some(port), .. }) if port == "CNCB0"
        ));
        assert!(parse(&args("startup")).is_err());
        assert_eq!(parse(&args("check-writes")), Ok(Command::CheckWrites(None)));
        assert_eq!(
            parse(&args("check-writes --civ COM5")),
            Ok(Command::CheckWrites(Some("COM5".to_owned())))
        );
        assert!(parse(&args("check-writes --pretend")).is_err());
        assert_eq!(
            parse(&args("check-writes --flex 192.168.1.20")),
            Ok(Command::CheckWritesFlex("192.168.1.20".to_owned()))
        );
        assert!(parse(&args("check-writes --flex")).is_err());
        assert!(parse(&args("startup maybe")).is_err());
        for refused in [
            "station --tx-limit 0",
            "station --tx-limit 601",
            "station --engine http://x",
            "station --key-file k",
            "station --station fieldlab-1",
            "station --listen nowhere",
            "station --civ",
            "station --pretend",
        ] {
            assert!(parse(&args(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn reads_record() {
        assert_eq!(
            parse(&args("record --civ-pretend --records r")),
            Ok(Command::Record {
                radio: RadioChoice::CivPretend,
                records: "r".to_owned()
            })
        );
        assert!(
            parse(&args("record --civ-pretend")).is_err(),
            "needs --records"
        );
        assert!(parse(&args("read --civ-pretend --records r")).is_err());
    }

    #[test]
    fn reads_the_audio_commands() {
        assert_eq!(parse(&args("audio-devices")), Ok(Command::AudioDevices));
        assert_eq!(
            parse(&args("rx-audio --from CODEC")),
            Ok(Command::RxAudio {
                from: "CODEC".to_owned(),
                to: "CABLE Input".to_owned()
            })
        );
        assert_eq!(
            parse(&args("rx-audio --from CODEC --to CABLE-A")),
            Ok(Command::RxAudio {
                from: "CODEC".to_owned(),
                to: "CABLE-A".to_owned()
            })
        );
        assert!(parse(&args("rx-audio")).is_err(), "needs --from");
        assert!(parse(&args("rx-audio --from")).is_err());
        assert!(parse(&args("audio-devices --all")).is_err());
    }

    #[test]
    fn reads_civ_demo() {
        assert_eq!(parse(&args("civ-demo")), Ok(Command::CivDemo));
        assert!(parse(&args("civ-demo --pretend")).is_err());
    }

    #[test]
    fn refuses_unclear_radios() {
        for bad in [
            "read",
            "read --pretend --hamlib-model 1",
            "read --hamlib-model 1",
            "read --hamlib-model 1 --device x --rigctld 127.0.0.1:4532",
            "read --hamlib-model one --device x",
            "run --pretend",
            "run --pretend --engine http://x",
            "transmit --pretend",
            "read --pretend --frequency 7",
            "read --flex",
            "read --flex 10.0.0.2 --pretend",
            "read --flex 10.0.0.2 --hamlib-model 3073 --device x",
            "read --pretend --ft8",
            "read --pretend --wsjtx 127.0.0.1:2237",
            "run --engine http://x --key-file k --pretend --wsjtx 0.0.0.0:2237",
            "run --engine http://x --key-file k --pretend --wsjtx 192.168.1.5:2237",
            "run --engine http://x --key-file k --pretend --journal j",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad} should be refused");
        }
    }
}
