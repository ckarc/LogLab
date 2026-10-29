// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A pretend FlexRadio on this computer, answering in the SmartSDR TCP/IP
//! API as FlexRadio's API wiki describes it, so the FlexRadio driver is
//! built and tested, and `--flex-pretend` can be tried, without a radio.
//!
//! It has one slice, A, which transmits. It tunes, changes mode and keys
//! when told, and says so in status lines as the radio does; anything else
//! it doesn't know it answers with the API's "malformed command" code.
//! While keyed, it sends meter packets (forward power, SWR and ALC) to the
//! UDP port it was given, as VITA-49 packets laid out as the API's
//! Metering Protocol page describes.

use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::flex::parse_mhz;

/// The status handle the pretend radio gives itself.
const HANDLE: &str = "2A3B4C5D";
/// The modes it has: the API's list of supported modes, in capitals.
const MODES: [&str; 11] = [
    "USB", "LSB", "CW", "AM", "SAM", "FM", "NFM", "DFM", "DIGL", "DIGU", "RTTY",
];
/// Its range: 30 kHz to 54 MHz, as a FLEX-6000's receiver.
const RANGE_HZ: (u64, u64) = (30_000, 54_000_000);
/// Its meters while keyed: number, name, unit, raw value (1/128 steps):
/// about 40 W forward, SWR 1.3, ALC at -10 dBFS.
const METERS: [(u16, &str, &str, i16); 3] = [
    (15, "FWDPWR", "dBm", 5891),
    (17, "SWR", "SWR", 166),
    (7, "ALC", "dBFS", -1280),
];
/// How often it sends meter packets while keyed.
const METER_EVERY: Duration = Duration::from_millis(50);

/// What the pretend radio is doing.
struct State {
    model: String,
    can_transmit: bool,
    hz: u64,
    mode: String,
    transmitting: bool,
    heard: Vec<String>,
    /// Where on this computer to send meter packets.
    meter_port: Option<u16>,
}

/// The pretend radio, listening on 127.0.0.1.
pub struct PretendFlex {
    address: String,
    /// What it is doing, for tests to look at.
    #[cfg(test)]
    state: Arc<Mutex<State>>,
}

impl PretendFlex {
    /// Starts a pretend radio of `model` on a free port on this computer,
    /// on 14.074 MHz DIGU. One program at a time may connect. Without
    /// `can_transmit` it refuses `xmit`, as a radio with no transmitter
    /// does.
    ///
    /// # Errors
    ///
    /// No port could be opened on this computer.
    pub fn start(model: &str, can_transmit: bool) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?.to_string();
        let state = Arc::new(Mutex::new(State {
            model: model.to_owned(),
            can_transmit,
            hz: 14_074_000,
            mode: "DIGU".to_owned(),
            transmitting: false,
            heard: Vec::new(),
            meter_port: None,
        }));
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let _ = serve(stream, &shared);
            }
        });
        let meters = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let shared = Arc::downgrade(&state);
        thread::spawn(move || {
            while let Some(state) = shared.upgrade() {
                let port = state
                    .lock()
                    .ok()
                    .and_then(|s| s.transmitting.then_some(s.meter_port).flatten());
                drop(state);
                if let Some(port) = port {
                    let _ = meters.send_to(&meter_packet(), (Ipv4Addr::LOCALHOST, port));
                }
                thread::sleep(METER_EVERY);
            }
        });
        Ok(Self {
            address,
            #[cfg(test)]
            state,
        })
    }

    /// Where it listens, as `--flex` takes it.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    #[cfg(test)]
    /// Every command it was sent, without its number.
    #[must_use]
    pub fn heard(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.heard.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    /// Whether it is transmitting.
    #[must_use]
    pub fn transmitting(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.transmitting)
    }
}

/// Talks with one program until it goes away.
fn serve(stream: TcpStream, state: &Mutex<State>) -> io::Result<()> {
    let mut out = stream.try_clone()?;
    out.write_all(format!("V1.4.0.0\nH{HANDLE}\n").as_bytes())?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let Some((seq, command)) = line.strip_prefix('C').and_then(|l| l.split_once('|')) else {
            continue;
        };
        let reply = match state.lock() {
            Ok(mut state) => state.answer(seq, command),
            Err(_) => break,
        };
        out.write_all(reply.as_bytes())?;
    }
    Ok(())
}

impl State {
    /// The lines it sends back for command `seq`: any status lines, then the
    /// answer.
    fn answer(&mut self, seq: &str, command: &str) -> String {
        self.heard.push(command.to_owned());
        let status = |text: String| format!("S{HANDLE}|{text}\n");
        let answer = |code: u32, text: &str| format!("R{seq}|{code:X}|{text}\n");
        let words: Vec<&str> = command.split(' ').collect();
        match words.as_slice() {
            ["info"] => answer(
                0,
                &format!("model=\"{}\",name=\"Pretend\",callsign=\"\"", self.model),
            ),
            ["sub", "slice", "all"] => status(self.slice_status()) + &answer(0, ""),
            ["sub", "tx", "all"] => status(self.interlock_status()) + &answer(0, ""),
            ["sub", "radio", "all"] | ["ping"] => answer(0, ""),
            ["client", "udpport", port] => match port.parse() {
                Ok(port) => {
                    self.meter_port = Some(port);
                    answer(0, "")
                }
                Err(_) => answer(0x5000_0016, ""),
            },
            ["sub", "meter", "all"] => {
                let mut described = String::new();
                for (n, name, unit, _) in METERS {
                    let _ = write!(
                        described,
                        "{n}.src=TX-#{n}.num=1#{n}.nam={name}#{n}.unit={unit}#{n}.desc=a pretend meter#"
                    );
                }
                status(format!("meter {described}")) + &answer(0, "")
            }
            ["slice", "tune" | "t", slice, frequency] => match (*slice, parse_mhz(frequency)) {
                ("0", Ok(hz)) if (RANGE_HZ.0..=RANGE_HZ.1).contains(&hz) => {
                    self.hz = hz;
                    status(format!("slice 0 RF_frequency={}", mhz(hz))) + &answer(0, "")
                }
                ("0", _) => answer(0x5000_000C, ""),
                _ => answer(0x5000_000D, ""),
            },
            ["slice", "set" | "s", slice, setting] => match (*slice, setting.split_once('=')) {
                ("0", Some(("mode", mode))) if MODES.contains(&mode) => {
                    mode.clone_into(&mut self.mode);
                    status(format!("slice 0 mode={mode}")) + &answer(0, "")
                }
                ("0", Some(("mode", _))) => answer(0x5000_0032, ""),
                ("0", _) => answer(0x5000_002D, ""),
                _ => answer(0x5000_000D, ""),
            },
            ["xmit", on @ ("0" | "1")] if self.can_transmit => {
                let on = *on == "1";
                let (passing, settled) = if on {
                    ("PTT_REQUESTED", "TRANSMITTING")
                } else {
                    ("UNKEY_REQUESTED", "READY")
                };
                self.transmitting = on;
                answer(0, "")
                    + &status(format!("interlock state={passing} reason= source=SW"))
                    + &status(format!("interlock state={settled} reason= source=SW"))
            }
            ["xmit", "0" | "1"] => answer(0x5000_003D, ""),
            _ => answer(0x5000_0016, ""),
        }
    }

    fn slice_status(&self) -> String {
        format!(
            "slice 0 in_use=1 RF_frequency={} mode={} filter_lo=100 filter_hi=3100 \
             index_letter=A tx=1 active=1",
            mhz(self.hz),
            self.mode
        )
    }

    fn interlock_status(&self) -> String {
        let state = if self.transmitting {
            "TRANSMITTING"
        } else {
            "READY"
        };
        format!("interlock state={state} reason= source=")
    }
}

/// One VITA-49 meter packet with every meter's reading: an extension data
/// packet with a stream ID, a class ID (packet class 0x8002) and both
/// timestamps, then (number, value) pairs.
fn meter_packet() -> Vec<u8> {
    let words = 7 + METERS.len();
    let size = u32::try_from(words).unwrap_or(0);
    let header: u32 = 0x3000_0000 | 0x0800_0000 | 0x0040_0000 | 0x0010_0000 | size;
    let mut packet = Vec::with_capacity(words * 4);
    for word in [header, 0x0000_0700, 0x0000_1C2D, 0x534C_8002, 0, 0, 0] {
        packet.extend_from_slice(&word.to_be_bytes());
    }
    for (number, _, _, value) in METERS {
        packet.extend_from_slice(&number.to_be_bytes());
        packet.extend_from_slice(&value.to_be_bytes());
    }
    packet
}

/// Hertz in megahertz as the radio writes it: "14.074000".
fn mhz(hz: u64) -> String {
    format!("{}.{:06}", hz / 1_000_000, hz % 1_000_000)
}
