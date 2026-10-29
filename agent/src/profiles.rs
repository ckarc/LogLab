// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The lab's known-good profiles: for each radio model, one setup per
//! program (VARA, say), the same for every radio of that model the lab has
//! enrolled. They are built into the Agent from `profiles/` beside this
//! crate's `Cargo.toml`, so they are locked: only a new build replaces them,
//! and the Agent never saves a radio's settings over them (brian's
//! decision, 2026-10-28). The Agent puts a program's profile on an enrolled
//! radio once, as the program starts (Principle X, second exception).

use serde::Deserialize;

use crate::rigdef::Definition;

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

/// The profile for `program` on the radio `maker` `model`, if the lab has
/// one.
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

impl Profile {
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

    use super::{BUILT_IN, ModelFile, any_for, find};
    use crate::rigdef;

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
