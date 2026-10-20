// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A crowded Field Day band for the bench: the scenario the Contest
//! decoder profile is measured on.
//!
//! The standard bench is ten well-spaced signals of similar strength. A
//! Field Day band is nothing like it: fifty to eighty stations in 3 kHz,
//! forty decibels between the loudest and the quietest, signals on top of
//! one another, and a pile-up on the operator's own frequency. This module
//! models that band as a *population* of stations that persist from one
//! receive slot to the next, each at its own frequency and strength and
//! each working through Field Day QSOs with stations the receiver never
//! hears (they transmit in the other half of the cycle, as the operator's
//! own station does). The operator's own station, with the call the
//! scenario names, calls CQ in its transmit slots, so every receive slot
//! may hold stations answering it, usually on or near its frequency, and
//! the station it answered sending its `RR73`.
//!
//! Everything comes from the seed, so the band is the same on every
//! machine.

use super::{Rng, Signal, Slot, random_call};
use crate::ft8::message;

/// What the contest band looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct ContestSpec {
    /// The operator's own callsign: the station the band answers.
    pub own_call: String,
    /// The operator's own transmit frequency (lowest tone), in hertz:
    /// where the pile-up answering it gathers.
    pub own_hz: f64,
    /// Fewest signals in a slot.
    pub min_signals: usize,
    /// Most signals in a slot.
    pub max_signals: usize,
}

impl ContestSpec {
    /// The Field Day band the brief describes: 50 to 80 signals per slot,
    /// answering K9KSU.
    #[must_use]
    pub fn field_day() -> Self {
        Self {
            own_call: "K9KSU".to_owned(),
            own_hz: 1_500.0,
            min_signals: 50,
            max_signals: 80,
        }
    }
}

/// Lowest and highest base frequency stations sit at, in hertz.
const LOWEST_HZ: f64 = 200.0;
const HIGHEST_HZ: f64 = 2_900.0;

/// Furthest a station answering the operator's CQ on the operator's own
/// frequency sits from it, in hertz, either way.
const PILEUP_HZ: f64 = 25.0;

/// How many stations beyond the most that transmit in a slot make up the
/// population, so that a slot's stations are a sample of a larger band.
const SPARE_STATIONS: usize = 12;

/// Share of stations whose strength is drawn from the loud end: the few
/// very loud ones a 40 dB spread owes to nearby stations.
const LOUD_SHARE: f64 = 0.12;

/// Width, in decibels, of the loud end of the strength range.
const LOUD_SPAN_DB: f64 = 12.0;

/// Standard deviation of the rest of the stations' strengths around the
/// middle of the quiet range, in decibels.
const STRENGTH_SD_DB: f64 = 6.0;

/// How much a station's strength changes from one slot to the next, in
/// decibels (one standard deviation).
const SLOT_FADE_SD_DB: f64 = 1.5;

/// Grid fields North American Field Day stations are in.
const GRID_FIELDS: [&str; 9] = ["CM", "CN", "DM", "DN", "EL", "EM", "EN", "FM", "FN"];

/// What a heard station is doing.
#[derive(Debug, Clone, PartialEq)]
enum Doing {
    /// Calling CQ.
    Cq,
    /// Answering a station that answered its CQ, with `R` and its exchange.
    Rogering(String),
    /// Sending `73` to finish a QSO it was called in.
    Finishing(String),
    /// Calling a station whose CQ it answered, with its exchange.
    Calling(String),
    /// Sending `RR73` to a station that rogered its call.
    Confirming(String),
}

/// A station the receiver hears.
#[derive(Debug, Clone)]
struct Station {
    call: String,
    /// `3A`: transmitters and class.
    class: String,
    section: &'static str,
    grid: String,
    base_hz: f64,
    /// Its strength over the slots, in decibels.
    snr_db: f64,
    dt_s: f64,
    /// How likely it is to transmit in any given slot.
    duty: f64,
    doing: Doing,
}

/// A station answering the operator's own CQ.
#[derive(Debug, Clone)]
struct Caller {
    call: String,
    class: String,
    section: &'static str,
    base_hz: f64,
    snr_db: f64,
    dt_s: f64,
}

/// What the operator's own station is doing.
#[derive(Debug, Clone, PartialEq)]
enum Own {
    /// Just called CQ; the next receive slot may bring answers.
    Cq,
    /// Just rogered one of its callers; the next slot should bring its
    /// `RR73`.
    Rogered(String),
}

/// The band, slot by slot.
pub(super) struct Band<'a> {
    spec: &'a ContestSpec,
    min_snr_db: f64,
    max_snr_db: f64,
    rng: Rng,
    stations: Vec<Station>,
    own_hz: f64,
    own: Own,
    callers: Vec<Caller>,
}

impl<'a> Band<'a> {
    /// A band from `seed`, with strengths from `min_snr_db` to
    /// `max_snr_db`.
    pub(super) fn new(spec: &'a ContestSpec, seed: u64, min_snr_db: f64, max_snr_db: f64) -> Self {
        let mut band = Self {
            spec,
            min_snr_db,
            max_snr_db,
            rng: Rng::new(seed),
            stations: Vec::new(),
            own_hz: spec.own_hz,
            own: Own::Cq,
            callers: Vec::new(),
        };
        while band.stations.len() < spec.max_signals + SPARE_STATIONS {
            let station = band.new_station();
            band.stations.push(station);
        }
        band
    }

    /// The next receive slot.
    pub(super) fn slot(&mut self, index: usize) -> Slot {
        let mut signals = Vec::new();
        let mut messages = std::collections::BTreeSet::new();
        let mut used_calls: std::collections::BTreeSet<String> =
            self.stations.iter().map(|s| s.call.clone()).collect();
        used_calls.insert(self.spec.own_call.clone());

        // The stations answering the operator's own CQ, on its frequency
        // and around it. They come first so that the slot's count has room
        // for them.
        for caller in self.own_slot(&mut used_calls) {
            if messages.insert(caller.0.clone()) {
                signals.push(caller);
            }
        }

        // Then the band: each station transmits by its duty, and the count
        // is held within the scenario's range.
        let wanted = self
            .rng
            .below(self.spec.max_signals - self.spec.min_signals + 1)
            + self.spec.min_signals;
        let mut active: Vec<usize> = (0..self.stations.len())
            .filter(|&i| self.rng.unit() < self.stations[i].duty)
            .collect();
        while active.len() + signals.len() > wanted {
            let drop = self.rng.below(active.len());
            active.swap_remove(drop);
        }
        let mut idle: Vec<usize> = (0..self.stations.len())
            .filter(|i| !active.contains(i))
            .collect();
        while active.len() + signals.len() < wanted && !idle.is_empty() {
            let add = self.rng.below(idle.len());
            active.push(idle.swap_remove(add));
        }
        active.sort_unstable();
        for i in active {
            let text = self.transmit(i, &mut used_calls);
            if !messages.insert(text.clone()) {
                continue;
            }
            let station = &self.stations[i];
            let snr_db = self.rng.gaussian().mul_add(SLOT_FADE_SD_DB, station.snr_db);
            signals.push((
                text,
                station.base_hz,
                station.dt_s,
                snr_db.clamp(self.min_snr_db, self.max_snr_db),
            ));
        }
        signals.sort_by(|a, b| a.1.total_cmp(&b.1));
        Slot {
            index,
            signals: signals
                .into_iter()
                .map(|(message, base_hz, dt_s, snr_db)| Signal {
                    message,
                    base_hz,
                    dt_s,
                    snr_db: (snr_db * 10.0).round() / 10.0,
                })
                .collect(),
        }
    }

    /// A fresh station at a frequency of its own.
    fn new_station(&mut self) -> Station {
        let call = self.fresh_call();
        let base_hz = loop {
            let hz = self.rng.range(LOWEST_HZ, HIGHEST_HZ).round();
            if (hz - self.own_hz).abs() >= 1.0
                && self.stations.iter().all(|s| (s.base_hz - hz).abs() >= 1.0)
            {
                break hz;
            }
        };
        let (class, section) = self.exchange();
        Station {
            call,
            class,
            section,
            grid: self.grid(),
            base_hz,
            snr_db: self.strength(),
            dt_s: self.timing(),
            duty: self.rng.range(0.55, 0.95),
            doing: Doing::Cq,
        }
    }

    /// A callsign no station on the band has.
    fn fresh_call(&mut self) -> String {
        loop {
            let call = random_call(&mut self.rng);
            if call != self.spec.own_call && self.stations.iter().all(|s| s.call != call) {
                return call;
            }
        }
    }

    /// A Field Day exchange: transmitters and class, and a section.
    fn exchange(&mut self) -> (String, &'static str) {
        // Most entries run one to five transmitters; a few run many.
        let transmitters = if self.rng.unit() < 0.9 {
            self.rng.below(5) + 1
        } else {
            self.rng.below(32) + 1
        };
        let class = b"ABCDEF"[self.rng.below(6)];
        let sections = message::field_day_sections();
        (
            format!("{transmitters}{}", char::from(class)),
            sections[self.rng.below(sections.len())],
        )
    }

    fn grid(&mut self) -> String {
        format!(
            "{}{}{}",
            GRID_FIELDS[self.rng.below(GRID_FIELDS.len())],
            self.rng.below(10),
            self.rng.below(10)
        )
    }

    /// A station's strength: most sit around the middle of the quiet
    /// range, a few are very loud.
    fn strength(&mut self) -> f64 {
        let loud_from = self.max_snr_db - LOUD_SPAN_DB;
        if self.rng.unit() < LOUD_SHARE {
            self.rng.range(loud_from, self.max_snr_db)
        } else {
            let middle = f64::midpoint(self.min_snr_db, loud_from);
            self.rng
                .gaussian()
                .mul_add(STRENGTH_SD_DB, middle)
                .clamp(self.min_snr_db, loud_from)
        }
    }

    fn timing(&mut self) -> f64 {
        (self.rng.range(-0.4, 1.0) * 100.0).round() / 100.0
    }

    /// What station `i` sends this slot, and what it will be doing next.
    fn transmit(
        &mut self,
        i: usize,
        used_calls: &mut std::collections::BTreeSet<String>,
    ) -> String {
        let mut other = |band: &mut Self| loop {
            let call = random_call(&mut band.rng);
            if used_calls.insert(call.clone()) {
                break call;
            }
        };
        let station = self.stations[i].clone();
        let exchange = format!("{} {}", station.class, station.section);
        let roll = self.rng.unit();
        let (text, next) = match station.doing {
            Doing::Cq => {
                let text = format!("CQ FD {} {}", station.call, station.grid);
                let next = if roll < 0.4 {
                    Doing::Rogering(other(self))
                } else if roll < 0.55 {
                    Doing::Calling(other(self))
                } else {
                    Doing::Cq
                };
                (text, next)
            }
            Doing::Rogering(ref partner) => {
                let text = format!("{partner} {} R {exchange}", station.call);
                let next = if roll < 0.15 {
                    Doing::Rogering(partner.clone())
                } else if roll < 0.55 {
                    Doing::Finishing(partner.clone())
                } else {
                    Doing::Cq
                };
                (text, next)
            }
            Doing::Finishing(ref partner) => {
                let text = format!("{partner} {} 73", station.call);
                (text, Doing::Cq)
            }
            Doing::Calling(ref partner) => {
                let text = format!("{partner} {} {exchange}", station.call);
                let next = if roll < 0.6 {
                    Doing::Confirming(partner.clone())
                } else if roll < 0.85 {
                    Doing::Calling(partner.clone())
                } else {
                    Doing::Cq
                };
                (text, next)
            }
            Doing::Confirming(ref partner) => {
                let text = format!("{partner} {} RR73", station.call);
                let next = if roll < 0.7 {
                    Doing::Cq
                } else {
                    Doing::Calling(other(self))
                };
                (text, next)
            }
        };
        self.stations[i].doing = next;
        text
    }

    /// The signals addressed to the operator's own station this slot, as
    /// (message, frequency, timing, strength), and the operator's next
    /// move.
    fn own_slot(
        &mut self,
        used_calls: &mut std::collections::BTreeSet<String>,
    ) -> Vec<(String, f64, f64, f64)> {
        let own_call = self.spec.own_call.clone();
        let mut out = Vec::new();
        match self.own.clone() {
            Own::Cq => {
                // Callers still waiting mostly try again; some give up.
                // New ones join them.
                let mut waiting = Vec::new();
                for caller in self.callers.clone() {
                    if self.rng.unit() < 0.7 {
                        waiting.push(caller);
                    }
                }
                self.callers = waiting;
                let count = match self.rng.below(100) {
                    0..30 => 0,
                    30..60 => 1,
                    60..80 => 2,
                    80..92 => 3,
                    _ => 4,
                };
                while self.callers.len() < count {
                    let caller = self.new_caller(used_calls);
                    self.callers.push(caller);
                }
                for caller in &self.callers {
                    out.push((
                        format!(
                            "{own_call} {} {} {}",
                            caller.call, caller.class, caller.section
                        ),
                        caller.base_hz,
                        caller.dt_s,
                        caller.snr_db,
                    ));
                }
                // The operator answers the strongest caller, if any.
                self.own = self
                    .callers
                    .iter()
                    .max_by(|a, b| a.snr_db.total_cmp(&b.snr_db))
                    .map_or(Own::Cq, |c| Own::Rogered(c.call.clone()));
            }
            Own::Rogered(call) => {
                let mut remaining = Vec::new();
                for caller in self.callers.clone() {
                    if caller.call == call {
                        // Usually it heard the roger and confirms; now and
                        // then it did not and calls again.
                        let text = if self.rng.unit() < 0.85 {
                            format!("{own_call} {} RR73", caller.call)
                        } else {
                            remaining.push(caller.clone());
                            format!(
                                "{own_call} {} {} {}",
                                caller.call, caller.class, caller.section
                            )
                        };
                        out.push((text, caller.base_hz, caller.dt_s, caller.snr_db));
                    } else if self.rng.unit() < 0.6 {
                        // The others keep calling, or give up.
                        out.push((
                            format!(
                                "{own_call} {} {} {}",
                                caller.call, caller.class, caller.section
                            ),
                            caller.base_hz,
                            caller.dt_s,
                            caller.snr_db,
                        ));
                        remaining.push(caller);
                    }
                }
                self.callers = remaining;
                self.own = Own::Cq;
            }
        }
        for caller in &mut self.callers {
            caller.snr_db = self
                .rng
                .gaussian()
                .mul_add(SLOT_FADE_SD_DB, caller.snr_db)
                .clamp(self.min_snr_db, self.max_snr_db);
        }
        out
    }

    /// A station answering the operator's CQ: on the operator's frequency
    /// or close to it, as most do, or wherever it was. Half the time it is
    /// a station the receiver has been hearing work others on the band, so
    /// its exchange was already heard before it called; the rest are new
    /// to the band. A station that moves to the pile-up leaves its place
    /// on the band to a fresh one.
    fn new_caller(&mut self, used_calls: &mut std::collections::BTreeSet<String>) -> Caller {
        let on_own_frequency = self.rng.unit() < 0.6;
        let base_hz = (self.own_hz + self.rng.range(-PILEUP_HZ, PILEUP_HZ)).round();
        if self.rng.unit() < 0.5 {
            let known = self
                .stations
                .swap_remove(self.rng.below(self.stations.len()));
            let fresh = self.new_station();
            self.stations.push(fresh);
            return Caller {
                call: known.call,
                class: known.class,
                section: known.section,
                base_hz: if on_own_frequency {
                    base_hz
                } else {
                    known.base_hz
                },
                snr_db: known.snr_db,
                dt_s: known.dt_s,
            };
        }
        let call = loop {
            let call = random_call(&mut self.rng);
            if used_calls.insert(call.clone()) {
                break call;
            }
        };
        let base_hz = if on_own_frequency {
            base_hz
        } else {
            self.rng.range(LOWEST_HZ, HIGHEST_HZ).round()
        };
        let (class, section) = self.exchange();
        let snr_db = self.strength();
        Caller {
            call,
            class,
            section,
            base_hz,
            snr_db,
            dt_s: self.timing(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Band, ContestSpec};
    use crate::ft8::message::{MessageError, pack};

    #[test]
    fn every_slot_is_within_the_range_and_encodes() -> Result<(), MessageError> {
        let spec = ContestSpec::field_day();
        let mut band = Band::new(&spec, 1, -24.0, 16.0);
        for index in 0..20 {
            let slot = band.slot(index);
            assert!(
                (50..=80).contains(&slot.signals.len()),
                "{}",
                slot.signals.len()
            );
            for s in &slot.signals {
                pack(&s.message)?;
                assert!((-24.0..=16.0).contains(&s.snr_db), "{}", s.snr_db);
                assert!(
                    s.message.split(' ').all(|w| w != "K9KSU") || s.message.starts_with("K9KSU ")
                );
            }
            let messages: std::collections::BTreeSet<&str> =
                slot.signals.iter().map(|s| s.message.as_str()).collect();
            assert_eq!(
                messages.len(),
                slot.signals.len(),
                "a message was sent twice"
            );
        }
        Ok(())
    }

    #[test]
    fn the_operator_is_answered_and_confirmed() {
        let spec = ContestSpec::field_day();
        let mut band = Band::new(&spec, 2, -24.0, 16.0);
        let (mut answers, mut confirmations) = (0, 0);
        for index in 0..40 {
            for s in band.slot(index).signals {
                if let Some(rest) = s.message.strip_prefix("K9KSU ") {
                    if rest.ends_with(" RR73") {
                        confirmations += 1;
                    } else {
                        answers += 1;
                    }
                }
            }
        }
        assert!(answers > 20, "{answers} answers");
        assert!(confirmations > 5, "{confirmations} confirmations");
    }

    #[test]
    fn the_band_repeats_from_its_seed() {
        let spec = ContestSpec::field_day();
        let mut a = Band::new(&spec, 5, -24.0, 16.0);
        let mut b = Band::new(&spec, 5, -24.0, 16.0);
        for index in 0..5 {
            assert_eq!(a.slot(index), b.slot(index));
        }
    }
}
