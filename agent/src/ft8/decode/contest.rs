// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Contest profile: the same decoder, spending its time where a
//! crowded band needs it.
//!
//! On a Field Day band fifty to eighty signals sit in 3 kHz, many on top
//! of one another, and the operator wants the strong ones and the pile-up
//! on their own frequency within a couple of seconds of the slot's end.
//! The everyday search (one deep pass per candidate, three passes) spends
//! most of a cycle on candidates that are only the leftovers of louder
//! signals, and stops looking while signals are still being uncovered.
//! This profile instead runs
//!
//! 1. **quick rounds**: belief propagation alone on every candidate, in
//!    small batches so decodes are handed out as they come, loudest first
//!    and anything on the operator's frequency before that; each decoded
//!    signal is subtracted at once, and the band is searched again until
//!    a round finds nothing new, since every signal removed uncovers the
//!    ones it was hiding;
//! 2. **the deep search**: ordered-statistics decoding on what the quick
//!    rounds left, loudest first, re-locking any candidate whose
//!    neighbourhood changed since; what it finds is subtracted and the
//!    quick rounds run again;
//!
//! until nothing new turns up or the time limit is reached. Every decode
//! passes the same checks as in the everyday profile: nothing about how a
//! codeword is accepted differs between the two.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::{
    Baseband, Candidate, Decode, Fit, Leftover, Locked, NEAR_HZ, NEIGHBOURS, Options, Spectrogram,
    TONE_SPACING_HZ, ToneMeter, candidates, deep_decode, demodulate_groups, fit, in_parallel,
    profile, record, row_to_hz,
};

/// Candidates locked and tried at once in a quick round: few, so that
/// the first decodes reach the caller within a fraction of a second.
const QUICK_BATCH: usize = 16;

/// Leftovers given to the deep search at once.
const DEEP_BATCH: usize = 16;

/// Candidates within this of the operator's own frequency come first, in
/// hertz either way: the pile-up answering a CQ.
const OWN_HZ: f64 = 60.0;

/// The group sizes a quick round tries. The four-symbol likelihoods cost
/// more to build than the other three together and rarely decode what
/// the three-symbol ones cannot by belief propagation, so they are left
/// for the deep search, which builds them only for the leftovers it
/// reaches.
const QUICK_GROUPS: [usize; 3] = [1, 3, 2];

/// The likelihood sets a quick round keeps for the deep search: the ones
/// it has built that the deep search uses for some class.
fn keep(_class: usize) -> &'static [usize] {
    &[1, 3]
}

/// What a quick try leaves when belief propagation fails: the fit, the
/// best lock and its leftover, boxed because it is large and rarely
/// moved.
type Left = Box<(Fit, (Locked, Leftover))>;

/// A candidate the quick rounds could not decode.
struct Pending {
    candidate: Candidate,
    /// Its frequency, in hertz.
    hz: f64,
    /// Where its lock settled.
    fit: Fit,
    /// The best lock and what belief propagation left of it.
    lock: (Locked, Leftover),
    /// How many signals had been decoded when the locks were made; a
    /// signal decoded since, near the candidate, changed its audio.
    seen: usize,
}

/// The state of one slot's search.
struct Search<'a> {
    options: &'a Options,
    started: Instant,
    meter: ToneMeter,
    /// The audio with every decoded signal subtracted.
    residual: Vec<f32>,
    /// The messages decoded so far.
    found: BTreeSet<String>,
    /// Where candidates have been tried, and how many signals had been
    /// decoded then: a place is tried again once a signal near it has
    /// been removed since.
    tried: BTreeMap<(isize, usize), usize>,
    /// The frequencies of the signals decoded, in order.
    decoded_hz: Vec<f64>,
    /// What the quick rounds left for the deep search.
    pending: Vec<Pending>,
}

/// Decodes one slot under the contest profile; see the module notes.
pub(super) fn decode(audio: &[f32], options: &Options, each: &mut dyn FnMut(Decode)) -> Vec<f32> {
    let mut search = Search {
        options,
        started: Instant::now(),
        meter: ToneMeter::new(),
        residual: audio.to_vec(),
        found: BTreeSet::new(),
        tried: BTreeMap::new(),
        decoded_hz: Vec::new(),
        pending: Vec::new(),
    };
    loop {
        if !search.quick_rounds(each) {
            break;
        }
        if search.pending.is_empty() || !search.deep_phase(each) {
            break;
        }
    }
    search.residual
}

/// Candidates near the operator's own frequency come before all others,
/// then the louder before the quieter.
fn priority(options: &Options, a: (f64, f32), b: (f64, f32)) -> std::cmp::Ordering {
    let own_first = |hz: f64| options.own_hz.is_some_and(|own| (hz - own).abs() <= OWN_HZ);
    own_first(b.0)
        .cmp(&own_first(a.0))
        .then(b.1.total_cmp(&a.1))
}

impl Search<'_> {
    fn out_of_time(&self) -> bool {
        self.started.elapsed().as_secs_f64() >= self.options.limit_s
    }

    /// Whether a signal within [`NEAR_HZ`] of `hz` has been decoded since
    /// `since` signals had been.
    fn changed_near(&self, hz: f64, since: usize) -> bool {
        self.decoded_hz[since..]
            .iter()
            .any(|d| (d - hz).abs() < NEAR_HZ)
    }

    /// Hands out a decode and subtracts it; whether it was new.
    fn take(&mut self, locked: &Locked, each: &mut dyn FnMut(Decode)) -> bool {
        match record(
            &mut self.found,
            &mut self.residual,
            locked,
            self.options,
            each,
        ) {
            Some(hz) => {
                self.decoded_hz.push(hz);
                true
            }
            None => false,
        }
    }

    /// Quick rounds until one finds nothing new; whether time remains.
    fn quick_rounds(&mut self, each: &mut dyn FnMut(Decode)) -> bool {
        loop {
            if self.out_of_time() {
                return false;
            }
            let timer = profile::Timer::start();
            let spectrogram =
                Spectrogram::new(&self.residual, self.options.high_hz + 8.0 * TONE_SPACING_HZ);
            let baseband = Baseband::new(&self.residual);
            let mut fresh: Vec<Candidate> = candidates(&spectrogram, self.options)
                .into_iter()
                .filter(|c| match self.tried.get(&(c.column, c.row)) {
                    Some(&since) => self.changed_near(row_to_hz(c.row), since),
                    None => true,
                })
                .collect();
            for c in &fresh {
                self.tried.insert((c.column, c.row), self.decoded_hz.len());
            }
            fresh.sort_by(|a, b| {
                priority(
                    self.options,
                    (row_to_hz(a.row), a.power),
                    (row_to_hz(b.row), b.power),
                )
            });
            timer.stop(profile::Stage::Search);
            profile::count(profile::Count::Candidates, fresh.len());
            let mut any = false;
            for batch in fresh.chunks(QUICK_BATCH) {
                if self.out_of_time() {
                    return false;
                }
                let quick = in_parallel(batch, self.options.threads, |&candidate| {
                    quick_try(&baseband, &self.meter, candidate, self.options)
                });
                for (candidate, outcome) in batch.iter().zip(quick) {
                    match outcome {
                        Ok(locked) => any |= self.take(&locked, each),
                        Err(left) => {
                            let (fit, lock) = *left;
                            self.pending.push(Pending {
                                candidate: *candidate,
                                hz: row_to_hz(candidate.row),
                                fit,
                                lock,
                                seen: self.decoded_hz.len(),
                            });
                        }
                    }
                }
            }
            if !any {
                return true;
            }
        }
    }

    /// The deep search on what is pending, loudest first; whether it
    /// found anything and time remains.
    fn deep_phase(&mut self, each: &mut dyn FnMut(Decode)) -> bool {
        let mut waiting = std::mem::take(&mut self.pending);
        waiting.sort_by(|a, b| {
            priority(
                self.options,
                (a.hz, a.candidate.power),
                (b.hz, b.candidate.power),
            )
        });
        let timer = profile::Timer::start();
        let baseband = Baseband::new(&self.residual);
        timer.stop(profile::Stage::Search);
        let mut any = false;
        for batch in waiting.chunks(DEEP_BATCH) {
            if self.out_of_time() {
                return false;
            }
            let deep = in_parallel(batch, self.options.threads, |p| self.deep_try(&baseband, p));
            for locked in deep.into_iter().flatten() {
                any |= self.take(&locked, each);
            }
        }
        any
    }

    /// The deep search on one pending candidate: the best lock from the
    /// quick round, unless a signal removed since changed the audio
    /// under it; then its neighbours, measured now, which the quick round
    /// skipped.
    fn deep_try(&self, baseband: &Baseband, p: &Pending) -> Option<Locked> {
        let (fit, best) = if self.changed_near(p.hz, p.seen) {
            match quick_try(baseband, &self.meter, p.candidate, self.options) {
                Ok(locked) => return Some(locked),
                Err(left) => *left,
            }
        } else {
            (p.fit, p.lock.clone())
        };
        let mut locks = vec![best];
        for offset in NEIGHBOURS {
            let locked = fit.lock(baseband, &self.meter, offset);
            match demodulate_groups(&locked, self.options, &QUICK_GROUPS, keep) {
                Ok(_) => return Some(locked),
                Err(leftover) => locks.push((locked, leftover)),
            }
        }
        locks
            .into_iter()
            .find(|(_, leftover)| deep_decode(leftover, self.options).is_some())
            .map(|(locked, _)| locked)
    }
}

/// Locks a candidate and tries belief propagation at the best lock alone
/// (its neighbours wait for the deep search: a signal strong enough to
/// decode by belief propagation rarely needs them, and most candidates
/// here are not signals at all): the decode, or the fit and what the lock
/// left for the deep search.
fn quick_try(
    baseband: &Baseband,
    meter: &ToneMeter,
    candidate: Candidate,
    options: &Options,
) -> Result<Locked, Left> {
    let timer = profile::Timer::start();
    let fit = fit(baseband, meter, candidate);
    let locked = fit.lock(baseband, meter, (0.0, 0));
    timer.stop(profile::Stage::Lock);
    match demodulate_groups(&locked, options, &QUICK_GROUPS, keep) {
        Ok(_) => Ok(locked),
        Err(leftover) => Err(Box::new((fit, (locked, leftover)))),
    }
}
