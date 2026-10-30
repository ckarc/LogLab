// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A pretend Yaesu radio that speaks CAT as the FTDX10's CAT manual
//! describes, so the CAT driver is built and tested without a radio on the
//! bench. It answers what its rig definition lists, sets what it is sent,
//! and answers `?;` to anything else.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::civ_radio::Port;
use crate::rigdef::Definition;

/// Where VFO-A starts: FT8 on 20 m.
const START_A_HZ: u64 = 14_074_000;
/// Where VFO-B starts.
const START_B_HZ: u64 = 14_076_000;
/// What the meters read while transmitting: ALC, Po, SWR (0 to 255).
const TX_METERS: [(u8, u16); 3] = [(4, 40), (5, 147), (6, 26)];

/// The pretend radio.
pub struct PretendCat {
    id: String,
    frequency_a: u64,
    frequency_b: u64,
    mode: u8,
    modes: Vec<u8>,
    transmitting: bool,
    /// Whether it ignores `TX0;`, as a radio keyed from its own microphone
    /// would stay on the air.
    stuck: bool,
    /// Settings, by the command that reads them, with their values as sent.
    settings: BTreeMap<String, String>,
    /// How many set commands it took.
    sets: usize,
    /// What it has to send back.
    out: Vec<u8>,
    /// What arrived but isn't a whole command yet.
    waiting: Vec<u8>,
}

impl PretendCat {
    /// A pretend radio of the model `definition` describes, on DATA-USB.
    #[must_use]
    pub fn new(definition: &Definition) -> Self {
        let mode = definition
            .mode_codes
            .get("DATA-USB")
            .copied()
            .unwrap_or(b'C');
        let settings = definition
            .settings
            .iter()
            .map(|setting| {
                let command = String::from_utf8_lossy(&setting.read).into_owned();
                // A level starts at its middle (as Yaesu's 0 to 100 levels
                // often do, at 50), anything with digits at its lowest
                // value, a raw value as one character.
                let value = match (setting.digits, setting.range, &setting.values) {
                    (Some(digits), Some([low, high]), _) => {
                        format!(
                            "{:0width$}",
                            u16::midpoint(u16::from(low), u16::from(high)),
                            width = usize::from(digits)
                        )
                    }
                    (Some(digits), None, Some(values)) => format!(
                        "{:0width$}",
                        values
                            .values()
                            .min()
                            .copied()
                            .map(from_bcd)
                            .unwrap_or_default(),
                        width = usize::from(digits)
                    ),
                    (Some(digits), _, _) => "0".repeat(usize::from(digits)),
                    (None, _, _) if command == "MD0" => char::from(mode).to_string(),
                    (None, _, _) => "0".to_owned(),
                };
                (command, value)
            })
            .collect();
        Self {
            id: definition.identity_answer.clone().unwrap_or_default(),
            frequency_a: START_A_HZ,
            frequency_b: START_B_HZ,
            mode,
            modes: definition.mode_codes.values().copied().collect(),
            transmitting: false,
            stuck: false,
            settings,
            sets: 0,
            out: Vec::new(),
            waiting: Vec::new(),
        }
    }

    #[cfg(test)]
    /// Whether it is transmitting.
    #[must_use]
    pub fn transmitting(&self) -> bool {
        self.transmitting
    }

    #[cfg(test)]
    /// VFO-B's frequency.
    #[must_use]
    pub fn frequency_b(&self) -> u64 {
        self.frequency_b
    }

    #[cfg(test)]
    /// How many set commands it took.
    #[must_use]
    pub fn sets(&self) -> usize {
        self.sets
    }

    #[cfg(test)]
    /// From now on it stays on the air when told `TX0;`.
    pub fn stick_transmit(&mut self) {
        self.stuck = true;
    }

    /// Takes bytes from the computer.
    pub fn receive(&mut self, bytes: &[u8]) {
        self.waiting.extend_from_slice(bytes);
        while let Some(end) = self.waiting.iter().position(|b| *b == b';') {
            let command: Vec<u8> = self.waiting.drain(..=end).take(end).collect();
            let command = String::from_utf8_lossy(&command).into_owned();
            let answer = self.answer(&command);
            self.out.extend_from_slice(answer.as_bytes());
        }
    }

    /// What it sends back, once.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn answer(&mut self, command: &str) -> String {
        let refuse = || "?;".to_owned();
        match command {
            "ID" => format!("ID{};", self.id),
            "FA" => format!("FA{:09};", self.frequency_a),
            "FB" => format!("FB{:09};", self.frequency_b),
            "MD0" => format!("MD0{};", char::from(self.mode)),
            "TX" => format!("TX{};", u8::from(self.transmitting)),
            "TX0" => {
                self.sets += 1;
                if !self.stuck {
                    self.transmitting = false;
                }
                String::new()
            }
            "TX1" => {
                self.sets += 1;
                self.transmitting = true;
                String::new()
            }
            "VS0" | "VS1" | "ST0" | "ST1" => {
                self.sets += 1;
                String::new()
            }
            _ if command.starts_with("RM") => {
                let which = command[2..].parse::<u8>().ok();
                match TX_METERS.iter().find(|(meter, _)| Some(*meter) == which) {
                    Some((meter, level)) => {
                        let level = if self.transmitting { *level } else { 0 };
                        format!("RM{meter}{level:03}000;")
                    }
                    None => refuse(),
                }
            }
            _ => self.set_or_read(command).unwrap_or_else(refuse),
        }
    }

    fn set_or_read(&mut self, command: &str) -> Option<String> {
        if let Some(value) = self.settings.get(command) {
            return Some(format!("{command}{value};"));
        }
        for letter in ['A', 'B'] {
            if let Some(digits) = command.strip_prefix(&format!("F{letter}"))
                && digits.len() == 9
            {
                let hz = digits.parse().ok()?;
                self.sets += 1;
                if letter == 'A' {
                    self.frequency_a = hz;
                } else {
                    self.frequency_b = hz;
                }
                return Some(String::new());
            }
        }
        if let Some(&[code]) = command.strip_prefix("MD0").map(str::as_bytes)
            && self.modes.contains(&code)
        {
            self.sets += 1;
            self.mode = code;
            return Some(String::new());
        }
        let (read, value) = self
            .settings
            .iter()
            .find(|(read, value)| {
                command.len() == read.len() + value.len() && command.starts_with(read.as_str())
            })
            .map(|(read, _)| (read.clone(), command[read.len()..].to_owned()))?;
        self.sets += 1;
        self.settings.insert(read, value);
        Some(String::new())
    }
}

/// A one- or two-digit BCD byte's number.
fn from_bcd(byte: u8) -> u16 {
    u16::from(byte >> 4) * 10 + u16::from(byte & 0x0F)
}

/// The pretend radio, reached as if through a serial port. It only listens
/// at its own speed: at any other, it sends back noise.
pub struct PretendCatPort {
    radio: PretendCat,
    speed: u32,
    baud: u32,
    waiting: Vec<u8>,
}

impl PretendCatPort {
    /// The pretend radio, listening at `speed`.
    #[must_use]
    pub fn new(radio: PretendCat, speed: u32) -> Self {
        Self {
            radio,
            speed,
            baud: 0,
            waiting: Vec::new(),
        }
    }

    #[cfg(test)]
    /// The radio behind the port.
    pub fn radio(&mut self) -> &mut PretendCat {
        &mut self.radio
    }

    /// Bytes the radio sends on its own, such as an Auto Information
    /// report, ahead of anything it answers.
    #[cfg(test)]
    pub fn arrive(&mut self, bytes: &[u8]) {
        self.waiting.extend_from_slice(bytes);
    }
}

impl Port for PretendCatPort {
    fn set_baud(&mut self, baud: u32) -> Result<(), String> {
        self.baud = baud;
        self.waiting.clear();
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.baud == self.speed {
            self.radio.receive(bytes);
            let answer = self.radio.take();
            self.waiting.extend_from_slice(&answer);
        } else {
            // What a wrong speed sounds like.
            self.waiting.extend_from_slice(&[0xF0, 0x0F, 0xFF]);
        }
        Ok(())
    }

    fn receive(&mut self, _wait: Duration) -> Result<Vec<u8>, String> {
        Ok(std::mem::take(&mut self.waiting))
    }
}
