// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Enrollment: whether a radio is one of the lab's, under lab settings
//! control. Only an enrolled radio gets the lab's profiles put on as a
//! program starts; a radio the lab has never seen, such as a visitor's, is
//! left alone until an operator says it is the lab's (brian's decision,
//! 2026-10-28; Principle X, second exception). Kept per radio, by its
//! serial number, in a small file in the records folder.

use std::path::{Path, PathBuf};

/// The file the radio with serial number `serial` is enrolled in or left
/// out by, in the records folder `folder`.
#[must_use]
pub fn path(folder: &Path, serial: &str) -> PathBuf {
    let serial: String = serial
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    folder.join(format!("lab-radio-{serial}.txt"))
}

/// Whether the radio is enrolled (`Some(true)`), was left out
/// (`Some(false)`), or nobody has said yet (`None`).
#[must_use]
pub fn load(path: &Path) -> Option<bool> {
    match std::fs::read_to_string(path).ok()?.trim() {
        "enrolled" => Some(true),
        "left out" => Some(false),
        _ => None,
    }
}

/// Enrolls the radio, or leaves it out.
///
/// # Errors
///
/// When the folder can't be made or the file can't be written.
pub fn save(path: &Path, enrolled: bool) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("{}: {error}", folder.display()))?;
    }
    std::fs::write(path, if enrolled { "enrolled\n" } else { "left out\n" })
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// What being enrolled or not means, to tell the operator.
#[must_use]
pub fn said(enrolled: Option<bool>) -> String {
    match enrolled {
        Some(true) => "Lab profile: this radio is enrolled in lab settings control, so the \
                       lab's profile for a program goes on as that program starts (enroll off \
                       leaves it alone)."
            .to_owned(),
        Some(false) => "Lab profile: this radio is not enrolled, so the Agent never puts the \
                        lab's profiles on it (enroll on enrolls it)."
            .to_owned(),
        None => "Lab profile: nobody has said yet whether this is a lab radio, so the Agent \
                 leaves it alone (enroll on enrolls it)."
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{load, path, said, save};

    #[test]
    fn a_radio_is_left_alone_until_enrolled_and_is_remembered_by_serial() {
        let folder = std::env::temp_dir().join(format!("lablog-enroll-{}", std::process::id()));
        let records = folder.join("records");
        let one = path(&records, "12001234");
        let other = path(&records, "../12009999");
        assert!(
            other.starts_with(&records),
            "a serial can't leave the folder"
        );
        assert_eq!(load(&one), None, "nobody has said yet");
        assert_eq!(save(&one, true), Ok(()));
        assert_eq!(load(&one), Some(true));
        assert_eq!(load(&other), None, "each radio is its own");
        assert_eq!(save(&one, false), Ok(()));
        assert_eq!(load(&one), Some(false));
        assert!(std::fs::write(&one, "maybe").is_ok());
        assert_eq!(load(&one), None, "anything else means nobody has said");
        assert!(said(Some(true)).contains("enrolled in lab settings control"));
        assert!(said(None).contains("leaves it alone"));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
