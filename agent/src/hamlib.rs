// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Radios reached through Hamlib's `rigctld`.
//!
//! Hamlib (LGPL-2.1-or-later, <https://hamlib.github.io>) already knows how
//! to talk to hundreds of radios. The Agent starts `rigctld` itself, as its
//! own child process listening only on this computer, and is its only user,
//! so the Agent stays the radio's one owner (Principle IX). It can also use a
//! `rigctld` that is already running on this computer.
//!
//! The Agent speaks to `rigctld` in its documented network protocol, using
//! the "extended response" form, where every answer ends in an `RPRT` line.
//! Every answer is checked before it is believed (Principle XII): lines and
//! whole answers are capped in size, and frequencies and widths must be in
//! range.
//!
//! Only questions can be sent. [`Query`] is the whole list of what the Agent
//! may say to `rigctld`, and none of it tunes the radio or keys it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::rig::{ControlPath, MAX_FREQUENCY_HZ, Mode, Radio, RadioError, RadioIdentity, Reading};

/// The longest line the Agent reads from `rigctld`.
const MAX_LINE: usize = 1024;
/// The most lines in one answer (`dump_caps` is the longest, at a few hundred).
const MAX_LINES: usize = 2000;
/// How long one answer may take.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a freshly started `rigctld` has to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Hamlib's "feature not available" result, for radios that can't report PTT.
const NOT_AVAILABLE: i32 = -11;
/// Hamlib's "not implemented" result.
const NOT_IMPLEMENTED: i32 = -4;

/// Everything the Agent may ask `rigctld`. All of it only reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// The frequency.
    Frequency,
    /// The mode and filter width.
    Mode,
    /// Whether the radio is transmitting.
    Ptt,
    /// The active VFO.
    Vfo,
    /// The radio's maker and model.
    Capabilities,
}

impl Query {
    /// The command line sent for this question.
    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Frequency => "+f\n",
            Self::Mode => "+m\n",
            Self::Ptt => "+t\n",
            Self::Vfo => "+v\n",
            Self::Capabilities => "+\\dump_caps\n",
        }
    }
}

/// One `rigctld` answer: its `Name: value` lines, and its result code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Each `Name: value` line, in order. Lines without a value are skipped.
    pub values: Vec<(String, String)>,
    /// The `RPRT` code: 0 for success, negative for a Hamlib error.
    pub result: i32,
}

impl Answer {
    /// The first value named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Read one extended-response answer from `reader`.
///
/// # Errors
///
/// The stream ended, a line is too long or not text, the answer has too many
/// lines, or its `RPRT` line is not a number.
pub fn read_answer(reader: &mut impl BufRead) -> Result<Answer, RadioError> {
    let mut answer = Answer::default();
    for _ in 0..MAX_LINES {
        let line = read_line(reader)?;
        if let Some(code) = line.strip_prefix("RPRT ") {
            answer.result = code
                .trim()
                .parse()
                .map_err(|_| RadioError(format!("Hamlib sent a bad result line: {code:?}")))?;
            return Ok(answer);
        }
        // The first line echoes the command ("get_freq:"); it has no value.
        if let Some((name, value)) = line.split_once(':')
            && !value.trim().is_empty()
        {
            answer
                .values
                .push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Err(RadioError("Hamlib's answer was too long".to_owned()))
}

fn read_line(reader: &mut impl BufRead) -> Result<String, RadioError> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_LINE + 1).unwrap_or(u64::MAX);
    reader
        .take(limit)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => RadioError(
                "the radio did not answer in time; is it on, and are the port and speed right?"
                    .to_owned(),
            ),
            _ => RadioError(format!("lost touch with Hamlib: {error}")),
        })?;
    if bytes.is_empty() {
        return Err(RadioError("Hamlib closed the connection".to_owned()));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(RadioError("Hamlib sent a line that is too long".to_owned()));
    }
    bytes.pop();
    String::from_utf8(bytes)
        .map(|line| line.trim_end_matches('\r').to_owned())
        .map_err(|_| RadioError("Hamlib sent something that is not text".to_owned()))
}

/// A frequency as Hamlib writes it (a whole number of hertz, or with a
/// decimal part), checked to be in range.
///
/// # Errors
///
/// It is not a number, or not between 1 Hz and [`MAX_FREQUENCY_HZ`].
pub fn parse_frequency(text: &str) -> Result<u64, RadioError> {
    let whole = text.split('.').next().unwrap_or_default();
    let bad = || {
        RadioError(format!(
            "the radio reported a frequency that makes no sense: {text:?}"
        ))
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || whole.len() > 13 {
        return Err(bad());
    }
    let hz: u64 = whole.parse().map_err(|_| bad())?;
    if hz == 0 || hz > MAX_FREQUENCY_HZ {
        return Err(bad());
    }
    Ok(hz)
}

/// The rig model's name for a Hamlib mode name; `None` when there is no mode.
#[must_use]
pub fn mode_from_hamlib(name: &str) -> Option<Mode> {
    Some(match name {
        "" | "None" => return None,
        "USB" => Mode::Usb,
        "LSB" => Mode::Lsb,
        "CW" => Mode::Cw,
        "CWR" => Mode::CwReverse,
        "RTTY" => Mode::Rtty,
        "RTTYR" => Mode::RttyReverse,
        "AM" | "SAM" | "AMS" | "DSB" => Mode::Am,
        "FM" | "WFM" | "FMN" => Mode::Fm,
        "PKTUSB" => Mode::DataUsb,
        "PKTLSB" => Mode::DataLsb,
        "PKTFM" | "PKTFMN" => Mode::DataFm,
        _ => Mode::Other,
    })
}

/// How to start `rigctld` for a radio on a serial port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The `rigctld` program to run.
    pub program: String,
    /// Hamlib's number for the radio model (3073 is the Icom IC-7300).
    pub model: u32,
    /// The radio's serial port, such as `/dev/ttyUSB0` or `COM3`.
    pub device: String,
    /// The serial speed, or `None` for the model's default.
    pub baud: Option<u32>,
}

/// Where the Agent's `rigctld` is.
#[derive(Debug)]
pub enum Source {
    /// One the Agent started and owns; it is stopped with the Agent.
    Started(Launch),
    /// One already running on this computer, for this radio model.
    Running {
        /// Its address, on this computer.
        address: SocketAddr,
        /// Hamlib's number for the radio model.
        model: u32,
    },
}

/// A radio reached through `rigctld`.
pub struct HamlibRadio {
    identity: RadioIdentity,
    connection: BufReader<TcpStream>,
    child: Option<Child>,
}

impl HamlibRadio {
    /// Reach the radio: start `rigctld` if asked to, connect, and ask the
    /// radio's maker and model.
    ///
    /// # Errors
    ///
    /// `rigctld` could not be started or reached, or the address is not on
    /// this computer (the protocol has no sign-in, Principle XXI).
    pub fn open(source: &Source) -> Result<Self, RadioError> {
        let (address, model, child) = match source {
            Source::Running { address, model } => {
                if !address.ip().is_loopback() {
                    return Err(RadioError(
                        "rigctld must be on this computer (127.0.0.1)".to_owned(),
                    ));
                }
                (*address, *model, None)
            }
            Source::Started(launch) => {
                let (address, child) = start(launch)?;
                (address, launch.model, Some(child))
            }
        };
        let mut child = child;
        let stream = match connect(address, child.is_some()) {
            Ok(stream) => stream,
            Err(error) => {
                stop(child.as_mut());
                return Err(error);
            }
        };
        let mut radio = Self {
            identity: RadioIdentity {
                maker: "Unknown".to_owned(),
                model: format!("Hamlib model {model}"),
                path: ControlPath::Hamlib { model },
            },
            connection: BufReader::new(stream),
            child,
        };
        let caps = radio.ask(Query::Capabilities)?;
        if let Some(maker) = caps.get("Mfg name").filter(|name| !name.is_empty()) {
            radio.identity.maker = clip(maker);
        }
        if let Some(name) = caps.get("Model name").filter(|name| !name.is_empty()) {
            radio.identity.model = clip(name);
        }
        Ok(radio)
    }

    /// Send one question and read its answer.
    fn ask(&mut self, query: Query) -> Result<Answer, RadioError> {
        let stream = self.connection.get_mut();
        stream
            .write_all(query.command().as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|error| RadioError(format!("lost touch with Hamlib: {error}")))?;
        read_answer(&mut self.connection)
    }

    fn ask_ok(&mut self, query: Query) -> Result<Option<Answer>, RadioError> {
        let answer = self.ask(query)?;
        match answer.result {
            0 => Ok(Some(answer)),
            NOT_AVAILABLE | NOT_IMPLEMENTED => Ok(None),
            code => Err(RadioError(format!(
                "the radio did not answer (Hamlib error {code}); is it on and connected?"
            ))),
        }
    }
}

impl Radio for HamlibRadio {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn read(&mut self) -> Result<Reading, RadioError> {
        let frequency = self
            .ask_ok(Query::Frequency)?
            .ok_or_else(|| RadioError("the radio does not report its frequency".to_owned()))?;
        let frequency_hz = parse_frequency(frequency.get("Frequency").unwrap_or_default())?;
        let (mode, passband_hz) = match self.ask_ok(Query::Mode)? {
            Some(answer) => (
                mode_from_hamlib(answer.get("Mode").unwrap_or_default()),
                answer
                    .get("Passband")
                    .and_then(|width| width.parse::<u32>().ok())
                    .filter(|width| (1..=1_000_000).contains(width)),
            ),
            None => (None, None),
        };
        let ptt = self
            .ask_ok(Query::Ptt)?
            .and_then(|answer| answer.get("PTT").map(|ptt| ptt != "0"));
        let vfo = self
            .ask_ok(Query::Vfo)?
            .and_then(|answer| answer.get("VFO").map(clip))
            .filter(|vfo| !vfo.is_empty() && vfo != "None");
        Ok(Reading {
            frequency_hz,
            mode,
            passband_hz,
            ptt,
            vfo,
        })
    }
}

impl Drop for HamlibRadio {
    fn drop(&mut self) {
        stop(self.child.as_mut());
    }
}

/// A name from Hamlib, kept to 40 printable characters.
fn clip(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn start(launch: &Launch) -> Result<(SocketAddr, Child), RadioError> {
    let address = free_local_port()?;
    let mut command = Command::new(&launch.program);
    command
        .arg(format!("--model={}", launch.model))
        .arg(format!("--rig-file={}", launch.device))
        .arg("--listen-addr=127.0.0.1")
        .arg(format!("--port={}", address.port()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(baud) = launch.baud {
        command.arg(format!("--serial-speed={baud}"));
    }
    let child = command.spawn().map_err(|error| {
        RadioError(format!(
            "could not start Hamlib's {}: {error}; is Hamlib installed?",
            launch.program
        ))
    })?;
    Ok((address, child))
}

fn free_local_port() -> Result<SocketAddr, RadioError> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map_err(|error| RadioError(format!("no free port on this computer: {error}")))
}

fn connect(address: SocketAddr, just_started: bool) -> Result<TcpStream, RadioError> {
    let deadline = Instant::now()
        + if just_started {
            START_TIMEOUT
        } else {
            Duration::ZERO
        };
    loop {
        match TcpStream::connect_timeout(&address, ANSWER_TIMEOUT) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(ANSWER_TIMEOUT))
                    .and_then(|()| stream.set_write_timeout(Some(ANSWER_TIMEOUT)))
                    .and_then(|()| stream.set_nodelay(true))
                    .map_err(|error| RadioError(format!("could not set up Hamlib: {error}")))?;
                return Ok(stream);
            }
            Err(error) if Instant::now() < deadline => {
                if error.kind() != io::ErrorKind::ConnectionRefused {
                    return Err(RadioError(format!("could not reach Hamlib: {error}")));
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(RadioError(format!(
                    "could not reach Hamlib at {address}: {error}"
                )));
            }
        }
    }
}

fn stop(child: Option<&mut Child>) {
    if let Some(child) = child {
        // Best effort: the process may already have ended.
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::{Query, mode_from_hamlib, parse_frequency, read_answer};
    use crate::rig::Mode;

    #[test]
    fn reads_an_extended_answer() {
        let mut text: &[u8] = b"get_mode:\nMode: PKTUSB\nPassband: 3000\nRPRT 0\n";
        let answer = read_answer(&mut text).unwrap_or_default();
        assert_eq!(answer.result, 0);
        assert_eq!(answer.get("Mode"), Some("PKTUSB"));
        assert_eq!(answer.get("Passband"), Some("3000"));
    }

    #[test]
    fn reads_an_error_answer() {
        let mut text: &[u8] = b"get_ptt:\nRPRT -11\n";
        let answer = read_answer(&mut text).unwrap_or_default();
        assert_eq!(answer.result, -11);
        assert!(answer.values.is_empty());
    }

    #[test]
    fn refuses_a_line_that_is_too_long() {
        let mut long = vec![b'x'; 5000];
        long.push(b'\n');
        assert!(read_answer(&mut long.as_slice()).is_err());
    }

    #[test]
    fn refuses_an_answer_that_never_ends() {
        let text = "Name: value\n".repeat(3000);
        assert!(read_answer(&mut text.as_bytes()).is_err());
    }

    #[test]
    fn refuses_a_closed_connection_and_bad_results() {
        assert!(read_answer(&mut &b"Frequency: 7074000\n"[..]).is_err());
        assert!(read_answer(&mut &b"RPRT zero\n"[..]).is_err());
        assert!(read_answer(&mut &b"\xff\xfe\n"[..]).is_err());
    }

    #[test]
    fn checks_frequencies() {
        assert_eq!(parse_frequency("14074000"), Ok(14_074_000));
        assert_eq!(parse_frequency("7074000.000000"), Ok(7_074_000));
        for bad in [
            "",
            "0",
            "-5",
            "1e9",
            "abc",
            "999999999999999",
            "300000000001",
        ] {
            assert!(parse_frequency(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn maps_hamlib_modes() {
        assert_eq!(mode_from_hamlib("USB"), Some(Mode::Usb));
        assert_eq!(mode_from_hamlib("CWR"), Some(Mode::CwReverse));
        assert_eq!(mode_from_hamlib("PKTUSB"), Some(Mode::DataUsb));
        assert_eq!(mode_from_hamlib("FAX"), Some(Mode::Other));
        assert_eq!(mode_from_hamlib("None"), None);
    }

    #[test]
    fn only_asks_questions() {
        // Hamlib's commands that change the radio are upper case (F, M, T,
        // V, L ...) or long-form set_ names. Nothing the Agent can send is
        // either.
        for query in [
            Query::Frequency,
            Query::Mode,
            Query::Ptt,
            Query::Vfo,
            Query::Capabilities,
        ] {
            let command = query.command();
            assert!(
                !command.chars().any(|c| c.is_ascii_uppercase()),
                "{command}"
            );
            assert!(!command.contains("set_"), "{command}");
        }
    }
}
