// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The pretend FT8 band: made-up stations for trying the FT8 box with no
//! radio attached.
//!
//! A few dozen invented stations share the band. Some call CQ; others answer
//! them and run through the usual exchange (grid, signal report, roger with a
//! report, RR73, 73), one message per 15-second slot, taking turns on even and
//! odd slots. Now and then a message is "not heard" and is sent again, as on
//! the air. Stations come and go.
//!
//! The pretend band only decides what each station sends in a slot. The
//! signals are then rendered into audio with static, at each station's own
//! strength and frequency, and LogLab's real decoder listens to that audio
//! the way it would listen to a radio. Nothing here is real and nothing is
//! transmitted; everything heard from it is labelled pretend.

use loglab_agent::ft8::bench::{Rng, Signal, Slot};

/// How many stations are on the band at once.
const STATIONS: usize = 36;
/// The share of stations that call CQ; the rest answer.
const CQ_SHARE: f64 = 0.45;
/// The chance a message is not heard, so it is sent again next turn.
const MISSED: f64 = 0.15;
/// The chance an idle CQ station stays quiet in a slot.
const QUIET: f64 = 0.25;
/// The chance an idle answering station calls someone in a slot.
const ANSWERS: f64 = 0.5;
/// The chance an idle station leaves and a new one arrives, per slot.
const TURNOVER: f64 = 0.03;
/// Lowest and highest audio frequency a station sits at, in hertz.
const LOWEST_HZ: f64 = 250.0;
const HIGHEST_HZ: f64 = 2_850.0;
/// Closest two stations sit, in hertz (an FT8 signal is 50 Hz wide).
const SPACING_HZ: f64 = 60.0;

/// One made-up station.
#[derive(Debug, Clone)]
struct Station {
    call: String,
    grid: String,
    /// Audio frequency of its lowest tone, in hertz.
    base_hz: f64,
    /// How strong it is here, in decibels in 2,500 Hz.
    snr_db: f64,
    /// How far off its clock is, in seconds.
    dt_s: f64,
    /// 0 for even slots, 1 for odd slots.
    parity: u64,
    /// Whether it calls CQ (true) or answers CQs (false).
    calls_cq: bool,
    /// Whether it called CQ in the last slot it had.
    just_called: bool,
}

/// A contact in progress between a CQ station and the station answering it.
#[derive(Debug, Clone)]
struct Contact {
    cq: usize,
    answer: usize,
    /// Which message of the exchange is next (0 to 4).
    step: u8,
    /// The reports each side gives the other.
    report_to_answer: i32,
    report_to_cq: i32,
}

/// The pretend band and everything going on in it.
#[derive(Debug, Clone)]
pub struct PretendBand {
    rng: Rng,
    stations: Vec<Station>,
    contacts: Vec<Contact>,
}

impl PretendBand {
    /// A band of made-up stations, the same every time for the same seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut band = Self {
            rng: Rng::new(seed),
            stations: Vec::with_capacity(STATIONS),
            contacts: Vec::new(),
        };
        for _ in 0..STATIONS {
            let station = band.new_station();
            band.stations.push(station);
        }
        band
    }

    /// What every station sends in slot `index` (slots are counted in
    /// 15-second steps; even and odd slots take turns), and the band moves on.
    pub fn slot(&mut self, index: u64) -> Slot {
        let parity = index % 2;
        let mut signals = Vec::new();
        let mut busy = vec![false; self.stations.len()];
        for contact in &self.contacts {
            busy[contact.cq] = true;
            busy[contact.answer] = true;
        }

        // Contacts under way: whoever's turn it is sends the next message.
        let mut finished = Vec::new();
        for (n, contact) in self.contacts.iter_mut().enumerate() {
            let cq = &self.stations[contact.cq];
            let answer = &self.stations[contact.answer];
            let sender = if contact.step % 2 == 0 { answer } else { cq };
            if sender.parity != parity {
                continue;
            }
            let message = match contact.step {
                0 => format!("{} {} {}", cq.call, answer.call, answer.grid),
                1 => format!(
                    "{} {} {:+03}",
                    answer.call, cq.call, contact.report_to_answer
                ),
                2 => format!("{} {} R{:+03}", cq.call, answer.call, contact.report_to_cq),
                3 => format!("{} {} RR73", answer.call, cq.call),
                _ => format!("{} {} 73", cq.call, answer.call),
            };
            signals.push(signal(sender, message, &mut self.rng));
            if self.rng.unit() >= MISSED {
                contact.step += 1;
                if contact.step > 4 {
                    finished.push(n);
                }
            }
        }
        for n in finished.into_iter().rev() {
            self.contacts.remove(n);
        }

        // Answering stations pick a CQ they heard in the slot before.
        let heard: Vec<usize> = (0..self.stations.len())
            .filter(|&s| {
                let st = &self.stations[s];
                st.calls_cq && st.just_called && st.parity != parity && !busy[s]
            })
            .collect();
        let mut taken = vec![false; self.stations.len()];
        for s in 0..self.stations.len() {
            let st = &self.stations[s];
            if st.calls_cq || busy[s] || st.parity != parity || heard.is_empty() {
                continue;
            }
            if self.rng.unit() >= ANSWERS {
                continue;
            }
            let cq = heard[self.rng.below(heard.len())];
            if taken[cq] {
                continue;
            }
            taken[cq] = true;
            busy[s] = true;
            busy[cq] = true;
            let contact = Contact {
                cq,
                answer: s,
                step: 0,
                report_to_answer: report(self.stations[s].snr_db),
                report_to_cq: report(self.stations[cq].snr_db),
            };
            let message = format!(
                "{} {} {}",
                self.stations[cq].call, self.stations[s].call, self.stations[s].grid
            );
            signals.push(signal(&self.stations[s], message, &mut self.rng));
            self.contacts.push(Contact {
                step: u8::from(self.rng.unit() >= MISSED),
                ..contact
            });
        }

        // Idle CQ stations call CQ, or rest.
        for (s, &in_contact) in busy.iter().enumerate() {
            if self.stations[s].parity != parity || !self.stations[s].calls_cq {
                continue;
            }
            let calling = !in_contact && self.rng.unit() >= QUIET;
            self.stations[s].just_called = calling;
            if calling {
                let st = &self.stations[s];
                let message = format!("CQ {} {}", st.call, st.grid);
                signals.push(signal(st, message, &mut self.rng));
            }
        }

        // Stations not in a contact sometimes leave, and someone new arrives.
        for (s, &in_contact) in busy.iter().enumerate() {
            if !in_contact && self.rng.unit() < TURNOVER {
                let station = self.new_station_except(s);
                self.stations[s] = station;
            }
        }

        Slot {
            index: usize::try_from(index).unwrap_or_default(),
            signals,
        }
    }

    fn new_station(&mut self) -> Station {
        self.new_station_except(usize::MAX)
    }

    /// A new station, at a frequency clear of everyone but station `leaving`.
    fn new_station_except(&mut self, leaving: usize) -> Station {
        let mut base_hz = LOWEST_HZ;
        for _ in 0..200 {
            base_hz = self.rng.range(LOWEST_HZ, HIGHEST_HZ).round();
            let clear = self
                .stations
                .iter()
                .enumerate()
                .all(|(n, s)| n == leaving || (s.base_hz - base_hz).abs() >= SPACING_HZ);
            if clear {
                break;
            }
        }
        let mut call = make_call(&mut self.rng);
        while self.stations.iter().any(|s| s.call == call) {
            call = make_call(&mut self.rng);
        }
        Station {
            call,
            grid: make_grid(&mut self.rng),
            base_hz,
            snr_db: (self.rng.range(-22.0, 6.0) * 10.0).round() / 10.0,
            dt_s: (self.rng.range(-0.2, 0.6) * 100.0).round() / 100.0,
            parity: u64::from(self.rng.unit() < 0.5),
            calls_cq: self.rng.unit() < CQ_SHARE,
            just_called: false,
        }
    }
}

/// A station's transmission in this slot, with its strength wavering a little.
fn signal(station: &Station, message: String, rng: &mut Rng) -> Signal {
    let fade = rng.range(-2.0, 2.0);
    Signal {
        message,
        base_hz: station.base_hz,
        dt_s: station.dt_s,
        snr_db: ((station.snr_db + fade) * 10.0).round() / 10.0,
    }
}

/// The signal report a station would give for `snr_db`.
#[allow(clippy::cast_possible_truncation)] // clamped to FT8's report range first
fn report(snr_db: f64) -> i32 {
    snr_db.round().clamp(-24.0, 24.0) as i32
}

/// A made-up standard callsign: a one- or two-character prefix, a digit, and
/// one to three letters. None starts with Q, as no calls are issued so.
fn make_call(rng: &mut Rng) -> String {
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const FIRST: &[u8] = b"ABCDEFGHIJKLMNOPRSTUVWXYZ";
    const DIGITS: &[u8] = b"0123456789";
    let pick = |rng: &mut Rng, from: &[u8]| char::from(from[rng.below(from.len())]);
    let mut call = String::new();
    call.push(pick(rng, FIRST));
    if rng.below(2) == 0 {
        call.push(pick(rng, LETTERS));
    }
    call.push(pick(rng, DIGITS));
    for _ in 0..=rng.below(3) {
        call.push(pick(rng, LETTERS));
    }
    call
}

/// A made-up four-character grid square.
fn make_grid(rng: &mut Rng) -> String {
    const FIELD: &[u8] = b"ABCDEFGHIJKLMNOPQR";
    const DIGITS: &[u8] = b"0123456789";
    let pick = |rng: &mut Rng, from: &[u8]| char::from(from[rng.below(from.len())]);
    [
        pick(rng, FIELD),
        pick(rng, FIELD),
        pick(rng, DIGITS),
        pick(rng, DIGITS),
    ]
    .iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use loglab_agent::ft8::encode;

    use super::PretendBand;

    #[test]
    fn every_message_is_one_ft8_can_send() {
        let mut band = PretendBand::new(7);
        for index in 0..80 {
            for signal in band.slot(index).signals {
                assert!(
                    encode::tones(&signal.message).is_ok(),
                    "{} cannot be sent",
                    signal.message
                );
            }
        }
    }

    #[test]
    fn the_band_is_busy_but_never_crowded_onto_one_frequency() {
        let mut band = PretendBand::new(3);
        for index in 0..40 {
            let slot = band.slot(index);
            if index < 4 {
                continue; // the band takes a few slots to get going
            }
            assert!(
                slot.signals.len() >= 4,
                "slot {index} has only {}",
                slot.signals.len()
            );
            let mut on: BTreeMap<i64, usize> = BTreeMap::new();
            for signal in &slot.signals {
                #[allow(clippy::cast_possible_truncation)] // whole hertz
                let hz = signal.base_hz as i64;
                *on.entry(hz).or_default() += 1;
            }
            assert!(
                on.values().all(|&n| n == 1),
                "two signals share a frequency"
            );
        }
    }

    #[test]
    fn contacts_run_through_to_73() {
        let mut band = PretendBand::new(11);
        let mut finished = 0;
        for index in 0..60 {
            finished += band
                .slot(index)
                .signals
                .iter()
                .filter(|s| s.message.ends_with(" 73"))
                .count();
        }
        assert!(finished >= 5, "only {finished} contacts finished");
    }

    #[test]
    fn the_same_seed_makes_the_same_band() {
        let mut a = PretendBand::new(5);
        let mut b = PretendBand::new(5);
        for index in 0..10 {
            assert_eq!(a.slot(index), b.slot(index));
        }
    }
}
