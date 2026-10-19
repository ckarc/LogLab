// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LogLab Station Agent.
//!
//! The Agent owns the radios: it is the only component that opens a radio's
//! port or control connection, it presents every radio through one common rig
//! model, and it is the only code that can key a transmitter. See
//! `CONSTITUTION.md` at the repository root.
//!
//! This version reads one radio, through Hamlib or the built-in pretend
//! radio, and streams its live state to the Engine. It never tunes the radio
//! and never keys it.

mod engine;
mod hamlib;
mod pretend;
mod rig;

use std::fs;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use engine::{RadioReport, Report, SendError, Uplink};
use hamlib::{HamlibRadio, Launch, Source};
use pretend::PretendRadio;
use rig::{Radio, Reading};

/// The Agent's version, from `Cargo.toml`.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How often the radio is read.
const POLL: Duration = Duration::from_millis(250);
/// How often the Engine hears from the Agent when nothing changes.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// How long to wait before trying an unreachable radio again.
const RETRY: Duration = Duration::from_secs(3);

const USAGE: &str = "\
Usage:
  loglab-agent run  --engine URL --key-file FILE RADIO
  loglab-agent read RADIO              read the radio once and print it

RADIO is one of:
  --pretend                            the built-in pretend radio
  --hamlib-model N --device PORT [--baud N] [--rigctld-program PATH]
                                       start Hamlib's rigctld for a radio
                                       (3073 is the Icom IC-7300)
  --hamlib-model N --rigctld 127.0.0.1:PORT
                                       use a rigctld already running here

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
    Run {
        engine: String,
        key_file: String,
        radio: RadioChoice,
    },
}

#[derive(Default)]
struct Options {
    engine: Option<String>,
    key_file: Option<String>,
    pretend: bool,
    model: Option<u32>,
    device: Option<String>,
    baud: Option<u32>,
    program: Option<String>,
    rigctld: Option<SocketAddr>,
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
            "--hamlib-model" => options.model = Some(number(flag, &value)?),
            "--device" => options.device = Some(value),
            "--baud" => options.baud = Some(number(flag, &value)?),
            "--rigctld-program" => options.program = Some(value),
            "--rigctld" => {
                options.rigctld = Some(value.parse().map_err(|_| {
                    format!("--rigctld needs an address like 127.0.0.1:4532, not {value:?}")
                })?);
            }
            _ => return Err(format!("unknown option {flag:?}")),
        }
    }
    let radio = radio_choice(&mut options)?;
    match verb.as_str() {
        "read" => Ok(Command::Read(radio)),
        "run" => Ok(Command::Run {
            engine: options
                .engine
                .ok_or("run needs --engine, such as --engine http://127.0.0.1:8073")?,
            key_file: options
                .key_file
                .ok_or("run needs --key-file, the file holding this station's key")?,
            radio,
        }),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn radio_choice(options: &mut Options) -> Result<RadioChoice, String> {
    if options.pretend {
        if options.model.is_some() || options.device.is_some() || options.rigctld.is_some() {
            return Err("choose either --pretend or a Hamlib radio, not both".to_owned());
        }
        return Ok(RadioChoice::Pretend);
    }
    let model = options
        .model
        .ok_or("choose a radio: --pretend, or --hamlib-model with --device or --rigctld")?;
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

fn run(engine: &str, key_file: &str, choice: &RadioChoice) -> Result<(), String> {
    let key = fs::read_to_string(key_file)
        .map_err(|error| format!("cannot read the station key from {key_file}: {error}"))?;
    let uplink = Uplink::new(engine, key.trim())?;
    let agent = banner();
    // Stopping the Agent (Ctrl-C, or the service manager) stops its Hamlib
    // too, so the radio's port is free for the next start.
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("cannot watch for being stopped: {error}"))?;
    println!("{agent}: sending this station's radio to {engine}");
    let mut radio: Option<Box<dyn Radio>> = None;
    let mut sent = Sent::Nothing;
    let mut sent_at = Instant::now();
    let mut retry_at = Instant::now();
    let mut engine_ok = true;
    while running.load(Ordering::SeqCst) {
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
                    sent = send_problem(
                        &uplink,
                        &agent,
                        &problem,
                        &sent,
                        &mut sent_at,
                        &mut engine_ok,
                    );
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
                        if deliver(&uplink, &report, &mut engine_ok) {
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
                    sent = send_problem(
                        &uplink,
                        &agent,
                        &problem,
                        &sent,
                        &mut sent_at,
                        &mut engine_ok,
                    );
                }
            }
        }
        thread::sleep(POLL);
    }
    drop(radio);
    println!("stopped");
    Ok(())
}

fn send_problem(
    uplink: &Uplink,
    agent: &str,
    problem: &str,
    sent: &Sent,
    sent_at: &mut Instant,
    engine_ok: &mut bool,
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
    if deliver(uplink, &report, engine_ok) {
        *sent_at = Instant::now();
        Sent::Problem(problem.to_owned())
    } else {
        Sent::Nothing
    }
}

/// Send a report, saying once when the Engine stops or starts answering.
fn deliver(uplink: &Uplink, report: &Report<'_>, engine_ok: &mut bool) -> bool {
    match uplink.send(report) {
        Ok(()) => {
            if !*engine_ok {
                println!("the Engine is answering again");
            }
            *engine_ok = true;
            true
        }
        Err(error) => {
            if *engine_ok || matches!(error, SendError::Refused(_)) {
                println!("{error}");
            }
            *engine_ok = false;
            false
        }
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
        Ok(Command::Run {
            engine,
            key_file,
            radio,
        }) => run(&engine, &key_file, &radio),
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
    use super::{Command, HamlibChoice, RadioChoice, VERSION, banner, parse};
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
    fn reads_the_pretend_radio() {
        assert_eq!(
            parse(&args("read --pretend")),
            Ok(Command::Read(RadioChoice::Pretend))
        );
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
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad} should be refused");
        }
    }
}
