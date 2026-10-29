// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A virtual radio for other programs, in Hamlib's `rigctld` protocol.
//!
//! Programs such as WSJT-X, `VarAC` and `fldigi` can reach a radio over the
//! network through Hamlib's documented `rigctld` protocol (in WSJT-X, the
//! radio named "Hamlib NET rigctl"). This server speaks that protocol, so
//! those programs connect to the Agent instead of to the radio, and the
//! Agent stays the radio's one owner (Principle IX).
//!
//! It answers questions about frequency, mode, PTT, VFO and split. Whether
//! programs may also **tune** the radio (frequency, mode, VFO and split), and
//! whether they may **key** it, is the [`Source`]'s choice, made by the
//! operator on the command line until the tray program decides which program
//! controls a radio: by default they may do neither (`RPRT -9`, Hamlib's
//! "command rejected by the rig", which every other change the server doesn't
//! make gets too). With `serve --allow-tuning` they may tune, and with
//! `serve --allow-transmit` they may also key it (the Agent's CI-V driver
//! only).
//!
//! When programs may key the radio, failing safe means not transmitting
//! (Principle IX):
//!
//! - one program at a time has it keyed, and a request to transmit from
//!   another is refused;
//! - the radio's own PTT wins: if the radio is already transmitting when a
//!   program asks, the request is refused, and a request to stop is never
//!   passed to a radio the Agent didn't key;
//! - a program that disconnects while keyed is unkeyed;
//! - the radio is unkeyed once it has been keyed longer than the time limit;
//! - if unkeying fails, the Agent says the radio may still be transmitting
//!   and keeps trying to unkey it; and
//! - the Agent never keys a radio on its own: not on a timer, not as a
//!   retry, not after a reconnect.
//!
//! The protocol has no sign-in, so the server listens only on this computer
//! and refuses connections from anywhere else (Principle XXI). Every line it
//! receives is capped in length (Principle XII).
//!
//! The protocol details here (the shape of the `\dump_state` answer, the
//! extended `+` answer form, and the result codes) follow Hamlib's own
//! `rigctld` and its network client, read in Hamlib's source
//! (<https://github.com/Hamlib/Hamlib>, LGPL-2.1-or-later), as Principle XVI
//! allows. No Hamlib code is in LabLog.

use std::fmt::Write as _;
use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::hamlib::mode_from_hamlib;
use crate::rig::{
    Exposes, Keyable, Mode, Radio, RadioIdentity, Reading, Tunable, Tuning, TxMeters, Vfo,
};

/// The longest command line accepted.
const MAX_LINE: usize = 256;
/// The most programs connected at once.
const MAX_CLIENTS: usize = 8;
/// How long a reading is reused before the radio is asked again, so several
/// programs polling at once don't crowd the radio.
const FRESH: Duration = Duration::from_millis(150);
/// How often the server looks up from waiting to see if it should stop, and
/// checks the transmit time limit.
const TICK: Duration = Duration::from_millis(100);
/// How long after a failed attempt to unkey the radio it is tried again.
const UNKEY_RETRY: Duration = Duration::from_secs(1);

/// Hamlib's result codes the server gives.
const RPRT_OK: i32 = 0;
/// "Input/output error": the radio couldn't be read.
const RPRT_IO: i32 = -6;
/// "Command rejected by the rig": a request to change something.
const RPRT_REJECTED: i32 = -9;
/// "Function not available": a command this server doesn't have.
const RPRT_NOT_AVAILABLE: i32 = -11;
/// "Invalid parameter": a change asked for with a value that makes no sense.
const RPRT_INVALID: i32 = -1;
/// "Protocol error": a line too long or not text.
const RPRT_PROTOCOL: i32 = -8;

/// Hamlib's mode bits for the modes the rig model has (`RIG_MODE_*`).
const ALL_MODES: u64 = 0x1DBF;

/// Where the server's readings come from.
pub trait Source: Send + Sync {
    /// What the radio is.
    fn identity(&self) -> RadioIdentity;

    /// The radio's live state.
    ///
    /// # Errors
    ///
    /// The radio couldn't be read.
    fn reading(&self) -> Result<Reading, String>;

    /// Whether programs may tune the radio through the server.
    fn can_tune(&self) -> bool {
        false
    }

    /// Asks the radio for a change.
    ///
    /// # Errors
    ///
    /// Tuning isn't allowed, or the radio refused.
    fn tune(&self, tuning: Tuning) -> Result<(), String> {
        let _ = tuning;
        Err("tuning isn't allowed here".to_owned())
    }

    /// Whether programs may key the radio through the server.
    fn can_transmit(&self) -> bool {
        false
    }

    /// Keys the radio (`on`) for `program`, or unkeys it.
    ///
    /// # Errors
    ///
    /// Transmitting isn't allowed, something else is transmitting, or the
    /// radio refused.
    fn transmit(&self, program: SocketAddr, on: bool) -> Result<(), String> {
        let _ = (program, on);
        Err("transmitting isn't allowed here".to_owned())
    }

    /// `program` has connected to the server listening at `server`, and
    /// nothing it asks is answered until this returns.
    fn connected(&self, program: SocketAddr, server: SocketAddr) {
        let _ = (program, server);
    }

    /// `program` has disconnected: if it has the radio keyed, unkey it.
    fn gone(&self, program: SocketAddr) {
        let _ = program;
    }

    /// Called every [`TICK`] while the server runs, to keep the transmit time
    /// limit.
    fn watch(&self) {}

    /// Something worth telling the person at the computer: a program came
    /// or went, or asked for a change and what became of it.
    fn note(&self, event: &str) {
        let _ = event;
    }
}

/// What to do about a change the radio refused, in plain words, on its own
/// line under the note.
fn refused_hint(tuning: Tuning, identity: &RadioIdentity) -> String {
    match tuning {
        Tuning::Frequency(_) | Tuning::OtherFrequency(_) => format!(
            "\n-> The {} {} most likely has no band there. A program offering that band \
             (WSJT-X shows \"Rig Control Error\" and reconnects) should have it removed \
             from its band list: in WSJT-X, Settings > Frequencies.",
            identity.maker, identity.model
        ),
        Tuning::Mode(_) => format!(
            "\n-> The {} {} doesn't have that mode, or not on this band. In WSJT-X, \
             Settings > Radio > Mode: None leaves the mode alone.",
            identity.maker, identity.model
        ),
        Tuning::Select(_) | Tuning::Split(_) => {
            "\n-> In WSJT-X, Settings > Radio > Split Operation: None avoids this.".to_owned()
        }
    }
}

/// A change request in plain words.
#[must_use]
pub fn describe(tuning: Tuning) -> String {
    let mhz = |hz: u64| format!("{}.{:06} MHz", hz / 1_000_000, hz % 1_000_000);
    match tuning {
        Tuning::Frequency(hz) => format!("tune to {}", mhz(hz)),
        Tuning::OtherFrequency(hz) => format!("set the other VFO to {}", mhz(hz)),
        Tuning::Mode(mode) => format!("change mode to {}", hamlib_mode(Some(mode))),
        Tuning::Select(Vfo::A) => "select VFO A".to_owned(),
        Tuning::Select(Vfo::B) => "select VFO B".to_owned(),
        Tuning::Split(true) => "turn split on".to_owned(),
        Tuning::Split(false) => "turn split off".to_owned(),
    }
}

/// The radio a [`Shared`] holds: one that can only be read, one programs may
/// tune, or one programs may tune and key.
enum Held {
    Read(Box<dyn Radio + Send>),
    Tune(Box<dyn Tunable + Send>),
    Key(Box<dyn Keyable + Send>),
}

/// The radio is keyed, by the Agent, for a program.
struct Keyed {
    /// The program it was keyed for.
    by: SocketAddr,
    /// When it was keyed.
    since: Instant,
    /// When the Agent next unkeys it unasked: at the time limit, or, after a
    /// failed attempt, a moment later.
    unkey_at: Instant,
    /// An attempt to unkey it failed: it may still be transmitting.
    stuck: bool,
    /// The highest each transmit meter read while keyed, if the radio's
    /// meters can be read.
    peak: Option<TxMeters>,
    /// When the meters are next read.
    next_meters: Instant,
}

/// How often the transmit meters are read while keyed.
const METER_EVERY: Duration = Duration::from_millis(250);
/// The most ALC that is fine, in percent of its scale: Icom suggests about
/// a third of the scale; Winlink asks for little or none.
pub const ALC_HIGHEST: u16 = 33;
/// The highest SWR, in tenths, before the operator is warned.
const SWR_HIGHEST: u16 = 20;

/// The transmit meters' peaks, in words, and what to do if one is too high.
fn meter_note(peak: TxMeters) -> String {
    let mut note = format!(
        "; peak Po {}%, ALC {}% of its scale, SWR {}.{}",
        peak.power,
        peak.alc,
        peak.swr_tenths / 10,
        peak.swr_tenths % 10
    );
    if peak.tuning {
        note.push_str(" (the radio's tuner was tuning, so they say nothing about the drive)");
        return note;
    }
    if peak.alc > ALC_HIGHEST {
        note.push_str(
            "\n-> ALC went over a third of its scale: lower the radio's USB MOD Level, or the \
             program's drive level, until it stays under.",
        );
    }
    if peak.swr_tenths > SWR_HIGHEST {
        note.push_str(
            "\n-> SWR went over 2.0: check the antenna and tuner before transmitting again.",
        );
    }
    note
}

/// Keys `radio` for `program`, unless something else is transmitting.
/// Returns what happened, and what to tell the person at the computer.
fn key(
    radio: &mut dyn Keyable,
    keyed: &mut Option<Keyed>,
    program: SocketAddr,
    limit: Duration,
) -> (Result<(), String>, Option<String>) {
    let refuse = |why: String| {
        let note = format!("asked to transmit: refused ({why})");
        (Err(why), Some(note))
    };
    match keyed {
        Some(now) if now.stuck => {
            return refuse("the radio may still be transmitting: the Agent is unkeying it".into());
        }
        // Asked again while keyed for it: still keyed, nothing to do.
        Some(now) if now.by == program => return (Ok(()), None),
        Some(now) => return refuse(format!("another program ({}) is transmitting", now.by)),
        None => {}
    }
    // The radio's own PTT wins (Principle X): the operator is transmitting.
    match radio.read() {
        Ok(Reading {
            ptt: Some(false), ..
        }) => {}
        Ok(Reading {
            ptt: Some(true), ..
        }) => {
            return refuse(
                "the radio is already transmitting: its own PTT wins until it's released".into(),
            );
        }
        Ok(Reading { ptt: None, .. }) => {
            return refuse("the radio can't say whether it is transmitting".into());
        }
        Err(error) => return refuse(format!("the radio couldn't be read: {error}")),
    }
    let since = Instant::now();
    match radio.transmit(true) {
        Ok(()) => {
            *keyed = Some(Keyed {
                by: program,
                since,
                unkey_at: since + limit,
                stuck: false,
                peak: None,
                next_meters: since + METER_EVERY,
            });
            (
                Ok(()),
                Some(format!("asked to transmit: transmitting ({program})")),
            )
        }
        // It may have keyed anyway: unkey it, and if that fails too, say so
        // and keep trying.
        Err(error) => {
            let mut note = format!("asked to transmit: refused (the radio refused: {error})");
            if let Err(again) = radio.transmit(false) {
                *keyed = Some(Keyed {
                    by: program,
                    since,
                    unkey_at: Instant::now() + UNKEY_RETRY,
                    stuck: true,
                    peak: None,
                    next_meters: since + METER_EVERY,
                });
                note.push_str(&stuck_hint(&again.to_string()));
            }
            (Err(error.to_string()), Some(note))
        }
    }
}

/// Unkeys `radio` if the Agent keyed it, saying `why`. A radio the Agent
/// didn't key is left alone: the operator's own PTT wins.
fn unkey(
    radio: &mut dyn Keyable,
    keyed: &mut Option<Keyed>,
    why: &str,
) -> (Result<(), String>, String) {
    let Some(now) = keyed.as_mut() else {
        return (
            Ok(()),
            format!("{why}: done (the Agent hadn't keyed anything)"),
        );
    };
    match radio.transmit(false) {
        Ok(()) => {
            let held = now.since.elapsed().as_secs_f64();
            let meters = now.peak.map(meter_note).unwrap_or_default();
            *keyed = None;
            (Ok(()), format!("{why}: unkeyed after {held:.1} s{meters}"))
        }
        Err(error) => {
            now.stuck = true;
            now.unkey_at = Instant::now() + UNKEY_RETRY;
            let error = error.to_string();
            let note = format!("{why}: unkeying failed ({error}){}", stuck_hint(&error));
            (Err(error), note)
        }
    }
}

/// A time limit in seconds, in plain words.
fn seconds(limit: Duration) -> String {
    if limit.subsec_millis() == 0 {
        format!("{} s", limit.as_secs())
    } else {
        format!("{:.1} s", limit.as_secs_f64())
    }
}

/// What to do when the radio may still be transmitting.
fn stuck_hint(error: &str) -> String {
    format!(
        "\n-> The radio may still be transmitting ({error}). Check it now, and unkey it \
         there if it is. The Agent keeps trying to unkey it."
    )
}

/// A radio shared between the programs connected to the server. Readings
/// are reused for a moment so several programs polling at once ask the
/// radio only once.
pub struct Shared {
    radio: Mutex<Held>,
    identity: RadioIdentity,
    last: Mutex<Option<(Instant, Reading)>>,
    notes: Mutex<Option<Sender<String>>>,
    keyed: Mutex<Option<Keyed>>,
    limit: Duration,
    on_air: Arc<AtomicBool>,
    /// The meters' peaks during the last transmission that had any.
    last_peak: Mutex<Option<TxMeters>>,
}

impl Shared {
    fn holding(radio: Held, identity: RadioIdentity, limit: Duration) -> Self {
        Self {
            radio: Mutex::new(radio),
            identity,
            last: Mutex::new(None),
            notes: Mutex::new(None),
            keyed: Mutex::new(None),
            limit,
            on_air: Arc::new(AtomicBool::new(false)),
            last_peak: Mutex::new(None),
        }
    }

    /// The transmit meters' peaks during the last transmission the Agent
    /// keyed and read them in.
    #[must_use]
    pub fn last_peak(&self) -> Option<TxMeters> {
        self.last_peak.lock().ok().and_then(|peak| *peak)
    }

    /// Set exactly while the Agent has the radio keyed for a program, so a
    /// program's transmit audio is sent to the radio only then.
    #[must_use]
    pub fn on_air(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.on_air)
    }

    /// Shares `radio` for reading only.
    #[must_use]
    pub fn new(radio: Box<dyn Radio + Send>) -> Self {
        let identity = radio.identity().clone();
        Self::holding(Held::Read(radio), identity, Duration::ZERO)
    }

    /// Sends what happens (programs coming and going, the changes they ask
    /// for) to `notes`, for the person at the computer.
    #[must_use]
    pub fn with_notes(self, notes: Sender<String>) -> Self {
        if let Ok(mut slot) = self.notes.lock() {
            *slot = Some(notes);
        }
        self
    }

    /// Shares `radio`, and lets programs tune it (but never transmit).
    #[must_use]
    pub fn with_tuning(radio: Box<dyn Tunable + Send>) -> Self {
        let identity = radio.identity().clone();
        Self::holding(Held::Tune(radio), identity, Duration::ZERO)
    }

    /// Shares `radio`, and lets programs tune it and key it, one program at
    /// a time, for at most `limit` at once.
    #[must_use]
    pub fn with_transmit(radio: Box<dyn Keyable + Send>, limit: Duration) -> Self {
        let identity = radio.identity().clone();
        Self::holding(Held::Key(radio), identity, limit)
    }

    /// Runs `act` on the radio and who has it keyed, if programs may key it,
    /// then tells the person at the computer what happened. The next reading
    /// comes from the radio, the truth about what changed (Principle X).
    fn keying<T>(
        &self,
        act: impl FnOnce(&mut dyn Keyable, &mut Option<Keyed>) -> (T, Option<String>),
    ) -> Option<T> {
        let mut held = self.radio.lock().ok()?;
        let Held::Key(radio) = &mut *held else {
            return None;
        };
        let mut keyed = self.keyed.lock().ok()?;
        let peak = keyed.as_ref().and_then(|now| now.peak);
        let (done, note) = act(radio.as_mut(), &mut keyed);
        if keyed.is_none()
            && peak.is_some()
            && let Ok(mut last) = self.last_peak.lock()
        {
            *last = peak;
        }
        // Audio goes to the radio only while it is keyed for a program and
        // not stuck: after keying, and never after a request to unkey.
        self.on_air.store(
            keyed.as_ref().is_some_and(|now| !now.stuck),
            Ordering::SeqCst,
        );
        // Let go of the radio before telling anyone.
        drop(keyed);
        drop(held);
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
        if let Some(note) = note {
            self.note(&note);
        }
        Some(done)
    }
}

impl Shared {
    /// Runs `act` on the radio's settings, if its driver can read them, and
    /// only while no program has it keyed: reading every setting takes a
    /// while, and nothing may come between a program and its transmission.
    ///
    /// # Errors
    ///
    /// The radio's settings can't be read by this driver, or it is keyed.
    pub fn settings<T>(&self, act: impl FnOnce(&mut dyn Exposes) -> T) -> Result<T, String> {
        let mut held = self
            .radio
            .lock()
            .map_err(|_| "the radio is unavailable".to_owned())?;
        if self.keyed.lock().map_or(true, |keyed| keyed.is_some()) {
            return Err(
                "the radio is transmitting for a program; try again when it's done".to_owned(),
            );
        }
        let radio: &mut dyn Radio = match &mut *held {
            Held::Read(radio) => radio.as_mut(),
            Held::Tune(radio) => radio.as_mut(),
            Held::Key(radio) => radio.as_mut(),
        };
        let exposes = radio
            .exposes()
            .ok_or("this radio's driver can't read its settings")?;
        let done = act(exposes);
        drop(held);
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
        Ok(done)
    }
}

impl Drop for Shared {
    /// The Agent going away never leaves a radio keyed.
    fn drop(&mut self) {
        self.on_air.store(false, Ordering::SeqCst);
        if let (Ok(Held::Key(radio)), Ok(keyed)) = (self.radio.get_mut(), self.keyed.get_mut())
            && keyed.is_some()
        {
            let _ = radio.transmit(false);
        }
    }
}

impl Source for Shared {
    fn identity(&self) -> RadioIdentity {
        self.identity.clone()
    }

    fn reading(&self) -> Result<Reading, String> {
        let mut last = self
            .last
            .lock()
            .map_err(|_| "the radio's state is unavailable".to_owned())?;
        if let Some((at, reading)) = last.as_ref()
            && at.elapsed() < FRESH
        {
            return Ok(reading.clone());
        }
        let mut radio = self
            .radio
            .lock()
            .map_err(|_| "the radio is unavailable".to_owned())?;
        let reading = match &mut *radio {
            Held::Read(radio) => radio.read(),
            Held::Tune(radio) => radio.read(),
            Held::Key(radio) => radio.read(),
        }
        .map_err(|error| error.to_string())?;
        *last = Some((Instant::now(), reading.clone()));
        Ok(reading)
    }

    fn can_tune(&self) -> bool {
        self.radio
            .lock()
            .is_ok_and(|radio| matches!(*radio, Held::Tune(_) | Held::Key(_)))
    }

    fn tune(&self, tuning: Tuning) -> Result<(), String> {
        let mut held = self
            .radio
            .lock()
            .map_err(|_| "the radio is unavailable".to_owned())?;
        let done = match &mut *held {
            Held::Tune(radio) => radio.tune(tuning),
            Held::Key(radio) => radio.tune(tuning),
            Held::Read(_) => return Err("tuning isn't allowed here".to_owned()),
        }
        .map_err(|error| error.to_string());
        // Let go of the radio before telling anyone.
        drop(held);
        // Whatever happened, the next reading comes from the radio, which is
        // the truth about what changed (Principle X).
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
        self.note(&match &done {
            Ok(()) => format!("asked to {}: done", describe(tuning)),
            Err(why) => format!(
                "asked to {}: refused ({why}){}",
                describe(tuning),
                refused_hint(tuning, &self.identity)
            ),
        });
        done
    }

    fn can_transmit(&self) -> bool {
        self.radio
            .lock()
            .is_ok_and(|radio| matches!(*radio, Held::Key(_)))
    }

    fn transmit(&self, program: SocketAddr, on: bool) -> Result<(), String> {
        let limit = self.limit;
        self.keying(|radio, keyed| {
            if on {
                key(radio, keyed, program, limit)
            } else {
                let (done, note) = unkey(radio, keyed, "asked to stop transmitting");
                (done, Some(note))
            }
        })
        .unwrap_or_else(|| Err("transmitting isn't allowed here".to_owned()))
    }

    fn gone(&self, program: SocketAddr) {
        self.keying(|radio, keyed| {
            if keyed.as_ref().is_some_and(|now| now.by == program) {
                let (_, note) = unkey(radio, keyed, "the program disconnected while transmitting");
                ((), Some(note))
            } else {
                ((), None)
            }
        });
    }

    fn watch(&self) {
        let limit = self.limit;
        self.keying(|radio, keyed| match keyed.as_ref() {
            Some(now) if Instant::now() >= now.unkey_at => {
                let why = if now.stuck {
                    "trying again to unkey the radio".to_owned()
                } else {
                    format!("transmitting longer than the {} limit", seconds(limit))
                };
                let (done, mut note) = unkey(radio, keyed, &why);
                if done.is_ok() && !why.starts_with("trying") {
                    note.push_str(
                        "\n-> If a program needs to transmit longer at a time, start serve with \
                         a longer --tx-limit.",
                    );
                }
                ((), Some(note))
            }
            // While keyed, the meters are read every so often and their
            // peaks kept, to say after the transmission.
            Some(now) if !now.stuck && Instant::now() >= now.next_meters => {
                let read = radio.meters();
                if let Some(now) = keyed.as_mut() {
                    now.next_meters = Instant::now() + METER_EVERY;
                    if let Ok(Some(meters)) = read {
                        let peak = now.peak.get_or_insert_with(TxMeters::default);
                        peak.power = peak.power.max(meters.power);
                        peak.swr_tenths = peak.swr_tenths.max(meters.swr_tenths);
                        peak.alc = peak.alc.max(meters.alc);
                        peak.tuning |= meters.tuning;
                        peak.readings = peak.readings.saturating_add(1);
                    }
                }
                ((), None)
            }
            _ => ((), None),
        });
    }

    fn note(&self, event: &str) {
        if let Ok(notes) = self.notes.lock()
            && let Some(notes) = notes.as_ref()
        {
            let _ = notes.send(event.to_owned());
        }
    }
}

/// A running server.
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accepting: Option<JoinHandle<()>>,
}

impl Server {
    /// Starts listening at `address`, which must be on this computer.
    ///
    /// # Errors
    ///
    /// The address isn't on this computer, or can't be listened on.
    pub fn start(address: SocketAddr, source: Arc<dyn Source>) -> Result<Self, String> {
        if !address.ip().is_loopback() {
            return Err(format!(
                "the rigctld server must listen on this computer, like 127.0.0.1:4532, not {address}: the protocol has no sign-in (Principle XXI)"
            ));
        }
        let listener = TcpListener::bind(address)
            .map_err(|error| format!("can't listen at {address}: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("can't tell where it is listening: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("can't listen at {address}: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let accepting = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || accept(&listener, address, &source, &stop))
        };
        Ok(Self {
            address,
            stop,
            accepting: Some(accepting),
        })
    }

    /// Where it is listening.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

fn accept(
    listener: &TcpListener,
    address: SocketAddr,
    source: &Arc<dyn Source>,
    stop: &Arc<AtomicBool>,
) {
    let clients = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
        source.watch();
        match listener.accept() {
            Ok((stream, peer)) => {
                if !peer.ip().is_loopback() || clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
                    drop(stream);
                    continue;
                }
                clients.fetch_add(1, Ordering::SeqCst);
                source.note(&format!("a program connected ({peer})"));
                let (source, stop, clients) =
                    (Arc::clone(source), Arc::clone(stop), Arc::clone(&clients));
                thread::spawn(move || {
                    source.connected(peer, address);
                    let _ = serve(stream, peer, source.as_ref(), &stop);
                    source.gone(peer);
                    clients.fetch_sub(1, Ordering::SeqCst);
                    source.note(&format!("a program disconnected ({peer})"));
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(TICK),
            Err(_) => thread::sleep(TICK),
        }
    }
}

/// Answers one program, connected from `program`, until it leaves, sends
/// something unreadable, or the server stops.
fn serve(
    stream: TcpStream,
    program: SocketAddr,
    source: &dyn Source,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(TICK))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let mut byte = [0u8; 1];
        match std::io::Read::read(&mut reader, &mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) if byte[0] == b'\n' => {
                let Ok(text) = std::str::from_utf8(&line) else {
                    writer.write_all(format!("RPRT {RPRT_PROTOCOL}\n").as_bytes())?;
                    return Ok(());
                };
                match respond(text, program, source) {
                    Reply::Text(answer) => writer.write_all(answer.as_bytes())?,
                    Reply::Quit => return Ok(()),
                }
                writer.flush()?;
                line.clear();
            }
            Ok(_) if line.len() >= MAX_LINE => {
                writer.write_all(format!("RPRT {RPRT_PROTOCOL}\n").as_bytes())?;
                return Ok(());
            }
            Ok(_) => line.push(byte[0]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// What to send back for one line.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    /// Send this text.
    Text(String),
    /// The program said goodbye.
    Quit,
}

/// The questions the server answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Get {
    Frequency,
    Mode,
    Ptt,
    Vfo,
    Split,
    SplitFrequency,
    PowerStatus,
    CheckVfo,
    DumpState,
    DumpCaps,
}

impl Get {
    fn long_name(self) -> &'static str {
        match self {
            Self::Frequency => "get_freq",
            Self::Mode => "get_mode",
            Self::Ptt => "get_ptt",
            Self::Vfo => "get_vfo",
            Self::Split => "get_split_vfo",
            Self::SplitFrequency => "get_split_freq",
            Self::PowerStatus => "get_powerstat",
            Self::CheckVfo => "chk_vfo",
            Self::DumpState => "dump_state",
            Self::DumpCaps => "dump_caps",
        }
    }
}

/// The reply to one command line from `program`.
#[must_use]
pub fn respond(line: &str, program: SocketAddr, source: &dyn Source) -> Reply {
    let line = line.trim_end_matches('\r');
    let (extended, command) = match line.strip_prefix('+') {
        Some(rest) => (true, rest.trim()),
        None => (false, line.trim()),
    };
    let Some(word) = command.split_whitespace().next() else {
        return Reply::Text(String::new());
    };
    let get = match word {
        "q" | "Q" | "\\quit" => return Reply::Quit,
        "f" | "\\get_freq" => Get::Frequency,
        "m" | "\\get_mode" => Get::Mode,
        "t" | "\\get_ptt" => Get::Ptt,
        "v" | "\\get_vfo" => Get::Vfo,
        "s" | "\\get_split_vfo" => Get::Split,
        "i" | "\\get_split_freq" => Get::SplitFrequency,
        "\\get_powerstat" => Get::PowerStatus,
        "\\chk_vfo" => Get::CheckVfo,
        "\\dump_state" => Get::DumpState,
        "1" | "\\dump_caps" => Get::DumpCaps,
        "T" | "\\set_ptt" => return Reply::Text(ptt(word, command, extended, program, source)),
        _ if source.can_tune() && tuning_command(word) => {
            return Reply::Text(tune(word, command, extended, source));
        }
        _ if changes_something(word) => {
            source.note(&if source.can_tune() {
                format!("asked {command:?}: refused (not something the Agent changes)")
            } else {
                format!("asked {command:?}: refused (read-only)")
            });
            return Reply::Text(result(extended, word, RPRT_REJECTED));
        }
        _ => return Reply::Text(result(extended, word, RPRT_NOT_AVAILABLE)),
    };
    Reply::Text(answer(get, extended, source))
}

/// A PTT request. A request to **stop** transmitting (`T 0`) is always
/// honored, never refused: a program that sends it, as WSJT-X does whenever
/// it closes its connection, must never be told no. Unless programs may key
/// the radio, every request to transmit is refused. Hamlib's 1, 2 and 3 all
/// mean transmit (from the radio's choice of audio, its microphone, or its
/// data input): the radio sends whatever its own data mode setting chooses.
fn ptt(
    word: &str,
    command: &str,
    extended: bool,
    program: SocketAddr,
    source: &dyn Source,
) -> String {
    let on = match command.split_whitespace().nth(1) {
        Some("0") => false,
        Some("1" | "2" | "3") if source.can_transmit() => true,
        _ if source.can_transmit() => return result(extended, word, RPRT_INVALID),
        _ => {
            source.note(
                "asked to transmit: refused (transmitting isn't allowed)\n\
                 -> To let the program key the radio, start serve with --allow-transmit. \
                 Or set the program's PTT to the radio's VOX (in WSJT-X, Settings > Radio > \
                 PTT Method: VOX).",
            );
            return result(extended, word, RPRT_REJECTED);
        }
    };
    if !source.can_transmit() {
        source.note("asked to stop transmitting: done (the Agent hadn't keyed anything)");
        return result(extended, word, RPRT_OK);
    }
    match (source.transmit(program, on), on) {
        (Ok(()), _) => result(extended, word, RPRT_OK),
        (Err(_), true) => result(extended, word, RPRT_REJECTED),
        // Unkeying failed: the radio didn't do it, and the program is told.
        (Err(_), false) => result(extended, word, RPRT_IO),
    }
}

/// The commands that tune: frequency, transmit frequency, mode, VFO, split.
fn tuning_command(word: &str) -> bool {
    matches!(
        word,
        "F" | "\\set_freq"
            | "I"
            | "\\set_split_freq"
            | "M"
            | "\\set_mode"
            | "V"
            | "\\set_vfo"
            | "S"
            | "\\set_split_vfo"
    )
}

/// Reads a tuning command's value and asks for the change.
fn tune(word: &str, command: &str, extended: bool, source: &dyn Source) -> String {
    let value = command.split_whitespace().nth(1).unwrap_or_default();
    let tuning = match word {
        "F" | "\\set_freq" => hertz(value).map(Tuning::Frequency),
        "I" | "\\set_split_freq" => hertz(value).map(Tuning::OtherFrequency),
        "M" | "\\set_mode" => match mode_from_hamlib(value) {
            Some(Mode::Other) | None => None,
            Some(mode) => Some(Tuning::Mode(mode)),
        },
        "V" | "\\set_vfo" => match value {
            "VFOA" | "Main" => Some(Tuning::Select(Vfo::A)),
            "VFOB" | "Sub" => Some(Tuning::Select(Vfo::B)),
            // "The current VFO": nothing to change.
            "currVFO" => return result(extended, word, RPRT_OK),
            _ => None,
        },
        _ => match value {
            "0" => Some(Tuning::Split(false)),
            "1" => Some(Tuning::Split(true)),
            _ => None,
        },
    };
    let code = match tuning {
        None => RPRT_INVALID,
        Some(tuning) => match source.tune(tuning) {
            Ok(()) => RPRT_OK,
            Err(_) => RPRT_REJECTED,
        },
    };
    result(extended, word, code)
}

/// A frequency as Hamlib sends it, such as `14074055.000000`, in whole
/// hertz.
fn hertz(value: &str) -> Option<u64> {
    let hz: f64 = value.parse().ok()?;
    if !hz.is_finite() || !(1.0..=300e9).contains(&hz) {
        return None;
    }
    format!("{hz:.0}").parse().ok()
}

/// Whether a command asks to change the radio: Hamlib's short set commands
/// are upper case, and its long ones start `\set_` or name an action.
fn changes_something(word: &str) -> bool {
    let mut chars = word.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_uppercase(),
        _ => {
            word.starts_with("\\set_")
                || word.starts_with("\\send_")
                || matches!(
                    word,
                    "\\vfo_op" | "\\scan" | "\\reset" | "\\halt" | "\\stop_morse" | "\\uplink"
                )
        }
    }
}

fn result(extended: bool, word: &str, code: i32) -> String {
    if extended {
        format!("{}:\nRPRT {code}\n", word.trim_start_matches('\\'))
    } else {
        format!("RPRT {code}\n")
    }
}

/// The answer to a question, plain or extended.
fn answer(get: Get, extended: bool, source: &dyn Source) -> String {
    let values: Result<Vec<(&str, String)>, String> = match get {
        Get::CheckVfo => Ok(vec![("ChkVFO", "0".to_owned())]),
        Get::PowerStatus => Ok(vec![("Power Status", "1".to_owned())]),
        Get::DumpState => Ok(dump_state(&source.identity())
            .into_iter()
            .map(|line| ("", line))
            .collect()),
        Get::DumpCaps => Ok(dump_caps(&source.identity())),
        _ => source
            .reading()
            .map(|reading| reading_values(get, &reading)),
    };
    let Ok(values) = values else {
        return result(extended, get.long_name(), RPRT_IO);
    };
    let mut out = String::new();
    if extended {
        out.push_str(get.long_name());
        out.push_str(":\n");
    }
    for (name, value) in values {
        if extended && !name.is_empty() {
            let _ = writeln!(out, "{name}: {value}");
        } else {
            out.push_str(&value);
            out.push('\n');
        }
    }
    if extended {
        let _ = writeln!(out, "RPRT {RPRT_OK}");
    }
    out
}

fn reading_values(get: Get, reading: &Reading) -> Vec<(&'static str, String)> {
    let vfo = reading.vfo.clone().unwrap_or_else(|| "VFOA".to_owned());
    match get {
        Get::Frequency => vec![("Frequency", reading.frequency_hz.to_string())],
        Get::SplitFrequency => vec![("TX Frequency", reading.frequency_hz.to_string())],
        Get::Mode => vec![
            ("Mode", hamlib_mode(reading.mode).to_owned()),
            ("Passband", reading.passband_hz.unwrap_or(0).to_string()),
        ],
        Get::Ptt => vec![("PTT", u8::from(reading.ptt == Some(true)).to_string())],
        Get::Vfo => vec![("VFO", vfo)],
        Get::Split => vec![("Split", "0".to_owned()), ("TX VFO", vfo)],
        _ => Vec::new(),
    }
}

/// The rig model's mode by Hamlib's name for it.
fn hamlib_mode(mode: Option<Mode>) -> &'static str {
    match mode {
        Some(Mode::Usb) => "USB",
        Some(Mode::Lsb) => "LSB",
        Some(Mode::Cw) => "CW",
        Some(Mode::CwReverse) => "CWR",
        Some(Mode::Rtty) => "RTTY",
        Some(Mode::RttyReverse) => "RTTYR",
        Some(Mode::Am) => "AM",
        Some(Mode::Fm) => "FM",
        Some(Mode::DataUsb) => "PKTUSB",
        Some(Mode::DataLsb) => "PKTLSB",
        Some(Mode::DataFm) => "PKTFM",
        Some(Mode::Other) | None => "None",
    }
}

/// The description a Hamlib network client reads when it connects, in
/// `rigctld` protocol version 1. The frequency range is the IC-7300's,
/// the only radio the Agent's own driver knows yet.
fn dump_state(identity: &RadioIdentity) -> Vec<String> {
    let range = format!("30000 74800000 0x{ALL_MODES:x} -1 -1 0x3 0x0");
    let tx = format!("1800000 54000000 0x{ALL_MODES:x} 1000 100000 0x3 0x0");
    let mut lines = vec![
        "1".to_owned(), // protocol version
        "2".to_owned(), // Hamlib's number for "NET rigctl"
        "0".to_owned(), // ITU region (no longer used)
        range,
        "0 0 0 0 0 0 0".to_owned(),
        tx,
        "0 0 0 0 0 0 0".to_owned(),
        format!("0x{ALL_MODES:x} 1"), // tuning step: 1 Hz
        "0 0".to_owned(),
        "0xc 2400".to_owned(),    // USB, LSB
        "0x192 500".to_owned(),   // CW, RTTY, CWR, RTTYR
        "0x1c00 3000".to_owned(), // PKTLSB, PKTUSB, PKTFM
        "0x1 6000".to_owned(),    // AM
        "0x20 15000".to_owned(),  // FM
        "0 0".to_owned(),
        "9999".to_owned(), // max RIT
        "9999".to_owned(), // max XIT
        "0".to_owned(),    // max IF shift
        "0".to_owned(),    // announces
        String::new(),     // preamps: none
        String::new(),     // attenuators: none
        "0x0".to_owned(),  // has_get_func
        "0x0".to_owned(),  // has_set_func
        "0x0".to_owned(),  // has_get_level
        "0x0".to_owned(),  // has_set_level
        "0x0".to_owned(),  // has_get_parm
        "0x0".to_owned(),  // has_set_parm
    ];
    lines.extend(
        [
            "vfo_ops=0x0",
            "ptt_type=0x1",
            "targetable_vfo=0x0",
            "has_set_vfo=0",
            "has_get_vfo=1",
            "has_set_freq=1",
            "has_get_freq=1",
            "has_set_conf=0",
            "has_get_conf=0",
            "has_power2mW=0",
            "has_mW2power=0",
            "has_get_ant=0",
            "has_set_ant=0",
            "timeout=1000",
            "rig_model=2",
        ]
        .map(str::to_owned),
    );
    lines.push(format!(
        "rigctld_version=LabLog Station Agent {} for {} {}",
        env!("CARGO_PKG_VERSION"),
        identity.maker,
        identity.model
    ));
    lines.push("done".to_owned());
    lines
}

fn dump_caps(identity: &RadioIdentity) -> Vec<(&'static str, String)> {
    vec![
        ("Caps dump for model", "2".to_owned()),
        ("Model name", identity.model.clone()),
        ("Mfg name", identity.maker.clone()),
        ("Backend version", env!("CARGO_PKG_VERSION").to_owned()),
        (
            "Backend status",
            "LabLog Station Agent, read-only".to_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{METER_EVERY, Reply, Server, Shared, Source, meter_note, respond};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::hamlib::{HamlibRadio, Source as Hamlib};
    use crate::rig::{
        ControlPath, Keyable, Mode, Radio, RadioError, RadioIdentity, Reading, Tunable, Tuning,
        TxMeters,
    };
    use crate::rigdef;

    /// The program the tests speak as.
    const PROGRAM: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 50_001));
    /// Another program.
    const OTHER: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 50_002));

    fn pretend_ic_7300() -> Arc<dyn Source> {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendPort::new(PretendCiv::new(&definitions[0]), 115_200);
        let radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        Arc::new(Shared::new(Box::new(radio)))
    }

    fn text(line: &str) -> String {
        match respond(line, PROGRAM, pretend_ic_7300().as_ref()) {
            Reply::Text(text) => text,
            Reply::Quit => unreachable!("{line} is not quit"),
        }
    }

    #[test]
    fn answers_plain_questions() {
        assert_eq!(text("f"), "14074000\n");
        assert_eq!(text("\\get_freq"), "14074000\n");
        assert_eq!(text("m"), "PKTUSB\n0\n");
        assert_eq!(text("t"), "0\n");
        assert_eq!(text("v"), "VFOA\n");
        assert_eq!(text("s"), "0\nVFOA\n");
        assert_eq!(text("\\chk_vfo"), "0\n");
        assert_eq!(text("\\get_powerstat"), "1\n");
    }

    #[test]
    fn answers_extended_questions() {
        assert_eq!(text("+f"), "get_freq:\nFrequency: 14074000\nRPRT 0\n");
        assert_eq!(text("+m"), "get_mode:\nMode: PKTUSB\nPassband: 0\nRPRT 0\n");
    }

    #[test]
    fn rejects_every_request_to_change_something() {
        for line in [
            "F 7074000",
            "M USB 2400",
            "T 1",
            "V VFOB",
            "S 1 VFOB",
            "\\set_freq 7074000",
            "\\set_ptt 1",
            "+T 1",
            "\\send_morse CQ",
        ] {
            let reply = text(line);
            assert!(reply.ends_with("RPRT -9\n"), "{line}: {reply}");
        }
        assert_eq!(text("+T 1"), "T:\nRPRT -9\n");
    }

    fn tunable_ic_7300() -> Arc<dyn Source> {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendPort::new(PretendCiv::new(&definitions[0]), 115_200);
        let radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        Arc::new(Shared::with_tuning(Box::new(radio)))
    }

    fn say(source: &Arc<dyn Source>, line: &str) -> String {
        match respond(line, PROGRAM, source.as_ref()) {
            Reply::Text(text) => text,
            Reply::Quit => unreachable!("{line} is not quit"),
        }
    }

    #[test]
    fn tunes_when_tuning_is_allowed() {
        let radio = tunable_ic_7300();
        // What WSJT-X sends as soon as it connects.
        assert_eq!(say(&radio, "F 14074055.000000"), "RPRT 0\n");
        assert_eq!(
            say(&radio, "f"),
            "14074055\n",
            "read back from the radio itself"
        );
        assert_eq!(say(&radio, "F 7074000"), "RPRT 0\n");
        assert_eq!(say(&radio, "M USB 2400"), "RPRT 0\n");
        assert_eq!(say(&radio, "m"), "USB\n0\n");
        assert_eq!(say(&radio, "M PKTUSB 0"), "RPRT 0\n");
        assert_eq!(say(&radio, "m"), "PKTUSB\n0\n");
        assert_eq!(say(&radio, "V currVFO"), "RPRT 0\n");
        assert_eq!(say(&radio, "S 0 VFOA"), "RPRT 0\n");
        assert_eq!(say(&radio, "+F 7074000"), "F:\nRPRT 0\n");
    }

    #[test]
    fn a_request_to_stop_transmitting_is_never_refused() {
        // WSJT-X sends T 0 whenever it closes its connection.
        for radio in [tunable_ic_7300(), pretend_ic_7300()] {
            assert_eq!(say(&radio, "T 0"), "RPRT 0\n");
            assert_eq!(say(&radio, "\\set_ptt 0"), "RPRT 0\n");
            assert_eq!(say(&radio, "+T 0"), "T:\nRPRT 0\n");
            assert_eq!(say(&radio, "t"), "0\n");
        }
    }

    #[test]
    fn never_transmits_even_when_tuning_is_allowed() {
        let radio = tunable_ic_7300();
        for line in ["T 1", "\\set_ptt 1", "+T 1", "T 2", "T 3", "T", "T x"] {
            assert!(say(&radio, line).ends_with("RPRT -9\n"), "{line}");
        }
        assert_eq!(say(&radio, "t"), "0\n");
    }

    #[test]
    fn refuses_changes_that_make_no_sense_or_the_radio_refuses() {
        let radio = tunable_ic_7300();
        assert_eq!(say(&radio, "F seven"), "RPRT -1\n");
        assert_eq!(say(&radio, "F -5"), "RPRT -1\n");
        assert_eq!(say(&radio, "M WIDGET 0"), "RPRT -1\n");
        assert_eq!(say(&radio, "F 80000000"), "RPRT -9\n", "beyond the IC-7300");
        assert_eq!(say(&radio, "f"), "14074000\n", "and nothing moved");
        // Other changes it doesn't make are still refused.
        assert_eq!(say(&radio, "L RFPOWER 0.5"), "RPRT -9\n");
    }

    #[test]
    fn tells_the_person_at_the_computer_what_programs_asked() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendPort::new(PretendCiv::new(&definitions[0]), 115_200);
        let radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        let (notes, heard) = std::sync::mpsc::channel();
        let radio: Arc<dyn Source> =
            Arc::new(Shared::with_tuning(Box::new(radio)).with_notes(notes));
        say(&radio, "f");
        say(&radio, "F 7074000");
        say(&radio, "F 80000000");
        say(&radio, "T 1");
        say(&radio, "L RFPOWER 0.5");
        say(&radio, "T 0");
        let heard: Vec<String> = heard.try_iter().collect();
        assert_eq!(
            heard.len(),
            5,
            "questions aren't noted, changes are: {heard:?}"
        );
        assert_eq!(heard[0], "asked to tune to 7.074000 MHz: done");
        assert!(
            heard[1].starts_with("asked to tune to 80.000000 MHz: refused (the radio refused"),
            "{heard:?}"
        );
        assert!(
            heard[2].starts_with("asked to transmit: refused (transmitting isn't allowed)"),
            "{heard:?}"
        );
        assert!(heard[2].contains("--allow-transmit"), "{heard:?}");
        assert!(heard[2].contains("PTT Method: VOX"), "{heard:?}");
        assert!(
            heard[1].contains("Settings > Frequencies"),
            "a refused band says how to fix it: {heard:?}"
        );
        assert!(heard[3].contains("L RFPOWER 0.5"), "{heard:?}");
        assert_eq!(
            heard[4],
            "asked to stop transmitting: done (the Agent hadn't keyed anything)"
        );
    }

    #[test]
    fn says_what_it_does_not_have() {
        assert_eq!(text("\\get_lock_mode"), "RPRT -11\n");
        assert_eq!(text("l RFPOWER"), "RPRT -11\n");
        assert_eq!(
            respond("q", PROGRAM, pretend_ic_7300().as_ref()),
            Reply::Quit
        );
        assert_eq!(text(""), "");
    }

    /// Reads a `\dump_state` answer the way Hamlib's network client does
    /// (`netrigctl_open`), and returns its key=value settings.
    fn read_like_netrigctl(dump: &str) -> Vec<(String, String)> {
        let mut lines = dump.lines();
        let mut next = || {
            lines
                .next()
                .unwrap_or_else(|| unreachable!("dump_state ended early"))
        };
        assert_eq!(next(), "1", "protocol version 1");
        assert!(next().parse::<u32>().is_ok(), "rig model");
        assert!(next().parse::<i32>().is_ok(), "ITU region");
        for _ in 0..2 {
            // RX ranges, then TX ranges, each ending in seven zeros.
            loop {
                let range = next();
                assert_eq!(range.split_whitespace().count(), 7, "{range}");
                if range == "0 0 0 0 0 0 0" {
                    break;
                }
            }
        }
        for _ in 0..2 {
            // Tuning steps, then filters, each "0xMODES N" ending in "0 0".
            loop {
                let pair = next();
                let fields: Vec<&str> = pair.split_whitespace().collect();
                assert_eq!(fields.len(), 2, "{pair}");
                assert!(
                    i64::from_str_radix(fields[0].trim_start_matches("0x"), 16).is_ok(),
                    "{pair}"
                );
                if pair == "0 0" {
                    break;
                }
            }
        }
        for what in ["max RIT", "max XIT", "max IF shift", "announces"] {
            assert!(next().parse::<i64>().is_ok(), "{what}");
        }
        next(); // preamps
        next(); // attenuators
        for _ in 0..6 {
            let mask = next();
            assert!(mask.starts_with("0x"), "{mask}");
        }
        let mut settings = Vec::new();
        loop {
            let line = next();
            if line == "done" {
                break;
            }
            let (key, value) = line
                .split_once('=')
                .unwrap_or_else(|| unreachable!("{line}"));
            settings.push((key.to_owned(), value.to_owned()));
        }
        settings
    }

    #[test]
    fn describes_itself_the_way_hamlib_clients_read_it() {
        let settings = read_like_netrigctl(&text("\\dump_state"));
        let get = |key: &str| {
            settings
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("ptt_type"), Some("0x1"));
        assert_eq!(get("has_get_freq"), Some("1"));
        assert!(get("rigctld_version").is_some_and(|v| v.contains("Icom IC-7300")));
    }

    fn server() -> Server {
        let address: SocketAddr = "127.0.0.1:0".parse().unwrap_or_else(|_| unreachable!());
        Server::start(address, pretend_ic_7300()).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn refuses_to_listen_beyond_this_computer() {
        let address: SocketAddr = "0.0.0.0:0".parse().unwrap_or_else(|_| unreachable!());
        let error = Server::start(address, pretend_ic_7300())
            .err()
            .unwrap_or_default();
        assert!(error.contains("must listen on this computer"), "{error}");
    }

    #[test]
    fn talks_to_a_program_the_way_wsjt_x_connects() {
        let server = server();
        let stream =
            TcpStream::connect(server.address()).unwrap_or_else(|error| unreachable!("{error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| unreachable!("{error}"));
        let mut writer = stream
            .try_clone()
            .unwrap_or_else(|error| unreachable!("{error}"));
        let mut reader = BufReader::new(stream);
        let mut ask = |line: &str, lines: usize| -> Vec<String> {
            writer
                .write_all(format!("{line}\n").as_bytes())
                .unwrap_or_else(|error| unreachable!("{error}"));
            (0..lines)
                .map(|_| {
                    let mut text = String::new();
                    reader
                        .read_line(&mut text)
                        .unwrap_or_else(|error| unreachable!("{error}"));
                    text.trim_end().to_owned()
                })
                .collect()
        };
        assert_eq!(ask("\\chk_vfo", 1), ["0"]);
        let dump = ask("\\dump_state", 44).join("\n");
        assert!(dump.ends_with("done"), "{dump}");
        assert_eq!(ask("f", 1), ["14074000"]);
        assert_eq!(ask("m", 2), ["PKTUSB", "0"]);
        assert_eq!(ask("t", 1), ["0"]);
        assert_eq!(ask("F 7074000", 1), ["RPRT -9"]);
        assert_eq!(ask("f", 1), ["14074000"], "and the radio did not move");
    }

    #[test]
    fn the_agents_own_hamlib_client_reads_through_it() {
        let server = server();
        let mut radio = HamlibRadio::open(&Hamlib::Running {
            address: server.address(),
            model: 2,
        })
        .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(radio.identity().model, "IC-7300");
        assert_eq!(radio.identity().maker, "Icom");
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 14_074_000);
        assert_eq!(reading.mode, Some(Mode::DataUsb));
        assert_eq!(reading.passband_hz, None);
        assert_eq!(reading.ptt, Some(false));
    }

    #[test]
    fn cuts_off_a_line_that_never_ends() {
        let server = server();
        let mut stream =
            TcpStream::connect(server.address()).unwrap_or_else(|error| unreachable!("{error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| unreachable!("{error}"));
        let _ = stream.write_all(&[b'f'; 1000]);
        let mut reply = String::new();
        let _ = BufReader::new(stream).read_line(&mut reply);
        assert_eq!(reply, "RPRT -8\n");
    }

    /// What goes wrong with a test radio.
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    enum Fault {
        #[default]
        None,
        /// It refuses to key.
        Key,
        /// It refuses to unkey.
        Unkey,
        /// It can't say whether it is transmitting.
        PttUnknown,
    }

    /// What a test radio does, seen from outside it.
    #[derive(Default)]
    struct Bench {
        /// Whether it is transmitting, whoever keyed it.
        transmitting: bool,
        /// How many times it was asked to key.
        keyed: usize,
        /// How many times it was asked to unkey.
        unkeyed: usize,
        fault: Fault,
        /// What its meters read while transmitting, if it has any.
        meters: Option<TxMeters>,
    }

    /// A radio the tests can watch and press the PTT of.
    struct TestRadio {
        identity: RadioIdentity,
        bench: Arc<Mutex<Bench>>,
    }

    fn transmitting(on: &Arc<Mutex<Bench>>) -> bool {
        on_bench(on, |bench| bench.transmitting)
    }

    fn on_bench<T>(bench: &Arc<Mutex<Bench>>, look: impl FnOnce(&mut Bench) -> T) -> T {
        let mut bench = bench
            .lock()
            .unwrap_or_else(|_| unreachable!("not poisoned"));
        look(&mut bench)
    }

    impl Radio for TestRadio {
        fn identity(&self) -> &RadioIdentity {
            &self.identity
        }

        fn read(&mut self) -> Result<Reading, RadioError> {
            Ok(on_bench(&self.bench, |bench| Reading {
                frequency_hz: 14_105_000,
                mode: Some(Mode::DataUsb),
                passband_hz: None,
                ptt: (bench.fault != Fault::PttUnknown).then_some(bench.transmitting),
                vfo: None,
            }))
        }
    }

    impl Tunable for TestRadio {
        fn tune(&mut self, _: Tuning) -> Result<(), RadioError> {
            Ok(())
        }
    }

    impl Keyable for TestRadio {
        fn transmit(&mut self, on: bool) -> Result<(), RadioError> {
            on_bench(&self.bench, |bench| {
                if on {
                    bench.keyed += 1;
                    if bench.fault == Fault::Key {
                        return Err(RadioError("no answer".to_owned()));
                    }
                } else {
                    bench.unkeyed += 1;
                    if bench.fault == Fault::Unkey {
                        return Err(RadioError("no answer".to_owned()));
                    }
                }
                bench.transmitting = on;
                Ok(())
            })
        }

        fn meters(&mut self) -> Result<Option<TxMeters>, RadioError> {
            Ok(on_bench(&self.bench, |bench| {
                bench.meters.map(|keyed| {
                    if bench.transmitting {
                        keyed
                    } else {
                        TxMeters::default()
                    }
                })
            }))
        }
    }

    /// A radio programs may key, for at most `limit` at once, the bench it
    /// sits on, and what the person at the computer is told.
    fn keyable(limit: Duration) -> (Arc<Shared>, Arc<Mutex<Bench>>, Receiver<String>) {
        let bench = Arc::new(Mutex::new(Bench::default()));
        let radio = TestRadio {
            identity: RadioIdentity {
                maker: "Icom".to_owned(),
                model: "IC-7300".to_owned(),
                path: ControlPath::Civ,
            },
            bench: Arc::clone(&bench),
        };
        let (notes, heard) = std::sync::mpsc::channel();
        let shared = Shared::with_transmit(Box::new(radio), limit).with_notes(notes);
        (Arc::new(shared), bench, heard)
    }

    fn as_program(source: &Shared, program: SocketAddr, line: &str) -> String {
        match respond(line, program, source) {
            Reply::Text(text) => text,
            Reply::Quit => unreachable!("{line} is not quit"),
        }
    }

    const MINUTE: Duration = Duration::from_mins(1);

    #[test]
    fn reads_settings_only_while_nobody_has_the_radio_keyed() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendPort::new(PretendCiv::new(&definitions[0]), 115_200);
        let radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        let listed = radio.definition().settings.len();
        let shared = Shared::with_transmit(Box::new(radio), MINUTE);
        assert_eq!(
            shared.settings(|radio| radio.definition().settings.len()),
            Ok(listed)
        );
        assert_eq!(as_program(&shared, PROGRAM, "T 1"), "RPRT 0\n");
        assert!(
            shared.settings(|_| ()).is_err(),
            "nothing comes between a program and its transmission"
        );
        assert_eq!(as_program(&shared, PROGRAM, "T 0"), "RPRT 0\n");
        assert!(shared.settings(|_| ()).is_ok());
    }

    #[test]
    fn says_when_a_driver_cant_read_settings() {
        let (radio, _, _) = keyable(MINUTE);
        assert!(radio.settings(|_| ()).is_err());
    }

    #[test]
    fn keys_and_unkeys_when_programs_may_transmit() {
        let (radio, bench, heard) = keyable(MINUTE);
        assert!(radio.can_transmit() && radio.can_tune());
        let on_air = radio.on_air();
        assert!(!on_air.load(Ordering::SeqCst));
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        assert!(transmitting(&bench));
        assert!(on_air.load(Ordering::SeqCst), "transmit audio may flow");
        assert_eq!(as_program(&radio, PROGRAM, "t"), "1\n", "the radio says so");
        assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT 0\n");
        assert!(!transmitting(&bench));
        assert!(!on_air.load(Ordering::SeqCst), "and stops");
        assert_eq!(as_program(&radio, PROGRAM, "t"), "0\n");
        let heard: Vec<String> = heard.try_iter().collect();
        assert_eq!(
            heard[0],
            "asked to transmit: transmitting (127.0.0.1:50001)"
        );
        assert!(
            heard[1].starts_with("asked to stop transmitting: unkeyed after "),
            "{heard:?}"
        );
    }

    #[test]
    fn every_hamlib_transmit_value_keys_and_nothing_else_does() {
        let (radio, bench, _heard) = keyable(MINUTE);
        for on in ["T 1", "T 2", "T 3", "\\set_ptt 1", "+T 1"] {
            assert!(
                as_program(&radio, PROGRAM, on).ends_with("RPRT 0\n"),
                "{on}"
            );
            assert!(transmitting(&bench), "{on}");
            assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT 0\n");
        }
        for nonsense in ["T", "T x", "T 4", "T -1"] {
            assert_eq!(as_program(&radio, PROGRAM, nonsense), "RPRT -1\n");
        }
        assert_eq!(on_bench(&bench, |bench| bench.keyed), 5, "once each");
        assert!(!transmitting(&bench));
    }

    #[test]
    fn asking_again_while_keyed_keys_nothing_more() {
        let (radio, bench, heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        assert_eq!(on_bench(&bench, |bench| bench.keyed), 1);
        assert_eq!(heard.try_iter().count(), 1, "noted once");
    }

    #[test]
    fn one_program_at_a_time_and_any_program_may_stop_it() {
        let (radio, bench, heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        assert_eq!(as_program(&radio, OTHER, "T 1"), "RPRT -9\n");
        assert_eq!(on_bench(&bench, |bench| bench.keyed), 1);
        // A request to stop is never refused, whoever sends it.
        assert_eq!(as_program(&radio, OTHER, "T 0"), "RPRT 0\n");
        assert!(!transmitting(&bench));
        // Now the other program may key it.
        assert_eq!(as_program(&radio, OTHER, "T 1"), "RPRT 0\n");
        let heard: Vec<String> = heard.try_iter().collect();
        assert_eq!(
            heard[1],
            "asked to transmit: refused (another program (127.0.0.1:50001) is transmitting)"
        );
    }

    #[test]
    fn the_radios_own_ptt_wins() {
        let (radio, bench, heard) = keyable(MINUTE);
        // The operator presses the microphone's PTT.
        on_bench(&bench, |bench| bench.transmitting = true);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT -9\n");
        assert_eq!(
            on_bench(&bench, |bench| bench.keyed),
            0,
            "never asked to key"
        );
        // A program's request to stop leaves the operator transmitting.
        assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT 0\n");
        assert_eq!(
            on_bench(&bench, |bench| bench.unkeyed),
            0,
            "never asked to unkey"
        );
        assert!(transmitting(&bench));
        let heard: Vec<String> = heard.try_iter().collect();
        assert!(heard[0].contains("its own PTT wins"), "{heard:?}");
        assert_eq!(
            heard[1],
            "asked to stop transmitting: done (the Agent hadn't keyed anything)"
        );
        // Released: the program may key it.
        on_bench(&bench, |bench| bench.transmitting = false);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
    }

    #[test]
    fn wont_key_a_radio_that_cant_say_whether_it_is_transmitting() {
        let (radio, bench, _heard) = keyable(MINUTE);
        on_bench(&bench, |bench| bench.fault = Fault::PttUnknown);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT -9\n");
        assert_eq!(on_bench(&bench, |bench| bench.keyed), 0);
    }

    #[test]
    fn says_the_meters_peaks_after_a_transmission() {
        let (radio, bench, heard) = keyable(MINUTE);
        on_bench(&bench, |bench| {
            bench.meters = Some(TxMeters {
                power: 100,
                swr_tenths: 13,
                alc: 50,
                ..TxMeters::default()
            });
        });
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        std::thread::sleep(METER_EVERY + Duration::from_millis(20));
        radio.watch();
        assert!(transmitting(&bench), "reading the meters keeps it keyed");
        assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT 0\n");
        let heard: Vec<String> = heard.try_iter().collect();
        assert!(
            heard[1].contains("; peak Po 100%, ALC 50% of its scale, SWR 1.3"),
            "{heard:?}"
        );
        assert!(
            heard[1].contains("lower the radio's USB MOD Level"),
            "{heard:?}"
        );
        // A radio without meters, or a transmission too short to read
        // them, says nothing about them.
        let (radio, _, heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        radio.watch();
        assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT 0\n");
        let heard: Vec<String> = heard.try_iter().collect();
        assert!(!heard[1].contains("peak"), "{heard:?}");
    }

    #[test]
    fn warns_only_when_alc_or_swr_is_too_high() {
        let fine = TxMeters {
            power: 50,
            swr_tenths: 20,
            alc: 33,
            ..TxMeters::default()
        };
        assert_eq!(
            meter_note(fine),
            "; peak Po 50%, ALC 33% of its scale, SWR 2.0"
        );
        let note = meter_note(TxMeters {
            swr_tenths: 21,
            alc: 34,
            ..fine
        });
        assert!(note.contains("ALC went over a third"), "{note}");
        assert!(note.contains("SWR went over 2.0"), "{note}");
        let note = meter_note(TxMeters {
            alc: 65,
            tuning: true,
            ..fine
        });
        assert!(note.contains("the radio's tuner was tuning"), "{note}");
        assert!(
            !note.contains("->"),
            "no advice from the tuner's carrier: {note}"
        );
    }

    #[test]
    fn unkeys_at_the_time_limit_and_never_keys_again_on_its_own() {
        let limit = Duration::from_millis(200);
        let (radio, bench, heard) = keyable(limit);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        radio.watch();
        assert!(transmitting(&bench), "not yet");
        std::thread::sleep(limit + Duration::from_millis(50));
        radio.watch();
        assert!(!transmitting(&bench), "unkeyed at the limit");
        for _ in 0..5 {
            radio.watch();
        }
        assert_eq!(
            on_bench(&bench, |bench| (bench.keyed, bench.unkeyed)),
            (1, 1)
        );
        let heard: Vec<String> = heard.try_iter().collect();
        assert!(
            heard[1].starts_with("transmitting longer than the 0.2 s limit: unkeyed after "),
            "{heard:?}"
        );
        assert!(heard[1].contains("--tx-limit"), "{heard:?}");
        // The program may key it again, by asking.
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
    }

    #[test]
    fn says_the_radio_may_be_transmitting_when_it_wont_unkey_and_keeps_trying() {
        let (radio, bench, heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        on_bench(&bench, |bench| bench.fault = Fault::Unkey);
        assert_eq!(as_program(&radio, PROGRAM, "T 0"), "RPRT -6\n");
        assert!(
            !radio.on_air().load(Ordering::SeqCst),
            "no audio once unkeying was asked for, even if it failed"
        );
        // Nobody may key it while it may still be transmitting.
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT -9\n");
        assert_eq!(on_bench(&bench, |bench| bench.keyed), 1);
        radio.watch();
        assert_eq!(on_bench(&bench, |bench| bench.unkeyed), 1, "a moment first");
        on_bench(&bench, |bench| bench.fault = Fault::None);
        std::thread::sleep(super::UNKEY_RETRY + Duration::from_millis(50));
        radio.watch();
        assert!(!transmitting(&bench), "unkeyed on the retry");
        assert_eq!(
            on_bench(&bench, |bench| bench.keyed),
            1,
            "never keyed again"
        );
        let heard: Vec<String> = heard.try_iter().collect();
        assert!(
            heard[1].starts_with("asked to stop transmitting: unkeying failed (no answer)"),
            "{heard:?}"
        );
        assert!(heard[1].contains("may still be transmitting"), "{heard:?}");
        assert!(heard[2].contains("the Agent is unkeying it"), "{heard:?}");
        assert!(
            heard[3].starts_with("trying again to unkey the radio: unkeyed after "),
            "{heard:?}"
        );
    }

    #[test]
    fn a_radio_that_refuses_to_key_is_unkeyed_to_be_sure() {
        let (radio, bench, _heard) = keyable(MINUTE);
        on_bench(&bench, |bench| bench.fault = Fault::Key);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT -9\n");
        assert_eq!(
            on_bench(&bench, |bench| (bench.keyed, bench.unkeyed)),
            (1, 1)
        );
        on_bench(&bench, |bench| bench.fault = Fault::None);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n", "not stuck");
    }

    #[test]
    fn unkeys_when_the_program_that_keyed_it_goes() {
        let (radio, bench, _heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        radio.gone(OTHER);
        assert!(
            transmitting(&bench),
            "another program going changes nothing"
        );
        radio.gone(PROGRAM);
        assert!(!transmitting(&bench));
        radio.gone(PROGRAM);
        assert_eq!(
            on_bench(&bench, |bench| (bench.keyed, bench.unkeyed)),
            (1, 1)
        );
    }

    #[test]
    fn the_agent_going_away_unkeys() {
        let (radio, bench, _heard) = keyable(MINUTE);
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        drop(radio);
        assert!(!transmitting(&bench));
    }

    #[test]
    fn a_program_disconnecting_while_keyed_is_unkeyed_over_the_network() {
        let (radio, bench, _heard) = keyable(MINUTE);
        let address: SocketAddr = "127.0.0.1:0".parse().unwrap_or_else(|_| unreachable!());
        let server = Server::start(address, radio).unwrap_or_else(|error| unreachable!("{error}"));
        let stream =
            TcpStream::connect(server.address()).unwrap_or_else(|error| unreachable!("{error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| unreachable!("{error}"));
        let mut writer = stream
            .try_clone()
            .unwrap_or_else(|error| unreachable!("{error}"));
        let mut reader = BufReader::new(stream);
        writer
            .write_all(b"T 1\n")
            .unwrap_or_else(|error| unreachable!("{error}"));
        let mut reply = String::new();
        reader
            .read_line(&mut reply)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reply, "RPRT 0\n");
        assert!(transmitting(&bench));
        // The program crashes, or is closed, without unkeying.
        drop(writer);
        drop(reader);
        let deadline = Instant::now() + Duration::from_secs(5);
        while transmitting(&bench) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!transmitting(&bench), "unkeyed when it left");
    }

    #[test]
    fn the_network_server_keeps_the_time_limit() {
        let (radio, bench, _heard) = keyable(Duration::from_millis(200));
        let address: SocketAddr = "127.0.0.1:0".parse().unwrap_or_else(|_| unreachable!());
        let server = Server::start(address, Arc::clone(&radio) as Arc<dyn Source>)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(as_program(&radio, PROGRAM, "T 1"), "RPRT 0\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        while transmitting(&bench) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!transmitting(&bench), "unkeyed by the server's watch");
        drop(server);
    }
}
