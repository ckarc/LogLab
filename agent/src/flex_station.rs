// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `station` with this computer's FlexRadio instead of a radio on a port,
//! while the operator wants it (`flex on`, the tray's "Use FlexRadio"),
//! as `FlexTest` proved it:
//!
//! - The Agent's own sound card, "LabLog Radio" ([`Plugged`]), plugged in
//!   when the station starts and kept until it stops, so a program opened
//!   before the FlexRadio is used already sees it.
//! - The slice SmartSDR on this computer owns, its receive audio into the
//!   card's input and what programs play to the card's output out to the
//!   radio when it transmits, with the Agent's automatic levels
//!   (`flex_rx`, `flex_levels`).
//! - Programs' rig control of that slice on the station's `rigctld`
//!   address, with the Agent's PTT rules (`rigctld_server::Shared`), over
//!   a second connection bound to SmartSDR and kept to its slice
//!   (`flex::FlexRadio::bind_to`).
//! - WSJT-X, `JS8Call`, fldigi, MMSSTV and VARA set up to the card, as
//!   each is closed (`programs`); one that is open is told about.
//!
//! `flex off`, quit, or losing the radio stops it: rig control first
//! (unkeying the radio if a program has it keyed), then the audio, and the
//! slice's DAX settings are put back as they were (Principle X).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::flex::FlexRadio;
use crate::flex_find::{LISTEN, discover, here_toward};
use crate::flex_rx::{FlexRx, pick};
use crate::programs::Sound;
use crate::rigctld_server::{Server, Shared, Source};
use crate::sound_card::{Feed, NAME as CARD, RX, TX, card_side};
use crate::sound_card_windows::Card;

/// The card's input and output as Windows names them now (see
/// `sound_card::card_side`), if Windows shows both.
fn card_names() -> Option<(String, String)> {
    let (recording, playing) = crate::audio_windows::device_names();
    Some((card_side(&recording, RX)?, card_side(&playing, TX)?))
}

/// How often the log hears how the FlexRadio's audio is doing.
const STATUS_EVERY: Duration = Duration::from_mins(1);

/// "LabLog Radio", plugged in or not, and what feeds it.
pub struct Plugged {
    feed: Arc<Feed>,
    card: Option<Card>,
}

impl Plugged {
    /// Plugs in the card, saying how that went; standing alone
    /// (`RadioAgent`), it isn't, and the FlexRadio isn't offered.
    pub fn plug_in() -> Self {
        let feed = Arc::new(Feed::default());
        if crate::product::current().standalone {
            return Self { feed, card: None };
        }
        let card = match Card::plug_in(Arc::clone(&feed)) {
            Ok(card) => {
                println!(
                    "{CARD}: plugged in (usbip port {}), for the FlexRadio: programs record \
                     its {RX} side and play to its {TX} side.",
                    card.port().unwrap_or_default()
                );
                Some(card)
            }
            Err(why) => {
                println!("{CARD}: not plugged in ({why}), so the FlexRadio can't be used.");
                None
            }
        };
        Self { feed, card }
    }
}

impl Plugged {
    /// Whether the card is plugged in.
    pub fn is_plugged_in(&self) -> bool {
        self.card.is_some()
    }
}

impl Drop for Plugged {
    fn drop(&mut self) {
        self.feed.disconnect();
        if let Some(mut card) = self.card.take() {
            match card.unplug() {
                Ok(()) => println!("{CARD}: unplugged."),
                Err(why) => println!("{CARD}: couldn't unplug it ({why})."),
            }
        }
    }
}

/// Uses this computer's FlexRadio until `flex` or `running` turns false
/// or the radio is lost, then lets go of it. Turns `flex` false itself if
/// the radio can't be used or is lost, so the tray unticks it.
pub fn run(
    plugged: &Plugged,
    listen: SocketAddr,
    limit: Duration,
    running: &AtomicBool,
    flex: &AtomicBool,
) {
    let Some(rig) = start(plugged, listen, limit) else {
        flex.store(false, Ordering::SeqCst);
        return;
    };
    let Rig {
        rx,
        server,
        shared,
        heard,
    } = rig;
    let names = card_names();
    let sound = names
        .as_ref()
        .map(|(input, output)| Sound { input, output });
    match sound {
        Some(sound) => {
            println!(
                "FlexRadio: programs record \"{}\" and play to \"{}\".",
                sound.input, sound.output
            );
            tell_open_programs(sound);
            set_up(sound, server.address());
        }
        None => crate::print_event(&format!(
            "FlexRadio: Windows doesn't show {CARD}'s input and output, so programs were \
             left as they are."
        )),
    }
    let on_air = shared.on_air();
    let mut open = crate::station::ProgramsOpen::now();
    let mut told = Instant::now();
    let serving = AtomicBool::new(true);
    let mut lost = false;
    crate::show_until_stopped(shared.as_ref(), &heard, &serving, || {
        if let Some(closed) = open.closed_since() {
            crate::print_event(&format!(
                "{closed} closed, so the Agent sets up the programs that are closed now."
            ));
            if let Some(sound) = sound {
                set_up(sound, server.address());
            }
        } else if crate::sends_as::take_changed()
            && let Some(sound) = sound
        {
            crate::programs::set_up_for(sound, server.address(), crate::winlink::Tuning::Agent);
        }
        if told.elapsed() >= STATUS_EVERY {
            told = Instant::now();
            println!("Flex audio: {}", status(&rx, &plugged.feed));
            println!("Flex levels: {}", rx.levels.describe());
        }
        lost = !rx.connected();
        let off = !flex.load(Ordering::SeqCst) && !on_air.load(Ordering::SeqCst);
        if lost || off || !running.load(Ordering::SeqCst) {
            serving.store(false, Ordering::SeqCst);
        }
    });
    // Rig control first, unkeying the radio if a program has it keyed.
    drop(server);
    for note in heard.try_iter() {
        crate::print_event(&note);
    }
    plugged.feed.disconnect();
    for done in rx.stop() {
        println!("FlexRadio: {done}");
    }
    if lost {
        flex.store(false, Ordering::SeqCst);
        crate::print_event(
            "FlexRadio: off, lost: its audio stopped (SmartSDR closed, or the radio went).\n\
             -> Open SmartSDR with one slice, then use the FlexRadio again.",
        );
    } else {
        crate::print_event("FlexRadio: off; its slice is put back as it was.");
    }
}

/// Sets up the closed programs, VARA among them, for `sound` and rig
/// control at `radio`.
fn set_up(sound: Sound<'_>, radio: SocketAddr) {
    crate::station::vara_for(sound);
    crate::programs::set_up_for(sound, radio, crate::winlink::Tuning::Agent);
}

/// The FlexRadio in use: its audio, and programs' rig control of it.
struct Rig {
    rx: FlexRx,
    server: Server,
    shared: Arc<Shared>,
    heard: mpsc::Receiver<String>,
}

/// Finds this computer's FlexRadio, starts its audio into the card and
/// serves its rig control on `listen`, saying what it did or why it
/// couldn't.
fn start(plugged: &Plugged, listen: SocketAddr, limit: Duration) -> Option<Rig> {
    let failed = |why: &str| {
        crate::print_event(&format!(
            "FlexRadio: off, it couldn't be used: {why}\n\
             -> Open SmartSDR on this computer with one slice, then use the FlexRadio again."
        ));
    };
    if plugged.card.is_none() {
        failed(&format!("{CARD} isn't plugged in (Open log says why)"));
        return None;
    }
    crate::print_event("FlexRadio: looking for the one SmartSDR uses on this computer...");
    let (radio, owner, model) = match discover(LISTEN).and_then(|heard| pick(&heard, here_toward)) {
        Ok(found) => found,
        Err(why) => {
            failed(&why);
            return None;
        }
    };
    let rx = match FlexRx::start(radio, owner) {
        Ok(rx) => rx,
        Err(why) => {
            failed(&why);
            return None;
        }
    };
    let (notes, heard) = mpsc::channel();
    let control = FlexRadio::open(&radio.to_string())
        .map_err(|error| error.0)
        .and_then(|mut control| {
            control
                .bind_to(&rx.client_id, rx.slice.number)
                .map_err(|error| error.0)?;
            Ok(control)
        });
    let control = match control {
        Ok(control) => control,
        Err(why) => {
            let _ = rx.stop();
            failed(&format!("its rig control: {why}"));
            return None;
        }
    };
    let shared = Arc::new(Shared::with_transmit(Box::new(control), limit).with_notes(notes));
    let server = match Server::start(listen, Arc::clone(&shared) as Arc<dyn Source>) {
        Ok(server) => server,
        Err(why) => {
            let _ = rx.stop();
            failed(&format!("rig control can't listen on {listen}: {why}"));
            return None;
        }
    };
    plugged
        .feed
        .connect(Arc::clone(&rx.buffer), Arc::clone(&rx.transmit));
    println!(
        "Found: FlexRadio {model}, slice {} at {}.",
        rx.slice.letter,
        radio.ip()
    );
    println!(
        "FlexRadio: on. Programs record {CARD}'s {RX}, play to its {TX}, and tune and key \
         slice {} at {} (WSJT-X: Rig Hamlib NET rigctl, PTT Method CAT): one at a time, for at \
         most {} s at once. Audio streams: receive 0x{:08X}, transmit 0x{:08X} (the slice was \
         on DAX channel {}; transmit DAX was {}).",
        rx.slice.letter,
        server.address(),
        limit.as_secs(),
        rx.streams.rx,
        rx.streams.tx,
        rx.slice.dax_before,
        rx.slice.tx_dax_before,
    );
    Some(Rig {
        rx,
        server,
        shared,
        heard,
    })
}

/// How the FlexRadio's audio is doing, for the log.
fn status(rx: &FlexRx, feed: &Feed) -> String {
    let peak = rx
        .buffer
        .take_peak_dbfs()
        .map_or_else(|| "silent".to_owned(), |db| format!("peak {db:.1} dBFS"));
    let tx_peak = rx.transmit.take_peak_dbfs().map_or_else(
        || "programs played silence".to_owned(),
        |db| format!("programs' peak {db:.1} dBFS"),
    );
    let listening = if feed.recording() {
        "a program is recording"
    } else {
        "no program recording"
    };
    format!(
        "{listening}; {} packets in, {peak}, {} ms waiting, {} dropouts, {} dropped, {} refused; \
         {} packets out, {tx_peak}, {} ms waiting to go",
        rx.counts.packets.load(Ordering::Relaxed),
        rx.buffer.level().as_millis(),
        rx.buffer.gaps(),
        rx.buffer.dropped(),
        rx.counts.refused.load(Ordering::Relaxed),
        rx.counts.sent.load(Ordering::Relaxed),
        rx.transmit.level().as_millis(),
    )
}

/// Tells the operator about each of WSJT-X and `JS8Call` that is open with
/// other sound devices than `sound`: it keeps them until it closes.
fn tell_open_programs(sound: Sound<'_>) {
    for (app, image) in [("WSJT-X", "wsjtx.exe"), ("JS8Call", "js8call.exe")] {
        let Some(path) = crate::qt_settings::find(app) else {
            continue;
        };
        if crate::settings_file::running(image) != Ok(true) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let (input, output) = crate::qt_settings::sound_devices(&text);
        if input.as_deref() != Some(sound.input) || output.as_deref() != Some(sound.output) {
            crate::print_event(&format!(
                "FlexRadio: {app} is open with other sound devices, and keeps them until it \
                 closes.\n-> Close {app}: the Agent sets it up for the FlexRadio as it closes; \
                 then open it again."
            ));
        }
    }
}
