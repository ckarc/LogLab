// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Where the radio's setups are kept: on the Engine, for every computer, or
//! when the Agent runs with no Engine, in files on this computer
//! (Principle V). Either way a setup is kept whole and never overwritten; a
//! newer setup of the same name is a new version, and the newest is used.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::{KeptRadio, KeptReading, KeptSetting, Uplink};
use crate::settings_record::{self, Record};

/// Somewhere setups are kept.
pub trait Setups: Send {
    /// Keep the radio's settings as setup `name`.
    ///
    /// # Errors
    ///
    /// The setup couldn't be kept; says why.
    fn keep(&self, record: &Record, name: &str) -> Result<(), String>;

    /// Every setup kept for one radio, the newest of each name.
    ///
    /// # Errors
    ///
    /// The setups couldn't be fetched; says why.
    fn kept(&self, maker: &str, model: &str, serial: &str) -> Result<KeptRadio, String>;

    /// Where they are kept, to say so: `on the Engine`.
    fn place(&self) -> String;
}

impl Setups for Uplink {
    fn keep(&self, record: &Record, name: &str) -> Result<(), String> {
        self.keep_reading(record, Some(name))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn kept(&self, maker: &str, model: &str, serial: &str) -> Result<KeptRadio, String> {
        self.radio_settings(maker, model, serial)
            .map_err(|error| error.to_string())
    }

    fn place(&self) -> String {
        "on the Engine".to_owned()
    }
}

/// Setups kept in files in one folder on this computer.
pub struct Local {
    folder: PathBuf,
}

impl Local {
    /// Setups kept in `folder`.
    #[must_use]
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }
}

/// One setup as a file on this computer keeps it.
#[derive(Debug, Serialize, Deserialize)]
struct SetupFile {
    /// The file format's version.
    format: u32,
    maker: String,
    model: String,
    serial: String,
    /// The setup's name, as typed.
    name: String,
    /// The computer it was read on.
    station: String,
    /// When it was read, in UTC.
    saved_at: String,
    settings: Vec<KeptSetting>,
}

/// The file name a setup is kept under: everything but letters and digits
/// made a dash, so any setup name makes a name every computer takes.
fn path_for(folder: &Path, setup: &SetupFile) -> PathBuf {
    let name: String = format!(
        "setup-{}-{}-{}-{}-{}",
        setup.maker, setup.model, setup.serial, setup.name, setup.saved_at
    )
    .chars()
    .map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        }
    })
    .collect();
    folder.join(format!("{name}.json"))
}

impl Setups for Local {
    fn keep(&self, record: &Record, name: &str) -> Result<(), String> {
        crate::engine::check_setup_name(name)?;
        let serial = record.serial.clone().ok_or_else(|| {
            "the radio's serial number isn't known, so its setups can't be told from another \
             of its model's"
                .to_owned()
        })?;
        let setup = SetupFile {
            format: 1,
            maker: record.maker.clone(),
            model: record.model.clone(),
            serial,
            name: name.to_owned(),
            station: record.computer.clone(),
            saved_at: record.read_utc.clone(),
            settings: record
                .settings
                .iter()
                .map(|entry| KeptSetting {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    bytes: entry.bytes.clone(),
                })
                .collect(),
        };
        settings_record::save(&path_for(&self.folder, &setup), &setup)
    }

    fn kept(&self, maker: &str, model: &str, serial: &str) -> Result<KeptRadio, String> {
        let mut setups: Vec<SetupFile> = Vec::new();
        let entries = match fs::read_dir(&self.folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(KeptRadio {
                    as_received: None,
                    setups: Vec::new(),
                });
            }
            Err(error) => return Err(format!("can't look in {}: {error}", self.folder.display())),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_setup = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("setup-")
                        && Path::new(name)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                });
            if !is_setup {
                continue;
            }
            // A file that isn't a setup this Agent wrote is passed over.
            let Some(setup) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<SetupFile>(&text).ok())
            else {
                continue;
            };
            if setup.maker != maker || setup.model != model || setup.serial != serial {
                continue;
            }
            match setups.iter_mut().find(|kept| kept.name == setup.name) {
                Some(kept) if kept.saved_at < setup.saved_at => *kept = setup,
                Some(_) => {}
                None => setups.push(setup),
            }
        }
        setups.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(KeptRadio {
            as_received: None,
            setups: setups
                .into_iter()
                .map(|setup| KeptReading {
                    name: Some(setup.name),
                    settings: setup.settings,
                    station: setup.station,
                    operator: None,
                    saved_at: setup.saved_at,
                })
                .collect(),
        })
    }

    fn place(&self) -> String {
        format!("on this computer, in {}", self.folder.display())
    }
}

#[cfg(test)]
mod tests {
    use super::{Local, Setups};
    use crate::settings_record::{Entry, Record, Value};

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lablog-setups-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn record(serial: Option<&str>, at: &str, nr: &str) -> Record {
        Record {
            format: 1,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            address: 0x94,
            serial: serial.map(str::to_owned),
            read_utc: at.to_owned(),
            computer: "FL-1".to_owned(),
            settings: vec![Entry {
                id: "nr_level".to_owned(),
                label: "NR level".to_owned(),
                value: Value::Raw,
                bytes: Some(nr.to_owned()),
            }],
        }
    }

    #[test]
    fn keeps_setups_and_finds_the_newest_of_each_name() {
        let dir = folder("newest");
        let local = Local::new(dir.clone());
        let serial = Some("IC-7300_02065962");
        local
            .keep(&record(serial, "2026-10-27T22:00:00Z", "00 05"), "VARA HF")
            .unwrap_or_else(|error| unreachable!("{error}"));
        local
            .keep(&record(serial, "2026-10-27T22:05:00Z", "00 08"), "VARA HF")
            .unwrap_or_else(|error| unreachable!("{error}"));
        local
            .keep(&record(serial, "2026-10-27T22:01:00Z", "00 01"), "FT8")
            .unwrap_or_else(|error| unreachable!("{error}"));
        // Another radio's setup isn't this one's.
        local
            .keep(
                &record(Some("other"), "2026-10-27T22:09:00Z", "00 09"),
                "FT8",
            )
            .unwrap_or_else(|error| unreachable!("{error}"));
        std::fs::write(dir.join("setup-junk.json"), "not json")
            .unwrap_or_else(|error| unreachable!("{error}"));

        let kept = local
            .kept("Icom", "IC-7300", "IC-7300_02065962")
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(kept.as_received, None);
        let found: Vec<(Option<&str>, &str, Option<&str>)> = kept
            .setups
            .iter()
            .map(|s| {
                (
                    s.name.as_deref(),
                    s.saved_at.as_str(),
                    s.settings[0].bytes.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (Some("FT8"), "2026-10-27T22:01:00Z", Some("00 01")),
                (Some("VARA HF"), "2026-10-27T22:05:00Z", Some("00 08")),
            ]
        );
        assert_eq!(kept.setups[0].station, "FL-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_overwrites_and_needs_a_serial_and_a_good_name() {
        let dir = folder("refuse");
        let local = Local::new(dir.clone());
        let serial = Some("IC-7300_02065962");
        let first = record(serial, "2026-10-27T22:00:00Z", "00 05");
        local
            .keep(&first, "FT8")
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(local.keep(&first, "FT8").is_err());
        assert!(
            local
                .keep(&record(None, "2026-10-27T22:00:00Z", "00 05"), "FT8")
                .is_err()
        );
        assert!(local.keep(&first, "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_folder_yet_means_no_setups() {
        let local = Local::new(folder("missing"));
        let kept = local
            .kept("Icom", "IC-7300", "x")
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(kept.setups.is_empty());
        assert!(local.place().starts_with("on this computer"));
    }
}
