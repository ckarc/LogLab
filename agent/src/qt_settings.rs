// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pointing WSJT-X and `JS8Call` at the Agent: their sound devices at the
//! Agent's virtual cables, and their radio at the Agent's virtual radio,
//! keyed by CAT.
//!
//! Neither program has a command line or network command for these
//! settings. Both keep them in a Qt INI file, read when the program starts
//! and written when it closes, so the Agent changes the file only while the
//! program isn't running. Where everything is was read from each program's
//! source (WSJT-X: `Configuration.cpp`, `MultiSettings.cpp`, `main.cpp`;
//! `JS8Call`, github.com/js8call/js8call, the same files):
//!
//! - The file is `<app>.ini` in Qt's configuration folder, which on Windows
//!   is `%LOCALAPPDATA%\<app>` (Qt's `qstandardpaths_win.cpp`:
//!   `ConfigLocation` is `FOLDERID_LocalAppData` plus the organisation and
//!   application names). `<app>` is `WSJT-X` or `JS8Call`.
//! - The configuration in use is the `[Configuration]` group; the others
//!   kept by name (Settings > Configurations) are left alone.
//! - `SoundInName` and `SoundOutName` are the sound devices, matched by
//!   their whole name against the devices Qt lists.
//! - `Rig` is `Hamlib NET rigctl` (Hamlib's maker and model names), and
//!   `CATNetworkPort` is the virtual radio's address.
//! - `PTTMethod` is a Qt enum saved as a variant. The value written here is
//!   byte for byte what Qt 5 and Qt 6 write for `PTT_method_CAT`, checked by
//!   building a program with each that declares the enum and its stream
//!   operators the way both programs do, saving it, and reading the file
//!   back.
//! - `DataMode` (Settings > Radio > Mode) is set to Data/Pkt,
//!   `Configuration::data_mode_data`. It is the same enum, streamed the
//!   same way, in both programs (WSJT-X `Configuration.hpp`, `JS8Call`
//!   `Configuration.hpp` line 68), and was checked the same way as
//!   `PTTMethod` with Qt 5.15 and Qt 6. WSJT-X's guide
//!   (`settings-radio.adoc`) says to choose Data/Pkt where the radio uses it
//!   for the rear audio input, as the IC-7300's DATA modes do; the program
//!   then asks for the data mode (`PKTUSB`) whenever it tunes, so a band
//!   change never leaves the radio in plain USB.
//! - `Polling` is the seconds between the program's readings of the radio.
//!   Both read a missing one as 0 (WSJT-X `Configuration.cpp`
//!   `value ("Polling", 0)`; `JS8Call` the same, line 2022), and with 0
//!   they never read the radio again (`PollingTransceiver::start_timer`),
//!   so turning the radio's knob wouldn't show. The settings box can't go
//!   below 1, so 0 only comes from a file never saved from it. The Agent
//!   sets a missing or 0 `Polling` to 1 and leaves any other alone.
//!
//! The maintainer decided (2026-10-27) the Agent sets these up itself, so a
//! station works with nothing typed in, as it already does for VARA's sound
//! devices. Nothing else in the file is touched, the file as first found is
//! kept beside it, and the new file is written whole (see `settings_file`).

use std::net::SocketAddr;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::ini;
use crate::settings_file::{self, Change, Outcome};

/// The group holding the configuration in use.
const GROUP: &str = "Configuration";
/// The rig WSJT-X and `JS8Call` list as Hamlib's network client.
pub const RIG: &str = "Hamlib NET rigctl";
/// `PTTMethod` set to CAT, exactly as Qt writes it.
pub const PTT_CAT: &str =
    r"@Variant(\0\0\0\x7f\0\0\0\x1eTransceiverFactory::PTTMethod\0\0\0\0\xfPTT_method_CAT\0)";
/// WSJT-X's `DataMode` set to Data/Pkt, exactly as Qt writes it (Qt escapes
/// a hex digit that follows a `\x` escape, hence `\x43`, `\x64`, `\x61`).
pub const DATA_MODE_DATA: &str =
    r"@Variant(\0\0\0\x7f\0\0\0\x18\x43onfiguration::DataMode\0\0\0\0\xf\x64\x61ta_mode_data\0)";

/// What the Agent wants a program's settings to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    /// The device the program records from, or `None` to leave it alone.
    pub input: Option<String>,
    /// The device the program plays into, or `None` to leave it alone.
    pub output: Option<String>,
    /// The Agent's virtual radio.
    pub radio: SocketAddr,
}

/// `value` as Qt's INI format writes a string, or `None` if it holds
/// anything but printable ASCII (the Agent writes nothing it can't check).
/// Qt quotes a value holding `"`, `\`, `,`, `;` or `=`, or starting or
/// ending with a space, and doubles a leading `@`.
#[must_use]
pub fn encode(value: &str) -> Option<String> {
    if !value.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return None;
    }
    let quote = value.contains(['"', '\\', ',', ';', '='])
        || value.starts_with(' ')
        || value.ends_with(' ');
    if quote {
        let escaped = value.replace('\\', r"\\").replace('"', "\\\"");
        return Some(format!("\"{escaped}\""));
    }
    if value.starts_with('@') {
        return Some(format!("@{value}"));
    }
    Some(value.to_owned())
}

/// The string a value `encode` wrote stands for.
#[must_use]
pub fn decode(raw: &str) -> String {
    if let Some(inner) = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    raw.strip_prefix('@')
        .filter(|rest| rest.starts_with('@'))
        .unwrap_or(raw)
        .to_owned()
}

/// The sound devices the file `text` names: (input, output).
#[must_use]
pub fn sound_devices(text: &str) -> (Option<String>, Option<String>) {
    let read = |key| ini::value(text, GROUP, key).map(|raw| decode(&raw));
    (read("SoundInName"), read("SoundOutName"))
}

/// `text` changed to say what `wanted` says, and what changed.
///
/// # Errors
///
/// A device name Qt's format can't hold as plain text.
pub fn with_station(text: &str, wanted: &Wanted) -> Result<(String, Vec<Change>), String> {
    let device = |name: &str| {
        encode(name).ok_or_else(|| format!("the device name {name:?} isn't plain text"))
    };
    let mut lines: Vec<(&str, String, String)> = Vec::new();
    if let Some(input) = &wanted.input {
        lines.push(("SoundInName", device(input)?, input.clone()));
    }
    if let Some(output) = &wanted.output {
        lines.push(("SoundOutName", device(output)?, output.clone()));
    }
    lines.push(("Rig", RIG.to_owned(), RIG.to_owned()));
    let radio = wanted.radio.to_string();
    lines.push(("CATNetworkPort", radio.clone(), radio));
    lines.push(("PTTMethod", PTT_CAT.to_owned(), "CAT".to_owned()));
    lines.push(("DataMode", DATA_MODE_DATA.to_owned(), "Data/Pkt".to_owned()));
    if matches!(
        ini::value(text, GROUP, "Polling").as_deref(),
        None | Some("0")
    ) {
        lines.push(("Polling", "1".to_owned(), "1".to_owned()));
    }
    let diffs: Vec<Change> = lines
        .iter()
        .filter_map(|(key, raw, shown)| {
            let before = ini::value(text, GROUP, key);
            (before.as_deref() != Some(raw.as_str())).then(|| Change {
                key: (*key).to_owned(),
                before: before.map(|raw| {
                    if raw.starts_with("@Variant(") {
                        raw
                    } else {
                        decode(&raw)
                    }
                }),
                after: shown.clone(),
            })
        })
        .collect();
    let set: Vec<(&str, String)> = lines.into_iter().map(|(key, raw, _)| (key, raw)).collect();
    Ok((ini::with_keys(text, GROUP, &set), diffs))
}

/// Sets the program's settings file at `path` to say what `wanted` says.
/// The program must not be running: it writes its settings when it closes.
///
/// # Errors
///
/// The file can't be read or written, or a device name can't be written.
pub fn set(path: &Path, wanted: &Wanted) -> Result<Outcome, String> {
    settings_file::update(path, |text| with_station(text, wanted))
}

/// Where the settings file of `app` (`WSJT-X` or `JS8Call`) is:
/// `%LOCALAPPDATA%\<app>\<app>.ini`, or, if Qt was given an organisation
/// name, `%LOCALAPPDATA%\<organisation>\<app>\<app>.ini`.
#[cfg(windows)]
#[must_use]
pub fn find(app: &str) -> Option<PathBuf> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let file = format!("{app}.ini");
    let direct = local.join(app).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&local)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(app).join(&file))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::{
        DATA_MODE_DATA, PTT_CAT, Wanted, decode, encode, set, sound_devices, with_station,
    };
    use crate::settings_file::Outcome;

    /// A WSJT-X.ini shaped like the real one: the configuration in use,
    /// another kept by name, and Qt's sorted keys.
    const INI: &str = "[Configuration]\r\nCATNetworkPort=\r\nPTTMethod=@Variant(\\0\\0\\0\\x7f\\0\\0\\0\\x1eTransceiverFactory::PTTMethod\\0\\0\\0\\0\\xfPTT_method_VOX\\0)\r\nRig=Icom IC-7300\r\nSoundInName=Microphone (USB Audio CODEC )\r\nSoundOutName=Speakers (USB Audio CODEC )\r\n\r\n[MultiSettings/Field%20Day/Configuration]\r\nRig=Icom IC-7300\r\n";

    fn wanted() -> Wanted {
        Wanted {
            input: Some("CABLE Output (VB-Audio Virtual Cable)".to_owned()),
            output: Some("CABLE-A Input (VB-Audio Cable A)".to_owned()),
            radio: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
        }
    }

    #[test]
    fn encodes_strings_as_qt_writes_them() {
        // Qt 5 and Qt 6 wrote each of these (checked with QSettings).
        assert_eq!(
            encode("Speakers (USB Audio CODEC )").as_deref(),
            Some("Speakers (USB Audio CODEC )")
        );
        assert_eq!(
            encode("Speakers (2- USB, Audio)").as_deref(),
            Some("\"Speakers (2- USB, Audio)\"")
        );
        assert_eq!(
            encode("say \"hi\" \\ x;y=z").as_deref(),
            Some(r#""say \"hi\" \\ x;y=z""#)
        );
        assert_eq!(encode("@odd").as_deref(), Some("@@odd"));
        assert_eq!(encode(" lead").as_deref(), Some("\" lead\""));
        assert_eq!(encode("Lautsprecher (H\u{f6}rer)"), None);
        for value in [
            "Speakers (2- USB, Audio)",
            "say \"hi\" \\ x;y=z",
            "@odd",
            " lead",
            "plain",
        ] {
            assert_eq!(decode(&encode(value).unwrap_or_default()), value);
        }
    }

    #[test]
    fn sets_sound_rig_and_ptt_and_nothing_else() {
        let (changed, diffs) =
            with_station(INI, &wanted()).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(
            changed,
            format!(
                "[Configuration]\r\nCATNetworkPort=127.0.0.1:4532\r\nPTTMethod={PTT_CAT}\r\nRig=Hamlib NET rigctl\r\nSoundInName=CABLE Output (VB-Audio Virtual Cable)\r\nSoundOutName=CABLE-A Input (VB-Audio Cable A)\r\nDataMode={DATA_MODE_DATA}\r\nPolling=1\r\n\r\n[MultiSettings/Field%20Day/Configuration]\r\nRig=Icom IC-7300\r\n"
            )
        );
        assert_eq!(diffs.len(), 7);
        assert_eq!(diffs[2].key, "Rig");
        assert_eq!(diffs[2].before.as_deref(), Some("Icom IC-7300"));
        assert_eq!(sound_devices(&changed), (wanted().input, wanted().output));
        // Run again: nothing more to change.
        let (again, diffs) =
            with_station(&changed, &wanted()).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(again, changed);
        assert!(diffs.is_empty());
    }

    #[test]
    fn adds_what_a_new_file_lacks_and_leaves_out_what_isnt_wanted() {
        let mut only_radio = wanted();
        only_radio.output = None;
        let (changed, _) = with_station("[General]\nx=1\n", &only_radio)
            .unwrap_or_else(|why| unreachable!("{why}"));
        assert!(
            changed.starts_with("[General]\nx=1\n[Configuration]\nSoundInName=CABLE Output"),
            "{changed}"
        );
        assert!(!changed.contains("SoundOutName"));
        assert!(changed.contains(&format!("PTTMethod={PTT_CAT}\n")));
    }

    #[test]
    fn writes_the_file_and_keeps_the_original() {
        let folder =
            std::env::temp_dir().join(format!("lablog-qt-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("WSJT-X.ini");
        std::fs::write(&path, INI).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(set(&path, &wanted()), Ok(Outcome::Changed(_))));
        assert_eq!(set(&path, &wanted()), Ok(Outcome::AlreadySet));
        let kept =
            std::fs::read_to_string(folder.join("WSJT-X.ini.lablog-original")).unwrap_or_default();
        assert_eq!(kept, INI);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn sets_mode_to_data_pkt_and_a_missing_or_zero_polling_to_one() {
        let file = "[Configuration]\r\nDataMode=@Variant(\\0\\0\\0\\x7f\\0\\0\\0\\x18\\x43onfiguration::DataMode\\0\\0\\0\\0\\xf\\x64\\x61ta_mode_none\\0)\r\nPolling=0\r\n";
        let (changed, diffs) =
            with_station(file, &wanted()).unwrap_or_else(|why| unreachable!("{why}"));
        assert!(
            changed.contains(&format!("\r\nDataMode={DATA_MODE_DATA}\r\n")),
            "{changed}"
        );
        assert!(!changed.contains("ta_mode_none"));
        assert!(changed.contains("\r\nPolling=1\r\n"), "{changed}");
        let shown = |key: &str| {
            diffs
                .iter()
                .find(|change| change.key == key)
                .map(|change| change.after.clone())
        };
        assert_eq!(shown("DataMode").as_deref(), Some("Data/Pkt"));
        assert_eq!(shown("Polling").as_deref(), Some("1"));
        // A polling interval the operator chose is left alone.
        let chosen = file.replace("Polling=0", "Polling=3");
        let (kept, diffs) =
            with_station(&chosen, &wanted()).unwrap_or_else(|why| unreachable!("{why}"));
        assert!(kept.contains("\r\nPolling=3\r\n"), "{kept}");
        assert!(diffs.iter().all(|change| change.key != "Polling"));
    }

    #[test]
    fn data_mode_is_byte_for_byte_what_qt_wrote() {
        // Written by QSettings (Qt 5.15.13 and Qt 6) for
        // QVariant::fromValue(Configuration::data_mode_data), with the enum and
        // its stream operators declared as WSJT-X declares them.
        assert_eq!(
            DATA_MODE_DATA,
            r"@Variant(\0\0\0\x7f\0\0\0\x18\x43onfiguration::DataMode\0\0\0\0\xf\x64\x61ta_mode_data\0)"
        );
    }
}
