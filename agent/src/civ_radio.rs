// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Agent's own driver for Icom radios over CI-V. It reads and tunes, and
//! keys the transmitter only through [`Keyable`], which only
//! `serve --allow-transmit` uses.
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
//! [`Ask`] is the whole list of questions the driver may ask a radio, and
//! none of it tunes the radio or keys it. [`Change`] is the whole list of
//! changes it may ask for, and none of it keys it either: the one transmit
//! command is [`transmit_payload`], kept apart. A real radio is reached through a serial
//! port (`civ_serial`, `--civ COM5`); the pretend radio through
//! [`PretendPort`], which is how the driver is tested and what
//! `loglab-agent read --civ-pretend` uses.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::civ::{self, Decoder, Frame, NG, OK};
use crate::civ_pretend::PretendCiv;
use crate::rig::{
    ControlPath, Exposes, Keyable, MAX_FREQUENCY_HZ, Mode, Radio, RadioError, RadioIdentity,
    Reading, Tunable, Tuning, TxMeters, Vfo,
};
use crate::rigdef::{self, Definition, SettingSummary};

/// How long the driver waits for one answer.
const ANSWER_WAIT: Duration = Duration::from_millis(300);
/// How long it listens in each slice of that wait.
const LISTEN: Duration = Duration::from_millis(20);
/// The most messages it skips while waiting for one answer.
const MAX_SKIPPED: usize = 32;
/// The most bytes one setting's value may have, as the Engine keeps them.
const MAX_SETTING_BYTES: usize = 64;

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
    /// The attenuator (`11`).
    Attenuator,
    /// A transmit meter (`15` and which: `11` Po, `12` SWR, `13` ALC).
    /// Bare, it only asks.
    Meter(u8),
    /// A front-panel control's value, by its command, such as `14 02` for
    /// the RF gain. Only commands a rig definition may read settings with
    /// get here (`rigdef::READABLE`): each only asks when sent bare.
    Control([u8; 2]),
}

impl Ask {
    /// Every question there is, for the test that none of them changes a radio.
    #[cfg(test)]
    pub const ALL: [Self; 8] = [
        Self::Identity,
        Self::Frequency,
        Self::Mode,
        Self::DataMode,
        Self::Transmitting,
        Self::Meter(POWER_METER),
        Self::Meter(SWR_METER),
        Self::Meter(ALC_METER),
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
            Self::Attenuator => vec![0x11],
            Self::Meter(which) => vec![0x15, which],
            Self::Control([a, b]) => vec![a, b],
        }
    }
}

/// Everything the driver may ask a radio to change. It holds no transmit
/// command (`1C 00` with a value): tuning never keys a transmitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Tune the selected VFO (`05`), to these BCD bytes.
    Frequency([u8; 5]),
    /// Tune the unselected VFO (`25 01`), to these BCD bytes.
    OtherFrequency([u8; 5]),
    /// Change the selected VFO's mode (`06`), to this mode code, on this
    /// filter (FIL1 to FIL3).
    Mode(u8, u8),
    /// Turn data mode on, on this filter, or off (`1A 06`; off takes 00).
    DataMode(bool, u8),
    /// Select VFO A (`07 00`) or B (`07 01`).
    Select(Vfo),
    /// Turn split off (`0F 00`) or on (`0F 01`).
    Split(bool),
}

impl Change {
    /// The command bytes for this change.
    #[must_use]
    pub fn payload(self) -> Vec<u8> {
        match self {
            Self::Frequency(bcd) => [&[0x05][..], &bcd].concat(),
            Self::OtherFrequency(bcd) => [&[0x25, 0x01][..], &bcd].concat(),
            Self::Mode(code, filter) => vec![0x06, code, filter],
            Self::DataMode(true, filter) => vec![0x1A, 0x06, 0x01, filter],
            Self::DataMode(false, _) => vec![0x1A, 0x06, 0x00, 0x00],
            Self::Select(Vfo::A) => vec![0x07, 0x00],
            Self::Select(Vfo::B) => vec![0x07, 0x01],
            Self::Split(on) => vec![0x0F, u8::from(on)],
        }
    }
}

/// The driver's one transmit command: `1C 00 01` keys the transmitter and
/// `1C 00 00` unkeys it.
#[must_use]
pub fn transmit_payload(on: bool) -> Vec<u8> {
    vec![0x1C, 0x00, u8::from(on)]
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
    baud: u32,
}

impl<P: Port> CivRadio<P> {
    /// Finds which known radio is on `port`, and at what speed.
    ///
    /// # Errors
    ///
    /// No known radio answered at any speed, or the port failed.
    pub fn find(mut port: P, definitions: &[Definition]) -> Result<Self, RadioError> {
        // Only Icom radios: another family's radio isn't asked in CI-V.
        let icom: Vec<Definition> = definitions
            .iter()
            .filter(|d| d.family == rigdef::ICOM_CIV)
            .cloned()
            .collect();
        let definitions = icom.as_slice();
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
                radio.baud = *baud;
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
            baud: 0,
        }
    }

    /// The definition of the radio that answered.
    #[cfg(test)]
    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// The port, to reach a pretend radio behind it in tests.
    #[cfg(test)]
    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// The speed the radio answered at.
    pub fn baud(&self) -> u32 {
        self.baud
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

    /// Reads one setting the definition lists, menu item or control.
    /// Returns the value's bytes as the radio sent them.
    ///
    /// # Errors
    ///
    /// The radio refused, didn't answer, or answered about something else.
    fn read_one(&mut self, setting: &SettingSummary) -> Result<Vec<u8>, RadioError> {
        let ask = match (setting.item(), setting.read.as_slice()) {
            (Some(item), _) => return self.read_setting(item),
            (None, [0x04]) => Ask::Mode,
            (None, [0x11]) => Ask::Attenuator,
            (None, [a, b]) => Ask::Control([*a, *b]),
            (None, other) => {
                return Err(RadioError(format!(
                    "{} is not a command settings are read with",
                    civ::hex(other)
                )));
            }
        };
        let asked = ask.payload();
        match self.ask(ask)? {
            answer if answer.len() > asked.len() && answer.starts_with(&asked) => {
                Ok(answer[asked.len()..].to_vec())
            }
            other => Err(RadioError(format!(
                "odd answer to {}: {}",
                civ::hex(&asked),
                civ::hex(&other)
            ))),
        }
    }

    /// Reads one setting, writes exactly the bytes it just read back to it,
    /// and reads it again: a test of the command that sets it which changes
    /// nothing on the radio, since nothing remembered is written
    /// (Principle X). Only for a setting the Agent may ever change.
    ///
    /// # Errors
    ///
    /// The setting is one the Agent never changes, or couldn't be read
    /// before the write.
    pub fn write_back(&mut self, setting: &SettingSummary) -> Result<WriteBack, RadioError> {
        if !setting.write {
            return Err(RadioError(format!(
                "{} is one the Agent never changes",
                setting.label
            )));
        }
        let before = self.read_one(setting)?;
        let command = set_command(setting, &before)?;
        let written = self.command(&command);
        let after = self.read_one(setting);
        Ok(WriteBack {
            before,
            written: written.map_err(|error| error.to_string()),
            after: after.map_err(|error| error.to_string()),
        })
    }

    /// Asks one question and returns the radio's answer.
    fn ask(&mut self, ask: Ask) -> Result<Vec<u8>, RadioError> {
        self.exchange(&ask.payload())
    }

    /// Asks for one change, and returns once the radio says it's done (FB).
    fn change(&mut self, change: Change) -> Result<(), RadioError> {
        self.command(&change.payload())
    }

    /// Sends one command, and returns once the radio says it's done (FB).
    fn command(&mut self, payload: &[u8]) -> Result<(), RadioError> {
        match self.exchange(payload)?.as_slice() {
            [OK] => Ok(()),
            other => Err(RadioError(format!(
                "odd answer to {}: {}",
                civ::hex(payload),
                civ::hex(other)
            ))),
        }
    }

    /// Sends one message and returns the radio's answer to it.
    fn exchange(&mut self, payload: &[u8]) -> Result<Vec<u8>, RadioError> {
        let sent = Frame::new(self.address, self.me, payload);
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
                            civ::hex(payload)
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
            civ::hex(payload),
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

impl<P: Port> CivRadio<P> {
    fn bcd(hz: u64) -> Result<[u8; 5], RadioError> {
        civ::frequency_to_bcd(hz)
            .filter(|_| (1..=MAX_FREQUENCY_HZ).contains(&hz))
            .ok_or_else(|| RadioError(format!("{hz} Hz can't be sent to the radio")))
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), RadioError> {
        let (voice, data) = match mode {
            Mode::Usb => ("USB", Some(false)),
            Mode::Lsb => ("LSB", Some(false)),
            Mode::Fm => ("FM", Some(false)),
            Mode::DataUsb => ("USB", Some(true)),
            Mode::DataLsb => ("LSB", Some(true)),
            Mode::DataFm => ("FM", Some(true)),
            Mode::Am => ("AM", None),
            Mode::Cw => ("CW", None),
            Mode::CwReverse => ("CW-R", None),
            Mode::Rtty => ("RTTY", None),
            Mode::RttyReverse => ("RTTY-R", None),
            Mode::Other => return Err(RadioError("no such mode".to_owned())),
        };
        let code = self
            .definition
            .mode_codes
            .get(voice)
            .copied()
            .ok_or_else(|| RadioError(format!("the {} has no {voice}", self.identity.model)))?;
        // The filter the operator has selected stays selected: a program
        // asking for a mode doesn't choose the filter (Principle X).
        let filter = match self.ask(Ask::Mode)?.as_slice() {
            [0x04, _, filter @ 1..=3] => *filter,
            _ => 1,
        };
        self.change(Change::Mode(code, filter))?;
        match data {
            Some(on) => self.change(Change::DataMode(on, filter)),
            None => Ok(()),
        }
    }
}

impl<P: Port> Tunable for CivRadio<P> {
    fn tune(&mut self, tuning: Tuning) -> Result<(), RadioError> {
        match tuning {
            Tuning::Frequency(hz) => self.change(Change::Frequency(Self::bcd(hz)?)),
            Tuning::OtherFrequency(hz) => self.change(Change::OtherFrequency(Self::bcd(hz)?)),
            Tuning::Mode(mode) => self.set_mode(mode),
            Tuning::Select(vfo) => self.change(Change::Select(vfo)),
            Tuning::Split(on) => self.change(Change::Split(on)),
        }
    }
}

impl<P: Port> Keyable for CivRadio<P> {
    fn transmit(&mut self, on: bool) -> Result<(), RadioError> {
        self.command(&transmit_payload(on))
    }

    fn meters(&mut self) -> Result<Option<TxMeters>, RadioError> {
        let Some(meters) = self.definition.meters.clone() else {
            return Ok(None);
        };
        let mut level = |which: u8| -> Result<u16, RadioError> {
            match self.ask(Ask::Meter(which))?.as_slice() {
                [0x15, asked, bcd @ ..] if *asked == which && bcd.len() == 2 => {
                    civ::level_from_bcd(bcd)
                        .ok_or_else(|| RadioError(format!("not a meter level: {}", civ::hex(bcd))))
                }
                other => Err(RadioError(format!(
                    "odd answer to 15 {which:02X}: {}",
                    civ::hex(other)
                ))),
            }
        };
        let power = meters.power.value(level(POWER_METER)?);
        let swr_tenths = meters.swr.value(level(SWR_METER)?);
        let alc = meters.alc.value(level(ALC_METER)?);
        // Only the one question that only asks (rigdef checks it is 1C 01).
        let tuning = match &meters.tuner {
            Some((command, tuning)) if command.as_slice() == rigdef::TUNER_STATUS => {
                match self.exchange(command)?.as_slice() {
                    [asked @ .., state] if asked == command.as_slice() => state == tuning,
                    other => {
                        return Err(RadioError(format!(
                            "odd answer to {}: {}",
                            civ::hex(command),
                            civ::hex(other)
                        )));
                    }
                }
            }
            _ => false,
        };
        Ok(Some(TxMeters {
            power,
            swr_tenths,
            alc: Some(alc),
            tuning,
            readings: 0,
        }))
    }
}

/// The Po meter (`15 11`).
const POWER_METER: u8 = 0x11;
/// The SWR meter (`15 12`).
const SWR_METER: u8 = 0x12;
/// The ALC meter (`15 13`).
const ALC_METER: u8 = 0x13;

impl<P: Port> Exposes for CivRadio<P> {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn read_exposed(&mut self, setting: &SettingSummary) -> Result<Vec<u8>, RadioError> {
        self.read_one(setting)
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
        let command = set_command(setting, bytes)?;
        self.command(&command)
    }
}

/// What became of writing a setting's own value back to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteBack {
    /// The bytes the radio had, and was sent back.
    pub before: Vec<u8>,
    /// Whether the radio took the write (FB), or why not.
    pub written: Result<(), String>,
    /// What the radio had after, or why it couldn't be read.
    pub after: Result<Vec<u8>, String>,
}

impl WriteBack {
    /// Whether the radio took its own value and still has it: the command
    /// that sets the setting works.
    #[must_use]
    pub fn works(&self) -> bool {
        self.written.is_ok() && self.after.as_ref().is_ok_and(|after| *after == self.before)
    }
}

/// The command that sets `setting` to `bytes`: the command that reads it,
/// with the value after it.
fn set_command(setting: &SettingSummary, bytes: &[u8]) -> Result<Vec<u8>, RadioError> {
    if bytes.is_empty() || bytes.len() > MAX_SETTING_BYTES {
        return Err(RadioError(format!(
            "{} is not a value for {}",
            civ::hex(bytes),
            setting.label
        )));
    }
    let command = match (setting.item(), setting.read.as_slice()) {
        (Some(item), _) => Ask::Setting(item).payload(),
        // The mode and filter are read with 04 and set with 06, the same
        // two bytes after it (Icom CI-V reference, commands 04 and 06).
        (None, [0x04]) => vec![0x06],
        (None, [0x11]) => Ask::Attenuator.payload(),
        (None, [a, b]) => Ask::Control([*a, *b]).payload(),
        (None, other) => {
            return Err(RadioError(format!(
                "{} is not a command settings are set with",
                civ::hex(other)
            )));
        }
    };
    Ok([command.as_slice(), bytes].concat())
}

impl<P: Port> Radio for CivRadio<P> {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn exposes(&mut self) -> Option<&mut dyn Exposes> {
        Some(self)
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
    use super::{Ask, Change, CivRadio, Port, PretendPort, transmit_payload};
    use crate::civ::Frame;
    use crate::civ_pretend::PretendCiv;
    use crate::rig::{ControlPath, Keyable, Mode, Radio, Tunable, Tuning, TxMeters, Vfo};
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
            assert_eq!(radio.baud(), speed, "and says so");
        }
    }

    #[test]
    fn tells_an_ic_9700_from_an_ic_7300_by_its_address() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        for model in ["IC-7300", "IC-9700"] {
            let definition = definitions
                .iter()
                .find(|d| d.model == model)
                .unwrap_or_else(|| unreachable!("{model} is built in"));
            let port = PretendPort::new(PretendCiv::new(definition), 115_200);
            let mut radio =
                CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
            assert_eq!(radio.identity().model, model);
            let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
            assert_eq!(reading.mode, Some(Mode::DataUsb), "{model}");
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
        let reads: [&[u8]; 8] = [
            &[0x19, 0x00],
            &[0x03],
            &[0x04],
            &[0x1A, 0x06],
            &[0x1C, 0x00],
            &[0x15, 0x11],
            &[0x15, 0x12],
            &[0x15, 0x13],
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
    fn no_change_can_key_the_transmitter() {
        let bcd = crate::civ::frequency_to_bcd(7_074_000).unwrap_or_default();
        let changes = [
            Change::Frequency(bcd),
            Change::OtherFrequency(bcd),
            Change::Mode(0x01, 2),
            Change::DataMode(true, 2),
            Change::DataMode(false, 2),
            Change::Select(Vfo::A),
            Change::Select(Vfo::B),
            Change::Split(true),
            Change::Split(false),
        ];
        for change in changes {
            assert_ne!(
                change.payload().first(),
                Some(&0x1C),
                "{change:?} is a transmit command"
            );
        }
        // Every kind of tuning, on the pretend radio: none of it transmits.
        let mut radio = found(115_200);
        for tuning in [
            Tuning::Frequency(7_074_000),
            Tuning::OtherFrequency(7_076_000),
            Tuning::Mode(Mode::Cw),
            Tuning::Mode(Mode::DataUsb),
            Tuning::Select(Vfo::B),
            Tuning::Select(Vfo::A),
            Tuning::Split(true),
            Tuning::Split(false),
        ] {
            assert!(radio.tune(tuning).is_ok(), "{tuning:?}");
            assert!(!radio.port.radio().transmitting(), "{tuning:?} keyed it");
        }
    }

    #[test]
    fn keys_and_unkeys_only_when_asked_and_the_radio_reports_it() {
        assert_eq!(transmit_payload(true), [0x1C, 0x00, 0x01]);
        assert_eq!(transmit_payload(false), [0x1C, 0x00, 0x00]);
        let mut radio = found(115_200);
        assert!(radio.transmit(true).is_ok());
        assert!(radio.port.radio().transmitting());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.ptt, Some(true), "the radio says so");
        assert!(radio.transmit(false).is_ok());
        assert!(!radio.port.radio().transmitting());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.ptt, Some(false));
    }

    #[test]
    fn reads_the_transmit_meters_on_the_radio_s_scales() {
        let mut radio = found(115_200);
        let idle = radio
            .meters()
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            idle,
            Some(TxMeters {
                power: 0,
                swr_tenths: 10,
                alc: Some(0),
                ..TxMeters::default()
            }),
            "receiving: every meter at the bottom"
        );
        radio.port.radio().set_tx_meters([213, 80, 40]);
        assert!(radio.transmit(true).is_ok());
        let keyed = radio
            .meters()
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            keyed,
            Some(TxMeters {
                power: 100,
                swr_tenths: 20,
                alc: Some(33),
                ..TxMeters::default()
            })
        );
        radio.port.radio().set_tuning(true);
        let tuning = radio
            .meters()
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(
            tuning.is_some_and(|meters| meters.tuning),
            "the tuner's carrier, said so"
        );
        assert!(radio.transmit(false).is_ok());
    }

    #[test]
    fn tunes_and_the_radio_reports_it() {
        let mut radio = found(115_200);
        assert!(radio.tune(Tuning::Frequency(7_074_000)).is_ok());
        assert!(radio.tune(Tuning::Mode(Mode::Lsb)).is_ok());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 7_074_000);
        assert_eq!(
            reading.mode,
            Some(Mode::Lsb),
            "data mode was turned off too"
        );
        assert!(radio.tune(Tuning::Mode(Mode::DataUsb)).is_ok());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.mode, Some(Mode::DataUsb));
    }

    #[test]
    fn a_mode_change_keeps_the_selected_filter() {
        let mut radio = found(115_200);
        assert_eq!(
            Change::Mode(0x01, 2).payload(),
            [0x06, 0x01, 0x02],
            "06 with the filter"
        );
        assert_eq!(
            Change::DataMode(true, 2).payload(),
            [0x1A, 0x06, 0x01, 0x02]
        );
        assert_eq!(
            Change::DataMode(false, 2).payload(),
            [0x1A, 0x06, 0x00, 0x00]
        );
        // The operator picks FIL2; a program then asks for LSB, then data USB.
        assert!(radio.command(&[0x06, 0x01, 0x02]).is_ok());
        assert!(radio.tune(Tuning::Mode(Mode::Lsb)).is_ok());
        assert_eq!(radio.ask(Ask::Mode).ok(), Some(vec![0x04, 0x00, 0x02]));
        assert!(radio.tune(Tuning::Mode(Mode::DataUsb)).is_ok());
        assert_eq!(radio.ask(Ask::Mode).ok(), Some(vec![0x04, 0x01, 0x02]));
        assert_eq!(
            radio.ask(Ask::DataMode).ok(),
            Some(vec![0x1A, 0x06, 0x01, 0x02]),
            "still FIL2"
        );
    }

    #[test]
    fn says_when_the_radio_refuses_a_change() {
        let mut radio = found(115_200);
        assert!(
            radio.tune(Tuning::Frequency(80_000_000)).is_err(),
            "beyond the IC-7300"
        );
        assert!(radio.tune(Tuning::Mode(Mode::Other)).is_err());
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 14_074_000, "nothing changed");
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
