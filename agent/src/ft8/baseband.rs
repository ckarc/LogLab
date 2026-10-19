// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! One signal's slice of the band, moved down to zero hertz.
//!
//! The whole slot is transformed once. For each candidate, the 75 Hz around
//! it is cut out of that spectrum, shifted so the signal's lowest tone sits
//! at zero, and transformed back at a 200 Hz sample rate: 32 samples per
//! symbol, with tone `t` at exactly `t` cycles per symbol. Timing and
//! frequency can then be tuned finely and cheaply, and each symbol's eight
//! tones measured as complex values.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::TONE_SPACING_HZ;

/// Length of the whole-slot transform: 16 s, so 0.0625 Hz per bin.
const LONG: usize = 192_000;
/// Decimation from 12,000 to 200 samples per second.
const DECIMATION: usize = 60;
/// Length of a baseband slice: also 16 s.
const SHORT: usize = LONG / DECIMATION;
/// Baseband samples per symbol.
pub(super) const SAMPLES_PER_SYMBOL: usize = 32;
/// Baseband sample rate, in samples per second.
pub(super) const RATE: f32 = 200.0;
/// Hertz per bin of the whole-slot transform.
const BIN_HZ: f64 = 0.0625; // 12,000 Hz / 192,000 bins
/// Band kept below the lowest tone and above the highest, in hertz.
const GUARD_HZ: f64 = 12.5;
/// Width of the raised-cosine taper at each edge of the band, in hertz.
const TAPER_HZ: f64 = 6.25;

/// The slot's spectrum, ready to give out baseband slices.
pub(super) struct Baseband {
    spectrum: Vec<Complex32>,
    inverse: Arc<dyn Fft<f32>>,
}

impl Baseband {
    /// Transforms a slot of 12,000-sample-per-second audio.
    pub(super) fn new(audio: &[f32]) -> Self {
        let mut planner = FftPlanner::new();
        let mut spectrum: Vec<Complex32> = (0..LONG)
            .map(|i| Complex32::new(audio.get(i).copied().unwrap_or(0.0), 0.0))
            .collect();
        planner.plan_fft_forward(LONG).process(&mut spectrum);
        Self {
            spectrum,
            inverse: planner.plan_fft_inverse(SHORT),
        }
    }

    /// The signal whose lowest tone is at `base_hz`, at baseband: sample `m`
    /// lines up with audio sample `60 m`.
    pub(super) fn slice(&self, base_hz: f64) -> Vec<Complex32> {
        let mut out = vec![Complex32::new(0.0, 0.0); SHORT];
        let base = base_hz / BIN_HZ;
        let below = (GUARD_HZ / BIN_HZ).round();
        let above = ((7.0 * TONE_SPACING_HZ + GUARD_HZ) / BIN_HZ).round();
        let taper = TAPER_HZ / BIN_HZ;
        let mut offset = -below;
        while offset <= above {
            let source = (base + offset).round();
            if source >= 1.0 && source < to_f64(LONG / 2) {
                let edge = (offset + below).min(above - offset);
                let weight = if edge < taper {
                    0.5 - 0.5 * (std::f64::consts::PI * edge / taper).cos()
                } else {
                    1.0
                };
                let target = wrap(offset, SHORT);
                out[target] = self.spectrum[to_index(source)] * to_f32(weight);
            }
            offset += 1.0;
        }
        self.inverse.process(&mut out);
        out
    }
}

/// Shifts a baseband slice down in frequency by `hz`.
pub(super) fn shift(samples: &[Complex32], hz: f32) -> Vec<Complex32> {
    let step = -2.0 * PI * hz / RATE;
    samples
        .iter()
        .enumerate()
        .map(|(m, &s)| s * Complex32::from_polar(1.0, step * to_f32_index(m)))
        .collect()
}

/// Measures the eight FT8 tones of one symbol: a 32-point DFT, only at the
/// eight bins that carry tones.
pub(super) struct ToneMeter {
    twiddles: [[Complex32; SAMPLES_PER_SYMBOL]; 8],
}

impl ToneMeter {
    pub(super) fn new() -> Self {
        let mut twiddles = [[Complex32::new(0.0, 0.0); SAMPLES_PER_SYMBOL]; 8];
        for (t, row) in twiddles.iter_mut().enumerate() {
            for (i, w) in row.iter_mut().enumerate() {
                let angle = -2.0 * PI * to_f32_index(t * i) / to_f32_index(SAMPLES_PER_SYMBOL);
                *w = Complex32::from_polar(1.0, angle);
            }
        }
        Self { twiddles }
    }

    /// The complex amplitude of tone `tone` in the symbol starting at
    /// sample `start`; samples outside the slice count as zero.
    pub(super) fn tone(&self, samples: &[Complex32], start: isize, tone: usize) -> Complex32 {
        let mut sum = Complex32::new(0.0, 0.0);
        for (i, w) in self.twiddles[tone].iter().enumerate() {
            if let Some(&s) = usize::try_from(start + to_isize(i))
                .ok()
                .and_then(|m| samples.get(m))
            {
                sum += s * w;
            }
        }
        sum
    }

    /// Moves `tones`, measured for the symbol starting at `start`, on to the
    /// symbol starting one sample later (a sliding DFT).
    pub(super) fn slide(&self, tones: &mut [Complex32; 8], samples: &[Complex32], start: isize) {
        let at = |i: isize| {
            usize::try_from(i)
                .ok()
                .and_then(|m| samples.get(m))
                .copied()
                .unwrap_or_default()
        };
        let change = at(start + to_isize(SAMPLES_PER_SYMBOL)) - at(start);
        for (t, tone) in tones.iter_mut().enumerate() {
            // Advancing one sample turns tone t by one step of its own
            // rotation, the conjugate of its first twiddle.
            *tone = (*tone + change) * self.twiddles[t][1].conj();
        }
    }

    /// All eight tones of the symbol starting at `start`.
    pub(super) fn tones(&self, samples: &[Complex32], start: isize) -> [Complex32; 8] {
        std::array::from_fn(|t| self.tone(samples, start, t))
    }
}

/// A signed bin offset as an index into a buffer of length `n`.
#[allow(clippy::cast_possible_truncation)] // small, rounded
fn wrap(offset: f64, n: usize) -> usize {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    usize::try_from((offset as i64 % n + n) % n).unwrap_or(0)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked range
fn to_index(x: f64) -> usize {
    x as usize
}

#[allow(clippy::cast_precision_loss)] // small
fn to_f64(n: usize) -> f64 {
    n as f64
}

#[allow(clippy::cast_possible_truncation)] // sample weights
fn to_f32(x: f64) -> f32 {
    x as f32
}

#[allow(clippy::cast_precision_loss)] // indexes below 2^24
fn to_f32_index(n: usize) -> f32 {
    n as f32
}

fn to_isize(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Complex32, ToneMeter};

    #[test]
    fn sliding_matches_measuring_afresh() {
        let samples: Vec<Complex32> = (0..200_u16)
            .map(|i| {
                let x = f32::from(i);
                Complex32::new((x * 0.37).sin(), (x * 0.11).cos())
            })
            .collect();
        let meter = ToneMeter::new();
        // Starts before the slice, inside it, and running off its end.
        let mut tones = meter.tones(&samples, -10);
        for start in -10..190 {
            let fresh = meter.tones(&samples, start + 1);
            meter.slide(&mut tones, &samples, start);
            for (a, b) in tones.iter().zip(&fresh) {
                assert!((a - b).norm() < 1e-3, "start {start}: {a} vs {b}");
            }
        }
    }
}
