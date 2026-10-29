// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Agent's own driver for Yaesu radios over CAT, such as the FTDX10. It
//! reads and tunes, and keys the transmitter only through [`Keyable`],
//! which only the transmit path uses.
//!
//! Yaesu CAT is text: a command of two capital letters, its parameters, and a
//! semicolon (`FA014074000;`). A command sent with no parameters asks, and
//! the radio answers in the same shape; a command sent with them sets, and
//! the radio says nothing (FTDX10 CAT operation reference manual, "Command
//! table"). A radio that can't do what it was sent answers `?;`, which the
//! driver takes as a refusal (Hamlib's rigs/yaesu/newcat.c treats it so; the
//! FTDX10 manual doesn't say).
//!
//! Given a [`Port`] and the rig definitions the Agent knows, the driver:
//!
//! 1. tries each speed the Yaesu definitions list, and at each asks `ID;`;
//! 2. takes the radio whose answer matches a definition's
//!    `identity.answer` (`ID0761;` is an FTDX10);
//! 3. reads it by asking: frequency (`FA;`), mode (`MD0;`) and transmit
//!    status (`TX;`).
//!
//! While it waits for an answer it skips anything else the radio sends,
//! such as the reports Auto Information (`AI1;`) makes it send on its own.
//!
//! [`Ask`] is the whole list of questions the driver asks besides settings,
//! and none of it changes the radio. The one transmit command is
//! [`transmit_command`], kept apart.
//!
//! A setting's value is kept as an Icom radio's would be: a value of digits
//! (`RG0255;`) becomes BCD bytes (`02 55`), so ranges, named values, the
//! records and the lab's profiles mean the same for both families. A raw
//! setting with no digits count is kept as the characters the radio sent.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::civ::hex;
use crate::civ_radio::{Port, WriteBack};
use crate::rig::{
    ControlPath, Exposes, Keyable, Mode, Radio, RadioError, RadioIdentity, Reading, Tunable,
    Tuning, TxMeters, Vfo,
};
use crate::rigdef::{self, Definition, SettingSummary};

/// How long the driver waits for one answer. A Yaesu radio at 4800 baud
/// takes about 25 ms to send a frequency.
const ANSWER_WAIT: Duration = Duration::from_millis(400);
/// How long the driver listens for a refusal (`?;`) after setting
/// something, which the radio otherwise doesn't answer.
const REFUSAL_WAIT: Duration = Duration::from_millis(80);
/// How long it listens for anything left over before it sends, each time.
const FLUSH_LISTEN: Duration = Duration::from_millis(1);
/// The most times it does, so a radio that never stops talking can't hold
/// it.
const FLUSH_READS: usize = 8;
/// How long it listens in each slice of a wait.
const LISTEN: Duration = Duration::from_millis(20);
/// The most messages it skips while waiting for one answer.
const MAX_SKIPPED: usize = 32;
/// The longest message the driver takes from a radio.
const MAX_MESSAGE: usize = 64;
/// The most characters one setting's value may have.
const MAX_SETTING_CHARS: usize = 32;
/// The highest frequency nine digits hold (`FA` takes nine).
const MAX_CAT_HZ: u64 = 999_999_999;

/// Everything the driver asks a radio, besides settings. All of it only
/// asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// "Who are you?" (`ID`).
    Identity,
    /// VFO-A's frequency (`FA`).
    Frequency,
    /// The main band's mode (`MD0`).
    Mode,
    /// Whether the radio is transmitting (`TX`).
    Transmitting,
    /// A transmit meter (`RM` and which: 4 ALC, 5 Po, 6 SWR).
    Meter(u8),
}

impl Ask {
    /// Every question there is, for the test that none of them changes a
    /// radio.
    #[cfg(test)]
    const ALL: [Self; 7] = [
        Self::Identity,
        Self::Frequency,
        Self::Mode,
        Self::Transmitting,
        Self::Meter(ALC_METER),
        Self::Meter(POWER_METER),
        Self::Meter(SWR_METER),
    ];

    /// The command, without its semicolon. The answer starts with it.
    #[must_use]
    pub fn command(self) -> String {
        match self {
            Self::Identity => "ID".to_owned(),
            Self::Frequency => "FA".to_owned(),
            Self::Mode => "MD0".to_owned(),
            Self::Transmitting => "TX".to_owned(),
            Self::Meter(which) => format!("RM{which}"),
        }
    }
}

/// The ALC meter (`RM4`).
const ALC_METER: u8 = 4;
/// The Po meter (`RM5`).
const POWER_METER: u8 = 5;
/// The SWR meter (`RM6`).
const SWR_METER: u8 = 6;

/// The command that keys the radio (`TX1`, CAT TX on) or unkeys it
/// (`TX0`): the only one that can key it, kept apart from everything else
/// the driver sends (Principle XI).
#[must_use]
pub fn transmit_command(on: bool) -> &'static str {
    if on { "TX1" } else { "TX0" }
}

/// A Yaesu radio, found on a port.
pub struct CatRadio<P: Port> {
    port: P,
    definition: Definition,
    identity: RadioIdentity,
    /// Mode names by the character the radio sends for them.
    modes: BTreeMap<u8, String>,
    /// What arrived but isn't a whole message yet.
    waiting: Vec<u8>,
    baud: u32,
}

impl<P: Port> CatRadio<P> {
    /// Finds which known Yaesu radio is on `port`, and at what speed.
    ///
    /// # Errors
    ///
    /// No known radio answered at any speed.
    pub fn find(port: P, definitions: &[Definition]) -> Result<Self, RadioError> {
        let yaesu: Vec<&Definition> = definitions
            .iter()
            .filter(|d| d.family == rigdef::YAESU_CAT)
            .collect();
        let mut speeds: Vec<u32> = Vec::new();
        for definition in &yaesu {
            for baud in &definition.baud_rates {
                if !speeds.contains(baud) {
                    speeds.push(*baud);
                }
            }
        }
        let Some(first) = yaesu.first() else {
            return Err(RadioError(
                "no Yaesu radio's definition is built in".to_owned(),
            ));
        };
        let mut radio = Self::new(port, first);
        for baud in &speeds {
            radio.port.set_baud(*baud).map_err(RadioError)?;
            radio.waiting.clear();
            radio.baud = *baud;
            let Ok(answer) = radio.ask(Ask::Identity) else {
                continue;
            };
            if let Some(definition) = yaesu
                .iter()
                .find(|d| d.identity_answer.as_deref() == Some(answer.as_str()))
            {
                let mut found = Self::new(radio.port, definition);
                found.baud = *baud;
                return Ok(found);
            }
        }
        let names: Vec<String> = yaesu
            .iter()
            .map(|d| {
                format!(
                    "{} {} (ID{})",
                    d.maker,
                    d.model,
                    d.identity_answer.as_deref().unwrap_or("?")
                )
            })
            .collect();
        Err(RadioError(format!(
            "no Yaesu radio answered at any speed ({}); looked for {}",
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
                path: ControlPath::Cat,
            },
            modes: definition
                .mode_codes
                .iter()
                .map(|(name, code)| (*code, name.clone()))
                .collect(),
            waiting: Vec::new(),
            baud: 0,
        }
    }

    /// The speed the radio answered at.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// The port, to reach a pretend radio behind it in tests.
    #[cfg(test)]
    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    fn ask(&mut self, ask: Ask) -> Result<String, RadioError> {
        self.exchange(&ask.command())
    }

    /// Sends `command;` and returns what the answer has after the command.
    fn exchange(&mut self, command: &str) -> Result<String, RadioError> {
        self.flush()?;
        self.port
            .send(format!("{command};").as_bytes())
            .map_err(RadioError)?;
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut skipped = 0;
        while Instant::now() < deadline {
            for message in self.messages()? {
                if message == "?" {
                    return Err(RadioError(format!("the radio refused {command} (?;)")));
                }
                if let Some(rest) = message.strip_prefix(command) {
                    return Ok(rest.to_owned());
                }
                // A report the radio sent on its own, or noise.
                skipped += 1;
                if skipped > MAX_SKIPPED {
                    return Err(RadioError(
                        "too many messages that aren't the answer".to_owned(),
                    ));
                }
            }
        }
        Err(RadioError(format!(
            "no answer to {command}; within {} ms",
            ANSWER_WAIT.as_millis()
        )))
    }

    /// Sends a command that sets something, which the radio doesn't answer
    /// unless it refuses.
    fn set(&mut self, command: &str) -> Result<(), RadioError> {
        self.flush()?;
        self.port
            .send(format!("{command};").as_bytes())
            .map_err(RadioError)?;
        let deadline = Instant::now() + REFUSAL_WAIT;
        while Instant::now() < deadline {
            if self.messages()?.iter().any(|message| message == "?") {
                return Err(RadioError(format!("the radio refused {command};")));
            }
        }
        Ok(())
    }

    /// Drops whatever the radio sent before the next command, such as an
    /// Auto Information report, so it isn't taken for the answer.
    fn flush(&mut self) -> Result<(), RadioError> {
        for _ in 0..FLUSH_READS {
            if self
                .port
                .receive(FLUSH_LISTEN)
                .map_err(RadioError)?
                .is_empty()
            {
                break;
            }
        }
        self.waiting.clear();
        Ok(())
    }

    /// The whole messages that arrive within one listening slice, without
    /// their semicolons. Anything that isn't printable text is noise (a
    /// wrong speed): it is dropped.
    fn messages(&mut self) -> Result<Vec<String>, RadioError> {
        let bytes = self.port.receive(LISTEN).map_err(RadioError)?;
        self.waiting.extend_from_slice(&bytes);
        let mut out = Vec::new();
        while let Some(end) = self.waiting.iter().position(|b| *b == b';') {
            let message: Vec<u8> = self.waiting.drain(..=end).take(end).collect();
            if message.len() <= MAX_MESSAGE && message.iter().all(|b| (b' '..=b'~').contains(b)) {
                out.push(String::from_utf8_lossy(&message).into_owned());
            }
        }
        if self.waiting.len() > MAX_MESSAGE {
            self.waiting.clear();
        }
        Ok(out)
    }

    fn mode_named(&self, code: u8) -> Mode {
        match self.modes.get(&code).map(String::as_str) {
            Some("USB") => Mode::Usb,
            Some("LSB") => Mode::Lsb,
            Some("CW") => Mode::Cw,
            Some("CW-R") => Mode::CwReverse,
            Some("RTTY") => Mode::Rtty,
            Some("RTTY-R") => Mode::RttyReverse,
            Some("AM") => Mode::Am,
            Some("FM") => Mode::Fm,
            Some("DATA-USB") => Mode::DataUsb,
            Some("DATA-LSB") => Mode::DataLsb,
            Some("DATA-FM") => Mode::DataFm,
            _ => Mode::Other,
        }
    }

    fn mode_code(&self, mode: Mode) -> Result<u8, RadioError> {
        let name = match mode {
            Mode::Usb => "USB",
            Mode::Lsb => "LSB",
            Mode::Cw => "CW",
            Mode::CwReverse => "CW-R",
            Mode::Rtty => "RTTY",
            Mode::RttyReverse => "RTTY-R",
            Mode::Am => "AM",
            Mode::Fm => "FM",
            Mode::DataUsb => "DATA-USB",
            Mode::DataLsb => "DATA-LSB",
            Mode::DataFm => "DATA-FM",
            Mode::Other => "",
        };
        self.definition
            .mode_codes
            .get(name)
            .copied()
            .ok_or_else(|| RadioError(format!("the {} has no {mode:?} mode", self.identity.model)))
    }

    fn frequency_command(letter: char, hz: u64) -> Result<String, RadioError> {
        if !(1..=MAX_CAT_HZ).contains(&hz) {
            return Err(RadioError(format!(
                "{hz} Hz is not a frequency CAT can set"
            )));
        }
        Ok(format!("F{letter}{hz:09}"))
    }

    /// Reads one setting. Returns its value as the Agent keeps it: BCD bytes
    /// for a value of digits, the characters themselves for a raw one.
    ///
    /// # Errors
    ///
    /// The radio refused, didn't answer, or answered with something that
    /// isn't a value for it.
    pub fn read_one(&mut self, setting: &SettingSummary) -> Result<Vec<u8>, RadioError> {
        let command = String::from_utf8_lossy(&setting.read).into_owned();
        let value = self.exchange(&command)?;
        from_wire(setting, &value)
    }

    /// Reads one setting, writes exactly what it just read back to it, and
    /// reads it again: a test of the command that sets it which changes
    /// nothing on the radio (Principle X). Only for a setting the Agent may
    /// ever change.
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
        let written = set_command(setting, &before).and_then(|command| self.set(&command));
        let after = self.read_one(setting);
        Ok(WriteBack {
            before,
            written: written.map_err(|error| error.to_string()),
            after: after.map_err(|error| error.to_string()),
        })
    }
}

/// A setting's value as the radio sent it, as the Agent keeps it.
fn from_wire(setting: &SettingSummary, value: &str) -> Result<Vec<u8>, RadioError> {
    let odd = || RadioError(format!("{value:?} is not a value for {}", setting.label));
    if value.is_empty() || value.len() > MAX_SETTING_CHARS {
        return Err(odd());
    }
    let Some(digits) = setting.digits else {
        return Ok(value.as_bytes().to_vec());
    };
    if value.len() != usize::from(digits) || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(odd());
    }
    let padded = if value.len() % 2 == 1 {
        format!("0{value}")
    } else {
        value.to_owned()
    };
    Ok(padded
        .as_bytes()
        .chunks(2)
        .map(|pair| ((pair[0] - b'0') << 4) | (pair[1] - b'0'))
        .collect())
}

/// The characters the radio takes for a setting's value, from the value as
/// the Agent keeps it.
fn to_wire(setting: &SettingSummary, bytes: &[u8]) -> Result<String, RadioError> {
    let odd = || {
        RadioError(format!(
            "{} is not a value for {}",
            hex(bytes),
            setting.label
        ))
    };
    if bytes.is_empty() || bytes.len() > MAX_SETTING_CHARS {
        return Err(odd());
    }
    let Some(digits) = setting.digits else {
        // A raw value goes back as the characters the radio sent: printable,
        // and never a semicolon, which would end the command early.
        return if bytes
            .iter()
            .all(|b| (b' '..=b'~').contains(b) && *b != b';')
        {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        } else {
            Err(odd())
        };
    };
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let (high, low) = (byte >> 4, byte & 0x0F);
        if high > 9 || low > 9 {
            return Err(odd());
        }
        text.push(char::from(b'0' + high));
        text.push(char::from(b'0' + low));
    }
    let digits = usize::from(digits);
    let extra = text.len().checked_sub(digits).ok_or_else(odd)?;
    if !text[..extra].bytes().all(|b| b == b'0') {
        return Err(odd());
    }
    Ok(text[extra..].to_owned())
}

/// The command that sets `setting` to `bytes`: the command that reads it,
/// with the value after it (`RG0` and `255`: `RG0255`).
fn set_command(setting: &SettingSummary, bytes: &[u8]) -> Result<String, RadioError> {
    let read = String::from_utf8_lossy(&setting.read).into_owned();
    Ok(format!("{read}{}", to_wire(setting, bytes)?))
}

impl<P: Port> Radio for CatRadio<P> {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn exposes(&mut self) -> Option<&mut dyn Exposes> {
        Some(self)
    }

    fn read(&mut self) -> Result<Reading, RadioError> {
        let frequency = self.ask(Ask::Frequency)?;
        let frequency_hz = frequency
            .parse::<u64>()
            .ok()
            .filter(|hz| frequency.len() == 9 && (1..=MAX_CAT_HZ).contains(hz))
            .ok_or_else(|| RadioError(format!("not a frequency: FA{frequency}")))?;
        let mode = match self.ask(Ask::Mode)?.as_bytes() {
            [code] => self.mode_named(*code),
            other => {
                return Err(RadioError(format!(
                    "odd answer to MD0: {}",
                    String::from_utf8_lossy(other)
                )));
            }
        };
        // TX answers 0 (receiving), 1 (CAT keyed it) or 2 (keyed from the
        // radio itself, by its microphone, say).
        let ptt = match self.ask(Ask::Transmitting).as_deref() {
            Ok("0") => Some(false),
            Ok("1" | "2") => Some(true),
            _ => None,
        };
        Ok(Reading {
            frequency_hz,
            mode: Some(mode),
            passband_hz: None,
            ptt,
            vfo: None,
        })
    }
}

impl<P: Port> Tunable for CatRadio<P> {
    fn tune(&mut self, tuning: Tuning) -> Result<(), RadioError> {
        let command = match tuning {
            Tuning::Frequency(hz) => Self::frequency_command('A', hz)?,
            Tuning::OtherFrequency(hz) => Self::frequency_command('B', hz)?,
            Tuning::Mode(mode) => format!("MD0{}", char::from(self.mode_code(mode)?)),
            Tuning::Select(Vfo::A) => "VS0".to_owned(),
            Tuning::Select(Vfo::B) => "VS1".to_owned(),
            Tuning::Split(on) => format!("ST{}", u8::from(on)),
        };
        self.set(&command)
    }
}

impl<P: Port> Keyable for CatRadio<P> {
    /// Keys or unkeys, then asks the radio whether it is transmitting: the
    /// radio doesn't answer a set command, so only its own answer says the
    /// change took (Principle X).
    fn transmit(&mut self, on: bool) -> Result<(), RadioError> {
        self.set(transmit_command(on))?;
        match self.ask(Ask::Transmitting)?.as_str() {
            "1" | "2" if on => Ok(()),
            "0" if !on => Ok(()),
            other => Err(RadioError(format!(
                "sent {}; but the radio answers TX{other}",
                transmit_command(on)
            ))),
        }
    }

    fn meters(&mut self) -> Result<Option<TxMeters>, RadioError> {
        let Some(meters) = self.definition.meters.clone() else {
            return Ok(None);
        };
        let mut level = |which: u8| -> Result<u16, RadioError> {
            // RM answers the meter, three digits 000 to 255, then 000.
            let answer = self.ask(Ask::Meter(which))?;
            answer
                .get(..3)
                .and_then(|digits| digits.parse::<u16>().ok())
                .filter(|level| *level <= 255)
                .ok_or_else(|| RadioError(format!("not a meter level: RM{which}{answer}")))
        };
        let power = meters.power.value(level(POWER_METER)?);
        let swr_tenths = meters.swr.value(level(SWR_METER)?);
        let alc = meters.alc.value(level(ALC_METER)?);
        Ok(Some(TxMeters {
            power,
            swr_tenths,
            alc,
            tuning: false,
            readings: 0,
        }))
    }
}

impl<P: Port> Exposes for CatRadio<P> {
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
        self.set(&command)
    }
}

#[cfg(test)]
mod tests {
    use super::{Ask, CatRadio, from_wire, to_wire, transmit_command};
    use crate::cat_pretend::{PretendCat, PretendCatPort};
    use crate::rig::{ControlPath, Exposes, Keyable, Mode, Radio, Tunable, Tuning};
    use crate::rigdef::{self, Definition};

    fn ftdx10() -> Definition {
        rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "FTDX10"))
            .unwrap_or_else(|| unreachable!("the FTDX10 is built in"))
    }

    fn found(speed: u32) -> CatRadio<PretendCatPort> {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendCatPort::new(PretendCat::new(&ftdx10()), speed);
        CatRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn finds_the_radio_at_any_listed_speed() {
        for speed in ftdx10().baud_rates {
            let radio = found(speed);
            assert_eq!(radio.identity().model, "FTDX10");
            assert_eq!(radio.identity().path, ControlPath::Cat);
            assert_eq!(radio.baud(), speed);
        }
    }

    #[test]
    fn says_when_no_radio_answers() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendCatPort::new(PretendCat::new(&ftdx10()), 1200);
        let error = CatRadio::find(port, &definitions)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("no Yaesu radio answered"), "{error}");
        assert!(error.contains("FTDX10 (ID0761)"), "{error}");
    }

    #[test]
    fn reads_frequency_mode_and_transmit_status() {
        let mut radio = found(38_400);
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 14_074_000);
        assert_eq!(reading.mode, Some(Mode::DataUsb));
        assert_eq!(reading.ptt, Some(false));
    }

    #[test]
    fn skips_what_the_radio_reports_on_its_own() {
        let mut radio = found(38_400);
        radio.port_mut().arrive(b"FA007030000;MD03;");
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            reading.frequency_hz, 14_074_000,
            "the answer, not the report"
        );
    }

    #[test]
    fn tunes_and_reads_back() {
        let mut radio = found(38_400);
        for tuning in [
            Tuning::Frequency(7_074_000),
            Tuning::Mode(Mode::Cw),
            Tuning::OtherFrequency(7_080_000),
        ] {
            radio
                .tune(tuning)
                .unwrap_or_else(|error| unreachable!("{error}"));
        }
        let reading = radio.read().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(reading.frequency_hz, 7_074_000);
        assert_eq!(reading.mode, Some(Mode::Cw));
        assert_eq!(radio.port_mut().radio().frequency_b(), 7_080_000);
        assert!(radio.tune(Tuning::Frequency(1_000_000_000)).is_err());
    }

    #[test]
    fn only_asks_questions() {
        // A Yaesu command with nothing after it asks; every question here
        // is one, and none is TX with a value.
        for ask in Ask::ALL {
            let command = ask.command();
            assert!(
                !command.starts_with("TX") || command == "TX",
                "{command} must not key the radio"
            );
            assert!(!command.contains(';'));
        }
        let mut radio = found(38_400);
        let _ = radio.read();
        assert!(!radio.port_mut().radio().transmitting());
        assert_eq!(radio.port_mut().radio().sets(), 0, "reading set nothing");
    }

    #[test]
    fn keys_and_unkeys_only_when_asked_and_the_radio_reports_it() {
        assert_eq!(transmit_command(true), "TX1");
        assert_eq!(transmit_command(false), "TX0");
        let mut radio = found(38_400);
        radio
            .transmit(true)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(radio.port_mut().radio().transmitting());
        let meters = radio
            .meters()
            .unwrap_or_else(|error| unreachable!("{error}"))
            .unwrap_or_else(|| unreachable!("the FTDX10 has meters"));
        assert_eq!(meters.power, 50, "level 147 is 50 W of 100");
        radio
            .transmit(false)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(!radio.port_mut().radio().transmitting());
        // A radio that won't unkey is an error, not a success.
        radio.port_mut().radio().stick_transmit();
        radio
            .transmit(true)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(radio.transmit(false).is_err());
    }

    #[test]
    fn keeps_values_as_bcd_and_puts_them_back_as_digits() {
        let definition = ftdx10();
        let setting = |id: &str| {
            definition
                .settings
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .unwrap_or_else(|| unreachable!("{id}"))
        };
        let rf_gain = setting("rf_gain");
        assert_eq!(from_wire(&rf_gain, "255").ok(), Some(vec![0x02, 0x55]));
        assert_eq!(
            to_wire(&rf_gain, &[0x02, 0x55]).ok().as_deref(),
            Some("255")
        );
        assert!(from_wire(&rf_gain, "25").is_err(), "three digits");
        assert!(to_wire(&rf_gain, &[0x12, 0x55]).is_err(), "1255 has four");
        assert!(to_wire(&rf_gain, &[0x0A, 0x00]).is_err(), "not BCD");
        let rear = setting("data_rear_select");
        assert_eq!(from_wire(&rear, "1").ok(), Some(vec![0x01]));
        assert_eq!(to_wire(&rear, &[0x01]).ok().as_deref(), Some("1"));
        let mode = setting("mode");
        assert_eq!(from_wire(&mode, "C").ok(), Some(vec![b'C']));
        assert!(
            to_wire(&mode, b"C;TX1").is_err(),
            "no command can ride along"
        );
    }

    #[test]
    fn reads_settings_and_writes_only_checked_ones() {
        let mut radio = found(38_400);
        let definition = radio.definition().clone();
        let level = definition
            .settings
            .iter()
            .find(|s| s.id == "data_rport_gain")
            .cloned()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(radio.read_exposed(&level).ok(), Some(vec![0x00, 0x50]));
        assert!(
            radio.write_exposed(&level, &[0x00, 0x40]).is_err(),
            "not checked on a real radio"
        );
        let mut checked = level.clone();
        checked.write_checked = true;
        assert!(radio.write_exposed(&checked, &[0x00, 0x40]).is_ok());
        assert_eq!(radio.read_exposed(&level).ok(), Some(vec![0x00, 0x40]));
        let back = radio
            .write_back(&level)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(back.works());
    }
}
