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
//! programs may also **tune** the radio (frequency, mode, VFO and split) is
//! the [`Source`]'s choice: by default they may not, and with tuning allowed
//! (`serve --allow-tuning`, for the pretend radio only, until the tray
//! program decides which program controls a radio) they may. **Transmitting
//! is always refused** here, tuning allowed or not: every PTT request gets
//! `RPRT -9`, Hamlib's "command rejected by the rig", and so does every
//! other change the server doesn't make.
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
use crate::rig::{Mode, Radio, RadioIdentity, Reading, Tunable, Tuning, Vfo};

/// The longest command line accepted.
const MAX_LINE: usize = 256;
/// The most programs connected at once.
const MAX_CLIENTS: usize = 8;
/// How long a reading is reused before the radio is asked again, so several
/// programs polling at once don't crowd the radio.
const FRESH: Duration = Duration::from_millis(150);
/// How often the server looks up from waiting to see if it should stop.
const TICK: Duration = Duration::from_millis(100);

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

/// The radio a [`Shared`] holds: one that can only be read, or one programs
/// may tune.
enum Held {
    Read(Box<dyn Radio + Send>),
    Tune(Box<dyn Tunable + Send>),
}

/// A radio shared between the programs connected to the server. Readings
/// are reused for a moment so several programs polling at once ask the
/// radio only once.
pub struct Shared {
    radio: Mutex<Held>,
    identity: RadioIdentity,
    last: Mutex<Option<(Instant, Reading)>>,
    notes: Mutex<Option<Sender<String>>>,
}

impl Shared {
    /// Shares `radio` for reading only.
    #[must_use]
    pub fn new(radio: Box<dyn Radio + Send>) -> Self {
        let identity = radio.identity().clone();
        Self {
            radio: Mutex::new(Held::Read(radio)),
            identity,
            last: Mutex::new(None),
            notes: Mutex::new(None),
        }
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
        Self {
            radio: Mutex::new(Held::Tune(radio)),
            identity,
            last: Mutex::new(None),
            notes: Mutex::new(None),
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
        }
        .map_err(|error| error.to_string())?;
        *last = Some((Instant::now(), reading.clone()));
        Ok(reading)
    }

    fn can_tune(&self) -> bool {
        self.radio
            .lock()
            .is_ok_and(|radio| matches!(*radio, Held::Tune(_)))
    }

    fn tune(&self, tuning: Tuning) -> Result<(), String> {
        let mut held = self
            .radio
            .lock()
            .map_err(|_| "the radio is unavailable".to_owned())?;
        let Held::Tune(radio) = &mut *held else {
            return Err("tuning isn't allowed here".to_owned());
        };
        let done = radio.tune(tuning).map_err(|error| error.to_string());
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
            thread::spawn(move || accept(&listener, &source, &stop))
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

fn accept(listener: &TcpListener, source: &Arc<dyn Source>, stop: &Arc<AtomicBool>) {
    let clients = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
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
                    let _ = serve(stream, source.as_ref(), &stop);
                    clients.fetch_sub(1, Ordering::SeqCst);
                    source.note(&format!("a program disconnected ({peer})"));
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(TICK),
            Err(_) => thread::sleep(TICK),
        }
    }
}

/// Answers one program until it leaves, sends something unreadable, or the
/// server stops.
fn serve(stream: TcpStream, source: &dyn Source, stop: &AtomicBool) -> std::io::Result<()> {
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
                match respond(text, source) {
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

/// The reply to one command line.
#[must_use]
pub fn respond(line: &str, source: &dyn Source) -> Reply {
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
        // Transmitting is refused, always, whatever else is allowed.
        "T" | "\\set_ptt" => return Reply::Text(ptt(word, command, extended, source)),
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
/// it closes its connection, must never be told no. The Agent keys nothing
/// yet, so today there is nothing to unkey. Every request to transmit is
/// refused.
fn ptt(word: &str, command: &str, extended: bool, source: &dyn Source) -> String {
    if command.split_whitespace().nth(1) == Some("0") {
        source.note("asked to stop transmitting: done (the Agent hadn't keyed anything)");
        return result(extended, word, RPRT_OK);
    }
    source.note(
        "asked to transmit: refused (the Agent doesn't transmit yet)\n\
         -> Until it does, in WSJT-X set Settings > Radio > PTT Method: VOX.",
    );
    result(extended, word, RPRT_REJECTED)
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
    use std::net::{SocketAddr, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Reply, Server, Shared, Source, respond};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::hamlib::{HamlibRadio, Source as Hamlib};
    use crate::rig::{Mode, Radio};
    use crate::rigdef;

    fn pretend_ic_7300() -> Arc<dyn Source> {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendPort::new(PretendCiv::new(&definitions[0]), 115_200);
        let radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        Arc::new(Shared::new(Box::new(radio)))
    }

    fn text(line: &str) -> String {
        match respond(line, pretend_ic_7300().as_ref()) {
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
        match respond(line, source.as_ref()) {
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
            heard[2].starts_with("asked to transmit: refused (the Agent doesn't transmit yet)"),
            "{heard:?}"
        );
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
        assert_eq!(respond("q", pretend_ic_7300().as_ref()), Reply::Quit);
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
}
