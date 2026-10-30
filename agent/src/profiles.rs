// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The lab's known-good profiles: for each radio model, one setup per
//! program (VARA, say), the same for every radio of that model the lab has
//! enrolled. The Agent puts a program's profile on an enrolled radio once,
//! as the program starts (Principle X, second exception), and never saves a
//! radio's settings over one on its own (brian's decision, 2026-10-28).
//!
//! Each has a default built into the Agent from `profiles/` beside this
//! crate's `Cargo.toml`. An administrator replaces it from LabLog (Admin,
//! Lab Profiles), from the settings a lab radio has, without a new build
//! (brian, 2026-10-30): the Agent fetches the profiles saved there and keeps
//! a copy in its records folder, so the newest it heard of is used when
//! LabLog can't be reached (Principle V). The built-in one is used only
//! while LabLog has none for that program.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::{SavedProfile, StockSetting};
use crate::rigdef::Definition;

/// The longest copy of LabLog's profiles the Agent reads back.
const MAX_SAVED_BYTES: u64 = 4 * 1024 * 1024;

/// The profile files built into the Agent, by their path under
/// `profiles/`; a test fails if a file there is missing here.
const BUILT_IN: [(&str, &str); 1] = [(
    "icom/ic-7300.toml",
    include_str!("../profiles/icom/ic-7300.toml"),
)];

/// One model's profiles, as its file has them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    maker: String,
    model: String,
    profiles: Vec<Profile>,
}

/// One program's profile for a model.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// The program it is for, as the tray names setups: VARA, FT8, JS8,
    /// fldigi or SSTV.
    pub name: String,
    /// Who set it and where it came from.
    pub source: String,
    /// The settings it puts on, in the order they are written.
    pub settings: Vec<Setting>,
    /// Its version number in LabLog, if an administrator saved it there;
    /// `None` for the one built into the Agent.
    #[serde(skip)]
    pub lablog: Option<u64>,
}

/// One setting of a profile.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    /// The setting's id in the model's definition.
    pub id: String,
    /// The bytes it is set to, in hex, as the radio sends them.
    pub bytes: String,
}

/// The profile for `program` on the radio `maker` `model` built into the
/// Agent, if it has one.
#[must_use]
pub fn find(maker: &str, model: &str, program: &str) -> Option<Profile> {
    BUILT_IN
        .iter()
        .filter_map(|(_, text)| toml::from_str::<ModelFile>(text).ok())
        .find(|file| file.maker == maker && file.model == model)?
        .profiles
        .into_iter()
        .find(|profile| profile.name.eq_ignore_ascii_case(program))
}

/// Whether the lab has any profile for the radio `maker` `model`: only
/// then is there anything to enroll a radio of that model for.
#[must_use]
pub fn any_for(maker: &str, model: &str) -> bool {
    BUILT_IN
        .iter()
        .filter_map(|(_, text)| toml::from_str::<ModelFile>(text).ok())
        .any(|file| file.maker == maker && file.model == model && !file.profiles.is_empty())
}

/// Where the copy of LabLog's profiles for the radio `maker` `model` is
/// kept, in the records folder `records`.
#[must_use]
pub fn saved_path(records: &Path, maker: &str, model: &str) -> PathBuf {
    let name: String = format!("lab-profiles-{maker}-{model}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    records.join(format!("{name}.json"))
}

/// The copy of LabLog's profiles at `path`: none if there is none, or it
/// can't be read.
#[must_use]
pub fn load_saved(path: &Path) -> Vec<SavedProfile> {
    use std::io::Read as _;
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut text = String::new();
    if file
        .take(MAX_SAVED_BYTES + 1)
        .read_to_string(&mut text)
        .is_err()
        || u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_SAVED_BYTES
    {
        return Vec::new();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

/// Keeps `saved`, LabLog's profiles for one model, at `path`, written whole
/// and renamed into place (Principle XIV), replacing the copy before.
///
/// # Errors
///
/// The file could not be written.
pub fn keep_saved(path: &Path, saved: &[SavedProfile]) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("can't keep {}: {error}", path.display());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temp).map_err(fail)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(fail)?;
    drop(file);
    fs::rename(&temp, path).map_err(fail)
}

/// `saved` as a profile the Agent can put on a radio of `definition`'s
/// model: `None` unless it is for that model and every setting is one the
/// Agent may write, with bytes in hex (Principle XII).
#[must_use]
pub fn from_saved(saved: &SavedProfile, definition: &Definition) -> Option<Profile> {
    if saved.maker != definition.maker || saved.model != definition.model {
        return None;
    }
    let mut settings = Vec::new();
    for setting in &saved.settings {
        let summary = definition.settings.iter().find(|s| s.id == setting.id)?;
        if !summary.write || !summary.write_checked {
            return None;
        }
        crate::restore::parse_hex(&setting.bytes)?;
        settings.push(Setting {
            id: setting.id.clone(),
            bytes: setting.bytes.clone(),
        });
    }
    (!settings.is_empty()).then(|| Profile {
        name: saved.program.clone(),
        source: format!(
            "LabLog version #{}, saved by {} at {}",
            saved.id, saved.saved_by, saved.saved_at
        ),
        settings,
        lablog: Some(saved.id),
    })
}

/// The profile for `program` on a radio of `definition`'s model: the one
/// saved in LabLog, as the copy in `records` has it, or else the one built
/// into the Agent.
#[must_use]
pub fn current(records: &Path, definition: &Definition, program: &str) -> Option<Profile> {
    load_saved(&saved_path(records, &definition.maker, &definition.model))
        .iter()
        .filter(|saved| saved.program.eq_ignore_ascii_case(program))
        .find_map(|saved| from_saved(saved, definition))
        .or_else(|| find(&definition.maker, &definition.model, program))
}

/// Whether the lab has any profile for a radio of `definition`'s model,
/// built in or saved in LabLog.
#[must_use]
pub fn any_here(records: &Path, definition: &Definition) -> bool {
    any_for(&definition.maker, &definition.model)
        || load_saved(&saved_path(records, &definition.maker, &definition.model))
            .iter()
            .any(|saved| from_saved(saved, definition).is_some())
}

impl Profile {
    /// Where it is from, in a few words: LabLog's version, or the Agent.
    #[must_use]
    pub fn origin(&self) -> String {
        self.lablog.map_or_else(
            || "built into the Agent".to_owned(),
            |id| format!("saved in LabLog, version #{id}"),
        )
    }

    /// Its settings as LabLog shows them: id, the setting's name on the
    /// radio's menu, bytes, and the value in words.
    #[must_use]
    pub fn shown(&self, definition: &Definition) -> Vec<StockSetting> {
        self.kept(definition)
            .into_iter()
            .map(|(id, label, bytes)| StockSetting {
                id: id.to_owned(),
                label: label.to_owned(),
                bytes: bytes.unwrap_or_default().to_owned(),
                shown: Some(crate::settings_record::shown(definition, id, bytes)).filter(|shown| {
                    !shown.is_empty()
                        && shown.chars().count() <= 200
                        && !shown.chars().any(char::is_control)
                }),
            })
            .collect()
    }

    /// Its settings as a restore plan reads a kept setup: id, the
    /// setting's name on the radio's menu, and bytes.
    #[must_use]
    pub fn kept<'a>(
        &'a self,
        definition: &'a Definition,
    ) -> Vec<(&'a str, &'a str, Option<&'a str>)> {
        self.settings
            .iter()
            .map(|setting| {
                let label = definition
                    .settings
                    .iter()
                    .find(|s| s.id == setting.id)
                    .map_or(setting.id.as_str(), |s| s.label.as_str());
                (setting.id.as_str(), label, Some(setting.bytes.as_str()))
            })
            .collect()
    }

    /// The ids of its settings.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.settings.iter().map(|setting| setting.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BUILT_IN, ModelFile, any_for, any_here, current, find, from_saved, saved_path};
    use crate::engine::{SavedProfile, StockSetting};
    use crate::rigdef;

    fn ic7300() -> rigdef::Definition {
        rigdef::built_in()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.model == "IC-7300")
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"))
    }

    fn saved(id: u64, program: &str, settings: &[(&str, &str)]) -> SavedProfile {
        SavedProfile {
            id,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            program: program.to_owned(),
            settings: settings
                .iter()
                .map(|(id, bytes)| StockSetting {
                    id: (*id).to_owned(),
                    label: (*id).to_owned(),
                    bytes: (*bytes).to_owned(),
                    shown: None,
                })
                .collect(),
            saved_by: "K9KSU".to_owned(),
            saved_at: "2026-10-30T00:40:00Z".to_owned(),
        }
    }

    #[test]
    fn lablogs_saved_profile_takes_the_place_of_the_built_in_one() {
        let definition = ic7300();
        let records =
            std::env::temp_dir().join(format!("lablog-saved-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&records);
        let built_in = current(&records, &definition, "VARA");
        assert_eq!(built_in.as_ref().and_then(|p| p.lablog), None);
        assert!(current(&records, &definition, "SSTV").is_none());

        let path = saved_path(&records, "Icom", "IC-7300");
        assert!(path.ends_with("lab-profiles-icom-ic-7300.json"), "{path:?}");
        let kept = [
            saved(4, "VARA", &[("data_mod", "03"), ("rf_power", "00 69")]),
            saved(5, "SSTV", &[("agc", "01")]),
        ];
        assert!(super::keep_saved(&path, &kept).is_ok());
        assert_eq!(super::load_saved(&path), kept);
        let vara = current(&records, &definition, "vara");
        assert_eq!(vara.as_ref().and_then(|p| p.lablog), Some(4));
        let ids: Vec<&str> = vara.iter().flat_map(super::Profile::ids).collect();
        assert_eq!(ids, ["data_mod", "rf_power"]);
        assert_eq!(
            current(&records, &definition, "SSTV").and_then(|p| p.lablog),
            Some(5)
        );
        // FT8 has none saved, so the built-in one stays.
        assert_eq!(
            current(&records, &definition, "FT8").and_then(|p| p.lablog),
            None
        );
        assert!(any_here(&records, &definition));
        let _ = std::fs::remove_dir_all(&records);
    }

    #[test]
    fn a_saved_profile_with_a_setting_the_agent_may_not_write_is_not_used() {
        let definition = ic7300();
        let unchecked = definition
            .settings
            .iter()
            .find(|s| !(s.write && s.write_checked))
            .map(|s| s.id.clone())
            .unwrap_or_default();
        assert!(from_saved(&saved(1, "VARA", &[("data_mod", "03")]), &definition).is_some());
        assert!(from_saved(&saved(1, "VARA", &[(&unchecked, "00")]), &definition).is_none());
        assert!(from_saved(&saved(1, "VARA", &[("no_such", "00")]), &definition).is_none());
        assert!(from_saved(&saved(1, "VARA", &[("data_mod", "zz")]), &definition).is_none());
        assert!(from_saved(&saved(1, "VARA", &[]), &definition).is_none());
        let mut other = saved(1, "VARA", &[("data_mod", "03")]);
        other.model = "IC-9700".to_owned();
        assert!(from_saved(&other, &definition).is_none());
    }

    #[test]
    fn every_profile_file_is_built_in_and_fits_its_definition() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles");
        let mut on_disk = Vec::new();
        for maker in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            for file in std::fs::read_dir(maker.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                on_disk.push(format!(
                    "{}/{}",
                    maker.file_name().to_string_lossy(),
                    file.file_name().to_string_lossy()
                ));
            }
        }
        on_disk.sort();
        let mut built_in: Vec<String> = BUILT_IN
            .iter()
            .map(|(path, _)| (*path).to_owned())
            .collect();
        built_in.sort();
        assert_eq!(
            on_disk, built_in,
            "add each file in profiles/ to BUILT_IN in profiles.rs"
        );
        let definitions = rigdef::built_in().unwrap_or_default();
        for (path, text) in BUILT_IN {
            let file: Result<ModelFile, _> = toml::from_str(text);
            assert!(file.is_ok(), "{path}: {file:?}");
            let Ok(file) = file else { continue };
            let definition = definitions
                .iter()
                .find(|d| d.maker == file.maker && d.model == file.model);
            assert!(
                definition.is_some(),
                "{path}: no definition for {}",
                file.model
            );
            let Some(definition) = definition else {
                continue;
            };
            for profile in &file.profiles {
                for setting in &profile.settings {
                    let summary = definition.settings.iter().find(|s| s.id == setting.id);
                    assert!(
                        summary.is_some_and(|s| s.write && s.write_checked),
                        "{path} {}: {} isn't a setting the Agent may write",
                        profile.name,
                        setting.id
                    );
                    assert!(
                        crate::restore::parse_hex(&setting.bytes).is_some(),
                        "{path} {}: {} has bytes that aren't hex",
                        profile.name,
                        setting.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_ic_7300_vara_profile_is_data_usb_fil1_at_2_3_khz_and_40_percent() {
        assert!(any_for("Icom", "IC-7300"));
        assert!(!any_for("Icom", "IC-705"));
        assert_eq!(find("Icom", "IC-7300", "SSTV"), None);
        let vara = find("Icom", "IC-7300", "VARA");
        let bytes: Vec<(String, String)> = vara
            .map(|vara| vara.settings.into_iter().map(|s| (s.id, s.bytes)).collect())
            .unwrap_or_default();
        let wanted = [
            ("mode_and_filter", "01 01"),
            ("data_mode_and_filter", "01 01"),
            ("filter_width", "27"),
            ("usb_mod_level", "01 02"),
            ("data_mod", "03"),
        ];
        assert_eq!(
            bytes,
            wanted
                .iter()
                .map(|(id, b)| ((*id).to_owned(), (*b).to_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_ic_7300_fldigi_profile_is_the_vara_one_with_agc_fast_and_no_nb_nr_or_notch() {
        let fldigi = find("Icom", "IC-7300", "fldigi");
        let bytes: Vec<(String, String)> = fldigi
            .map(|fldigi| {
                fldigi
                    .settings
                    .into_iter()
                    .map(|s| (s.id, s.bytes))
                    .collect()
            })
            .unwrap_or_default();
        let wanted = [
            ("mode_and_filter", "01 01"),
            ("data_mode_and_filter", "01 01"),
            ("filter_width", "27"),
            ("agc", "01"),
            ("noise_blanker", "00"),
            ("noise_reduction", "00"),
            ("auto_notch", "00"),
            ("manual_notch", "00"),
            ("usb_mod_level", "01 02"),
            ("data_mod", "03"),
        ];
        assert_eq!(
            bytes,
            wanted
                .iter()
                .map(|(id, b)| ((*id).to_owned(), (*b).to_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_ic_7300_ft8_and_js8_profiles_are_data_usb_fil1_at_3_khz() {
        for program in ["FT8", "JS8"] {
            let profile = find("Icom", "IC-7300", program);
            let bytes: Vec<(String, String)> = profile
                .map(|profile| {
                    profile
                        .settings
                        .into_iter()
                        .map(|s| (s.id, s.bytes))
                        .collect()
                })
                .unwrap_or_default();
            let wanted = [
                ("mode_and_filter", "01 01"),
                ("data_mode_and_filter", "01 01"),
                ("filter_width", "34"),
                ("usb_mod_level", "01 02"),
                ("data_mod", "03"),
            ];
            assert_eq!(
                bytes,
                wanted
                    .iter()
                    .map(|(id, b)| ((*id).to_owned(), (*b).to_owned()))
                    .collect::<Vec<_>>(),
                "{program}"
            );
        }
    }
}
