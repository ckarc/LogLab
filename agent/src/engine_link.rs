// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Which LabLog Engine `station` links to.
//!
//! With no `--engine`, it is FieldLab's own: the Engine on the kiosk box
//! ([`FIELDLAB_ENGINE`], `deploy/kiosk`), as the station named after this
//! computer, with the key in `station.key` beside the Agent. Nothing has to
//! be typed on a FieldLab PC:
//!
//! - With no key yet, the Agent asks to join, in the background, and an
//!   administrator approves it once in LabLog (Menu > Station Agents). The
//!   radio is served all the while; once the key comes, the station starts
//!   again with the link.
//! - Where FieldLab's Engine isn't reached (off FieldLab's network, or it
//!   isn't running), setups are kept on this computer, as with no Engine,
//!   and that is said once (Principle V).
//!
//! `--engine none` links to no Engine at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::engine::Uplink;

/// FieldLab's Engine, on the kiosk box with its MQTT broker.
pub const FIELDLAB_ENGINE: &str = "http://10.73.10.10:8073";
/// The station key's file, beside the Agent.
const KEY_FILE: &str = "station.key";
/// How long FieldLab's Engine has to answer before it counts as not there.
const ASK_TIMEOUT: Duration = Duration::from_secs(3);
/// The longest station name the Engine takes.
const NAME_MAX: usize = 64;

/// The Engine `station` links to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineLink {
    /// FieldLab's, found and joined by itself (no `--engine`).
    FieldLab,
    /// None (`--engine none`).
    Off,
    /// The one given with `--engine` and `--key-file`, and the station to
    /// ask to join as when the key file has no key.
    Given {
        engine: String,
        key_file: String,
        station: Option<String>,
    },
}

/// The station name for a computer called `computer`, as the Engine takes
/// station names: lower case, letters, digits, `.`, `_` and `-`, starting
/// with a letter or digit, at most 64 characters. `FL-1` is `fl-1`.
#[must_use]
pub fn station_name(computer: Option<&str>) -> String {
    let name: String = computer
        .unwrap_or_default()
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take(NAME_MAX)
        .collect();
    if name.is_empty() {
        "station".to_owned()
    } else {
        name
    }
}

/// This computer's name, as Windows (`COMPUTERNAME`) or the shell
/// (`HOSTNAME`) has it.
fn computer_name() -> Option<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|name| !name.trim().is_empty())
}

/// The key in `path`, when it holds one.
fn saved_key(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let key = text.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

/// Whether the Engine at `base` answers its status, which needs no key.
fn answers(base: &str) -> bool {
    let http: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ASK_TIMEOUT))
        .build()
        .into();
    http.get(&format!("{base}/api/v1/status")).call().is_ok()
}

/// The link to FieldLab's Engine, made again each time a radio is found.
pub struct FieldLab {
    base: String,
    key_file: PathBuf,
    station: String,
    /// Set once joining has saved a key (see [`FieldLab::joined`]).
    joined: Arc<AtomicBool>,
    joining: bool,
    unreached_said: bool,
}

impl FieldLab {
    /// FieldLab's Engine, with the key file in `folder` (the Agent's own).
    #[must_use]
    pub fn new(folder: &Path) -> Self {
        Self::at(
            FIELDLAB_ENGINE,
            folder,
            station_name(computer_name().as_deref()),
        )
    }

    fn at(base: &str, folder: &Path, station: String) -> Self {
        Self {
            base: base.to_owned(),
            key_file: folder.join(KEY_FILE),
            station,
            joined: Arc::new(AtomicBool::new(false)),
            joining: false,
            unreached_said: false,
        }
    }

    /// The link, when FieldLab's Engine answers and this station has its
    /// key. Without a key, starts asking to join in the background (once)
    /// and gives none for now.
    pub fn uplink(&mut self, running: &Arc<AtomicBool>) -> Option<Uplink> {
        if !answers(&self.base) {
            if !self.unreached_said {
                println!(
                    "LabLog: FieldLab's Engine at {} isn't reached (off FieldLab's network, or \
                     it isn't running), so setups are kept on this computer.",
                    self.base
                );
                self.unreached_said = true;
            }
            return None;
        }
        self.unreached_said = false;
        if let Some(key) = saved_key(&self.key_file) {
            return match Uplink::new(&self.base, &key) {
                Ok(uplink) => {
                    println!(
                        "LabLog: linked to {} as station {}.",
                        self.base, self.station
                    );
                    Some(uplink)
                }
                Err(error) => {
                    println!("LabLog: {error} (in {})", self.key_file.display());
                    None
                }
            };
        }
        if !self.joining {
            self.joining = true;
            println!(
                "LabLog: asking FieldLab's Engine at {} to let this computer in as station {}.\n\
                 -> In LabLog, an administrator opens Menu > Station Agents and approves \
                 station {}. The radio works meanwhile.",
                self.base, self.station, self.station
            );
            let base = self.base.clone();
            let key_file = self.key_file.to_string_lossy().into_owned();
            let station = self.station.clone();
            let joined = Arc::clone(&self.joined);
            let running = Arc::clone(running);
            thread::spawn(move || {
                match crate::station_key(&base, &key_file, Some(&station), &running, false) {
                    Ok(_) => joined.store(true, Ordering::SeqCst),
                    Err(error) => println!("LabLog: didn't join: {error}"),
                }
            });
        }
        None
    }

    /// Set once joining has saved a key, so the station starts again with
    /// the link; whoever acts on it clears it.
    #[must_use]
    pub fn joined(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_station_after_the_computer() {
        assert_eq!(station_name(Some("FL-1")), "fl-1");
        assert_eq!(station_name(Some("DESKTOP-BRIAN")), "desktop-brian");
        assert_eq!(station_name(Some("  Field Lab #2 ")), "field-lab--2");
        assert_eq!(station_name(Some("-_x")), "x");
        assert_eq!(station_name(Some("ÅSA")), "sa");
        assert_eq!(station_name(Some("")), "station");
        assert_eq!(station_name(None), "station");
        assert_eq!(station_name(Some(&"a".repeat(80))).len(), NAME_MAX);
    }

    #[test]
    fn an_engine_not_there_gives_no_link_and_asks_nothing() {
        let folder = std::env::temp_dir().join(format!("lablog-link-{}", std::process::id()));
        // Port 9 on this computer: nothing answers there.
        let mut link = FieldLab::at("http://127.0.0.1:9", &folder, "fl-1".to_owned());
        let running = Arc::new(AtomicBool::new(true));
        assert!(link.uplink(&running).is_none());
        assert!(link.unreached_said);
        assert!(!link.joining);
        assert!(!link.joined().load(Ordering::SeqCst));
    }

    #[test]
    fn reads_the_saved_key() {
        let folder = std::env::temp_dir().join(format!("lablog-key-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap_or_else(|_| unreachable!());
        let path = folder.join(KEY_FILE);
        assert_eq!(saved_key(&path), None);
        std::fs::write(&path, "  \n").unwrap_or_else(|_| unreachable!());
        assert_eq!(saved_key(&path), None);
        std::fs::write(&path, "abc\n").unwrap_or_else(|_| unreachable!());
        assert_eq!(saved_key(&path).as_deref(), Some("abc"));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
