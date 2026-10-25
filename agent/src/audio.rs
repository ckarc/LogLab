// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Moving audio between two sound devices whose clocks don't agree.
//!
//! A radio's USB sound chip and a virtual cable each run on their own clock.
//! Even both set to 48 kHz, they differ by tens of parts per million, and
//! sometimes they run at different rates altogether (48 kHz and 44.1 kHz).
//! Left alone, audio piles up or runs dry, and every few minutes there is a
//! glitch, which costs decodes.
//!
//! So audio from one device goes into a [`Buffer`], and the other device
//! takes it out through a [`Resampler`]. The resampler watches how full the
//! buffer is: when it fills, it reads very slightly faster, and when it
//! drains, slightly slower, interpolating between samples so there are no
//! clicks. The buffer settles at about [`TARGET`] of audio.
//!
//! Everything here is mono `f32` in -1.0 to 1.0, and plain Rust, so it runs
//! and is tested on every platform. The sound devices themselves are in
//! `audio_windows`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How much audio the buffer settles at: enough to ride out a late callback,
/// little enough not to delay anything noticeably.
pub const TARGET: Duration = Duration::from_millis(100);
/// The most audio the buffer holds before it throws the oldest away.
pub const MAX: Duration = Duration::from_secs(2);
/// How hard the resampler steers toward the target: the speed change per
/// unit of fill error.
const GAIN: f64 = 0.05;
/// The most it ever changes speed by: 1%, far more than any two clocks
/// differ, far less than anyone could hear.
const MAX_CORRECTION: f64 = 0.01;
/// How slowly the steering follows the fill level, so one burst of samples
/// doesn't jerk the speed around.
const SMOOTHING: f64 = 0.01;

/// Audio waiting between the capturing device and the playing one, with the
/// numbers worth showing a person.
pub struct Buffer {
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
    rate: u32,
    /// The loudest sample since the meter was last read, as `f32` bits.
    peak: AtomicU32,
    /// Times the playing side found nothing to play.
    gaps: AtomicU64,
    /// Samples thrown away because the buffer was full.
    dropped: AtomicU64,
}

impl Buffer {
    /// An empty buffer for audio arriving at `rate` samples a second.
    #[must_use]
    pub fn new(rate: u32) -> Arc<Self> {
        let capacity = samples_in(MAX, rate).max(1);
        Arc::new(Self {
            samples: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            rate,
            peak: AtomicU32::new(0),
            gaps: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    /// Adds captured audio: one sample per frame, already mixed to mono.
    pub fn push(&self, frames: impl Iterator<Item = f32>) {
        let Ok(mut samples) = self.samples.lock() else {
            return;
        };
        let mut peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        for sample in frames {
            // Anything outside the range a sound card can mean is clipped,
            // and anything that isn't a number becomes silence.
            let sample = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            peak = peak.max(sample.abs());
            if samples.len() >= self.capacity {
                samples.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            samples.push_back(sample);
        }
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
    }

    /// How much audio is waiting.
    #[must_use]
    pub fn level(&self) -> Duration {
        let waiting = self.samples.lock().map_or(0, |s| s.len());
        Duration::from_secs_f64(count(waiting) / f64::from(self.rate.max(1)))
    }

    /// The loudest sample since the last call, in dBFS (0 is full scale),
    /// and starts measuring again. `None` if everything was silent.
    pub fn take_peak_dbfs(&self) -> Option<f64> {
        let peak = f32::from_bits(self.peak.swap(0, Ordering::Relaxed));
        (peak > 0.0).then(|| 20.0 * f64::from(peak).log10())
    }

    /// Times the playing side found nothing to play.
    #[must_use]
    pub fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// Samples thrown away because the buffer was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Takes audio out of a [`Buffer`] at the playing device's rate, steering to
/// keep the buffer near [`TARGET`].
pub struct Resampler {
    buffer: Arc<Buffer>,
    /// Input samples per output sample, before any steering.
    nominal: f64,
    /// Where the next output falls between `last` and the buffer's first
    /// sample, from 0.0 to 1.0.
    position: f64,
    /// The sample just behind `position`.
    last: f32,
    /// The fill level, smoothed, in input samples.
    smoothed: f64,
    target: f64,
    /// After running dry, waiting for the buffer to refill before playing.
    priming: bool,
}

impl Resampler {
    /// Takes audio out of `buffer` for a device playing `out_rate` samples a
    /// second.
    #[must_use]
    pub fn new(buffer: Arc<Buffer>, out_rate: u32) -> Self {
        let in_rate = f64::from(buffer.rate.max(1));
        let target = count(samples_in(TARGET, buffer.rate));
        Self {
            nominal: in_rate / f64::from(out_rate.max(1)),
            buffer,
            position: 0.0,
            last: 0.0,
            smoothed: target,
            target,
            priming: true,
        }
    }

    /// Fills `out`, one mono sample per frame.
    pub fn fill(&mut self, out: &mut [f32]) {
        let Ok(mut samples) = self.buffer.samples.lock() else {
            out.fill(0.0);
            return;
        };
        if self.priming {
            if count(samples.len()) < self.target {
                out.fill(0.0);
                return;
            }
            self.priming = false;
        }
        let fill = count(samples.len());
        self.smoothed += (fill - self.smoothed) * SMOOTHING;
        let error = (self.smoothed - self.target) / self.target;
        let step = self.nominal * (1.0 + (error * GAIN).clamp(-MAX_CORRECTION, MAX_CORRECTION));
        for slot in out.iter_mut() {
            while self.position >= 1.0 {
                match samples.pop_front() {
                    Some(sample) => {
                        self.last = sample;
                        self.position -= 1.0;
                    }
                    None => break,
                }
            }
            let Some(&next) = samples.front() else {
                // Ran dry: silence until there's a cushion again.
                self.buffer.gaps.fetch_add(1, Ordering::Relaxed);
                self.priming = true;
                self.position = 0.0;
                *slot = 0.0;
                continue;
            };
            if self.priming {
                *slot = 0.0;
                continue;
            }
            let (from, to) = (f64::from(self.last), f64::from(next));
            *slot = sample(from + (to - from) * self.position);
            self.position += step;
        }
    }
}

/// How many samples `span` is at `rate`, by whole-number arithmetic.
#[must_use]
pub fn samples_in(span: Duration, rate: u32) -> usize {
    let samples = span.as_micros() * u128::from(rate) / 1_000_000;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

/// A count of samples as a number to steer with. The buffer never holds
/// more than [`MAX`] of audio, a few hundred thousand samples, so the count
/// fits a `u32` and converts exactly.
fn count(samples: usize) -> f64 {
    u32::try_from(samples).map_or(f64::from(u32::MAX), f64::from)
}

/// An interpolated value back as a sample. Every value here lies between
/// two samples already in -1.0 to 1.0, so narrowing to `f32` only rounds.
#[allow(clippy::cast_possible_truncation)]
fn sample(value: f64) -> f32 {
    value as f32
}

// The tests build tones and measure them from sample counts, which is
// arithmetic on small numbers where the casts can't lose anything.
#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use std::f32::consts::TAU;
    use std::time::Duration;

    use super::{Buffer, Resampler, TARGET, count, samples_in};

    /// A tone at `hz`, `count` samples long at `rate`, starting at sample
    /// `start`.
    fn tone(hz: f32, rate: u32, start: usize, count: usize) -> Vec<f32> {
        (start..start + count)
            .map(|n| 0.5 * (TAU * hz * n as f32 / rate as f32).sin())
            .collect()
    }

    /// Runs a capture at `in_rate` (off by `ppm`) into a playback at
    /// `out_rate` for `seconds`, in 10 ms blocks, and returns what played.
    fn run(
        in_rate: u32,
        ppm: f64,
        out_rate: u32,
        seconds: usize,
    ) -> (Vec<f32>, std::sync::Arc<Buffer>) {
        let buffer = Buffer::new(in_rate);
        let mut resampler = Resampler::new(std::sync::Arc::clone(&buffer), out_rate);
        let actual_in = f64::from(in_rate) * (1.0 + ppm / 1e6);
        let mut produced = 0.0_f64;
        let mut sent = 0;
        let mut played = Vec::new();
        for block in 0..seconds * 100 {
            produced += actual_in / 100.0;
            let due = produced as usize - sent;
            buffer.push(tone(1000.0, in_rate, sent, due).into_iter());
            sent += due;
            let mut out = vec![0.0; out_rate as usize / 100];
            resampler.fill(&mut out);
            if block >= 100 {
                played.extend(out);
            }
        }
        (played, buffer)
    }

    /// The tone's frequency, from how often it crosses zero going up.
    fn frequency(samples: &[f32], rate: u32) -> f64 {
        let rises = samples
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        rises as f64 * f64::from(rate) / samples.len() as f64
    }

    #[test]
    fn holds_the_buffer_steady_when_clocks_drift() {
        // At 1000 ppm the clocks part by 120 ms in two minutes, more than
        // the whole cushion: without steering this would run dry or pile up.
        for ppm in [-1000.0, -200.0, 0.0, 200.0, 1000.0] {
            let (_, buffer) = run(48_000, ppm, 48_000, 120);
            let level = buffer.level();
            assert!(
                level > TARGET * 3 / 4 && level < TARGET * 5 / 4,
                "{ppm} ppm: buffer drifted to {level:?}"
            );
            assert!(buffer.gaps() <= 1, "{ppm} ppm: {} gaps", buffer.gaps());
            assert_eq!(buffer.dropped(), 0, "{ppm} ppm");
        }
    }

    #[test]
    fn changes_rate_without_changing_pitch() {
        for (in_rate, out_rate) in [(48_000, 48_000), (48_000, 44_100), (44_100, 48_000)] {
            let (played, buffer) = run(in_rate, 30.0, out_rate, 20);
            let hz = frequency(&played, out_rate);
            assert!((hz - 1000.0).abs() < 2.0, "{in_rate}->{out_rate}: {hz} Hz");
            assert!(
                buffer.gaps() <= 1,
                "{in_rate}->{out_rate}: {} gaps",
                buffer.gaps()
            );
        }
    }

    #[test]
    fn plays_silence_until_there_is_a_cushion() {
        let buffer = Buffer::new(48_000);
        let mut resampler = Resampler::new(std::sync::Arc::clone(&buffer), 48_000);
        buffer.push(std::iter::repeat_n(0.5, 100));
        let mut out = vec![1.0; 480];
        resampler.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "not enough waiting to start");
        buffer.push(std::iter::repeat_n(0.5, samples_in(TARGET, 48_000)));
        resampler.fill(&mut out);
        assert!(out[100..].iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn never_holds_more_than_its_maximum() {
        let buffer = Buffer::new(8_000);
        buffer.push(std::iter::repeat_n(0.1, 8_000 * 5));
        assert_eq!(buffer.level(), Duration::from_secs(2));
        assert_eq!(buffer.dropped(), 8_000 * 3);
    }

    #[test]
    fn cleans_up_what_it_is_given() {
        let buffer = Buffer::new(48_000);
        buffer.push([f32::NAN, f32::INFINITY, 3.0, -0.25].into_iter());
        let peak = buffer.take_peak_dbfs().unwrap_or(f64::NAN);
        assert!(peak.abs() < 1e-9, "a clipped sample reads 0 dBFS: {peak}");
        assert_eq!(buffer.take_peak_dbfs(), None, "and the meter starts again");
    }

    #[test]
    fn counts_samples_exactly() {
        assert_eq!(samples_in(TARGET, 48_000), 4_800);
        assert_eq!(samples_in(Duration::from_secs(2), 44_100), 88_200);
        assert_eq!(samples_in(Duration::ZERO, 48_000), 0);
        assert!((count(4_800) - 4_800.0).abs() < f64::EPSILON);
        assert!((count(usize::MAX) - f64::from(u32::MAX)).abs() < 1.0);
    }
}
