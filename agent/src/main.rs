// SPDX-FileCopyrightText: 2026 LogLab contributors
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

mod civ;
mod civ_demo;
mod civ_pretend;
mod engine;
mod flex;
mod hamlib;
mod journal;
mod pairing;
mod pretend;
mod rig;
mod rigdef;
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

use engine::{Handed, RadioReport, Report, SendError, Uplink};
use flex::FlexRadio;
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

const USAGE: &str = "\
Usage:
  loglab-agent run  --engine URL --key-file FILE [--station NAME]
                    [--wsjtx 127.0.0.1:2237 [--journal FILE]] RADIO
  loglab-agent read RADIO              read the radio once and print it
  loglab-agent check-rig PATH...       check rig definition files (a file,
                                       or every .toml file in a folder)
  loglab-agent civ-demo                show a CI-V conversation with the
                                       pretend IC-7300, byte by byte

RADIO is one of:
  --pretend                            the built-in pretend radio
  --hamlib-model N --device PORT [--baud N] [--rigctld-program PATH]
                                       start Hamlib's rigctld for a radio
                                       (3073 is the Icom IC-7300)
  --hamlib-model N --rigctld 127.0.0.1:PORT
                                       use a rigctld already running here
  --flex ADDRESS                       a FlexRadio on the network, by its IP
                                       address or name (port 4992 unless
                                       given, as in 192.168.1.20:4992)

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
    Hamlib(HamlibChoice),
    Flex(String),
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

#[derive(Default)]
struct Options {
    engine: Option<String>,
    key_file: Option<String>,
    station: Option<String>,
    pretend: bool,
    model: Option<u32>,
    device: Option<String>,
    baud: Option<u32>,
    program: Option<String>,
    rigctld: Option<SocketAddr>,
    flex: Option<String>,
    wsjtx: Option<SocketAddr>,
    journal: Option<String>,
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
        if flag == "--pretend" {
            options.pretend = true;
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
            "--journal" => options.journal = Some(value),
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
    if options.journal.is_some() && options.wsjtx.is_none() {
        return Err("--journal goes with --wsjtx".to_owned());
    }
    match verb.as_str() {
        "read" if options.wsjtx.is_some() => Err("--wsjtx goes with run".to_owned()),
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

fn radio_choice(options: &mut Options) -> Result<RadioChoice, String> {
    let hamlib = options.model.is_some() || options.device.is_some() || options.rigctld.is_some();
    let chosen = [options.pretend, hamlib, options.flex.is_some()];
    if chosen.iter().filter(|c| **c).count() > 1 {
        return Err("choose one radio: --pretend, a Hamlib radio, or --flex".to_owned());
    }
    if options.pretend {
        return Ok(RadioChoice::Pretend);
    }
    if let Some(address) = options.flex.take() {
        return Ok(RadioChoice::Flex(address));
    }
    let model = options
        .model
        .ok_or("choose a radio: --pretend, --flex, or --hamlib-model with --device or --rigctld")?;
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

fn open(choice: &RadioChoice) -> Result<Box<dyn Radio>, rig::RadioError> {
    Ok(match choice {
        RadioChoice::Pretend => Box::new(PretendRadio::new()),
        RadioChoice::Flex(address) => Box::new(FlexRadio::open(address)?),
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
    format!(
        "{} {}: {mhz} MHz {mode}, {ptt}",
        identity.maker, identity.model
    )
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
    fn reads_a_flexradio() {
        assert_eq!(
            parse(&args("read --flex 192.168.1.20")),
            Ok(Command::Read(RadioChoice::Flex("192.168.1.20".to_owned())))
        );
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
