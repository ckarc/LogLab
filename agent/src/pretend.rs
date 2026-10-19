// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The pretend radio: a stand-in for trying LogLab with no radio attached.
//!
//! It moves through a few typical spots on the bands, tuning slowly at each,
//! and says it is transmitting for a few seconds now and then, so every part
//! of the live display has something to show. It is not connected to
//! anything and cannot transmit.

use std::time::Instant;

use crate::rig::{ControlPath, Mode, Radio, RadioError, RadioIdentity, Reading};

/// How long the pretend radio stays at each spot, in seconds.
const STAY: u64 = 20;
/// How long it "transmits" at each spot, in seconds.
const TALK: u64 = 4;
/// How fast it tunes, in hertz per second.
const TUNING_RATE: u64 = 10;

/// The spots it visits: frequency, mode and filter width.
const SPOTS: [(u64, Mode, u32); 5] = [
    (14_205_000, Mode::Usb, 2400),
    (7_030_000, Mode::Cw, 500),
    (14_074_000, Mode::DataUsb, 3000),
    (3_850_000, Mode::Lsb, 2400),
    (146_520_000, Mode::Fm, 15_000),
];

/// Where the pretend radio sits while it listens to the pretend FT8 band:
/// 20 metres' FT8 frequency, upper sideband for data, a 3 kHz filter.
pub const FT8_DIAL_HZ: u64 = 14_074_000;

/// The pretend radio.
pub struct PretendRadio {
    identity: RadioIdentity,
    started: Instant,
    /// Whether it stays on FT8 instead of moving around the bands.
    ft8: bool,
}

impl PretendRadio {
    /// A pretend radio, starting at its first spot now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            identity: RadioIdentity {
                maker: "LabLog".to_owned(),
                model: "Pretend radio".to_owned(),
                path: ControlPath::Pretend,
            },
            started: Instant::now(),
            ft8: false,
        }
    }

    /// A pretend radio that stays on 20 metres FT8, listening, so the
    /// pretend FT8 band has a radio to come from.
    #[must_use]
    pub fn on_ft8() -> Self {
        Self {
            ft8: true,
            ..Self::new()
        }
    }

    /// What the pretend radio reports while it listens to FT8.
    #[must_use]
    pub fn listening_to_ft8() -> Reading {
        Reading {
            frequency_hz: FT8_DIAL_HZ,
            mode: Some(Mode::DataUsb),
            passband_hz: Some(3000),
            ptt: Some(false),
            vfo: Some("VFOA".to_owned()),
        }
    }

    /// What the pretend radio reports `seconds` after it started.
    #[must_use]
    pub fn at(seconds: u64) -> Reading {
        let index = usize::try_from(seconds / STAY).unwrap_or_default() % SPOTS.len();
        let (start_hz, mode, passband_hz) = SPOTS[index];
        let into = seconds % STAY;
        Reading {
            frequency_hz: start_hz + into * TUNING_RATE,
            mode: Some(mode),
            passband_hz: Some(passband_hz),
            ptt: Some((STAY - TALK..STAY).contains(&into)),
            vfo: Some("VFOA".to_owned()),
        }
    }
}

impl Default for PretendRadio {
    fn default() -> Self {
        Self::new()
    }
}

impl Radio for PretendRadio {
    fn identity(&self) -> &RadioIdentity {
        &self.identity
    }

    fn read(&mut self) -> Result<Reading, RadioError> {
        if self.ft8 {
            return Ok(Self::listening_to_ft8());
        }
        Ok(Self::at(self.started.elapsed().as_secs()))
    }
}

#[cfg(test)]
mod tests {
    use super::PretendRadio;
    use crate::rig::{Mode, Radio};

    #[test]
    fn starts_on_twenty_metres_phone() {
        let reading = PretendRadio::at(0);
        assert_eq!(reading.frequency_hz, 14_205_000);
        assert_eq!(reading.mode, Some(Mode::Usb));
        assert_eq!(reading.ptt, Some(false));
    }

    #[test]
    fn tunes_then_talks_then_moves_on() {
        assert_eq!(PretendRadio::at(5).frequency_hz, 14_205_050);
        assert_eq!(PretendRadio::at(17).ptt, Some(true));
        let next = PretendRadio::at(20);
        assert_eq!(next.frequency_hz, 7_030_000);
        assert_eq!(next.mode, Some(Mode::Cw));
        assert_eq!(next.ptt, Some(false));
        assert_eq!(PretendRadio::at(100).frequency_hz, 14_205_000);
    }

    #[test]
    fn stays_on_ft8_and_never_transmits_while_listening() {
        let mut radio = PretendRadio::on_ft8();
        let reading = radio.read().unwrap_or_else(|_| PretendRadio::at(0));
        assert_eq!(reading.frequency_hz, 14_074_000);
        assert_eq!(reading.mode, Some(Mode::DataUsb));
        assert_eq!(reading.ptt, Some(false));
    }
}
