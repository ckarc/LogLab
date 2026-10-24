// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A pretend Icom radio that speaks CI-V.
//!
//! It takes the bytes a program would send a real IC-7300 down its USB cable
//! and answers with the bytes the radio would send back, so CI-V drivers can
//! be built and tested with no radio on the bench. Its menu is the one its
//! rig definition file lists ([`crate::rigdef`]), and only those settings
//! exist on it.
//!
//! The commands it answers are the Icom CI-V commands the IC-7300 definition
//! uses: frequency (03, 05, 25), mode (04, 06, 26), VFO (07), split (0F),
//! identity (19 00), menu settings (1A 05), data mode (1A 06) and transmit
//! status (1C 00). Anything else gets `FA`, as a real radio answers a command
//! it doesn't have. Like the definition, this follows Icom's published CI-V
//! command set and has not yet been compared against a real radio byte for
//! byte.
//!
//! It is not connected to anything and cannot transmit: "transmitting" is a
//! flag it reports back, nothing more.

use std::collections::{BTreeMap, BTreeSet};

use crate::civ::{self, BROADCAST, Decoder, Frame, NG, OK};
use crate::rigdef::Definition;

/// The lowest frequency it tunes to: 30 kHz.
pub const LOWEST_HZ: u64 = 30_000;
/// The highest frequency it tunes to: 74.8 MHz.
pub const HIGHEST_HZ: u64 = 74_800_000;
/// Where VFO A starts: the 20 m FT8 frequency.
const START_A_HZ: u64 = 14_074_000;
/// Where VFO B starts.
const START_B_HZ: u64 = 14_076_000;
/// The filter numbers a radio has: FIL1, FIL2 and FIL3.
const FILTERS: [u8; 3] = [1, 2, 3];

/// One VFO's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Vfo {
    hz: u64,
    mode: u8,
    filter: u8,
    data: bool,
}

/// What a menu setting can hold.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// One of these codes, sent as one byte.
    Named(BTreeSet<u8>),
    /// A level from low to high, sent as two BCD bytes.
    Level(u16, u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Menu {
    kind: Kind,
    value: u16,
}

/// The pretend radio.
pub struct PretendCiv {
    address: u8,
    modes: BTreeSet<u8>,
    vfos: [Vfo; 2],
    selected: usize,
    split: bool,
    transmitting: bool,
    menu: BTreeMap<[u8; 2], Menu>,
    /// The echo-back setting and its "ON" code, if the definition has one.
    echo: Option<([u8; 2], u8)>,
    /// The transceive setting and its "ON" code, if the definition has one.
    transceive: Option<([u8; 2], u8)>,
    decoder: Decoder,
}

impl PretendCiv {
    /// A pretend radio built from its definition, at its default address,
    /// with VFO A on 14.074 MHz in USB data mode. Each named setting starts
    /// on its lowest code and each level in the middle of its range; those
    /// are pretend values, not the radio's factory defaults. Transceive
    /// starts on and echo off, so changes on the front panel are reported.
    #[must_use]
    pub fn new(definition: &Definition) -> Self {
        let usb = definition.mode_codes.get("USB").copied().unwrap_or(0x01);
        let mut radio = Self {
            address: definition.default_address,
            modes: definition.mode_codes.values().copied().collect(),
            vfos: [
                Vfo {
                    hz: START_A_HZ,
                    mode: usb,
                    filter: 1,
                    data: true,
                },
                Vfo {
                    hz: START_B_HZ,
                    mode: usb,
                    filter: 1,
                    data: false,
                },
            ],
            selected: 0,
            split: false,
            transmitting: false,
            menu: BTreeMap::new(),
            echo: None,
            transceive: None,
            decoder: Decoder::default(),
        };
        for setting in &definition.settings {
            let menu = match (&setting.values, setting.range) {
                (Some(values), _) => {
                    let codes: BTreeSet<u8> = values.values().copied().collect();
                    let first = codes.first().copied().unwrap_or(0);
                    Menu {
                        kind: Kind::Named(codes),
                        value: u16::from(first),
                    }
                }
                (None, Some([low, high])) => Menu {
                    kind: Kind::Level(u16::from(low), u16::from(high)),
                    value: u16::midpoint(u16::from(low), u16::from(high)),
                },
                (None, None) => continue,
            };
            radio.menu.insert(setting.item, menu);
            let on = setting.values.as_ref().and_then(|v| v.get("ON")).copied();
            let off = setting.values.as_ref().and_then(|v| v.get("OFF")).copied();
            match (setting.id.as_str(), on) {
                ("civ_transceive", Some(on)) => {
                    radio.transceive = Some((setting.item, on));
                    radio.set_menu(setting.item, u16::from(on));
                }
                ("civ_usb_echo_back", Some(on)) => {
                    radio.echo = Some((setting.item, on));
                    if let Some(off) = off {
                        radio.set_menu(setting.item, u16::from(off));
                    }
                }
                _ => {}
            }
        }
        radio
    }

    /// Bytes arriving from the computer. Returns the bytes the radio sends
    /// back: an echo of each message first when echo-back is on, then its
    /// answer to each message addressed to it.
    pub fn receive(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for frame in self.decoder.push(bytes) {
            if self.is_on(self.echo) {
                out.extend(frame.encode());
            }
            if frame.to != self.address || frame.from == self.address {
                continue;
            }
            let answer = self.answer(&frame.payload);
            out.extend(Frame::new(frame.from, self.address, &answer).encode());
        }
        out
    }

    /// Someone turns the VFO knob to `hz`. Returns what the radio reports on
    /// its own: the new frequency, to everyone, when transceive is on.
    pub fn turn_dial(&mut self, hz: u64) -> Vec<u8> {
        let hz = hz.clamp(LOWEST_HZ, HIGHEST_HZ);
        self.vfos[self.selected].hz = hz;
        match (self.is_on(self.transceive), civ::frequency_to_bcd(hz)) {
            (true, Some(bcd)) => {
                let mut payload = vec![0x00];
                payload.extend(bcd);
                Frame::new(BROADCAST, self.address, &payload).encode()
            }
            _ => Vec::new(),
        }
    }

    /// Sets a menu setting directly, as if from the radio's own menu.
    /// Returns `false` if it has no such setting or the value doesn't fit.
    pub fn set_menu(&mut self, item: [u8; 2], value: u16) -> bool {
        let Some(menu) = self.menu.get_mut(&item) else {
            return false;
        };
        let fits = match &menu.kind {
            Kind::Named(codes) => u8::try_from(value).is_ok_and(|v| codes.contains(&v)),
            Kind::Level(low, high) => (*low..=*high).contains(&value),
        };
        if fits {
            menu.value = value;
        }
        fits
    }

    /// Whether it says it is transmitting.
    #[cfg(test)]
    #[must_use]
    pub fn transmitting(&self) -> bool {
        self.transmitting
    }

    fn is_on(&self, setting: Option<([u8; 2], u8)>) -> bool {
        setting.is_some_and(|(item, on)| {
            self.menu
                .get(&item)
                .is_some_and(|menu| menu.value == u16::from(on))
        })
    }

    /// The answer's payload: the asked-for state, `FB`, or `FA`.
    fn answer(&mut self, payload: &[u8]) -> Vec<u8> {
        let done = |ok: bool| vec![if ok { OK } else { NG }];
        match payload {
            [0x03] => self.frequency_answer(&[0x03], self.selected),
            [0x04] => {
                let vfo = self.vfos[self.selected];
                vec![0x04, vfo.mode, vfo.filter]
            }
            [0x05, bcd @ ..] => done(self.tune(self.selected, bcd)),
            [0x06, mode] => done(self.set_mode(self.selected, *mode, None, None)),
            [0x06, mode, filter] => done(self.set_mode(self.selected, *mode, None, Some(*filter))),
            [0x07, 0x00] => done(self.select(0)),
            [0x07, 0x01] => done(self.select(1)),
            [0x07, 0xB0] => {
                self.vfos.swap(0, 1);
                done(true)
            }
            [0x0F] => vec![0x0F, u8::from(self.split)],
            [0x0F, on @ (0x00 | 0x01)] => {
                self.split = *on == 0x01;
                done(true)
            }
            [0x19, 0x00] => vec![0x19, 0x00, self.address],
            [0x1A, 0x05, a, b, value @ ..] => self.menu_answer([*a, *b], value),
            [0x1A, 0x06] => {
                let vfo = self.vfos[self.selected];
                let filter = if vfo.data { vfo.filter } else { 0 };
                vec![0x1A, 0x06, u8::from(vfo.data), filter]
            }
            [0x1A, 0x06, data, filter] => done(self.set_data(*data, *filter)),
            [0x1C, 0x00] => vec![0x1C, 0x00, u8::from(self.transmitting)],
            [0x1C, 0x00, on @ (0x00 | 0x01)] => {
                self.transmitting = *on == 0x01;
                done(true)
            }
            [0x25, which @ (0x00 | 0x01)] => {
                let vfo = self.which(*which);
                self.frequency_answer(&[0x25, *which], vfo)
            }
            [0x25, which @ (0x00 | 0x01), bcd @ ..] => {
                let vfo = self.which(*which);
                done(self.tune(vfo, bcd))
            }
            [0x26, which @ (0x00 | 0x01)] => {
                let vfo = self.vfos[self.which(*which)];
                vec![0x26, *which, vfo.mode, u8::from(vfo.data), vfo.filter]
            }
            [0x26, which @ (0x00 | 0x01), mode, rest @ ..] => {
                let vfo = self.which(*which);
                match rest {
                    [] => done(self.set_mode(vfo, *mode, None, None)),
                    [data @ (0x00 | 0x01), filter] => {
                        done(self.set_mode(vfo, *mode, Some(*data == 0x01), Some(*filter)))
                    }
                    _ => done(false),
                }
            }
            _ => done(false),
        }
    }

    /// The VFO index for 25 / 26: 00 is the selected VFO, 01 the other one.
    fn which(&self, which: u8) -> usize {
        if which == 0x00 {
            self.selected
        } else {
            1 - self.selected
        }
    }

    fn frequency_answer(&self, head: &[u8], vfo: usize) -> Vec<u8> {
        match civ::frequency_to_bcd(self.vfos[vfo].hz) {
            Some(bcd) => [head, &bcd].concat(),
            None => vec![NG],
        }
    }

    fn tune(&mut self, vfo: usize, bcd: &[u8]) -> bool {
        match civ::frequency_from_bcd(bcd) {
            Some(hz) if (LOWEST_HZ..=HIGHEST_HZ).contains(&hz) => {
                self.vfos[vfo].hz = hz;
                true
            }
            _ => false,
        }
    }

    fn set_mode(&mut self, vfo: usize, mode: u8, data: Option<bool>, filter: Option<u8>) -> bool {
        if !self.modes.contains(&mode) || filter.is_some_and(|f| !FILTERS.contains(&f)) {
            return false;
        }
        let state = &mut self.vfos[vfo];
        state.mode = mode;
        if let Some(filter) = filter {
            state.filter = filter;
        }
        if let Some(data) = data {
            state.data = data;
        }
        true
    }

    fn select(&mut self, vfo: usize) -> bool {
        self.selected = vfo;
        true
    }

    fn set_data(&mut self, data: u8, filter: u8) -> bool {
        let state = &mut self.vfos[self.selected];
        match (data, filter) {
            (0x00, 0x00) => {
                state.data = false;
                true
            }
            (0x01, f) if FILTERS.contains(&f) => {
                state.data = true;
                state.filter = f;
                true
            }
            _ => false,
        }
    }

    fn menu_answer(&mut self, item: [u8; 2], value: &[u8]) -> Vec<u8> {
        let Some(menu) = self.menu.get(&item) else {
            return vec![NG];
        };
        if value.is_empty() {
            let mut answer = vec![0x1A, 0x05, item[0], item[1]];
            match menu.kind {
                Kind::Named(_) => answer.push(u8::try_from(menu.value).unwrap_or(0)),
                Kind::Level(..) => match civ::level_to_bcd(menu.value) {
                    Some(bcd) => answer.extend(bcd),
                    None => return vec![NG],
                },
            }
            return answer;
        }
        let new = match (&menu.kind, value) {
            (Kind::Named(_), [code]) => Some(u16::from(*code)),
            (Kind::Level(..), bcd) => civ::level_from_bcd(bcd),
            _ => None,
        };
        let ok = new.is_some_and(|v| self.set_menu(item, v));
        vec![if ok { OK } else { NG }]
    }
}

#[cfg(test)]
mod tests {
    use super::{HIGHEST_HZ, PretendCiv};
    use crate::civ::{Decoder, Frame, NG, OK, frequency_to_bcd, hex, level_to_bcd};
    use crate::rigdef;

    /// The agent's own address, as the IC-7300 definition gives it.
    const ME: u8 = 0xE0;
    /// The IC-7300's address.
    const RADIO: u8 = 0x94;

    fn radio() -> PretendCiv {
        let text = include_str!("../rigs/icom/ic-7300.toml");
        let definition =
            rigdef::parse(text).unwrap_or_else(|problems| unreachable!("{problems:?}"));
        PretendCiv::new(&definition)
    }

    /// Sends one command and returns every message that comes back.
    fn ask(radio: &mut PretendCiv, payload: &[u8]) -> Vec<Frame> {
        let bytes = radio.receive(&Frame::new(RADIO, ME, payload).encode());
        Decoder::default().push(&bytes)
    }

    /// Sends one command and returns the answer's payload.
    fn answer(radio: &mut PretendCiv, payload: &[u8]) -> Vec<u8> {
        let frames = ask(radio, payload);
        assert_eq!(frames.len(), 1, "expected one answer, got {frames:?}");
        assert_eq!((frames[0].to, frames[0].from), (ME, RADIO));
        frames[0].payload.clone()
    }

    fn bcd(hz: u64) -> Vec<u8> {
        frequency_to_bcd(hz)
            .unwrap_or_else(|| unreachable!())
            .to_vec()
    }

    #[test]
    fn says_who_it_is() {
        assert_eq!(answer(&mut radio(), &[0x19, 0x00]), [0x19, 0x00, RADIO]);
    }

    #[test]
    fn reads_and_tunes_the_frequency() {
        let mut radio = radio();
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(14_074_000)].concat()
        );
        assert_eq!(
            answer(&mut radio, &[[0x05].as_slice(), &bcd(7_074_000)].concat()),
            [OK]
        );
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(7_074_000)].concat()
        );
    }

    #[test]
    fn refuses_frequencies_it_cannot_tune() {
        let mut radio = radio();
        assert_eq!(
            answer(
                &mut radio,
                &[[0x05].as_slice(), &bcd(HIGHEST_HZ + 1)].concat()
            ),
            [NG]
        );
        assert_eq!(answer(&mut radio, &[0x05, 0x0A, 0, 0, 0, 0]), [NG]);
        assert_eq!(answer(&mut radio, &[0x05, 0x00]), [NG]);
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(14_074_000)].concat()
        );
    }

    #[test]
    fn sets_modes_it_has_and_refuses_others() {
        let mut radio = radio();
        assert_eq!(answer(&mut radio, &[0x04]), [0x04, 0x01, 0x01]);
        assert_eq!(answer(&mut radio, &[0x06, 0x03, 0x02]), [OK]);
        assert_eq!(answer(&mut radio, &[0x04]), [0x04, 0x03, 0x02]);
        assert_eq!(
            answer(&mut radio, &[0x06, 0x06]),
            [NG],
            "06h is not an IC-7300 mode"
        );
        assert_eq!(
            answer(&mut radio, &[0x06, 0x01, 0x04]),
            [NG],
            "there is no FIL4"
        );
    }

    #[test]
    fn reports_and_sets_data_mode() {
        let mut radio = radio();
        assert_eq!(answer(&mut radio, &[0x1A, 0x06]), [0x1A, 0x06, 0x01, 0x01]);
        assert_eq!(answer(&mut radio, &[0x1A, 0x06, 0x00, 0x00]), [OK]);
        assert_eq!(answer(&mut radio, &[0x1A, 0x06]), [0x1A, 0x06, 0x00, 0x00]);
        assert_eq!(answer(&mut radio, &[0x1A, 0x06, 0x00, 0x02]), [NG]);
    }

    #[test]
    fn reaches_the_other_vfo_without_switching() {
        let mut radio = radio();
        assert_eq!(
            answer(&mut radio, &[0x25, 0x01]),
            [[0x25, 0x01].as_slice(), &bcd(14_076_000)].concat()
        );
        assert_eq!(
            answer(
                &mut radio,
                &[[0x25, 0x01].as_slice(), &bcd(14_080_000)].concat()
            ),
            [OK]
        );
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(14_074_000)].concat()
        );
        assert_eq!(answer(&mut radio, &[0x07, 0x01]), [OK]);
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(14_080_000)].concat()
        );
        assert_eq!(
            answer(&mut radio, &[0x26, 0x00]),
            [0x26, 0x00, 0x01, 0x00, 0x01]
        );
    }

    #[test]
    fn turns_split_on_and_off() {
        let mut radio = radio();
        assert_eq!(answer(&mut radio, &[0x0F]), [0x0F, 0x00]);
        assert_eq!(answer(&mut radio, &[0x0F, 0x01]), [OK]);
        assert_eq!(answer(&mut radio, &[0x0F]), [0x0F, 0x01]);
    }

    #[test]
    fn reads_and_changes_menu_settings_from_its_definition() {
        let mut radio = radio();
        // DATA MOD (00 67) starts on its lowest code, MIC.
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x67]),
            [0x1A, 0x05, 0x00, 0x67, 0x00]
        );
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x67, 0x03]),
            [OK],
            "USB"
        );
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x67, 0x09]),
            [NG],
            "no such input"
        );
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x67]),
            [0x1A, 0x05, 0x00, 0x67, 0x03]
        );
        // USB MOD Level (00 65) is a level, sent as two BCD bytes.
        let middle = level_to_bcd(127).unwrap_or_else(|| unreachable!());
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x65]),
            [[0x1A, 0x05, 0x00, 0x65].as_slice(), &middle].concat()
        );
        assert_eq!(
            answer(&mut radio, &[0x1A, 0x05, 0x00, 0x65, 0x02, 0x56]),
            [NG],
            "256 is past the top"
        );
        // A menu item the definition doesn't list doesn't exist here.
        assert_eq!(answer(&mut radio, &[0x1A, 0x05, 0x09, 0x99]), [NG]);
    }

    #[test]
    fn reports_dial_turns_only_with_transceive_on() {
        let mut radio = radio();
        let report = Decoder::default().push(&radio.turn_dial(7_076_000));
        assert_eq!(
            report,
            [Frame::new(
                0x00,
                RADIO,
                &[[0x00].as_slice(), &bcd(7_076_000)].concat()
            )]
        );
        assert!(radio.set_menu([0x00, 0x71], 0x00), "transceive off");
        assert!(radio.turn_dial(7_077_000).is_empty());
        assert_eq!(
            answer(&mut radio, &[0x03]),
            [[0x03].as_slice(), &bcd(7_077_000)].concat()
        );
    }

    #[test]
    fn echoes_only_when_echo_back_is_on() {
        let mut radio = radio();
        assert_eq!(ask(&mut radio, &[0x19, 0x00]).len(), 1);
        assert!(
            radio.set_menu([0x00, 0x75], 0x00),
            "echo back on (00 is ON on the IC-7300)"
        );
        let frames = ask(&mut radio, &[0x19, 0x00]);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0],
            Frame::new(RADIO, ME, &[0x19, 0x00]),
            "the echo comes first"
        );
    }

    #[test]
    fn reports_transmit_status_and_never_starts_on_its_own() {
        let mut radio = radio();
        assert!(!radio.transmitting());
        assert_eq!(answer(&mut radio, &[0x1C, 0x00]), [0x1C, 0x00, 0x00]);
        assert_eq!(answer(&mut radio, &[0x1C, 0x00, 0x01]), [OK]);
        assert!(radio.transmitting());
        assert_eq!(answer(&mut radio, &[0x1C, 0x00, 0x00]), [OK]);
        assert!(!radio.transmitting());
    }

    #[test]
    fn ignores_messages_for_other_radios_and_refuses_unknown_commands() {
        let mut radio = radio();
        assert!(
            radio
                .receive(&Frame::new(0xA2, ME, &[0x03]).encode())
                .is_empty()
        );
        assert_eq!(answer(&mut radio, &[0x2A]), [NG]);
        assert_eq!(
            hex(&radio.receive(&[0x12, 0xFD, 0xFE])),
            "",
            "noise gets no answer"
        );
    }
}
