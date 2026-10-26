// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Agent's journal of QSOs a program logged, until the Engine has them.
//!
//! A QSO counts as logged once the Engine has saved it (Principle XV). Until
//! then, the Agent keeps it here, on disk, so a QSO WSJT-X logged while the
//! Engine was unreachable, or while nobody was signed in, is sent later and
//! never lost. Each QSO gets its ID when it enters the journal, and keeps it
//! on every send, so a re-send never logs it twice.
//!
//! The journal is a file of JSON lines, only ever added to: a QSO, then a
//! line saying it was logged, or that the Engine refused it and why. A QSO
//! the Engine refused is also added to an ADIF file beside the journal, so
//! the operator can fix what was wrong and import it in LabLog's Menu.

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The longest journal line the Agent reads back; longer ones are skipped.
const MAX_LINE: usize = 256 * 1024;

/// A QSO waiting for the Engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The QSO's ID, given when it entered the journal.
    pub id: String,
    /// The QSO as the program sent it.
    pub adif: String,
    /// Which program logged it, such as "WSJT-X".
    pub program: String,
}

/// One line of the journal.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Line {
    Add(Entry),
    Logged(String),
    Refused { id: String, why: String },
}

/// The journal: its file, and the QSOs in it the Engine doesn't have yet.
pub struct Journal {
    file: File,
    refused: PathBuf,
    waiting: BTreeMap<u64, Entry>,
    next: u64,
}

impl Journal {
    /// Open the journal at `path`, making it if it isn't there, and read
    /// back the QSOs still waiting. Unreadable lines are skipped.
    ///
    /// # Errors
    ///
    /// The file can't be read or written.
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut waiting = BTreeMap::new();
        let mut next = 0;
        if path.exists() {
            let file = File::open(path)
                .map_err(|error| format!("cannot read the journal {}: {error}", path.display()))?;
            let mut order: Vec<String> = Vec::new();
            let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else { continue };
                if line.len() > MAX_LINE {
                    continue;
                }
                match serde_json::from_str::<Line>(&line) {
                    Ok(Line::Add(entry)) => {
                        order.push(entry.id.clone());
                        entries.insert(entry.id.clone(), entry);
                    }
                    Ok(Line::Logged(id) | Line::Refused { id, .. }) => {
                        entries.remove(&id);
                    }
                    Err(_) => {}
                }
            }
            for id in order {
                if let Some(entry) = entries.remove(&id) {
                    waiting.insert(next, entry);
                    next += 1;
                }
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("cannot write the journal {}: {error}", path.display()))?;
        // A line cut short (the computer lost power mid-write) is ended, so
        // the next line starts clean.
        let ends_clean =
            std::fs::read(path).map_or(true, |bytes| bytes.last().is_none_or(|b| *b == b'\n'));
        if !ends_clean {
            file.write_all(b"\n")
                .map_err(|error| format!("cannot write the journal {}: {error}", path.display()))?;
        }
        let mut refused = path.as_os_str().to_owned();
        refused.push(".refused.adi");
        Ok(Self {
            file,
            refused: PathBuf::from(refused),
            waiting,
            next,
        })
    }

    fn write(&mut self, line: &Line) -> Result<(), String> {
        let mut text = serde_json::to_string(line).map_err(|error| error.to_string())?;
        text.push('\n');
        self.file
            .write_all(text.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|error| format!("cannot write the journal: {error}"))
    }

    /// Keep a QSO a program just logged, on disk before anything else, and
    /// give it its ID.
    ///
    /// # Errors
    ///
    /// The journal can't be written.
    pub fn add(&mut self, adif: &str, program: &str) -> Result<Entry, String> {
        let entry = Entry {
            id: new_id(),
            adif: adif.to_owned(),
            program: program.to_owned(),
        };
        self.write(&Line::Add(entry.clone()))?;
        self.waiting.insert(self.next, entry.clone());
        self.next += 1;
        Ok(entry)
    }

    /// The QSOs the Engine doesn't have yet, oldest first.
    pub fn waiting(&self) -> Vec<Entry> {
        self.waiting.values().cloned().collect()
    }

    fn forget(&mut self, id: &str) {
        self.waiting.retain(|_, entry| entry.id != id);
    }

    /// The Engine has saved this QSO.
    ///
    /// # Errors
    ///
    /// The journal can't be written.
    pub fn logged(&mut self, id: &str) -> Result<(), String> {
        self.write(&Line::Logged(id.to_owned()))?;
        self.forget(id);
        Ok(())
    }

    /// The Engine refused this QSO for good. It is kept, and added to the
    /// refused-QSOs ADIF file for importing by hand. Returns that file's path.
    ///
    /// # Errors
    ///
    /// The journal or the ADIF file can't be written.
    pub fn refused(&mut self, entry: &Entry, why: &str) -> Result<PathBuf, String> {
        let record = entry
            .adif
            .split_once("<EOH>")
            .or_else(|| entry.adif.split_once("<eoh>"))
            .map_or(entry.adif.as_str(), |(_, rest)| rest)
            .trim();
        let fresh = !self.refused.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.refused)
            .map_err(|error| format!("cannot write {}: {error}", self.refused.display()))?;
        let header = if fresh {
            "QSOs the LabLog Engine refused; fix them and import this file in LabLog's Menu\n<programid:12>loglab-agent<EOH>\n"
        } else {
            ""
        };
        file.write_all(format!("{header}{record}\n").as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|error| format!("cannot write {}: {error}", self.refused.display()))?;
        self.write(&Line::Refused {
            id: entry.id.clone(),
            why: why.to_owned(),
        })?;
        self.forget(&entry.id);
        Ok(self.refused.clone())
    }
}

/// A new random ID in the UUID form the Engine expects (version 4).
fn new_id() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let count = COUNT.fetch_add(1, Ordering::Relaxed);
    // Each RandomState is seeded from the operating system's randomness.
    let half = |salt: u64| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u64(count);
        hasher.write_u32(std::process::id());
        hasher.write_u64(salt);
        hasher.finish()
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&half(1).to_be_bytes());
    bytes[8..].copy_from_slice(&half(2).to_be_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::{Journal, new_id};
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loglab-journal-{}-{name}", new_id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("journal")
    }

    const QSO: &str = "<adif_ver:5>3.1.0<EOH>\n<call:4>W1AW <mode:3>FT8 <EOR>";

    #[test]
    fn a_qso_waits_until_logged_even_across_a_restart() {
        let path = scratch("restart");
        let first = {
            let mut journal = Journal::open(&path).unwrap_or_else(|e| unreachable!("{e}"));
            let first = journal
                .add(QSO, "WSJT-X")
                .unwrap_or_else(|e| unreachable!("{e}"));
            journal
                .add(QSO, "WSJT-X")
                .unwrap_or_else(|e| unreachable!("{e}"));
            first
        };
        let mut journal = Journal::open(&path).unwrap_or_else(|e| unreachable!("{e}"));
        let waiting = journal.waiting();
        assert_eq!(waiting.len(), 2);
        assert_eq!(
            waiting[0], first,
            "the oldest comes first, with its ID kept"
        );
        journal
            .logged(&first.id)
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(journal.waiting().len(), 1);
        let reopened = Journal::open(&path).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(reopened.waiting().len(), 1);
        assert_ne!(reopened.waiting()[0].id, first.id);
    }

    #[test]
    fn a_refused_qso_is_kept_for_importing_by_hand() {
        let path = scratch("refused");
        let mut journal = Journal::open(&path).unwrap_or_else(|e| unreachable!("{e}"));
        let entry = journal
            .add(QSO, "WSJT-X")
            .unwrap_or_else(|e| unreachable!("{e}"));
        let file = journal
            .refused(&entry, "outside the operation's hours")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert!(journal.waiting().is_empty());
        let text = std::fs::read_to_string(file).unwrap_or_default();
        assert!(text.contains("<EOH>"));
        assert!(text.contains("<call:4>W1AW <mode:3>FT8 <EOR>"));
        assert!(
            !text.contains("adif_ver"),
            "WSJT-X's own header is left out"
        );
        assert!(Journal::open(&path).is_ok_and(|j| j.waiting().is_empty()));
    }

    #[test]
    fn a_damaged_line_is_skipped() {
        let path = scratch("damaged");
        {
            let mut journal = Journal::open(&path).unwrap_or_else(|e| unreachable!("{e}"));
            journal
                .add(QSO, "WSJT-X")
                .unwrap_or_else(|e| unreachable!("{e}"));
        }
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str("{\"add\": {\"id\": \"half a line");
        std::fs::write(&path, text).unwrap_or_else(|e| unreachable!("{e}"));
        assert!(Journal::open(&path).is_ok_and(|j| j.waiting().len() == 1));
    }

    #[test]
    fn ids_look_like_uuids_and_differ() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }
}
