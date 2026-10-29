// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Setting MMSSTV up to send and receive slow-scan TV pictures through the
//! Agent, with nothing set up by hand.
//!
//! Receiving: its input is the Agent's receive cable. Transmitting: its
//! output is the Agent's transmit cable, and it keys the radio through
//! LabLog's PTT plugin, `LabLog.fsk` (the `mmsstv-ptt` package), which the
//! Agent carries and puts next to `Mmsstv.exe`. MMSSTV has no Hamlib or
//! network radio control; it keys through a COM port or, when the port name
//! it is given isn't one, through a plugin named `NAME.fsk` that it loads
//! from its folder (see the plugin's notes). `PTT=LabLog` names the plugin.
//! An Agent built without the plugin sets up receiving only.
//!
//! MMSSTV is open source (LGPL); where everything is was read from its
//! source (github.com/n5ac/mmsstv @ 8060b5f: `Main.cpp`, `Sound.cpp`,
//! `Option.cpp`, `Wave.cpp`, `Comm.cpp`, `cradio.cpp`):
//!
//! - The file is `Mmsstv.ini`, next to `Mmsstv.exe`. MMSSTV reads it when it
//!   starts and writes it when it closes, so the Agent changes it only while
//!   MMSSTV isn't running.
//! - `[Define]` `SoundDevice` names the devices as `"input","output"`, or
//!   `"device"` for one device both ways; `-1` is Windows' default device.
//! - The names are matched exactly against Windows' wave device names,
//!   which are cut at 31 characters (`WAVEINCAPS.szPname`).
//! - `[Define]` `PTT` is the PTT port's name (`NONE` for none), and
//!   `PTTLock` = 1 keeps it open between transmissions (`Main.cpp`,
//!   `ReadRegister`; 1 is MMSSTV's own default). With it set, MMSSTV says
//!   at its start if it can't load the plugin ("Could not open 'LabLog' for
//!   the PTT controls", `OpenCloseCom`).
//! - `[RADIO]` `PortName` is the port MMSSTV sends radio commands on
//!   (`cradio.cpp`). The Agent owns the radio's COM port (Principle IX), so
//!   a COM port named there is set to `NONE`; MMSSTV couldn't open it
//!   anyway while the Agent has it.
//!
//! The maintainer asked (2026-10-27) for every program to be set up as
//! seamlessly as it can be, MMSSTV included; see `qt_settings` for the same
//! decision about WSJT-X and `JS8Call`. Nothing else in the file is touched,
//! the file as first found is kept beside it, and the new file is written
//! whole (see `settings_file`).

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::ini;
use crate::settings_file::{self, Change, Outcome};

/// The section naming MMSSTV's sound devices and PTT port.
const SECTION: &str = "Define";
/// The key naming MMSSTV's sound devices.
const KEY: &str = "SoundDevice";
/// The longest name Windows gives a wave device.
const WAVE_NAME: usize = 31;
/// Windows' default sound device, as MMSSTV writes it.
const DEFAULT: &str = "-1";
/// The PTT port name that makes MMSSTV load LabLog's plugin.
pub const PLUGIN_NAME: &str = "LabLog";
/// The plugin's file, next to `Mmsstv.exe`: the name, then `.fsk`.
pub const PLUGIN_FILE: &str = "LabLog.fsk";

/// LabLog's PTT plugin for MMSSTV, a 32-bit DLL, when this Agent was built
/// with it (CI's Windows build; see `build.rs`).
#[cfg(mmsstv_plugin)]
pub const PLUGIN: Option<&[u8]> = Some(include_bytes!(env!("LABLOG_MMSSTV_PTT_FILE")));
/// LabLog's PTT plugin for MMSSTV: not in this build.
#[cfg(not(mmsstv_plugin))]
pub const PLUGIN: Option<&[u8]> = None;

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

/// What MMSSTV should be set to.
#[derive(Debug)]
pub struct Wanted<'a> {
    /// The sound device it receives from.
    pub input: &'a str,
    /// The sound device it transmits into, when it transmits through the
    /// Agent; it then keys through LabLog's plugin too. `None` leaves its
    /// output and PTT as they are.
    pub output: Option<&'a str>,
}

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

/// `name` as `SoundDevice` writes it: a number bare, a name quoted.
fn quoted(name: &str) -> String {
    if name.parse::<i32>().is_ok() {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    }
}

/// `name`, checked to fit MMSSTV's format, cut as Windows names it.
fn device(name: &str) -> Result<String, String> {
    if name.contains(['"', ',']) {
        return Err(format!(
            "the device name {name:?} can't be written for MMSSTV"
        ));
    }
    Ok(wave_name(name))
}

/// `text` set up as `wanted`, and what changed.
///
/// # Errors
///
/// A device name holds a `"` or `,`, which MMSSTV's format can't hold.
pub fn with(text: &str, wanted: &Wanted<'_>) -> Result<(String, Vec<Change>), String> {
    let input = device(wanted.input)?;
    let output = wanted.output.map(device).transpose()?;
    let before = ini::value(text, SECTION, KEY);
    let (held_input, held_output) = before
        .as_deref()
        .map_or_else(|| (DEFAULT.to_owned(), DEFAULT.to_owned()), devices);
    let new_output = output.clone().unwrap_or_else(|| held_output.clone());
    let mut define: Vec<(&str, String)> = Vec::new();
    let mut changes = Vec::new();
    if held_input != input || held_output != new_output {
        define.push((KEY, format!("{},{}", quoted(&input), quoted(&new_output))));
    }
    for (side, held, now) in [
        ("input", held_input, input),
        ("output", held_output, new_output),
    ] {
        if held != now {
            changes.push(Change {
                key: format!("{KEY} ({side})"),
                before: before.as_ref().map(|_| held),
                after: now,
            });
        }
    }
    let mut radio: Vec<(&str, String)> = Vec::new();
    if output.is_some() {
        for (key, value) in [("PTT", PLUGIN_NAME), ("PTTLock", "1")] {
            let held = ini::value(text, SECTION, key);
            if held.as_deref() != Some(value) {
                define.push((key, value.to_owned()));
                changes.push(Change {
                    key: key.to_owned(),
                    before: held,
                    after: value.to_owned(),
                });
            }
        }
        let port = ini::value(text, "RADIO", "PortName");
        if port
            .as_deref()
            .is_some_and(|port| port.to_ascii_uppercase().starts_with("COM"))
        {
            radio.push(("PortName", "NONE".to_owned()));
            changes.push(Change {
                key: "PortName (radio commands)".to_owned(),
                before: port,
                after: "NONE".to_owned(),
            });
        }
    }
    let mut out = text.to_owned();
    if !define.is_empty() {
        out = ini::with_keys(&out, SECTION, &define);
    }
    if !radio.is_empty() {
        out = ini::with_keys(&out, "RADIO", &radio);
    }
    Ok((out, changes))
}

/// Sets MMSSTV up as `wanted` in its settings file at `path`. MMSSTV must
/// not be running: it writes its settings when it closes.
///
/// # Errors
///
/// The file can't be read or written, or a name can't be written.
pub fn set(path: &Path, wanted: &Wanted<'_>) -> Result<Outcome, String> {
    settings_file::update(path, |text| with(text, wanted))
}

/// Puts `plugin` in `folder` (MMSSTV's) as [`PLUGIN_FILE`], unless it is
/// there already; written whole, then renamed into place. Whether it was
/// written.
///
/// # Errors
///
/// The folder can't be written (MMSSTV installed under Program Files, say).
pub fn install(folder: &Path, plugin: &[u8]) -> Result<bool, String> {
    let path = folder.join(PLUGIN_FILE);
    if std::fs::read(&path).is_ok_and(|held| held == plugin) {
        return Ok(false);
    }
    let temporary = folder.join(format!("{PLUGIN_FILE}.lablog-new"));
    std::fs::write(&temporary, plugin)
        .and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|error| format!("can't write {}: {error}", path.display()))?;
    Ok(true)
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
    use super::{PLUGIN, PLUGIN_FILE, Wanted, devices, install, set, wave_name, with};
    use crate::pe_exports;
    use crate::settings_file::Outcome;

    const INI: &str = "[SoundCard]\r\nSampFreq=11025.00\r\n[Define]\r\nSoundDevice=\"Microphone (USB Audio CODEC )\",\"Speakers (USB Audio CODEC )\"\r\nPTT=NONE\r\n[RADIO]\r\nPortName=COM4\r\nCmdxx=94\r\n";
    const CABLE: &str = "CABLE Output (VB-Audio Virtual Cable)";
    const TRANSMIT: &str = "CABLE-A Input (VB-Audio Cable A)";

    fn receive_only() -> Wanted<'static> {
        Wanted {
            input: CABLE,
            output: None,
        }
    }

    fn both_ways() -> Wanted<'static> {
        Wanted {
            input: CABLE,
            output: Some(TRANSMIT),
        }
    }

    fn set_up(text: &str, wanted: &Wanted<'_>) -> (String, Vec<crate::settings_file::Change>) {
        with(text, wanted).unwrap_or_else(|why| unreachable!("{why}"))
    }

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
    fn receive_only_sets_the_input_and_keeps_the_rest() {
        let (changed, diffs) = set_up(INI, &receive_only());
        assert_eq!(
            changed,
            "[SoundCard]\r\nSampFreq=11025.00\r\n[Define]\r\nSoundDevice=\"CABLE Output (VB-Audio Virtual \",\"Speakers (USB Audio CODEC )\"\r\nPTT=NONE\r\n[RADIO]\r\nPortName=COM4\r\nCmdxx=94\r\n"
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            diffs[0].before.as_deref(),
            Some("Microphone (USB Audio CODEC )")
        );
        let (again, diffs) = set_up(&changed, &receive_only());
        assert_eq!(again, changed);
        assert!(diffs.is_empty());
    }

    #[test]
    fn transmitting_sets_the_output_the_plugin_and_frees_the_radio_port() {
        let (changed, diffs) = set_up(INI, &both_ways());
        assert_eq!(
            changed,
            "[SoundCard]\r\nSampFreq=11025.00\r\n[Define]\r\nSoundDevice=\"CABLE Output (VB-Audio Virtual \",\"CABLE-A Input (VB-Audio Cable A\"\r\nPTT=LabLog\r\nPTTLock=1\r\n[RADIO]\r\nPortName=NONE\r\nCmdxx=94\r\n"
        );
        let keys: Vec<&str> = diffs.iter().map(|diff| diff.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "SoundDevice (input)",
                "SoundDevice (output)",
                "PTT",
                "PTTLock",
                "PortName (radio commands)"
            ]
        );
        assert_eq!(diffs[2].before.as_deref(), Some("NONE"));
        assert_eq!(diffs[3].before, None);
        assert_eq!(diffs[4].before.as_deref(), Some("COM4"));
        let (again, diffs) = set_up(&changed, &both_ways());
        assert_eq!(again, changed);
        assert!(diffs.is_empty());
    }

    #[test]
    fn leaves_a_radio_plugin_and_no_radio_port_alone() {
        for port in ["NONE", "MyRig"] {
            let text = format!("[Define]\nPTT=COM3\n[RADIO]\nPortName={port}\n");
            let (changed, _) = set_up(&text, &both_ways());
            assert!(
                changed.ends_with(&format!("[RADIO]\nPortName={port}\n")),
                "{changed}"
            );
            assert!(changed.contains("PTT=LabLog\n"), "{changed}");
        }
    }

    #[test]
    fn keeps_one_device_both_ways_as_the_output() {
        let one = "[Define]\nSoundDevice=\"Speakers (Radio)\"\n";
        let (changed, _) = set_up(one, &receive_only());
        assert!(
            changed
                .contains("SoundDevice=\"CABLE Output (VB-Audio Virtual \",\"Speakers (Radio)\"\n")
        );
        let (changed, _) = set_up("[Define]\nPTT=NONE\n", &receive_only());
        assert!(
            changed.contains("SoundDevice=\"CABLE Output (VB-Audio Virtual \",-1\n"),
            "{changed}"
        );
        assert!(
            with(
                one,
                &Wanted {
                    input: "a, b",
                    output: None
                }
            )
            .is_err()
        );
        assert!(
            with(
                one,
                &Wanted {
                    input: CABLE,
                    output: Some("say \"hi\"")
                }
            )
            .is_err()
        );
    }

    #[test]
    fn writes_the_file_and_the_plugin_and_keeps_the_original() {
        let folder = std::env::temp_dir().join(format!("lablog-mmsstv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("Mmsstv.ini");
        std::fs::write(&path, INI).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(set(&path, &both_ways()), Ok(Outcome::Changed(_))));
        assert_eq!(set(&path, &both_ways()), Ok(Outcome::AlreadySet));
        let kept =
            std::fs::read_to_string(folder.join("Mmsstv.ini.lablog-original")).unwrap_or_default();
        assert_eq!(kept, INI);
        assert_eq!(install(&folder, b"plugin"), Ok(true));
        assert_eq!(install(&folder, b"plugin"), Ok(false));
        assert_eq!(install(&folder, b"newer"), Ok(true));
        assert_eq!(
            std::fs::read(folder.join(PLUGIN_FILE)).unwrap_or_default(),
            b"newer"
        );
        assert!(!folder.join("LabLog.fsk.lablog-new").exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_carried_plugin_is_one_mmsstv_can_load() {
        // Checked again here (build.rs checks it too), so a test fails if
        // the build's check is ever lost. Without the plugin, nothing to
        // check.
        let Some(plugin) = PLUGIN else {
            return;
        };
        let exports = pe_exports::read(plugin).unwrap_or_else(|| unreachable!("not a DLL"));
        assert_eq!(exports.machine, pe_exports::I386, "MMSSTV is 32-bit");
        for name in [
            "extfskOpen",
            "extfskClose",
            "extfskIsTxBusy",
            "extfskPutChar",
            "extfskSetPTT",
        ] {
            assert!(exports.names.iter().any(|found| found == name), "{name}");
        }
    }
}
