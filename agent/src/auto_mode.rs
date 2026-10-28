// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Auto mode: whether, on this computer, the Agent makes the changes it
//! would otherwise offer for the operator's yes (Principle X, third
//! exception). Off until an operator turns it on; kept in a small file in
//! the records folder, so it stays with this computer and nowhere else.

use std::path::{Path, PathBuf};

/// The file auto mode is kept in, in the records folder `folder`.
#[must_use]
pub fn path(folder: &Path) -> PathBuf {
    folder.join("auto-mode.txt")
}

/// Whether auto mode is on: only if the file says `on`. A missing or
/// unreadable file means off.
#[must_use]
pub fn load(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.trim() == "on")
}

/// Keeps auto mode on or off.
///
/// # Errors
///
/// When the folder can't be made or the file can't be written.
pub fn save(path: &Path, on: bool) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("{}: {error}", folder.display()))?;
    }
    std::fs::write(path, if on { "on\n" } else { "off\n" })
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// What auto mode being `on` or off means, to tell the operator.
#[must_use]
pub fn said(on: bool) -> String {
    if on {
        "auto mode is on on this computer: the Agent makes its fixes without asking, between \
         transmissions, and says each one (auto off asks first)."
            .to_owned()
    } else {
        "auto mode is off on this computer: the Agent asks before each change (auto on lets it \
         go ahead)."
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{load, path, said, save};

    #[test]
    fn is_off_until_turned_on_and_remembered() {
        let folder = std::env::temp_dir().join(format!("lablog-auto-{}", std::process::id()));
        let file = path(&folder.join("records"));
        assert!(!load(&file), "off when nothing was ever saved");
        assert_eq!(save(&file, true), Ok(()));
        assert!(load(&file));
        assert_eq!(save(&file, false), Ok(()));
        assert!(!load(&file));
        assert!(std::fs::write(&file, "maybe").is_ok());
        assert!(!load(&file), "anything but on is off");
        assert!(said(true).starts_with("auto mode is on"));
        assert!(said(false).starts_with("auto mode is off"));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
