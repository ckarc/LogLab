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
//!    phase step from each Costas tone to the next what a signal at that
//!    frequency would show; asking only neighbouring tones to agree keeps
//!    the lock good when fading scrambles the phase over a whole array. If
//!    the best lock does not decode, its nearest neighbours in frequency
//!    are tried too.
//! 4. **Soft demodulation.** The Costas tones first say what the channel
//!    is doing to the signal's phase: steady, or wandering as a Doppler
//!    spread of about 0.5, 1, 2 or 3 Hz would make it ([`classify_fading`]).
//!    Then the complex tone amplitudes of each data symbol become a
//!    likelihood for each bit, first symbol by symbol and then three, two
//!    and four symbols at a time, combining the tones as that channel
//!    warrants ([`Combiner`]): coherently on a steady signal (FT8's phase
//!    runs on unbroken from symbol to symbol), by power alone when the
//!    phase is scrambled, and in between by the likelihood a fading channel
//!    gives, which keeps what coherence the fading leaves.
//! 5. **Error correction.** The LDPC code's belief propagation turns the
//!    likelihoods into a codeword; the CRC confirms it. Signals it decodes
//!    are removed (step 6) before the slower, deeper ordered-statistics
//!    decoding is tried on the rest, so the leftovers of a strong signal
//!    cannot be mistaken for another one. Every codeword, from either, must
//!    also be borne out by the signal's phase ([`validates`]): the tones it
//!    says were sent have to hold their phase from each symbol to the next
//!    along the transmission, which the tones noise happens to favour never
//!    do. The deeper decoding works on the three- and four-symbol
//!    likelihoods, which with the combining matched to the channel are the
//!    strongest sets whatever the channel does; and a deep decode is
//!    believed only when it beats its runner-up clearly and the signal
//!    arrived evenly, or both sets agree on it.
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
pub mod profile;

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
        let timer = profile::Timer::start();
        let spectrogram = Spectrogram::new(&residual, options.high_hz + 8.0 * TONE_SPACING_HZ);
        let baseband = Baseband::new(&residual);
        // A later pass only retries a place if subtraction changed it
        // enough to move the peak.
        let fresh: Vec<Candidate> = candidates(&spectrogram, options)
            .into_iter()
            .filter(|c| tried.insert((c.column, c.row)))
            .collect();
        timer.stop(profile::Stage::Search);
        profile::count(profile::Count::Candidates, fresh.len());

        // First belief propagation alone, at the best lock and its
        // neighbours.
        let quick = in_parallel(&fresh, options.threads, |&candidate| {
            let timer = profile::Timer::start();
            let locks = lock(&baseband, &meter, candidate);
            timer.stop(profile::Stage::Lock);
            let mut leftovers = Vec::with_capacity(locks.len());
            for locked in locks {
                match demodulate(&locked, options) {
                    Ok(_) => return Ok(locked),
                    Err(leftover) => leftovers.push((locked, leftover)),
                }
            }
            Err(leftovers)
        });
        let mut decoded_hz = Vec::new();
        let mut failed = Vec::new();
        for (candidate, outcome) in fresh.into_iter().zip(quick) {
            match outcome {
                Ok(locked) => {
                    if let Some(hz) = record(&mut found, &mut residual, &locked, options) {
                        decoded_hz.push(hz);
                    }
                }
                Err(leftovers) => failed.push((candidate, leftovers)),
            }
        }

        // Then the slower, riskier ordered-statistics decoding, on audio
        // with those signals already removed, so their leftovers cannot be
        // mistaken for other signals. Candidates near a removed signal are
        // locked again.
        let timer = profile::Timer::start();
        let baseband = if decoded_hz.is_empty() {
            baseband
        } else {
            Baseband::new(&residual)
        };
        timer.stop(profile::Stage::Search);
        let near = |candidate: &Candidate| {
            let hz = row_to_hz(candidate.row);
            decoded_hz.iter().any(|&d| (d - hz).abs() < NEAR_HZ)
        };
        let deep = in_parallel(&failed, options.threads, |(candidate, leftovers)| {
            // What the quick pass left of each lock is reused unless the
            // audio under the candidate has changed.
            let leftovers: Vec<(Locked, Leftover)> = if near(candidate) {
                let timer = profile::Timer::start();
                let locks = lock(&baseband, &meter, *candidate);
                timer.stop(profile::Stage::Lock);
                locks
                    .into_iter()
                    .filter_map(|locked| match demodulate(&locked, options) {
                        Ok(_) => None,
                        Err(leftover) => Some((locked, leftover)),
                    })
                    .collect()
            } else {
                leftovers.clone()
            };
            leftovers
                .into_iter()
                .find(|(_, leftover)| deep_decode(leftover, options).is_some())
                .map(|(locked, _)| locked)
        });
        let mut any = !decoded_hz.is_empty();
        for locked in deep.into_iter().flatten() {
            any |= record(&mut found, &mut residual, &locked, options).is_some();
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
) -> Option<f64> {
    let (payload, tones) = match demodulate(locked, options) {
        Ok(found) => found,
        Err(leftover) => deep_decode(&leftover, options)?,
    };
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

/// Reach and step, in hertz, of the fully coherent refinement of a steady
/// signal's frequency after the lag fit, whose error on a strong steady
/// signal is within 0.1 Hz.
const STEADY_HZ: (f32, f32) = (0.1, 0.01);

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
/// tone of every data symbol, over coarse steps. Then from the phases of
/// the Costas tones: the frequency offset and steady drift that make the
/// phase step from each Costas tone to the next what a signal at that
/// frequency would show ([`costas_offset_and_drift`]), the start that
/// fits best, and the offset once more. This settles the frequency to a
/// few hundredths of a hertz on a steady signal and the timing to one
/// baseband sample, which the multi-symbol likelihoods of [`metrics`]
/// need: a quarter-hertz error turns the phase by a third of a cycle
/// across three symbols. The fit asks only neighbouring tones to agree in
/// phase, so it holds up when fading scrambles the phase over a whole
/// array (see [`LOCK_CORRELATION`]).
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
        .max_by(|a, b| costas_fit(&a.1).total_cmp(&costas_fit(&b.1)))
        .unwrap_or((start, symbols));
    let (offset, slope) =
        costas_offset_and_drift(&symbols, COHERENT_HZ_AGAIN, DRIFT_HZ_PER_S_AGAIN);
    hz += offset;
    drift += slope;
    // On a steady signal the fully coherent fit is the more precise, so
    // when the channel reads steady the offset is settled once more by it,
    // within the reach the lag fit leaves.
    let symbols = measure(&baseband::chirp(&slice, hz, drift, middle), start);
    if classify_fading(&symbols, noise_power(&symbols)) == 0 {
        let (offset, _) = costas_offset(&symbols, STEADY_HZ, (0.0, 1.0), 1.0);
        hz += offset;
    }

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
/// `drift_step`) that together fit the Costas tones' phases best: each
/// Costas array is fitted ([`array_fit`]) at the offset plus the drift
/// times its own time from the middle of the transmission. A frequency
/// error turns the tones' phases steadily from symbol to symbol; a drift
/// makes that turn differ from one array to the next.
fn costas_offset_and_drift(
    symbols: &[[Complex32; 8]],
    reach: (f32, f32),
    drift_reach: (f32, f32),
) -> (f32, f32) {
    costas_offset(symbols, reach, drift_reach, LOCK_CORRELATION)
}

/// [`costas_offset_and_drift`] fitting the arrays' phases under lag-one
/// correlation `correlation` (see [`array_fit`]).
fn costas_offset(
    symbols: &[[Complex32; 8]],
    (reach, step): (f32, f32),
    (drift_reach, drift_step): (f32, f32),
    correlation: f32,
) -> (f32, f32) {
    let seconds_per_symbol = to_f32(baseband::SAMPLES_PER_SYMBOL) / baseband::RATE;
    let times = costas_times();
    let farthest = times.iter().fold(0.0_f32, |m, t| m.max(t.abs()));
    // Each array's fit over a grid of frequencies wide enough for any
    // offset and drift in reach.
    let grid_reach = reach + drift_reach * farthest;
    let grid_steps = round_to_usize(grid_reach / step);
    let grid: Vec<Vec<f32>> = COSTAS_POSITIONS
        .iter()
        .map(|&first| {
            let array = costas_array(symbols, first);
            let lags = costas_lags(&array);
            (0..=2 * grid_steps)
                .map(|i| {
                    let f = (to_f32(i) - to_f32(grid_steps)) * step;
                    array_fit(
                        &lags,
                        -2.0 * std::f32::consts::PI * f * seconds_per_symbol,
                        correlation,
                    )
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
    let mut best_steady = (f32::NEG_INFINITY, 0.0_f32);
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
            if d == drift_steps && power > best_steady.0 {
                best_steady = (power, f);
            }
        }
    }
    if best.0 < best_steady.0 * (1.0 + DRIFT_MARGIN) {
        return (best_steady.1, 0.0);
    }
    (best.1, best.2)
}

/// How much more of the Costas power a drifting fit must explain than the
/// best steady one before the drift is believed. A drift that costs the
/// steady fit less than this costs nothing worth chasing (0.02 Hz/s loses
/// about 4 percent of the Costas power; 0.05 Hz/s, the least the bench
/// sweeps, loses 20 percent), while on a weak steady signal noise readily
/// favours a small spurious drift, and dechirping by it spoils the lock
/// that a steady fit would have found. Measured on the clean bench: with no
/// margin, four signals in 1,800 were lost that way.
const DRIFT_MARGIN: f32 = 0.1;

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small, non-negative
fn round_to_usize(x: f32) -> usize {
    x.round().max(0.0) as usize
}

/// The seven Costas tones of the array starting at `first`.
fn costas_array(symbols: &[[Complex32; 8]], first: usize) -> [Complex32; 7] {
    std::array::from_fn(|j| symbols[first + j][usize::from(COSTAS[j])])
}

/// An array's power and its lag sums: `lags[0]` is the sum of the tones'
/// powers, `lags[k]` the sum of each tone times the conjugate of the tone
/// `k` symbols earlier.
fn costas_lags(array: &[Complex32; 7]) -> [Complex32; 7] {
    std::array::from_fn(|k| {
        (k..7)
            .map(|j| array[j] * array[j - k].conj())
            .sum::<Complex32>()
    })
}

/// How well an array fits a signal whose phase turns by `angle` per
/// symbol over a channel whose phase correlation from one symbol to the
/// next is `correlation`: the array's power plus, for each lag, the real
/// part of its lag sum turned back by the angle, weighted by the
/// correlation to the power of the lag squared (a Gaussian spread's). With
/// a correlation of 1 this is the array's in-phase power at that
/// frequency; at [`LOCK_CORRELATION`] only the step from each tone to the
/// next, and faintly the step over two, are asked to agree.
fn array_fit(lags: &[Complex32; 7], angle: f32, correlation: f32) -> f32 {
    let mut fit = lags[0].re;
    for (k, lag) in lags.iter().enumerate().skip(1) {
        let weight = correlation.powi(to_i32(k * k));
        if weight < 1e-3 {
            break;
        }
        fit += 2.0 * weight * (lag * Complex32::from_polar(1.0, angle * to_f32(k))).re;
    }
    fit
}

/// The phase correlation between neighbouring symbols the lock's main fit
/// assumes: that of a 3 Hz Doppler spread, which weights the lag-one phase
/// steps fully, the lag-two steps by less than a hundredth, and longer
/// ones not at all. On a steady signal this alone costs nothing measurable
/// against the fully coherent fit (the clean bench decodes the same
/// signals, with a frequency error of 0.02 Hz typical against 0.015, and
/// the coherent refinement of [`STEADY_HZ`] recovers the rest); at a 1 to
/// 2 Hz spread, where the phase wanders within an array and the coherent
/// fit's error reached 0.5 to 0.8 Hz, it cuts that error by a third and
/// recovers five or six signals in 400.
const LOCK_CORRELATION: f32 = 0.3;

/// The sum of every Costas array's [`array_fit`] at its measured
/// frequency; largest when the timing is right.
fn costas_fit(symbols: &[[Complex32; 8]]) -> f32 {
    COSTAS_POSITIONS
        .iter()
        .map(|&start| {
            array_fit(
                &costas_lags(&costas_array(symbols, start)),
                0.0,
                LOCK_CORRELATION,
            )
        })
        .sum()
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

/// The channels the decoder tells apart, by the correlation of the
/// signal's phase from one symbol to the next: a steady one, and Gaussian
/// Doppler spreads of about 0.5, 1, 2 and 3 Hz. (For a Gaussian spread
/// the correlation over `k` symbols is the lag-one correlation to the
/// power `k` squared.)
const FADING_HYPOTHESES: [f32; 5] = [1.0, 0.97, 0.88, 0.6, 0.3];

/// Which of the [`FADING_HYPOTHESES`] the signal's Costas tones fit best:
/// the likelihood of each array's seven tones under each hypothesis,
/// taking the fading as complex Gaussian with that correlation at the
/// measured tone signal-to-noise ratio, summed over the three arrays. A
/// steady signal is the first hypothesis, which with this likelihood a
/// steady phase favours too, since under the others its in-phase sum
/// would be improbably large. On the bench the classifier puts clean
/// signals in the steady class at every strength, 1 Hz spreads in the 1 Hz
/// class, 2 Hz spreads in the 2 Hz class, and 2 Hz of phase noise in the
/// 2 and 3 Hz classes.
fn classify_fading(symbols: &[[Complex32; 8]], noise: f32) -> usize {
    let gamma = tone_snr(symbols, noise);
    let mut best = (f32::NEG_INFINITY, 0);
    for (h, (values, vectors)) in costas_eigen().iter().enumerate() {
        let mut log_likelihood = 0.0_f32;
        for &first in &COSTAS_POSITIONS {
            let array = costas_array(symbols, first);
            for (lambda, vector) in values.iter().zip(vectors) {
                let projection: Complex32 = array.iter().zip(vector).map(|(a, v)| a * v).sum();
                let variance = gamma * lambda.max(0.0) + 1.0;
                log_likelihood -= projection.norm_sqr() / (noise * variance) + variance.ln();
            }
        }
        if log_likelihood > best.0 {
            best = (log_likelihood, h);
        }
    }
    best.1
}

/// The signal's tone power over the noise power, from the Costas tones.
fn tone_snr(symbols: &[[Complex32; 8]], noise: f32) -> f32 {
    let mut power = 0.0_f32;
    let mut tones = 0.0_f32;
    for &first in &COSTAS_POSITIONS {
        for tone in costas_array(symbols, first) {
            power += tone.norm_sqr();
            tones += 1.0;
        }
    }
    if noise > 0.0 {
        (power / tones / noise - 1.0).clamp(0.5, 100.0)
    } else {
        1.0
    }
}

/// Eigenvalues, and the eigenvectors that go with them.
type Eigen = (Vec<f32>, Vec<Vec<f32>>);

/// The correlation matrix of `n` consecutive symbols' phases under lag-one
/// correlation `rho`, as its eigenvalues and eigenvectors.
fn correlation_eigen(rho: f32, n: usize) -> Eigen {
    let matrix: Vec<Vec<f32>> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| rho.powi(to_i32(i.abs_diff(j).pow(2))))
                .collect()
        })
        .collect();
    jacobi_eigen(matrix)
}

/// For each fading hypothesis, the eigenvalues and eigenvectors of a Costas
/// array's seven tones' correlation matrix; computed once.
fn costas_eigen() -> &'static [Eigen] {
    static EIGEN: std::sync::OnceLock<Vec<Eigen>> = std::sync::OnceLock::new();
    EIGEN.get_or_init(|| {
        FADING_HYPOTHESES
            .iter()
            .map(|&rho| correlation_eigen(rho, 7))
            .collect()
    })
}

/// Eigenvalues and eigenvectors of a small symmetric matrix, by Jacobi
/// rotations. Only run at start-up, on the correlation matrices.
fn jacobi_eigen(mut matrix: Vec<Vec<f32>>) -> Eigen {
    let size = matrix.len();
    let mut basis: Vec<Vec<f32>> = (0..size)
        .map(|i| (0..size).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    for _ in 0..100 {
        let off: f32 = matrix
            .iter()
            .enumerate()
            .flat_map(|(i, row)| row.iter().enumerate().filter(move |(j, _)| i != *j))
            .map(|(_, x)| x * x)
            .sum();
        if off < 1e-12 {
            break;
        }
        for p in 0..size {
            for q in p + 1..size {
                if matrix[p][q].abs() < 1e-9 {
                    continue;
                }
                let theta = (matrix[q][q] - matrix[p][p]) / (2.0 * matrix[p][q]);
                let tangent = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let cos = 1.0 / (tangent * tangent + 1.0).sqrt();
                let sin = tangent * cos;
                let rotate = |row: &mut Vec<f32>| {
                    let (at_p, at_q) = (row[p], row[q]);
                    row[p] = cos * at_p - sin * at_q;
                    row[q] = sin * at_p + cos * at_q;
                };
                for row in &mut matrix {
                    rotate(row);
                }
                let (row_p, row_q): (Vec<f32>, Vec<f32>) = (0..size)
                    .map(|k| {
                        let (at_p, at_q) = (matrix[p][k], matrix[q][k]);
                        (cos * at_p - sin * at_q, sin * at_p + cos * at_q)
                    })
                    .unzip();
                matrix[p] = row_p;
                matrix[q] = row_q;
                for row in &mut basis {
                    rotate(row);
                }
            }
        }
    }
    let values: Vec<f32> = (0..size).map(|i| matrix[i][i]).collect();
    // Eigenvector i is column i of the basis.
    let vectors: Vec<Vec<f32>> = (0..size)
        .map(|i| basis.iter().map(|row| row[i]).collect())
        .collect();
    (values, vectors)
}

/// How the tones of consecutive symbols are combined into one strength,
/// for every group size up to [`MAX_GROUP`]: the eigenvectors of the
/// group's phase correlation matrix under one fading hypothesis, and a
/// weight for each. A steady channel (correlation 1) gives the plain
/// coherent sum of the group, an uncorrelated one the sum of the tones'
/// powers, and a fading channel the Gaussian-channel likelihood in
/// between, which keeps what coherence the fading leaves.
#[derive(Clone, Debug)]
struct Combiner {
    /// By group size, from 1: each eigenvector and its weight.
    sizes: Vec<Vec<(Vec<f32>, f32)>>,
}

impl Combiner {
    /// The combiner for fading hypothesis `hypothesis` at tone
    /// signal-to-noise ratio `gamma`. The weights are scaled so that a
    /// steady channel's coherent sum weighs exactly as before, whatever
    /// `gamma`, and `gamma` only shapes how much the orthogonal parts
    /// (which a steady signal leaves to the noise) are let in.
    fn new(hypothesis: usize, gamma: f32) -> Self {
        let sizes = group_eigen()[hypothesis]
            .iter()
            .enumerate()
            .map(|(i, (values, vectors))| {
                let g = to_f32(i + 1);
                let full = g * gamma / (g * gamma + 1.0);
                // A part that weighs nothing (a steady channel's orthogonal
                // parts) is left out, so a steady channel costs what the
                // plain coherent sum did.
                values
                    .iter()
                    .zip(vectors)
                    .map(|(&lambda, vector)| {
                        let lambda = lambda.max(0.0);
                        (
                            vector.clone(),
                            (gamma * lambda / (gamma * lambda + 1.0)) / full,
                        )
                    })
                    .filter(|(_, weight)| *weight > 1e-3)
                    .collect()
            })
            .collect();
        Self { sizes }
    }

    /// The steady channel's combiner: plain coherent sums.
    fn steady() -> Self {
        Self::new(0, 1.0)
    }
}

/// For each fading hypothesis and group size, the eigenvalues and
/// eigenvectors of the group's correlation matrix; computed once.
fn group_eigen() -> &'static [Vec<Eigen>] {
    static EIGEN: std::sync::OnceLock<Vec<Vec<Eigen>>> = std::sync::OnceLock::new();
    EIGEN.get_or_init(|| {
        FADING_HYPOTHESES
            .iter()
            .map(|&rho| (1..=MAX_GROUP).map(|n| correlation_eigen(rho, n)).collect())
            .collect()
    })
}

/// The bit likelihoods of `group`-symbol combining, scaled for belief
/// propagation.
fn likelihoods(
    data: &[[Complex32; 8]],
    group: usize,
    noise: f32,
    combiner: &Combiner,
) -> [f32; ldpc::N] {
    let mut llr = metrics(data, group, noise, combiner);
    normalize(&mut llr);
    llr
}

/// Bit likelihoods from the data symbols, taking `group` consecutive
/// symbols together (within each half between Costas arrays). Every
/// combination of tones in the group is weighed by the strength of the
/// group combined as `combiner` says (the coherent sum, on a steady
/// channel), and a bit's likelihood compares the total weight of the
/// combinations with that bit 0 against those with it 1.
fn metrics(
    data: &[[Complex32; 8]],
    group: usize,
    noise: f32,
    combiner: &Combiner,
) -> [f32; ldpc::N] {
    let mut llr = [0.0_f32; ldpc::N];
    // The combiner's parts of every tone combination, built a symbol at a
    // time: the sums over the first j symbols are the first 8^j entries.
    let mut sums = vec![[Complex32::new(0.0, 0.0); MAX_GROUP]; 1 << (3 * MAX_GROUP)];
    for (half, symbols) in data.chunks(29).enumerate() {
        let mut first = 0;
        while first < symbols.len() {
            let size = group.min(symbols.len() - first);
            let parts = &combiner.sizes[size - 1];
            let combos = 1_usize << (3 * size);
            sums[0] = [Complex32::new(0.0, 0.0); MAX_GROUP];
            for j in 0..size {
                let done = 1_usize << (3 * j);
                for prefix in (0..done).rev() {
                    let partial = sums[prefix];
                    for (tone, amplitude) in symbols[first + j].iter().enumerate() {
                        let mut next = partial;
                        for (n, (vector, _)) in next.iter_mut().zip(parts) {
                            *n += amplitude * vector[j];
                        }
                        sums[prefix << 3 | tone] = next;
                    }
                }
            }
            let weight = |sum: &[Complex32; MAX_GROUP]| {
                let strength: f32 = parts
                    .iter()
                    .zip(sum)
                    .map(|((_, w), s)| w * s.norm_sqr())
                    .sum();
                METRIC_SCALE * (strength.max(0.0) / noise).sqrt()
            };
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

/// Indexes of the 58 data symbols, in order.
fn data_symbols() -> impl Iterator<Item = usize> {
    (0..SYMBOLS).filter(|s| !COSTAS_POSITIONS.iter().any(|&c| (c..c + 7).contains(s)))
}

/// What belief propagation leaves of a lock it could not decode: what the
/// deep search needs to go on with.
#[derive(Clone)]
struct Leftover {
    /// The data symbols' tones.
    data: Vec<[Complex32; 8]>,
    /// Noise power per tone.
    noise: f32,
    /// How evenly the signal arrived ([`evenness`]).
    evenness: f32,
    /// The likelihood sets of [`DEEP_GROUPS`].
    llrs: Vec<[f32; ldpc::N]>,
}

/// Tries the soft-decision metrics in turn until one decodes by belief
/// propagation; otherwise returns what [`deep_decode`] needs.
fn demodulate(locked: &Locked, options: &Options) -> Result<(Payload, Tones), Leftover> {
    profile::count(profile::Count::Locks, 1);
    let data: Vec<[Complex32; 8]> = data_symbols().map(|k| locked.symbols[k]).collect();
    let noise = noise_power(&data);
    let combiner = Combiner::new(
        classify_fading(&locked.symbols, noise),
        tone_snr(&locked.symbols, noise),
    );
    let mut llrs = Vec::with_capacity(DEEP_GROUPS.len());
    for group in GROUPS {
        let timer = profile::Timer::start();
        let llr = likelihoods(&data, group, noise, &combiner);
        let timer = timer.lap(profile::Stage::Likelihoods);
        let (decoded, _) = ldpc::decode_soft(&llr, options.ldpc_iterations);
        let timer = timer.lap(profile::Stage::BeliefPropagation);
        profile::count(profile::Count::BeliefPropagation, 1);
        let found = decoded
            .filter(|(bits, _)| validates(&data, bits, noise))
            .and_then(|(bits, _)| accept(&bits));
        timer.stop(profile::Stage::Validation);
        if let Some(found) = found {
            profile::count(profile::Count::DecodedByBp, 1);
            return Ok(found);
        }
        if DEEP_GROUPS.contains(&group) {
            llrs.push(llr);
        }
    }
    let evenness = evenness(&locked.symbols, noise);
    Err(Leftover {
        data,
        noise,
        evenness,
        llrs,
    })
}

/// Ordered-statistics decoding on every likelihood set belief propagation
/// left; a codeword found from more than one set is the more believable
/// for it.
fn deep_decode(leftover: &Leftover, options: &Options) -> Option<(Payload, Tones)> {
    if options.osd_pairs == 0 {
        return None;
    }
    let timer = profile::Timer::start();
    profile::count(profile::Count::DeepSearches, leftover.llrs.len());
    let results: Vec<([bool; ldpc::N], f32, f32)> = leftover
        .llrs
        .iter()
        .filter_map(|llr| ldpc::osd(llr, options.osd_pairs, options.osd_triples))
        .collect();
    let timer = timer.lap(profile::Stage::DeepSearch);
    let found = deep_pick(&results, &leftover.data, leftover.noise, leftover.evenness);
    timer.stop(profile::Stage::Validation);
    if found.is_some() {
        profile::count(profile::Count::DecodedByDeep, 1);
    }
    found
}

/// The first deep-search codeword that earns belief.
fn deep_pick(
    results: &[([bool; ldpc::N], f32, f32)],
    data: &[[Complex32; 8]],
    noise: f32,
    evenness: f32,
) -> Option<(Payload, Tones)> {
    results.iter().find_map(|(bits, distance, margin)| {
        let agreeing = results.iter().filter(|r| r.0 == *bits).count();
        if *distance > OSD_MAX_DISTANCE
            || *margin < OSD_MIN_MARGIN
            || (agreeing < 2 && evenness < OSD_MIN_EVENNESS)
            || !validates(data, bits, noise)
        {
            return None;
        }
        let found = accept(bits).filter(|(payload, _)| plausible(*payload));
        if found.is_some() {
            profile::count(
                if agreeing >= 2 {
                    profile::Count::DeepByAgreement
                } else {
                    profile::Count::DeepByEvenness
                },
                1,
            );
        }
        found
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

/// Whether a codeword is borne out by the signal's phase, by either of two
/// statistics. The [`differential_coherence`] asks the decoded tones to
/// hold their phase step from one symbol to the next, which drift and slow
/// fading leave alone; but it has no floor, so a frequency-selective path
/// (an echo 20 ms behind puts a notch inside every FT8 signal's 50 Hz) that
/// turns the phase differently for each tone can defeat it even on a strong
/// signal. The [`run_coherence`] adds the decoded tones in phase over runs
/// of four symbols as a power, which never falls below the tones' own power
/// added incoherently, so a strong signal always passes it, though it
/// refuses weak real decodes under phase noise, where the differential
/// statistic holds. Noise passes neither (see the two thresholds).
fn validates(data: &[[Complex32; 8]], bits: &[bool; ldpc::N], noise: f32) -> bool {
    let tones = encode::tones_for_codeword(bits);
    differential_coherence(data, &tones, noise) >= DIFFERENTIAL_COHERENCE_MIN
        || run_coherence(data, &tones, noise) >= RUN_COHERENCE_MIN
}

/// The in-phase power of the decoded tones over every run of
/// [`COHERENCE_SPAN`] consecutive data symbols, averaged over the runs, in
/// units of the noise power. A real signal's tones add up (FT8's phase runs
/// on unbroken from symbol to symbol); the tones noise happens to favour
/// have random phases and do not.
fn run_coherence(data: &[[Complex32; 8]], tones: &Tones, noise: f32) -> f32 {
    let data_tones: Vec<usize> = data_symbols().map(|k| usize::from(tones[k])).collect();
    let mut total = 0.0_f32;
    let mut runs = 0.0_f32;
    for (symbols, sent) in data.chunks(29).zip(data_tones.chunks(29)) {
        for first in 0..=symbols.len().saturating_sub(COHERENCE_SPAN) {
            let sum: Complex32 = (first..first + COHERENCE_SPAN)
                .map(|k| symbols[k][sent[k]])
                .sum();
            total += sum.norm_sqr();
            runs += 1.0;
        }
    }
    if noise > 0.0 && runs > 0.0 {
        total / (noise * runs)
    } else {
        0.0
    }
}

/// Symbols added in phase by [`run_coherence`]: short enough to hold on a
/// fading channel, long enough that noise cannot match it.
const COHERENCE_SPAN: usize = 4;

/// The least run coherence of a codeword that is believed: noise never
/// reached 27 in 480,000 tries on the bench (about 13 typically); real
/// decodes on a steady channel read 34 and up down to -23 dB.
const RUN_COHERENCE_MIN: f32 = 30.0;

/// The least differential coherence of a codeword that is believed. On
/// the bench, noise never reached 1.53 in 480,000 tries, and above 1.2 its
/// tail thins fivefold with every 0.1 (298 above 1.2, 51 above 1.3, 10
/// above 1.4, 2 above 1.5); the weakest real decodes read 1.9 and up,
/// however they drift or flutter.
const DIFFERENTIAL_COHERENCE_MIN: f32 = 1.8;

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

/// The likelihood sets ordered-statistics decoding works on. With the
/// combining matched to the channel, the three- and four-symbol sets carry
/// what the shorter ones carry and more in every condition on the bench,
/// steady or fluttering: measured from an oracle lock over seven
/// conditions, they are the best pair everywhere, and adding the one- and
/// two-symbol sets would rescue at most two more signals in 400 for twice
/// the deep-search time.
const DEEP_GROUPS: [usize; 2] = [3, 4];

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

/// How sharply the tone amplitudes, measured in units of the noise, are
/// weighed against each other; tuned on the bench.
const METRIC_SCALE: f32 = 4.0;

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

fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

#[allow(clippy::cast_precision_loss)] // small counts
fn to_f32(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use rustfft::num_complex::Complex32;

    use super::{
        COHERENT_HZ, COSTAS, COSTAS_POSITIONS, Combiner, DIFFERENTIAL_COHERENCE_MIN,
        DRIFT_HZ_PER_S, FADING_HYPOTHESES, MAX_GROUP, Options, STEADY_HZ, SYMBOLS, array_fit,
        classify_fading, costas_array, costas_lags, costas_offset, costas_offset_and_drift,
        data_symbols, decode_slot, differential_coherence, evenness, to_f32,
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
                (found_offset - offset).abs() <= 0.08,
                "offset {offset}: found {found_offset} (drift {found_drift})"
            );
            assert!(
                (found_drift - drift).abs() <= 0.05,
                "drift {drift}: found {found_drift} (offset {found_offset})"
            );
            // The lag fit leaves the coherent refinement of a steady
            // signal little to do, and it does that precisely.
            let dechirped: Vec<[Complex32; 8]> = symbols
                .iter()
                .enumerate()
                .map(|(k, tones)| {
                    let t = (to_f32(k) + 0.5 - middle) * seconds_per_symbol;
                    let turn = -2.0
                        * std::f32::consts::PI
                        * (found_offset * t + found_drift * t * t / 2.0);
                    tones.map(|a| a * Complex32::from_polar(1.0, turn))
                })
                .collect();
            let (refined, _) = costas_offset(&dechirped, STEADY_HZ, (0.0, 1.0), 1.0);
            assert!(
                (found_offset + refined - offset).abs() <= 0.03,
                "offset {offset}: refined to {}",
                found_offset + refined
            );
        }
    }

    #[test]
    fn the_classifier_tells_a_steady_phase_from_a_scrambled_one() {
        let mut rng = Rng::new(7);
        let costas: [u8; SYMBOLS] = std::array::from_fn(|k| {
            COSTAS_POSITIONS
                .iter()
                .find(|&&c| (c..c + 7).contains(&k))
                .map_or(0, |&c| COSTAS[k - c])
        });
        let (mut steady_right, mut scrambled_right) = (0, 0);
        for _ in 0..20 {
            // A signal about 5 dB over the noise in its tone, as a -21 dB
            // FT8 signal is, first with a steady phase, then with a phase
            // unrelated from symbol to symbol.
            let mut steady = noise_symbols(&mut rng);
            add_signal(&mut steady, &costas, 1.8, |_| 0.4);
            steady_right += usize::from(classify_fading(&steady, 1.0) == 0);
            let mut scrambled = noise_symbols(&mut rng);
            let phases: Vec<f32> = (0..SYMBOLS)
                .map(|_| {
                    #[allow(clippy::cast_possible_truncation)]
                    let p = rng.range(0.0, std::f64::consts::TAU) as f32;
                    p
                })
                .collect();
            add_signal(&mut scrambled, &costas, 1.8, |k| phases[k]);
            scrambled_right += usize::from(classify_fading(&scrambled, 1.0) >= 3);
        }
        assert!(
            steady_right >= 18,
            "steady signals read steady {steady_right} of 20"
        );
        assert!(
            scrambled_right >= 18,
            "scrambled signals read fast fading {scrambled_right} of 20"
        );
    }

    #[test]
    fn the_combiner_sums_coherently_when_steady_and_by_power_when_scrambled() {
        // Steady: one part, the mean over the group (so its squared
        // magnitude is the coherent power over the group size), weight 1.
        let steady = Combiner::steady();
        for (size, parts) in steady.sizes.iter().enumerate() {
            let n = size + 1;
            let kept: Vec<&(Vec<f32>, f32)> = parts.iter().filter(|(_, w)| *w > 1e-3).collect();
            assert_eq!(kept.len(), 1, "group of {n}: {parts:?}");
            let (vector, weight) = kept[0];
            assert!((weight - 1.0).abs() < 1e-4, "group of {n}: weight {weight}");
            for v in vector {
                assert!(
                    (v.abs() - 1.0 / to_f32(n).sqrt()).abs() < 1e-4,
                    "group of {n}: {vector:?}"
                );
            }
        }
        // The fastest hypothesis, 3 Hz: every part weighs nearly the same,
        // so the strength is nearly the sum of the tones' powers.
        let fast = Combiner::new(FADING_HYPOTHESES.len() - 1, 3.0);
        let parts = &fast.sizes[MAX_GROUP - 1];
        let weights: Vec<f32> = parts.iter().map(|(_, w)| *w).collect();
        let (low, high) = weights
            .iter()
            .fold((f32::INFINITY, 0.0_f32), |(l, h), &w| (l.min(w), h.max(w)));
        assert!(high / low < 1.6, "weights {weights:?}");
    }

    #[test]
    fn the_lag_fit_tracks_the_in_phase_power_on_a_steady_signal() {
        let mut rng = Rng::new(9);
        let costas: [u8; SYMBOLS] = std::array::from_fn(|k| {
            COSTAS_POSITIONS
                .iter()
                .find(|&&c| (c..c + 7).contains(&k))
                .map_or(0, |&c| COSTAS[k - c])
        });
        let mut symbols = noise_symbols(&mut rng);
        add_signal(&mut symbols, &costas, 10.0, |k| 0.2 * to_f32(k));
        // Over the three arrays, the lag fit peaks at the same phase step
        // as the in-phase power, and with a correlation of 1 it is the
        // in-phase power.
        let lags: Vec<[Complex32; 7]> = COSTAS_POSITIONS
            .iter()
            .map(|&first| costas_lags(&costas_array(&symbols, first)))
            .collect();
        let power = |angle: f32| -> f32 {
            COSTAS_POSITIONS
                .iter()
                .map(|&first| {
                    (0..7)
                        .map(|j| {
                            symbols[first + j][usize::from(COSTAS[j])]
                                * Complex32::from_polar(1.0, angle * to_f32(j))
                        })
                        .sum::<Complex32>()
                        .norm_sqr()
                })
                .sum()
        };
        let peak = |score: &dyn Fn(f32) -> f32| {
            (-50..=50)
                .map(|i| to_f32_signed(i) * 0.02)
                .max_by(|a, b| score(*a).total_cmp(&score(*b)))
                .unwrap_or(0.0)
        };
        let fit_peak = peak(&|angle| lags.iter().map(|l| array_fit(l, angle, 0.3)).sum());
        let power_peak = peak(&power);
        assert!(
            (fit_peak - power_peak).abs() <= 0.1,
            "fit peaks at {fit_peak}, in-phase power at {power_peak}"
        );
        for angle in [-0.3_f32, 0.0, 0.25] {
            let coherent: f32 = lags.iter().map(|l| array_fit(l, angle, 1.0)).sum();
            assert!(
                (coherent - power(angle)).abs() <= 1e-2 * power(angle).abs().max(1.0),
                "at {angle}: fit {coherent}, power {}",
                power(angle)
            );
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn to_f32_signed(i: i32) -> f32 {
        i as f32
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
    fn decodes_signals_that_drift_flutter_or_echo() -> Result<(), MessageError> {
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
        for channel in [
            "drift=0.3",
            "phasenoise=0.5",
            "spread=0.5",
            "spread=2",
            "echo=20,echodb=-3",
        ] {
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
