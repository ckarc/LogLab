// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Removing a decoded signal from the audio, so it cannot hide weaker
//! signals near it or leave behind energy that looks like another signal.
//!
//! The decoded message is re-encoded into the exact waveform that was sent.
//! Mixing the audio with that waveform and smoothing the result over about
//! one symbol measures the signal's amplitude and phase as they drift
//! through the transmission (fading, a small frequency error); that
//! measured copy of the signal is then subtracted.

use rustfft::num_complex::Complex32;

use super::encode::Tones;
use super::{SAMPLES_PER_SYMBOL, wave};

/// Audio samples either side of the given start searched for the best
/// alignment, and the step between tries.
const ALIGN_REACH: isize = 30;
const ALIGN_STEP: usize = 5;

/// Subtracts the signal sending `tones` with its lowest tone at `base_hz`,
/// whose first symbol starts near audio sample `start` (which may be
/// negative: a signal that began before the recording).
pub(super) fn subtract(audio: &mut [f32], tones: &Tones, base_hz: f64, start: isize) {
    let reference = wave::synthesize_complex(tones, base_hz);
    let start = align(audio, &reference, start);
    let n = reference.len();
    let mut mixed = vec![Complex32::new(0.0, 0.0); n];
    let mut weight = vec![0.0_f32; n];
    for (i, r) in reference.iter().enumerate() {
        if let Some(&x) = sample(audio, start, i) {
            mixed[i] = r.conj() * x;
            weight[i] = r.norm_sqr();
        }
    }
    let mixed = smooth(&smooth(&mixed));
    let weight = smooth(&smooth(&weight));
    for (i, r) in reference.iter().enumerate() {
        let Some(index) = position(audio.len(), start, i) else {
            continue;
        };
        if weight[i] > 1e-3 {
            // The audio is the real part of amplitude times reference, so
            // mixing finds half the amplitude times the reference's power.
            let amplitude = mixed[i] * (2.0 / weight[i]);
            audio[index] -= (amplitude * r).re;
        }
    }
}

/// The start, within [`ALIGN_REACH`] of `start`, at which the audio best
/// matches the reference.
fn align(audio: &[f32], reference: &[Complex32], start: isize) -> isize {
    (-ALIGN_REACH..=ALIGN_REACH)
        .step_by(ALIGN_STEP)
        .map(|offset| {
            let at = start + offset;
            let mut sum = Complex32::new(0.0, 0.0);
            for (i, r) in reference.iter().enumerate() {
                if let Some(&x) = sample(audio, at, i) {
                    sum += r.conj() * x;
                }
            }
            (sum.norm_sqr(), at)
        })
        .fold(
            (f32::NEG_INFINITY, start),
            |a, b| if b.0 > a.0 { b } else { a },
        )
        .1
}

fn position(len: usize, start: isize, i: usize) -> Option<usize> {
    let index = usize::try_from(start + isize::try_from(i).ok()?).ok()?;
    (index < len).then_some(index)
}

fn sample(audio: &[f32], start: isize, i: usize) -> Option<&f32> {
    audio.get(position(audio.len(), start, i)?)
}

/// A centred moving average one symbol long.
fn smooth<T>(values: &[T]) -> Vec<T>
where
    T: Copy
        + Default
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::Mul<f32, Output = T>,
{
    let half = SAMPLES_PER_SYMBOL / 2;
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(T::default());
    let mut total = T::default();
    for &v in values {
        total = total + v;
        prefix.push(total);
    }
    (0..values.len())
        .map(|i| {
            let low = i.saturating_sub(half);
            let high = (i + half).min(values.len());
            (prefix[high] - prefix[low]) * (1.0 / to_f32(high - low))
        })
        .collect()
}

#[allow(clippy::cast_precision_loss)] // window lengths
fn to_f32(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use super::subtract;
    use crate::ft8::bench::Rng;
    use crate::ft8::encode::tones;
    use crate::ft8::message::MessageError;
    use crate::ft8::wave::synthesize;

    #[test]
    fn removes_a_signal_and_leaves_the_noise() -> Result<(), MessageError> {
        let sent = tones("CQ K9KSU EN52")?;
        let mut rng = Rng::new(5);
        #[allow(clippy::cast_possible_truncation)]
        let noise: Vec<f32> = (0..180_000).map(|_| rng.gaussian() as f32).collect();
        let start = 6_017;
        let mut audio = noise.clone();
        // A strong signal, a quarter of a hertz off where it is said to be,
        // that fades to half strength along the way.
        let wave = synthesize(&sent, 1500.25);
        let length = wave.len();
        #[allow(clippy::cast_precision_loss)]
        for (i, w) in wave.iter().enumerate() {
            audio[start + i] += 4.0 * w * (1.0 - 0.5 * i as f32 / length as f32);
        }
        subtract(&mut audio, &sent, 1500.0, 6_000);
        let residual: f32 = audio
            .iter()
            .zip(&noise)
            .map(|(a, n)| (a - n).powi(2))
            .sum::<f32>();
        let signal: f32 = wave.iter().map(|w| (4.0 * w).powi(2)).sum::<f32>();
        assert!(
            residual < signal / 100.0,
            "left {:.1}% of the signal",
            100.0 * residual / signal
        );
        Ok(())
    }
}
