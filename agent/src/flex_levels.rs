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
//! **Receive.** [`RxLevel`] slowly holds the receive audio's loudness
//! (RMS) at the level WSJT-X decoded 55 FT8 signals at on FL-1 on
//! 2026-11-28 (about -32 dBFS RMS). It moves at most a dB a second and stays
//! within limits, so it never pumps with signals, and it holds still while
//! the radio transmits.
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

/// The receive loudness (RMS) held: what WSJT-X decoded well at on FL-1.
pub const RX_TARGET_DBFS: f64 = -32.0;
/// The most receive audio is made louder, and quieter.
const RX_MOST_UP_DB: f64 = 20.0;
const RX_MOST_DOWN_DB: f64 = 12.0;
/// The most the receive gain moves, in dB a second.
const RX_SLEW_DB_PER_S: f64 = 1.0;
/// How slowly the measured loudness follows: about 10 s.
const RX_SMOOTHING_S: f64 = 10.0;

/// The receive level: see the module notes.
#[derive(Debug, Clone)]
pub struct RxLevel {
    gain_db: f64,
    /// The loudness of what arrives, before our gain, smoothed.
    rms_db: Option<f64>,
}

impl Default for RxLevel {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            rms_db: None,
        }
    }
}

impl RxLevel {
    /// The gain now, in dB.
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// Scales `samples` (`seconds` long) by the gain, first taking in how
    /// loud they were. While `on_air` the gain holds still.
    pub fn apply(&mut self, samples: &mut [f32], seconds: f64, on_air: bool) {
        if !on_air && !samples.is_empty() {
            let power: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            // A packet holds at most a few hundred samples: exact as f64.
            #[allow(clippy::cast_precision_loss)]
            let mean = power / samples.len() as f64;
            if mean > 0.0 {
                let now_db = 10.0 * mean.log10();
                let weight = (seconds / RX_SMOOTHING_S).clamp(0.0, 1.0);
                let rms = self
                    .rms_db
                    .map_or(now_db, |was| was + (now_db - was) * weight);
                self.rms_db = Some(rms);
                let wanted = (RX_TARGET_DBFS - rms).clamp(-RX_MOST_DOWN_DB, RX_MOST_UP_DB);
                let most = RX_SLEW_DB_PER_S * seconds;
                self.gain_db += (wanted - self.gain_db).clamp(-most, most);
            }
        }
        // Within -12..=20 dB, so the narrowing only rounds.
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
        LEAST_DRIVE_DB, MOST_DRIVE_DB, MeterIds, RX_TARGET_DBFS, RxLevel, START_DRIVE_DB, Step,
        TxDrive, db, watts,
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

    #[test]
    fn receive_is_held_near_the_target_slowly() {
        let mut level = RxLevel::default();
        // A steady signal at -44 dBFS RMS, 12 dB quiet, in 5 ms packets.
        let amplitude = 10_f32.powf(-44.0 / 20.0);
        let mut packet = vec![0.0_f32; 120];
        let mut after_one_second = 0.0;
        for n in 0..12_000 {
            for (i, s) in packet.iter_mut().enumerate() {
                *s = if i % 2 == 0 { amplitude } else { -amplitude };
            }
            level.apply(&mut packet, 0.005, false);
            if n == 199 {
                after_one_second = level.gain_db();
            }
        }
        // No more than a dB in the first second...
        assert!(after_one_second <= 1.0 + 1e-9, "{after_one_second}");
        // ...and within a dB of the target after a minute.
        let out_db = 20.0 * f64::from(packet[0].abs()).log10();
        assert!((out_db - RX_TARGET_DBFS).abs() < 1.0, "{out_db}");
        // While on the air it holds still.
        let held = level.gain_db();
        let mut loud = vec![0.9_f32; 120];
        level.apply(&mut loud, 5.0, true);
        assert!((level.gain_db() - held).abs() < f64::EPSILON);
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
