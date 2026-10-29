// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Noticing the QSOs programs on this computer log, from their own ADIF logs.
//!
//! Each program the Agent sets up keeps its own log as an ADIF file (the open
//! format of Principle IV) and adds each QSO to it the moment the operator
//! logs it. The Agent reads those files; it never writes them. Where each
//! program keeps its log, from its own source or documentation:
//!
//! - **WSJT-X**: `wsjtx_log.adi` in its data folder,
//!   `%LOCALAPPDATA%\WSJT-X` (`Configuration.cpp`, `writeable_data_dir_` is
//!   Qt's `DataLocation`). `logbook/WorkedBefore.cpp` adds each QSO to its
//!   end, after a header ending `<eoh>`.
//! - **`JS8Call`**: `js8call_log.adi` in its data folder,
//!   `%LOCALAPPDATA%\JS8Call` (`logqso.cpp`, Qt's `AppLocalDataLocation`).
//!   `logbook/adif.cpp` `addQSOToFile` adds each QSO to its end.
//!   A rig name given with `-r` adds " - NAME" to either folder's name.
//! - **fldigi**: the file its settings name as `LOGBOOKFILENAME`, else
//!   `logbook.adi` in `fldigi.files\logs` on Windows (`logbook.cxx`
//!   `start_logbook`; `logsupport.h` gives Windows the `.adi` suffix).
//! - **`VarAC`**: `VarAC_qso_log.adi` in `VarAC`'s folder unless set
//!   otherwise in its Settings > Logging (`VarAC`'s forum, "Location of
//!   ADIF files"). `VarAC` is not open source and its install folder isn't
//!   documented, so the usual places are looked in; a log elsewhere can be
//!   named in `qso-logs.txt` (below).
//!
//! Any other log is watched when its path is a line of `qso-logs.txt` in the
//! Agent's records folder.
//!
//! The Agent remembers each record it has seen in each file, by a
//! fingerprint, in `qso-logs.json` beside it. A record it hasn't seen is a
//! new QSO. The first time the Agent looks at a file, everything already in
//! it counts as seen, so a log's history is never sent; a file that wasn't
//! there yet when first looked for is watched from empty, so its first QSO
//! is sent. A QSO logged while the Agent wasn't running is found when it
//! starts. A program that rewrites its whole log (fldigi does, after an
//! edit) sends nothing new unless a record changed.
//!
//! The file is untrusted: it is read only up to a size limit, only whole
//! records (ending `<EOR>`) count, and the Engine checks each (Principle XII).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The biggest log the Agent reads.
const MAX_LOG: u64 = 64 * 1024 * 1024;
/// The longest one record may be; a longer one is skipped.
const MAX_RECORD: usize = 64 * 1024;
/// The extra logs file, in the records folder.
pub const EXTRA_LOGS: &str = "qso-logs.txt";
/// What the Agent has seen, in the records folder.
pub const SEEN_FILE: &str = "qso-logs.json";

/// A program's ADIF log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    /// The program, as the Engine is told it logged the QSO.
    pub program: String,
    /// The file.
    pub path: PathBuf,
}

/// Every program log to watch on this computer: each program's own, found
/// where it keeps it (whether or not the file exists yet), then those named
/// in `extra` (`qso-logs.txt`).
#[must_use]
pub fn find(extra: &Path) -> Vec<Log> {
    let mut logs = program_logs();
    if let Ok(text) = fs::read_to_string(extra) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let path = PathBuf::from(line);
            if logs.iter().all(|log| log.path != path) {
                logs.push(Log {
                    program: program_of(&path),
                    path,
                });
            }
        }
    }
    logs
}

/// The program a log named in `qso-logs.txt` is from, by its file's name.
fn program_of(path: &Path) -> String {
    let name = path
        .to_string_lossy()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let known = [
        ("varac", "VarAC"),
        ("wsjtx", "WSJT-X"),
        ("js8call", "JS8Call"),
        ("logbook", "fldigi"),
    ];
    known
        .iter()
        .find(|(part, _)| name.contains(part))
        .map_or_else(|| name.clone(), |(_, program)| (*program).to_owned())
}

/// The logs of the programs the Agent sets up, on Windows.
#[cfg(windows)]
fn program_logs() -> Vec<Log> {
    let mut logs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for (program, folder, file) in [
            ("WSJT-X", "WSJT-X", "wsjtx_log.adi"),
            ("JS8Call", "JS8Call", "js8call_log.adi"),
        ] {
            logs.push(Log {
                program: program.to_owned(),
                path: local.join(folder).join(file),
            });
            // A rig name (-r NAME) gives the program its own folder.
            if let Ok(entries) = fs::read_dir(&local) {
                for entry in entries.filter_map(Result::ok) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(&format!("{folder} - ")) {
                        logs.push(Log {
                            program: program.to_owned(),
                            path: entry.path().join(file),
                        });
                    }
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        let named = crate::fldigi::find()
            .and_then(|settings| fs::read_to_string(settings).ok())
            .and_then(|text| crate::fldigi::value(&text, "LOGBOOKFILENAME"))
            .filter(|name| !name.trim().is_empty())
            .map(PathBuf::from);
        logs.push(Log {
            program: "fldigi".to_owned(),
            path: named
                .unwrap_or_else(|| home.join("fldigi.files").join("logs").join("logbook.adi")),
        });
    }
    // VarAC's install folder isn't documented; these are where it and VARA
    // are usually put. Only one that has the log is watched.
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let program_files_x86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
    let places = [
        Some(PathBuf::from(r"C:\VarAC")),
        program_files.map(|p| p.join("VarAC")),
        program_files_x86.map(|p| p.join("VarAC")),
        Some(PathBuf::from(r"C:\VARA")),
    ];
    if let Some(path) = places
        .into_iter()
        .flatten()
        .map(|folder| folder.join("VarAC_qso_log.adi"))
        .find(|path| path.is_file())
    {
        logs.push(Log {
            program: "VarAC".to_owned(),
            path,
        });
    }
    logs
}

/// Only Windows' programs are looked for; elsewhere, only `qso-logs.txt`.
#[cfg(not(windows))]
fn program_logs() -> Vec<Log> {
    Vec::new()
}

/// The whole records in an ADIF file's text, each trimmed and ending in
/// `<EOR>`, after its header if it has one. A record still being written
/// (no `<EOR>` yet) is left for next time.
#[must_use]
pub fn records(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    // ADIF: a file whose first character isn't '<' has a header, ended by <EOH>.
    let start = if text.trim_start().starts_with('<') {
        0
    } else {
        match lower.find("<eoh>") {
            Some(at) => at + "<eoh>".len(),
            None => return Vec::new(),
        }
    };
    let mut out = Vec::new();
    let mut from = start;
    while let Some(at) = lower[from..].find("<eor>") {
        let end = from + at + "<eor>".len();
        let record = text[from..end].trim();
        if record.len() > "<eor>".len() && record.len() <= MAX_RECORD && record.contains(':') {
            out.push(record.to_owned());
        }
        from = end;
    }
    out
}

/// A record's fingerprint: FNV-1a, 64-bit, of its bytes with line endings
/// and runs of spaces made one space, so the same record written again with
/// other line endings is the same record.
#[must_use]
pub fn fingerprint(record: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut last_space = false;
    for byte in record.bytes() {
        let byte = if byte.is_ascii_whitespace() {
            b' '
        } else {
            byte
        };
        if byte == b' ' && last_space {
            continue;
        }
        last_space = byte == b' ';
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What the Agent has seen, as kept on disk.
#[derive(Default, Serialize, Deserialize)]
struct Seen {
    files: BTreeMap<String, BTreeSet<String>>,
}

/// The logs being watched, and what has been seen in each.
pub struct Watch {
    seen_path: PathBuf,
    seen: Seen,
    /// Each file's size and modification time when last read.
    looked: BTreeMap<PathBuf, (u64, Option<SystemTime>)>,
}

impl Watch {
    /// The watch that keeps what it has seen at `seen_path`. A missing or
    /// unreadable file starts it from nothing.
    #[must_use]
    pub fn open(seen_path: &Path) -> Self {
        let seen = fs::read(seen_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            seen_path: seen_path.to_owned(),
            seen,
            looked: BTreeMap::new(),
        }
    }

    /// The records in `path` not seen before, oldest first. Unchanged since
    /// the last look, it isn't read again. The first look at a file counts
    /// everything in it as seen and returns nothing.
    ///
    /// # Errors
    ///
    /// The file is too big, can't be read, or what was seen can't be saved.
    pub fn new_records(&mut self, path: &Path) -> Result<Vec<String>, String> {
        let key = path.to_string_lossy().into_owned();
        let Ok(meta) = fs::metadata(path) else {
            // Not there (yet): from now on, its first QSO is new.
            if let std::collections::btree_map::Entry::Vacant(entry) = self.seen.files.entry(key) {
                entry.insert(BTreeSet::new());
                self.save()?;
            }
            return Ok(Vec::new());
        };
        let stamp = (meta.len(), meta.modified().ok());
        if self.looked.get(path) == Some(&stamp) {
            return Ok(Vec::new());
        }
        if meta.len() > MAX_LOG {
            return Err(format!(
                "{} is over {} MB, so the Agent doesn't read it",
                path.display(),
                MAX_LOG / 1024 / 1024
            ));
        }
        let bytes = fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        self.looked.insert(path.to_owned(), stamp);
        let found = records(&String::from_utf8_lossy(&bytes));
        if let Some(seen) = self.seen.files.get(&key) {
            let mut fresh = Vec::new();
            let mut taken = BTreeSet::new();
            for record in found {
                let print = fingerprint(&record);
                if !seen.contains(&print) && taken.insert(print) {
                    fresh.push(record);
                }
            }
            return Ok(fresh);
        }
        let all = found.iter().map(|record| fingerprint(record)).collect();
        self.seen.files.insert(key, all);
        self.save()?;
        Ok(Vec::new())
    }

    /// Remember that `record` from `path` is in the journal.
    ///
    /// # Errors
    ///
    /// What was seen can't be saved.
    pub fn mark(&mut self, path: &Path, record: &str) -> Result<(), String> {
        self.seen
            .files
            .entry(path.to_string_lossy().into_owned())
            .or_default()
            .insert(fingerprint(record));
        self.save()
    }

    /// Writes what was seen: whole, to a new file, then renamed over the old.
    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_vec(&self.seen).map_err(|e| e.to_string())?;
        if let Some(folder) = self.seen_path.parent() {
            let _ = fs::create_dir_all(folder);
        }
        let mut fresh = self.seen_path.as_os_str().to_owned();
        fresh.push(".new");
        let fresh = PathBuf::from(fresh);
        fs::write(&fresh, text)
            .and_then(|()| fs::rename(&fresh, &self.seen_path))
            .map_err(|e| format!("can't save {}: {e}", self.seen_path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Watch, find, fingerprint, records};
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loglab-qso-logs-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// The start of a WSJT-X log, as `WorkedBefore.cpp` writes a new one.
    const HEADER: &str = "ADIF Export\n<adif_ver:5>3.1.1\n<created_timestamp:15>20261029 185900\n<programid:6>WSJT-X\n<programversion:5>2.7.0\n<eoh>\n";
    const ONE: &str = "<call:4>W1AW <gridsquare:4>FN31 <mode:3>FT8 <qso_date:8>20261029 <time_on:6>190000 <band:3>20m <freq:9>14.075500 <eor>";
    const TWO: &str =
        "<call:5>K0ABC <mode:3>JS8 <qso_date:8>20261029 <time_on:6>191500 <band:3>40m <EOR>";

    #[test]
    fn reads_whole_records_after_the_header() {
        let text = format!("{HEADER}{ONE}\n{TWO}\r\n<call:4>N0CA <mode");
        assert_eq!(records(&text), vec![ONE.to_owned(), TWO.to_owned()]);
        assert_eq!(records(&format!("{ONE}\n")), vec![ONE.to_owned()]);
        assert!(records("a header with no end <call:4>W1AW <eor>").is_empty());
        assert!(records("<eoh> <eor> <EOR>").is_empty());
    }

    #[test]
    fn a_record_is_the_same_however_its_lines_end() {
        assert_eq!(
            fingerprint("<call:4>W1AW <mode:3>FT8 <eor>"),
            fingerprint("<call:4>W1AW\r\n<mode:3>FT8\n<eor>")
        );
        assert_ne!(
            fingerprint("<call:4>W1AW <eor>"),
            fingerprint("<call:4>W1AX <eor>")
        );
    }

    #[test]
    fn a_log_already_there_is_history_and_only_new_qsos_are_sent() {
        let dir = scratch("history");
        let log = dir.join("wsjtx_log.adi");
        let seen = dir.join("qso-logs.json");
        std::fs::write(&log, format!("{HEADER}{ONE}\n")).unwrap_or_else(|e| unreachable!("{e}"));
        let mut watch = Watch::open(&seen);
        assert_eq!(
            watch.new_records(&log),
            Ok(Vec::new()),
            "history isn't sent"
        );
        std::fs::write(&log, format!("{HEADER}{ONE}\n{TWO}\n"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let fresh = watch.new_records(&log).unwrap_or_default();
        assert_eq!(fresh, vec![TWO.to_owned()]);
        // Not marked yet (say the journal couldn't be written): found again
        // after a restart.
        let mut again = Watch::open(&seen);
        assert_eq!(again.new_records(&log).unwrap_or_default(), fresh);
        again
            .mark(&log, &fresh[0])
            .unwrap_or_else(|e| unreachable!("{e}"));
        let mut restarted = Watch::open(&seen);
        assert_eq!(restarted.new_records(&log), Ok(Vec::new()));
    }

    #[test]
    fn a_log_that_appears_later_is_watched_from_empty() {
        let dir = scratch("later");
        let log = dir.join("js8call_log.adi");
        let mut watch = Watch::open(&dir.join("qso-logs.json"));
        assert_eq!(watch.new_records(&log), Ok(Vec::new()));
        std::fs::write(&log, format!("{TWO}\n")).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(watch.new_records(&log), Ok(vec![TWO.to_owned()]));
    }

    #[test]
    fn a_rewritten_log_sends_nothing_unchanged() {
        let dir = scratch("rewritten");
        let log = dir.join("logbook.adi");
        let mut watch = Watch::open(&dir.join("qso-logs.json"));
        std::fs::write(&log, format!("{ONE}\n{TWO}\n")).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(watch.new_records(&log), Ok(Vec::new()));
        // fldigi writes the whole log again, with other line endings.
        std::fs::write(&log, format!("{TWO}\r\n\r\n{ONE}\r\n"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(watch.new_records(&log), Ok(Vec::new()));
    }

    #[test]
    fn extra_logs_are_named_one_to_a_line() {
        let dir = scratch("extra");
        let extra = dir.join("qso-logs.txt");
        std::fs::write(
            &extra,
            "# logs the Agent should also watch\n\nD:\\Ham\\VarAC_qso_log.adi\n",
        )
        .unwrap_or_else(|e| unreachable!("{e}"));
        let logs = find(&extra);
        let named = logs
            .iter()
            .find(|log| log.path.as_os_str() == "D:\\Ham\\VarAC_qso_log.adi");
        assert_eq!(
            named.map(|log| log.program.as_str()),
            Some("VarAC"),
            "{logs:?}"
        );
    }
}
