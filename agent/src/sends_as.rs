// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The call and grid square the programs here send on the air, kept to
//! where the station is now, so a radio and computer moved to another
//! place (FieldLab on the road) never send last week's grid.
//!
//! The grid comes from, in order:
//!
//! 1. **FieldLab's GPS**, measured: the trailer's fix, read from FieldLab's
//!    MQTT bus (`--mqtt`, or `$LABLOG_MQTT`, as the Engine takes it), topic
//!    `fieldlab/location`, `{"lat", "lon", "alt", "ts"}`, published by
//!    FieldLab's kiosk (see the Engine's `fieldlab.py`); or the same fix as
//!    the Engine passes it on (`grid_source` "gps", see `carrier`). A fix
//!    over an hour old doesn't count, as on the kiosk's own pages.
//! 2. **The grid given with `--grid`**: where this station is said to be.
//! 3. **The grid the Engine has saved** for the operator signed in here
//!    (`grid_source` "saved"): their own station's, which may not be here.
//!
//! The call is the one the Engine says QSOs here are made as (the
//! operation's, when one is chosen), else the one given with `--call`.
//! Where neither is known, a program's own setting is left alone.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::engine::StationInfo;

/// FieldLab's GPS fix, on its bus.
pub const LOCATION_TOPIC: &str = "fieldlab/location";
/// MQTT's own port, when `--mqtt` names none.
pub const MQTT_PORT: u16 = 1883;
/// How often the bus is read.
const READ_EVERY: Duration = Duration::from_secs(30);
/// How old a fix may be and still count, in seconds.
const FIX_MAX_AGE: f64 = 3600.0;
/// How far ahead of this computer's clock a fix may be, in seconds.
const FIX_AHEAD: f64 = 60.0;
const CLIENT_ID: &str = "lablog-agent-fieldlab";

/// Where the station's grid came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridFrom {
    /// FieldLab's GPS: measured.
    Gps,
    /// Given with `--grid`.
    Stated,
    /// The Engine's saved grid for the operator signed in.
    Saved,
}

/// The call and grid the programs here send, where known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendsAs {
    /// The call, in capitals.
    pub call: Option<String>,
    /// The 4- or 6-character grid square, in capitals, as WSJT-X and
    /// `JS8Call` keep it, and where it came from.
    pub grid: Option<(String, GridFrom)>,
}

impl SendsAs {
    /// The grid alone.
    #[must_use]
    pub fn grid(&self) -> Option<&str> {
        self.grid.as_ref().map(|(grid, _)| grid.as_str())
    }

    /// What the programs send, in words.
    #[must_use]
    pub fn text(&self) -> String {
        let call = self
            .call
            .as_deref()
            .map_or_else(|| "their own call".to_owned(), ToOwned::to_owned);
        let grid = self.grid.as_ref().map_or_else(
            || {
                "their own grid (none known here: give --grid, or --mqtt for FieldLab's GPS)"
                    .to_owned()
            },
            |(grid, from)| {
                let from = match from {
                    GridFrom::Gps => "FieldLab's GPS",
                    GridFrom::Stated => "--grid",
                    GridFrom::Saved => "the grid LabLog has saved for the operator",
                };
                format!("grid {grid} (from {from})")
            },
        );
        format!("{call} and {grid}")
    }
}

/// What the station was told at its start: FieldLab's bus, and a call and
/// grid to use where nothing better is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stated {
    /// FieldLab's MQTT broker: host and port.
    pub mqtt: Option<(String, u16)>,
    /// `--call`, checked, in capitals.
    pub call: Option<String>,
    /// `--grid`, checked, in capitals.
    pub grid: Option<String>,
}

/// `HOST` or `HOST:PORT` as a broker's address (port 1883 when not given).
///
/// # Errors
///
/// No host, or a port that isn't a number.
pub fn broker(text: &str) -> Result<(String, u16), String> {
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse()
                .map_err(|_| format!("--mqtt needs HOST or HOST:PORT, not {text:?}"))?,
        ),
        None => (text, MQTT_PORT),
    };
    if host.is_empty() {
        return Err(format!("--mqtt needs HOST or HOST:PORT, not {text:?}"));
    }
    Ok((host.to_owned(), port))
}

/// `text` as a 4- or 6-character Maidenhead grid square in capitals, or
/// `None` if it isn't one: fields A to R, squares 0 to 9, subsquares A to X
/// (the patterns WSJT-X's `MaidenheadLocatorValidator` accepts).
#[must_use]
pub fn checked_grid(text: &str) -> Option<String> {
    let grid = text.trim().to_ascii_uppercase();
    let bytes = grid.as_bytes();
    let field = |at: usize| bytes.get(at).is_some_and(|b| (b'A'..=b'R').contains(b));
    let square = |at: usize| bytes.get(at).is_some_and(u8::is_ascii_digit);
    let sub = |at: usize| bytes.get(at).is_some_and(|b| (b'A'..=b'X').contains(b));
    let good = matches!(bytes.len(), 4 | 6)
        && field(0)
        && field(1)
        && square(2)
        && square(3)
        && (bytes.len() == 4 || (sub(4) && sub(5)));
    good.then_some(grid)
}

/// `text` as a call in capitals, or `None` if it doesn't look like one:
/// 3 to 12 letters, digits and `/`, with at least one digit and one letter.
#[must_use]
pub fn checked_call(text: &str) -> Option<String> {
    let call = text.trim().to_ascii_uppercase();
    let good = (3..=12).contains(&call.len())
        && call.chars().all(|c| c.is_ascii_alphanumeric() || c == '/')
        && call.chars().any(|c| c.is_ascii_digit())
        && call.chars().any(|c| c.is_ascii_alphabetic());
    good.then_some(call)
}

/// The 6-character grid square holding the place at `lat`, `lon` degrees,
/// or `None` off the map: 20 by 10 degree fields, 2 by 1 degree squares,
/// then 24 subsquares a side (the Engine's `place_grid`, in capitals).
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each part is checked to be from 0 to 24 before it is cast"
)]
pub fn grid_at(lat: f64, lon: f64) -> Option<String> {
    if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
        return None;
    }
    let lon = (lon + 180.0).min(359.999_999);
    let lat = (lat + 90.0).min(179.999_999);
    let letter = |base: u8, part: f64| char::from(base + part.floor() as u8);
    Some(
        [
            letter(b'A', lon / 20.0),
            letter(b'A', lat / 10.0),
            letter(b'0', lon % 20.0 / 2.0),
            letter(b'0', lat % 10.0),
            letter(b'A', lon % 2.0 * 12.0),
            letter(b'A', lat % 1.0 * 24.0),
        ]
        .iter()
        .collect(),
    )
}

/// FieldLab's GPS fix: degrees, and when (seconds since 1970, UTC).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Latitude, degrees north.
    pub lat: f64,
    /// Longitude, degrees east.
    pub lon: f64,
    /// When it was taken.
    pub at: f64,
}

impl Fix {
    /// Whether the fix still counts at `now` (seconds since 1970).
    #[must_use]
    pub fn recent(&self, now: f64) -> bool {
        (-FIX_AHEAD..=FIX_MAX_AGE).contains(&(now - self.at))
    }
}

/// A `fieldlab/location` message as a fix, if it is a usable one (the
/// Engine's `parse_fix`: a number for each of `lat`, `lon` and `ts`, on the
/// map, and not 0, 0, which a GPS with no fix sends).
#[must_use]
pub fn parse_fix(payload: &[u8]) -> Option<Fix> {
    let doc: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let number = |key: &str| doc.get(key)?.as_f64().filter(|value| value.is_finite());
    let (lat, lon, at) = (number("lat")?, number("lon")?, number("ts")?);
    let on_map = (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon);
    #[allow(
        clippy::float_cmp,
        reason = "0, 0 exactly is what a GPS with no fix sends"
    )]
    let no_fix = lat == 0.0 && lon == 0.0;
    (on_map && !no_fix).then_some(Fix { lat, lon, at })
}

/// What the programs send, from FieldLab's GPS fix `fix` (if still recent
/// at `now`), what the Engine last said `engine`, and what the station was
/// told `stated`.
#[must_use]
pub fn choose(
    fix: Option<Fix>,
    now: f64,
    engine: Option<&StationInfo>,
    stated: &Stated,
) -> SendsAs {
    let engine_grid = |source: &str| {
        engine
            .filter(|info| info.grid_source.as_deref() == Some(source))
            .and_then(|info| info.grid.as_deref())
            .and_then(checked_grid)
    };
    let grid = fix
        .filter(|fix| fix.recent(now))
        .and_then(|fix| grid_at(fix.lat, fix.lon))
        .or_else(|| engine_grid("gps"))
        .map(|grid| (grid, GridFrom::Gps))
        .or_else(|| stated.grid.clone().map(|grid| (grid, GridFrom::Stated)))
        .or_else(|| engine_grid("saved").map(|grid| (grid, GridFrom::Saved)));
    let call = engine
        .and_then(|info| info.station_call.as_deref())
        .and_then(checked_call)
        .or_else(|| stated.call.clone());
    SendsAs { call, grid }
}

/// What the programs send, as the station last worked it out: what
/// `programs` writes. Nothing until the station starts keeping up.
static LATEST: Mutex<SendsAs> = Mutex::new(SendsAs {
    call: None,
    grid: None,
});
/// Whether [`LATEST`] changed since [`take_changed`] last looked.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// How often the Agent works out again what the programs send.
const UPDATE_EVERY: Duration = Duration::from_secs(5);

/// What the programs send, as the station last worked it out.
#[must_use]
pub fn current() -> SendsAs {
    LATEST
        .lock()
        .map(|latest| latest.clone())
        .unwrap_or_default()
}

/// Whether what the programs send changed since this was last asked, so
/// the programs that are closed should be set up again.
pub fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::SeqCst)
}

/// Keeps `sends` as what the programs send; whether it changed.
fn keep(sends: &SendsAs) -> bool {
    let Ok(mut latest) = LATEST.lock() else {
        return false;
    };
    if *latest == *sends {
        return false;
    }
    latest.clone_from(sends);
    true
}

/// Seconds since 1970, UTC, by this computer's clock.
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Keeps up with where the station is, in the background: reads FieldLab's
/// bus, when there is one, and what the Engine last said. Dropping it stops
/// it.
pub struct Whereabouts {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Whereabouts {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Whereabouts {
    /// Works out what the programs send, says it, and starts keeping up,
    /// with what the Engine last said kept in the records folder `records`
    /// (see `carrier`).
    #[must_use]
    pub fn start(stated: Stated, records: PathBuf) -> Self {
        let fix = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let engine = crate::carrier::last_station(&records);
        let sends = choose(None, now(), engine.as_ref(), &stated);
        keep(&sends);
        CHANGED.store(false, Ordering::SeqCst);
        crate::print_event(&format!("Programs send {}.", sends.text()));
        let mut threads = Vec::new();
        if let Some((host, port)) = stated.mqtt.clone() {
            let (fix, stop) = (Arc::clone(&fix), Arc::clone(&stop));
            threads.push(thread::spawn(move || read_bus(&host, port, &fix, &stop)));
        }
        let flag = Arc::clone(&stop);
        threads.push(thread::spawn(move || {
            while wait(UPDATE_EVERY, &flag) {
                let fix = fix.lock().ok().and_then(|fix| *fix);
                let engine = crate::carrier::last_station(&records);
                let sends = choose(fix, now(), engine.as_ref(), &stated);
                if keep(&sends) {
                    CHANGED.store(true, Ordering::SeqCst);
                    crate::print_event(&format!(
                        "Programs now send {}.\n-> Each closed program is set up for it now; \
                         one that is running, as soon as it closes.",
                        sends.text()
                    ));
                }
            }
        }));
        Self { stop, threads }
    }
}

/// Waits `time`, or until `stop`; whether to go on.
fn wait(time: Duration, stop: &AtomicBool) -> bool {
    let until = std::time::Instant::now() + time;
    while std::time::Instant::now() < until {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(200));
    }
    !stop.load(Ordering::SeqCst)
}

/// Reads FieldLab's GPS fix from the broker every [`READ_EVERY`] into
/// `fix`, until `stop`. A read that fails keeps the last fix, which counts
/// only while it is recent; the first failure, and the first fix after
/// one, are said.
fn read_bus(host: &str, port: u16, fix: &Mutex<Option<Fix>>, stop: &AtomicBool) {
    let mut failing = None;
    while !stop.load(Ordering::SeqCst) {
        match crate::mqtt::read_retained(host, port, LOCATION_TOPIC, CLIENT_ID) {
            Ok(payload) => {
                let found = payload.as_deref().and_then(parse_fix);
                if found.is_some() && failing.take().is_some() {
                    crate::print_event(&format!("FieldLab's GPS: heard again on {host}:{port}."));
                }
                if found.is_none() && failing.is_none() {
                    failing = Some(());
                    crate::print_event(&format!(
                        "FieldLab's GPS: no usable fix on {host}:{port} ({LOCATION_TOPIC})."
                    ));
                }
                if let (Some(found), Ok(mut slot)) = (found, fix.lock()) {
                    *slot = Some(found);
                }
            }
            Err(why) => {
                if failing.replace(()).is_none() {
                    crate::print_event(&format!(
                        "FieldLab's GPS: not read from {host}:{port} ({why}); tried again \
                         every {} s.",
                        READ_EVERY.as_secs()
                    ));
                }
            }
        }
        if !wait(READ_EVERY, stop) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Fix, GridFrom, SendsAs, Stated, broker, checked_call, checked_grid, choose, grid_at,
        parse_fix,
    };
    use crate::engine::StationInfo;

    #[test]
    fn finds_the_grid_square_of_a_place() {
        // The same places and answers as the Engine's own tests
        // (tests/reference/test_locator.py), in capitals.
        assert_eq!(grid_at(41.714_775, -72.727_260).as_deref(), Some("FN31PR"));
        assert_eq!(grid_at(38.979_167, -94.625).as_deref(), Some("EM28QX"));
        assert_eq!(grid_at(90.0, 180.0).as_deref(), Some("RR99XX"));
        assert_eq!(grid_at(-90.0, -180.0).as_deref(), Some("AA00AA"));
        assert_eq!(grid_at(91.0, 0.0), None);
    }

    #[test]
    fn checks_grids_and_calls() {
        assert_eq!(checked_grid("em18ct").as_deref(), Some("EM18CT"));
        assert_eq!(checked_grid(" EM18 ").as_deref(), Some("EM18"));
        for bad in ["EM1", "EM18c", "SM18", "EM18cz", "EM18ct00", "", "1M18"] {
            assert_eq!(checked_grid(bad), None, "{bad}");
        }
        assert_eq!(checked_call("w0cy").as_deref(), Some("W0CY"));
        assert_eq!(checked_call("K9KSU/P").as_deref(), Some("K9KSU/P"));
        for bad in ["W0", "WOCY", "1234", "W0 CY", "W0CY;x"] {
            assert_eq!(checked_call(bad), None, "{bad}");
        }
        assert_eq!(broker("10.73.10.10"), Ok(("10.73.10.10".to_owned(), 1883)));
        assert_eq!(broker("kiosk:1884"), Ok(("kiosk".to_owned(), 1884)));
        assert!(broker("kiosk:x").is_err());
        assert!(broker(":1883").is_err());
    }

    #[test]
    fn reads_a_fix_as_the_engine_does() {
        assert_eq!(
            parse_fix(br#"{"lat":41.5,"lon":-93.6,"alt":290,"ts":1000}"#),
            Some(Fix {
                lat: 41.5,
                lon: -93.6,
                at: 1000.0
            })
        );
        for bad in [
            &br#"{"lat":0,"lon":0,"ts":1000}"#[..],
            br#"{"lat":"41.5","lon":-93.6,"ts":1000}"#,
            br#"{"lat":95,"lon":-93.6,"ts":1000}"#,
            br#"{"lon":-93.6,"ts":1000}"#,
            b"not json",
        ] {
            assert_eq!(parse_fix(bad), None);
        }
        let fix = Fix {
            lat: 1.0,
            lon: 1.0,
            at: 1000.0,
        };
        assert!(fix.recent(1000.0 + 3600.0));
        assert!(!fix.recent(1000.0 + 3601.0));
        assert!(fix.recent(1000.0 - 60.0));
        assert!(!fix.recent(1000.0 - 61.0));
    }

    fn engine(call: Option<&str>, grid: Option<&str>, source: Option<&str>) -> StationInfo {
        StationInfo {
            station: "FL 1".to_owned(),
            operator: Some("K9KSU".to_owned()),
            station_call: call.map(str::to_owned),
            operation: None,
            grid: grid.map(str::to_owned),
            grid_source: source.map(str::to_owned),
        }
    }

    #[test]
    fn gps_beats_a_stated_grid_which_beats_a_saved_one() {
        let stated = Stated {
            mqtt: None,
            call: Some("W0CY".to_owned()),
            grid: Some("EM18CT".to_owned()),
        };
        // Des Moines, EN31eo by the Engine's place_grid.
        let fix = Fix {
            lat: 41.586,
            lon: -93.625,
            at: 1000.0,
        };
        let gps = choose(Some(fix), 1010.0, None, &stated);
        assert_eq!(gps.grid(), Some("EN31EO"));
        assert_eq!(gps.grid.map(|(_, from)| from), Some(GridFrom::Gps));
        assert_eq!(gps.call.as_deref(), Some("W0CY"));
        // An old fix doesn't count; the stated grid does.
        let old = choose(Some(fix), 1000.0 + 7200.0, None, &stated);
        assert_eq!(old.grid, Some(("EM18CT".to_owned(), GridFrom::Stated)));
        // The Engine's GPS grid counts as GPS; its saved grid comes last.
        let passed_on = engine(Some("K9KSU"), Some("EM18bm"), Some("gps"));
        let chosen = choose(None, 0.0, Some(&passed_on), &stated);
        assert_eq!(chosen.grid, Some(("EM18BM".to_owned(), GridFrom::Gps)));
        assert_eq!(chosen.call.as_deref(), Some("K9KSU"));
        let saved = engine(Some("K9KSU"), Some("EN52wa"), Some("saved"));
        let chosen = choose(None, 0.0, Some(&saved), &stated);
        assert_eq!(chosen.grid, Some(("EM18CT".to_owned(), GridFrom::Stated)));
        let chosen = choose(None, 0.0, Some(&saved), &Stated::default());
        assert_eq!(chosen.grid, Some(("EN52WA".to_owned(), GridFrom::Saved)));
        // Nothing known: programs keep their own.
        assert_eq!(
            choose(None, 0.0, None, &Stated::default()),
            SendsAs::default()
        );
        // Something from the Engine that isn't a grid or a call is ignored.
        let odd = engine(Some("not a call"), Some("XX99"), Some("gps"));
        assert_eq!(
            choose(None, 0.0, Some(&odd), &Stated::default()),
            SendsAs::default()
        );
    }

    #[test]
    fn says_what_the_programs_send() {
        let sends = SendsAs {
            call: Some("W0CY".to_owned()),
            grid: Some(("EM18CT".to_owned(), GridFrom::Gps)),
        };
        assert_eq!(sends.text(), "W0CY and grid EM18CT (from FieldLab's GPS)");
    }
}
