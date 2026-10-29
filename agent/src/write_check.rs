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

use std::time::SystemTime;

use serde::Serialize;

use crate::civ;
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
    mut progress: impl FnMut(usize, usize),
) -> Report {
    let definition = radio.definition().clone();
    let changeable: Vec<_> = definition.settings.iter().filter(|s| s.write).collect();
    let mut settings = Vec::with_capacity(changeable.len());
    for (done, setting) in changeable.iter().enumerate() {
        let checked = match radio.write_back(setting) {
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

/// The file a report is kept in, under `dir`.
#[must_use]
pub fn path_for(dir: &std::path::Path, report: &Report) -> std::path::PathBuf {
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

    use super::{check, path_for};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
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
}
