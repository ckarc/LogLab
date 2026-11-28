// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The LabLog Radio sound card: a virtual USB sound card the Agent presents
//! to Windows, so programs such as WSJT-X record a radio's receive audio
//! from it and play their transmit audio into it, with no virtual audio
//! cable and no audio driver of LabLog's own.
//!
//! The card is a USB Audio Class 2.0 device served over USB/IP on this
//! computer (the vendored `usbip-audio` crate, MIT). The usbip-win2 client
//! driver attaches it, and Windows' own USB audio driver runs it. On FL-1
//! (2026-11-27) Windows then showed it as ordinary playback and recording
//! devices, and played and recorded through them in shared mode from the
//! signed-in user's session.
//!
//! Its input is declared a line connector, not a microphone: on Windows 11
//! 25H2 a microphone gets the Voice Clarity effect, which removed a steady
//! 1 kHz tone (measured on FL-1, 2026-11-27), and a radio's digital-mode
//! audio is made of steady tones.
//!
//! This first step carries no radio audio yet: what programs play is
//! counted and dropped, and what they record is silence.

use std::sync::atomic::{AtomicU64, Ordering};

use usbip_audio::uac2::{Uac2Config, Uac2Streams};

/// The card's name, as Windows shows it: "Line (LabLog Radio)" and
/// "Speakers (LabLog Radio)".
pub const NAME: &str = "LabLog Radio";

/// Its one sample rate, in hertz.
pub const SAMPLE_RATE: u32 = 48_000;

/// Its channels each way.
pub const CHANNELS: u16 = 2;

/// Its bus id on the USB/IP server.
pub const BUS_ID: &str = "1-1";

/// USB Audio Terminal Types 1.0: a line connector.
const LINE_CONNECTOR: u16 = 0x0603;

/// The card as Windows will see it.
///
/// The vendor and product ids are pid.codes' test ids (vendor 0x1209,
/// product 0x0001), which pid.codes reserves for testing only: a released
/// Agent needs ids of its own.
#[must_use]
pub fn config() -> Uac2Config {
    Uac2Config {
        vendor_id: 0x1209,
        product_id: 0x0001,
        bcd_device: 0x0100,
        manufacturer: "LabLog".to_owned(),
        product: NAME.to_owned(),
        // Windows keeps each card's settings by its serial number.
        serial: "lablog-radio-1".to_owned(),
        sample_rate: SAMPLE_RATE,
        playback_channels: CHANNELS,
        capture_channels: CHANNELS,
        playback_name: None,
        capture_name: None,
        capture_connector: false,
        capture_terminal_type: LINE_CONNECTOR,
    }
}

/// The card's audio for now: what programs play is counted and dropped,
/// and what they record is silence.
#[derive(Debug, Default)]
pub struct Silence {
    played: AtomicU64,
    recorded: AtomicU64,
}

impl Silence {
    /// Frames programs have played into the card.
    #[must_use]
    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// Frames programs have recorded from the card.
    #[must_use]
    pub fn recorded(&self) -> u64 {
        self.recorded.load(Ordering::Relaxed)
    }
}

/// Frames in `samples`, `CHANNELS` samples to a frame.
fn frames(samples: usize) -> u64 {
    u64::try_from(samples / usize::from(CHANNELS)).unwrap_or(u64::MAX)
}

impl Uac2Streams for Silence {
    fn playback(&self, _frame: u64, samples: &[i32]) {
        self.played
            .fetch_add(frames(samples.len()), Ordering::Relaxed);
    }

    fn capture(&self, _frame: u64, samples: &mut [i32]) {
        samples.fill(0);
        self.recorded
            .fetch_add(frames(samples.len()), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{CHANNELS, LINE_CONNECTOR, NAME, SAMPLE_RATE, Silence, config};
    use usbip_audio::uac2::{Uac2Device, Uac2Streams};

    #[test]
    fn the_card_fits_a_high_speed_usb_audio_device() {
        assert_eq!(config().validate(), Ok(()));
        assert!(Uac2Device::new(config(), Silence::default()).is_ok());
    }

    #[test]
    fn its_input_is_a_line_not_a_microphone() {
        assert_eq!(config().capture_terminal_type, LINE_CONNECTOR);
    }

    #[test]
    fn it_plays_and_records_in_stereo_at_48_khz_under_its_name() {
        let card = config();
        assert_eq!(card.product, NAME);
        assert_eq!(card.sample_rate, SAMPLE_RATE);
        assert_eq!(
            (card.playback_channels, card.capture_channels),
            (CHANNELS, CHANNELS)
        );
    }

    #[test]
    fn recording_gives_silence_and_both_ways_are_counted_in_frames() {
        let silence = Silence::default();
        let mut recorded = [7; 12];
        silence.capture(0, &mut recorded);
        assert_eq!(recorded, [0; 12]);
        silence.playback(0, &[1; 8]);
        assert_eq!((silence.recorded(), silence.played()), (6, 4));
    }
}
