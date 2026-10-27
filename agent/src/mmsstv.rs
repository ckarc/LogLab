// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pointing MMSSTV's receive sound device at the Agent's receive cable, so
//! slow-scan TV pictures come in with nothing set up by hand.
//!
//! Only receiving, for now: MMSSTV keys a radio only through a COM port
//! (RTS/DTR, or radio commands it sends on that port), never through
//! Hamlib or a network radio, and the Agent passes a program's transmit
//! audio to the radio only while that program has it keyed. So MMSSTV's
//! sound output is left as it is until the Agent offers it a COM port.
//!
//! MMSSTV is open source (LGPL); where everything is was read from its
//! source (github.com/n5ac/mmsstv: `Main.cpp`, `Sound.cpp`, `Option.cpp`,
//! `Wave.cpp`):
//!
//! - The file is `Mmsstv.ini`, next to `Mmsstv.exe`. MMSSTV reads it when it
//!   starts and writes it when it closes, so the Agent changes it only while
//!   MMSSTV isn't running.
//! - `[Define]` `SoundDevice` names the devices as `"input","output"`, or
//!   `"device"` for one device both ways; `-1` is Windows' default device.
//! - The names are matched exactly against Windows' wave device names,
//!   which are cut at 31 characters (`WAVEINCAPS.szPname`).
//!
//! The maintainer asked (2026-10-27) for every program to be set up as
//! seamlessly as it can be; see `qt_settings` for the same decision about
//! WSJT-X and `JS8Call`. Nothing else in the file is touched, the file as
//! first found is kept beside it, and the new file is written whole (see
//! `settings_file`).

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::ini;
use crate::settings_file::{self, Change, Outcome};

/// The section and key naming MMSSTV's sound devices.
const SECTION: &str = "Define";
/// The key naming MMSSTV's sound devices.
const KEY: &str = "SoundDevice";
/// The longest name Windows gives a wave device.
const WAVE_NAME: usize = 31;
/// Windows' default sound device, as MMSSTV writes it.
const DEFAULT: &str = "-1";

/// Where MMSSTV may be installed, looked in, in order. Its installer's
/// default folder isn't documented anywhere found, so these are guesses;
/// MMSSTV elsewhere is reported as not found.
#[cfg(windows)]
const PLACES: [&str; 4] = [
    r"C:\MMSSTV",
    r"C:\Program Files (x86)\MMSSTV",
    r"C:\Program Files\MMSSTV",
    r"C:\Ham\MMSSTV",
];

/// `device`'s name as Windows' wave devices, and so MMSSTV, name it.
#[must_use]
pub fn wave_name(device: &str) -> String {
    device.chars().take(WAVE_NAME).collect()
}

/// The two devices a `SoundDevice` value names: (input, output). A value
/// naming one device names it both ways.
#[must_use]
pub fn devices(raw: &str) -> (String, String) {
    let mut parts = raw
        .split(',')
        .map(|part| part.trim().trim_matches('"').to_owned());
    let input = parts.next().unwrap_or_else(|| DEFAULT.to_owned());
    let output = parts.next().unwrap_or_else(|| input.clone());
    (input, output)
}

/// `text` with MMSSTV's receive device set to `input`, and what changed.
///
/// # Errors
///
/// `input` holds a `"` or `,`, which MMSSTV's format can't hold.
pub fn with_input(text: &str, input: &str) -> Result<(String, Vec<Change>), String> {
    if input.contains(['"', ',']) {
        return Err(format!(
            "the device name {input:?} can't be written for MMSSTV"
        ));
    }
    let input = wave_name(input);
    let before = ini::value(text, SECTION, KEY);
    let (held_input, held_output) = before
        .as_deref()
        .map_or_else(|| (DEFAULT.to_owned(), DEFAULT.to_owned()), devices);
    if held_input == input {
        return Ok((text.to_owned(), Vec::new()));
    }
    let quoted = |name: &str| {
        if name.parse::<i32>().is_ok() {
            name.to_owned()
        } else {
            format!("\"{name}\"")
        }
    };
    let raw = format!("{},{}", quoted(&input), quoted(&held_output));
    let changed = ini::with_keys(text, SECTION, &[(KEY, raw)]);
    let change = Change {
        key: format!("{KEY} (input)"),
        before: before.map(|_| held_input),
        after: input,
    };
    Ok((changed, vec![change]))
}

/// Sets MMSSTV's receive device in its settings file at `path`. MMSSTV must
/// not be running: it writes its settings when it closes.
///
/// # Errors
///
/// The file can't be read or written, or the name can't be written.
pub fn set(path: &Path, input: &str) -> Result<Outcome, String> {
    settings_file::update(path, |text| with_input(text, input))
}

/// Where MMSSTV's settings file is: `Mmsstv.ini` next to `Mmsstv.exe`, in
/// the first of the usual places that has one.
#[cfg(windows)]
#[must_use]
pub fn find() -> Option<PathBuf> {
    PLACES
        .iter()
        .map(|place| Path::new(place).join("Mmsstv.ini"))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::{devices, set, wave_name, with_input};
    use crate::settings_file::Outcome;

    const INI: &str = "[SoundCard]\r\nSampFreq=11025.00\r\n[Define]\r\nSoundDevice=\"Microphone (USB Audio CODEC )\",\"Speakers (USB Audio CODEC )\"\r\nPTT=NONE\r\n";
    const CABLE: &str = "CABLE Output (VB-Audio Virtual Cable)";

    #[test]
    fn names_a_device_as_windows_wave_devices_do() {
        assert_eq!(wave_name(CABLE), "CABLE Output (VB-Audio Virtual ");
        assert_eq!(wave_name("Short"), "Short");
    }

    #[test]
    fn reads_the_devices() {
        assert_eq!(devices("\"a b\",\"c\""), ("a b".to_owned(), "c".to_owned()));
        assert_eq!(devices("\"a\""), ("a".to_owned(), "a".to_owned()));
        assert_eq!(devices("-1"), ("-1".to_owned(), "-1".to_owned()));
    }

    #[test]
    fn sets_the_input_and_keeps_the_output() {
        let (changed, diffs) = with_input(INI, CABLE).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(
            changed,
            "[SoundCard]\r\nSampFreq=11025.00\r\n[Define]\r\nSoundDevice=\"CABLE Output (VB-Audio Virtual \",\"Speakers (USB Audio CODEC )\"\r\nPTT=NONE\r\n"
        );
        assert_eq!(
            diffs[0].before.as_deref(),
            Some("Microphone (USB Audio CODEC )")
        );
        let (again, diffs) =
            with_input(&changed, CABLE).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(again, changed);
        assert!(diffs.is_empty());
    }

    #[test]
    fn keeps_one_device_both_ways_as_the_output() {
        let one = "[Define]\nSoundDevice=\"Speakers (Radio)\"\n";
        let (changed, _) = with_input(one, CABLE).unwrap_or_else(|why| unreachable!("{why}"));
        assert!(
            changed
                .contains("SoundDevice=\"CABLE Output (VB-Audio Virtual \",\"Speakers (Radio)\"\n")
        );
        let (changed, _) =
            with_input("[Define]\nPTT=NONE\n", CABLE).unwrap_or_else(|why| unreachable!("{why}"));
        assert!(
            changed.contains("SoundDevice=\"CABLE Output (VB-Audio Virtual \",-1\n"),
            "{changed}"
        );
        assert!(with_input(one, "a, b").is_err());
    }

    #[test]
    fn writes_the_file_and_keeps_the_original() {
        let folder = std::env::temp_dir().join(format!("lablog-mmsstv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("Mmsstv.ini");
        std::fs::write(&path, INI).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(set(&path, CABLE), Ok(Outcome::Changed(_))));
        assert_eq!(set(&path, CABLE), Ok(Outcome::AlreadySet));
        let kept =
            std::fs::read_to_string(folder.join("Mmsstv.ini.lablog-original")).unwrap_or_default();
        assert_eq!(kept, INI);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
