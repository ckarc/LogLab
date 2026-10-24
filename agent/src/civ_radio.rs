// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Agent's own driver for Icom radios over CI-V. It only reads.
//!
//! Given a [`Port`] and the rig definitions the Agent knows, the driver:
//!
//! 1. tries each speed the definitions list, and at each asks every known
//!    radio "who are you?" (`19 00`) at its default address;
//! 2. takes the first radio that answers as the radio, and its definition as
//!    the description of it;
//! 3. reads it by asking: frequency (`03`), mode (`04`), data mode (`1A 06`)
//!    and transmit status (`1C 00`).
//!
//! While it waits for an answer it skips the copy of its own message a radio
//! with echo-back on sends first, and any report a radio sends on its own
//! (to everyone, address 00h), so neither is mistaken for the answer.
//!
//! [`Ask`] is the whole list of what the driver may say to a radio, and none
//! of it tunes the radio or keys it. A real serial port is not here yet: it
//! comes once there is a radio to test it on. Until then the driver runs
//! against the pretend radio through [`PretendPort`], which is how it is
//! tested and what `loglab-agent read --civ-pretend` uses.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::civ::{self, Decoder, Frame, NG};
use crate::civ_pretend::PretendCiv;
use crate::rig::{ControlPath, MAX_FREQUENCY_HZ, Mode, Radio, RadioError, RadioIdentity, Reading};
use crate::rigdef::Definition;

/// How long the driver waits for one answer.
const ANSWER_WAIT: Duration = Duration::from_millis(300);
/// How long it listens in each slice of that wait.
const LISTEN: Duration = Duration::from_millis(20);
/// The most messages it skips while waiting for one answer.
const MAX_SKIPPED: usize = 32;

/// What the CI-V driver needs from a port.
pub trait Port {
    /// Switch to `baud`.
    ///
    /// # Errors
    ///
    /// The port refused the speed.
    fn set_baud(&mut self, baud: u32) -> Result<(), String>;

    /// Send bytes.
    ///
    /// # Errors
    ///
    /// The port could not be written.
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;

    /// The bytes that arrive within `wait`; empty if none did.
    ///
    /// # Errors
    ///
    /// The port could not be read.
    fn receive(&mut self, wait: Duration) -> Result<Vec<u8>, String>;
}

/// Everything the driver may say to a radio. All of it only asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// "Who are you?"
    Identity,
    /// The selected VFO's frequency.
    Frequency,
    /// The selected VFO's mode and filter.
    Mode,
    /// Whether data mode is on.
    DataMode,
    /// Whether the radio is transmitting.
    Transmitting,
    /// A menu setting's value, by its item number.
    Setting([u8; 2]),
}

impl Ask {
    /// Every question there is, for the test that none of them changes a radio.
    #[cfg(test)]
    pub const ALL: [Self; 5] = [
        Self::Identity,
        Self::Frequency,
        Self::Mode,
        Self::DataMode,
        Self::Transmitting,
    ];

    /// The command bytes for this question.
    #[must_use]
    pub fn payload(self) -> Vec<u8> {
        match self {
            Self::Identity => vec![0x19, 0x00],
            Self::Frequency => vec![0x03],
            Self::Mode => vec![0x04],
            Self::DataMode => vec![0x1A, 0x06],
            Self::Transmitting => vec![0x1C, 0x00],
            // With no value after the item number, 1A 05 only asks.
            Self::Setting([a, b]) => vec![0x1A, 0x05, a, b],
        }
    }
}

/// An Icom radio found on a port, read over CI-V.
pub struct CivRadio<P: Port> {
    port: P,
    definition: Definition,
    identity: RadioIdentity,
    address: u8,
    me: u8,
    modes: BTreeMap<u8, String>,
    decoder: Decoder,
}

impl<P: Port> CivRadio<P> {
    /// Finds which known radio is on `port`, and at what speed.
    ///
    /// # Errors
    ///
    /// No known radio answered at any speed, or the port failed.
    pub fn find(mut port: P, definitions: &[Definition]) -> Result<Self, RadioError> {
        let mut speeds: Vec<u32> = Vec::new();
        for definition in definitions {
            for baud in &definition.baud_rates {
                if !speeds.contains(baud) {
                    speeds.push(*baud);
                }
            }
        }
        for baud in &speeds {
            port.set_baud(*baud).map_err(RadioError)?;
            for definition in definitions {
                let mut radio = Self::new(port, definition);
                if radio.ask(Ask::Identity).ok().as_deref()
                    == Some(&[0x19, 0x00, definition.default_address][..])
                {
                    return Ok(radio);
                }
                port = radio.port;
            }
        }
        let names: Vec<String> = definitions
            .iter()
            .map(|d| format!("{} {} at {:02X}h", d.maker, d.model, d.default_address))
            .collect();
        Err(RadioError(format!(
            "no radio answered at any speed ({}); looked for {}",
            speeds
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            names.join(", ")
        )))
    }

    fn new(port: P, definition: &Definition) -> Self {
        Self {
            port,
            definition: definition.clone(),
            identity: RadioIdentity {
                maker: definition.maker.clone(),
                model: definition.model.clone(),
                path: ControlPath::Civ,
            },
            address: definition.default_address,
            me: definition.controller_address,
            modes: definition
                .mode_codes
                .iter()
                .map(|(name, code)| (*code, name.clone()))
                .collect(),
            decoder: Decoder::default(),
        }
    }

    /// The definition of the radio that answered.
    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// Reads one menu setting. Returns the value's bytes as the radio sent
    /// them: one byte for a named value, two BCD bytes for a level.
    ///
    /// # Errors
    ///
    /// The radio refused, didn't answer, or answered about something else.
    pub fn read_setting(&mut self, item: [u8; 2]) -> Result<Vec<u8>, RadioError> {
        match self.ask(Ask::Setting(item))?.as_slice() {
            [0x1A, 0x05, a, b, value @ ..] if [*a, *b] == item && !value.is_empty() => {
                Ok(value.to_vec())
            }
            other => Err(RadioError(format!(
                "odd answer to 1A 05 {}: {}",
                civ::hex(&item),
                civ::hex(other)
            ))),
        }
    }

    /// Asks one question and returns the radio's answer.
    fn ask(&mut self, ask: Ask) -> Result<Vec<u8>, RadioError> {
        let payload = ask.payload();
        let sent = Frame::new(self.address, self.me, &payload);
        self.port.send(&sent.encode()).map_err(RadioError)?;
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut skipped = 0;
        while Instant::now() < deadline {
            let bytes = self.port.receive(LISTEN).map_err(RadioError)?;
            for frame in self.decoder.push(&bytes) {
                // An answer comes from the radio, to us.
                if frame.from == self.address && frame.to == self.me {
                    if frame.payload == [NG] {
                        return Err(RadioError(format!(
                            "the radio refused {} (FA)",
                            civ::hex(&payload)
                        )));
                    }
                    return Ok(frame.payload);
                }
                // Our own message echoed back, a report to everyone
                // (BROADCAST), or a message for someone else: not the answer.
                skipped += 1;
                if skipped > MAX_SKIPPED {
                    return Err(RadioError(
                        "too many messages that aren't the answer".to_owned(),
                    ));
                }
            }
        }
        Err(RadioError(format!(
            "no answer to {} within {} ms",
            civ::hex(&payload),
            ANSWER_WAIT.as_millis()
        )))
    }

    fn mode(&self, code: u8, data: bool) -> Mode {
        match (self.modes.get(&code).map(String::as_str), data) {
            (Some("USB"), true) => Mode::DataUsb,
            (Some("LSB"), true) => Mode::DataLsb,
            (Some("FM"), true) => Mode::DataFm,
            (Some("USB"), false) => Mode::Usb,
            (Some("LSB"), false) => Mode::Lsb,
            (Some("FM"), false) => Mode::Fm,
            (Some("AM"), _) => Mode::Am,
            (Some("CW"), _) => Mode::Cw,
            (Some("CW-R"), _) => Mode::CwReverse,
            (Some("RTTY"), _) => Mode::Rtty,
            (Some("RTTY-R"), _) => Mode::RttyReverse,
            _ => Mode::Other,
        }
    }
}

impl<P: Port> Radio for CivRadio<P> {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn read(&mut self) -> Result<Reading, RadioError> {
        let frequency_hz = match self.ask(Ask::Frequency)?.as_slice() {
            [0x03, bcd @ ..] => civ::frequency_from_bcd(bcd)
                .filter(|hz| (1..=MAX_FREQUENCY_HZ).contains(hz))
                .ok_or_else(|| RadioError(format!("not a frequency: {}", civ::hex(bcd))))?,
            other => return Err(RadioError(format!("odd answer to 03: {}", civ::hex(other)))),
        };
        let code = match self.ask(Ask::Mode)?.as_slice() {
            [0x04, code, ..] => *code,
            other => return Err(RadioError(format!("odd answer to 04: {}", civ::hex(other)))),
        };
        // A radio without data mode refuses 1A 06; that means it is off.
        let data = matches!(
            self.ask(Ask::DataMode).as_deref(),
            Ok([0x1A, 0x06, 0x01, ..])
        );
        let ptt = match self.ask(Ask::Transmitting).as_deref() {
            Ok([0x1C, 0x00, on]) => Some(*on == 0x01),
            _ => None,
        };
        Ok(Reading {
            frequency_hz,
            mode: Some(self.mode(code, data)),
            passband_hz: None,
            ptt,
            vfo: None,
        })
    }
}

/// The pretend radio, reached as if through a serial port. It only listens
/// at its own speed: at any other, it sends back noise, as a real radio's
/// port does when the speeds don't match.
pub struct PretendPort {
    radio: PretendCiv,
    speed: u32,
    baud: u32,
    waiting: Vec<u8>,
}

impl PretendPort {
    /// The pretend radio, listening at `speed`.
    #[must_use]
    pub fn new(radio: PretendCiv, speed: u32) -> Self {
        Self {
            radio,
            speed,
            baud: 0,
            waiting: Vec::new(),
        }
    }

    /// The pretend radio itself, to change it as if from its front panel.
    #[cfg(test)]
    pub fn radio(&mut self) -> &mut PretendCiv {
        &mut self.radio
    }

    /// Bytes the radio sends on its own, queued to arrive next.
    #[cfg(test)]
    pub fn arrive(&mut self, bytes: &[u8]) {
        self.waiting.extend_from_slice(bytes);
    }
}

impl Port for PretendPort {
    fn set_baud(&mut self, baud: u32) -> Result<(), String> {
        self.baud = baud;
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.baud == self.speed {
            let answer = self.radio.receive(bytes);
            self.waiting.extend(answer);
        } else {
            // What mismatched speeds look like: bytes, but not CI-V.
            self.waiting.extend([0x00, 0x80, 0xF8, 0x78]);
        }
        Ok(())
    }

    fn receive(&mut self, wait: Duration) -> Result<Vec<u8>, String> {
        if self.waiting.is_empty() {
            // Nothing came: the wait passes, as it would on a real port.
            std::thread::sleep(wait);
        }
        Ok(std::mem::take(&mut self.waiting))
    }
}

#[cfg(test)]
mod tests {
    use super::{Ask, CivRadio, Port, PretendPort};
    use crate::civ::Frame;
    use crate::civ_pretend::PretendCiv;
    use crate::rig::{ControlPath, Mode, Radio};
    use crate::rigdef::{self, Definition};

    fn ic_7300() -> Definition {
        rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"))
    }

    fn found(speed: u32) -> CivRadio<PretendPort> {
        let definition = ic_7300();
        let port = PretendPort::new(PretendCiv::new(&definition), speed);
        CivRadio::find(port, &[definition]).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn finds_the_radio_at_any_listed_speed() {
        for speed in ic_7300().baud_rates {
            let radio = found(speed);
            assert_eq!(radio.identity().model, "IC-7300");
            assert_eq!(radio.identity().path, ControlPath::Civ);
            assert_eq!(radio.port.baud, speed, "settled on the radio's speed");
        }
    }

    #[test]
    fn says_when_no_radio_answers() {
        let definition = ic_7300();
        let port = PretendPort::new(PretendCiv::new(&definition), 1200);
        let error = CivRadio::find(port, &[definition])
            .err()
            .map(|e| e.to_string());
        let error = error.unwrap_or_default();
        assert!(error.contains("no radio answered"), "{error}");
        assert!(error.contains("IC-7300 at 94h"), "{error}");
    }

    #[test]
    fn reads_frequency_mode_and_transmit_status() {
        let mut radio = found(115_200);
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 14_074_000);
        assert_eq!(reading.mode, Some(Mode::DataUsb), "USB with data mode on");
        assert_eq!(reading.ptt, Some(false));
    }

    #[test]
    fn follows_the_radio_when_its_dial_turns() {
        let mut radio = found(115_200);
        let report = radio.port.radio().turn_dial(7_030_000);
        radio.port.arrive(&report);
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            reading.frequency_hz, 7_030_000,
            "the radio's own report was skipped, and the answer read"
        );
    }

    #[test]
    fn reads_through_echo_back() {
        let mut radio = found(115_200);
        assert!(
            radio.port.radio().set_menu([0x00, 0x75], 0x00),
            "echo back on"
        );
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 14_074_000);
    }

    #[test]
    fn reports_plain_modes_without_data() {
        let mut radio = found(115_200);
        // Switch the pretend radio to CW from its "front panel".
        radio
            .port
            .radio()
            .receive(&Frame::new(0x94, 0xE0, &[0x06, 0x03]).encode());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.mode, Some(Mode::Cw));
    }

    #[test]
    fn only_asks_questions() {
        // The commands that change an Icom radio carry data after their
        // command bytes (05, 06, 07 xx, 0F xx, 1A 05 .. value, 1C 00 xx).
        // Every question here is one of the bare read forms.
        let reads: [&[u8]; 5] = [
            &[0x19, 0x00],
            &[0x03],
            &[0x04],
            &[0x1A, 0x06],
            &[0x1C, 0x00],
        ];
        let asked: Vec<Vec<u8>> = Ask::ALL.iter().map(|ask| ask.payload()).collect();
        assert_eq!(asked, reads);
        // A setting is asked by its item number alone, with no value after.
        assert_eq!(
            Ask::Setting([0x00, 0x67]).payload(),
            [0x1A, 0x05, 0x00, 0x67]
        );
        let mut radio = found(115_200);
        assert!(radio.read().is_ok());
        assert!(!radio.port.radio().transmitting(), "reading never keys it");
    }

    #[test]
    fn reads_a_menu_setting() {
        let mut radio = found(115_200);
        assert_eq!(
            radio.read_setting([0x00, 0x67]).ok(),
            Some(vec![0x00]),
            "DATA MOD is MIC"
        );
        assert_eq!(
            radio.read_setting([0x00, 0x65]).ok(),
            Some(vec![0x01, 0x27]),
            "level 127"
        );
        assert!(radio.read_setting([0x09, 0x99]).is_err(), "no such item");
    }

    #[test]
    fn pretend_port_sends_noise_at_the_wrong_speed() {
        let definition = ic_7300();
        let mut port = PretendPort::new(PretendCiv::new(&definition), 19_200);
        assert!(port.set_baud(9_600).is_ok());
        assert!(port.send(&Frame::new(0x94, 0xE0, &[0x03]).encode()).is_ok());
        let noise = port.receive(super::LISTEN).unwrap_or_default();
        assert!(!noise.is_empty());
        assert!(crate::civ::Decoder::default().push(&noise).is_empty());
    }
}
