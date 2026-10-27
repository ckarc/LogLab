// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Writing another program's settings file safely: the file as the Agent
//! first found it is kept beside it, and the new file is written whole and
//! renamed into place, so the program never reads half a file
//! (Principle XIV).

use std::fs;
use std::path::{Path, PathBuf};

/// What became of a program's settings file.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It already said what the Agent wants.
    AlreadySet,
    /// It now does; these are the lines that changed.
    Changed(Vec<Change>),
}

/// One setting the Agent changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    /// The setting's name in the file.
    pub key: String,
    /// What the file said before, if it said anything.
    pub before: Option<String>,
    /// What it says now.
    pub after: String,
}

/// A settings file's bytes as text, and how to turn the text back into
/// bytes. UTF-8 is read as UTF-8; anything else is read one byte to one
/// character (Latin-1), so every byte the Agent doesn't change is written
/// back exactly as it was read.
#[derive(Debug, PartialEq, Eq)]
pub struct Text {
    /// The file's text.
    pub text: String,
    utf8: bool,
}

impl Text {
    /// `bytes`, as text.
    #[must_use]
    pub fn read(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => Self {
                text: text.to_owned(),
                utf8: true,
            },
            Err(_) => Self {
                text: bytes.iter().map(|&byte| char::from(byte)).collect(),
                utf8: false,
            },
        }
    }

    /// `text`, a changed copy of this file's text, as the file's bytes.
    /// `None` if it holds a character the file's encoding can't (only
    /// possible if a change added one).
    #[must_use]
    pub fn bytes_of(&self, text: &str) -> Option<Vec<u8>> {
        if self.utf8 {
            return Some(text.as_bytes().to_vec());
        }
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect()
    }
}

/// Changes the settings file at `path`: `change` gets its text and returns
/// the new text and what changed. Nothing is written if nothing changed.
///
/// # Errors
///
/// The file can't be read or written, or `change` refuses.
pub fn update(
    path: &Path,
    change: impl FnOnce(&str) -> Result<(String, Vec<Change>), String>,
) -> Result<Outcome, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("can't read {}: {error}", path.display()))?;
    let read = Text::read(&bytes);
    let (text, changes) = change(&read.text)?;
    if changes.is_empty() {
        return Ok(Outcome::AlreadySet);
    }
    let after = read.bytes_of(&text).ok_or_else(|| {
        format!(
            "a new setting can't be written in {}'s encoding",
            path.display()
        )
    })?;
    replace(path, &bytes, &after)?;
    Ok(Outcome::Changed(changes))
}

/// The copy of `path` as the Agent first found it: the same name with
/// `.lablog-original` added.
#[must_use]
pub fn original(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lablog-original");
    path.with_file_name(name)
}

/// Replaces the file at `path`, which held `before`, with `after`. Keeps
/// `before` as the original first, unless an original is already kept.
///
/// # Errors
///
/// The original or the new file can't be written.
pub fn replace(path: &Path, before: &[u8], after: &[u8]) -> Result<(), String> {
    let kept = original(path);
    if !kept.exists() {
        fs::write(&kept, before)
            .map_err(|error| format!("can't keep a copy at {}: {error}", kept.display()))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lablog-new");
    let temporary = path.with_file_name(name);
    fs::write(&temporary, after)
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(|error| format!("can't write {}: {error}", path.display()))
}

/// Whether a program whose executable is `image` (`WSJT-X.exe`, say) is
/// running, from Windows' list of running programs.
///
/// # Errors
///
/// The list couldn't be read.
#[cfg(windows)]
pub fn running(image: &str) -> Result<bool, String> {
    let listed = std::process::Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH", "/FO", "CSV"])
        .output()
        .map_err(|error| format!("can't list running programs: {error}"))?;
    Ok(String::from_utf8_lossy(&listed.stdout)
        .to_ascii_lowercase()
        .contains(&format!("\"{}\"", image.to_ascii_lowercase())))
}

#[cfg(test)]
mod tests {
    use super::{Text, original, replace};

    #[test]
    fn carries_bytes_that_are_not_utf8_over_unchanged() {
        let bytes = b"Name=Jos\xe9\r\n";
        let read = Text::read(bytes);
        assert_eq!(read.bytes_of(&read.text).as_deref(), Some(&bytes[..]));
        let changed = format!("{}Added=x\r\n", read.text);
        assert_eq!(
            read.bytes_of(&changed),
            Some(b"Name=Jos\xe9\r\nAdded=x\r\n".to_vec())
        );
        assert_eq!(read.bytes_of("\u{263a}"), None);
        let utf8 = Text::read("Name=Jos\u{e9}".as_bytes());
        assert_eq!(
            utf8.bytes_of(&utf8.text),
            Some("Name=Jos\u{e9}".as_bytes().to_vec())
        );
    }

    #[test]
    fn keeps_the_first_original_and_replaces_the_file() {
        let folder =
            std::env::temp_dir().join(format!("lablog-settings-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("Program.ini");
        std::fs::write(&path, "first").unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(original(&path), folder.join("Program.ini.lablog-original"));
        assert_eq!(replace(&path, b"first", b"second"), Ok(()));
        assert_eq!(replace(&path, b"second", b"third"), Ok(()));
        let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(read(&path), "third");
        assert_eq!(read(&original(&path)), "first", "the file as first found");
        assert!(!folder.join("Program.ini.lablog-new").exists());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
