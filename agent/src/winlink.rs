// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Setting Winlink Express's VARA radio setup so the Agent keys the radio
//! for it: no radio model, no control port, PTT "External".
//!
//! Winlink Express keys a radio only through its own drivers on a COM
//! port, which the Agent holds. With no control port and PTT "External",
//! it leaves the radio alone, and the Agent keys it when VARA asks (see
//! `vara_proxy`).
//!
//! Where these live was read from Winlink Express 1.8.6.0 on a real
//! computer (2026-10-29), as Winlink Express is closed source: its settings
//! file is `RMS Express.ini` in `C:\RMS Express` (nothing in the
//! registry), and its VARA Radio Setup is the `[Vara]` section, written
//! the first time Radio Setup is saved, with `Model=Manual`,
//! `Control Port=NONE` and `PTT Port=EXTERNAL` as a fresh install's own
//! choices. A file with no `[Vara]` section has never had Radio Setup
//! saved, so it already has those choices and is left alone.
//!
//! The maintainer decided (2026-10-29) that the Agent sets up each
//! program's settings file at start to work with the Agent. Only these
//! three lines are changed, only while Winlink Express is closed (it writes
//! the file as it closes), the file as first found is kept beside it, and
//! the new file is written whole (see `settings_file`).

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::ini;
use crate::settings_file::{self, Change, Outcome};

/// The section holding Winlink Express's VARA radio setup.
const SECTION: &str = "Vara";
/// The lines the Agent sets, and what to: no radio driver, no control
/// port, and PTT left to something outside Winlink Express.
const WANTED: [(&str, &str); 3] = [
    ("Model", "Manual"),
    ("Control Port", "NONE"),
    ("PTT Port", "EXTERNAL"),
];

/// Winlink Express's executable, as Windows lists it running.
#[cfg(windows)]
pub const IMAGE: &str = "RMS Express.exe";

/// Where Winlink Express keeps its settings file.
#[cfg(windows)]
const FOLDER: &str = r"C:\RMS Express";

/// `text` with Winlink Express's VARA radio setup left to the Agent, and
/// what changed. With `kenwood`, Winlink Express tunes the radio through
/// the Agent's virtual Kenwood (see `kenwood_cat`), so the radio model and
/// its port are left as they are, and only PTT stays the Agent's.
#[must_use]
pub fn with_agent_keying(text: &str, kenwood: bool) -> (String, Vec<Change>) {
    let has_section = text
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(&format!("[{SECTION}]")));
    if !has_section {
        return (text.to_owned(), Vec::new());
    }
    let changes: Vec<Change> = WANTED
        .iter()
        .filter(|(key, _)| !kenwood || *key == "PTT Port")
        .filter_map(|(key, value)| {
            let before = ini::value(text, SECTION, key);
            (before.as_deref().map(str::to_ascii_uppercase) != Some(value.to_ascii_uppercase()))
                .then(|| Change {
                    key: (*key).to_owned(),
                    before,
                    after: (*value).to_owned(),
                })
        })
        .collect();
    if changes.is_empty() {
        return (text.to_owned(), changes);
    }
    let wanted: Vec<(&str, String)> = changes
        .iter()
        .map(|change| {
            let key = WANTED
                .iter()
                .find(|(key, _)| *key == change.key)
                .map_or("", |(key, _)| *key);
            (key, change.after.clone())
        })
        .collect();
    (ini::with_keys(text, SECTION, &wanted), changes)
}

/// Leaves keying to the Agent in Winlink Express's settings file at
/// `path`, and tuning too unless `kenwood`. Winlink Express must not be
/// running.
///
/// # Errors
///
/// The file can't be read or written.
pub fn set(path: &Path, kenwood: bool) -> Result<Outcome, String> {
    settings_file::update(path, |text| Ok(with_agent_keying(text, kenwood)))
}

/// Where Winlink Express's settings file is, if it is installed and has
/// been started.
#[cfg(windows)]
#[must_use]
pub fn find() -> Option<PathBuf> {
    Some(Path::new(FOLDER).join("RMS Express.ini")).filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::{set, with_agent_keying};
    use crate::settings_file::Outcome;

    /// The `[Vara]` section as Winlink Express 1.8.6.0 wrote it, with the
    /// radio's COM port chosen instead of the Agent.
    const INI: &str = "[W0CY]\r\nMode=Vara HF Winlink\r\n\r\n[Vara]\r\nPTT Baud=9600\r\nControl Baud=19200\r\nControl Port=COM5\r\nModel=Icom IC-7300\r\nPTT Port=COM5\r\nPTT DTR=True\r\nInternal soundcard=True\r\n\r\n[Radio]\r\nEnable ALE=False\r\n";

    #[test]
    fn leaves_the_radio_to_the_agent_and_nothing_else() {
        let (text, changes) = with_agent_keying(INI, false);
        assert_eq!(
            text,
            INI.replace("Control Port=COM5", "Control Port=NONE")
                .replace("Model=Icom IC-7300", "Model=Manual")
                .replace("PTT Port=COM5", "PTT Port=EXTERNAL")
        );
        let keys: Vec<&str> = changes.iter().map(|change| change.key.as_str()).collect();
        assert_eq!(keys, ["Model", "Control Port", "PTT Port"]);
        assert_eq!(changes[0].before.as_deref(), Some("Icom IC-7300"));
        // Already so: nothing changes, whatever the case Winlink wrote.
        assert!(with_agent_keying(&text, false).1.is_empty());
        assert!(
            with_agent_keying(&text.replace("=NONE", "=None"), false)
                .1
                .is_empty()
        );
    }

    #[test]
    fn tuning_through_the_virtual_kenwood_keeps_the_radio_choice() {
        let (text, changes) = with_agent_keying(INI, true);
        assert_eq!(text, INI.replace("PTT Port=COM5", "PTT Port=EXTERNAL"));
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn a_file_without_radio_setup_is_left_alone() {
        let fresh = "[W0CY]\r\nMode=Vara HF Winlink\r\n[Vara TNC]\r\nProcess Id=-1\r\n";
        assert_eq!(
            with_agent_keying(fresh, false),
            (fresh.to_owned(), Vec::new())
        );
    }

    #[test]
    fn keeps_the_original_and_writes_the_file_whole() {
        let folder = std::env::temp_dir().join(format!("lablog-winlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("RMS Express.ini");
        std::fs::write(&path, INI).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(set(&path, false), Ok(Outcome::Changed(changes)) if changes.len() == 3));
        assert_eq!(set(&path, false), Ok(Outcome::AlreadySet));
        let kept = std::fs::read_to_string(folder.join("RMS Express.ini.lablog-original"))
            .unwrap_or_default();
        assert_eq!(kept, INI);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
