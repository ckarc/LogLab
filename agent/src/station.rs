// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent station`: everything programs on this computer need to use
//! the radio, from one command.
//!
//! 1. Finds the radio: the serial port whose USB IDs match a known radio's,
//!    confirmed by the radio's own answer to "who are you?" at the speed it
//!    answers at.
//! 2. Reads every setting the radio's definition lists, keeps the first
//!    reading of each radio (by its serial number) as its as-received record
//!    in `records` beside the Agent, and says what has changed since. Checks
//!    the settings against what digital modes need, and says what is wrong
//!    and how to fix it on the radio. It changes nothing on the radio: its
//!    definition's settings aren't verified on a real radio yet, and the
//!    Agent never writes an unverified setting.
//! 3. Carries the radio's audio both ways (Windows): receive into VB-CABLE,
//!    and a program's transmit audio from a second virtual cable to the
//!    radio, only while the Agent has it keyed for that program.
//! 4. Points VARA's sound devices at those cables, if VARA isn't running
//!    (see `vara`).
//! 5. Serves the radio to programs on this computer, which may tune it and
//!    key it (`serve --allow-transmit`).
//! 6. With `--engine` and `--key-file`, keeps the radio's settings on the
//!    Engine, so every computer it is plugged into knows them: sends its
//!    as-received record, lists what differs from each setup an operator
//!    kept (FT8, say), and keeps the radio's settings as a setup when the
//!    operator types `keep NAME`. It only reads: nothing is put back on the
//!    radio (Principle X).
//!
//! Starting `station` is the operator's choice that programs on this
//! computer may key the radio, as `--allow-transmit` is for `serve`
//! (Principle IX).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::civ_radio::CivRadio;
use crate::civ_serial::{self, SerialPort};
use crate::engine::{KeptRadio, KeptReading, SendError, Uplink};
use crate::rig::{Radio, RadioError};
use crate::rigctld_server::{self, Shared, Source};
use crate::rigdef::Definition;
use crate::settings_record::Record;
use crate::{rigdef, settings_record};

/// The radio, found and checked.
struct Found {
    radio: CivRadio<SerialPort>,
    port: String,
    serial: Option<String>,
}

/// Finds the radio on `port`, or, with none given, on whichever serial port
/// looks like a known radio's and answers as one. On Windows, a radio
/// plugged in with no driver for its USB serial bridge gets one first (see
/// `driver`).
fn find(port: Option<&str>, running: &AtomicBool) -> Result<Found, String> {
    let definitions = rigdef::built_in()?;
    if let Some(port) = port {
        let opened = SerialPort::open(port)?;
        let radio = CivRadio::find(opened, &definitions).map_err(|e| e.to_string())?;
        return Ok(Found {
            radio,
            port: port.to_owned(),
            serial: None,
        });
    }
    let candidates = civ_serial::radio_ports(&definitions)?;
    #[cfg(windows)]
    let candidates = if candidates.is_empty() && crate::driver::ensure(&definitions, running)? {
        civ_serial::radio_ports(&definitions)?
    } else {
        candidates
    };
    #[cfg(not(windows))]
    let _ = running;
    if candidates.is_empty() {
        return Err("no radio's USB serial port is on this computer.\n\
             -> Is the radio on, with its USB cable in? If it is, unplug the USB cable and \
             plug it in again. On Windows, Device Manager > Ports (COM & LPT) should show \
             \"Silicon Labs CP210x USB to UART Bridge\" for an IC-7300; if it is under \
             Other devices instead, its driver isn't installed."
            .to_owned());
    }
    let mut tried = Vec::new();
    for candidate in candidates {
        let answered = SerialPort::open(&candidate.port).and_then(|opened| {
            CivRadio::find(opened, &definitions).map_err(|error: RadioError| error.to_string())
        });
        match answered {
            Ok(radio) => {
                return Ok(Found {
                    radio,
                    port: candidate.port,
                    serial: candidate.serial,
                });
            }
            Err(why) => tried.push(format!("{}: {why}", candidate.port)),
        }
    }
    Err(format!(
        "no radio answered on any port that looked like one's:\n  {}\n\
         -> Is the radio on? Is another program (VarAC, WSJT-X, Hamlib's rigctld) holding \
         its port? Close it: the Agent is the radio's one owner.",
        tried.join("\n  ")
    ))
}

/// How often the radio is looked for again while it isn't there.
const LOOK_AGAIN: Duration = Duration::from_secs(5);

/// Looks for the radio until it is found or `running` turns false, so the
/// Agent can start before the radio is turned on (at Windows sign-in, for
/// example). Says why it isn't found once, and again only when the reason
/// changes.
fn wait_for_radio(port: Option<&str>, running: &AtomicBool) -> Option<Found> {
    let mut said = String::new();
    while running.load(Ordering::SeqCst) {
        match find(port, running) {
            Ok(found) => return Some(found),
            Err(why) => {
                if why != said {
                    crate::print_event(&format!(
                        "waiting for the radio: {why}\n-> The Agent keeps looking every {} s; Ctrl-C stops.",
                        LOOK_AGAIN.as_secs()
                    ));
                    said = why;
                }
            }
        }
        let until = std::time::Instant::now() + LOOK_AGAIN;
        while running.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

/// Where settings records are kept: `records` beside the Agent.
fn records_folder() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|folder| folder.join("records")))
        .unwrap_or_else(|| "records".into())
}

/// Reads the radio's settings, keeps the first reading of this radio as its
/// as-received record, and says what changed since and what in them stops
/// digital modes working. Returns the reading.
fn check_settings(radio: &mut CivRadio<SerialPort>, serial: Option<&str>) -> Record {
    let definition = radio.definition().clone();
    let record = settings_record::read(
        radio,
        serial,
        std::time::SystemTime::now(),
        &settings_record::computer_name(),
    );
    let unread = record
        .settings
        .iter()
        .filter(|entry| matches!(entry.value, settings_record::Value::Unread { .. }))
        .count();
    println!(
        "Radio settings: read {} of {}.",
        record.settings.len() - unread,
        record.settings.len()
    );
    let path = settings_record::path_for(&records_folder(), &record);
    match settings_record::load(&path) {
        Ok(None) => match settings_record::save(&path, &record) {
            Ok(()) => println!(
                "Radio settings: kept as this radio's as-received record, {}",
                path.display()
            ),
            Err(error) => println!("Radio settings: {error}"),
        },
        Ok(Some(first)) => {
            let changed = settings_record::changes(&first, &record);
            if changed.is_empty() {
                println!("Radio settings: nothing has changed since the radio arrived.");
            } else {
                println!(
                    "Radio settings: {} changed since the radio arrived ({} on {}):",
                    changed.len(),
                    first.read_utc,
                    first.computer
                );
                for change in changed {
                    println!("  {}: {} -> {}", change.label, change.before, change.after);
                }
            }
        }
        Err(error) => println!("Radio settings: {error}"),
    }
    let found = settings_record::findings(&record, &definition);
    if found.is_empty() {
        println!("Radio settings: every one digital modes need is right.");
    }
    for finding in found {
        println!(
            "Radio settings: {} is {}. {}\n  -> On the radio, set {} to {}.",
            finding.label,
            finding.now,
            finding.explain,
            finding.label,
            finding.offer.unwrap_or_else(|| finding.accept.join(" or "))
        );
    }
    record
}

/// The link to the Engine, from `--engine` and the key in `--key-file`, or
/// a new key from joining as `station` when the file has none.
fn uplink(
    engine: &str,
    key_file: &str,
    station: Option<&str>,
    running: &AtomicBool,
) -> Result<Uplink, String> {
    let key = crate::station_key(engine, key_file, station, running, false)?;
    Uplink::new(engine, &key)
}

/// The radio's settings as a setup kept on the Engine has them, in lines to
/// show: each setting that differs, or that it matches.
fn compared(kept: &KeptReading, now: &Record, definition: &Definition) -> Vec<String> {
    let differ = settings_record::differences(
        kept.settings
            .iter()
            .map(|s| (s.id.as_str(), s.label.as_str(), s.bytes.as_deref())),
        now,
        definition,
    );
    let what = kept.name.as_deref().map_or_else(
        || "the as-received record".to_owned(),
        |name| format!("setup {name}"),
    );
    let whence = format!(
        "kept {} at {}{}",
        kept.saved_at,
        kept.station,
        kept.operator
            .as_deref()
            .map(|who| format!(" by {who}"))
            .unwrap_or_default()
    );
    if differ.is_empty() {
        return vec![format!("the radio matches {what} ({whence}).")];
    }
    let mut lines = vec![format!(
        "{} differ{} from {what} ({whence}):",
        differ.len(),
        if differ.len() == 1 { "s" } else { "" }
    )];
    lines.extend(differ.into_iter().map(|change| {
        format!(
            "  {}: the radio has {}; {what} has {}",
            change.label, change.after, change.before
        )
    }));
    lines
}

/// Every setup kept for the radio, compared with what it has now.
fn compared_all(kept: &KeptRadio, now: &Record, definition: &Definition) -> Vec<String> {
    if kept.setups.is_empty() {
        return vec![
            "no setups are kept for this radio yet.\n-> Set the radio up for a job, then \
             type keep NAME here (keep FT8, say)."
                .to_owned(),
        ];
    }
    let mut lines = Vec::new();
    for setup in &kept.setups {
        lines.extend(compared(setup, now, definition));
    }
    lines.push(
        "-> For now, change those on the radio by hand: the Agent doesn't put settings \
         back until they are verified on a real radio."
            .to_owned(),
    );
    lines
}

/// Sends the radio's as-received record to the Engine, and says how the
/// radio compares with each setup kept there.
fn sync(uplink: &Uplink, record: &Record, definition: &Definition) {
    let Some(serial) = record.serial.as_deref() else {
        println!(
            "Engine: the radio's settings aren't kept there, because its serial number isn't \
             known (it comes with the USB serial port; leave out --civ to find it)."
        );
        return;
    };
    match uplink.keep_reading(record, None) {
        Ok(true) => {
            println!("Engine: kept this as the radio's as-received record, for every computer.");
        }
        Ok(false) => println!("Engine: already has this radio's as-received record."),
        Err(error) => {
            println!("Engine: {error}");
            return;
        }
    }
    match uplink.radio_settings(&record.maker, &record.model, serial) {
        Ok(kept) => {
            for line in compared_all(&kept, record, definition) {
                println!("Engine: {line}");
            }
        }
        Err(error) => println!("Engine: {error}"),
    }
}

/// A line typed at the Agent's console.
#[derive(Debug, PartialEq, Eq)]
enum Typed {
    /// Keep the radio's settings on the Engine as this setup.
    Keep(String),
    /// Compare the radio with every setup kept for it.
    Compare,
    /// Say what can be typed.
    Help,
    /// Nothing typed.
    Nothing,
}

/// What a typed line asks for.
fn typed(line: &str) -> Result<Typed, String> {
    let line = line.trim();
    let (word, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(word, rest)| (word, rest.trim()));
    match (word.to_ascii_lowercase().as_str(), rest) {
        ("", _) => Ok(Typed::Nothing),
        ("keep", "") => Err("keep needs a setup name, like keep FT8".to_owned()),
        ("keep", name) => {
            crate::engine::check_setup_name(name)?;
            Ok(Typed::Keep(name.to_owned()))
        }
        ("compare", "") => Ok(Typed::Compare),
        ("help" | "?", "") => Ok(Typed::Help),
        _ => Err(format!("unknown command {line:?}")),
    }
}

/// What can be typed at the Agent's console.
const TYPE_HELP: &str = "Type keep NAME to keep the radio's settings on the Engine as a setup \
                         (keep FT8, say), or compare to see what differs from each setup.";

/// Reads the operator's commands from the console until it closes.
fn console(shared: &Shared, uplink: &Uplink, serial: &str, definition: &Definition) {
    let read = || {
        shared.settings(|radio| {
            settings_record::read(
                radio,
                Some(serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            )
        })
    };
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { return };
        let said = match typed(&line) {
            Ok(Typed::Nothing) => continue,
            Ok(Typed::Help) => TYPE_HELP.to_owned(),
            Err(why) => format!("{why}\n-> {TYPE_HELP}"),
            Ok(Typed::Keep(name)) => match read() {
                Ok(record) => match uplink.keep_reading(&record, Some(&name)) {
                    Ok(_) => format!(
                        "kept the radio's settings on the Engine as setup {name} ({} settings)",
                        record.settings.len()
                    ),
                    Err(error) => format!("setup {name} not kept: {error}"),
                },
                Err(why) => format!("setup {name} not kept: {why}"),
            },
            Ok(Typed::Compare) => match read().map_err(SendError::Unreachable).and_then(|now| {
                uplink
                    .radio_settings(&now.maker, &now.model, serial)
                    .map(|kept| compared_all(&kept, &now, definition))
            }) {
                Ok(lines) => lines.join("\n"),
                Err(error) => format!("not compared: {error}"),
            },
        };
        crate::print_event(&said);
    }
}

/// The radio's audio, both ways, and VARA pointed at it. Returns what to
/// keep running and what to show while it runs.
#[cfg(windows)]
fn audio(sound: Option<&str>, on_air: Arc<AtomicBool>) -> Option<crate::audio_windows::Station> {
    use crate::audio_windows::Station;
    let Some(sound) = sound else {
        println!("Audio: this radio's definition doesn't name its USB sound device.");
        return None;
    };
    let station = match Station::start(sound, on_air) {
        Ok(station) => station,
        Err(why) => {
            println!("Audio: not carried ({why})");
            return None;
        }
    };
    println!(
        "Audio: the radio's receive audio plays into \"{}\".",
        crate::audio::RECEIVE_CABLE
    );
    match (&station.transmit_cable, &station.no_transmit) {
        (Some(cable), _) => println!(
            "Audio: programs' transmit audio, played into \"{cable}\", goes to the radio while it is keyed."
        ),
        (None, why) => println!(
            "Audio: transmit audio doesn't go through the Agent: {}.\n  -> Until it does, \
             a program's sound output must go straight to the radio's USB sound device.",
            why.as_deref().unwrap_or("no transmit cable")
        ),
    }
    vara(&station);
    Some(station)
}

/// Points VARA's sound devices at the Agent's cables, if VARA isn't running.
#[cfg(windows)]
fn vara(station: &crate::audio_windows::Station) {
    use crate::vara::{self, Outcome, Soundcard};
    let Some(path) = vara::find() else {
        println!(
            "VARA: not found in C:\\VARA or Program Files, so its sound devices were left alone."
        );
        return;
    };
    match vara::running() {
        Ok(false) => {}
        Ok(true) => {
            println!(
                "VARA: running, so its sound devices were left alone (it keeps its settings \
                 while it runs).\n  -> Close VARA, then start the Agent again."
            );
            return;
        }
        Err(why) => {
            println!("VARA: its sound devices were left alone ({why}).");
            return;
        }
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let now = vara::soundcard(&text);
    let (recording, playing) = crate::audio_windows::device_names();
    let wanted = Soundcard {
        input: Some(vara::as_vara_names_it(
            crate::audio::RECEIVE_CABLE_OUT,
            now.input.as_deref(),
            &recording,
        )),
        output: station
            .transmit_cable
            .as_deref()
            .map(|cable| vara::as_vara_names_it(cable, now.output.as_deref(), &playing)),
    };
    match vara::set(&path, &wanted) {
        Ok(Outcome::AlreadySet) => println!("VARA: its sound devices are already the Agent's."),
        Ok(Outcome::Changed(before)) => println!(
            "VARA: sound devices set in {} (input {:?} was {:?}, output {:?} was {:?}). \
             The file as first found is kept as VARA.ini.lablog-original.",
            path.display(),
            wanted.input.as_deref().unwrap_or(""),
            before.input.as_deref().unwrap_or(""),
            wanted.output.as_deref().unwrap_or("(left alone)"),
            before.output.as_deref().unwrap_or(""),
        ),
        Err(why) => println!("VARA: its sound devices weren't set ({why})."),
    }
}

/// Tells the operator how programs on this computer reach the radio.
fn say_how_to_connect(model: &str, address: SocketAddr, limit: Duration) {
    println!(
        "Programs on this computer may tune and key the {} at {}: one at a time, for at most \
         {} s at once, and the radio's own PTT wins.",
        model,
        address,
        limit.as_secs()
    );
    println!(
        "  VarAC: Settings > RIG: Hamlib for PTT and Frequency Control, host localhost, port {}.",
        address.port()
    );
    println!(
        "  WSJT-X: Settings > Radio: Rig Hamlib NET rigctl, Network Server {address}, PTT Method CAT."
    );
}

/// Runs the station until Ctrl-C.
///
/// # Errors
///
/// The radio isn't found, the virtual radio can't listen, or `engine` (the
/// Engine's address, the key file, and the station to join as) doesn't
/// make a link to the Engine.
pub fn run(
    port: Option<&str>,
    listen: SocketAddr,
    limit: Duration,
    engine: Option<(&str, &str, Option<&str>)>,
) -> Result<(), String> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    let uplink = engine
        .map(|(engine, key_file, station)| uplink(engine, key_file, station, &running))
        .transpose()?;
    let Some(mut found) = wait_for_radio(port, &running) else {
        println!("stopped");
        return Ok(());
    };
    let identity = found.radio.identity().clone();
    println!(
        "Found: {} {} on {}{}, at {} baud.",
        identity.maker,
        identity.model,
        found.port,
        found
            .serial
            .as_deref()
            .map(|serial| format!(" (USB serial {serial})"))
            .unwrap_or_default(),
        found.radio.baud()
    );
    let record = check_settings(&mut found.radio, found.serial.as_deref());
    let definition = found.radio.definition().clone();
    if let Some(uplink) = &uplink {
        sync(uplink, &record, &definition);
    }
    let sound = definition.usb_audio.clone();
    let (notes, heard) = mpsc::channel();
    let shared = Arc::new(Shared::with_transmit(Box::new(found.radio), limit).with_notes(notes));
    let on_air = shared.on_air();
    #[cfg(windows)]
    let station = audio(sound.as_deref(), Arc::clone(&on_air));
    #[cfg(not(windows))]
    {
        let _ = (&sound, &on_air);
        println!("Audio: carried on Windows only.");
    }
    let server = rigctld_server::Server::start(listen, Arc::clone(&shared) as Arc<dyn Source>)?;
    say_how_to_connect(&identity.model, server.address(), limit);
    if let (Some(uplink), Some(serial)) = (uplink, found.serial.clone()) {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || console(&shared, &uplink, &serial, &definition));
        println!("{TYPE_HELP}");
    }
    println!("Ctrl-C stops.");
    #[cfg(windows)]
    let mut hint: Option<&'static str> = None;
    #[cfg(windows)]
    let mut last = std::time::Instant::now();
    #[cfg(windows)]
    let mut was_on_air = false;
    crate::show_until_stopped(shared.as_ref(), &heard, &running, || {
        #[cfg(windows)]
        if let Some(station) = &station {
            for error in station.errors.try_iter() {
                crate::print_event(&format!("sound device error: {error}"));
            }
            // How loud a program's transmit audio was, each time the radio
            // is unkeyed: silence means the program isn't playing into the
            // transmit cable.
            if let (Some(transmit), Some(cable)) = (&station.transmit, &station.transmit_cable) {
                let on_air_now = on_air.load(Ordering::SeqCst);
                if on_air_now && !was_on_air {
                    let _ = transmit.take_peak_dbfs();
                }
                if was_on_air && !on_air_now {
                    crate::print_event(&match transmit.take_peak_dbfs() {
                        Some(db) => format!("transmit audio: {db:.0} dBFS peak"),
                        None => format!(
                            "transmit audio: silent\n-> The program's sound output should be \"{cable}\"."
                        ),
                    });
                }
                was_on_air = on_air_now;
            }
            // The receive level, said only when what to do about it changes.
            if last.elapsed() >= Duration::from_secs(5) {
                last = std::time::Instant::now();
                let now = station.receive_hint();
                if now != hint {
                    crate::print_event(&match now {
                        Some(what) => format!("receive audio: {what}"),
                        None => "receive audio: level good".to_owned(),
                    });
                    hint = now;
                }
            }
        }
    });
    drop(server);
    #[cfg(windows)]
    drop(station);
    println!("stopped");
    Ok(())
}

/// Where Windows keeps the programs it runs at each sign-in, for this user
/// (Microsoft Learn, "Run and `RunOnce` Registry Keys").
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// The Agent's entry under it.
#[cfg(windows)]
const RUN_NAME: &str = "LabLog Station Agent";

/// The command line Windows runs at sign-in: this program's `station`.
/// Windows takes at most 260 characters there.
#[cfg(any(windows, test))]
fn startup_command(exe: &std::path::Path) -> Result<String, String> {
    let command = format!("\"{}\" station", exe.display());
    if command.chars().count() > 260 {
        return Err(format!(
            "the Agent's path is too long for Windows to start it at sign-in ({command})"
        ));
    }
    Ok(command)
}

/// `startup on`, `startup off` or `startup status`: whether Windows starts
/// `station` each time this user signs in.
///
/// # Errors
///
/// Windows' registry couldn't be read or changed, or this isn't Windows.
pub fn startup(what: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let reg = |args: &[&str]| {
            std::process::Command::new("reg")
                .args(args)
                .output()
                .map_err(|error| format!("can't run reg: {error}"))
        };
        match what {
            "on" => {
                let exe = std::env::current_exe()
                    .map_err(|error| format!("can't tell where the Agent is: {error}"))?;
                let command = startup_command(&exe)?;
                let done = reg(&[
                    "add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &command, "/f",
                ])?;
                if !done.status.success() {
                    return Err(format!(
                        "Windows refused: {}",
                        String::from_utf8_lossy(&done.stderr).trim()
                    ));
                }
                println!("Windows will run {command} each time you sign in.");
                println!("Windows may wait a little after sign-in before starting it.");
                Ok(())
            }
            "off" => {
                let done = reg(&["delete", RUN_KEY, "/v", RUN_NAME, "/f"])?;
                if done.status.success() {
                    println!("Windows won't start the Agent at sign-in any more.");
                } else {
                    println!("Windows wasn't starting the Agent at sign-in.");
                }
                Ok(())
            }
            "status" => {
                let done = reg(&["query", RUN_KEY, "/v", RUN_NAME])?;
                let text = String::from_utf8_lossy(&done.stdout);
                match text.lines().find(|line| line.contains("REG_SZ")) {
                    Some(line) if done.status.success() => println!(
                        "Windows starts the Agent at sign-in: {}",
                        line.split("REG_SZ").nth(1).unwrap_or_default().trim()
                    ),
                    _ => println!("Windows doesn't start the Agent at sign-in."),
                }
                Ok(())
            }
            other => Err(format!("startup takes on, off or status, not {other:?}")),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = what;
        Err(
            "startup is for Windows; elsewhere, use the system's own way to start programs"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Typed, compared, compared_all, startup_command, typed};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::engine::{KeptRadio, KeptReading, KeptSetting};
    use crate::rigdef;
    use crate::settings_record;

    #[test]
    fn reads_what_the_operator_types() {
        assert_eq!(typed("  "), Ok(Typed::Nothing));
        assert_eq!(typed("keep FT8"), Ok(Typed::Keep("FT8".to_owned())));
        assert_eq!(
            typed("KEEP  Field Day SSB "),
            Ok(Typed::Keep("Field Day SSB".to_owned()))
        );
        assert_eq!(typed("compare"), Ok(Typed::Compare));
        assert_eq!(typed("help"), Ok(Typed::Help));
        assert_eq!(typed("?"), Ok(Typed::Help));
        assert!(typed("keep").is_err());
        assert!(typed(&format!("keep {}", "x".repeat(41))).is_err());
        assert!(
            typed("restore FT8").is_err(),
            "nothing puts settings back yet"
        );
        assert!(typed("compare FT8").is_err());
    }

    #[test]
    fn shows_how_the_radio_differs_from_each_setup() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let ic_7300 = definitions
            .iter()
            .find(|d| d.model == "IC-7300")
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let port = PretendPort::new(PretendCiv::new(ic_7300), 115_200);
        let mut radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        let now = settings_record::read(
            &mut radio,
            Some("IC-7300_02065962"),
            UNIX_EPOCH + Duration::from_mins(1),
            "FL2",
        );
        let setup = |name: &str, att: &str| KeptReading {
            name: Some(name.to_owned()),
            settings: now
                .settings
                .iter()
                .map(|e| KeptSetting {
                    id: e.id.clone(),
                    label: e.label.clone(),
                    bytes: if e.label == "Attenuator (ATT)" {
                        Some(att.to_owned())
                    } else {
                        e.bytes.clone()
                    },
                })
                .collect(),
            station: "fieldlab-1".to_owned(),
            operator: Some("K9KSU".to_owned()),
            saved_at: "2026-10-27T18:00:00Z".to_owned(),
        };
        assert_eq!(
            compared(&setup("FT8", "00"), &now, ic_7300),
            ["the radio matches setup FT8 (kept 2026-10-27T18:00:00Z at fieldlab-1 by K9KSU)."]
        );
        assert_eq!(
            compared(&setup("SSB", "00 20"), &now, ic_7300),
            [
                "1 differs from setup SSB (kept 2026-10-27T18:00:00Z at fieldlab-1 by K9KSU):",
                "  Attenuator (ATT): the radio has 00; setup SSB has 00 20"
            ]
        );
        let none = KeptRadio {
            as_received: None,
            setups: Vec::new(),
        };
        assert!(compared_all(&none, &now, ic_7300)[0].starts_with("no setups"));
        let two = KeptRadio {
            as_received: None,
            setups: vec![setup("FT8", "00"), setup("SSB", "00 20")],
        };
        assert_eq!(compared_all(&two, &now, ic_7300).len(), 4);
    }

    #[test]
    fn the_sign_in_command_runs_station_and_fits_windows() {
        let exe = std::path::Path::new(r"C:\LabLog\loglab-agent.exe");
        assert_eq!(
            startup_command(exe).as_deref(),
            Ok(r#""C:\LabLog\loglab-agent.exe" station"#)
        );
        let deep = std::path::PathBuf::from("x".repeat(300));
        assert!(startup_command(&deep).is_err(), "over 260 characters");
    }
}
