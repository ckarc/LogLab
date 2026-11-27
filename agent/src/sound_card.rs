// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Agent's own sound card, research step 1 (brian, 2026-11-27: "lets
//! write our own virtual sound card"; a "software-only, radio-independent,
//! bidirectional Windows audio endpoint managed by the LabLog Agent").
//!
//! The Agent pretends to be a USB sound card, "LabLog Radio", and serves it
//! to this same computer over USB/IP on 127.0.0.1. usbip-win2's signed
//! driver plugs it in, and Windows runs it with its own built-in USB Audio
//! 2.0 driver, so programs see an ordinary sound card with one output and
//! one input. The USB side comes from `usbip-audio` (MIT, Rust).
//!
//! Step 1 proves the card and nothing else: what Windows plays to it comes
//! straight back in its input ([`Loopback`]), and `SoundCardTest` plays a
//! tone in, records it back, and checks it ([`analyse`]). No radio is
//! involved. Later steps put a radio's audio where the loopback is.
//!
//! What it decides lives here, apart from Windows, so it is tested on every
//! platform; `sound_card_windows` plugs it in and plays through it.

use std::collections::VecDeque;
use std::process::ExitCode;
use std::sync::Mutex;

use usbip_audio::uac2::{Uac2Config, Uac2Streams};

/// The card's name, as Windows shows it.
pub const NAME: &str = "LabLog Radio";
/// Its sample rate.
pub const RATE: u32 = 48_000;
/// Its channels, each way.
pub const CHANNELS: u16 = 2;
/// The tone `SoundCardTest` plays in.
pub const TONE_HZ: f64 = 1000.0;
/// And how loud, as a fraction of full scale (-12 dBFS).
pub const TONE_AMPLITUDE: f64 = 0.25;
/// The most audio the loopback holds before it drops the oldest: half a
/// second.
const LOOPBACK_MOST_FRAMES: usize = RATE as usize / 2;

/// What the card looks like to Windows. The ids are pid.codes' test ids
/// (vendor 0x1209, product 0x0001): fine for research, not for release.
#[must_use]
pub fn config() -> Uac2Config {
    Uac2Config {
        vendor_id: 0x1209,
        product_id: 0x0001,
        bcd_device: 0x0100,
        manufacturer: "LabLog".to_owned(),
        product: NAME.to_owned(),
        serial: "lablog-radio-1".to_owned(),
        sample_rate: RATE,
        playback_channels: CHANNELS,
        capture_channels: CHANNELS,
        playback_name: None,
        capture_name: None,
        capture_connector: false,
    }
}

/// What the loopback has carried so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Frames Windows played to the card.
    pub played: u64,
    /// Frames Windows recorded from the card.
    pub recorded: u64,
    /// Recorded frames for which nothing had been played yet (silence).
    pub short: u64,
    /// Played frames dropped because nothing recorded them in time.
    pub dropped: u64,
}

#[derive(Default)]
struct Held {
    samples: VecDeque<i32>,
    counts: Counts,
}

/// Step 1's streams: what Windows plays to the card comes back in its
/// input, after however much Windows queued.
#[derive(Default)]
pub struct Loopback {
    held: Mutex<Held>,
}

impl Loopback {
    /// What it has carried so far.
    #[must_use]
    pub fn counts(&self) -> Counts {
        self.held.lock().map(|held| held.counts).unwrap_or_default()
    }
}

impl Uac2Streams for Loopback {
    fn playback(&self, _frame: u64, samples: &[i32]) {
        let Ok(mut held) = self.held.lock() else {
            return;
        };
        let channels = usize::from(CHANNELS);
        held.samples.extend(samples);
        held.counts.played += (samples.len() / channels) as u64;
        let most = LOOPBACK_MOST_FRAMES * channels;
        if held.samples.len() > most {
            let over = held.samples.len() - most;
            held.samples.drain(..over);
            held.counts.dropped += (over / channels) as u64;
        }
    }

    fn capture(&self, _frame: u64, samples: &mut [i32]) {
        let Ok(mut held) = self.held.lock() else {
            samples.fill(0);
            return;
        };
        let channels = usize::from(CHANNELS);
        let mut short = 0;
        for sample in samples.iter_mut() {
            if let Some(next) = held.samples.pop_front() {
                *sample = next;
            } else {
                *sample = 0;
                short += 1;
            }
        }
        held.counts.recorded += (samples.len() / channels) as u64;
        held.counts.short += (short / channels) as u64;
    }
}

/// The test tone's sample at `frame`.
#[must_use]
pub fn tone(frame: u64) -> f32 {
    // Frames within a test run fit an f64's 53 bits exactly.
    #[allow(clippy::cast_precision_loss)]
    let seconds = frame as f64 / f64::from(RATE);
    #[allow(clippy::cast_possible_truncation)]
    let sample = (TONE_AMPLITUDE * (seconds * TONE_HZ * std::f64::consts::TAU).sin()) as f32;
    sample
}

/// What a stretch of recorded audio holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Heard {
    /// Its frequency, from how often it crosses zero.
    pub hz: f64,
    /// Its loudest sample, in dB below full scale.
    pub peak_dbfs: f64,
    /// How many samples it had.
    pub samples: usize,
}

/// Measures `samples` (one channel, at [`RATE`]).
#[must_use]
pub fn analyse(samples: &[f32]) -> Heard {
    let peak = samples.iter().fold(0.0_f32, |most, s| most.max(s.abs()));
    let crossings = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let seconds = samples.len() as f64 / f64::from(RATE);
    #[allow(clippy::cast_precision_loss)]
    let hz = if seconds > 0.0 {
        crossings as f64 / 2.0 / seconds
    } else {
        0.0
    };
    Heard {
        hz,
        peak_dbfs: 20.0 * f64::from(peak).max(1e-9).log10(),
        samples: samples.len(),
    }
}

/// How far the tone may come back off, in Hz and in dB.
const HZ_OFF_MOST: f64 = 5.0;
const DB_OFF_MOST: f64 = 1.0;

/// Whether `heard` is the test tone come back whole: its frequency and its
/// level as played. `Err` says what was wrong.
///
/// # Errors
///
/// The tone came back at the wrong frequency or level, or not at all.
pub fn judge(heard: &Heard) -> Result<(), String> {
    let wanted_db = 20.0 * TONE_AMPLITUDE.log10();
    if heard.samples < RATE as usize {
        return Err(format!(
            "less than a second was recorded ({} samples)",
            heard.samples
        ));
    }
    if (heard.hz - TONE_HZ).abs() > HZ_OFF_MOST {
        return Err(format!(
            "the tone came back at {:.1} Hz, not {TONE_HZ} Hz",
            heard.hz
        ));
    }
    if (heard.peak_dbfs - wanted_db).abs() > DB_OFF_MOST {
        return Err(format!(
            "the tone came back at {:.1} dBFS, not {wanted_db:.1} dBFS",
            heard.peak_dbfs
        ));
    }
    Ok(())
}

/// Runs `SoundCardTest`: plugs the card in, plays the tone through it for
/// `seconds`, and says what came back.
#[must_use]
pub fn start(args: &[String]) -> ExitCode {
    let seconds = match args {
        [] => 20,
        [flag, value] if flag == "--seconds" => match value.parse::<u64>() {
            Ok(seconds) if seconds >= 3 => seconds,
            _ => {
                println!("SoundCardTest: --seconds needs a whole number, 3 or more");
                return ExitCode::FAILURE;
            }
        },
        _ => {
            println!("usage: SoundCardTest [--seconds N]");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(windows)]
    let outcome = crate::sound_card_windows::run(seconds);
    #[cfg(not(windows))]
    let outcome: Result<(), String> = {
        let _ = seconds;
        Err("the sound card is for Windows; it has nothing to do here".to_owned())
    };
    match outcome {
        Ok(()) => {
            println!("SoundCardTest: PASS");
            ExitCode::SUCCESS
        }
        Err(why) => {
            println!("SoundCardTest: FAIL: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Counts, Loopback, RATE, TONE_HZ, analyse, config, judge, tone};
    use usbip_audio::uac2::{Uac2Device, Uac2Streams};

    #[test]
    fn the_card_is_a_valid_usb_audio_device() {
        assert!(config().validate().is_ok());
        assert!(Uac2Device::new(config(), Loopback::default()).is_ok());
    }

    #[test]
    fn what_is_played_comes_back_in_order() {
        let loopback = Loopback::default();
        loopback.playback(0, &[1, 2, 3, 4, 5, 6]);
        let mut recorded = [0; 4];
        loopback.capture(0, &mut recorded);
        assert_eq!(recorded, [1, 2, 3, 4]);
        let mut more = [9; 4];
        loopback.capture(2, &mut more);
        assert_eq!(more, [5, 6, 0, 0]);
        assert_eq!(
            loopback.counts(),
            Counts {
                played: 3,
                recorded: 4,
                short: 1,
                dropped: 0
            }
        );
    }

    #[test]
    fn the_loopback_never_holds_more_than_half_a_second() {
        let loopback = Loopback::default();
        let second = vec![7; RATE as usize * 2];
        loopback.playback(0, &second);
        assert_eq!(loopback.counts().dropped, u64::from(RATE / 2));
    }

    #[test]
    fn the_tone_is_heard_as_played() {
        let samples: Vec<f32> = (0..u64::from(RATE) * 2).map(tone).collect();
        let heard = analyse(&samples);
        assert!((heard.hz - TONE_HZ).abs() < 1.0, "{heard:?}");
        assert!((heard.peak_dbfs + 12.04).abs() < 0.1, "{heard:?}");
        assert_eq!(judge(&heard), Ok(()));
    }

    #[test]
    fn a_wrong_or_missing_tone_fails() {
        let quiet: Vec<f32> = (0..u64::from(RATE) * 2).map(|f| tone(f) / 4.0).collect();
        assert!(judge(&analyse(&quiet)).is_err_and(|why| why.contains("dBFS")));
        let silent = vec![0.0; RATE as usize * 2];
        assert!(judge(&analyse(&silent)).is_err());
        let short: Vec<f32> = (0..100).map(tone).collect();
        assert!(judge(&analyse(&short)).is_err_and(|why| why.contains("second")));
    }
}
