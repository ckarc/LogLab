// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! What `station` carries between this computer's programs and the LabLog
//! Engine, on its own thread, while the station runs with `--engine`.
//!
//! To the Engine:
//!
//! - **QSOs** the programs log (WSJT-X, `JS8Call`, fldigi, `VarAC`), found in
//!   their own ADIF logs (see `qso_logs`). Each goes into the Agent's
//!   journal on disk first, and stays there until the Engine has logged it,
//!   as whoever is signed in to LabLog at this station (Principle XV). With
//!   the Engine unreachable or nobody signed in, QSOs wait and are sent
//!   later; one the Engine refuses for good is kept in an ADIF file for
//!   importing by hand.
//! - **The radio's live state**, for LabLog's entry screen: what the radio
//!   last reported, when it changes and every few seconds otherwise. Truth
//!   flows radio to Engine only (Principle X).
//!
//! From the Engine:
//!
//! - **Who programs here log as**: the operator signed in at this station,
//!   the call QSOs are made as (the operation's, when one is chosen), and
//!   the station's grid. The Agent says when it changes, and keeps the latest
//!   in `lablog-station.json` in its records folder for setting programs up.
//!   Nothing from the Engine goes to the radio.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::engine::{Handed, RadioReport, Report, SendError, StationInfo, Uplink};
use crate::journal::Journal;
use crate::qso_logs::{self, Log, Watch};
use crate::rig::Reading;
use crate::rigctld_server::Source;

/// How often the programs' logs are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// How often the Agent looks again for where programs keep their logs.
const FIND_EVERY: Duration = Duration::from_secs(30);
/// How long QSOs wait before the Engine is tried again.
const RETRY: Duration = Duration::from_secs(15);
/// How often the radio's state is read for the Engine; it is sent only when
/// it changed (or every [`RIG_HEARTBEAT`]), so LabLog follows the VFO knob
/// within about a quarter second without a steady stream.
const RIG_EVERY: Duration = Duration::from_millis(250);
/// How long an unchanged radio state goes unsent.
const RIG_HEARTBEAT: Duration = Duration::from_secs(10);
/// How often the Agent asks who programs here log as.
const STATION_EVERY: Duration = Duration::from_secs(30);
/// The journal of QSOs, in the records folder.
pub const JOURNAL: &str = "qsos.journal";
/// The latest station information, in the records folder.
pub const STATION_FILE: &str = "lablog-station.json";

/// Who programs at this station log as, as the Engine last said, from
/// `lablog-station.json` in the records folder `folder`: `None` before the
/// Agent has heard from the Engine. Use `station_call` as the call a
/// program sends on the air, and `grid` as its grid; leave a program's own
/// setting alone where either is `None`.
#[must_use]
pub fn last_station(folder: &Path) -> Option<StationInfo> {
    let bytes = fs::read(folder.join(STATION_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The running carrier; dropping it stops it.
pub struct Carrier {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Carrier {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts carrying for the radio `radio`, keeping the journal and what it
/// has seen in `folder`.
///
/// # Errors
///
/// The journal can't be opened.
pub fn start(uplink: Uplink, radio: Arc<dyn Source>, folder: &Path) -> Result<Carrier, String> {
    let _ = fs::create_dir_all(folder);
    let journal = Journal::open(&folder.join(JOURNAL))?;
    let waiting = journal.waiting().len();
    if waiting > 0 {
        crate::print_event(&format!(
            "QSOs: {waiting} wait in the journal for LabLog; sending them now."
        ));
    }
    if let Some(info) = last_station(folder) {
        crate::print_event(&format!(
            "LabLog, when last heard from: {}",
            station_text(&info)
        ));
    }
    let mut work = Work {
        uplink,
        radio,
        journal,
        watch: Watch::open(&folder.join(qso_logs::SEEN_FILE)),
        folder: folder.to_owned(),
        logs: Vec::new(),
        watched_said: Vec::new(),
        log_problem_said: Vec::new(),
        found_at: None,
        looked_at: None,
        retry_at: Instant::now(),
        waiting_said: None,
        rig_at: None,
        rig_sent: None,
        rig_sent_at: Instant::now(),
        rig_said: None,
        station_at: None,
        station: None,
        station_said: None,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            work.step();
            thread::sleep(Duration::from_millis(250));
        }
    });
    Ok(Carrier {
        stop,
        thread: Some(thread),
    })
}

/// The carrier's state.
struct Work {
    uplink: Uplink,
    radio: Arc<dyn Source>,
    journal: Journal,
    watch: Watch,
    folder: PathBuf,
    logs: Vec<Log>,
    watched_said: Vec<PathBuf>,
    log_problem_said: Vec<(PathBuf, String)>,
    found_at: Option<Instant>,
    looked_at: Option<Instant>,
    retry_at: Instant,
    waiting_said: Option<String>,
    rig_at: Option<Instant>,
    rig_sent: Option<Result<Reading, String>>,
    rig_sent_at: Instant,
    rig_said: Option<String>,
    station_at: Option<Instant>,
    station: Option<StationInfo>,
    station_said: Option<String>,
}

/// Whether `at` is unset or `every` ago.
fn due(at: Option<Instant>, every: Duration) -> bool {
    at.is_none_or(|at| at.elapsed() >= every)
}

impl Work {
    fn step(&mut self) {
        if due(self.station_at, STATION_EVERY) {
            self.station_at = Some(Instant::now());
            self.ask_station();
        }
        if due(self.found_at, FIND_EVERY) {
            self.found_at = Some(Instant::now());
            self.logs = qso_logs::find(&self.folder.join(qso_logs::EXTRA_LOGS));
            for log in &self.logs {
                if log.path.is_file() && !self.watched_said.contains(&log.path) {
                    crate::print_event(&format!(
                        "QSOs: {} logs to {}; the Agent sends each new QSO to LabLog.",
                        log.program,
                        log.path.display()
                    ));
                    self.watched_said.push(log.path.clone());
                }
            }
        }
        if due(self.looked_at, LOOK_EVERY) {
            self.looked_at = Some(Instant::now());
            self.look();
        }
        if Instant::now() >= self.retry_at && !self.journal.waiting().is_empty() {
            self.send_qsos();
        }
        if due(self.rig_at, RIG_EVERY) {
            self.rig_at = Some(Instant::now());
            self.send_rig();
        }
    }

    /// Journals each new QSO in the programs' logs.
    fn look(&mut self) {
        for log in self.logs.clone() {
            let found = match self.watch.new_records(&log.path) {
                Ok(found) => found,
                Err(why) => {
                    let said = (log.path.clone(), why.clone());
                    if !self.log_problem_said.contains(&said) {
                        crate::print_event(&format!("QSOs: {}: {why}", log.program));
                        self.log_problem_said.push(said);
                    }
                    continue;
                }
            };
            for record in found {
                match self.journal.add(&record, &log.program) {
                    Ok(_) => {
                        if let Err(why) = self.watch.mark(&log.path, &record) {
                            crate::print_event(&format!("QSOs: {why}"));
                        }
                        crate::print_event(&format!(
                            "QSOs: {} logged {}; sending it to LabLog.",
                            log.program,
                            adif_field(&record, "call").unwrap_or_else(|| "a QSO".to_owned())
                        ));
                        self.retry_at = Instant::now();
                    }
                    Err(why) => {
                        // Not marked, so it is found again next time.
                        crate::print_event(&format!(
                            "QSOs: {why}\n-> The QSO stays in {}'s own log; the Agent tries again.",
                            log.program
                        ));
                        return;
                    }
                }
            }
        }
    }

    /// Hands the Engine every QSO waiting in the journal, oldest first,
    /// until it says "not yet".
    fn send_qsos(&mut self) {
        for entry in self.journal.waiting() {
            let outcome = match self.uplink.log_qso(&entry) {
                Handed::Logged(message) => {
                    crate::print_event(&format!("LabLog: {message}"));
                    self.waiting_said = None;
                    self.journal.logged(&entry.id)
                }
                Handed::Refused(why) => match self.journal.refused(&entry, &why) {
                    Ok(file) => {
                        crate::print_event(&format!(
                            "LabLog refused a QSO from {}: {why}\n-> It is kept in {}; fix it \
                             there and import that file in LabLog's Menu.",
                            entry.program,
                            file.display()
                        ));
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                Handed::NotYet(why) => {
                    self.say_waiting(&why);
                    self.retry_at = Instant::now() + RETRY;
                    return;
                }
                Handed::KeyRefused(code) => {
                    self.say_waiting(&SendError::Refused(code).to_string());
                    self.retry_at = Instant::now() + RETRY * 4;
                    return;
                }
            };
            if let Err(why) = outcome {
                crate::print_event(&format!("QSOs: {why}"));
                self.retry_at = Instant::now() + RETRY;
                return;
            }
        }
    }

    fn say_waiting(&mut self, why: &str) {
        if self.waiting_said.as_deref() != Some(why) {
            let count = self.journal.waiting().len();
            crate::print_event(&format!(
                "QSOs: {count} wait in the Agent's journal, safe on disk: {why}\n-> They are \
                 sent as soon as LabLog takes them (sign in to LabLog at this station)."
            ));
            self.waiting_said = Some(why.to_owned());
        }
    }

    /// Sends the radio's state when it changed, or every few seconds.
    fn send_rig(&mut self) {
        let now = self.radio.reading();
        if self.rig_sent.as_ref() == Some(&now) && self.rig_sent_at.elapsed() < RIG_HEARTBEAT {
            return;
        }
        let identity = self.radio.identity();
        let agent = crate::banner();
        let report = match &now {
            Ok(reading) => Report {
                agent: &agent,
                radio: Some(RadioReport {
                    identity: &identity,
                    reading,
                }),
                problem: None,
            },
            Err(problem) => Report {
                agent: &agent,
                radio: None,
                problem: Some(problem),
            },
        };
        match self.uplink.send(&report) {
            Ok(()) => {
                if self.rig_said.take().is_some() {
                    crate::print_event("LabLog: answering again; it shows this radio.");
                }
                self.rig_sent = Some(now);
                self.rig_sent_at = Instant::now();
            }
            Err(error) => {
                let said = error.to_string();
                if self.rig_said.as_deref() != Some(said.as_str()) {
                    crate::print_event(&format!("LabLog: {said}"));
                    self.rig_said = Some(said);
                }
                self.rig_sent = None;
            }
        }
    }

    /// Asks who programs here log as; says so and keeps it when it changes.
    fn ask_station(&mut self) {
        let info = match self.uplink.station() {
            Ok(info) => info,
            Err(error) => {
                let said = error.to_string();
                if self.station_said.as_deref() != Some(said.as_str()) {
                    crate::print_event(&format!("LabLog: {said}"));
                    self.station_said = Some(said);
                }
                return;
            }
        };
        if self.station.as_ref() == Some(&info) {
            return;
        }
        let said = station_text(&info);
        crate::print_event(&format!("LabLog: {said}"));
        self.station_said = Some(said);
        let path = self.folder.join(STATION_FILE);
        if let Err(why) = serde_json::to_vec_pretty(&info)
            .map_err(|e| e.to_string())
            .and_then(|bytes| fs::write(&path, bytes).map_err(|e| e.to_string()))
        {
            crate::print_event(&format!("LabLog: can't keep {}: {why}", path.display()));
        }
        self.station = Some(info);
    }
}

/// What the Engine said about this station, for the operator.
fn station_text(info: &StationInfo) -> String {
    let grid = info.grid.as_deref().map_or_else(String::new, |grid| {
        let from = match info.grid_source.as_deref() {
            Some("gps") => " (from GPS)",
            _ => "",
        };
        format!(", grid {grid}{from}")
    });
    match (&info.operator, &info.station_call) {
        (Some(operator), Some(call)) => {
            let operation = info
                .operation
                .as_deref()
                .map(|name| format!(" in {name}"))
                .unwrap_or_default();
            format!(
                "{operator} is signed in at {}; QSOs here are logged as {call}{operation}{grid}.",
                info.station
            )
        }
        _ => format!(
            "nobody is signed in at {}{grid}.\n-> Sign in to LabLog at this station; QSOs \
             programs log wait in the Agent until then.",
            info.station
        ),
    }
}

/// The value of an ADIF field in one record, by name, any case.
fn adif_field(record: &str, name: &str) -> Option<String> {
    let lower = record.to_ascii_lowercase();
    let tag = format!("<{name}:");
    let at = lower.find(&tag)? + tag.len();
    let close = at + lower[at..].find('>')?;
    let length: usize = lower[at..close].split(':').next()?.trim().parse().ok()?;
    let value = record.get(close + 1..close + 1 + length)?;
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{STATION_FILE, StationInfo, adif_field, last_station, station_text};

    #[test]
    fn reads_a_field_of_a_record() {
        let record = "<CALL:4>W1AW <gridsquare:4>FN31 <mode:3:S>FT8 <eor>";
        assert_eq!(adif_field(record, "call").as_deref(), Some("W1AW"));
        assert_eq!(adif_field(record, "mode").as_deref(), Some("FT8"));
        assert_eq!(adif_field(record, "name"), None);
        assert_eq!(adif_field("<call:40>W1AW <eor>", "call"), None);
    }

    fn info(operator: Option<&str>) -> StationInfo {
        StationInfo {
            station: "fieldlab-1".to_owned(),
            operator: operator.map(str::to_owned),
            station_call: operator.map(|_| "W0CK".to_owned()),
            operation: operator.map(|_| "Field Day".to_owned()),
            grid: Some("EM18bm".to_owned()),
            grid_source: Some("gps".to_owned()),
        }
    }

    #[test]
    fn keeps_who_programs_log_as_for_setting_them_up() {
        let dir = std::env::temp_dir().join(format!("loglab-carrier-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join(STATION_FILE));
        assert_eq!(last_station(&dir), None);
        let kept = info(Some("K9KSU"));
        std::fs::write(
            dir.join(STATION_FILE),
            serde_json::to_vec_pretty(&kept).unwrap_or_default(),
        )
        .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(last_station(&dir), Some(kept));
    }

    #[test]
    fn says_who_programs_log_as() {
        assert_eq!(
            station_text(&info(Some("K9KSU"))),
            "K9KSU is signed in at fieldlab-1; QSOs here are logged as W0CK in Field Day, \
             grid EM18bm (from GPS)."
        );
        assert!(station_text(&info(None)).starts_with("nobody is signed in at fieldlab-1"));
    }
}
