// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LogLab's FT8 decoder: 15 seconds of audio in, the messages in it out.
//!
//! An original design following the published description of FT8 ("The FT4
//! and FT8 Communication Protocols", QEX 2020), judged only by its score on
//! the bench against WSJT-X (see `tools/ft8-reference/`).
//!
//! 1. **Spectrogram.** One-symbol FFTs every quarter symbol, at twice the
//!    tone spacing's resolution, so every signal lines up with a column and
//!    row of the grid to within an eighth of a symbol and a quarter tone.
//! 2. **Sync search.** Every frequency and start time is scored by how much
//!    of its power sits on the three Costas arrays; the best-scoring local
//!    peaks become candidates.
//! 3. **Lock.** Each candidate's slice of the band is moved to baseband,
//!    where its start time is tuned to 1/32 of a symbol and its frequency
//!    to a quarter hertz by maximizing the power on the signal: the Costas
//!    tones and the strongest tone of each data symbol. Then the phases of
//!    the Costas tones settle the frequency to a few hundredths of a hertz,
//!    any steady drift of it, and the timing to a sample, by making the
//!    tones of each Costas array add up in phase. If the best lock does not
//!    decode, its nearest neighbours in frequency are tried too.
//! 4. **Soft demodulation.** The complex tone amplitudes of each data symbol
//!    become a likelihood for each bit, first symbol by symbol and then
//!    three, two and four symbols at a time, adding the tones coherently
//!    (FT8's phase runs on unbroken from symbol to symbol).
//! 5. **Error correction.** The LDPC code's belief propagation turns the
//!    likelihoods into a codeword; the CRC confirms it. Signals it decodes
//!    are removed (step 6) before the slower, deeper ordered-statistics
//!    decoding is tried on the rest, so the leftovers of a strong signal
//!    cannot be mistaken for another one. Every codeword, from either, must
//!    also be borne out by the signal's phase ([`validates`]): the tones it
//!    says were sent have to hold their phase from each symbol to the next
//!    along the transmission, which the tones noise happens to favour never
//!    do. How steady the signal's phase was (its [`steadiness`]) decides
//!    which likelihoods the deeper decoding works on, since adding symbols
//!    coherently stops helping when fading scrambles the phase; and a deep
//!    decode is believed only when it beats its runner-up clearly and the
//!    signal arrived evenly, or two likelihood sets agree on it.
//! 6. **Subtraction.** Each decoded signal is rebuilt and removed from the
//!    audio itself, and the search runs again, so signals hidden under it
//!    can be found.
//!
//! Every decode here is *received* in the sense of Principle XX: all of it
//! came from the signal and passed the parity checks and the CRC. Nothing
//! is assumed about what a message probably says.

use std::collections::BTreeMap;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::baseband::{self, Baseband, ToneMeter};
use super::encode::{self, COSTAS, COSTAS_POSITIONS, Tones};
use super::ldpc;
use super::message::{self, Payload};
use super::subtract;
use super::{NOMINAL_START_SECONDS, SAMPLE_RATE, SAMPLES_PER_SYMBOL, SYMBOLS, TONE_SPACING_HZ};

pub mod diagnose;

/// Spectrogram columns per symbol.
const TIME_STEPS_PER_SYMBOL: usize = 4;
/// Spectrogram rows per tone spacing.
const BINS_PER_TONE: usize = 2;
/// Samples between spectrogram columns.
const STEP: usize = SAMPLES_PER_SYMBOL / TIME_STEPS_PER_SYMBOL;
/// FFT length: one symbol, zero-padded to twice its length.
const NFFT: usize = SAMPLES_PER_SYMBOL * BINS_PER_TONE;
/// Width of one spectrogram row, in hertz.
const BIN_HZ: f64 = TONE_SPACING_HZ / 2.0; // BINS_PER_TONE
/// Gray decoding: the three codeword bits each tone carries.
const TONE_BITS: [u8; 8] = [0, 1, 3, 2, 6, 4, 5, 7];
/// Ratio of FT8's 2,500 Hz reference bandwidth to one tone's 6.25 Hz.
const REFERENCE_BANDWIDTH_DB: f64 = 26.020_6;

/// How hard the decoder looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Lowest base frequency searched, in hertz.
    pub low_hz: f64,
    /// Highest base frequency searched, in hertz.
    pub high_hz: f64,
    /// Most candidates tried per pass.
    pub max_candidates: usize,
    /// Smallest sync score a candidate needs (1.0 is pure noise).
    pub min_sync: f32,
    /// Most belief-propagation rounds per candidate.
    pub ldpc_iterations: usize,
    /// Search passes; after each, decoded signals are subtracted.
    pub passes: usize,
    /// Threads to decode candidates on.
    pub threads: usize,
    /// How many of the least reliable bits ordered-statistics decoding tries
    /// in pairs when belief propagation fails; 0 turns it off.
    pub osd_pairs: usize,
    /// How many of the least reliable bits it tries in triples.
    pub osd_triples: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            low_hz: 100.0,
            high_hz: 3_000.0,
            max_candidates: 400,
            min_sync: 1.4,
            ldpc_iterations: 40,
            passes: 3,
            threads: std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(8),
            osd_pairs: 91,
            osd_triples: 25,
        }
    }
}

/// One decoded message.
#[derive(Debug, Clone, PartialEq)]
pub struct Decode {
    /// The message text.
    pub message: String,
    /// Estimated signal-to-noise ratio in 2,500 Hz, in decibels.
    pub snr_db: f64,
    /// Start time relative to the nominal 0.5 s into the slot, in seconds.
    pub dt_s: f64,
    /// Frequency of the lowest tone, in hertz.
    pub base_hz: f64,
}

impl Decode {
    /// The decode as one line in the layout WSJT-X's `jt9` prints, so the
    /// bench scores both decoders the same way.
    #[must_use]
    pub fn to_line(&self) -> String {
        format!(
            "000000 {:3.0} {:4.1} {:4.0} ~  {}",
            self.snr_db, self.dt_s, self.base_hz, self.message
        )
    }
}

/// Power spectrogram: `power[column][row]`.
struct Spectrogram {
    power: Vec<Vec<f32>>,
    rows: usize,
}

impl Spectrogram {
    fn new(audio: &[f32], max_hz: f64) -> Self {
        let fft: Arc<dyn Fft<f32>> = FftPlanner::new().plan_fft_forward(NFFT);
        let rows = hz_to_row(max_hz).min(NFFT / 2);
        let columns = audio.len().saturating_sub(SAMPLES_PER_SYMBOL) / STEP + 1;
        let mut buffer = vec![Complex32::new(0.0, 0.0); NFFT];
        let power = (0..columns)
            .map(|c| {
                let start = c * STEP;
                for (b, i) in buffer.iter_mut().zip(0..) {
                    let sample = if i < SAMPLES_PER_SYMBOL {
                        audio.get(start + i).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    *b = Complex32::new(sample, 0.0);
                }
                fft.process(&mut buffer);
                buffer[..rows].iter().map(Complex32::norm_sqr).collect()
            })
            .collect();
        Self { power, rows }
    }

    /// The eight tone powers of symbol `symbol` of a signal starting at
    /// `column` with its lowest tone on `row`, or `None` outside the
    /// recording.
    fn tones(&self, column: isize, row: usize, symbol: usize) -> Option<[f32; 8]> {
        let c = column + to_isize(symbol * TIME_STEPS_PER_SYMBOL);
        let col = self.power.get(usize::try_from(c).ok()?)?;
        let mut out = [0.0; 8];
        for (t, o) in out.iter_mut().enumerate() {
            *o = *col.get(row + BINS_PER_TONE * t)?;
        }
        Some(out)
    }
}

/// A place a signal may start.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    column: isize,
    row: usize,
    sync: f32,
}

/// Decodes one 15-second slot of 12,000-sample-per-second audio.
#[must_use]
pub fn decode_slot(audio: &[f32], options: &Options) -> Vec<Decode> {
    let meter = ToneMeter::new();
    let mut residual = audio.to_vec();
    let mut found: BTreeMap<String, Decode> = BTreeMap::new();
    let mut tried = std::collections::BTreeSet::new();
    for _ in 0..options.passes {
        let spectrogram = Spectrogram::new(&residual, options.high_hz + 8.0 * TONE_SPACING_HZ);
        let baseband = Baseband::new(&residual);
        // A later pass only retries a place if subtraction changed it
        // enough to move the peak.
        let fresh: Vec<Candidate> = candidates(&spectrogram, options)
            .into_iter()
            .filter(|c| tried.insert((c.column, c.row)))
            .collect();

        // First belief propagation alone, at the best lock and its
        // neighbours.
        let quick = in_parallel(&fresh, options.threads, |&candidate| {
            let mut locks = lock(&baseband, &meter, candidate);
            match locks
                .iter()
                .position(|l| demodulate(l, options, false).is_some())
            {
                Some(i) => Ok(locks.swap_remove(i)),
                None => Err(locks),
            }
        });
        let mut decoded_hz = Vec::new();
        let mut failed = Vec::new();
        for (candidate, outcome) in fresh.into_iter().zip(quick) {
            match outcome {
                Ok(locked) => {
                    if let Some(hz) = record(&mut found, &mut residual, &locked, options, false) {
                        decoded_hz.push(hz);
                    }
                }
                Err(locked) => failed.push((candidate, locked)),
            }
        }

        // Then the slower, riskier ordered-statistics decoding, on audio
        // with those signals already removed, so their leftovers cannot be
        // mistaken for other signals. Candidates near a removed signal are
        // locked again.
        let baseband = if decoded_hz.is_empty() {
            baseband
        } else {
            Baseband::new(&residual)
        };
        let near = |candidate: &Candidate| {
            let hz = row_to_hz(candidate.row);
            decoded_hz.iter().any(|&d| (d - hz).abs() < NEAR_HZ)
        };
        let deep = in_parallel(&failed, options.threads, |(candidate, locks)| {
            let locks = if near(candidate) {
                lock(&baseband, &meter, *candidate)
            } else {
                locks.clone()
            };
            locks
                .into_iter()
                .find(|locked| demodulate(locked, options, true).is_some())
        });
        let mut any = !decoded_hz.is_empty();
        for locked in deep.into_iter().flatten() {
            any |= record(&mut found, &mut residual, &locked, options, true).is_some();
        }
        if !any {
            break;
        }
    }
    let mut decodes: Vec<Decode> = found.into_values().collect();
    decodes.sort_by(|a, b| a.base_hz.total_cmp(&b.base_hz));
    decodes
}

/// Candidates whose frequency is within this of a signal just removed are
/// locked again before deep decoding, in hertz: a signal and its
/// neighbourhood are 50 Hz wide.
const NEAR_HZ: f64 = 75.0;

/// Adds the decode at `locked` (demodulating it again, which is cheap and
/// gives the same answer) unless it is already known, and removes its
/// signal from `residual`. Returns its frequency if it was new.
fn record(
    found: &mut BTreeMap<String, Decode>,
    residual: &mut [f32],
    locked: &Locked,
    options: &Options,
    deep: bool,
) -> Option<f64> {
    let (payload, tones) = demodulate(locked, options, deep)?;
    let text = message::unpack(payload)?;
    if found.contains_key(&text) {
        return None;
    }
    found.insert(
        text.clone(),
        Decode {
            message: text,
            snr_db: estimate_snr(locked, &tones),
            dt_s: start_to_dt(locked.start),
            base_hz: (locked.base_hz * 10.0).round() / 10.0,
        },
    );
    let audio_start = locked.start * to_isize(SAMPLE_RATE as usize / 200);
    subtract::subtract(residual, &tones, locked.base_hz, audio_start);
    Some(locked.base_hz)
}

/// `work` applied to every item, spread over up to `threads` threads; the
/// results come back in the items' order, so decoding is the same however
/// many threads run it.
fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let threads = threads.clamp(1, items.len().max(1));
    if threads == 1 {
        return items.iter().map(&work).collect();
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&work).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// The best local peaks of the sync score.
fn candidates(spectrogram: &Spectrogram, options: &Options) -> Vec<Candidate> {
    let low = hz_to_row(options.low_hz);
    let high =
        hz_to_row(options.high_hz).min(spectrogram.rows.saturating_sub(BINS_PER_TONE * 7 + 1));
    // Starts from 2 s early to 2.5 s late, in spectrogram columns.
    let nominal = to_isize(NOMINAL_START_SECONDS_SAMPLES / STEP);
    let early = to_isize(2 * SAMPLE_RATE as usize / STEP);
    let late = to_isize(5 * SAMPLE_RATE as usize / (2 * STEP));
    let columns: Vec<isize> = (nominal - early..=nominal + late).collect();
    if low >= high {
        return Vec::new();
    }
    let scores: Vec<Vec<f32>> = columns
        .iter()
        .map(|&c| (low..high).map(|r| sync_score(spectrogram, c, r)).collect())
        .collect();
    let mut peaks = Vec::new();
    for (ci, row_scores) in scores.iter().enumerate() {
        for (ri, &score) in row_scores.iter().enumerate() {
            if score < options.min_sync {
                continue;
            }
            let is_peak = (ci.saturating_sub(1)..=(ci + 1).min(scores.len() - 1)).all(|cj| {
                (ri.saturating_sub(1)..=(ri + 1).min(row_scores.len() - 1))
                    .all(|rj| (cj, rj) == (ci, ri) || scores[cj][rj] <= score)
            });
            if is_peak {
                peaks.push(Candidate {
                    column: columns[ci],
                    row: low + ri,
                    sync: score,
                });
            }
        }
    }
    peaks.sort_by(|a, b| b.sync.total_cmp(&a.sync));
    peaks.truncate(options.max_candidates);
    peaks
}

/// Samples from the start of the slot to the nominal start of a signal.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 6,000
const NOMINAL_START_SECONDS_SAMPLES: usize = (NOMINAL_START_SECONDS * SAMPLE_RATE as f64) as usize;

/// Power on the Costas tones over the average power of all eight tones.
fn sync_score(spectrogram: &Spectrogram, column: isize, row: usize) -> f32 {
    let mut on = 0.0;
    let mut all = 0.0;
    for start in COSTAS_POSITIONS {
        for (k, &tone) in COSTAS.iter().enumerate() {
            if let Some(powers) = spectrogram.tones(column, row, start + k) {
                on += powers[usize::from(tone)];
                all += powers.iter().sum::<f32>() / 8.0;
            }
        }
    }
    if all > 0.0 { on / all } else { 0.0 }
}

/// A candidate after fine timing and frequency: its 79 symbols' tones.
#[derive(Clone)]
struct Locked {
    /// Baseband sample where symbol 0 starts (audio sample `60 start`).
    start: isize,
    /// Frequency of the lowest tone, in hertz.
    base_hz: f64,
    /// Complex amplitudes of the eight tones of every symbol.
    symbols: Vec<[Complex32; 8]>,
}

/// Coarse frequency steps tried, in hertz either side of the candidate; the
/// best is then refined a quarter hertz either way.
const FINE_HZ: [f32; 7] = [-1.5, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5];

/// Baseband samples either side of the candidate's start that are tried.
const FINE_SAMPLES: isize = 6;

/// Reach and step, in hertz, of the first coherent frequency refinement,
/// and of the second after the timing has been settled. Both search for
/// drift too; see [`DRIFT_HZ_PER_S`].
const COHERENT_HZ: (f32, f32) = (0.6, 0.02);
const COHERENT_HZ_AGAIN: (f32, f32) = (0.3, 0.01);

/// Baseband samples either side of the start tried by the coherent timing
/// refinement.
const COHERENT_SAMPLES: isize = 3;

/// Offsets from the best lock, in hertz and baseband samples, also tried
/// when it fails to decode: the coherent lock's timing is reliable, but
/// its frequency can still be off by a few hundredths of a hertz, which
/// the four-symbol likelihoods feel.
const NEIGHBOURS: [(f32, isize); 2] = [(-0.1, 0), (0.1, 0)];

/// Tunes a candidate's start and frequency to put the most power on the
/// signal, in two stages.
///
/// First, noncoherently: the power on its Costas tones plus the strongest
/// tone of every data symbol, over coarse steps. Then coherently, from the
/// phases of the Costas tones: the frequency offset that makes each Costas
/// array add up in phase, the start that gives the largest in-phase sum,
/// and the offset once more. The coherent stage settles the frequency to a
/// few hundredths of a hertz and the timing to one baseband sample, which
/// the multi-symbol likelihoods of [`metrics`] need: a quarter-hertz error
/// turns the phase by a third of a cycle across three symbols.
///
/// Returns the best lock first, then its [`NEIGHBOURS`].
fn lock(baseband: &Baseband, meter: &ToneMeter, candidate: Candidate) -> Vec<Locked> {
    let coarse_hz = row_to_hz(candidate.row);
    let slice = baseband.slice(coarse_hz);
    let coarse_start = candidate.column * to_isize(STEP / (SAMPLE_RATE as usize / 200));
    let sps = to_isize(baseband::SAMPLES_PER_SYMBOL);
    let costas_tone = |k: usize| -> Option<usize> {
        COSTAS_POSITIONS
            .iter()
            .find(|&&c| (c..c + 7).contains(&k))
            .map(|&c| usize::from(COSTAS[k - c]))
    };
    // The power on the signal for every start from `around - reach` to
    // `around + reach`: its Costas tones, plus the strongest tone of every
    // data symbol. The data symbols' tones are unknown, but the strongest
    // of each is usually right, and there are nearly three times as many
    // of them: at low signal levels this steadies the lock considerably.
    // Each symbol's tones are measured once and then slid a sample at a
    // time.
    let powers = |shifted: &[Complex32], around: isize, reach: isize| -> Vec<f32> {
        let starts = usize::try_from(2 * reach + 1).unwrap_or(1);
        let mut totals = vec![0.0_f32; starts];
        for k in 0..SYMBOLS {
            let first = around - reach + sps * to_isize(k);
            let mut tones = meter.tones(shifted, first);
            let costas = costas_tone(k);
            for (j, total) in totals.iter_mut().enumerate() {
                if j > 0 {
                    meter.slide(&mut tones, shifted, first + to_isize(j) - 1);
                }
                *total += match costas {
                    Some(tone) => tones[tone].norm_sqr(),
                    None => tones.iter().map(Complex32::norm_sqr).fold(0.0, f32::max),
                };
            }
        }
        totals
    };
    let best_start = |shifted: &[Complex32], around: isize, reach: isize| -> (f32, isize) {
        powers(shifted, around, reach)
            .into_iter()
            .zip(around - reach..)
            .fold(
                (f32::NEG_INFINITY, around),
                |a, b| if b.0 > a.0 { b } else { a },
            )
    };
    let mut best = (f32::NEG_INFINITY, 0.0_f32, coarse_start);
    for &hz in &FINE_HZ {
        let (power, start) = best_start(&baseband::shift(&slice, hz), coarse_start, FINE_SAMPLES);
        if power > best.0 {
            best = (power, hz, start);
        }
    }
    for hz in [best.1 - 0.25, best.1 + 0.25] {
        let (power, start) = best_start(&baseband::shift(&slice, hz), best.2, 2);
        if power > best.0 {
            best = (power, hz, start);
        }
    }
    let (_, mut hz, mut start) = best;

    // Coherent refinement from the Costas tones' phases: the frequency
    // offset and steady drift that make each Costas array add up in phase
    // at its own time, then the start that gives the largest in-phase sum,
    // then offset and drift once more, finely. The drift is reckoned from
    // the middle of the transmission, so `hz` is the frequency there.
    let measure = |shifted: &[Complex32], start: isize| -> Vec<[Complex32; 8]> {
        (0..SYMBOLS)
            .map(|k| meter.tones(shifted, start + sps * to_isize(k)))
            .collect()
    };
    let middle = start + sps * to_isize(SYMBOLS / 2) + sps / 2;
    let mut drift = 0.0_f32;
    let mut symbols = measure(&baseband::shift(&slice, hz), start);
    let (offset, slope) = costas_offset_and_drift(&symbols, COHERENT_HZ, DRIFT_HZ_PER_S);
    hz += offset;
    drift += slope;
    let shifted = baseband::chirp(&slice, hz, drift, middle);
    (start, symbols) = (start - COHERENT_SAMPLES..=start + COHERENT_SAMPLES)
        .map(|s| (s, measure(&shifted, s)))
        .max_by(|a, b| costas_power(&a.1).total_cmp(&costas_power(&b.1)))
        .unwrap_or((start, symbols));
    let (offset, slope) =
        costas_offset_and_drift(&symbols, COHERENT_HZ_AGAIN, DRIFT_HZ_PER_S_AGAIN);
    hz += offset;
    drift += slope;

    std::iter::once((0.0, 0))
        .chain(NEIGHBOURS)
        .map(|(dhz, dstart)| {
            let shifted = baseband::chirp(&slice, hz + dhz, drift, middle);
            let start = start + dstart;
            Locked {
                start,
                base_hz: coarse_hz + f64::from(hz + dhz),
                symbols: measure(&shifted, start),
            }
        })
        .collect()
}

/// Reach and step, in hertz per second, of the drift search alongside
/// [`COHERENT_HZ`], and of the finer one alongside [`COHERENT_HZ_AGAIN`].
/// Half a hertz per second moves a signal six hertz, a whole tone, within
/// one transmission; nothing that drifts faster is worth chasing.
const DRIFT_HZ_PER_S: (f32, f32) = (0.5, 0.02);
const DRIFT_HZ_PER_S_AGAIN: (f32, f32) = (0.1, 0.005);

/// Time from the middle of the transmission to the middle of each Costas
/// array, in seconds: the first is 5.76 s early, the last 5.76 s late.
fn costas_times() -> [f32; 3] {
    let seconds_per_symbol = to_f32(baseband::SAMPLES_PER_SYMBOL) / baseband::RATE;
    let middle = to_f32(SYMBOLS) / 2.0;
    std::array::from_fn(|a| (to_f32(COSTAS_POSITIONS[a]) + 3.5 - middle) * seconds_per_symbol)
}

/// The frequency offset (within `reach` hertz, in steps of `step`) and the
/// steady drift (within `drift_reach` hertz per second, in steps of
/// `drift_step`) that together make the Costas tones add up most nearly in
/// phase: each Costas array is added in phase at the offset plus the drift
/// times its own time from the middle of the transmission. A frequency
/// error turns the tones' phases steadily from symbol to symbol; a drift
/// makes that turn differ from one array to the next.
fn costas_offset_and_drift(
    symbols: &[[Complex32; 8]],
    (reach, step): (f32, f32),
    (drift_reach, drift_step): (f32, f32),
) -> (f32, f32) {
    let seconds_per_symbol = to_f32(baseband::SAMPLES_PER_SYMBOL) / baseband::RATE;
    let times = costas_times();
    let farthest = times.iter().fold(0.0_f32, |m, t| m.max(t.abs()));
    // Each array's in-phase power over a grid of frequencies wide enough
    // for any offset and drift in reach.
    let grid_reach = reach + drift_reach * farthest;
    let grid_steps = round_to_usize(grid_reach / step);
    let grid: Vec<Vec<f32>> = COSTAS_POSITIONS
        .iter()
        .map(|&first| {
            (0..=2 * grid_steps)
                .map(|i| {
                    let f = (to_f32(i) - to_f32(grid_steps)) * step;
                    COSTAS
                        .iter()
                        .enumerate()
                        .map(|(j, &tone)| {
                            let angle =
                                -2.0 * std::f32::consts::PI * f * seconds_per_symbol * to_f32(j);
                            symbols[first + j][usize::from(tone)]
                                * Complex32::from_polar(1.0, angle)
                        })
                        .sum::<Complex32>()
                        .norm_sqr()
                })
                .collect()
        })
        .collect();
    let at = |a: usize, f: f32| -> f32 {
        let i = round_to_usize(f / step + to_f32(grid_steps));
        grid[a].get(i).copied().unwrap_or(0.0)
    };
    let steps = round_to_usize(reach / step);
    let drift_steps = round_to_usize(drift_reach / drift_step);
    let mut best = (f32::NEG_INFINITY, 0.0_f32, 0.0_f32);
    for d in 0..=2 * drift_steps {
        let drift = (to_f32(d) - to_f32(drift_steps)) * drift_step;
        for i in 0..=2 * steps {
            let f = (to_f32(i) - to_f32(steps)) * step;
            let power: f32 = times
                .iter()
                .enumerate()
                .map(|(a, t)| at(a, f + drift * t))
                .sum();
            if power > best.0 {
                best = (power, f, drift);
            }
        }
    }
    (best.1, best.2)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small, non-negative
fn round_to_usize(x: f32) -> usize {
    x.round().max(0.0) as usize
}

/// The sum of the Costas tones' amplitudes of each Costas array, added in
/// phase, as a power; largest when the frequency and timing are right.
fn costas_power(symbols: &[[Complex32; 8]]) -> f32 {
    COSTAS_POSITIONS
        .iter()
        .map(|&start| {
            COSTAS
                .iter()
                .enumerate()
                .map(|(j, &tone)| symbols[start + j][usize::from(tone)])
                .sum::<Complex32>()
                .norm_sqr()
        })
        .sum()
}

/// How steady the signal's phase is from symbol to symbol: the in-phase
/// power of each Costas array's seven tones over the sum of their powers,
/// summed over the arrays and divided by seven, so a steady signal reads
/// near 1 and one whose phase is unrelated from symbol to symbol (fast
/// fading, or noise) reads near 1/7.
fn steadiness(symbols: &[[Complex32; 8]]) -> f32 {
    let total: f32 = COSTAS_POSITIONS
        .iter()
        .flat_map(|&start| {
            COSTAS
                .iter()
                .enumerate()
                .map(move |(j, &tone)| symbols[start + j][usize::from(tone)].norm_sqr())
        })
        .sum();
    if total > 0.0 {
        costas_power(symbols) / (7.0 * total)
    } else {
        0.0
    }
}

/// Each Costas array's in-phase power, in units of the noise power.
fn array_powers(symbols: &[[Complex32; 8]], noise: f32) -> [f32; 3] {
    std::array::from_fn(|a| {
        let start = COSTAS_POSITIONS[a];
        let sum: Complex32 = COSTAS
            .iter()
            .enumerate()
            .map(|(j, &tone)| symbols[start + j][usize::from(tone)])
            .sum();
        if noise > 0.0 {
            sum.norm_sqr() / noise
        } else {
            0.0
        }
    })
}

/// Indexes of the 58 data symbols, in order.
fn data_symbols() -> impl Iterator<Item = usize> {
    (0..SYMBOLS).filter(|s| !COSTAS_POSITIONS.iter().any(|&c| (c..c + 7).contains(s)))
}

/// Tries the soft-decision metrics in turn until one decodes. With `deep`,
/// ordered-statistics decoding follows when belief propagation fails.
fn demodulate(locked: &Locked, options: &Options, deep: bool) -> Option<(Payload, Tones)> {
    let data: Vec<[Complex32; 8]> = data_symbols().map(|k| locked.symbols[k]).collect();
    let noise = noise_power(&data);
    let steady = steadiness(&locked.symbols);
    let osd_groups = osd_groups(steady);
    let evenness = evenness(&locked.symbols, noise);
    // Likelihood sets for ordered-statistics decoding, each with the
    // largest disagreement accepted from it.
    let mut fallbacks = Vec::new();
    for group in GROUPS {
        let llr = likelihoods(&data, group, noise);
        let (decoded, _) = ldpc::decode_soft(&llr, options.ldpc_iterations);
        if let Some(found) = decoded
            .filter(|(bits, _)| validates(&data, bits, noise))
            .and_then(|(bits, _)| accept(&bits))
        {
            return Some(found);
        }
        if osd_groups.contains(&group) {
            fallbacks.push(llr);
        }
    }
    if !deep || options.osd_pairs == 0 {
        return None;
    }
    // Ordered statistics on every fallback; a codeword found from more
    // than one likelihood set is the more believable for it.
    let results: Vec<([bool; ldpc::N], f32, f32)> = fallbacks
        .iter()
        .filter_map(|llr| ldpc::osd(llr, options.osd_pairs, options.osd_triples))
        .collect();
    results.iter().find_map(|(bits, distance, margin)| {
        let agreeing = results.iter().filter(|r| r.0 == *bits).count();
        if *distance > OSD_MAX_DISTANCE
            || *margin < OSD_MIN_MARGIN
            || (agreeing < 2 && evenness < OSD_MIN_EVENNESS)
            || !validates(&data, bits, noise)
        {
            return None;
        }
        accept(bits).filter(|(payload, _)| plausible(*payload))
    })
}

/// The least an ordered-statistics codeword's margin over the runner-up
/// may be. A wrong codeword built from a partly faded transmission is
/// usually one of several near misses, and so beats the next by little;
/// on the clean bench every rescued codeword clears this.
const OSD_MIN_MARGIN: f32 = 1.0;

/// Unless two likelihood sets agree on it, an ordered-statistics codeword
/// is only believed when the signal was this even across the three Costas
/// arrays: a deep fade over part of the transmission is what lets ordered
/// statistics build a near miss of the true codeword, and it shows here as
/// one faint array. Clean signals read above 0.6; faded ones 0.2 to 0.3.
const OSD_MIN_EVENNESS: f32 = 0.4;

/// How evenly the signal arrived over the transmission: the faintest Costas
/// array's in-phase power over the mean of the three.
fn evenness(symbols: &[[Complex32; 8]], noise: f32) -> f32 {
    let powers = array_powers(symbols, noise);
    let mean = powers.iter().sum::<f32>() / 3.0;
    if mean > 0.0 {
        powers.iter().copied().fold(f32::INFINITY, f32::min) / mean
    } else {
        0.0
    }
}

/// Whether a codeword is borne out by the signal: its tones must hold
/// their phase from one symbol to the next, see [`differential_coherence`].
fn validates(data: &[[Complex32; 8]], bits: &[bool; ldpc::N], noise: f32) -> bool {
    let tones = encode::tones_for_codeword(bits);
    differential_coherence(data, &tones, noise) >= DIFFERENTIAL_COHERENCE_MIN
}

/// The least differential coherence of a codeword that is believed. Noise
/// never reached 1.6 in 480,000 tries on the bench; real decodes read 2.2
/// and up, however they drift or flutter.
const DIFFERENTIAL_COHERENCE_MIN: f32 = 2.0;

/// Differential coherence: the products of each decoded tone's amplitude
/// with the conjugate of the next symbol's, summed over the transmission,
/// in units of the noise power per pair. A real signal's products all
/// share the phase step between symbols, whatever slow drift or fading
/// does to the phase itself; noise's point every way.
fn differential_coherence(data: &[[Complex32; 8]], tones: &Tones, noise: f32) -> f32 {
    let data_tones: Vec<usize> = data_symbols().map(|k| usize::from(tones[k])).collect();
    let mut sum = Complex32::new(0.0, 0.0);
    let mut pairs = 0.0_f32;
    for (symbols, sent) in data.chunks(29).zip(data_tones.chunks(29)) {
        for k in 0..symbols.len() - 1 {
            sum += symbols[k][sent[k]] * symbols[k + 1][sent[k + 1]].conj();
            pairs += 1.0;
        }
    }
    if noise > 0.0 {
        sum.norm() / (noise * pairs)
    } else {
        0.0
    }
}

/// How many consecutive data symbols the likelihoods take together, in the
/// order tried: one symbol alone needs no phase coherence at all; three and
/// two gain about a decibel on a steady signal; four gains a little more on
/// a very steady one, and is tried last.
const GROUPS: [usize; 4] = [1, 3, 2, 4];

/// The largest group, which sizes the tables of tone combinations.
const MAX_GROUP: usize = 4;

/// Which likelihood sets ordered-statistics decoding works on, by how
/// steady the signal's phase was ([`steadiness`]). A steady signal is
/// served by the long coherent sums, which carry what the short ones carry
/// and more; as the phase wanders (fast fading) the long sums turn
/// counterproductive and the one- and two-symbol sets decode most.
fn osd_groups(steady: f32) -> &'static [usize] {
    if steady >= STEADY_PHASE {
        &[3, 4]
    } else if steady >= WANDERING_PHASE {
        &[2, 3, 4]
    } else {
        &[1, 2]
    }
}

/// Steadiness above which a signal counts as phase-steady (clean signals
/// read 0.84 to 0.98), and below which its phase counts as wandering
/// (noise reads about 0.36; a 2 Hz Doppler spread about 0.5).
const STEADY_PHASE: f32 = 0.8;
const WANDERING_PHASE: f32 = 0.5;

/// Largest disagreement an ordered-statistics decode may have and still be
/// accepted, tuned on the bench; [`validates`] is the stronger guard.
const OSD_MAX_DISTANCE: f32 = 60.0;

/// Extra caution for ordered-statistics decodes, which can land on a wrong
/// codeword whose CRC happens to pass (about one in 16,000 tries): the
/// rarely used `/R` suffix is not believed from this path.
fn plausible(payload: Payload) -> bool {
    message::unpack(payload).is_some_and(|text| !text.contains("/R"))
}

/// Average noise power in one tone of one symbol, estimated from the median
/// tone power (most tones carry only noise, and the median of an
/// exponential distribution is its mean times ln 2).
fn noise_power(data: &[[Complex32; 8]]) -> f32 {
    let mut powers: Vec<f32> = data
        .iter()
        .flat_map(|s| s.iter().map(Complex32::norm_sqr))
        .collect();
    powers.sort_by(f32::total_cmp);
    powers
        .get(powers.len() / 2)
        .map_or(1.0, |m| m / std::f32::consts::LN_2)
}

/// The bit likelihoods of `group`-symbol combining, scaled for belief
/// propagation.
fn likelihoods(data: &[[Complex32; 8]], group: usize, noise: f32) -> [f32; ldpc::N] {
    let mut llr = metrics(data, group, noise);
    normalize(&mut llr);
    llr
}

/// How sharply the tone amplitudes, measured in units of the noise, are
/// weighed against each other; tuned on the bench.
const METRIC_SCALE: f32 = 4.0;

/// Bit likelihoods from the data symbols, taking `group` consecutive symbols
/// together (within each half between Costas arrays). Every combination of
/// tones in the group is weighed by the strength of their coherent sum, and
/// a bit's likelihood compares the total weight of the combinations with
/// that bit 0 against those with it 1.
fn metrics(data: &[[Complex32; 8]], group: usize, noise: f32) -> [f32; ldpc::N] {
    let mut llr = [0.0_f32; ldpc::N];
    // Coherent sums of every tone combination, built a symbol at a time:
    // the sums over the first j symbols are the first 8^j entries.
    let mut sums = vec![Complex32::new(0.0, 0.0); 1 << (3 * MAX_GROUP)];
    for (half, symbols) in data.chunks(29).enumerate() {
        let mut first = 0;
        while first < symbols.len() {
            let size = group.min(symbols.len() - first);
            let combos = 1_usize << (3 * size);
            let spread = noise * to_f32(size);
            sums[0] = Complex32::new(0.0, 0.0);
            for j in 0..size {
                let done = 1_usize << (3 * j);
                for prefix in (0..done).rev() {
                    let partial = sums[prefix];
                    for (tone, amplitude) in symbols[first + j].iter().enumerate() {
                        sums[prefix << 3 | tone] = partial + amplitude;
                    }
                }
            }
            let weight = |sum: &Complex32| METRIC_SCALE * (sum.norm_sqr() / spread).sqrt();
            // Log-sum-exp, relative to the largest weight so nothing
            // overflows: each combination's weight is exponentiated once
            // and credited to the tone it has in every symbol; a bit's
            // likelihood then compares the tones carrying 0 with those
            // carrying 1.
            let peak = sums[..combos]
                .iter()
                .map(weight)
                .fold(f32::NEG_INFINITY, f32::max);
            let mut mass = [[0.0_f32; 8]; MAX_GROUP];
            for (combo, sum) in sums[..combos].iter().enumerate() {
                let e = (weight(sum) - peak).exp();
                for (j, m) in mass[..size].iter_mut().enumerate() {
                    m[(combo >> (3 * (size - 1 - j))) & 7] += e;
                }
            }
            for (j, m) in mass[..size].iter().enumerate() {
                for b in 0..3 {
                    let (mut zero, mut one) = (0.0_f32, 0.0_f32);
                    for (tone, &e) in m.iter().enumerate() {
                        if (TONE_BITS[tone] >> (2 - b)) & 1 == 0 {
                            zero += e;
                        } else {
                            one += e;
                        }
                    }
                    llr[3 * (29 * half + first + j) + b] =
                        zero.max(f32::MIN_POSITIVE).ln() - one.max(f32::MIN_POSITIVE).ln();
                }
            }
            first += size;
        }
    }
    llr
}

/// How many wrong codewords have reached the CRC check and failed it since
/// the process started: each passed the parity checks (or ordered-statistics
/// decoding, its acceptance rules and [`validates`]) and only the CRC
/// caught it. A wrong codeword passes the CRC about once in 16,000 times,
/// so this count divided by 16,383 is the number of false decodes to
/// expect; the bench reports it, since zero false decodes in a few hundred
/// recordings cannot tell a safe decoder from a lucky one.
#[must_use]
pub fn crc_misses() -> u64 {
    CRC_MISSES.load(std::sync::atomic::Ordering::Relaxed)
}

static CRC_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The payload of a codeword whose CRC is right.
fn accept(bits: &[bool; ldpc::N]) -> Option<(Payload, Tones)> {
    let payload_bits: [bool; 77] = bits[..77].try_into().ok()?;
    let payload = Payload::from_bits(&payload_bits);
    if payload.value() == 0 {
        return None;
    }
    let crc = bits[77..91]
        .iter()
        .fold(0_u16, |v, &b| v << 1 | u16::from(b));
    if crc != encode::crc14(payload) {
        CRC_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return None;
    }
    Some((payload, encode::tones_for_payload(payload)))
}

/// Scales likelihoods to the spread belief propagation works best with.
fn normalize(llr: &mut [f32; ldpc::N]) {
    let n = to_f32(ldpc::N);
    let mean = llr.iter().sum::<f32>() / n;
    let variance = llr.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / n;
    if variance > 0.0 {
        let scale = (LLR_TARGET_VARIANCE / variance).sqrt();
        for l in llr.iter_mut() {
            *l *= scale;
        }
    }
}

/// Variance of the likelihoods after [`normalize`], tuned on the bench.
const LLR_TARGET_VARIANCE: f32 = 24.0;

/// Signal-to-noise ratio of a decoded signal, from its tones' power against
/// the power in tones more than one step away from it.
fn estimate_snr(locked: &Locked, tones: &Tones) -> f64 {
    let (mut signal, mut noise, mut noise_bins) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (symbol, &tone) in locked.symbols.iter().zip(tones) {
        let tone = usize::from(tone);
        signal += f64::from(symbol[tone].norm_sqr());
        for (t, x) in symbol.iter().enumerate() {
            if t.abs_diff(tone) > 1 {
                noise += f64::from(x.norm_sqr());
                noise_bins += 1.0;
            }
        }
    }
    if noise <= 0.0 {
        return -30.0;
    }
    let per_bin = noise / noise_bins;
    let ratio = (signal / to_f64(SYMBOLS) / per_bin - 1.0).max(0.001);
    (10.0 * ratio.log10() - REFERENCE_BANDWIDTH_DB)
        .clamp(-30.0, 30.0)
        .round()
}

fn hz_to_row(hz: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positive, small
    let row = (hz / BIN_HZ).round().max(0.0) as usize;
    row
}

#[allow(clippy::cast_precision_loss)] // rows are small
fn row_to_hz(row: usize) -> f64 {
    row as f64 * BIN_HZ
}

#[allow(clippy::cast_precision_loss)] // small
fn start_to_dt(start: isize) -> f64 {
    let seconds = start as f64 / f64::from(baseband::RATE);
    ((seconds - NOMINAL_START_SECONDS) * 100.0).round() / 100.0
}

#[allow(clippy::cast_precision_loss)] // small
fn to_f64(n: usize) -> f64 {
    n as f64
}

fn to_isize(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

#[allow(clippy::cast_precision_loss)] // small counts
fn to_f32(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use rustfft::num_complex::Complex32;

    use super::{
        COHERENT_HZ, COSTAS, COSTAS_POSITIONS, DIFFERENTIAL_COHERENCE_MIN, DRIFT_HZ_PER_S, Options,
        SYMBOLS, costas_offset_and_drift, data_symbols, decode_slot, differential_coherence,
        evenness, osd_groups, steadiness, to_f32,
    };
    use crate::ft8::bench::{Rng, Signal, Slot, render, render_through};
    use crate::ft8::channel::Channel;
    use crate::ft8::encode::tones_for_codeword;
    use crate::ft8::ldpc;
    use crate::ft8::message::MessageError;

    fn audio(slot: &Slot) -> Result<Vec<f32>, MessageError> {
        Ok(render(slot, 3)?.into_iter().map(f32::from).collect())
    }

    fn audio_through(slot: &Slot, channel: &str) -> Result<Vec<f32>, MessageError> {
        let channel = Channel::parse(channel).map_err(|_| MessageError::Empty)?;
        Ok(render_through(slot, 3, &channel)?
            .into_iter()
            .map(f32::from)
            .collect())
    }

    /// Unit-variance complex noise in every tone of every symbol.
    fn noise_symbols(rng: &mut Rng) -> Vec<[Complex32; 8]> {
        (0..SYMBOLS)
            .map(|_| {
                std::array::from_fn(|_| {
                    #[allow(clippy::cast_possible_truncation)]
                    let (re, im) = (rng.gaussian() as f32, rng.gaussian() as f32);
                    Complex32::new(re, im) / std::f32::consts::SQRT_2
                })
            })
            .collect()
    }

    /// Adds a signal of amplitude `amplitude` sending `tones` to `symbols`,
    /// with the phase `phase(k)` at symbol `k`.
    fn add_signal(
        symbols: &mut [[Complex32; 8]],
        tones: &[u8; SYMBOLS],
        amplitude: f32,
        phase: impl Fn(usize) -> f32,
    ) {
        for (k, s) in symbols.iter_mut().enumerate() {
            s[usize::from(tones[k])] += Complex32::from_polar(amplitude, phase(k));
        }
    }

    fn random_codeword(rng: &mut Rng) -> [bool; ldpc::N] {
        std::array::from_fn(|_| rng.unit() < 0.5)
    }

    #[test]
    fn differential_coherence_believes_a_drifting_signal_and_not_noise() {
        let mut rng = Rng::new(11);
        let sent = tones_for_codeword(&random_codeword(&mut rng));
        let other = tones_for_codeword(&random_codeword(&mut rng));
        let data = |symbols: &[[Complex32; 8]]| -> Vec<[Complex32; 8]> {
            data_symbols().map(|k| symbols[k]).collect()
        };
        for trial in 0..20 {
            let mut symbols = noise_symbols(&mut rng);
            let only_noise = data(&symbols);
            assert!(
                differential_coherence(&only_noise, &sent, 1.0) < DIFFERENTIAL_COHERENCE_MIN,
                "noise alone passed on trial {trial}"
            );
            // A weak signal (about 5 dB over the noise in its tone, as a
            // -21 dB FT8 signal is) whose phase drifts and bends over the
            // transmission.
            let k0 = to_f32(trial);
            add_signal(&mut symbols, &sent, 1.8, |k| {
                let k = to_f32(k);
                0.3 * k + 0.004 * k * k + k0
            });
            let with_signal = data(&symbols);
            assert!(
                differential_coherence(&with_signal, &sent, 1.0) >= DIFFERENTIAL_COHERENCE_MIN,
                "the real codeword failed on trial {trial}"
            );
            assert!(
                differential_coherence(&with_signal, &other, 1.0) < DIFFERENTIAL_COHERENCE_MIN,
                "a wrong codeword passed on trial {trial}"
            );
        }
    }

    #[test]
    fn the_lock_fits_frequency_offset_and_drift_together() {
        let seconds_per_symbol =
            to_f32(super::baseband::SAMPLES_PER_SYMBOL) / super::baseband::RATE;
        let mut rng = Rng::new(5);
        let costas: [u8; SYMBOLS] = std::array::from_fn(|k| {
            COSTAS_POSITIONS
                .iter()
                .find(|&&c| (c..c + 7).contains(&k))
                .map_or(0, |&c| COSTAS[k - c])
        });
        for &(offset, drift) in &[(0.0_f32, 0.0_f32), (0.21, 0.0), (-0.1, 0.3), (0.3, -0.45)] {
            let mut symbols = noise_symbols(&mut rng);
            // A chirp: the phase is the integral of the frequency, which
            // runs from the offset less half the drift's reach to the
            // offset plus it, measured from the middle of the transmission.
            let middle = to_f32(SYMBOLS) / 2.0;
            add_signal(&mut symbols, &costas, 3.0, |k| {
                let t = (to_f32(k) + 0.5 - middle) * seconds_per_symbol;
                2.0 * std::f32::consts::PI * (offset * t + drift * t * t / 2.0)
            });
            let (found_offset, found_drift) =
                costas_offset_and_drift(&symbols, COHERENT_HZ, DRIFT_HZ_PER_S);
            assert!(
                (found_offset - offset).abs() <= 0.05,
                "offset {offset}: found {found_offset} (drift {found_drift})"
            );
            assert!(
                (found_drift - drift).abs() <= 0.05,
                "drift {drift}: found {found_drift} (offset {found_offset})"
            );
        }
    }

    #[test]
    fn steadiness_tells_a_steady_phase_from_a_scrambled_one() {
        let mut rng = Rng::new(7);
        let costas: [u8; SYMBOLS] = std::array::from_fn(|k| {
            COSTAS_POSITIONS
                .iter()
                .find(|&&c| (c..c + 7).contains(&k))
                .map_or(0, |&c| COSTAS[k - c])
        });
        let mut steady = noise_symbols(&mut rng);
        add_signal(&mut steady, &costas, 3.0, |_| 0.4);
        let s = steadiness(&steady);
        assert!(s > 0.8, "steady signal read {s}");
        assert_eq!(osd_groups(s), &[3, 4]);
        let mut scrambled = noise_symbols(&mut rng);
        let phases: Vec<f32> = (0..SYMBOLS)
            .map(|_| {
                #[allow(clippy::cast_possible_truncation)]
                let p = rng.range(0.0, std::f64::consts::TAU) as f32;
                p
            })
            .collect();
        add_signal(&mut scrambled, &costas, 3.0, |k| phases[k]);
        let s = steadiness(&scrambled);
        assert!(s < 0.5, "scrambled signal read {s}");
        assert_eq!(osd_groups(s), &[1, 2]);
        assert_eq!(osd_groups(0.65), &[2, 3, 4]);
    }

    #[test]
    fn evenness_sees_an_array_lost_to_a_fade() {
        let mut rng = Rng::new(8);
        let costas: [u8; SYMBOLS] = std::array::from_fn(|k| {
            COSTAS_POSITIONS
                .iter()
                .find(|&&c| (c..c + 7).contains(&k))
                .map_or(0, |&c| COSTAS[k - c])
        });
        let mut even = noise_symbols(&mut rng);
        add_signal(&mut even, &costas, 3.0, |_| 0.0);
        assert!(evenness(&even, 1.0) > 0.6, "{}", evenness(&even, 1.0));
        let mut faded = noise_symbols(&mut rng);
        let mut faded_tones = costas;
        // The last Costas array never arrives: its tones carry only noise.
        for k in COSTAS_POSITIONS[2]..COSTAS_POSITIONS[2] + 7 {
            faded_tones[k] = (costas[k] + 1) % 8;
        }
        add_signal(&mut faded, &faded_tones, 3.0, |_| 0.0);
        assert!(evenness(&faded, 1.0) < 0.4, "{}", evenness(&faded, 1.0));
    }

    #[test]
    fn decodes_signals_that_drift_or_flutter() -> Result<(), MessageError> {
        let signals = vec![
            Signal {
                message: "CQ K9KSU EN52".to_owned(),
                base_hz: 1000.0,
                dt_s: 0.1,
                snr_db: -14.0,
            },
            Signal {
                message: "W1AW K9KSU -12".to_owned(),
                base_hz: 1900.0,
                dt_s: -0.2,
                snr_db: -14.0,
            },
        ];
        let slot = Slot {
            index: 1,
            signals: signals.clone(),
        };
        for channel in ["drift=0.3", "phasenoise=0.5", "spread=0.5", "spread=2"] {
            let decodes = decode_slot(&audio_through(&slot, channel)?, &Options::default());
            for signal in &signals {
                assert!(
                    decodes.iter().any(|d| d.message == signal.message),
                    "{channel}: missed {}: {decodes:?}",
                    signal.message
                );
            }
            assert_eq!(decodes.len(), 2, "{channel}: {decodes:?}");
        }
        Ok(())
    }

    #[test]
    fn decodes_clear_signals_with_their_frequency_and_timing() -> Result<(), MessageError> {
        let signals = vec![
            Signal {
                message: "CQ K9KSU EN52".to_owned(),
                base_hz: 1500.0,
                dt_s: 0.0,
                snr_db: -10.0,
            },
            Signal {
                message: "W1AW K9KSU -12".to_owned(),
                base_hz: 800.0,
                dt_s: 0.4,
                snr_db: -14.0,
            },
            Signal {
                message: "K9KSU W1AW RR73".to_owned(),
                base_hz: 2400.0,
                dt_s: -0.3,
                snr_db: -8.0,
            },
        ];
        let slot = Slot {
            index: 0,
            signals: signals.clone(),
        };
        let decodes = decode_slot(&audio(&slot)?, &Options::default());
        assert_eq!(decodes.len(), 3, "{decodes:?}");
        for signal in &signals {
            let d = decodes
                .iter()
                .find(|d| d.message == signal.message)
                .ok_or(MessageError::Empty)?;
            assert!((d.base_hz - signal.base_hz).abs() <= 3.2, "{d:?}");
            assert!((d.dt_s - signal.dt_s).abs() <= 0.1, "{d:?}");
            assert!((d.snr_db - signal.snr_db).abs() <= 3.0, "{d:?}");
        }
        Ok(())
    }

    #[test]
    fn decodes_weak_signals_without_inventing_any() -> Result<(), MessageError> {
        // Twelve signals at -18 dB, a few decibels above where FT8 stops
        // working; every one sent must come back, and nothing else.
        let messages = [
            "CQ K9KSU EN52",
            "W1AW K9KSU -12",
            "K9KSU W1AW R-09",
            "CQ DL1ABC JO62",
            "VE3XYZ DL1ABC RR73",
            "JA1ZZZ VK2AB 73",
            "CQ G4ABC IO91",
            "N0AX G4ABC RRR",
            "CQ PY2AA GG66",
            "ZS6AB PY2AA +03",
            "CQ VK3XY QF22",
            "K1ABC VK3XY R+01",
        ];
        let mut found = 0;
        for (index, group) in messages.chunks(4).enumerate() {
            let signals: Vec<Signal> = group
                .iter()
                .zip([400.0, 1100.0, 1800.0, 2500.0])
                .map(|(message, base_hz)| Signal {
                    message: (*message).to_owned(),
                    base_hz,
                    dt_s: 0.2,
                    snr_db: -18.0,
                })
                .collect();
            let slot = Slot { index, signals };
            for d in decode_slot(&audio(&slot)?, &Options::default()) {
                assert!(messages.contains(&d.message.as_str()), "invented {d:?}");
                found += 1;
            }
        }
        assert_eq!(found, messages.len());
        Ok(())
    }

    #[test]
    fn decodes_a_field_day_band() -> Result<(), MessageError> {
        // A Field Day band: calls and exchanges, every one received exactly.
        let messages = [
            "CQ FD K9KSU EN52",
            "K9KSU W1AW 2A EMA",
            "W1AW K9KSU R 1D IL",
            "KD9ABC VE3XYZ 32F ONE",
            "N0X AA9ZZZ R 17C DX",
            "WA9XYZ N9ABC 5B WI",
        ];
        let signals: Vec<Signal> = messages
            .iter()
            .zip([300.0, 700.0, 1100.0, 1500.0, 1900.0, 2300.0])
            .map(|(message, base_hz)| Signal {
                message: (*message).to_owned(),
                base_hz,
                dt_s: 0.2,
                snr_db: -14.0,
            })
            .collect();
        let slot = Slot { index: 0, signals };
        let mut decoded: Vec<String> = decode_slot(&audio(&slot)?, &Options::default())
            .into_iter()
            .map(|d| d.message)
            .collect();
        decoded.sort();
        let mut expected: Vec<String> = messages.iter().map(|m| (*m).to_owned()).collect();
        expected.sort();
        assert_eq!(decoded, expected);
        Ok(())
    }

    #[test]
    fn noise_alone_decodes_to_nothing() -> Result<(), MessageError> {
        for index in 0..3 {
            let slot = Slot {
                index,
                signals: Vec::new(),
            };
            assert!(decode_slot(&audio(&slot)?, &Options::default()).is_empty());
        }
        Ok(())
    }
}
