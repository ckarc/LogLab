// SPDX-FileCopyrightText: 2026 LogLab contributors
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
//! Only questions are ever sent. [`Ask`] is the whole list of what the Agent
//! may say to the radio, and none of it tunes the radio or keys it.
//! Every line from the radio is checked before it is believed
//! (Principle XII): lines are capped in size, the number of slices is capped,
//! and frequencies and widths must be in range.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::rig::{ControlPath, MAX_FREQUENCY_HZ, Mode, Radio, RadioError, RadioIdentity, Reading};

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
}

impl Ask {
    /// The command text sent for this question, after `C<n>|`.
    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Slices => "sub slice all",
            Self::Transmitter => "sub tx all",
            Self::Whole => "sub radio all",
            Self::Ping => "ping",
        }
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
}

/// The radio's live state, built up from its status lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Board {
    slices: BTreeMap<u8, Slice>,
    ptt: Option<bool>,
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
            Some("interlock") => {
                for (key, value) in pairs(status) {
                    if key == "state" {
                        // UNKEY_REQUESTED: still on the air until it unkeys.
                        self.ptt = Some(matches!(value, "TRANSMITTING" | "UNKEY_REQUESTED"));
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
        }
        Ok(())
    }

    /// The reading for the slice that matters: the transmit slice, else the
    /// active one, else the first.
    ///
    /// # Errors
    ///
    /// No slice is open, or it has not said its frequency yet.
    pub fn reading(&self) -> Result<Reading, RadioError> {
        let chosen = self
            .slices
            .iter()
            .find(|(_, s)| s.transmit)
            .or_else(|| self.slices.iter().find(|(_, s)| s.active))
            .or_else(|| self.slices.iter().next());
        let Some((index, slice)) = chosen else {
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
        Ok(radio)
    }

    /// Send one question; return its number.
    fn send(&mut self, ask: Ask) -> Result<u32, RadioError> {
        self.seq = self.seq.wrapping_add(1);
        let line = format!("C{}|{}\n", self.seq, ask.command());
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

    fn read(&mut self) -> Result<Reading, RadioError> {
        // Take in everything that has arrived since the last read.
        while let Some(line) = self.next_line()? {
            if let Line::Status(status) = classify(&line) {
                self.board.take(status)?;
            }
        }
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

    use super::{
        Ask, Board, FlexRadio, Line, classify, mode_from_flex, model_from_info, parse_mhz,
    };
    use crate::rig::{Mode, Radio, Reading};

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
        ] {
            let command = ask.command();
            let first = command.split(' ').next().unwrap_or_default();
            assert!(
                matches!(first, "info" | "sub" | "ping"),
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
