// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A virtual Kenwood TS-2000 on a serial port, for programs that tune a
//! radio only through a COM port, such as Winlink Express ("Kenwood
//! Amateur" in its Radio Setup).
//!
//! Kenwood's command set is text: two capital letters, any parameters, and
//! a semicolon. A command with no parameters asks, and the radio answers in
//! the same shape; with parameters it sets, and the radio says nothing; a
//! command it can't carry out is answered `?;`. A maker's published command
//! set is an open protocol the Agent may speak (Principle IV, constitution
//! 3.0.0). The formats here are as Hamlib reads and writes them for the
//! TS-2000 (<https://github.com/Hamlib/Hamlib>, LGPL-2.1-or-later,
//! `rigs/kenwood/kenwood.c` and `ts2000.c`), as Principle XVI allows; no
//! Hamlib code is here:
//!
//! - `ID;` is answered `ID019;`, the TS-2000's identity
//!   (`kenwood_id_list`).
//! - `FA;` is the frequency in hertz, eleven digits (`FA00014105000;`).
//! - `MD;` is the mode, one digit: 1 LSB, 2 USB, 3 CW, 4 FM, 5 AM, 6 FSK,
//!   7 CW-R, 9 FSK-R (`kenwood_mode_table`).
//! - `IF;` is the radio's state in 38 characters: the frequency from
//!   position 2, transmitting at 28, the mode at 29, the VFO at 30 and
//!   split at 32 (`kenwood_get_if` and its readers).
//! - `TX;` keys and `RX;` unkeys.
//!
//! The virtual radio is a client of the Agent's own `rigctld` server, as
//! VARA's client is (see `vara_proxy`), so every rule there holds for it:
//! tuning and keying only as that server allows, one program at a time,
//! the time limit, the radio's own PTT wins, and the radio is unkeyed when
//! the port closes (Principle IX). USB here means the radio's data USB
//! (`PKTUSB`): the programs that need this, Winlink Express with VARA, send
//! data, and a TS-2000 has no separate data mode to ask for.

// The serial port is opened on Windows only, and everything is tested
// everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The TS-2000's answer to `ID;`.
const IDENTITY: &str = "ID019;";
/// The longest command kept while reading; Kenwood's are a few characters.
const MAX_COMMAND: usize = 64;
/// How long the Agent waits for its own `rigctld` server to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// The radio behind the virtual Kenwood: asked in `rigctld`'s extended
/// protocol, answering each `Name: value` line, or refusing.
pub trait Rig {
    /// Sends `line` (`+f`, `+F 14105000`) and returns the named values in
    /// the answer.
    ///
    /// # Errors
    ///
    /// The radio refused (an answer other than `RPRT 0`) or couldn't be
    /// reached.
    fn ask(&mut self, line: &str) -> Result<Vec<(String, String)>, String>;
}

/// The value named `name` in an answer.
fn value<'a>(values: &'a [(String, String)], name: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Kenwood's mode digit for a Hamlib mode name.
fn kenwood_mode(hamlib: &str) -> char {
    match hamlib {
        "LSB" | "PKTLSB" => '1',
        "USB" | "PKTUSB" => '2',
        "CW" => '3',
        "FM" | "PKTFM" => '4',
        "AM" => '5',
        "RTTY" => '6',
        "CWR" => '7',
        "RTTYR" => '9',
        _ => '0',
    }
}

/// The Hamlib mode name to ask for, for a Kenwood mode digit.
fn hamlib_mode(kenwood: &str) -> Option<&'static str> {
    Some(match kenwood {
        "1" => "LSB",
        "2" => "PKTUSB",
        "3" => "CW",
        "4" => "FM",
        "5" => "AM",
        "6" => "RTTY",
        "7" => "CWR",
        "9" => "RTTYR",
        _ => return None,
    })
}

/// The answer to one Kenwood command (without its `;`): the text to send
/// back, empty when a set command carried out says nothing.
pub fn respond(command: &str, rig: &mut dyn Rig) -> String {
    let command = command.trim();
    let refused = || "?;".to_owned();
    let (name, parameters) = command.split_at(command.len().min(2));
    match (name, parameters) {
        ("ID", "") => IDENTITY.to_owned(),
        ("PS", "") => "PS1;".to_owned(),
        ("AI", "") => "AI0;".to_owned(),
        ("FR" | "FT", "") => format!("{name}0;"),
        ("SP", "") => "SP0;".to_owned(),
        // Already so: Auto Information off (the program asks for what it
        // wants), receiving and transmitting on VFO A, no split.
        ("AI" | "FR" | "FT" | "SP", "0") => String::new(),
        ("FA" | "FB", "") => rig
            .ask("+f")
            .ok()
            .and_then(|values| value(&values, "Frequency")?.parse::<u64>().ok())
            .map_or_else(refused, |hz| format!("{name}{hz:011};")),
        ("FA", digits) if digits.len() == 11 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            match digits.parse::<u64>() {
                Ok(hz) if rig.ask(&format!("+F {hz}")).is_ok() => String::new(),
                _ => refused(),
            }
        }
        ("MD", "") => rig
            .ask("+m")
            .ok()
            .and_then(|values| value(&values, "Mode").map(kenwood_mode))
            .map_or_else(refused, |mode| format!("MD{mode};")),
        ("MD", digit) => match hamlib_mode(digit) {
            Some(mode) if rig.ask(&format!("+M {mode} 0")).is_ok() => String::new(),
            _ => refused(),
        },
        ("IF", "") => information(rig).unwrap_or_else(refused),
        ("TX", "" | "0" | "1" | "2") => {
            if rig.ask("+T 1").is_ok() {
                String::new()
            } else {
                refused()
            }
        }
        ("RX", "") => {
            if rig.ask("+T 0").is_ok() {
                String::new()
            } else {
                refused()
            }
        }
        _ => refused(),
    }
}

/// The `IF;` answer: frequency, transmitting and mode from the radio; RIT,
/// XIT, memory, scan, split and tone all off, on VFO A.
fn information(rig: &mut dyn Rig) -> Option<String> {
    let hz: u64 = value(&rig.ask("+f").ok()?, "Frequency")?.parse().ok()?;
    let mode = kenwood_mode(value(&rig.ask("+m").ok()?, "Mode")?);
    let transmitting = value(&rig.ask("+t").ok()?, "PTT")? != "0";
    let answer = format!(
        "IF{hz:011}{step}{rit}00000{tx}{mode}0000000;",
        step = "     ",
        rit = "+0000",
        tx = u8::from(transmitting),
    );
    debug_assert_eq!(answer.len(), 38);
    Some(answer)
}

/// The Agent's `rigctld` server, reached over TCP on this computer.
pub struct Server {
    address: SocketAddr,
    connection: Option<BufReader<TcpStream>>,
}

impl Server {
    /// The server at `address`, connected to when first asked.
    #[must_use]
    pub fn at(address: SocketAddr) -> Self {
        Self {
            address,
            connection: None,
        }
    }

    /// Connects, if not connected: as the port opens, so the program using
    /// it is known to the server from the start.
    ///
    /// # Errors
    ///
    /// The server can't be reached.
    pub fn connect(&mut self) -> Result<(), String> {
        if self.connection.is_none() {
            let stream = TcpStream::connect_timeout(&self.address, ANSWER_WITHIN)
                .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(ANSWER_WITHIN))
                .map_err(|error| error.to_string())?;
            let _ = stream.set_nodelay(true);
            self.connection = Some(BufReader::new(stream));
        }
        Ok(())
    }
}

impl Rig for Server {
    fn ask(&mut self, line: &str) -> Result<Vec<(String, String)>, String> {
        self.connect()?;
        let Some(reader) = self.connection.as_mut() else {
            return Err("not connected".to_owned());
        };
        let sent = reader
            .get_mut()
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|error| error.to_string());
        if let Err(error) = sent {
            self.connection = None;
            return Err(error);
        }
        let mut values = Vec::new();
        loop {
            let mut text = String::new();
            match reader.read_line(&mut text) {
                Ok(0) | Err(_) => {
                    self.connection = None;
                    return Err("the Agent's radio server closed".to_owned());
                }
                Ok(_) => {}
            }
            let text = text.trim();
            if let Some(code) = text.strip_prefix("RPRT ") {
                return if code == "0" {
                    Ok(values)
                } else {
                    Err(format!("refused (RPRT {code})"))
                };
            }
            if let Some((name, value)) = text.split_once(": ") {
                values.push((name.to_owned(), value.to_owned()));
            }
        }
    }
}

/// Answers Kenwood commands read from `port` until it closes or `stop`,
/// asking `rig`. Whatever isn't a command (a stray carriage return) is
/// skipped.
pub fn serve(port: &mut dyn ReadWrite, rig: &mut dyn Rig, stop: &Arc<AtomicBool>) {
    let mut command = Vec::new();
    let mut byte = [0u8; 1];
    while !stop.load(Ordering::SeqCst) {
        match port.read(&mut byte) {
            Ok(0) => return,
            Ok(_) if byte[0] == b';' => {
                let text = String::from_utf8_lossy(&command).to_ascii_uppercase();
                command.clear();
                let answer = respond(&text, rig);
                if !answer.is_empty()
                    && port
                        .write_all(answer.as_bytes())
                        .and_then(|()| port.flush())
                        .is_err()
                {
                    return;
                }
            }
            Ok(_) if byte[0].is_ascii_graphic() && command.len() < MAX_COMMAND => {
                command.push(byte[0]);
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// The virtual radio, answering on a serial port until dropped.
pub struct Virtual {
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
}

impl Virtual {
    /// Opens the serial port named `name` (one end of a virtual COM-port
    /// pair; the program opens the other) and answers on it, asking the
    /// Agent's `rigctld` server at `server`.
    ///
    /// # Errors
    ///
    /// The port can't be opened.
    pub fn start(name: &str, server: SocketAddr) -> Result<Self, String> {
        // A virtual port carries bytes at any speed; 9600 is what Winlink
        // Express offers first.
        let mut port = serialport::new(name, 9600)
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|error| format!("can't open {name}: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let serving = std::thread::spawn(move || {
            let mut rig = Server::at(server);
            let _ = rig.connect();
            serve(&mut port, &mut rig, &flag);
            // Going, while keyed: unkey. The server unkeys a program that
            // goes, too.
            let _ = rig.ask("+T 0");
        });
        Ok(Self {
            stop,
            serving: Some(serving),
        })
    }
}

impl Drop for Virtual {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// A port the virtual radio is read and written through.
pub trait ReadWrite: Read + Write {}

impl<T: Read + Write> ReadWrite for T {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::{Rig, respond, serve};

    /// A radio on 14.105 MHz in data USB, receiving, that records what it
    /// was asked and refuses whatever `refuse` starts.
    struct Bench {
        asked: Vec<String>,
        refuse: &'static str,
        hz: u64,
        mode: &'static str,
        ptt: bool,
    }

    impl Bench {
        fn new() -> Self {
            Self {
                asked: Vec::new(),
                refuse: "nothing",
                hz: 14_105_000,
                mode: "PKTUSB",
                ptt: false,
            }
        }
    }

    impl Rig for Bench {
        fn ask(&mut self, line: &str) -> Result<Vec<(String, String)>, String> {
            self.asked.push(line.to_owned());
            if line.starts_with(self.refuse) {
                return Err("refused (RPRT -9)".to_owned());
            }
            let pair = |name: &str, value: String| vec![(name.to_owned(), value)];
            Ok(match line {
                "+f" => pair("Frequency", self.hz.to_string()),
                "+m" => vec![
                    ("Mode".to_owned(), self.mode.to_owned()),
                    ("Passband".to_owned(), "3000".to_owned()),
                ],
                "+t" => pair("PTT", u8::from(self.ptt).to_string()),
                _ => Vec::new(),
            })
        }
    }

    #[test]
    fn answers_like_a_ts2000() {
        let mut rig = Bench::new();
        assert_eq!(respond("ID", &mut rig), "ID019;");
        assert_eq!(respond("FA", &mut rig), "FA00014105000;");
        assert_eq!(respond("FB", &mut rig), "FB00014105000;");
        assert_eq!(respond("MD", &mut rig), "MD2;");
        assert_eq!(respond("PS", &mut rig), "PS1;");
        assert_eq!(respond("AI", &mut rig), "AI0;");
        assert_eq!(respond("FR", &mut rig), "FR0;");
        let info = respond("IF", &mut rig);
        assert_eq!(info.len(), 38, "{info}");
        assert_eq!(&info[2..13], "00014105000");
        assert_eq!(&info[28..31], "020", "receiving, USB, VFO A");
        assert_eq!(&info[32..33], "0", "no split");
        assert!(info.ends_with(';'));
        rig.ptt = true;
        rig.mode = "LSB";
        let info = respond("IF", &mut rig);
        assert_eq!(&info[28..30], "11", "transmitting, LSB");
    }

    #[test]
    fn tunes_and_keys_through_the_radio_server() {
        let mut rig = Bench::new();
        assert_eq!(respond("FA00007103000", &mut rig), "");
        assert_eq!(respond("MD2", &mut rig), "");
        assert_eq!(respond("MD1", &mut rig), "");
        assert_eq!(respond("TX", &mut rig), "");
        assert_eq!(respond("RX", &mut rig), "");
        assert_eq!(respond("TX1", &mut rig), "");
        assert_eq!(
            rig.asked,
            [
                "+F 7103000",
                "+M PKTUSB 0",
                "+M LSB 0",
                "+T 1",
                "+T 0",
                "+T 1"
            ]
        );
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let mut rig = Bench::new();
        for nonsense in [
            "ZZ",
            "FA123",
            "FAx0007103000",
            "MD8",
            "FB00007103000",
            "FR1",
            "AI2",
        ] {
            assert_eq!(respond(nonsense, &mut rig), "?;", "{nonsense}");
        }
        rig.refuse = "+T";
        assert_eq!(respond("TX", &mut rig), "?;");
        rig.refuse = "+F";
        assert_eq!(respond("FA00007103000", &mut rig), "?;");
        rig.refuse = "+f";
        assert_eq!(respond("FA", &mut rig), "?;");
        assert_eq!(respond("IF", &mut rig), "?;");
    }

    /// A serial port: what the program sends, and what it got back.
    struct Port {
        incoming: VecDeque<u8>,
        sent: Vec<u8>,
    }

    impl Read for Port {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match (self.incoming.pop_front(), buffer.first_mut()) {
                (Some(byte), Some(slot)) => {
                    *slot = byte;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    impl Write for Port {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn serves_a_port_command_by_command() {
        let mut port = Port {
            incoming: b"id;\r\nFA;FA00007103000;MD;TX;".iter().copied().collect(),
            sent: Vec::new(),
        };
        let mut rig = Bench::new();
        serve(&mut port, &mut rig, &Arc::new(AtomicBool::new(false)));
        assert_eq!(
            String::from_utf8_lossy(&port.sent),
            "ID019;FA00014105000;MD2;"
        );
        assert_eq!(rig.asked, ["+f", "+F 7103000", "+m", "+T 1"]);
    }
}
