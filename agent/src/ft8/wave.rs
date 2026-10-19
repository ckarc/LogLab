// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FT8 tones to audio: continuous-phase 8-FSK with Gaussian frequency
//! smoothing (GFSK, bandwidth-time product 2), and short raised-cosine ramps
//! at the start and end so the transmission does not click.

use std::f64::consts::PI;

use rustfft::num_complex::Complex32;

use super::encode::Tones;
use super::{SAMPLE_RATE, SAMPLES_PER_SYMBOL, SYMBOLS, TONE_SPACING_HZ};

/// FT8's Gaussian smoothing bandwidth-time product.
const BT: f64 = 2.0;

/// Audio for one transmission whose lowest tone is at `base_hz`: 79 symbols,
/// 151,680 samples at 12,000 per second, peak amplitude 1.
#[must_use]
pub fn synthesize(tones: &Tones, base_hz: f64) -> Vec<f32> {
    let (phases, envelope) = phases_and_envelope(tones, base_hz);
    phases
        .iter()
        .zip(&envelope)
        .map(|(p, e)| to_f32(p.sin()) * to_f32(*e))
        .collect()
}

/// The same transmission as a complex (analytic) signal, `e^(i phase)`
/// times the ramps: its real part is the audio shifted a quarter cycle.
/// Used to measure and subtract a decoded signal.
#[must_use]
pub fn synthesize_complex(tones: &Tones, base_hz: f64) -> Vec<Complex32> {
    let (phases, envelope) = phases_and_envelope(tones, base_hz);
    phases
        .iter()
        .zip(&envelope)
        .map(|(p, e)| Complex32::from_polar(to_f32(*e), to_f32(*p)))
        .collect()
}

/// The phase of every sample, and the ramp envelope.
fn phases_and_envelope(tones: &Tones, base_hz: f64) -> (Vec<f64>, Vec<f64>) {
    let nsps = SAMPLES_PER_SYMBOL;
    let pulse = gaussian_pulse();
    let rate = f64::from(SAMPLE_RATE);
    let peak_step = 2.0 * PI * TONE_SPACING_HZ / rate;

    // Phase step per sample, with one extra symbol before and after so the
    // smoothing of the first and last tones has something to lean on.
    let mut step = vec![2.0 * PI * base_hz / rate; (SYMBOLS + 2) * nsps];
    for (i, &tone) in tones.iter().enumerate() {
        let start = i * nsps;
        for (s, p) in step[start..start + 3 * nsps].iter_mut().zip(&pulse) {
            *s += peak_step * f64::from(tone) * p;
        }
    }
    let first = f64::from(tones[0]);
    let last = f64::from(tones[SYMBOLS - 1]);
    for j in 0..2 * nsps {
        step[j] += peak_step * pulse[j + nsps] * first;
        step[j + SYMBOLS * nsps] += peak_step * pulse[j] * last;
    }

    let mut phase = 0.0_f64;
    let phases: Vec<f64> = step[nsps..(SYMBOLS + 1) * nsps]
        .iter()
        .map(|s| {
            let now = phase;
            phase = (phase + s) % (2.0 * PI);
            now
        })
        .collect();

    let ramp = nsps / 8;
    let total = phases.len();
    let mut envelope = vec![1.0; total];
    for i in 0..ramp {
        let e = (1.0 - (PI * index_f64(i) / index_f64(ramp)).cos()) / 2.0;
        envelope[i] = e;
        envelope[total - 1 - i] = e;
    }
    (phases, envelope)
}

/// The Gaussian-smoothed frequency pulse of one symbol, three symbols long.
fn gaussian_pulse() -> Vec<f64> {
    let nsps = index_f64(SAMPLES_PER_SYMBOL);
    // pi * sqrt(2 / ln 2)
    let k = PI * (2.0 / 2.0_f64.ln()).sqrt();
    (0..3 * SAMPLES_PER_SYMBOL)
        .map(|i| {
            let t = index_f64(i) / nsps - 1.5;
            (erf(k * BT * (t + 0.5)) - erf(k * BT * (t - 0.5))) / 2.0
        })
        .collect()
}

/// The error function, to within 1.2e-7 (Numerical Recipes' `erfc`
/// Chebyshev fit).
fn erf(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let erfc = t * poly.exp();
    if x >= 0.0 { 1.0 - erfc } else { erfc - 1.0 }
}

/// An audio sample index as a float. Indexes here stay far below 2^52, so the
/// conversion is exact.
#[allow(clippy::cast_precision_loss)]
fn index_f64(i: usize) -> f64 {
    i as f64
}

/// Narrows a sample to the 32-bit float audio is stored in.
#[allow(clippy::cast_possible_truncation)]
fn to_f32(x: f64) -> f32 {
    x as f32
}

#[cfg(test)]
mod tests {
    use super::{erf, synthesize};
    use crate::ft8::{SAMPLES_PER_SYMBOL, SYMBOLS};

    #[test]
    fn erf_is_accurate() {
        assert!((erf(0.0)).abs() < 1e-7);
        assert!((erf(0.5) - 0.520_499_877_8).abs() < 2e-7);
        assert!((erf(-1.5) + 0.966_105_146_5).abs() < 2e-7);
    }

    #[test]
    fn a_steady_tone_has_the_expected_frequency() {
        // All tones 0 at 1000 Hz: count rising zero crossings over the middle
        // second, away from the ramps.
        let audio = synthesize(&[0; SYMBOLS], 1000.0);
        assert_eq!(audio.len(), SYMBOLS * SAMPLES_PER_SYMBOL);
        let middle = &audio[60_000..72_000];
        let crossings = middle
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!((999..=1001).contains(&crossings), "{crossings} crossings");
        assert!(audio.iter().all(|s| s.abs() <= 1.0));
    }
}
