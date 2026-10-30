// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Setting up the programs on this computer that use the radio, so each
//! works through the Agent with nothing typed in: WSJT-X and `JS8Call` (see
//! `qt_settings`), fldigi (see `fldigi`), MMSSTV (see `mmsstv`), and
//! Winlink Express's VARA radio setup (see `winlink`). VARA is set up the
//! same way (see `vara`, called from `station`). The call and grid square
//! each program sends are the ones the station keeps up to date (see
//! `sends_as`).
//!
//! A program that is running is left alone, because each writes its own
//! settings when it closes and would undo the change; `station` sets it
//! up as soon as it closes, ready for its next start. A program that isn't installed, or
//! has never been started (so has no settings file yet), is skipped.

use std::net::SocketAddr;
use std::path::Path;

use crate::audio::RECEIVE_CABLE_OUT;
use crate::audio_windows::{Station, device_names};
use crate::settings_file::{self, Change, Outcome};
use crate::vara::as_vara_names_it;
use crate::{fldigi, mmsstv, qt_settings, winlink};

/// Sets up every program found, for the radio at `radio` and the audio
/// `station` carries; `tuning` says whether Winlink Express tunes the
/// radio through the Agent's virtual Kenwood.
pub fn set_up(station: &Station, radio: SocketAddr, tuning: winlink::Tuning<'_>) {
    let (recording, playing) = device_names();
    let sends = crate::sends_as::current();
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
            output: Some(as_vara_names_it(
                transmit_to(station),
                output.as_deref(),
                &playing,
            )),
            radio,
            call: sends.call.clone(),
            grid: sends.grid().map(str::to_owned),
        };
        say(app, &path, qt_settings::set(&path, &wanted));
    }
    if let Some(path) = fldigi::find()
        && closed("fldigi", "fldigi.exe")
    {
        let wanted = fldigi::Wanted {
            input: Some(fldigi::mme_name(RECEIVE_CABLE_OUT)),
            output: Some(fldigi::mme_name(transmit_to(station))),
            radio,
            call: sends.call.clone(),
            grid: sends.grid().map(str::to_owned),
        };
        say("fldigi", &path, fldigi::set(&path, &wanted));
    }
    if let Some(path) = winlink::find()
        && closed("Winlink Express", winlink::IMAGE)
    {
        say("Winlink Express", &path, winlink::set(&path, tuning));
    }
    if let Some(path) = mmsstv::find()
        && closed("MMSSTV", "Mmsstv.exe")
    {
        set_up_mmsstv(&path, transmit_to(station));
    }
}

/// Sets MMSSTV up, its settings file at `path`: receiving, and
/// transmitting into `transmit` when this Agent carries the PTT plugin
/// (see `mmsstv`).
fn set_up_mmsstv(path: &Path, transmit: &str) {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let transmits = if let Some(plugin) = mmsstv::PLUGIN {
        match mmsstv::install(folder, plugin) {
            Ok(written) => {
                if written {
                    println!(
                        "MMSSTV: put LabLog's PTT plugin in {}, so MMSSTV keys the radio \
                         through the Agent.",
                        folder.join(mmsstv::PLUGIN_FILE).display()
                    );
                }
                true
            }
            Err(why) => {
                println!("MMSSTV: can't transmit through the Agent ({why}).");
                false
            }
        }
    } else {
        println!(
            "MMSSTV: this Agent was built without LabLog's PTT plugin for MMSSTV, so \
             MMSSTV receives only (use the Agent from CI's Windows build)."
        );
        false
    };
    let wanted = mmsstv::Wanted {
        input: RECEIVE_CABLE_OUT,
        output: transmits.then_some(transmit),
    };
    say("MMSSTV", path, mmsstv::set(path, &wanted));
}

/// Where a program plays its transmit audio: the transmit cable, or
/// without one, straight into the radio's own USB sound device, so it
/// doesn't transmit silence (as VARA does, see `station`).
fn transmit_to(station: &Station) -> &str {
    station
        .transmit_cable
        .as_deref()
        .unwrap_or(&station.radio_playing)
}

/// Whether `app` (executable `image`) is closed, saying why not if it isn't.
fn closed(app: &str, image: &str) -> bool {
    match settings_file::running(image) {
        Ok(false) => true,
        Ok(true) => {
            println!(
                "{app}: running, so its settings were left alone (it writes them when it \
                 closes).\n  -> Close {app}: the Agent sets it up as soon as it closes."
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
