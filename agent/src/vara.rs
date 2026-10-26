// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pointing VARA's sound devices at the Agent's virtual cables.
//!
//! VARA has no command line or control-port command for its sound devices;
//! they are kept in `VARA.ini`, next to `VARA.exe`, under `[Soundcard]` as
//! `Input Device Name=` and `Output Device Name=`. (The file and the key
//! names are as read by varanny, an open-source VARA launcher,
//! <https://github.com/islandmagic/varanny>, `vara_config.go`. It changes
//! the file only while VARA isn't running, and so does the Agent.)
//!
//! The maintainer decided the Agent sets these two lines itself (2026-10-26),
//! as an exception to the rule that the Agent never writes another program's
//! settings. So the Agent changes nothing else in the file, keeps a copy of
//! the file as it first found it, and writes it whole and renames it into
//! place (Principle XIV).

use std::fs;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

/// The section VARA keeps its sound devices in.
const SECTION: &str = "[Soundcard]";
/// VARA's capturing device.
const INPUT: &str = "Input Device Name";
/// VARA's playing device.
const OUTPUT: &str = "Output Device Name";
/// The copy of the file as the Agent first found it.
const BACKUP: &str = "VARA.ini.lablog-original";

/// Where VARA is usually installed, looked in, in order.
#[cfg(windows)]
const PLACES: [&str; 3] = [
    r"C:\VARA",
    r"C:\Program Files\VARA",
    r"C:\Program Files (x86)\VARA",
];

/// VARA's sound devices, as its settings file names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Soundcard {
    /// The device VARA records from.
    pub input: Option<String>,
    /// The device VARA plays into.
    pub output: Option<String>,
}

/// The sound devices `text`, a `VARA.ini`, names.
#[must_use]
pub fn soundcard(text: &str) -> Soundcard {
    let mut found = Soundcard::default();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line.eq_ignore_ascii_case(SECTION);
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                INPUT => found.input = Some(value.to_owned()),
                OUTPUT => found.output = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    found
}

/// `text` with VARA's sound devices set to `wanted`, every other line left
/// exactly as it was, line endings included. Adds the section or the keys if
/// they are missing.
#[must_use]
pub fn with_soundcard(text: &str, wanted: &Soundcard) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out: Vec<String> = Vec::new();
    let mut inside = false;
    let mut seen_section = false;
    let mut wrote_input = false;
    let mut wrote_output = false;
    let finish = |out: &mut Vec<String>, wrote_input: &mut bool, wrote_output: &mut bool| {
        if let Some(input) = wanted.input.as_ref().filter(|_| !*wrote_input) {
            out.push(format!("{INPUT}={input}"));
            *wrote_input = true;
        }
        if let Some(output) = wanted.output.as_ref().filter(|_| !*wrote_output) {
            out.push(format!("{OUTPUT}={output}"));
            *wrote_output = true;
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if inside {
                finish(&mut out, &mut wrote_input, &mut wrote_output);
            }
            inside = trimmed.eq_ignore_ascii_case(SECTION);
            seen_section |= inside;
            out.push(line.to_owned());
            continue;
        }
        let key = trimmed.split_once('=').map(|(key, _)| key.trim());
        match (inside, key, &wanted.input, &wanted.output) {
            (true, Some(INPUT), Some(input), _) => {
                out.push(format!("{INPUT}={input}"));
                wrote_input = true;
            }
            (true, Some(OUTPUT), _, Some(output)) => {
                out.push(format!("{OUTPUT}={output}"));
                wrote_output = true;
            }
            _ => out.push(line.to_owned()),
        }
    }
    if inside {
        finish(&mut out, &mut wrote_input, &mut wrote_output);
    }
    if !seen_section {
        out.push(SECTION.to_owned());
        finish(&mut out, &mut wrote_input, &mut wrote_output);
    }
    let mut joined = out.join(newline);
    joined.push_str(newline);
    joined
}

/// The name VARA would show for `device`: VARA's own file may hold device
/// names cut short, so when the name it holds now is the start of `device`
/// at some length, the same length is used again. Otherwise, the whole name.
#[must_use]
pub fn as_vara_names_it(device: &str, held: Option<&str>, others: &[String]) -> String {
    let Some(held) = held.filter(|held| !held.is_empty()) else {
        return device.to_owned();
    };
    // Which device is VARA's current choice, and how much of its name it
    // kept.
    let cut = others
        .iter()
        .find(|name| name.as_str() != held && name.starts_with(held))
        .map(|_| held.chars().count());
    match cut {
        Some(length) => device.chars().take(length).collect(),
        None => device.to_owned(),
    }
}

/// Where VARA's settings file is: `VARA.ini` next to `VARA.exe`, in the first
/// of the usual places that has one.
#[cfg(windows)]
#[must_use]
pub fn find() -> Option<PathBuf> {
    PLACES
        .iter()
        .map(|place| Path::new(place).join("VARA.ini"))
        .find(|path| path.is_file())
}

/// What happened to VARA's settings file.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It already named these devices.
    AlreadySet,
    /// It now names them; it named these before.
    Changed(Soundcard),
}

/// Sets VARA's sound devices in the settings file at `path` to `wanted`; a
/// device `wanted` leaves out is left as it is. VARA must not be running: it
/// keeps its settings while it runs.
///
/// # Errors
///
/// The file can't be read or written.
pub fn set(path: &Path, wanted: &Soundcard) -> Result<Outcome, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("can't read {}: {error}", path.display()))?;
    // The file is plain text in Windows' own code page; anything outside
    // ASCII is carried over as it was read.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let before = soundcard(&text);
    let same = |want: &Option<String>, have: &Option<String>| want.is_none() || want == have;
    if same(&wanted.input, &before.input) && same(&wanted.output, &before.output) {
        return Ok(Outcome::AlreadySet);
    }
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let backup = folder.join(BACKUP);
    if !backup.exists() {
        fs::write(&backup, &bytes)
            .map_err(|error| format!("can't keep a copy at {}: {error}", backup.display()))?;
    }
    let changed = with_soundcard(&text, wanted);
    let temporary = folder.join("VARA.ini.lablog-new");
    fs::write(&temporary, changed.as_bytes())
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(|error| format!("can't write {}: {error}", path.display()))?;
    Ok(Outcome::Changed(before))
}

/// Whether VARA is running, from Windows' list of running programs.
///
/// # Errors
///
/// The list couldn't be read.
#[cfg(windows)]
pub fn running() -> Result<bool, String> {
    let listed = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq VARA.exe", "/NH", "/FO", "CSV"])
        .output()
        .map_err(|error| format!("can't list running programs: {error}"))?;
    Ok(String::from_utf8_lossy(&listed.stdout)
        .to_ascii_lowercase()
        .contains("\"vara.exe\""))
}

#[cfg(test)]
mod tests {
    use super::{Outcome, Soundcard, as_vara_names_it, set, soundcard, with_soundcard};

    const INI: &str = "[Setup]\r\nTCP Command Port=8300\r\n\r\n[Soundcard]\r\nInput Device Name=Microphone (USB Audio CODEC )\r\nOutput Device Name=Speakers (USB Audio CODEC )\r\nDrive Level=-5\r\n[Monitor]\r\nOn=0\r\n";

    fn cables() -> Soundcard {
        Soundcard {
            input: Some("CABLE Output (VB-Audio Virtual Cable)".to_owned()),
            output: Some("CABLE-A Input (VB-Audio Cable A)".to_owned()),
        }
    }

    #[test]
    fn reads_the_sound_devices() {
        assert_eq!(
            soundcard(INI),
            Soundcard {
                input: Some("Microphone (USB Audio CODEC )".to_owned()),
                output: Some("Speakers (USB Audio CODEC )".to_owned()),
            }
        );
        assert_eq!(
            soundcard("[Setup]\nInput Device Name=x\n"),
            Soundcard::default()
        );
    }

    #[test]
    fn changes_only_the_two_lines() {
        let changed = with_soundcard(INI, &cables());
        assert_eq!(
            changed,
            "[Setup]\r\nTCP Command Port=8300\r\n\r\n[Soundcard]\r\nInput Device Name=CABLE Output (VB-Audio Virtual Cable)\r\nOutput Device Name=CABLE-A Input (VB-Audio Cable A)\r\nDrive Level=-5\r\n[Monitor]\r\nOn=0\r\n"
        );
        assert_eq!(soundcard(&changed), cables());
    }

    #[test]
    fn adds_what_is_missing() {
        let added = with_soundcard("[Setup]\nTCP Command Port=8300\n", &cables());
        assert_eq!(soundcard(&added), cables());
        assert!(added.starts_with("[Setup]\nTCP Command Port=8300\n[Soundcard]\n"));
        let added = with_soundcard("[Soundcard]\nDrive Level=-5\n[Monitor]\n", &cables());
        assert_eq!(soundcard(&added), cables());
        assert!(added.ends_with("Cable A)\n[Monitor]\n"), "{added}");
    }

    #[test]
    fn names_a_device_the_way_vara_already_does() {
        let windows = [
            "CABLE Output (VB-Audio Virtual Cable)".to_owned(),
            "Microphone (2- USB Audio CODEC )".to_owned(),
        ];
        // VARA's file holds whole names: the whole name.
        assert_eq!(
            as_vara_names_it(&windows[0], Some(windows[1].as_str()), &windows),
            windows[0]
        );
        // VARA's file holds a name cut at 31 characters: cut the same way.
        let cut: String = windows[1].chars().take(31).collect();
        assert_eq!(
            as_vara_names_it(&windows[0], Some(&cut), &windows),
            "CABLE Output (VB-Audio Virtual "
        );
        assert_eq!(as_vara_names_it(&windows[0], None, &windows), windows[0]);
    }

    #[test]
    fn keeps_the_original_and_writes_the_file_whole() {
        let folder = std::env::temp_dir().join(format!("lablog-vara-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("VARA.ini");
        std::fs::write(&path, INI).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(set(&path, &cables()), Ok(Outcome::Changed(soundcard(INI))));
        assert_eq!(set(&path, &cables()), Ok(Outcome::AlreadySet));
        let now = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(soundcard(&now), cables());
        let original =
            std::fs::read_to_string(folder.join("VARA.ini.lablog-original")).unwrap_or_default();
        assert_eq!(original, INI, "the file as first found");
        // A later change keeps the first original, not the Agent's own.
        let other = Soundcard {
            input: Some("x".to_owned()),
            output: Some("y".to_owned()),
        };
        assert!(set(&path, &other).is_ok());
        let original =
            std::fs::read_to_string(folder.join("VARA.ini.lablog-original")).unwrap_or_default();
        assert_eq!(original, INI);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
