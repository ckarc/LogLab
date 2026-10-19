// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Simulated HF propagation for the test bench: what the ionosphere and the
//! two stations' radios do to an FT8 signal on its way to the receiver.
//!
//! A clean bench recording holds each signal exactly as it was sent. On the
//! air a signal also
//!
//! - **fades**: its strength and phase wander as the paths through the
//!   ionosphere change. Watterson's model (and ITU-R F.1487, which is built
//!   on it) treats each path as a complex Gaussian gain whose spectrum is a
//!   Gaussian of width "frequency spread" (twice its standard deviation):
//!   *Rayleigh* fading when nothing else is there, *Rician* when a steady
//!   component is there too, in the ratio K;
//! - **fades deeply** for part of the transmission, a dip of many decibels
//!   lasting a few seconds;
//! - **drifts**: the transmitter's oscillator warms up, so the frequency
//!   moves steadily, or wanders back and forth, or curves;
//! - **jitters**: oscillator phase noise, modelled as a random walk of the
//!   phase whose spectral width is the given linewidth;
//! - **arrives twice**: a second path, delayed by a few milliseconds and
//!   often fading on its own, and sometimes shifted in frequency (Doppler)
//!   against the first.
//!
//! A [`Channel`] describes all of that; [`Channel::apply`] passes one
//! signal through it. The average power through the channel is the power
//! sent, so a signal's stated signal-to-noise ratio is its average over the
//! fading (except for a deep fade, which is a dip below that average).
//! Every random choice comes from the generator it is given, so a scenario
//! repeats exactly from its seed.
//!
//! This is bench code: it only ever shapes recordings written to files.

use std::f64::consts::PI;
use std::fmt;

use rustfft::FftPlanner;
use rustfft::num_complex::{Complex32, Complex64};

use super::SAMPLE_RATE;
use super::bench::Rng;

/// Sample rate the fading gains are generated at, in hertz; they are
/// interpolated up to the audio rate. Enough for spreads up to 20 Hz.
const FADING_RATE: f64 = 100.0;

/// Length of the generated fading process, in its samples: 82 seconds, so
/// its spectrum is resolved to about a hundredth of a hertz and even a
/// slow spread of 0.05 Hz is shaped properly.
const FADING_LEN: usize = 8_192;

/// Length of the raised-cosine edges of a deep fade, in seconds.
const DEEP_FADE_EDGE_S: f64 = 0.3;

/// What propagation does to a signal. All zeros ([`Channel::CLEAR`]) is a
/// clean channel: the signal arrives exactly as sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Channel {
    /// Frequency spread of the fading, in hertz (twice the standard
    /// deviation of its Gaussian Doppler spectrum); 0 for no fading.
    pub spread_hz: f64,
    /// Rician K factor, the steady component's power over the fading
    /// component's, in decibels; `None` for pure Rayleigh fading. Applies
    /// only when `spread_hz` is above zero.
    pub rician_k_db: Option<f64>,
    /// Depth of one deep fade somewhere in the transmission, in decibels;
    /// 0 for none.
    pub deep_fade_db: f64,
    /// Length of that deep fade, in seconds, edges included.
    pub deep_fade_s: f64,
    /// Steady frequency drift, in hertz per second; its direction is chosen
    /// at random for each signal.
    pub drift_hz_per_s: f64,
    /// Curved drift: how far the frequency at the start and end of the
    /// transmission is from the frequency in its middle, in hertz.
    pub bend_hz: f64,
    /// Wandering drift: the peak of a sinusoidal frequency wobble, in hertz.
    pub wobble_hz: f64,
    /// Period of that wobble, in seconds.
    pub wobble_period_s: f64,
    /// Phase noise: the linewidth of the random walk of the phase, in hertz.
    pub phase_noise_hz: f64,
    /// Delay of a second path, in milliseconds; 0 for one path only.
    pub echo_ms: f64,
    /// Power of the second path relative to the first, in decibels.
    pub echo_db: f64,
    /// Frequency shift of the second path relative to the first (a
    /// differential Doppler shift), in hertz.
    pub echo_shift_hz: f64,
}

impl Channel {
    /// The clean channel.
    pub const CLEAR: Self = Self {
        spread_hz: 0.0,
        rician_k_db: None,
        deep_fade_db: 0.0,
        deep_fade_s: 0.0,
        drift_hz_per_s: 0.0,
        bend_hz: 0.0,
        wobble_hz: 0.0,
        wobble_period_s: 0.0,
        phase_noise_hz: 0.0,
        echo_ms: 0.0,
        echo_db: 0.0,
        echo_shift_hz: 0.0,
    };

    /// Whether this is the clean channel.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        *self == Self::CLEAR
    }

    /// Reads a channel from `name=value` settings separated by commas (the
    /// names are those [`fmt::Display`] writes), or from one of the named
    /// presets of [`Channel::preset`], optionally followed by settings that
    /// change it (`moderate,drift=0.1`).
    ///
    /// # Errors
    ///
    /// A description of the first setting that could not be read.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut channel = Self::CLEAR;
        for (i, part) in text
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
        {
            let Some((name, value)) = part.split_once('=') else {
                channel = (i == 0)
                    .then(|| Self::preset(part))
                    .flatten()
                    .ok_or_else(|| format!("unknown channel or setting: {part}"))?;
                continue;
            };
            if name == "k" {
                channel.rician_k_db = if value == "none" {
                    None
                } else {
                    Some(number(name, value)?)
                };
                continue;
            }
            let v = number(name, value)?;
            match name {
                "spread" => channel.spread_hz = v,
                "deepfade" => channel.deep_fade_db = v,
                "fadelen" => channel.deep_fade_s = v,
                "drift" => channel.drift_hz_per_s = v,
                "bend" => channel.bend_hz = v,
                "wobble" => channel.wobble_hz = v,
                "period" => channel.wobble_period_s = v,
                "phasenoise" => channel.phase_noise_hz = v,
                "echo" => channel.echo_ms = v,
                "echodb" => channel.echo_db = v,
                "echoshift" => channel.echo_shift_hz = v,
                _ => return Err(format!("unknown channel setting: {name}")),
            }
        }
        Ok(channel)
    }

    /// Named channels. The three ITU-R F.1487 mid-latitude conditions are
    /// two equal paths fading independently: `quiet` (0.1 Hz spread, 0.5 ms
    /// apart), `moderate` (0.5 Hz, 1 ms) and `disturbed` (1 Hz, 2 ms);
    /// `flutter` is its disturbed high-latitude one (30 Hz is beyond FT8;
    /// 10 Hz here, 7 ms).
    #[must_use]
    pub fn preset(name: &str) -> Option<Self> {
        let watterson = |spread_hz, echo_ms| Self {
            spread_hz,
            echo_ms,
            ..Self::CLEAR
        };
        match name {
            "clear" => Some(Self::CLEAR),
            "quiet" => Some(watterson(0.1, 0.5)),
            "moderate" => Some(watterson(0.5, 1.0)),
            "disturbed" => Some(watterson(1.0, 2.0)),
            "flutter" => Some(watterson(10.0, 7.0)),
            _ => None,
        }
    }

    /// Passes `signal`, a transmission as a complex (analytic) waveform at
    /// 12,000 samples per second, through the channel. Its real part is
    /// the audio.
    #[must_use]
    pub fn apply(&self, signal: &[Complex32], rng: &mut Rng) -> Vec<Complex32> {
        let n = signal.len();
        let rate = f64::from(SAMPLE_RATE);
        let echo = self.echo_ms > 0.0;
        let echo_power = if echo {
            10_f64.powf(self.echo_db / 10.0)
        } else {
            0.0
        };
        let norm = 1.0 / (1.0 + echo_power).sqrt();

        // Each path's gain over time: fading, or a steady random phase.
        let first = self.path_gain(n, rng, self.rician_k_db);
        let second = if echo {
            // The steady component, if any, is on the first path only.
            Some(self.path_gain(n, rng, None))
        } else {
            None
        };
        let deep = self.deep_fade(n, rng);
        let phase = self.phase(n, rng);
        let delay = round_to_usize(self.echo_ms * rate / 1_000.0);
        let echo_step = 2.0 * PI * self.echo_shift_hz / rate;
        let echo_amplitude = echo_power.sqrt();

        (0..n)
            .map(|i| {
                let mut y = to_c64(signal[i]) * first[i];
                if let Some(g) = &second
                    && i >= delay
                {
                    let shift = Complex64::from_polar(1.0, echo_step * index_f64(i));
                    y += to_c64(signal[i - delay]) * g[i] * shift * echo_amplitude;
                }
                let turned = y * Complex64::from_polar(norm * deep[i], phase[i]);
                Complex32::new(to_f32(turned.re), to_f32(turned.im))
            })
            .collect()
    }

    /// One path's complex gain at every audio sample: a Gaussian-spectrum
    /// fading process of unit average power (with a steady part when
    /// `k_db` is given), or with no fading a steady gain of random phase.
    fn path_gain(&self, n: usize, rng: &mut Rng, k_db: Option<f64>) -> Vec<Complex64> {
        if self.spread_hz <= 0.0 {
            let g = Complex64::from_polar(1.0, rng.range(0.0, 2.0 * PI));
            return vec![g; n];
        }
        let fading = fading_process(self.spread_hz / 2.0, rng);
        let (steady, scatter) = match k_db {
            Some(k) => {
                let k = 10_f64.powf(k / 10.0);
                (
                    Complex64::from_polar((k / (k + 1.0)).sqrt(), rng.range(0.0, 2.0 * PI)),
                    (1.0 / (k + 1.0)).sqrt(),
                )
            }
            None => (Complex64::new(0.0, 0.0), 1.0),
        };
        let rate = f64::from(SAMPLE_RATE);
        (0..n)
            .map(|i| {
                // Linear interpolation from the fading rate to the audio
                // rate; the process is far slower than either.
                let at = index_f64(i) * FADING_RATE / rate;
                let below = floor_to_usize(at);
                let frac = at - index_f64(below);
                let a = fading[below.min(FADING_LEN - 1)];
                let b = fading[(below + 1).min(FADING_LEN - 1)];
                steady + (a + (b - a) * frac) * scatter
            })
            .collect()
    }

    /// The amplitude of one deep fade at a random place in the
    /// transmission: 1 outside it, down to the fade's depth inside, with
    /// raised-cosine edges.
    fn deep_fade(&self, n: usize, rng: &mut Rng) -> Vec<f64> {
        if self.deep_fade_db <= 0.0 || self.deep_fade_s <= 0.0 {
            return vec![1.0; n];
        }
        let rate = f64::from(SAMPLE_RATE);
        let length = self.deep_fade_s * rate;
        let total = index_f64(n);
        let start = rng.range(0.0, (total - length).max(0.0));
        let edge = (DEEP_FADE_EDGE_S * rate).min(length / 2.0);
        let floor = 10_f64.powf(-self.deep_fade_db / 20.0);
        (0..n)
            .map(|i| {
                let t = index_f64(i) - start;
                let into = t.min(length - t);
                if into <= 0.0 {
                    1.0
                } else if into >= edge {
                    floor
                } else {
                    let w = (1.0 - (PI * into / edge).cos()) / 2.0;
                    1.0 - (1.0 - floor) * w
                }
            })
            .collect()
    }

    /// The phase the channel adds at every sample, in radians: drift, curve
    /// and wobble integrated from frequency, plus phase noise.
    fn phase(&self, n: usize, rng: &mut Rng) -> Vec<f64> {
        let rate = f64::from(SAMPLE_RATE);
        let total_s = index_f64(n) / rate;
        let drift = if rng.below(2) == 0 {
            self.drift_hz_per_s
        } else {
            -self.drift_hz_per_s
        };
        let bend = if rng.below(2) == 0 {
            self.bend_hz
        } else {
            -self.bend_hz
        };
        let wobble_phase = rng.range(0.0, 2.0 * PI);
        let noise_step = (2.0 * PI * self.phase_noise_hz / rate).sqrt();
        let mut phase = 0.0_f64;
        let mut walk = 0.0_f64;
        (0..n)
            .map(|i| {
                let t = index_f64(i) / rate;
                let centred = t - total_s / 2.0;
                let u = 2.0 * centred / total_s; // -1 to 1
                let mut hz = drift * centred + bend * u * u;
                if self.wobble_hz > 0.0 && self.wobble_period_s > 0.0 {
                    hz +=
                        self.wobble_hz * (2.0 * PI * t / self.wobble_period_s + wobble_phase).sin();
                }
                let now = phase + walk;
                phase += 2.0 * PI * hz / rate;
                if self.phase_noise_hz > 0.0 {
                    walk += noise_step * rng.gaussian();
                }
                now
            })
            .collect()
    }
}

impl fmt::Display for Channel {
    /// The channel as the settings [`Channel::parse`] reads, leaving out
    /// those that are zero; `clear` for the clean channel.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clear() {
            return f.write_str("clear");
        }
        let mut parts = Vec::new();
        let mut add = |name: &str, v: f64| {
            if v != 0.0 {
                parts.push(format!("{name}={v}"));
            }
        };
        add("spread", self.spread_hz);
        add("deepfade", self.deep_fade_db);
        add("fadelen", self.deep_fade_s);
        add("drift", self.drift_hz_per_s);
        add("bend", self.bend_hz);
        add("wobble", self.wobble_hz);
        add("period", self.wobble_period_s);
        add("phasenoise", self.phase_noise_hz);
        add("echo", self.echo_ms);
        add("echodb", self.echo_db);
        add("echoshift", self.echo_shift_hz);
        if let Some(k) = self.rician_k_db {
            parts.push(format!("k={k}"));
        }
        f.write_str(&parts.join(","))
    }
}

/// A complex Gaussian process of unit average power whose spectrum is a
/// Gaussian of standard deviation `sigma_hz`, sampled at [`FADING_RATE`]:
/// white noise shaped in the frequency domain.
fn fading_process(sigma_hz: f64, rng: &mut Rng) -> Vec<Complex64> {
    let bin_hz = FADING_RATE / index_f64(FADING_LEN);
    let shape: Vec<f64> = (0..FADING_LEN)
        .map(|k| {
            let k = if k > FADING_LEN / 2 {
                index_f64(k) - index_f64(FADING_LEN)
            } else {
                index_f64(k)
            };
            let hz = k * bin_hz;
            (-hz * hz / (2.0 * sigma_hz * sigma_hz)).exp().sqrt()
        })
        .collect();
    let energy: f64 = shape.iter().map(|s| s * s).sum();
    // Unit power after the inverse transform (which does not divide by n):
    // each bin's noise has variance 1, so the output's is energy, and
    // dividing by its root leaves 1.
    let scale = 1.0 / energy.sqrt();
    let mut spectrum: Vec<Complex64> = shape
        .iter()
        .map(|s| {
            let re = rng.gaussian() * std::f64::consts::FRAC_1_SQRT_2;
            let im = rng.gaussian() * std::f64::consts::FRAC_1_SQRT_2;
            Complex64::new(re, im) * (s * scale)
        })
        .collect();
    FftPlanner::new()
        .plan_fft_inverse(FADING_LEN)
        .process(&mut spectrum);
    spectrum
}

fn number(name: &str, value: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("channel setting {name} must be a number, not {value}"))
}

fn to_c64(c: Complex32) -> Complex64 {
    Complex64::new(f64::from(c.re), f64::from(c.im))
}

#[allow(clippy::cast_possible_truncation)] // audio samples
fn to_f32(x: f64) -> f32 {
    x as f32
}

/// A sample index as a float; indexes stay far below 2^52.
#[allow(clippy::cast_precision_loss)]
fn index_f64(i: usize) -> f64 {
    i as f64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small, non-negative
fn round_to_usize(x: f64) -> usize {
    x.round().max(0.0) as usize
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small, non-negative
fn floor_to_usize(x: f64) -> usize {
    x.floor().max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use rustfft::num_complex::Complex32;

    use super::{Channel, fading_process};
    use crate::ft8::bench::Rng;

    fn tone(n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / 12_000.0;
                Complex32::from_polar(1.0, 2.0 * std::f32::consts::PI * 1_000.0 * t)
            })
            .collect()
    }

    #[allow(clippy::cast_precision_loss)]
    fn mean_power(x: &[Complex32]) -> f64 {
        x.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / x.len() as f64
    }

    #[test]
    fn settings_round_trip_and_presets_read() {
        let text = "spread=0.5,deepfade=20,fadelen=3,drift=0.1,echo=2,echodb=-3,k=6";
        let channel = Channel::parse(text).unwrap_or(Channel::CLEAR);
        assert_eq!(channel.spread_hz, 0.5);
        assert_eq!(channel.rician_k_db, Some(6.0));
        assert_eq!(Channel::parse(&channel.to_string()), Ok(channel));
        assert_eq!(Channel::parse("clear"), Ok(Channel::CLEAR));
        assert_eq!(Channel::CLEAR.to_string(), "clear");
        let moderate = Channel::parse("moderate,drift=0.2").unwrap_or(Channel::CLEAR);
        assert_eq!(
            (
                moderate.spread_hz,
                moderate.echo_ms,
                moderate.drift_hz_per_s
            ),
            (0.5, 1.0, 0.2)
        );
        assert!(Channel::parse("spread=fast").is_err());
        assert!(Channel::parse("sideways=1").is_err());
        assert!(Channel::parse("drift=1,moderate").is_err());
    }

    #[test]
    fn the_clear_channel_changes_only_the_phase() {
        let sent = tone(12_000);
        let got = Channel::CLEAR.apply(&sent, &mut Rng::new(1));
        let turn = got[0] / sent[0];
        for (a, b) in got.iter().zip(&sent) {
            assert!((a - b * turn).norm() < 1e-4);
        }
    }

    #[test]
    fn fading_keeps_the_average_power_and_has_its_spread() {
        // Over many independent processes the average power is 1.
        let mut rng = Rng::new(2);
        let mut power = 0.0;
        for _ in 0..40 {
            let f = fading_process(0.25, &mut rng);
            #[allow(clippy::cast_precision_loss)]
            let p = f.iter().map(|c| c.norm_sqr()).sum::<f64>() / f.len() as f64;
            power += p / 40.0;
        }
        assert!((power - 1.0).abs() < 0.1, "power {power}");

        // A 1 Hz spread decorrelates within about a second; a 0.05 Hz one
        // barely changes in a second.
        let correlation = |sigma: f64, lag: usize| {
            let mut rng = Rng::new(3);
            let mut sum = 0.0;
            let mut norm = 0.0;
            for _ in 0..20 {
                let f = fading_process(sigma, &mut rng);
                for i in 0..f.len() - lag {
                    sum += (f[i] * f[i + lag].conj()).re;
                    norm += f[i].norm_sqr();
                }
            }
            sum / norm
        };
        assert!(correlation(0.5, 100) < 0.2);
        assert!(correlation(0.025, 100) > 0.9);
    }

    #[test]
    fn a_rayleigh_channel_passes_the_average_power() {
        let channel = Channel::parse("disturbed").unwrap_or(Channel::CLEAR);
        let sent = tone(151_680);
        let mut rng = Rng::new(4);
        let mut power = 0.0;
        for _ in 0..30 {
            power += mean_power(&channel.apply(&sent, &mut rng)) / 30.0;
        }
        assert!((power - 1.0).abs() < 0.15, "power {power}");
    }

    #[test]
    fn a_deep_fade_dips_for_its_length() {
        let channel = Channel::parse("deepfade=20,fadelen=2").unwrap_or(Channel::CLEAR);
        let got = channel.apply(&tone(151_680), &mut Rng::new(5));
        let quiet = got.iter().filter(|c| c.norm() < 0.11).count();
        // 2 s long with 0.3 s edges: about 1.4 s at the floor.
        assert!((16_000..18_500).contains(&quiet), "{quiet} samples faded");
    }

    #[test]
    fn drift_moves_the_frequency_by_its_rate() {
        // 0.5 Hz/s over 12.64 s moves the tone about 6.3 Hz: compare the
        // phase advance per second near the start and the end.
        let channel = Channel::parse("drift=0.5").unwrap_or(Channel::CLEAR);
        let sent = tone(151_680);
        let got = channel.apply(&sent, &mut Rng::new(6));
        let hz_at = |i: usize| {
            let a = (got[i] / sent[i]).arg();
            let b = (got[i + 120] / sent[i + 120]).arg();
            let mut d = f64::from(b - a);
            if d > std::f64::consts::PI {
                d -= 2.0 * std::f64::consts::PI;
            }
            if d < -std::f64::consts::PI {
                d += 2.0 * std::f64::consts::PI;
            }
            d / (2.0 * std::f64::consts::PI) * 100.0
        };
        let change = (hz_at(150_000) - hz_at(1_000)).abs();
        assert!((change - 6.2).abs() < 0.3, "moved {change} Hz");
    }
}
