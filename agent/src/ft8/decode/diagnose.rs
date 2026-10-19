// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Where the decoder loses a signal: a bench tool, not part of decoding.
//!
//! Given a recording and the truth about one signal in it, this follows the
//! decoder's steps for that signal and says how far it got: whether the
//! sync search put a candidate near it, how close the lock came to the true
//! frequency and timing, how many symbols and bits the likelihoods got
//! wrong, which likelihoods belief propagation and ordered-statistics
//! decoding recover the right codeword from, and how strongly the right
//! codeword passes the phase-coherence check. It does all of that twice:
//! from the decoder's own lock, and from an *oracle* lock at the true
//! frequency and timing, which separates losses in synchronization from
//! losses in everything after it.

use std::fmt::Write as _;

use rustfft::num_complex::Complex32;

use super::{
    Baseband, Candidate, GROUPS, Locked, OSD_GROUPS, OSD_MAX_DISTANCE, Options, STEP, Spectrogram,
    TONE_SPACING_HZ, ToneMeter, candidates, coherence, data_symbols, ldpc, likelihoods, lock,
    noise_power, row_to_hz, to_isize,
};
use crate::ft8::baseband;
use crate::ft8::encode::{self, Tones};
use crate::ft8::message;
use crate::ft8::{NOMINAL_START_SECONDS, SAMPLE_RATE, SYMBOLS};

/// What is known about one signal.
#[derive(Debug, Clone)]
pub struct Truth {
    /// The 174 codeword bits sent.
    pub bits: [bool; ldpc::N],
    /// The 79 tones sent.
    pub tones: Tones,
    /// Frequency of the lowest tone, in hertz.
    pub base_hz: f64,
    /// Start relative to the nominal 0.5 s, in seconds.
    pub dt_s: f64,
}

impl Truth {
    /// The truth for a message sent at `base_hz` and `dt_s`; `None` if the
    /// message cannot be encoded.
    #[must_use]
    pub fn new(text: &str, base_hz: f64, dt_s: f64) -> Option<Self> {
        let payload = message::pack(text).ok()?;
        Some(Self {
            bits: encode::codeword(payload),
            tones: encode::tones_for_payload(payload),
            base_hz,
            dt_s,
        })
    }

    /// The true start, in baseband samples.
    #[allow(clippy::cast_possible_truncation)] // a few thousand
    fn start(&self) -> isize {
        ((NOMINAL_START_SECONDS + self.dt_s) * f64::from(baseband::RATE)).round() as isize
    }
}

/// What one decoding step did with the right codeword in reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It found the codeword that was sent.
    Right,
    /// It found another codeword.
    Wrong,
    /// It found nothing.
    Nothing,
}

impl Outcome {
    fn mark(self) -> char {
        match self {
            Self::Right => 'R',
            Self::Wrong => 'W',
            Self::Nothing => '-',
        }
    }
}

/// The decoder's steps from one lock.
#[derive(Debug, Clone)]
pub struct Trial {
    /// Data symbols whose strongest tone is not the one sent (of 58).
    pub symbol_errors: usize,
    /// For each group size in [`GROUPS`] order: bits whose likelihood has
    /// the wrong sign (of 174).
    pub bit_errors: Vec<usize>,
    /// For each group size in [`GROUPS`] order: belief propagation.
    pub bp: Vec<Outcome>,
    /// For each group size in [`OSD_GROUPS`] order: ordered statistics,
    /// within the distance limit.
    pub osd: Vec<Outcome>,
    /// The right codeword's score in the phase-coherence check.
    pub coherence: f32,
    /// The same, over each third of the data symbols: a fade in one third
    /// shows as a low score there.
    pub coherence_thirds: [f32; 3],
}

impl Trial {
    /// Whether some step found the right codeword (before the coherence
    /// check).
    #[must_use]
    pub fn recovers(&self) -> bool {
        self.bp
            .iter()
            .chain(&self.osd)
            .any(|&o| o == Outcome::Right)
    }
}

/// How far the decoder got with one signal.
#[derive(Debug, Clone)]
pub struct Diagnosis {
    /// The nearest sync-search candidate: its rank among the candidates
    /// (0 the strongest) and its sync score. `None` if there is none within
    /// a tone and a quarter symbol of the truth.
    pub candidate: Option<(usize, f32)>,
    /// The best sync score anywhere within a tone and a quarter symbol of
    /// the truth, candidate or not.
    pub sync_near: f32,
    /// Lock error in frequency (hertz) and timing (baseband samples, 1/32
    /// symbol), when there was a candidate.
    pub lock_error: Option<(f64, isize)>,
    /// From the decoder's own lock, when there was a candidate.
    pub actual: Option<Trial>,
    /// From the true frequency and timing.
    pub oracle: Trial,
}

/// Diagnoses every signal of one recording.
#[must_use]
pub fn diagnose(audio: &[f32], truths: &[Truth], options: &Options) -> Vec<Diagnosis> {
    let meter = ToneMeter::new();
    let spectrogram = Spectrogram::new(audio, options.high_hz + 8.0 * TONE_SPACING_HZ);
    let baseband = Baseband::new(audio);
    let found = candidates(&spectrogram, options);
    truths
        .iter()
        .map(|truth| {
            let audio_start =
                (NOMINAL_START_SECONDS + truth.dt_s) * f64::from(SAMPLE_RATE) / to_f64(STEP);
            let near = |row: usize, column: isize| {
                (row_to_hz(row) - truth.base_hz).abs() <= TONE_SPACING_HZ
                    && (to_f64_signed(column) - audio_start).abs() <= 1.01
            };
            let candidate = found
                .iter()
                .enumerate()
                .filter(|(_, c)| near(c.row, c.column))
                .min_by(|a, b| {
                    let d = |c: &Candidate| {
                        (row_to_hz(c.row) - truth.base_hz).abs() / TONE_SPACING_HZ
                            + (to_f64_signed(c.column) - audio_start).abs()
                    };
                    d(a.1).total_cmp(&d(b.1))
                });
            let sync_near = sync_near(&spectrogram, truth, audio_start);
            let (lock_error, actual) = match candidate {
                Some((_, &c)) => {
                    let locked = lock(&baseband, &meter, c).swap_remove(0);
                    (
                        Some((locked.base_hz - truth.base_hz, locked.start - truth.start())),
                        Some(trial(&locked, truth, options)),
                    )
                }
                None => (None, None),
            };
            Diagnosis {
                candidate: candidate.map(|(rank, c)| (rank, c.sync)),
                sync_near,
                lock_error,
                actual,
                oracle: trial(&oracle_lock(&baseband, &meter, truth), truth, options),
            }
        })
        .collect()
}

/// The best sync score within a tone and a quarter symbol of the truth.
fn sync_near(spectrogram: &Spectrogram, truth: &Truth, audio_start: f64) -> f32 {
    let row = super::hz_to_row(truth.base_hz);
    #[allow(clippy::cast_possible_truncation)] // a few hundred
    let column = audio_start.round() as isize;
    let mut best = 0.0_f32;
    for r in row.saturating_sub(2)..=row + 2 {
        for c in column - 1..=column + 1 {
            best = best.max(super::sync_score(spectrogram, c, r));
        }
    }
    best
}

/// The lock an oracle would give: the true frequency and timing.
fn oracle_lock(baseband: &Baseband, meter: &ToneMeter, truth: &Truth) -> Locked {
    let slice = baseband.slice(truth.base_hz);
    let start = truth.start();
    let sps = to_isize(baseband::SAMPLES_PER_SYMBOL);
    Locked {
        start,
        base_hz: truth.base_hz,
        symbols: (0..SYMBOLS)
            .map(|k| meter.tones(&slice, start + sps * to_isize(k)))
            .collect(),
    }
}

/// The decoder's demodulation and error correction from one lock, scored
/// against the truth.
fn trial(locked: &Locked, truth: &Truth, options: &Options) -> Trial {
    let data: Vec<[Complex32; 8]> = data_symbols().map(|k| locked.symbols[k]).collect();
    let noise = noise_power(&data);
    let symbol_errors = data_symbols()
        .zip(&data)
        .filter(|(k, tones)| strongest(tones) != usize::from(truth.tones[*k]))
        .count();
    let judge = |found: Option<[bool; ldpc::N]>| match found {
        Some(bits) if bits == truth.bits => Outcome::Right,
        Some(_) => Outcome::Wrong,
        None => Outcome::Nothing,
    };
    let mut bit_errors = Vec::new();
    let mut bp = Vec::new();
    let mut osd = vec![Outcome::Nothing; OSD_GROUPS.len()];
    for group in GROUPS {
        let llr = likelihoods(locked, &data, group, noise);
        bit_errors.push(
            llr.iter()
                .zip(&truth.bits)
                .filter(|(l, b)| (**l < 0.0) != **b)
                .count(),
        );
        bp.push(judge(
            ldpc::decode_soft(&llr, options.ldpc_iterations)
                .0
                .map(|(b, _)| b),
        ));
        if let Some(i) = OSD_GROUPS.iter().position(|&g| g == group) {
            osd[i] = judge(
                ldpc::osd(&llr, options.osd_pairs, options.osd_triples)
                    .filter(|(_, d, _)| *d <= OSD_MAX_DISTANCE)
                    .map(|(b, _, _)| b),
            );
        }
    }
    let thirds: Vec<usize> = vec![0, 19, 39, 58];
    let coherence_thirds = std::array::from_fn(|t| {
        // Coherence over one third: the other symbols zeroed would bias
        // the noise estimate, so score the third alone with the whole
        // transmission's noise.
        let part: Vec<[Complex32; 8]> = data[thirds[t]..thirds[t + 1]].to_vec();
        let tones: Vec<usize> = data_symbols()
            .skip(thirds[t])
            .take(thirds[t + 1] - thirds[t])
            .map(|k| usize::from(truth.tones[k]))
            .collect();
        span_coherence(&part, &tones, noise)
    });
    Trial {
        symbol_errors,
        bit_errors,
        bp,
        osd,
        coherence: coherence(&data, &truth.bits, noise),
        coherence_thirds,
    }
}

/// The coherence score of a run of symbols on their own.
fn span_coherence(symbols: &[[Complex32; 8]], tones: &[usize], noise: f32) -> f32 {
    let span = super::COHERENCE_SPAN;
    let mut total = 0.0_f32;
    let mut runs = 0.0_f32;
    for first in 0..=symbols.len().saturating_sub(span) {
        let sum: Complex32 = (first..first + span).map(|k| symbols[k][tones[k]]).sum();
        total += sum.norm_sqr();
        runs += 1.0;
    }
    if noise > 0.0 && runs > 0.0 {
        total / (noise * runs)
    } else {
        0.0
    }
}

fn strongest(tones: &[Complex32; 8]) -> usize {
    tones
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.norm_sqr().total_cmp(&b.1.norm_sqr()))
        .map_or(0, |(i, _)| i)
}

/// The column names of [`Diagnosis::to_tsv`].
#[must_use]
pub fn tsv_header() -> String {
    let mut out = String::from("rank\tsync\tsync_near\tdf_hz\tdt_samples");
    for who in ["act", "orc"] {
        let _ = write!(out, "\t{who}_symerr");
        for g in GROUPS {
            let _ = write!(out, "\t{who}_biterr{g}");
        }
        let _ = write!(out, "\t{who}_bp\t{who}_osd\t{who}_coh\t{who}_coh3");
    }
    out
}

impl Diagnosis {
    /// One tab-separated line, in the columns of [`tsv_header`]. The BP
    /// and OSD columns are one mark per group (`R` right, `W` wrong, `-`
    /// nothing), in [`GROUPS`] and [`OSD_GROUPS`] order.
    #[must_use]
    pub fn to_tsv(&self) -> String {
        let mut out = match self.candidate {
            Some((rank, sync)) => format!("{rank}\t{sync:.2}"),
            None => "-\t-".to_owned(),
        };
        let _ = write!(out, "\t{:.2}", self.sync_near);
        match self.lock_error {
            Some((hz, samples)) => {
                let _ = write!(out, "\t{hz:.3}\t{samples}");
            }
            None => out.push_str("\t-\t-"),
        }
        for trial in [self.actual.as_ref(), Some(&self.oracle)] {
            let Some(t) = trial else {
                out.push_str("\t-");
                for _ in GROUPS {
                    out.push_str("\t-");
                }
                out.push_str("\t-\t-\t-\t-");
                continue;
            };
            let _ = write!(out, "\t{}", t.symbol_errors);
            for e in &t.bit_errors {
                let _ = write!(out, "\t{e}");
            }
            let marks = |o: &[Outcome]| o.iter().map(|o| o.mark()).collect::<String>();
            let _ = write!(
                out,
                "\t{}\t{}\t{:.1}\t{:.0}/{:.0}/{:.0}",
                marks(&t.bp),
                marks(&t.osd),
                t.coherence,
                t.coherence_thirds[0],
                t.coherence_thirds[1],
                t.coherence_thirds[2]
            );
        }
        out
    }
}

#[allow(clippy::cast_precision_loss)] // small
fn to_f64(n: usize) -> f64 {
    n as f64
}

#[allow(clippy::cast_precision_loss)] // small
fn to_f64_signed(n: isize) -> f64 {
    n as f64
}
