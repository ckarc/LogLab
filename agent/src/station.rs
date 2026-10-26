// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent station`: everything programs on this computer need to use
//! the radio, from one command.
//!
//! 1. Finds the radio: the serial port whose USB IDs match a known radio's,
//!    confirmed by the radio's own answer to "who are you?" at the speed it
//!    answers at.
//! 2. Checks the radio's settings against what digital modes need, and says
//!    what is wrong and how to fix it on the radio. It changes nothing on
//!    the radio: its definition's settings aren't verified on a real radio
//!    yet, and the Agent never writes an unverified setting.
//! 3. Carries the radio's audio both ways (Windows): receive into VB-CABLE,
//!    and a program's transmit audio from a second virtual cable to the
//!    radio, only while the Agent has it keyed for that program.
//! 4. Points VARA's sound devices at those cables, if VARA isn't running
//!    (see `vara`).
//! 5. Serves the radio to programs on this computer, which may tune it and
//!    key it (`serve --allow-transmit`).
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
use crate::rig::{Radio, RadioError};
use crate::rigctld_server::{self, Shared, Source};
use crate::{rigdef, settings_record};

/// The radio, found and checked.
struct Found {
    radio: CivRadio<SerialPort>,
    port: String,
    serial: Option<String>,
}

/// Finds the radio on `port`, or, with none given, on whichever serial port
/// looks like a known radio's and answers as one.
fn find(port: Option<&str>) -> Result<Found, String> {
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

/// Says what in the radio's settings stops digital modes working.
fn check_settings(radio: &mut CivRadio<SerialPort>) {
    let definition = radio.definition().clone();
    let record = settings_record::read(radio, std::time::SystemTime::now(), "this computer");
    let found = settings_record::findings(&record, &definition);
    if found.is_empty() {
        println!("Radio settings: every one digital modes need is right.");
        return;
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

/// Runs the station until Ctrl-C.
///
/// # Errors
///
/// The radio isn't found, or the virtual radio can't listen.
pub fn run(port: Option<&str>, listen: SocketAddr, limit: Duration) -> Result<(), String> {
    let mut found = find(port)?;
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
    check_settings(&mut found.radio);
    let sound = found.radio.definition().usb_audio.clone();
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
    println!(
        "Programs on this computer may tune and key the {} at {}: one at a time, for at most \
         {} s at once, and the radio's own PTT wins.",
        identity.model,
        server.address(),
        limit.as_secs()
    );
    println!(
        "  VarAC: Settings > RIG: Hamlib for PTT and Frequency Control, host localhost, port {}.",
        server.address().port()
    );
    println!(
        "  WSJT-X: Settings > Radio: Rig Hamlib NET rigctl, Network Server {}, PTT Method CAT.",
        server.address()
    );
    println!("Ctrl-C stops.");
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
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
