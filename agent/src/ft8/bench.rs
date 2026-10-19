// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The FT8 decoder test bench.
//!
//! A *scenario* is a list of 15-second slots, each holding several FT8
//! signals at known frequencies, timing offsets, and signal-to-noise ratios,
//! buried in white noise. The bench renders each slot to a WAV recording and
//! writes the truth (what was really sent) beside it. Any decoder, WSJT-X's
//! today and LogLab's own later, is run on the recordings, and [`score`]
//! compares what it decoded with the truth: how many signals it found at each
//! signal strength, and whether it reported anything that was not there.
//!
//! Everything is generated from a seed, so a scenario is the same on every
//! machine and nothing large has to be stored in the repository.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::encode;
use super::message::normalize;
use super::wave;
use super::{NOMINAL_START_SECONDS, SAMPLE_RATE, SLOT_SAMPLES};

/// Bandwidth signal-to-noise ratios are quoted in, by FT8 convention.
const SNR_REFERENCE_HZ: f64 = 2_500.0;

/// Noise level of rendered recordings, in 16-bit sample units (one standard
/// deviation). Leaves room for many strong signals before clipping.
const NOISE_RMS: f64 = 300.0;

/// Lowest and highest base frequency signals are placed at, in hertz.
const LOWEST_HZ: f64 = 200.0;
const HIGHEST_HZ: f64 = 2_900.0;

/// Minimum distance between two signals' base frequencies. An FT8 signal is
/// 50 Hz wide, so signals in this bench do not overlap; overlapping signals
/// are a separate, harder scenario.
const MIN_SPACING_HZ: f64 = 60.0;

/// A small, fast, deterministic random number generator (`SplitMix64`), so
/// scenarios repeat exactly from their seed.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator started from `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `[0, 1)`.
    #[allow(clippy::cast_precision_loss)] // 53 bits fit an f64 exactly
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// A number in `[low, high)`.
    pub fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// An index in `0..n`; `n` must be positive.
    #[allow(clippy::cast_possible_truncation)] // the result is below n
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// A normally distributed number with mean 0 and standard deviation 1.
    pub fn gaussian(&mut self) -> f64 {
        let u1 = 1.0 - self.unit();
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    fn pick(&mut self, choices: &[u8]) -> char {
        char::from(choices[self.below(choices.len())])
    }
}

/// One signal placed in a slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// The message sent, normalized.
    pub message: String,
    /// Frequency of the lowest tone, in hertz.
    pub base_hz: f64,
    /// Start time relative to the nominal 0.5 s into the slot, in seconds.
    pub dt_s: f64,
    /// Signal-to-noise ratio in 2,500 Hz, in decibels.
    pub snr_db: f64,
}

/// One 15-second slot of a scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// The slot's number within its scenario, from 0.
    pub index: usize,
    /// The signals in it.
    pub signals: Vec<Signal>,
}

impl Slot {
    /// The slot's recording file name.
    #[must_use]
    pub fn file_stem(&self) -> String {
        format!("slot-{:04}", self.index)
    }
}

/// How a scenario is generated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScenarioSpec {
    /// Seed for every random choice.
    pub seed: u64,
    /// Number of slots.
    pub slots: usize,
    /// Signals in each slot.
    pub signals_per_slot: usize,
    /// Weakest signal, in decibels.
    pub min_snr_db: f64,
    /// Strongest signal, in decibels.
    pub max_snr_db: f64,
}

impl ScenarioSpec {
    /// The standard bench: 60 slots of 10 signals from -26 to -4 dB, which
    /// spans the range where FT8 decoders go from finding everything to
    /// finding nothing.
    #[must_use]
    pub fn standard(seed: u64) -> Self {
        Self {
            seed,
            slots: 60,
            signals_per_slot: 10,
            min_snr_db: -26.0,
            max_snr_db: -4.0,
        }
    }
}

/// Generates a scenario's slots and signals.
#[must_use]
pub fn generate(spec: &ScenarioSpec) -> Vec<Slot> {
    let mut rng = Rng::new(spec.seed);
    (0..spec.slots)
        .map(|index| {
            let mut used: Vec<f64> = Vec::new();
            let mut messages = BTreeSet::new();
            let mut signals = Vec::new();
            while signals.len() < spec.signals_per_slot {
                let base_hz = rng.range(LOWEST_HZ, HIGHEST_HZ).round();
                if used.iter().any(|f| (f - base_hz).abs() < MIN_SPACING_HZ) {
                    continue;
                }
                let message = random_message(&mut rng);
                if !messages.insert(message.clone()) {
                    continue;
                }
                used.push(base_hz);
                signals.push(Signal {
                    message,
                    base_hz,
                    dt_s: (rng.range(-0.4, 1.0) * 100.0).round() / 100.0,
                    snr_db: (rng.range(spec.min_snr_db, spec.max_snr_db) * 10.0).round() / 10.0,
                });
            }
            Slot { index, signals }
        })
        .collect()
}

/// A random standard FT8 message of the kinds seen in ordinary QSOs.
pub fn random_message(rng: &mut Rng) -> String {
    let a = random_call(rng);
    let b = random_call(rng);
    match rng.below(8) {
        0 | 1 => format!("CQ {b} {}", random_grid(rng)),
        2 => format!("{a} {b} {}", random_grid(rng)),
        3 => format!("{a} {b} {}", random_report(rng)),
        4 => format!("{a} {b} R{}", random_report(rng)),
        5 => format!("{a} {b} RR73"),
        6 => format!("{a} {b} RRR"),
        _ => format!("{a} {b} 73"),
    }
}

/// A random standard callsign: one or two prefix characters, a digit, and
/// one to three letters. Calls never start with Q, as none are issued.
fn random_call(rng: &mut Rng) -> String {
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const FIRST: &[u8] = b"ABCDEFGHIJKLMNOPRSTUVWXYZ";
    const DIGITS: &[u8] = b"0123456789";
    let mut call = String::new();
    call.push(rng.pick(FIRST));
    if rng.below(2) == 0 {
        call.push(rng.pick(LETTERS));
    }
    call.push(rng.pick(DIGITS));
    for _ in 0..=rng.below(3) {
        call.push(rng.pick(LETTERS));
    }
    call
}

fn random_grid(rng: &mut Rng) -> String {
    const FIELD: &[u8] = b"ABCDEFGHIJKLMNOPQR";
    const DIGITS: &[u8] = b"0123456789";
    [
        rng.pick(FIELD),
        rng.pick(FIELD),
        rng.pick(DIGITS),
        rng.pick(DIGITS),
    ]
    .iter()
    .collect()
}

fn random_report(rng: &mut Rng) -> String {
    let below = rng.below(41);
    let report = i32::try_from(below).unwrap_or(0) - 24;
    format!("{report:+03}")
}

/// Renders a slot to 15 seconds of 16-bit audio at 12,000 samples per second.
///
/// # Errors
///
/// A signal whose message the encoder refuses (generated scenarios never
/// contain one).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped and rounded
pub fn render(slot: &Slot, seed: u64) -> Result<Vec<i16>, super::message::MessageError> {
    let mut rng = Rng::new(seed ^ (slot.index as u64).wrapping_mul(0x2545_f491_4f6c_dd1d));
    let mut audio: Vec<f64> = (0..SLOT_SAMPLES).map(|_| rng.gaussian()).collect();
    let rate = f64::from(SAMPLE_RATE);
    for signal in &slot.signals {
        let tones = encode::tones(&signal.message)?;
        let wave = wave::synthesize(&tones, signal.base_hz);
        // White noise of unit variance spreads over 6,000 Hz; scale the
        // signal against the share of it that falls in 2,500 Hz.
        let amplitude =
            (2.0 * SNR_REFERENCE_HZ / (rate / 2.0)).sqrt() * 10_f64.powf(signal.snr_db / 20.0);
        let start = ((NOMINAL_START_SECONDS + signal.dt_s) * rate)
            .round()
            .max(0.0) as usize;
        for (out, s) in audio.iter_mut().skip(start).zip(wave) {
            *out += amplitude * f64::from(s);
        }
    }
    Ok(audio
        .into_iter()
        .map(|x| (x * NOISE_RMS).round().clamp(-32_767.0, 32_767.0) as i16)
        .collect())
}

/// A mono 16-bit PCM WAV file holding `samples` at 12,000 samples per second.
#[must_use]
pub fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2_u16.to_le_bytes()); // bytes per sample frame
    out.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The truth file for a scenario: one tab-separated line per signal (slot,
/// base frequency, timing offset, SNR, message) after a header naming the
/// scenario.
#[must_use]
pub fn truth_tsv(spec: &ScenarioSpec, slots: &[Slot]) -> String {
    let mut out = format!(
        "# loglab ft8bench scenario seed={} slots={} signals={} snr={}..{}\n",
        spec.seed, spec.slots, spec.signals_per_slot, spec.min_snr_db, spec.max_snr_db
    );
    for slot in slots {
        for s in &slot.signals {
            let _ = writeln!(
                out,
                "{}\t{}\t{:.2}\t{:.1}\t{}",
                slot.index, s.base_hz, s.dt_s, s.snr_db, s.message
            );
        }
    }
    out
}

/// Reads a truth file back.
///
/// # Errors
///
/// The number of the first line that is not a valid truth line.
pub fn parse_truth(text: &str) -> Result<Vec<Slot>, usize> {
    let mut slots: BTreeMap<usize, Vec<Signal>> = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let parsed = (|| {
            let [slot, base, dt, snr, message] = fields.as_slice() else {
                return None;
            };
            Some((
                slot.parse::<usize>().ok()?,
                Signal {
                    message: normalize(message),
                    base_hz: base.parse().ok()?,
                    dt_s: dt.parse().ok()?,
                    snr_db: snr.parse().ok()?,
                },
            ))
        })();
        let (slot, signal) = parsed.ok_or(number + 1)?;
        slots.entry(slot).or_default().push(signal);
    }
    Ok(slots
        .into_iter()
        .map(|(index, signals)| Slot { index, signals })
        .collect())
}

/// The message in one line of decoder output, in the format WSJT-X's `jt9`
/// prints (`time snr dt freq ~ message`, optionally followed by a confidence
/// mark such as `?` or `a2`). `None` for lines that are not decodes.
#[must_use]
pub fn parse_decode_line(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 6 || words[4] != "~" {
        return None;
    }
    words[1].parse::<i32>().ok()?;
    words[3].parse::<f64>().ok()?;
    let mut message = &words[5..];
    while let Some((last, rest)) = message.split_last() {
        let mark = *last == "?"
            || (last.len() >= 2
                && last.starts_with('a')
                && last[1..].bytes().all(|b| b.is_ascii_digit()));
        if !mark {
            break;
        }
        message = rest;
    }
    if message.is_empty() {
        None
    } else {
        Some(normalize(&message.join(" ")))
    }
}

/// How a decoder did on one group of signals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Signals sent.
    pub sent: usize,
    /// Signals the decoder found.
    pub found: usize,
}

/// How a decoder did on a whole scenario.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Score {
    /// Results by SNR band: the key is the lower edge of a 2 dB band.
    pub by_snr: BTreeMap<i32, Tally>,
    /// All signals together.
    pub total: Tally,
    /// Decodes of messages that were not sent, as (slot, message).
    pub false_decodes: Vec<(usize, String)>,
    /// Signals missed, as (slot, SNR, message).
    pub missed: Vec<(usize, f64, String)>,
}

/// Scores decoder output against the truth. `decoded` holds the messages
/// decoded in each slot, keyed by slot number; a slot missing from it counts
/// as decoded with nothing found.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // SNRs are small
pub fn score(truth: &[Slot], decoded: &BTreeMap<usize, Vec<String>>) -> Score {
    let mut result = Score::default();
    let empty = Vec::new();
    for slot in truth {
        let got: BTreeSet<String> = decoded
            .get(&slot.index)
            .unwrap_or(&empty)
            .iter()
            .map(|m| normalize(m))
            .collect();
        let sent: BTreeSet<&str> = slot.signals.iter().map(|s| s.message.as_str()).collect();
        for signal in &slot.signals {
            let band = ((signal.snr_db / 2.0).floor() * 2.0) as i32;
            let found = got.contains(&signal.message);
            for tally in [result.by_snr.entry(band).or_default(), &mut result.total] {
                tally.sent += 1;
                tally.found += usize::from(found);
            }
            if !found {
                result
                    .missed
                    .push((slot.index, signal.snr_db, signal.message.clone()));
            }
        }
        for message in &got {
            if !sent.contains(message.as_str()) {
                result.false_decodes.push((slot.index, message.clone()));
            }
        }
    }
    result
}

/// A score as a short Markdown report.
#[must_use]
pub fn report(name: &str, score: &Score) -> String {
    let mut out =
        format!("## {name}\n\n| SNR (dB) | Sent | Found | Found % |\n|---|---|---|---|\n");
    for (band, tally) in &score.by_snr {
        let _ = writeln!(
            out,
            "| {band} to {} | {} | {} | {} |",
            band + 2,
            tally.sent,
            tally.found,
            percent(*tally)
        );
    }
    let _ = writeln!(
        out,
        "| **All** | **{}** | **{}** | **{}** |\n\nFalse decodes: {}",
        score.total.sent,
        score.total.found,
        percent(score.total),
        score.false_decodes.len()
    );
    for (slot, message) in &score.false_decodes {
        let _ = writeln!(out, "- slot {slot}: {message}");
    }
    out
}

#[allow(clippy::cast_precision_loss)] // counts are small
fn percent(tally: Tally) -> String {
    if tally.sent == 0 {
        return "-".to_owned();
    }
    format!("{:.1}", 100.0 * tally.found as f64 / tally.sent as f64)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        Rng, ScenarioSpec, Slot, generate, parse_decode_line, parse_truth, random_message, render,
        score, truth_tsv, wav_bytes,
    };
    use crate::ft8::message::{MessageError, pack};

    #[test]
    fn scenarios_repeat_from_their_seed() {
        let spec = ScenarioSpec::standard(7);
        assert_eq!(generate(&spec), generate(&spec));
        assert_ne!(generate(&spec), generate(&ScenarioSpec::standard(8)));
    }

    #[test]
    fn generated_messages_all_encode() -> Result<(), MessageError> {
        let mut rng = Rng::new(1);
        for _ in 0..2_000 {
            pack(&random_message(&mut rng))?;
        }
        Ok(())
    }

    #[test]
    fn slots_have_distinct_well_spaced_signals() {
        for slot in generate(&ScenarioSpec::standard(3)) {
            assert_eq!(slot.signals.len(), 10);
            for (i, a) in slot.signals.iter().enumerate() {
                assert!((-26.0..=-4.0).contains(&a.snr_db));
                for b in &slot.signals[i + 1..] {
                    assert!((a.base_hz - b.base_hz).abs() >= 60.0);
                    assert_ne!(a.message, b.message);
                }
            }
        }
    }

    #[test]
    fn truth_round_trips() {
        let spec = ScenarioSpec {
            slots: 3,
            ..ScenarioSpec::standard(5)
        };
        let slots = generate(&spec);
        assert_eq!(parse_truth(&truth_tsv(&spec, &slots)), Ok(slots));
        assert_eq!(parse_truth("0\t1500\tx\t-10\tCQ K9KSU EN52"), Err(1));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn noise_and_signal_levels_are_as_specified() -> Result<(), MessageError> {
        let quiet = Slot {
            index: 0,
            signals: Vec::new(),
        };
        let noise = render(&quiet, 1)?;
        let rms =
            (noise.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / noise.len() as f64).sqrt();
        assert!((rms - 300.0).abs() < 3.0, "noise rms {rms}");

        // A +10 dB signal: power in the slot rises by the signal's share.
        let loud = Slot {
            index: 0,
            signals: vec![super::Signal {
                message: "CQ K9KSU EN52".to_owned(),
                base_hz: 1500.0,
                dt_s: 0.0,
                snr_db: 10.0,
            }],
        };
        let with = render(&loud, 1)?;
        let added: f64 = with
            .iter()
            .zip(&noise)
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
            .sum::<f64>()
            / (79.0 * 1920.0)
            / (300.0 * 300.0);
        // Signal power relative to noise in 2,500 Hz: 10 dB = 10x.
        let snr = added / (2_500.0 / 6_000.0);
        assert!((snr - 10.0).abs() < 0.3, "measured snr ratio {snr}");
        Ok(())
    }

    #[test]
    fn wav_header_is_well_formed() {
        let bytes = wav_bytes(&[0, 1, -1]);
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(&bytes[24..28], &12_000_u32.to_le_bytes());
        assert_eq!(&bytes[40..44], &6_u32.to_le_bytes());
    }

    #[test]
    fn reads_jt9_output() {
        assert_eq!(
            parse_decode_line("000000 -18 -0.0 1500 ~  CQ K9KSU EN52"),
            Some("CQ K9KSU EN52".to_owned())
        );
        assert_eq!(
            parse_decode_line("000000 -22  0.3 1234 ~  W1AW K9KSU -15         a2"),
            Some("W1AW K9KSU -15".to_owned())
        );
        assert_eq!(
            parse_decode_line("000000 -23  0.1  812 ~  K1ABC W9XYZ RR73 ?"),
            Some("K1ABC W9XYZ RR73".to_owned())
        );
        assert_eq!(parse_decode_line("<DecodeFinished>   0   1        0"), None);
    }

    #[test]
    fn scores_found_missed_and_false() {
        let truth = parse_truth("0\t1000\t0\t-10\tCQ K9KSU EN52\n0\t1500\t0\t-25\tW1AW K9KSU 73\n")
            .unwrap_or_default();
        let mut decoded = BTreeMap::new();
        decoded.insert(0, vec!["cq k9ksu en52".to_owned(), "CQ N0CALL".to_owned()]);
        let s = score(&truth, &decoded);
        assert_eq!((s.total.sent, s.total.found), (2, 1));
        assert_eq!(s.false_decodes, vec![(0, "CQ N0CALL".to_owned())]);
        assert_eq!(s.missed.len(), 1);
        assert_eq!(s.by_snr.get(&-10).map(|t| t.found), Some(1));
        assert_eq!(s.by_snr.get(&-26).map(|t| t.found), Some(0));
    }
}
