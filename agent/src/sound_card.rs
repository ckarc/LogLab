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
#[cfg(any(windows, test))]
use std::sync::Arc;
use std::sync::Mutex;

use usbip_audio::uac2::{Uac2Config, Uac2Streams};

#[cfg(any(windows, test))]
use crate::audio::{Buffer, Resampler};
use usbip_audio::{IsoEndpoint, IsoTiming, Notifier, Setup, Speed, UsbDevice};

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
/// The longest run `SoundCardTest` takes: 6 hours.
pub const MOST_SECONDS: u64 = 6 * 60 * 60;
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

    /// Frames played and not yet recorded: Windows' queue plus any drift
    /// between its playing and recording clocks.
    #[must_use]
    pub fn held_frames(&self) -> usize {
        self.held
            .lock()
            .map(|held| held.samples.len() / usize::from(CHANNELS))
            .unwrap_or_default()
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

/// Milestone 1's streams, with no radio yet: what Windows plays to the
/// card goes nowhere, and its input is silence.
#[derive(Default)]
pub struct Silence;

impl Uac2Streams for Silence {
    fn playback(&self, _frame: u64, _samples: &[i32]) {}

    fn capture(&self, _frame: u64, samples: &mut [i32]) {
        samples.fill(0);
    }
}

/// Milestone 2's streams: the card's input carries whatever audio
/// [`Feed::connect`] gives it (the Flex's receive audio), resampled to the
/// card's rate, on both channels; silence when nothing is connected. What
/// Windows plays to the card still goes nowhere (transmit is milestone 3).
#[cfg(any(windows, test))]
#[derive(Default)]
pub struct Feed {
    source: Mutex<Option<Resampler>>,
}

#[cfg(any(windows, test))]
impl Feed {
    /// From now on the input carries what arrives in `buffer`.
    pub fn connect(&self, buffer: Arc<Buffer>) {
        if let Ok(mut source) = self.source.lock() {
            *source = Some(Resampler::new(buffer, RATE));
        }
    }

    /// From now on the input is silence again.
    pub fn disconnect(&self) {
        if let Ok(mut source) = self.source.lock() {
            *source = None;
        }
    }
}

/// A sample from -1.0 to 1.0 as the card carries it: 24 bits at the top
/// of an `i32`.
#[cfg(any(windows, test))]
#[must_use]
pub fn to_card(sample: f32) -> i32 {
    // Clamped first, so the cast loses nothing that matters.
    #[allow(clippy::cast_possible_truncation)]
    let card = (f64::from(sample.clamp(-1.0, 1.0)) * f64::from(i32::MAX)) as i32;
    card
}

#[cfg(any(windows, test))]
impl Uac2Streams for Feed {
    fn playback(&self, _frame: u64, _samples: &[i32]) {}

    fn capture(&self, _frame: u64, samples: &mut [i32]) {
        let Ok(mut source) = self.source.lock() else {
            samples.fill(0);
            return;
        };
        let Some(resampler) = source.as_mut() else {
            samples.fill(0);
            return;
        };
        let channels = usize::from(CHANNELS);
        let mut mono = vec![0.0_f32; samples.len() / channels];
        resampler.fill(&mut mono);
        for (frame, sample) in samples.chunks_mut(channels).zip(mono) {
            frame.fill(to_card(sample));
        }
    }
}

/// USB Audio terminal types (USB Device Class Definition for Terminal
/// Types 2.0): a microphone, and an external line connector.
const MICROPHONE: u16 = 0x0201;
const LINE_CONNECTOR: u16 = 0x0603;
/// Descriptor types, and the class-specific audio control input terminal.
const INTERFACE: u8 = 4;
const CLASS_INTERFACE: u8 = 0x24;
const INPUT_TERMINAL: u8 = 0x02;
const AUDIO: u8 = 1;
const AUDIO_CONTROL: u8 = 1;

/// The input terminals' types in a configuration descriptor's audio
/// control interface, at the byte where each is written.
fn input_terminals(configuration: &[u8]) -> Vec<(usize, u16)> {
    let mut found = vec![];
    let mut at = 0;
    let mut audio_control = false;
    while let Some(&length) = configuration.get(at) {
        let length = usize::from(length);
        let Some(descriptor) = configuration.get(at..at + length) else {
            break;
        };
        if length < 2 {
            break;
        }
        match descriptor[1] {
            INTERFACE if length >= 7 => {
                audio_control = descriptor[5] == AUDIO && descriptor[6] == AUDIO_CONTROL;
            }
            // In an AudioStreaming interface the same subtype is a format.
            CLASS_INTERFACE if audio_control && length >= 6 && descriptor[2] == INPUT_TERMINAL => {
                found.push((at + 4, u16::from_le_bytes([descriptor[4], descriptor[5]])));
            }
            _ => {}
        }
        at += length;
    }
    found
}

/// `configuration` with its microphone made a line input.
#[must_use]
pub fn as_line_in(configuration: &[u8]) -> Vec<u8> {
    let mut out = configuration.to_vec();
    for (at, kind) in input_terminals(configuration) {
        if kind == MICROPHONE {
            out[at..at + 2].copy_from_slice(&LINE_CONNECTOR.to_le_bytes());
        }
    }
    out
}

/// A USB sound card whose input says it is a line input, not a
/// microphone. Windows 11 puts its Voice Clarity noise suppression on a
/// USB microphone, and on FL-1 (2026-11-28) that erased a steady test
/// tone; it leaves a line input alone, and calls it "Line (...)".
/// `usbip-audio` always calls its input a microphone, so this rewrites
/// that one field of what the card tells Windows.
pub struct LineIn<D>(pub D);

/// A standard request for the configuration descriptor.
fn asks_for_configuration(setup: Setup) -> bool {
    setup.request_type == 0x80 && setup.request == 6 && setup.value >> 8 == 2
}

impl<D: UsbDevice> UsbDevice for LineIn<D> {
    fn speed(&self) -> Speed {
        self.0.speed()
    }

    fn device_descriptor(&self) -> Vec<u8> {
        self.0.device_descriptor()
    }

    fn configuration_descriptor(&self) -> Vec<u8> {
        as_line_in(&self.0.configuration_descriptor())
    }

    fn control(&self, setup: Setup, data: &[u8]) -> Result<Vec<u8>, i32> {
        if asks_for_configuration(setup) {
            let mut descriptor = self.configuration_descriptor();
            descriptor.truncate(usize::from(setup.length));
            return Ok(descriptor);
        }
        self.0.control(setup, data)
    }

    fn iso_endpoint(&self, address: u8) -> Option<IsoEndpoint> {
        self.0.iso_endpoint(address)
    }

    fn iso_out(&self, address: u8, timing: IsoTiming, packets: &[&[u8]]) {
        self.0.iso_out(address, timing, packets);
    }

    fn iso_in(
        &self,
        address: u8,
        timing: IsoTiming,
        capacity: &[usize],
        data: &mut Vec<u8>,
    ) -> Vec<usize> {
        self.0.iso_in(address, timing, capacity, data)
    }

    fn interrupt_endpoint(&self, address: u8) -> Option<Notifier> {
        self.0.interrupt_endpoint(address)
    }

    fn interrupt_in(&self, address: u8, capacity: usize) -> Option<Vec<u8>> {
        self.0.interrupt_in(address, capacity)
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
    pub samples: u64,
    /// Dropouts: stretches of a millisecond or more of near-silence, which
    /// the tone never has.
    pub gaps: u64,
}

/// A sample this quiet is near-silence (-40 dBFS).
const QUIET: f32 = 0.01;
/// This many near-silent samples in a row are a dropout: 1 ms.
const GAP_SAMPLES: u32 = RATE / 1000;

/// Measures recorded audio as it arrives, holding none of it, so a run of
/// hours costs no memory.
#[derive(Debug, Default, Clone)]
pub struct Listener {
    samples: u64,
    crossings: u64,
    peak: f32,
    negative: Option<bool>,
    quiet_run: u32,
    gaps: u64,
}

impl Listener {
    /// Takes in one sample (one channel, at [`RATE`]).
    pub fn hear(&mut self, sample: f32) {
        self.samples += 1;
        self.peak = self.peak.max(sample.abs());
        let negative = sample < 0.0;
        if self.negative.is_some_and(|was| was != negative) {
            self.crossings += 1;
        }
        self.negative = Some(negative);
        if sample.abs() < QUIET {
            self.quiet_run += 1;
            if self.quiet_run == GAP_SAMPLES {
                self.gaps += 1;
            }
        } else {
            self.quiet_run = 0;
        }
    }

    /// What it has heard so far.
    #[must_use]
    pub fn heard(&self) -> Heard {
        // Sample counts within a test run fit an f64's 53 bits exactly.
        #[allow(clippy::cast_precision_loss)]
        let seconds = self.samples as f64 / f64::from(RATE);
        #[allow(clippy::cast_precision_loss)]
        let hz = if seconds > 0.0 {
            self.crossings as f64 / 2.0 / seconds
        } else {
            0.0
        };
        Heard {
            hz,
            peak_dbfs: 20.0 * f64::from(self.peak).max(1e-9).log10(),
            samples: self.samples,
            gaps: self.gaps,
        }
    }
}

/// Measures `samples` (one channel, at [`RATE`]).
#[must_use]
pub fn analyse(samples: &[f32]) -> Heard {
    let mut listener = Listener::default();
    for &sample in samples {
        listener.hear(sample);
    }
    listener.heard()
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
    if heard.samples < u64::from(RATE) {
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
    if heard.gaps > 0 {
        return Err(format!(
            "the tone dropped out {} time{}",
            heard.gaps,
            if heard.gaps == 1 { "" } else { "s" }
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
            Ok(seconds) if (3..=MOST_SECONDS).contains(&seconds) => seconds,
            _ => {
                println!("SoundCardTest: --seconds needs a whole number, 3 to {MOST_SECONDS}");
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
    use super::{
        Counts, LINE_CONNECTOR, LineIn, Loopback, MICROPHONE, RATE, Silence, TONE_HZ, analyse,
        config, input_terminals, judge, tone,
    };
    use usbip_audio::uac2::{Uac2Device, Uac2Streams};
    use usbip_audio::{Setup, UsbDevice};

    #[test]
    fn the_card_says_its_input_is_a_line_not_a_microphone() -> Result<(), String> {
        let plain = Uac2Device::new(config(), Silence)?;
        let as_made = plain.configuration_descriptor();
        let kinds: Vec<u16> = input_terminals(&as_made).iter().map(|t| t.1).collect();
        // usbip-audio's own: USB streaming (playback) and a microphone.
        assert_eq!(kinds, [0x0101, MICROPHONE]);

        let card = LineIn(plain);
        let told = card.configuration_descriptor();
        let kinds: Vec<u16> = input_terminals(&told).iter().map(|t| t.1).collect();
        assert_eq!(kinds, [0x0101, LINE_CONNECTOR]);
        // Only those two bytes differ.
        assert_eq!(told.len(), as_made.len());
        let changed = told.iter().zip(&as_made).filter(|(a, b)| a != b).count();
        assert!((1..=2).contains(&changed), "{changed} bytes changed");

        // And it's what Windows gets when it asks, cut to what it asked for.
        let ask = |length: u16| Setup {
            request_type: 0x80,
            request: 6,
            value: 0x0200,
            index: 0,
            length,
        };
        assert_eq!(card.control(ask(1024), &[]), Ok(told.clone()));
        assert_eq!(card.control(ask(9), &[]), Ok(told[..9].to_vec()));
        // Other requests pass through: the device descriptor is unchanged.
        let device = Setup {
            value: 0x0100,
            ..ask(18)
        };
        assert_eq!(card.control(device, &[]), card.0.control(device, &[]));
        Ok(())
    }

    #[test]
    fn the_feed_carries_what_it_is_connected_to_on_both_channels() {
        let feed = super::Feed::default();
        let mut recorded = [5; 8];
        feed.capture(0, &mut recorded);
        assert_eq!(recorded, [0; 8], "silent until connected");

        // A steady half-scale signal at 24 kHz, more than the resampler's
        // cushion, comes out at 48 kHz on both channels.
        let buffer = crate::audio::Buffer::new(24_000);
        buffer.push(std::iter::repeat_n(0.5, 24_000));
        feed.connect(std::sync::Arc::clone(&buffer));
        let mut recorded = vec![0; 2 * 960];
        feed.capture(0, &mut recorded);
        let half = super::to_card(0.5);
        assert!(recorded[400..].iter().all(|&s| (s - half).abs() < 1 << 16));
        assert!(recorded.chunks(2).all(|frame| frame[0] == frame[1]));

        feed.disconnect();
        feed.capture(0, &mut recorded);
        assert!(recorded.iter().all(|&s| s == 0));
    }

    #[test]
    fn samples_fill_the_cards_24_bits() {
        assert_eq!(super::to_card(0.0), 0);
        assert_eq!(super::to_card(1.0), i32::MAX);
        assert_eq!(super::to_card(-2.0), -i32::MAX);
        assert_eq!(super::to_card(f32::NAN), 0);
    }

    #[test]
    fn silence_is_silent() {
        let mut recorded = [5; 8];
        Silence.capture(0, &mut recorded);
        assert_eq!(recorded, [0; 8]);
    }

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

    #[test]
    fn a_dropout_in_the_tone_is_counted() {
        let mut samples: Vec<f32> = (0..u64::from(RATE) * 2).map(tone).collect();
        // 5 ms of silence in the middle.
        let middle = samples.len() / 2;
        samples[middle..middle + 240].fill(0.0);
        let heard = analyse(&samples);
        assert_eq!(heard.gaps, 1);
        assert!(judge(&heard).is_err_and(|why| why.contains("dropped out 1 time")));
        let whole: Vec<f32> = (0..u64::from(RATE) * 2).map(tone).collect();
        assert_eq!(analyse(&whole).gaps, 0);
    }
}
