// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent check-writes`: finds out which of a radio's settings the
//! Agent can set, without changing any of them.
//!
//! For each setting the definition says the Agent may change, it reads the
//! setting, writes back exactly the bytes it just read, and reads it again.
//! The radio ends as it began: nothing remembered is written (Principle X).
//! A setting whose own value the radio took and still has is one whose set
//! command works; the report lists them, and the ones that didn't, so they
//! can be marked in the rig definition from evidence (Principle XVII).
//!
//! The report is kept as a JSON file beside the settings records, written
//! whole and never overwritten.

use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::civ;
use crate::civ_radio::WriteBack;
use crate::rig::{Exposes, RadioError};
use crate::rigdef::{Definition, SettingSummary};
use crate::serial_radio::SerialRadio;
use crate::settings_record;

/// What became of one setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Checked {
    /// The setting's id.
    pub id: String,
    /// Its name on the radio's menu.
    pub label: String,
    /// The bytes it had, and was sent back, if it could be read.
    pub bytes: Option<String>,
    /// `works`, `refused` (the radio said no), `changed` (it took it but
    /// has something else now), or `not read`.
    pub outcome: &'static str,
    /// What went wrong, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// The report of one check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The radio's maker.
    pub maker: String,
    /// The radio's model.
    pub model: String,
    /// Its serial number, if known.
    pub serial: Option<String>,
    /// When the check began, in UTC.
    pub started_utc: String,
    /// The computer it ran on.
    pub computer: String,
    /// Every setting the Agent may change, in the definition's order.
    pub settings: Vec<Checked>,
}

impl Report {
    /// How many settings' set commands work.
    #[must_use]
    pub fn working(&self) -> usize {
        self.settings
            .iter()
            .filter(|c| c.outcome == "works")
            .count()
    }

    /// The report in lines to show: a count, then every one that didn't
    /// work.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Wrote back {} settings' own values: {} worked.",
            self.settings.len(),
            self.working()
        )];
        lines.extend(
            self.settings
                .iter()
                .filter(|c| c.outcome != "works")
                .map(|c| {
                    format!(
                        "  {} ({}): {}{}",
                        c.label,
                        c.id,
                        c.outcome,
                        c.why
                            .as_deref()
                            .map(|why| format!(", {why}"))
                            .unwrap_or_default()
                    )
                }),
        );
        lines
    }
}

/// Writes each changeable setting's own value back to it, and says which
/// the radio took. `progress` is told how many are done, out of how many.
pub fn check(
    radio: &mut dyn SerialRadio,
    serial: Option<&str>,
    now: SystemTime,
    computer: &str,
    progress: impl FnMut(usize, usize),
) -> Report {
    let definition = radio.definition().clone();
    check_with(
        &definition,
        |setting| radio.write_back(setting),
        serial,
        now,
        computer,
        progress,
    )
}

/// Writes one setting's own value back to it, through the settings side
/// of a radio the station already holds: read, its own bytes written back
/// with the command that sets it, and read again, as `write_back` does.
///
/// # Errors
///
/// The setting couldn't be read first.
pub fn write_back_exposed(
    radio: &mut dyn Exposes,
    setting: &SettingSummary,
) -> Result<WriteBack, RadioError> {
    let before = radio.read_exposed(setting)?;
    // Only to write back what was just read: the check is what finds out
    // whether the set command works, and the radio ends as it began.
    let mut trying = setting.clone();
    trying.write_checked = true;
    let written = radio
        .write_exposed(&trying, &before)
        .map_err(|error| error.to_string());
    let after = radio
        .read_exposed(setting)
        .map_err(|error| error.to_string());
    Ok(WriteBack {
        before,
        written,
        after,
    })
}

/// The check of each setting of `definition` the Agent may change, each
/// written back by `write_back`.
pub fn check_with(
    definition: &Definition,
    mut write_back: impl FnMut(&SettingSummary) -> Result<WriteBack, RadioError>,
    serial: Option<&str>,
    now: SystemTime,
    computer: &str,
    mut progress: impl FnMut(usize, usize),
) -> Report {
    let changeable: Vec<_> = definition.settings.iter().filter(|s| s.write).collect();
    let mut settings = Vec::with_capacity(changeable.len());
    for (done, setting) in changeable.iter().enumerate() {
        let checked = match write_back(setting) {
            Err(error) => Checked {
                id: setting.id.clone(),
                label: setting.label.clone(),
                bytes: None,
                outcome: "not read",
                why: Some(error.to_string()),
            },
            Ok(back) => {
                let works = back.works();
                let (outcome, why) = match (&back.written, &back.after) {
                    _ if works => ("works", None),
                    (Err(why), _) => ("refused", Some(why.clone())),
                    (Ok(()), Ok(after)) => (
                        "changed",
                        Some(format!("the radio has {} now", civ::hex(after))),
                    ),
                    (Ok(()), Err(why)) => ("changed", Some(format!("not read after: {why}"))),
                };
                Checked {
                    id: setting.id.clone(),
                    label: setting.label.clone(),
                    bytes: Some(civ::hex(&back.before)),
                    outcome,
                    why,
                }
            }
        };
        settings.push(checked);
        progress(done + 1, changeable.len());
    }
    Report {
        maker: definition.maker.clone(),
        model: definition.model.clone(),
        serial: serial.map(str::to_owned),
        started_utc: settings_record::utc_text(now),
        computer: computer.to_owned(),
        settings,
    }
}

/// A report as read back from its file: only what says which settings'
/// set commands worked.
#[derive(Deserialize)]
struct KeptReport {
    maker: String,
    model: String,
    started_utc: String,
    settings: Vec<KeptChecked>,
}

#[derive(Deserialize)]
struct KeptChecked {
    id: String,
    outcome: String,
}

/// The ids of the settings whose set command worked in the newest check of
/// a radio of the model `maker` `model` kept in `dir` on this computer, if
/// there is one.
#[must_use]
pub fn worked_here(dir: &Path, maker: &str, model: &str) -> Option<Vec<String>> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("write-check-"))
        })
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<KeptReport>(&text).ok())
        .filter(|report| report.maker == maker && report.model == model)
        .max_by(|a, b| a.started_utc.cmp(&b.started_utc))
        .map(|report| {
            report
                .settings
                .into_iter()
                .filter(|checked| checked.outcome == "works")
                .map(|checked| checked.id)
                .collect()
        })
}

/// `definition` with the settings a check on this computer found work
/// counted as checked on a real radio (Principle XVII: from that check's
/// report, kept in `dir`). A setting the Agent never changes stays so.
#[must_use]
pub fn with_checks_here(definition: &Definition, dir: &Path) -> Definition {
    let mut checked = definition.clone();
    if let Some(worked) = worked_here(dir, &definition.maker, &definition.model) {
        for setting in &mut checked.settings {
            if setting.write && worked.contains(&setting.id) {
                setting.write_checked = true;
            }
        }
    }
    checked
}

/// Whether nothing of `definition` is checked on a real radio yet, though
/// some of it may be changed: a check would find out what can be set.
#[must_use]
pub fn unchecked(definition: &Definition) -> bool {
    definition.settings.iter().any(|s| s.write)
        && !definition.settings.iter().any(|s| s.write_checked)
}

/// The file a report is kept in, under `dir`.
#[must_use]
pub fn path_for(dir: &Path, report: &Report) -> std::path::PathBuf {
    let name: String = format!(
        "write-check-{}-{}-{}-{}",
        report.maker,
        report.model,
        report.serial.as_deref().unwrap_or("unknown"),
        report.started_utc
    )
    .chars()
    .map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        }
    })
    .collect();
    dir.join(format!("{name}.json"))
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::{
        check, check_with, path_for, unchecked, with_checks_here, worked_here, write_back_exposed,
    };
    use crate::cat_pretend::{PretendCat, PretendCatPort};
    use crate::cat_radio::CatRadio;
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::rig::Exposes;
    use crate::rigdef;
    use crate::settings_record;

    fn radio() -> CivRadio<PretendPort> {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let port = PretendPort::new(PretendCiv::new(&definition), 115_200);
        CivRadio::find(port, &[definition]).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn writes_back_every_changeable_setting_and_changes_nothing() {
        let mut radio = radio();
        let before = settings_record::read(&mut radio, None, UNIX_EPOCH, "FL1");
        let changeable = radio
            .definition()
            .settings
            .iter()
            .filter(|s| s.write)
            .count();
        let mut seen = 0;
        let report = check(
            &mut radio,
            Some("IC-7300_02065962"),
            UNIX_EPOCH,
            "FL1",
            |done, of| {
                assert_eq!(of, changeable);
                seen = done;
            },
        );
        assert_eq!(seen, changeable, "told of each one");
        assert_eq!(report.settings.len(), changeable);
        assert!(
            report.settings.iter().all(|c| c.id != "civ_transceive"),
            "settings the Agent never changes are left out"
        );
        assert_eq!(report.working(), changeable, "{:?}", report.lines());
        assert_eq!(report.lines().len(), 1);
        let after = settings_record::read(&mut radio, None, UNIX_EPOCH, "FL1");
        assert_eq!(before, after, "the radio is as it was");
        assert_eq!(
            path_for(std::path::Path::new("records"), &report),
            std::path::Path::new(
                "records/write-check-icom-ic-7300-ic-7300-02065962-1970-01-01t00-00-00z.json"
            )
        );
    }

    /// A built-in definition by model.
    fn built_in(model: &str) -> rigdef::Definition {
        rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == model))
            .unwrap_or_else(|| unreachable!("the {model} is built in"))
    }

    /// The first-time check as a station runs it, through the settings
    /// side of the radio it holds, on `radio`: nothing of the model checked
    /// before, nothing changed by it, and the settings that worked counted
    /// as checked here after, so they can be written.
    fn checks_a_new_model_here(radio: &mut dyn Exposes, name: &str) {
        let definition = radio.definition().clone();
        assert!(unchecked(&definition), "{name}: nothing checked yet");
        let first = definition
            .settings
            .iter()
            .find(|s| s.write)
            .unwrap_or_else(|| unreachable!("{name} has settings to change"))
            .clone();
        let bytes = radio
            .read_exposed(&first)
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert!(
            radio.write_exposed(&first, &bytes).is_err(),
            "{name}: unchecked, it isn't written"
        );
        let before = settings_record::read(radio, None, UNIX_EPOCH, "FL1");
        let report = check_with(
            &definition,
            |setting| write_back_exposed(radio, setting),
            Some("NEW"),
            UNIX_EPOCH,
            "FL1",
            |_, _| {},
        );
        assert!(report.working() > 0, "{name}: {:?}", report.lines());
        let after = settings_record::read(radio, None, UNIX_EPOCH, "FL1");
        assert_eq!(before, after, "{name}: the radio is as it was");

        let dir =
            std::env::temp_dir().join(format!("lablog-write-check-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            worked_here(&dir, &definition.maker, &definition.model),
            None
        );
        assert_eq!(
            with_checks_here(&definition, &dir),
            definition,
            "no check, no change"
        );
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        settings_record::save(&path_for(&dir, &report), &report)
            .unwrap_or_else(|error| unreachable!("{error}"));
        let worked = worked_here(&dir, &definition.maker, &definition.model).unwrap_or_default();
        assert_eq!(worked.len(), report.working());
        assert_eq!(worked_here(&dir, &definition.maker, "another"), None);
        let checked = with_checks_here(&definition, &dir);
        assert!(!unchecked(&checked));
        for setting in &checked.settings {
            assert_eq!(
                setting.write_checked,
                setting.write && worked.contains(&setting.id),
                "{name}: {}",
                setting.id
            );
        }
        let first = checked
            .settings
            .iter()
            .find(|s| s.id == first.id)
            .unwrap_or_else(|| unreachable!("it is there"));
        if first.write_checked {
            assert_eq!(
                radio.write_exposed(first, &bytes),
                Ok(()),
                "{name}: now it is"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_icom_model_is_checked_here_and_then_can_be_set() {
        let definition = built_in("IC-9700");
        let port = PretendPort::new(PretendCiv::new(&definition), 115_200);
        let mut radio =
            CivRadio::find(port, &[definition]).unwrap_or_else(|error| unreachable!("{error}"));
        checks_a_new_model_here(&mut radio, "ic9700");
    }

    #[test]
    fn a_new_yaesu_model_is_checked_here_and_then_can_be_set() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let port = PretendCatPort::new(PretendCat::new(&built_in("FTDX10")), 38_400);
        let mut radio =
            CatRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        checks_a_new_model_here(&mut radio, "ftdx10");
    }
}
