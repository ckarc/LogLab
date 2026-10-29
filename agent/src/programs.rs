// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Setting up the programs on this computer that use the radio, so each
//! works through the Agent with nothing typed in: WSJT-X and `JS8Call` (see
//! `qt_settings`), fldigi (see `fldigi`), and MMSSTV's receive audio (see
//! `mmsstv`). VARA is set up the same way (see `vara`, called from
//! `station`).
//!
//! A program that is running is left alone, because each writes its own
//! settings when it closes and would undo the change; the Agent says to
//! close it and start the Agent again. A program that isn't installed, or
//! has never been started (so has no settings file yet), is skipped.

use std::net::SocketAddr;
use std::path::Path;

use crate::audio::RECEIVE_CABLE_OUT;
use crate::audio_windows::{Station, device_names};
use crate::settings_file::{self, Change, Outcome};
use crate::vara::as_vara_names_it;
use crate::{fldigi, mmsstv, qt_settings};

/// Sets up every program found, for the radio at `radio` and the audio
/// `station` carries.
pub fn set_up(station: &Station, radio: SocketAddr) {
    let (recording, playing) = device_names();
    for (app, image) in [("WSJT-X", "wsjtx.exe"), ("JS8Call", "js8call.exe")] {
        let Some(path) = qt_settings::find(app) else {
            continue;
        };
        if !closed(app, image) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let (input, output) = qt_settings::sound_devices(&text);
        let wanted = qt_settings::Wanted {
            input: Some(as_vara_names_it(
                RECEIVE_CABLE_OUT,
                input.as_deref(),
                &recording,
            )),
            output: station
                .transmit_cable
                .as_deref()
                .map(|cable| as_vara_names_it(cable, output.as_deref(), &playing)),
            radio,
        };
        say(app, &path, qt_settings::set(&path, &wanted));
    }
    if let Some(path) = fldigi::find()
        && closed("fldigi", "fldigi.exe")
    {
        let wanted = fldigi::Wanted {
            input: Some(fldigi::mme_name(RECEIVE_CABLE_OUT)),
            output: station.transmit_cable.as_deref().map(fldigi::mme_name),
            radio,
        };
        say("fldigi", &path, fldigi::set(&path, &wanted));
    }
    if let Some(path) = mmsstv::find()
        && closed("MMSSTV", "Mmsstv.exe")
    {
        say("MMSSTV", &path, mmsstv::set(&path, RECEIVE_CABLE_OUT));
        println!(
            "  MMSSTV receives through the Agent; it can't transmit through it yet (it keys \
             a radio only through a COM port)."
        );
    }
}

/// Whether `app` (executable `image`) is closed, saying why not if it isn't.
fn closed(app: &str, image: &str) -> bool {
    match settings_file::running(image) {
        Ok(false) => true,
        Ok(true) => {
            println!(
                "{app}: running, so its settings were left alone (it writes them when it \
                 closes).\n  -> Close {app}, then start the Agent again."
            );
            false
        }
        Err(why) => {
            println!("{app}: its settings were left alone ({why}).");
            false
        }
    }
}

/// Tells the operator what became of `app`'s settings file at `path`.
fn say(app: &str, path: &Path, outcome: Result<Outcome, String>) {
    match outcome {
        Ok(Outcome::AlreadySet) => println!("{app}: already set up for the Agent."),
        Ok(Outcome::Changed(changes)) => {
            println!(
                "{app}: set up for the Agent in {} (the file as first found is kept as {}):",
                path.display(),
                settings_file::original(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            for Change { key, before, after } in changes {
                println!(
                    "  {key}: {after:?} (was {})",
                    before.map_or_else(|| "not set".to_owned(), |before| format!("{before:?}"))
                );
            }
        }
        Err(why) => println!("{app}: not set up ({why})."),
    }
}
