// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FlexRadio radios, reached over the network through the SmartSDR API.
//!
//! A FlexRadio 6000- or 8000-series radio is a network device. It answers on
//! TCP port 4992 in a line-based text protocol that FlexRadio publishes for
//! anyone to use (the "SmartSDR TCP/IP API"). The radio already lets several
//! programs connect at once, so SmartSDR itself keeps working beside the
//! Agent. This driver was written from that published description; it uses
//! no FlexRadio code.
//!
//! In that protocol a client sends numbered commands (`C<n>|...`) and gets
//! numbered answers (`R<n>|<code>|...`). After subscribing, the radio also
//! sends a status line (`S<handle>|...`) whenever something changes, such as
//! a slice's frequency or mode, or the transmitter keying.
//!
//! A FlexRadio has slices (independent receivers) instead of VFOs. The Agent
//! reports the transmit slice, or the active slice when none transmits, and
//! names it in the reading's VFO field, such as "Slice A".
//!
//! [`Ask`] is the whole list of questions the Agent may send, and none of
//! it changes anything. [`Order`] is the whole list of changes: tuning the
//! slice the Agent reports, changing its mode, and keying the transmitter.
//! Orders are sent only through [`Tunable`] and [`Keyable`], so only
//! `serve --allow-tuning` or `--allow-transmit` ever sends one
//! (Principle IX), and whether a change took is what the radio's next status
//! says, not what was asked (Principle X).
//!
//! Every line from the radio is checked before it is believed
//! (Principle XII): lines are capped in size, the number of slices is capped,
//! and frequencies and widths must be in range.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use crate::civ_radio::WriteBack;
use crate::flex_settings::{self, FlexSetting};
use crate::rig::{
    ControlPath, Exposes, Keyable, MAX_FREQUENCY_HZ, Mode, Radio, RadioError, RadioIdentity,
    Reading, Tunable, Tuning, TxMeters,
};
use crate::rigdef::SettingSummary;

/// The port a FlexRadio's API listens on.
pub const PORT: u16 = 4992;
/// The longest line the Agent reads from the radio.
const MAX_LINE: usize = 8192;
/// The most slices the Agent keeps track of (the largest radios have eight).
const MAX_SLICES: usize = 16;
/// How long connecting, and the first answers, may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one read waits for more from the radio.
const READ_WAIT: Duration = Duration::from_millis(20);
/// How often the Agent asks the radio whether it is still there.
const PING_EVERY: Duration = Duration::from_secs(5);
/// How long the radio may say nothing before the Agent gives up on it.
const SILENCE: Duration = Duration::from_secs(15);
/// How long the radio may take to answer an order.
const ANSWER_WAIT: Duration = Duration::from_secs(2);
/// The most meters the Agent keeps track of.
const MAX_METERS: usize = 1024;
/// How old a meter reading may be and still count.
const METER_FRESH: Duration = Duration::from_secs(1);
/// The most meter packets taken in at once.
const MAX_PACKETS: usize = 256;
/// The VITA-49 packet class code of a FlexRadio meter packet.
const METER_CLASS: u16 = 0x8002;
/// A FLEX-6000 or -8000 series radio's full power, in watts.
const FLEX_FULL_WATTS: f64 = 100.0;

/// How long a setting may take to show its new value in the radio's
/// status lines.
const SETTLE: Duration = Duration::from_millis(500);
/// How long the transmit interlock may take to say the radio keyed or
/// unkeyed, after it took the order.
const KEY_WAIT: Duration = Duration::from_secs(3);

/// Everything the Agent may say to the radio. All of it only asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The radio's model and name.
    Info,
    /// Status lines whenever a slice changes.
    Slices,
    /// Status lines whenever the transmitter changes.
    Transmitter,
    /// Status lines whenever the radio as a whole changes; the transmit
    /// interlock (whether it is keyed) is reported with these on some
    /// versions and with the transmitter's on others, so the Agent asks for
    /// both.
    Whole,
    /// Whether the radio is still there.
    Ping,
    /// Where on this computer to send its meter readings (UDP).
    MeterPort(u16),
    /// Status lines describing its meters, and their readings (over UDP).
    Meters,
}

impl Ask {
    /// The command text sent for this question, after `C<n>|`.
    #[must_use]
    pub fn command(self) -> String {
        match self {
            Self::Info => "info".to_owned(),
            Self::Slices => "sub slice all".to_owned(),
            Self::Transmitter => "sub tx all".to_owned(),
            Self::Whole => "sub radio all".to_owned(),
            Self::Ping => "ping".to_owned(),
            Self::MeterPort(port) => format!("client udpport {port}"),
            Self::Meters => "sub meter all".to_owned(),
        }
    }
}

/// Everything the Agent may ask the radio to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// Tune a slice, in hertz.
    Tune {
        /// The slice's number.
        slice: u8,
        /// Its new frequency.
        hz: u64,
    },
    /// Change a slice's mode, by the radio's name for it.
    Mode {
        /// The slice's number.
        slice: u8,
        /// The mode, as [`mode_to_flex`] names it.
        mode: &'static str,
    },
    /// Key the transmitter, or unkey it, through the radio's transmit
    /// interlock. The slice that transmits is the one marked `tx`.
    Transmit(bool),
    /// Put one of [`flex_settings::SETTINGS`] to a value, on a slice
    /// (only for a restore the operator asked for, or `check-writes`).
    Setting {
        /// The setting.
        setting: &'static FlexSetting,
        /// The slice's number.
        slice: u8,
        /// The value, as [`flex_settings::to_text`] gives it.
        text: String,
    },
}

impl Order {
    /// The command text sent for this order, after `C<n>|`. The words are
    /// `FlexLib`'s (`slice tune`, `slice set`); the API's wiki also gives
    /// them shortened (`slice t`, `slice s`).
    #[must_use]
    pub fn command(&self) -> String {
        match self {
            Self::Tune { slice, hz } => format!(
                "slice tune {slice} {}.{:06}",
                hz / 1_000_000,
                hz % 1_000_000
            ),
            Self::Mode { slice, mode } => format!("slice set {slice} mode={mode}"),
            Self::Transmit(on) => format!("xmit {}", u8::from(*on)),
            Self::Setting {
                setting,
                slice,
                text,
            } => flex_settings::set_command(setting, *slice, text),
        }
    }
}

/// What a FlexRadio's refusal code means, in plain words, from the API's
/// wiki pages for `slice`, `xmit` and the known responses.
#[must_use]
pub fn refusal(code: u32) -> String {
    match code {
        0x5000_000C => "that frequency is out of the radio's range".to_owned(),
        0x5000_000D | 0x534C_9999 => "the radio has no such slice".to_owned(),
        0x5000_001D => "the radio has nothing free to tune that far".to_owned(),
        0x5000_002F | 0x5000_0030 | 0x5000_0032 => "the radio has no such mode".to_owned(),
        0x5000_0033 => "a value is out of range".to_owned(),
        0x5000_003D => "this radio cannot transmit".to_owned(),
        other => format!("error {other:08X}"),
    }
}

/// One line from the radio, as far as the Agent cares.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// The protocol version, sent first.
    Version(&'a str),
    /// The answer to command `seq`: its result code (0 is success) and text.
    Answer { seq: u32, code: u32, text: &'a str },
    /// A status line: what changed.
    Status(&'a str),
    /// Anything else (messages, handles); nothing the Agent needs.
    Other,
}

/// Sort one line from the radio.
#[must_use]
pub fn classify(line: &str) -> Line<'_> {
    if let Some(version) = line.strip_prefix('V') {
        return Line::Version(version);
    }
    if let Some(rest) = line.strip_prefix('R') {
        let mut parts = rest.splitn(3, '|');
        let seq = parts.next().and_then(|s| s.parse().ok());
        let code = parts.next().and_then(|c| u32::from_str_radix(c, 16).ok());
        if let (Some(seq), Some(code)) = (seq, code) {
            return Line::Answer {
                seq,
                code,
                text: parts.next().unwrap_or_default(),
            };
        }
        return Line::Other;
    }
    if let Some(rest) = line.strip_prefix('S')
        && let Some((_, status)) = rest.split_once('|')
    {
        return Line::Status(status);
    }
    Line::Other
}

/// The `key=value` words of a status or answer, in order.
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.split([' ', ','])
        .filter_map(|word| word.split_once('='))
}

/// The most `key=value`s kept for one slice or the transmitter.
const MAX_VALUES: usize = 256;
/// The longest key or value kept.
const MAX_VALUE: usize = 64;

/// Keeps one `key=value` from a status line, if it is plain enough and
/// there is room.
fn keep(values: &mut BTreeMap<String, String>, key: &str, value: &str) {
    let plain = |text: &str| text.len() <= MAX_VALUE && text.bytes().all(|b| b.is_ascii_graphic());
    if key.is_empty() || !plain(key) || !plain(value) {
        return;
    }
    if values.len() >= MAX_VALUES && !values.contains_key(key) {
        return;
    }
    values.insert(key.to_owned(), value.to_owned());
}

/// A frequency in megahertz as the radio writes it ("14.074000"), in hertz.
///
/// # Errors
///
/// It is not a decimal number, or not between 1 Hz and [`MAX_FREQUENCY_HZ`].
pub fn parse_mhz(text: &str) -> Result<u64, RadioError> {
    let bad = || {
        RadioError(format!(
            "the radio reported a frequency that makes no sense: {text:?}"
        ))
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || whole.len() > 6 || !digits(whole) || !digits(fraction) {
        return Err(bad());
    }
    let mut micro = String::from(fraction);
    micro.truncate(6);
    while micro.len() < 6 {
        micro.push('0');
    }
    let whole: u64 = whole.parse().map_err(|_| bad())?;
    let micro: u64 = micro.parse().map_err(|_| bad())?;
    let hz = whole * 1_000_000 + micro;
    if hz == 0 || hz > MAX_FREQUENCY_HZ {
        return Err(bad());
    }
    Ok(hz)
}

/// The rig model's name for a FlexRadio mode name.
#[must_use]
pub fn mode_from_flex(name: &str) -> Option<Mode> {
    Some(match name {
        "" => return None,
        "USB" => Mode::Usb,
        "LSB" => Mode::Lsb,
        "CW" => Mode::Cw,
        "RTTY" => Mode::Rtty,
        // Synchronous AM is still AM on the air.
        "AM" | "SAM" => Mode::Am,
        "FM" | "NFM" | "DFM" => Mode::Fm,
        "DIGU" => Mode::DataUsb,
        "DIGL" => Mode::DataLsb,
        _ => Mode::Other,
    })
}

/// The FlexRadio's name for a rig-model mode, if it has the mode. Names
/// from the API's list of supported modes; the radio reports them in
/// capitals, as they are sent.
#[must_use]
pub const fn mode_to_flex(mode: Mode) -> Option<&'static str> {
    Some(match mode {
        Mode::Usb => "USB",
        Mode::Lsb => "LSB",
        Mode::Cw => "CW",
        Mode::Rtty => "RTTY",
        Mode::Am => "AM",
        Mode::Fm => "FM",
        Mode::DataUsb => "DIGU",
        Mode::DataLsb => "DIGL",
        Mode::CwReverse | Mode::RttyReverse | Mode::DataFm | Mode::Other => return None,
    })
}

/// What the Agent knows about one slice, from its status lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Slice {
    frequency_hz: Option<u64>,
    mode: Option<String>,
    filter_lo: Option<i64>,
    filter_hi: Option<i64>,
    letter: Option<String>,
    transmit: bool,
    active: bool,
    /// Every other `key=value` it reported, for its settings.
    values: BTreeMap<String, String>,
}

/// The radio's live state, built up from its status lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Board {
    slices: BTreeMap<u8, Slice>,
    ptt: Option<bool>,
    /// The transmit interlock's state, as it last said it.
    interlock: Option<String>,
    /// The meters' numbers, by what the Agent reads them for.
    forward_meter: Option<u16>,
    swr_meter: Option<u16>,
    /// How many meters the radio has described.
    meters_seen: BTreeSet<u16>,
    /// The transmitter's `key=value`s, for its settings.
    transmit: BTreeMap<String, String>,
}

impl Board {
    /// Take in one status line.
    ///
    /// # Errors
    ///
    /// The line names a frequency that makes no sense, or more slices than
    /// any radio has.
    pub fn take(&mut self, status: &str) -> Result<(), RadioError> {
        let mut words = status.split(' ');
        match words.next() {
            Some("slice") => {
                let Some(index) = words.next().and_then(|i| i.parse::<u8>().ok()) else {
                    return Ok(());
                };
                let rest = status.splitn(3, ' ').nth(2).unwrap_or_default();
                self.take_slice(index, rest)
            }
            // "transmit band ..." lines are per-band settings, not these.
            Some("transmit") if words.next() != Some("band") => {
                for (key, value) in pairs(status) {
                    keep(&mut self.transmit, key, value);
                }
                Ok(())
            }
            Some("meter") => {
                self.take_meters(status.get("meter ".len()..).unwrap_or_default());
                Ok(())
            }
            Some("interlock") => {
                for (key, value) in pairs(status) {
                    if key == "state"
                        && value.len() <= 24
                        && value.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                    {
                        // UNKEY_REQUESTED: still on the air until it unkeys.
                        self.ptt = Some(matches!(value, "TRANSMITTING" | "UNKEY_REQUESTED"));
                        self.interlock = Some(value.to_owned());
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn take_slice(&mut self, index: u8, rest: &str) -> Result<(), RadioError> {
        if pairs(rest).any(|(key, value)| key == "in_use" && value == "0") {
            self.slices.remove(&index);
            return Ok(());
        }
        if !self.slices.contains_key(&index) && self.slices.len() >= MAX_SLICES {
            return Err(RadioError(
                "the radio reported more slices than any radio has".to_owned(),
            ));
        }
        let slice = self.slices.entry(index).or_default();
        for (key, value) in pairs(rest) {
            match key {
                "RF_frequency" => slice.frequency_hz = Some(parse_mhz(value)?),
                "mode" if value.len() <= 8 && value.bytes().all(|b| b.is_ascii_alphanumeric()) => {
                    slice.mode = Some(value.to_owned());
                }
                "filter_lo" => slice.filter_lo = value.parse().ok(),
                "filter_hi" => slice.filter_hi = value.parse().ok(),
                "index_letter"
                    if value.len() == 1 && value.bytes().all(|b| b.is_ascii_uppercase()) =>
                {
                    slice.letter = Some(value.to_owned());
                }
                "tx" => slice.transmit = value == "1",
                "active" => slice.active = value == "1",
                _ => {}
            }
            keep(&mut slice.values, key, value);
        }
        Ok(())
    }

    /// Take in the radio's description of its meters: `#`-separated
    /// `<number>.<field>=<value>` words. The Agent keeps only which numbers
    /// are the transmitter's forward power (in dBm) and SWR.
    fn take_meters(&mut self, text: &str) {
        let mut described: BTreeMap<u16, BTreeMap<&str, &str>> = BTreeMap::new();
        for word in text.split('#') {
            let Some((name, value)) = word.split_once('=') else {
                continue;
            };
            let Some((number, field)) = name.trim().split_once('.') else {
                continue;
            };
            let Ok(number) = number.parse::<u16>() else {
                continue;
            };
            if !self.meters_seen.contains(&number) && self.meters_seen.len() >= MAX_METERS {
                continue;
            }
            self.meters_seen.insert(number);
            described
                .entry(number)
                .or_default()
                .insert(field, value.trim_matches('"'));
        }
        for (number, fields) in described {
            let field = |name: &str| fields.get(name).copied().unwrap_or_default();
            if !field("src").starts_with("TX") {
                continue;
            }
            match (field("nam"), field("unit").to_ascii_lowercase().as_str()) {
                ("FWDPWR", "dbm") => self.forward_meter = Some(number),
                ("SWR", "swr") => self.swr_meter = Some(number),
                _ => {}
            }
        }
    }

    /// The value the radio last reported for `setting`, as text.
    fn value(&self, setting: &FlexSetting) -> Option<String> {
        let slice = || self.chosen().map(|(_, slice)| &slice.values);
        match setting.object {
            flex_settings::Object::Transmit => self.transmit.get(setting.status).cloned(),
            flex_settings::Object::Slice => slice()?.get(setting.status).cloned(),
            flex_settings::Object::Filter => {
                let values = slice()?;
                Some(format!(
                    "{} {}",
                    values.get("filter_lo")?,
                    values.get("filter_hi")?
                ))
            }
        }
    }

    /// The slice that matters: the transmit slice, else the active one,
    /// else the first.
    fn chosen(&self) -> Option<(&u8, &Slice)> {
        self.slices
            .iter()
            .find(|(_, s)| s.transmit)
            .or_else(|| self.slices.iter().find(|(_, s)| s.active))
            .or_else(|| self.slices.iter().next())
    }

    /// The reading for the slice that matters: the transmit slice, else the
    /// active one, else the first.
    ///
    /// # Errors
    ///
    /// No slice is open, or it has not said its frequency yet.
    pub fn reading(&self) -> Result<Reading, RadioError> {
        let Some((index, slice)) = self.chosen() else {
            return Err(RadioError(
                "no slice is open on the FlexRadio; open one in SmartSDR".to_owned(),
            ));
        };
        let frequency_hz = slice
            .frequency_hz
            .ok_or_else(|| RadioError("the FlexRadio has not said its frequency yet".to_owned()))?;
        let passband_hz = match (slice.filter_lo, slice.filter_hi) {
            (Some(lo), Some(hi)) if hi > lo => u32::try_from(hi - lo)
                .ok()
                .filter(|width| (1..=1_000_000).contains(width)),
            _ => None,
        };
        let letter = slice
            .letter
            .clone()
            .unwrap_or_else(|| char::from(b'A'.saturating_add(*index)).to_string());
        Ok(Reading {
            frequency_hz,
            mode: slice.mode.as_deref().and_then(mode_from_flex),
            passband_hz,
            ptt: self.ptt,
            vfo: Some(format!("Slice {letter}")),
        })
    }
}

/// The meter readings in one VITA-49 packet from the radio, as (meter
/// number, raw value) pairs: empty unless it is a whole meter packet.
///
/// The header's layout is VITA-49's: the first word gives the packet type
/// (top 4 bits), whether a class ID and a trailer follow (bits 27 and 26),
/// the timestamps (bits 23-22 and 21-20) and the size in 32-bit words (low
/// 16 bits). Types 1 and 3 carry a stream ID. The class ID's second word
/// ends in the packet class code; a meter packet's is 0x8002, and its
/// payload is pairs of a 16-bit meter number and a 16-bit signed value
/// (the API's Metering Protocol page; `AetherSDR`'s reading of it).
#[must_use]
pub fn meter_readings(packet: &[u8]) -> Vec<(u16, i16)> {
    let word = |at: usize| {
        packet
            .get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_be_bytes)
    };
    let Some(header) = word(0) else {
        return Vec::new();
    };
    let size = usize::try_from(header & 0xFFFF).unwrap_or(usize::MAX) * 4;
    let has_class = header & 0x0800_0000 != 0;
    if !has_class || size > packet.len() {
        return Vec::new();
    }
    let packet = &packet[..size];
    let mut at = 4;
    if matches!(header >> 28, 1 | 3) {
        at += 4;
    }
    if word(at + 4).map(|w| w & 0xFFFF) != Some(u32::from(METER_CLASS)) {
        return Vec::new();
    }
    at += 8;
    if header & 0x00C0_0000 != 0 {
        at += 4;
    }
    if header & 0x0030_0000 != 0 {
        at += 8;
    }
    let end = size.saturating_sub(if header & 0x0400_0000 != 0 { 4 } else { 0 });
    packet
        .get(at..end)
        .unwrap_or_default()
        .chunks_exact(4)
        .map(|pair| {
            (
                u16::from_be_bytes([pair[0], pair[1]]),
                i16::from_be_bytes([pair[2], pair[3]]),
            )
        })
        .collect()
}

/// Forward power in percent of `full_watts`, from a meter's raw dBm
/// (two's complement, 1/128 dB steps: the API's Meter Units).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn power_percent(raw_dbm: i16, full_watts: f64) -> u16 {
    let watts = 10_f64.powf(f64::from(raw_dbm) / 1280.0) / 1000.0;
    // Clamped to 0..=999 first, so the cast loses nothing.
    (watts / full_watts * 100.0).round().clamp(0.0, 999.0) as u16
}

/// SWR in tenths, from a meter's raw value (1/128 steps, as dBm's: the
/// steps `FlexLib` and `AetherSDR` read it in).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn swr_tenths(raw: i16) -> u16 {
    // Clamped to 10..=9990 first (1.0 to 999.0), so the cast loses nothing.
    (f64::from(raw) / 12.8).round().clamp(10.0, 9990.0) as u16
}

/// The radio's model from its `info` answer, if it is plain enough to show.
#[must_use]
pub fn model_from_info(text: &str) -> Option<String> {
    pairs(text)
        .find(|(key, _)| *key == "model")
        .map(|(_, value)| value.trim_matches('"'))
        .filter(|model| {
            !model.is_empty()
                && model.len() <= 40
                && model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b' ')
        })
        .map(str::to_owned)
}

/// A FlexRadio, read through its API.
pub struct FlexRadio {
    identity: RadioIdentity,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    pending: Vec<u8>,
    board: Board,
    seq: u32,
    heard_at: Instant,
    pinged_at: Instant,
    /// Where the radio sends meter readings, once asked for them.
    meters: Option<UdpSocket>,
    /// Whether asking for them was tried (it is tried once).
    meters_asked: bool,
    /// The radio's address: meter packets from anywhere else are ignored.
    radio_ip: IpAddr,
    /// The last forward power and SWR readings, raw, and when.
    forward: Option<(i16, Instant)>,
    swr: Option<(i16, Instant)>,
    /// Its settings, as the record and restore code takes them.
    definition: crate::rigdef::Definition,
}

impl FlexRadio {
    /// Connect to the radio at `address` and start following it.
    ///
    /// # Errors
    ///
    /// The radio can't be reached, or did not answer like a FlexRadio.
    pub fn open(address: &str) -> Result<Self, RadioError> {
        let target = resolve(address)?;
        let stream = TcpStream::connect_timeout(&target, OPEN_TIMEOUT).map_err(|error| {
            RadioError(format!(
                "cannot reach the FlexRadio at {target}: {error}; is it on, and is the address right?"
            ))
        })?;
        let lost = |error: io::Error| RadioError(format!("lost touch with the FlexRadio: {error}"));
        stream.set_nodelay(true).map_err(lost)?;
        stream.set_read_timeout(Some(READ_WAIT)).map_err(lost)?;
        let writer = stream.try_clone().map_err(lost)?;
        let now = Instant::now();
        let mut radio = Self {
            identity: RadioIdentity {
                maker: "FlexRadio".to_owned(),
                model: "FlexRadio".to_owned(),
                path: ControlPath::Flex,
            },
            reader: BufReader::new(stream),
            writer,
            pending: Vec::new(),
            board: Board::default(),
            seq: 0,
            heard_at: now,
            pinged_at: now,
            meters: None,
            meters_asked: false,
            radio_ip: target.ip(),
            forward: None,
            swr: None,
            definition: flex_settings::definition("FlexRadio"),
        };
        let info = radio.send(Ask::Info)?;
        let slices = radio.send(Ask::Slices)?;
        let transmitter = radio.send(Ask::Transmitter)?;
        let whole = radio.send(Ask::Whole)?;
        let deadline = Instant::now() + OPEN_TIMEOUT;
        let mut waiting = vec![info, slices, transmitter, whole];
        let mut versioned = false;
        while !waiting.is_empty() {
            if Instant::now() > deadline {
                return Err(RadioError(
                    "the radio at that address did not answer like a FlexRadio".to_owned(),
                ));
            }
            let Some(line) = radio.next_line()? else {
                continue;
            };
            match classify(&line) {
                Line::Version(_) => versioned = true,
                Line::Answer { seq, code, text } => {
                    waiting.retain(|s| *s != seq);
                    if seq == info && code == 0 {
                        if let Some(model) = model_from_info(text) {
                            radio.identity.model = model;
                        }
                    } else if seq == slices && code != 0 {
                        return Err(RadioError(format!(
                            "the FlexRadio would not report its slices (error {code:08X})"
                        )));
                    }
                }
                Line::Status(status) => radio.board.take(status)?,
                Line::Other => {}
            }
        }
        if !versioned {
            return Err(RadioError(
                "the radio at that address did not say it is a FlexRadio".to_owned(),
            ));
        }
        radio.definition = flex_settings::definition(&radio.identity.model);
        Ok(radio)
    }

    /// Send one question; return its number.
    fn send(&mut self, ask: Ask) -> Result<u32, RadioError> {
        self.send_text(&ask.command())
    }

    /// Send one order and wait for the radio to take it or refuse it,
    /// following its status lines meanwhile.
    fn order(&mut self, order: &Order) -> Result<(), RadioError> {
        self.command(&order.command())
    }

    /// Send one command and wait for the radio to take it or refuse it,
    /// following its status lines meanwhile.
    fn command(&mut self, command: &str) -> Result<(), RadioError> {
        let seq = self.send_text(command)?;
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            if Instant::now() > deadline {
                return Err(RadioError(format!(
                    "the FlexRadio did not answer {command:?}"
                )));
            }
            let Some(line) = self.next_line()? else {
                continue;
            };
            match classify(&line) {
                Line::Answer {
                    seq: answered,
                    code,
                    ..
                } if answered == seq => {
                    return if code == 0 {
                        Ok(())
                    } else {
                        Err(RadioError(format!(
                            "the FlexRadio refused {command:?}: {}",
                            refusal(code)
                        )))
                    };
                }
                Line::Status(status) => self.board.take(status)?,
                _ => {}
            }
        }
    }

    /// Asks the radio to send its meter readings to a UDP port on this
    /// computer, the first time meters are wanted.
    fn ask_for_meters(&mut self) -> Result<(), RadioError> {
        let lost = |error: io::Error| RadioError(format!("can't take FlexRadio meters: {error}"));
        let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(lost)?;
        socket.set_nonblocking(true).map_err(lost)?;
        let port = socket.local_addr().map_err(lost)?.port();
        self.command(&Ask::MeterPort(port).command())?;
        self.command(&Ask::Meters.command())?;
        self.meters = Some(socket);
        Ok(())
    }

    /// Take in the meter packets that have arrived.
    fn take_meter_packets(&mut self) {
        let Some(socket) = &self.meters else {
            return;
        };
        let mut packet = vec![0_u8; 65_536];
        for _ in 0..MAX_PACKETS {
            let Ok((size, from)) = socket.recv_from(&mut packet) else {
                break;
            };
            if from.ip() != self.radio_ip {
                continue;
            }
            let now = Instant::now();
            for (number, value) in meter_readings(&packet[..size]) {
                if Some(number) == self.board.forward_meter {
                    self.forward = Some((value, now));
                } else if Some(number) == self.board.swr_meter {
                    self.swr = Some((value, now));
                }
            }
        }
    }

    /// Take in every line that has arrived.
    fn take_in(&mut self) -> Result<(), RadioError> {
        while let Some(line) = self.next_line()? {
            if let Line::Status(status) = classify(&line) {
                self.board.take(status)?;
            }
        }
        Ok(())
    }

    /// Send one command line; return its number.
    fn send_text(&mut self, command: &str) -> Result<u32, RadioError> {
        self.seq = self.seq.wrapping_add(1);
        let line = format!("C{}|{command}\n", self.seq);
        self.writer
            .write_all(line.as_bytes())
            .map_err(|error| RadioError(format!("lost touch with the FlexRadio: {error}")))?;
        Ok(self.seq)
    }

    /// The next whole line, or `None` when nothing more has arrived yet.
    fn next_line(&mut self) -> Result<Option<String>, RadioError> {
        let room = u64::try_from(MAX_LINE + 1 - self.pending.len()).unwrap_or(0);
        match (&mut self.reader)
            .take(room)
            .read_until(b'\n', &mut self.pending)
        {
            Ok(0) => Err(RadioError("the FlexRadio closed the connection".to_owned())),
            Ok(_) if self.pending.last() == Some(&b'\n') => {
                self.heard_at = Instant::now();
                let mut bytes = std::mem::take(&mut self.pending);
                bytes.pop();
                let line = String::from_utf8(bytes).map_err(|_| {
                    RadioError("the FlexRadio sent something that is not text".to_owned())
                })?;
                Ok(Some(line.trim_end_matches('\r').to_owned()))
            }
            Ok(_) if self.pending.len() > MAX_LINE => Err(RadioError(
                "the FlexRadio sent a line that is too long".to_owned(),
            )),
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(RadioError(format!(
                "lost touch with the FlexRadio: {error}"
            ))),
        }
    }
}

impl Radio for FlexRadio {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn exposes(&mut self) -> Option<&mut dyn Exposes> {
        Some(self)
    }

    fn read(&mut self) -> Result<Reading, RadioError> {
        // Take in everything that has arrived since the last read.
        self.take_in()?;
        if self.heard_at.elapsed() > SILENCE {
            return Err(RadioError("the FlexRadio stopped answering".to_owned()));
        }
        if self.pinged_at.elapsed() >= PING_EVERY {
            self.send(Ask::Ping)?;
            self.pinged_at = Instant::now();
        }
        self.board.reading()
    }
}

impl Tunable for FlexRadio {
    /// Tunes the slice the Agent reports (the transmit slice, else the
    /// active one), or changes its mode. A FlexRadio has slices, not VFO A
    /// and B, so the other VFO, choosing a VFO and split are refused: split
    /// on a FlexRadio is a second slice, set up in SmartSDR.
    fn tune(&mut self, tuning: Tuning) -> Result<(), RadioError> {
        self.take_in()?;
        let slice = *self
            .board
            .chosen()
            .ok_or_else(|| {
                RadioError("no slice is open on the FlexRadio; open one in SmartSDR".to_owned())
            })?
            .0;
        let order = match tuning {
            Tuning::Frequency(hz) if hz == 0 || hz > MAX_FREQUENCY_HZ => {
                return Err(RadioError(format!("{hz} Hz is not a frequency")));
            }
            Tuning::Frequency(hz) => Order::Tune { slice, hz },
            Tuning::Mode(mode) => Order::Mode {
                slice,
                mode: mode_to_flex(mode)
                    .ok_or_else(|| RadioError(format!("a FlexRadio has no {mode:?} mode")))?,
            },
            Tuning::OtherFrequency(_) | Tuning::Select(_) | Tuning::Split(_) => {
                return Err(RadioError(
                    "a FlexRadio has slices, not VFO A and B; split is a second slice, set up in SmartSDR"
                        .to_owned(),
                ));
            }
        };
        self.order(&order)
    }
}

impl Keyable for FlexRadio {
    /// Keys or unkeys through the radio's transmit interlock, then waits for
    /// the interlock to say it did: the radio taking the order is not the
    /// radio transmitting (Principle X).
    fn transmit(&mut self, on: bool) -> Result<(), RadioError> {
        // Meters are asked for before the first transmission, so they are
        // flowing by the time it is on the air; without them the
        // transmission goes ahead.
        if on && !self.meters_asked {
            self.meters_asked = true;
            let _ = self.ask_for_meters();
        }
        self.order(&Order::Transmit(on))?;
        let deadline = Instant::now() + KEY_WAIT;
        loop {
            match self.board.interlock.as_deref() {
                Some("TRANSMITTING") if on => return Ok(()),
                Some("READY" | "RECEIVE" | "NOT_READY") if !on => return Ok(()),
                Some(state @ ("TX_FAULT" | "TIMEOUT")) if on => {
                    return Err(RadioError(format!(
                        "the FlexRadio would not transmit: its interlock says {state}"
                    )));
                }
                _ => {}
            }
            if Instant::now() > deadline {
                return Err(RadioError(format!(
                    "sent xmit {}, but the FlexRadio's interlock still says {}",
                    u8::from(on),
                    self.board.interlock.as_deref().unwrap_or("nothing")
                )));
            }
            if let Some(line) = self.next_line()?
                && let Line::Status(status) = classify(&line)
            {
                self.board.take(status)?;
            }
        }
    }

    /// Forward power and SWR, as the radio's meters measured them within
    /// the last second. Power is in percent of a FLEX-6000 or -8000's 100 W,
    /// so another model's isn't given. The radio's ALC meter is in dBFS,
    /// not on a scale the Agent can judge, so ALC is left out.
    fn meters(&mut self) -> Result<Option<TxMeters>, RadioError> {
        self.take_in()?;
        self.take_meter_packets();
        if !self.identity.model.starts_with("FLEX-") {
            return Ok(None);
        }
        let fresh = |reading: Option<(i16, Instant)>| {
            reading
                .filter(|(_, at)| at.elapsed() <= METER_FRESH)
                .map(|(value, _)| value)
        };
        Ok(match (fresh(self.forward), fresh(self.swr)) {
            (Some(forward), Some(swr)) => Some(TxMeters {
                power: power_percent(forward, FLEX_FULL_WATTS),
                swr_tenths: swr_tenths(swr),
                alc: None,
                tuning: false,
                readings: 0,
            }),
            _ => None,
        })
    }
}

impl FlexRadio {
    /// Reads one setting from what the radio last reported.
    fn read_setting(&mut self, summary: &SettingSummary) -> Result<Vec<u8>, RadioError> {
        let setting = known(summary)?;
        self.take_in()?;
        let text = self.board.value(setting).ok_or_else(|| {
            RadioError(format!("the FlexRadio hasn't reported {}", setting.label))
        })?;
        flex_settings::to_bytes(setting, &text).map_err(RadioError)
    }

    /// Puts one setting to `bytes`, then follows the radio's status lines
    /// for a moment so a read straight after sees what it reports.
    fn write_setting(&mut self, summary: &SettingSummary, bytes: &[u8]) -> Result<(), RadioError> {
        let setting = known(summary)?;
        let text = flex_settings::to_text(setting, bytes).map_err(RadioError)?;
        self.take_in()?;
        let slice = self.board.chosen().map_or(0, |(index, _)| *index);
        self.order(&Order::Setting {
            setting,
            slice,
            text: text.clone(),
        })?;
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline && self.board.value(setting).as_deref() != Some(&text) {
            if let Some(line) = self.next_line()?
                && let Line::Status(status) = classify(&line)
            {
                self.board.take(status)?;
            }
        }
        Ok(())
    }

    /// Reads one setting, writes exactly what it read back to it, and reads
    /// it again (`check-writes`).
    ///
    /// # Errors
    ///
    /// The setting is one the Agent never changes, or couldn't be read
    /// before the write.
    pub fn write_back(&mut self, summary: &SettingSummary) -> Result<WriteBack, RadioError> {
        if !summary.write {
            return Err(RadioError(format!(
                "{} is one the Agent never changes",
                summary.label
            )));
        }
        let before = self.read_setting(summary)?;
        let written = self.write_setting(summary, &before);
        let after = self.read_setting(summary);
        Ok(WriteBack {
            before,
            written: written.map_err(|error| error.to_string()),
            after: after.map_err(|error| error.to_string()),
        })
    }
}

/// The FlexRadio setting a summary names.
fn known(summary: &SettingSummary) -> Result<&'static FlexSetting, RadioError> {
    flex_settings::find(&summary.id)
        .ok_or_else(|| RadioError(format!("a FlexRadio has no setting {:?}", summary.id)))
}

impl Exposes for FlexRadio {
    fn definition(&self) -> &crate::rigdef::Definition {
        &self.definition
    }

    fn read_exposed(&mut self, setting: &SettingSummary) -> Result<Vec<u8>, RadioError> {
        self.read_setting(setting)
    }

    fn write_exposed(&mut self, setting: &SettingSummary, bytes: &[u8]) -> Result<(), RadioError> {
        if !setting.write {
            return Err(RadioError(format!(
                "{} is one the Agent never changes",
                setting.label
            )));
        }
        if !setting.write_checked {
            return Err(RadioError(format!(
                "{} isn't checked on a real radio yet, so the Agent doesn't write it",
                setting.label
            )));
        }
        self.write_setting(setting, bytes)
    }
}

/// The radio's address: a host name or IP address, with port 4992 unless
/// another is given.
fn resolve(address: &str) -> Result<SocketAddr, RadioError> {
    let bad = || RadioError(format!("cannot find the FlexRadio at {address:?}"));
    let with_port = if address.parse::<SocketAddr>().is_ok() || address.contains(':') {
        address.to_owned()
    } else {
        format!("{address}:{PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|_| bad())?
        .next()
        .ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    use std::time::{Duration, Instant};

    use super::{
        Ask, Board, FlexRadio, Line, Order, classify, meter_readings, mode_from_flex, mode_to_flex,
        model_from_info, parse_mhz, power_percent, swr_tenths,
    };
    use crate::flex_pretend::PretendFlex;
    use crate::rig::{Exposes, Keyable, Mode, Radio, Reading, Tunable, Tuning, TxMeters, Vfo};

    #[test]
    fn sorts_lines() {
        assert_eq!(classify("V1.4.0.0"), Line::Version("1.4.0.0"));
        assert_eq!(
            classify("R3|0|model=\"FLEX-8400\""),
            Line::Answer {
                seq: 3,
                code: 0,
                text: "model=\"FLEX-8400\""
            }
        );
        assert_eq!(
            classify("R4|50000016|"),
            Line::Answer {
                seq: 4,
                code: 0x5000_0016,
                text: ""
            }
        );
        assert_eq!(
            classify("S1A2B3C4D|slice 0 RF_frequency=14.074000"),
            Line::Status("slice 0 RF_frequency=14.074000")
        );
        assert_eq!(classify("H1A2B3C4D"), Line::Other);
        assert_eq!(classify("M10000001|hello"), Line::Other);
        assert_eq!(classify("Rx|y"), Line::Other);
    }

    #[test]
    fn reads_megahertz() {
        assert_eq!(parse_mhz("14.074000"), Ok(14_074_000));
        assert_eq!(parse_mhz("7.0305"), Ok(7_030_500));
        assert_eq!(parse_mhz("146.52"), Ok(146_520_000));
        assert_eq!(parse_mhz("0.475000"), Ok(475_000));
        assert_eq!(parse_mhz("10"), Ok(10_000_000));
        for bad in [
            "",
            "0.000000",
            "-14.0",
            "14.07x",
            "1e3",
            "999999999.0",
            ".5",
        ] {
            assert!(parse_mhz(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn maps_flex_modes() {
        assert_eq!(mode_from_flex("USB"), Some(Mode::Usb));
        assert_eq!(mode_from_flex("DIGU"), Some(Mode::DataUsb));
        assert_eq!(mode_from_flex("DIGL"), Some(Mode::DataLsb));
        assert_eq!(mode_from_flex("SAM"), Some(Mode::Am));
        assert_eq!(mode_from_flex("NFM"), Some(Mode::Fm));
        assert_eq!(mode_from_flex("FDV"), Some(Mode::Other));
        assert_eq!(mode_from_flex(""), None);
    }

    #[test]
    fn reads_the_model() {
        let info = "model=\"FLEX-8400\",chassis_serial=\"1234\",name=\"Shack\",callsign=\"K9KSU\"";
        assert_eq!(model_from_info(info), Some("FLEX-8400".to_owned()));
        assert_eq!(model_from_info("model=\"\u{1b}[2J\""), None);
        assert_eq!(model_from_info("name=x"), None);
    }

    #[test]
    fn follows_the_transmit_slice() {
        let mut board = Board::default();
        assert!(board.reading().is_err());
        let lines = [
            "slice 0 in_use=1 RF_frequency=14.074000 mode=DIGU filter_lo=100 filter_hi=3100 index_letter=A tx=0 active=0",
            "slice 1 in_use=1 RF_frequency=7.030000 mode=CW filter_lo=-250 filter_hi=250 index_letter=B tx=1 active=1",
            "interlock state=READY reason= source=",
        ];
        for line in lines {
            assert_eq!(board.take(line), Ok(()));
        }
        assert_eq!(
            board.reading(),
            Ok(Reading {
                frequency_hz: 7_030_000,
                mode: Some(Mode::Cw),
                passband_hz: Some(500),
                ptt: Some(false),
                vfo: Some("Slice B".to_owned()),
            })
        );
        assert_eq!(board.take("interlock state=TRANSMITTING source=SW"), Ok(()));
        assert_eq!(board.reading().map(|r| r.ptt), Ok(Some(true)));
        // The transmit slice closes: the other one is reported.
        assert_eq!(board.take("slice 1 in_use=0"), Ok(()));
        let reading = board.reading();
        assert_eq!(reading.clone().map(|r| r.frequency_hz), Ok(14_074_000));
        assert_eq!(reading.map(|r| r.mode), Ok(Some(Mode::DataUsb)));
        // A partial update changes only what it names.
        assert_eq!(board.take("slice 0 RF_frequency=14.075500"), Ok(()));
        assert_eq!(board.reading().map(|r| r.passband_hz), Ok(Some(3000)));
        assert_eq!(board.reading().map(|r| r.frequency_hz), Ok(14_075_500));
    }

    #[test]
    fn refuses_bad_status() {
        let mut board = Board::default();
        assert!(board.take("slice 0 RF_frequency=nonsense").is_err());
        for index in 0..16 {
            assert_eq!(
                board.take(&format!("slice {index} RF_frequency=14.0")),
                Ok(())
            );
        }
        assert!(board.take("slice 200 RF_frequency=14.0").is_err());
        assert_eq!(board.take("slice x RF_frequency=14.0"), Ok(()));
        assert_eq!(board.take("unknown thing=1"), Ok(()));
    }

    #[test]
    fn only_asks_questions() {
        for ask in [
            Ask::Info,
            Ask::Slices,
            Ask::Transmitter,
            Ask::Whole,
            Ask::Ping,
            Ask::MeterPort(4993),
            Ask::Meters,
        ] {
            let command = ask.command();
            let first = command.split(' ').next().unwrap_or_default();
            assert!(
                matches!(first, "info" | "sub" | "ping") || command.starts_with("client udpport "),
                "{command} is not a question"
            );
        }
    }

    /// A stand-in FlexRadio on this computer, answering as the API describes.
    /// Its thread returns every command it was sent.
    fn fake_radio(model: &'static str) -> Option<(String, thread::JoinHandle<Vec<String>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let address = listener.local_addr().ok()?.to_string();
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            let Ok((stream, _)) = listener.accept() else {
                return seen;
            };
            let Ok(mut out) = stream.try_clone() else {
                return seen;
            };
            let _ = out.write_all(b"V1.4.0.0\nH2A3B4C5D\n");
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let (seq, command) = line
                    .trim_start_matches('C')
                    .split_once('|')
                    .unwrap_or_default();
                let reply = match command {
                    "info" => format!("R{seq}|0|model=\"{model}\",name=\"Shack\"\n"),
                    "sub slice all" => format!(
                        "S2A3B4C5D|slice 0 in_use=1 RF_frequency=14.205000 mode=USB \
                         filter_lo=100 filter_hi=2900 index_letter=A tx=1 active=1\nR{seq}|0|\n"
                    ),
                    "sub tx all" => format!(
                        "S2A3B4C5D|interlock state=READY\nR{seq}|0|\n\
                         S2A3B4C5D|slice 0 mode=LSB RF_frequency=3.850000\n"
                    ),
                    _ => format!("R{seq}|0|\n"),
                };
                seen.push(command.to_owned());
                let _ = out.write_all(reply.as_bytes());
            }
            seen
        });
        Some((address, handle))
    }

    #[test]
    fn reads_a_flexradio_over_its_api() {
        let Some((address, handle)) = fake_radio("FLEX-8400") else {
            return;
        };
        let opened = FlexRadio::open(&address);
        assert!(opened.is_ok(), "{:?}", opened.as_ref().err());
        let Ok(mut radio) = opened else { return };
        assert_eq!(radio.identity().maker, "FlexRadio");
        assert_eq!(radio.identity().model, "FLEX-8400");
        let mut reading = radio.read();
        for _ in 0..100 {
            if reading.as_ref().map(|r| r.frequency_hz) == Ok(3_850_000) {
                break;
            }
            reading = radio.read();
        }
        assert_eq!(
            reading,
            Ok(Reading {
                frequency_hz: 3_850_000,
                mode: Some(Mode::Lsb),
                passband_hz: Some(2800),
                ptt: Some(false),
                vfo: Some("Slice A".to_owned()),
            })
        );
        drop(radio);
        // The Agent asked only questions.
        let seen = handle.join().unwrap_or_default();
        assert_eq!(
            seen,
            ["info", "sub slice all", "sub tx all", "sub radio all"]
        );
    }

    #[test]
    fn orders_are_written_as_the_api_takes_them() {
        assert_eq!(
            Order::Tune {
                slice: 1,
                hz: 14_074_000
            }
            .command(),
            "slice tune 1 14.074000"
        );
        assert_eq!(
            Order::Tune { slice: 0, hz: 475 }.command(),
            "slice tune 0 0.000475"
        );
        assert_eq!(
            Order::Mode {
                slice: 0,
                mode: "DIGU"
            }
            .command(),
            "slice set 0 mode=DIGU"
        );
        assert_eq!(Order::Transmit(true).command(), "xmit 1");
        assert_eq!(Order::Transmit(false).command(), "xmit 0");
        // Every mode the Agent sends reads back as the same mode.
        for mode in [
            Mode::Usb,
            Mode::Lsb,
            Mode::Cw,
            Mode::Rtty,
            Mode::Am,
            Mode::Fm,
            Mode::DataUsb,
            Mode::DataLsb,
        ] {
            assert_eq!(mode_to_flex(mode).and_then(mode_from_flex), Some(mode));
        }
        for mode in [
            Mode::CwReverse,
            Mode::RttyReverse,
            Mode::DataFm,
            Mode::Other,
        ] {
            assert_eq!(mode_to_flex(mode), None);
        }
    }

    fn pretend(can_transmit: bool) -> Option<(PretendFlex, FlexRadio)> {
        let pretend = PretendFlex::start("FLEX-6600", can_transmit).ok()?;
        let radio = FlexRadio::open(pretend.address());
        assert!(radio.is_ok(), "{:?}", radio.as_ref().err());
        Some((pretend, radio.ok()?))
    }

    #[test]
    fn tunes_and_keys_a_flexradio() {
        let Some((pretend, mut radio)) = pretend(true) else {
            return;
        };
        assert_eq!(radio.identity().model, "FLEX-6600");
        assert_eq!(radio.tune(Tuning::Frequency(7_074_000)), Ok(()));
        assert_eq!(radio.tune(Tuning::Mode(Mode::Usb)), Ok(()));
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 7_074_000);
        assert_eq!(reading.mode, Some(Mode::Usb));
        assert_eq!(reading.ptt, Some(false));
        assert_eq!(radio.transmit(true), Ok(()));
        assert!(pretend.transmitting());
        assert_eq!(radio.read().map(|r| r.ptt), Ok(Some(true)));
        assert_eq!(radio.transmit(false), Ok(()));
        assert!(!pretend.transmitting());
        assert_eq!(radio.read().map(|r| r.ptt), Ok(Some(false)));
        assert_eq!(
            heard(&pretend)[4..],
            [
                "slice tune 0 7.074000",
                "slice set 0 mode=USB",
                "client udpport N",
                "sub meter all",
                "xmit 1",
                "xmit 0"
            ]
        );
    }

    /// What the pretend radio heard, with the meter port as N.
    fn heard(pretend: &PretendFlex) -> Vec<String> {
        pretend
            .heard()
            .into_iter()
            .map(|command| {
                if command.starts_with("client udpport ") {
                    "client udpport N".to_owned()
                } else {
                    command
                }
            })
            .collect()
    }

    #[test]
    fn reads_the_transmit_meters_while_keyed() {
        let Some((_pretend, mut radio)) = pretend(true) else {
            return;
        };
        // Not keyed yet: nothing asked for, nothing to read.
        assert_eq!(radio.meters(), Ok(None));
        assert_eq!(radio.transmit(true), Ok(()));
        let mut meters = radio.meters();
        let deadline = Instant::now() + Duration::from_secs(5);
        while meters == Ok(None) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
            meters = radio.meters();
        }
        assert_eq!(
            meters,
            Ok(Some(TxMeters {
                power: 40,
                swr_tenths: 13,
                alc: None,
                tuning: false,
                readings: 0,
            }))
        );
        assert_eq!(radio.transmit(false), Ok(()));
    }

    #[test]
    fn reads_meter_packets() {
        let packet = |header: u32, class: u32, pairs: &[(u16, i16)], trailer: bool| {
            let mut words = vec![header, 0x0000_0700, 0x0000_1C2D, class];
            if header & 0x00C0_0000 != 0 {
                words.push(0);
            }
            if header & 0x0030_0000 != 0 {
                words.extend([0, 0]);
            }
            let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
            for (number, value) in pairs {
                bytes.extend_from_slice(&number.to_be_bytes());
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            if trailer {
                bytes.extend_from_slice(&[0xFF; 4]);
            }
            let size = u32::try_from(bytes.len() / 4).unwrap_or(0);
            bytes[..4].copy_from_slice(&(header | size).to_be_bytes());
            bytes
        };
        let pairs = [(15, 5891), (17, 166), (7, -1280)];
        // Extension data with a stream ID, both timestamps.
        let full = packet(0x3850_0000, 0x534C_8002, &pairs, false);
        assert_eq!(meter_readings(&full), pairs);
        // With a trailer, it is left out.
        let trailed = packet(0x3C50_0000, 0x534C_8002, &pairs, true);
        assert_eq!(meter_readings(&trailed), pairs);
        // No timestamps.
        let bare = packet(0x3800_0000, 0x534C_8002, &pairs, false);
        assert_eq!(meter_readings(&bare), pairs);
        // Not a meter packet, no class ID, cut short, empty.
        assert!(meter_readings(&packet(0x3850_0000, 0x534C_8003, &pairs, false)).is_empty());
        assert!(meter_readings(&packet(0x3050_0000, 0x534C_8002, &pairs, false)).is_empty());
        assert!(meter_readings(&full[..full.len() - 4]).is_empty());
        assert!(meter_readings(&[]).is_empty());
    }

    #[test]
    fn puts_meters_on_the_agents_scales() {
        assert_eq!(power_percent(5891, 100.0), 40);
        assert_eq!(power_percent(6400, 100.0), 100);
        assert_eq!(power_percent(0, 100.0), 0);
        assert_eq!(power_percent(i16::MAX, 100.0), 999);
        assert_eq!(swr_tenths(128), 10);
        assert_eq!(swr_tenths(166), 13);
        assert_eq!(swr_tenths(192), 15);
        assert_eq!(swr_tenths(-50), 10);
        let mut board = Board::default();
        assert_eq!(
            board.take(
                "meter 3.src=SLC#3.num=0#3.nam=LEVEL#3.unit=dBm#\
                 15.src=TX-#15.num=1#15.nam=FWDPWR#15.desc=RF Power Forward#15.unit=dBm#\
                 17.src=TX-#17.nam=SWR#17.unit=SWR#7.src=TX-#7.nam=ALC#7.unit=dBFS#"
            ),
            Ok(())
        );
        assert_eq!((board.forward_meter, board.swr_meter), (Some(15), Some(17)));
    }

    #[test]
    fn records_every_setting_and_writes_none_until_checked() {
        let Some((_pretend, mut radio)) = pretend(true) else {
            return;
        };
        let record = crate::settings_record::read(
            &mut radio,
            None,
            std::time::SystemTime::UNIX_EPOCH,
            "TEST",
        );
        assert_eq!(record.model, "FLEX-6600");
        let unread: Vec<&str> = record
            .settings
            .iter()
            .filter(|e| matches!(e.value, crate::settings_record::Value::Unread { .. }))
            .map(|e| e.id.as_str())
            .collect();
        assert!(unread.is_empty(), "{unread:?}");
        let definition = radio.definition().clone();
        // Its own words are shown as words.
        let lines = crate::settings_record::report(&record, &[], None, &definition);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Slice mode") && l.ends_with(" DIGU")),
            "{lines:?}"
        );
        let setting = |id: &str| {
            definition
                .settings
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .unwrap_or_else(|| unreachable!("{id}"))
        };
        assert_eq!(
            radio.read_exposed(&setting("rf_power")),
            Ok(vec![0x00, 0x50])
        );
        assert_eq!(radio.read_exposed(&setting("mode")), Ok(b"DIGU".to_vec()));
        assert_eq!(
            radio.read_exposed(&setting("filter")),
            Ok(b"100 3100".to_vec())
        );
        let refused = radio.write_exposed(&setting("rf_power"), &[0x00, 0x20]);
        assert!(refused.is_err_and(|e| e.0.contains("isn't checked on a real radio")));
        assert_eq!(
            radio.read_exposed(&setting("rf_power")),
            Ok(vec![0x00, 0x50])
        );
    }

    #[test]
    fn checks_its_writes_by_writing_each_value_back() {
        let Some((pretend, mut radio)) = pretend(true) else {
            return;
        };
        let report = crate::write_check::check(
            &mut radio,
            None,
            std::time::SystemTime::UNIX_EPOCH,
            "TEST",
            |_, _| {},
        );
        let failed: Vec<(&str, &str)> = report
            .settings
            .iter()
            .filter(|c| c.outcome != "works")
            .map(|c| (c.id.as_str(), c.outcome))
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        let heard = pretend.heard();
        for sent in [
            "transmit set miclevel=50",
            "slice set 0 agc_mode=med",
            "slice set 0 rf_gain=10",
            "filt 0 100 3100",
            "slice set 0 mode=DIGU",
        ] {
            assert!(heard.iter().any(|h| h == sent), "{sent} in {heard:?}");
        }
        // Nothing was keyed.
        assert!(!heard.iter().any(|h| h.starts_with("xmit")));
    }

    #[test]
    fn says_what_a_flexradio_refused() {
        let Some((pretend, mut radio)) = pretend(false) else {
            return;
        };
        let refused = |result: Result<(), crate::rig::RadioError>| {
            result.map_err(|error| error.0).err().unwrap_or_default()
        };
        assert!(
            refused(radio.tune(Tuning::Frequency(144_174_000)))
                .contains("out of the radio's range")
        );
        assert!(refused(radio.tune(Tuning::Mode(Mode::CwReverse))).contains("no CwReverse mode"));
        for tuning in [
            Tuning::OtherFrequency(14_076_000),
            Tuning::Select(Vfo::B),
            Tuning::Split(true),
        ] {
            assert!(refused(radio.tune(tuning)).contains("slices, not VFO A and B"));
        }
        assert!(refused(radio.transmit(true)).contains("cannot transmit"));
        assert!(!pretend.transmitting());
        // Only what could be sent was sent.
        assert_eq!(
            heard(&pretend)[4..],
            [
                "slice tune 0 144.174000",
                "client udpport N",
                "sub meter all",
                "xmit 1"
            ]
        );
        // The radio is still as it was.
        assert_eq!(radio.read().map(|r| r.frequency_hz), Ok(14_074_000));
    }

    #[test]
    fn keying_is_what_the_interlock_says() {
        // This radio takes `xmit 1` but its interlock never says it keyed.
        let Some((address, handle)) = fake_radio("FLEX-8400") else {
            return;
        };
        let Ok(mut radio) = FlexRadio::open(&address) else {
            unreachable!("the fake radio should open")
        };
        let error = radio.transmit(true).map_err(|error| error.0);
        assert_eq!(
            error,
            Err("sent xmit 1, but the FlexRadio's interlock still says READY".to_owned())
        );
        drop(radio);
        assert_eq!(
            handle.join().unwrap_or_default().last().map(String::as_str),
            Some("xmit 1")
        );
    }

    #[test]
    fn says_when_nothing_answers() {
        let Ok(listener) = TcpListener::bind("127.0.0.1:0") else {
            return;
        };
        let address = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        drop(listener);
        assert!(FlexRadio::open(&address).is_err());
    }
}
