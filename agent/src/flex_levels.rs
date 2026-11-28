// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest` setting the audio levels itself (milestone 4 of the sound card
//! plan; brian, 2026-11-27: the Agent "should monitor the audio and make
//! the required adjustments").
//!
//! **Transmit.** What apps play is scaled so the radio is driven just
//! right, whatever an app's own level (WSJT-X's Pwr slider, say). The drive
//! is a peak level in dBFS: the app's audio is scaled to reach it. While
//! the radio transmits, [`TxDrive`] raises the drive a step at a time and
//! watches forward power. When a step no longer raises the power, it
//! backs off one step and holds there. If the radio's ALC shows it limiting,
//! it backs off too. That is the rule found at FL-1 on 2026-11-22 (FLEX-8400:
//! "raise audio until FWDPWR stops rising, back off one step"; the ALC
//! limiter pinned at -2.5 dBFS). It never keys the radio and never changes
//! the radio's power setting; it only scales our own samples.
//!
//! **Receive.** [`RxLevel`] holds the band noise where WSJT-X's guide asks
//! for it: "around 30 dB when no signals are present" on the receive-level
//! scale at the lower left of its main window (WSJT-X user guide,
//! transceiver-setup). That scale is `10*log10(mean square)` of the 16-bit
//! samples WSJT-X records (`lib/symspec.f90`: `pxdb=10*log10(sq/(k-k0))`;
//! `Audio/soundin.cpp` opens the device at 16 bits), so 30 dB is an RMS of
//! about 31.6 counts of 32768: -60.3 dBFS. The noise is the quietest
//! half-second of the last 15 s, which falls in the gap between FT8 slots,
//! so the signals on the band do not pull the level down. The gain moves
//! slowly and holds still while the radio transmits.
//!
//! Plain arithmetic, tested on every platform. Times are passed in as
//! time since the audio started, so the tests set them.

use std::time::Duration;

/// Where the transmit drive starts, in dBFS of peak.
pub const START_DRIVE_DB: f64 = -10.0;
/// The most the drive goes to: just short of full scale.
pub const MOST_DRIVE_DB: f64 = -1.0;
/// The least it goes to.
pub const LEAST_DRIVE_DB: f64 = -30.0;
/// One step of the drive.
pub const STEP_DB: f64 = 1.0;
/// How long after a step the power is judged.
pub const STEP_EVERY: Duration = Duration::from_secs(1);
/// A step that raises forward power by less than this fraction has stopped
/// helping.
pub const STILL_RISING: f64 = 0.03;
/// Above this the radio's ALC is limiting (-2.5 dBFS pinned at FL-1).
pub const ALC_LIMITING_DBFS: f64 = -3.0;
/// The radio is on the air above this forward power.
pub const ON_AIR_WATTS: f64 = 1.0;
/// The most an app's audio is ever made louder: 24 dB.
const MOST_GAIN_DB: f64 = 24.0;
/// An app quieter than this is playing nothing: not judged.
const SILENT_DB: f64 = -60.0;
/// How fast the remembered peak of an app's audio falls, in dB a second,
/// so a quieter app is followed.
const PEAK_FALL_DB_PER_S: f64 = 3.0;

/// Watts from a FlexRadio power meter's raw value: dBm in 1/128 steps
/// (the API's meter units).
#[must_use]
pub fn watts(raw_dbm: i16) -> f64 {
    10_f64.powf(f64::from(raw_dbm) / 128.0 / 10.0) / 1000.0
}

/// dB from a FlexRadio meter's raw value in 1/128 steps.
#[must_use]
pub fn db(raw: i16) -> f64 {
    f64::from(raw) / 128.0
}

/// The transmit drive: see the module notes.
#[derive(Debug, Clone)]
pub struct TxDrive {
    drive_db: f64,
    /// The app's loudest recent sample, in dBFS, falling slowly.
    app_peak_db: f64,
    app_peak_at: Duration,
    /// Forward power when the last step was taken, and when.
    last: Option<(f64, Duration)>,
    /// Whether the last step was up.
    raised: bool,
    /// Found the point where power stops rising.
    settled: bool,
}

impl Default for TxDrive {
    fn default() -> Self {
        Self {
            drive_db: START_DRIVE_DB,
            app_peak_db: SILENT_DB,
            app_peak_at: Duration::ZERO,
            last: None,
            raised: false,
            settled: false,
        }
    }
}

/// What a step of [`TxDrive::meters`] did, for the log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Up(f64),
    Down(f64),
    /// Backed off because the ALC is limiting.
    Limiting(f64),
}

impl TxDrive {
    /// The drive now, in dBFS of peak.
    #[must_use]
    pub fn drive_db(&self) -> f64 {
        self.drive_db
    }

    /// Whether it has found where power stops rising.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.settled
    }

    /// Takes in the loudest sample of a stretch of the app's audio, at
    /// `now`.
    pub fn app_peak(&mut self, peak: f32, now: Duration) {
        let fallen = self.app_peak_db
            - PEAK_FALL_DB_PER_S * now.saturating_sub(self.app_peak_at).as_secs_f64();
        let heard = 20.0 * f64::from(peak.abs()).max(1e-9).log10();
        self.app_peak_db = heard.max(fallen).max(SILENT_DB);
        self.app_peak_at = now;
    }

    /// What to multiply the app's samples by.
    #[must_use]
    pub fn gain(&self) -> f32 {
        if self.app_peak_db <= SILENT_DB {
            return 1.0;
        }
        let gain_db = (self.drive_db - self.app_peak_db).min(MOST_GAIN_DB);
        // Within -90..=24 dB, so the narrowing only rounds.
        #[allow(clippy::cast_possible_truncation)]
        let gain = 10_f64.powf(gain_db / 20.0) as f32;
        gain
    }

    /// Takes in the radio's forward power and ALC at `now`, and steps the
    /// drive if it is time.
    pub fn meters(
        &mut self,
        forward_watts: f64,
        alc_dbfs: Option<f64>,
        now: Duration,
    ) -> Option<Step> {
        if forward_watts < ON_AIR_WATTS || self.app_peak_db <= SILENT_DB {
            // Off the air, or nothing played: the next transmission starts
            // judging afresh from the drive found so far.
            self.last = None;
            self.raised = false;
            return None;
        }
        if let Some((_, at)) = self.last
            && now.saturating_sub(at) < STEP_EVERY
        {
            return None;
        }
        let step = if alc_dbfs.is_some_and(|alc| alc > ALC_LIMITING_DBFS) {
            self.settled = true;
            self.drive_db = (self.drive_db - STEP_DB).max(LEAST_DRIVE_DB);
            Some(Step::Limiting(self.drive_db))
        } else if self.settled {
            None
        } else if self.raised
            && let Some((before, _)) = self.last
            && forward_watts < before * (1.0 + STILL_RISING)
        {
            self.settled = true;
            self.raised = false;
            self.drive_db = (self.drive_db - STEP_DB).max(LEAST_DRIVE_DB);
            Some(Step::Down(self.drive_db))
        } else if self.drive_db < MOST_DRIVE_DB {
            self.raised = true;
            self.drive_db = (self.drive_db + STEP_DB).min(MOST_DRIVE_DB);
            Some(Step::Up(self.drive_db))
        } else {
            self.settled = true;
            None
        };
        self.last = Some((forward_watts, now));
        step
    }
}

/// Full scale on WSJT-X's receive-level scale: `10*log10(32768^2)`.
pub const WSJTX_FULL_SCALE_DB: f64 = 90.309;
/// What WSJT-X's guide asks the band noise to read on that scale.
pub const WSJTX_NOISE_DB: f64 = 30.0;
/// The same, in dBFS RMS.
pub const RX_NOISE_DBFS: f64 = WSJTX_NOISE_DB - WSJTX_FULL_SCALE_DB;
/// The most receive audio is made louder, and quieter.
const RX_MOST_DB: f64 = 40.0;
/// The most the receive gain moves, in dB a second.
const RX_SLEW_DB_PER_S: f64 = 2.0;
/// The noise is measured in stretches this long...
const RX_WINDOW_S: f64 = 0.5;
/// ...and is the quietest stretch of this long: a whole FT8 slot and more.
const RX_LOOK_BACK_S: f64 = 15.0;
/// Stretches dropped when a transmission starts (a second's worth).
const RX_DROP_BEFORE: usize = 2;
/// How long after a transmission measuring waits.
const RX_SETTLE_S: f64 = 2.0;
/// Quieter than this, a stretch is silence (no receive audio), not noise.
const RX_SILENCE_DBFS: f64 = -110.0;

/// The receive level: see the module notes.
#[derive(Debug, Clone, Default)]
pub struct RxLevel {
    gain_db: f64,
    /// The stretch being measured: summed power, samples, seconds.
    power: f64,
    count: usize,
    seconds: f64,
    /// The loudness of each finished stretch, before our gain, newest last.
    stretches: std::collections::VecDeque<f64>,
    /// Whether the last audio came while on the air.
    was_on_air: bool,
    /// How long, in seconds, measuring waits yet after a transmission.
    settle_s: f64,
}

impl RxLevel {
    /// The gain now, in dB.
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// The band noise as it arrives, in dBFS RMS, once 15 s are measured.
    #[must_use]
    pub fn noise_dbfs(&self) -> Option<f64> {
        let full = Self::stretches_kept();
        (self.stretches.len() >= full)
            .then(|| self.stretches.iter().copied().fold(f64::INFINITY, f64::min))
    }

    /// What WSJT-X's receive-level scale should read for the noise now.
    #[must_use]
    pub fn wsjtx_noise_db(&self) -> Option<f64> {
        self.noise_dbfs()
            .map(|noise| noise + self.gain_db + WSJTX_FULL_SCALE_DB)
    }

    fn stretches_kept() -> usize {
        // 30: exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let kept = (RX_LOOK_BACK_S / RX_WINDOW_S).round() as usize;
        kept
    }

    /// Scales `samples` (`seconds` long) by the gain, first taking in how
    /// loud they were. While `on_air` the gain holds still and nothing is
    /// measured.
    pub fn apply(&mut self, samples: &mut [f32], seconds: f64, on_air: bool) {
        if on_air {
            // The receiver goes quiet around a transmission, a little before
            // forward power shows and a little after it falls: what was
            // measured just before is dropped, and nothing is measured until
            // a while after (FL-1, 2026-11-28: FT8 transmissions pulled the
            // gain up 35 dB).
            if !self.was_on_air {
                for _ in 0..RX_DROP_BEFORE {
                    self.stretches.pop_back();
                }
            }
            self.was_on_air = true;
            self.settle_s = RX_SETTLE_S;
            self.power = 0.0;
            self.count = 0;
            self.seconds = 0.0;
        } else if self.settle_s > 0.0 {
            self.was_on_air = false;
            self.settle_s -= seconds;
        } else if !samples.is_empty() {
            self.was_on_air = false;
            self.power += samples
                .iter()
                .map(|&s| f64::from(s) * f64::from(s))
                .sum::<f64>();
            self.count += samples.len();
            self.seconds += seconds;
            if self.seconds >= RX_WINDOW_S {
                // Some thousands of samples: exact as f64.
                #[allow(clippy::cast_precision_loss)]
                let mean = self.power / self.count as f64;
                let db = 10.0 * mean.max(1e-12).log10();
                // A stretch of (near) silence is no receive audio at all,
                // not band noise.
                if db > RX_SILENCE_DBFS {
                    self.stretches.push_back(db);
                }
                while self.stretches.len() > Self::stretches_kept() {
                    self.stretches.pop_front();
                }
                if let Some(noise) = self.noise_dbfs() {
                    let wanted = (RX_NOISE_DBFS - noise).clamp(-RX_MOST_DB, RX_MOST_DB);
                    let most = RX_SLEW_DB_PER_S * self.seconds;
                    self.gain_db += (wanted - self.gain_db).clamp(-most, most);
                }
                self.power = 0.0;
                self.count = 0;
                self.seconds = 0.0;
            }
        }
        // Within -40..=40 dB, so the narrowing only rounds.
        #[allow(clippy::cast_possible_truncation)]
        let gain = 10_f64.powf(self.gain_db / 20.0) as f32;
        for sample in samples.iter_mut() {
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }
}

/// The meters `FlexTest` watches, found by name in the radio's meter list
/// (`sub meter all`): `#`-separated `<number>.<field>=<value>` words, such
/// as `15.src=TX-#15.num=1#15.nam=FWDPWR#15.unit=dBm#`. Numbers differ by
/// radio (on FL-1's FLEX-8400: FWDPWR 8, ALC 30).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeterIds {
    pub forward: Option<u16>,
    pub alc: Option<u16>,
}

impl MeterIds {
    /// Takes in one `meter ...` status line.
    pub fn take(&mut self, text: &str) {
        let mut fields: std::collections::BTreeMap<u16, (String, String, String)> =
            std::collections::BTreeMap::new();
        for word in text.split('#') {
            let Some((name, value)) = word.split_once('=') else {
                continue;
            };
            let Some((number, field)) = name.trim().split_once('.') else {
                continue;
            };
            let Ok(number) = number.parse::<u16>() else {
                continue;
            };
            let entry = fields.entry(number).or_default();
            let value = value.trim_matches('"').to_owned();
            match field {
                "src" => entry.0 = value,
                "nam" => entry.1 = value,
                "unit" => entry.2 = value.to_ascii_lowercase(),
                _ => {}
            }
        }
        for (number, (src, name, unit)) in fields {
            if !src.starts_with("TX") {
                continue;
            }
            match (name.as_str(), unit.as_str()) {
                ("FWDPWR", "dbm") => self.forward = Some(number),
                ("ALC", "dbfs") => self.alc = Some(number),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LEAST_DRIVE_DB, MOST_DRIVE_DB, MeterIds, RX_NOISE_DBFS, RxLevel, START_DRIVE_DB, Step,
        TxDrive, WSJTX_NOISE_DB, db, watts,
    };
    use std::time::Duration;

    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn reads_the_meters_units() {
        // 50 dBm = 100 W; 40 dBm = 10 W.
        assert!((watts(50 * 128) - 100.0).abs() < 1e-6);
        assert!((watts(40 * 128) - 10.0).abs() < 1e-6);
        assert!((db(-320) + 2.5).abs() < 1e-9);
    }

    #[test]
    fn an_apps_own_level_doesnt_matter() {
        let mut quiet = TxDrive::default();
        quiet.app_peak(0.1, at(0));
        let mut loud = TxDrive::default();
        loud.app_peak(1.0, at(0));
        // Both come out at the drive's peak.
        let out = |drive: &TxDrive, peak: f32| 20.0 * f64::from(peak * drive.gain()).log10();
        assert!((out(&quiet, 0.1) - START_DRIVE_DB).abs() < 0.01);
        assert!((out(&loud, 1.0) - START_DRIVE_DB).abs() < 0.01);
        // Nothing playing: left alone.
        assert!((TxDrive::default().gain() - 1.0).abs() < f32::EPSILON);
    }

    /// A radio whose power rises with drive until it flattens at 90 W,
    /// as FL-1's 8400 did on 10 m, with ALC limiting above -3 dBFS.
    fn radio(drive_db: f64) -> (f64, f64) {
        let watts = (90.0 * 10_f64.powf((drive_db + 4.0) / 10.0)).min(90.0);
        (watts, drive_db + 1.6)
    }

    #[test]
    fn raises_the_drive_until_power_stops_rising_then_backs_off() {
        let mut drive = TxDrive::default();
        let mut steps = vec![];
        for second in 0..20 {
            drive.app_peak(0.9, at(second));
            let (watts, alc) = radio(drive.drive_db());
            if let Some(step) = drive.meters(watts, Some(alc), at(second)) {
                steps.push(step);
            }
        }
        assert!(drive.settled(), "{steps:?}");
        // Settles at or below where power flattens (-4 dBFS) and the ALC
        // limits (-4.6), never above.
        assert!(
            drive.drive_db() <= -4.0 && drive.drive_db() >= -6.0,
            "{steps:?}"
        );
        assert!(matches!(steps.first(), Some(Step::Up(_))));
        // Held from then on.
        let held = drive.drive_db();
        for second in 20..30 {
            let (watts, alc) = radio(drive.drive_db());
            drive.meters(watts, Some(alc), at(second));
        }
        assert!((drive.drive_db() - held).abs() < f64::EPSILON);
    }

    #[test]
    fn backs_off_when_the_alc_limits_and_never_goes_past_its_bounds() {
        let mut drive = TxDrive::default();
        drive.app_peak(0.5, at(0));
        assert_eq!(
            drive.meters(50.0, Some(-1.0), at(0)),
            Some(Step::Limiting(START_DRIVE_DB - 1.0))
        );
        for second in 1..60 {
            drive.meters(50.0, Some(-1.0), at(second));
        }
        assert!((drive.drive_db() - LEAST_DRIVE_DB).abs() < f64::EPSILON);
        let mut climbing = TxDrive::default();
        let mut watts = 2.0;
        for second in 0..60 {
            climbing.app_peak(0.5, at(second));
            // Power keeps rising a lot with every step.
            watts *= 1.2;
            climbing.meters(watts, None, at(second));
        }
        assert!(climbing.drive_db() <= MOST_DRIVE_DB);
    }

    #[test]
    fn does_nothing_off_the_air_or_with_nothing_playing() {
        let mut drive = TxDrive::default();
        drive.app_peak(0.5, at(0));
        assert_eq!(drive.meters(0.1, Some(-20.0), at(0)), None);
        let mut silent = TxDrive::default();
        assert_eq!(silent.meters(50.0, Some(-20.0), at(0)), None);
        assert!((drive.drive_db() - START_DRIVE_DB).abs() < f64::EPSILON);
    }

    #[test]
    fn steps_no_faster_than_once_a_second() {
        let mut drive = TxDrive::default();
        drive.app_peak(0.5, at(0));
        assert!(drive.meters(10.0, None, Duration::ZERO).is_some());
        assert!(
            drive
                .meters(20.0, None, Duration::from_millis(500))
                .is_none()
        );
        assert!(drive.meters(20.0, None, Duration::from_secs(1)).is_some());
    }

    /// FT8-like receive audio: noise at `noise` dBFS RMS all the time, and
    /// signals 20 dB louder for the first 12.6 s of every 15 s slot.
    fn band(second: f64, noise: f64) -> f32 {
        let slot = second % 15.0;
        let db = if slot < 12.6 { noise + 20.0 } else { noise };
        // dBFS RMS of a square wave is its amplitude.
        #[allow(clippy::cast_possible_truncation)]
        let amplitude = 10_f64.powf(db / 20.0) as f32;
        amplitude
    }

    #[test]
    fn receive_holds_the_band_noise_at_30_db_on_wsjtx_scale() {
        let mut level = RxLevel::default();
        let mut packet = vec![0.0_f32; 120];
        // 5 ms packets; the noise arrives 15 dB too quiet (-75.3 dBFS).
        let mut second = 0.0;
        let mut gain_at_16_s = 0.0;
        for n in 0..24_000 {
            let amplitude = band(second, RX_NOISE_DBFS - 15.0);
            for (i, s) in packet.iter_mut().enumerate() {
                *s = if i % 2 == 0 { amplitude } else { -amplitude };
            }
            level.apply(&mut packet, 0.005, false);
            second += 0.005;
            if n == 3200 {
                gain_at_16_s = level.gain_db();
            }
        }
        // Nothing for the first 15 s, then 1 dB a half second: 3 dB by 16 s...
        assert!(gain_at_16_s <= 3.0 + 1e-9, "{gain_at_16_s}");
        // ...and after two minutes the noise reads 30 dB on WSJT-X's scale,
        // though signals filled most of the time.
        let reads = level.wsjtx_noise_db().unwrap_or_default();
        assert!((reads - WSJTX_NOISE_DB).abs() < 0.5, "{reads}");
        assert!((level.gain_db() - 15.0).abs() < 0.5);
        // While on the air it holds still.
        let held = level.gain_db();
        let mut loud = vec![0.9_f32; 120];
        for _ in 0..1000 {
            level.apply(&mut loud, 0.005, true);
        }
        assert!((level.gain_db() - held).abs() < f64::EPSILON);
    }

    #[test]
    fn transmissions_and_the_quiet_around_them_dont_move_the_receive_gain() {
        let mut level = RxLevel::default();
        let mut packet = vec![0.0_f32; 120];
        let mut second = 0.0;
        // Two minutes of band noise already at the target: gain about 0 dB.
        for _ in 0..24_000 {
            let amplitude = band(second, RX_NOISE_DBFS);
            for (i, s) in packet.iter_mut().enumerate() {
                *s = if i % 2 == 0 { amplitude } else { -amplitude };
            }
            level.apply(&mut packet, 0.005, false);
            second += 0.005;
        }
        let before = level.gain_db();
        assert!(before.abs() < 0.5, "{before}");
        // Then FT8 transmissions, 13 s each, with the receiver silent for
        // half a second before power shows and a second after it falls.
        for _ in 0..8 {
            let mut quiet = vec![0.0_f32; 120];
            for _ in 0..100 {
                level.apply(&mut quiet, 0.005, false);
            }
            for _ in 0..2600 {
                level.apply(&mut quiet, 0.005, true);
            }
            for _ in 0..200 {
                level.apply(&mut quiet, 0.005, false);
            }
            for _ in 0..100 {
                let amplitude = band(12.7, RX_NOISE_DBFS);
                for (i, s) in packet.iter_mut().enumerate() {
                    *s = if i % 2 == 0 { amplitude } else { -amplitude };
                }
                level.apply(&mut packet, 0.005, false);
            }
        }
        let after = level.gain_db();
        assert!((after - before).abs() < 1.0, "{before} -> {after}");
    }

    #[test]
    fn wsjtx_scale_matches_its_formula() {
        // symspec.f90: pxdb = 10*log10(mean of x*x) for 16-bit samples x.
        let x: f64 = 31.62;
        let pxdb = 10.0 * (x * x).log10();
        let dbfs = 20.0 * (x / 32768.0).log10();
        assert!((pxdb - WSJTX_NOISE_DB).abs() < 0.01);
        assert!((dbfs - RX_NOISE_DBFS).abs() < 0.01);
    }

    #[test]
    fn finds_the_forward_power_and_alc_meters_by_name() {
        let mut ids = MeterIds::default();
        ids.take(
            "3.src=SLC#3.num=0#3.nam=LEVEL#3.unit=dBm#\
             8.src=TX-#8.num=1#8.nam=FWDPWR#8.desc=RF Power Forward#8.unit=dBm#\
             10.src=TX-#10.nam=SWR#10.unit=SWR#30.src=TX-#30.nam=ALC#30.unit=dBFS#",
        );
        assert_eq!(
            ids,
            MeterIds {
                forward: Some(8),
                alc: Some(30)
            }
        );
    }
}
