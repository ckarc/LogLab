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
//!    tones and the strongest tone of each data symbol. If the best lock
//!    does not decode, its nearest neighbours in time and frequency are
//!    tried too.
//! 4. **Soft demodulation.** The complex tone amplitudes of each data symbol
//!    become a likelihood for each bit, first symbol by symbol and then
//!    three and two symbols at a time, adding the tones coherently (FT8's
//!    phase runs on unbroken from symbol to symbol).
//! 5. **Error correction.** The LDPC code's belief propagation turns the
//!    likelihoods into a codeword; the CRC confirms it. Signals it decodes
//!    are removed (step 6) before the slower ordered-statistics decoding is
//!    tried on the rest, so the leftovers of a strong signal cannot be
//!    mistaken for another one.
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
            osd_pairs: 30,
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
                None => Err(locks.swap_remove(0)),
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
        let deep = in_parallel(&failed, options.threads, |(candidate, locked)| {
            let locked = if near(candidate) {
                lock(&baseband, &meter, *candidate).swap_remove(0)
            } else {
                locked.clone()
            };
            demodulate(&locked, options, true).map(|_| locked)
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

/// Offsets from the best lock, in hertz and baseband samples, also tried
/// when it fails to decode: at low signal levels the best-scoring lock is
/// often a step away from the true one.
const NEIGHBOURS: [(f32, isize); 4] = [(0.0, -1), (0.0, 1), (-0.25, 0), (0.25, 0)];

/// Tunes a candidate's start and frequency to put the most power on the
/// signal: its Costas tones, plus the strongest tone of every data symbol.
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
    let (_, hz, start) = best;
    std::iter::once((0.0, 0))
        .chain(NEIGHBOURS)
        .map(|(dhz, dstart)| {
            let shifted = baseband::shift(&slice, hz + dhz);
            let start = start + dstart;
            Locked {
                start,
                base_hz: coarse_hz + f64::from(hz + dhz),
                symbols: (0..SYMBOLS)
                    .map(|k| meter.tones(&shifted, start + sps * to_isize(k)))
                    .collect(),
            }
        })
        .collect()
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
    // Likelihood sets for ordered-statistics decoding, each with the
    // largest disagreement accepted from it.
    let mut fallbacks = Vec::new();
    for group in [1, 3, 2] {
        let mut llr = metrics(&data, group, noise);
        normalize(&mut llr);
        let (decoded, mut accumulated) = ldpc::decode_soft(&llr, options.ldpc_iterations);
        if let Some(found) = decoded.and_then(|(bits, _)| accept(&bits)) {
            return Some(found);
        }
        fallbacks.push((llr, OSD_MAX_DISTANCE));
        if group == 1 {
            normalize(&mut accumulated);
            fallbacks.push((accumulated, OSD_REFINED_MAX_DISTANCE));
        }
    }
    if !deep || options.osd_pairs == 0 {
        return None;
    }
    fallbacks.iter().find_map(|(llr, limit)| {
        let (bits, distance) = ldpc::osd(llr, options.osd_pairs)?;
        if distance > *limit {
            return None;
        }
        accept(&bits).filter(|(payload, _)| plausible(*payload))
    })
}

/// Largest disagreement an ordered-statistics decode may have and still be
/// accepted, tuned on the bench.
const OSD_MAX_DISTANCE: f32 = 45.0;

/// The same, for likelihoods refined by belief propagation, which are more
/// sharply spread, so wrong codewords come nearer; tuned on the bench.
const OSD_REFINED_MAX_DISTANCE: f32 = 24.0;

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
    let mut weights = [0.0_f32; 512];
    let mut labels = [0_usize; 512];
    for (half, symbols) in data.chunks(29).enumerate() {
        let mut first = 0;
        while first < symbols.len() {
            let size = group.min(symbols.len() - first);
            let combos = 1_usize << (3 * size);
            let spread = noise * to_f32(size);
            for combo in 0..combos {
                let mut sum = Complex32::new(0.0, 0.0);
                let mut bits = 0_usize;
                for j in 0..size {
                    let tone = (combo >> (3 * (size - 1 - j))) & 7;
                    sum += symbols[first + j][tone];
                    bits = bits << 3 | usize::from(TONE_BITS[tone]);
                }
                weights[combo] = METRIC_SCALE * (sum.norm_sqr() / spread).sqrt();
                labels[combo] = bits;
            }
            // Log-sum-exp, relative to the largest weight so nothing
            // overflows.
            let peak = weights[..combos]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            let offset = 3 * (29 * half + first);
            for b in 0..3 * size {
                let (mut zero, mut one) = (0.0_f32, 0.0_f32);
                for (w, &bits) in weights[..combos].iter().zip(&labels[..combos]) {
                    let e = (w - peak).exp();
                    if (bits >> (3 * size - 1 - b)) & 1 == 0 {
                        zero += e;
                    } else {
                        one += e;
                    }
                }
                llr[offset + b] =
                    zero.max(f32::MIN_POSITIVE).ln() - one.max(f32::MIN_POSITIVE).ln();
            }
            first += size;
        }
    }
    llr
}

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
    use super::{Options, decode_slot};
    use crate::ft8::bench::{Signal, Slot, render};
    use crate::ft8::message::MessageError;

    fn audio(slot: &Slot) -> Result<Vec<f32>, MessageError> {
        Ok(render(slot, 3)?.into_iter().map(f32::from).collect())
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
