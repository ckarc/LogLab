// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Putting a kept setup back on the radio.
//!
//! The one time the Agent writes a remembered value over the radio's own
//! (Principle X): only when the operator asks for a setup by name, only
//! after the operator has seen what would change and confirmed it, and only
//! settings the radio's definition says may be changed and that a real
//! radio took when written back (`check-writes`). Each setting is read again just before it is written, and
//! again after: what the radio then says is the truth, whatever was asked.
//! Every restore is recorded (what was changed, from what, to what, and
//! when, in UTC) in a file written whole and never overwritten.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::civ;
use crate::rig::Exposes;
use crate::rigdef::{Definition, SettingSummary};
use crate::settings_record::{self, Record};

/// One setting a restore would put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Put {
    /// The setting, as the definition has it.
    pub setting: SettingSummary,
    /// The bytes the setup has for it.
    pub to: Vec<u8>,
    /// What the radio has now, in plain words.
    pub now: String,
    /// What the setup has, in plain words.
    pub setup: String,
}

/// A setting that differs from the setup but won't be put back, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptBack {
    /// Its name on the radio's menu.
    pub label: String,
    /// Why not, in plain words.
    pub why: String,
}

/// What a restore of one setup would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The setup's name.
    pub setup: String,
    /// The settings it would put back.
    pub put: Vec<Put>,
    /// The settings that differ but would be left as they are.
    pub kept_back: Vec<KeptBack>,
}

impl Plan {
    /// The plan in lines to show the operator before they confirm.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.put.is_empty() && self.kept_back.is_empty() {
            lines.push(format!(
                "the radio already matches setup {}; nothing to put back.",
                self.setup
            ));
            return lines;
        }
        if !self.put.is_empty() {
            lines.push(format!(
                "Putting setup {} back would change {} setting{}:",
                self.setup,
                self.put.len(),
                if self.put.len() == 1 { "" } else { "s" }
            ));
            lines.extend(
                self.put
                    .iter()
                    .map(|put| format!("  {}: {} -> {}", put.setting.label, put.now, put.setup)),
            );
        }
        if !self.kept_back.is_empty() {
            lines.push(format!(
                "{} that differ{} would be left as {} on the radio:",
                self.kept_back.len(),
                if self.kept_back.len() == 1 { "s" } else { "" },
                if self.kept_back.len() == 1 {
                    "it is"
                } else {
                    "they are"
                }
            ));
            lines.extend(
                self.kept_back
                    .iter()
                    .map(|kept| format!("  {}: {}", kept.label, kept.why)),
            );
        }
        lines
    }
}

/// What putting a setup back would do, from the setup as kept (each
/// setting's id, label and bytes) and the radio's settings now.
#[must_use]
pub fn plan<'a>(
    setup: &str,
    kept: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
    now: &Record,
    definition: &Definition,
) -> Plan {
    let kept: Vec<(&str, &str, Option<&str>)> = kept.into_iter().collect();
    let selection = selection(&kept, now, definition);
    let mut put = Vec::new();
    let mut kept_back = Vec::new();
    for &(id, label, bytes) in &kept {
        let Some(entry) = now.settings.iter().find(|e| e.id == id) else {
            continue;
        };
        let setting = definition.settings.iter().find(|s| s.id == id);
        let per_selection = setting.is_some_and(|s| s.per_selection);
        // A setting of the selected mode or filter is read again once the
        // mode and filter are back, so it is put back even if it matches
        // now: what matches now belongs to another filter.
        let moved = per_selection && selection == Selection::PutBack;
        // A clock always differs and is never put back: not worth a line.
        if (entry.bytes.as_deref() == bytes && !moved) || setting.is_some_and(|s| s.moves) {
            continue;
        }
        let back = |why: &str| KeptBack {
            label: label.to_owned(),
            why: why.to_owned(),
        };
        let Some(bytes) = bytes else {
            kept_back.push(back("the setup couldn't read it"));
            continue;
        };
        let Some(setting) = definition.settings.iter().find(|s| s.id == id) else {
            kept_back.push(back("this radio's definition doesn't list it"));
            continue;
        };
        if !setting.write {
            kept_back.push(back("the Agent never changes it"));
            continue;
        }
        if !setting.write_checked {
            kept_back.push(back(&format!(
                "not checked on a real radio yet, so the Agent doesn't write it; \
                 set it to {} by hand",
                settings_record::shown(definition, id, Some(bytes))
            )));
            continue;
        }
        if per_selection {
            let why = match selection {
                Selection::Unknown => Some(
                    "the setup doesn't say which mode and filter it was kept on, so it would \
                     land on whichever one the radio is on now; keep the setup again",
                ),
                Selection::Stuck => Some(
                    "the radio is on a different mode or filter than the setup, and the Agent \
                     can't put that back, so it would land on the wrong one",
                ),
                Selection::Same | Selection::PutBack => None,
            };
            if let Some(why) = why {
                kept_back.push(back(why));
                continue;
            }
        }
        if entry.bytes.is_none() {
            kept_back.push(back("the radio wouldn't say what it has now"));
            continue;
        }
        let Some(to) = parse_hex(bytes) else {
            kept_back.push(back("the setup's bytes for it aren't bytes"));
            continue;
        };
        put.push(Put {
            setting: setting.clone(),
            to,
            now: if moved {
                "read again once the mode and filter are back".to_owned()
            } else {
                entry.text()
            },
            setup: settings_record::shown(definition, id, Some(bytes)),
        });
    }
    // What chooses the mode and filter goes first, in the definition's
    // order; everything else after.
    put.sort_by_key(|put| !put.setting.selects);
    Plan {
        setup: setup.to_owned(),
        put,
        kept_back,
    }
}

/// Whether the radio is on the setup's mode and filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    /// It is, or the definition has nothing that chooses them.
    Same,
    /// It isn't, and the Agent can put them back first.
    PutBack,
    /// It isn't, and the Agent can't put them back.
    Stuck,
    /// The setup doesn't say.
    Unknown,
}

fn selection(
    kept: &[(&str, &str, Option<&str>)],
    now: &Record,
    definition: &Definition,
) -> Selection {
    let mut result = Selection::Same;
    for setting in definition.settings.iter().filter(|s| s.selects) {
        let Some(bytes) = kept
            .iter()
            .find(|(id, _, _)| *id == setting.id)
            .and_then(|(_, _, bytes)| *bytes)
        else {
            return Selection::Unknown;
        };
        let now = now
            .settings
            .iter()
            .find(|e| e.id == setting.id)
            .and_then(|e| e.bytes.as_deref());
        if now == Some(bytes) {
            continue;
        }
        if setting.write && setting.write_checked && now.is_some() && parse_hex(bytes).is_some() {
            result = Selection::PutBack;
        } else {
            return Selection::Stuck;
        }
    }
    result
}

/// Bytes written as hex pairs, `00 20`.
fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let bytes: Option<Vec<u8>> = text
        .split(' ')
        .map(|pair| {
            (pair.len() == 2)
                .then(|| u8::from_str_radix(pair, 16).ok())
                .flatten()
        })
        .collect();
    bytes.filter(|bytes| !bytes.is_empty())
}

/// What became of one setting in a restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Done {
    /// The setting's id.
    pub id: String,
    /// Its name on the radio's menu.
    pub label: String,
    /// The bytes the radio had just before, if it said.
    pub from: Option<String>,
    /// The bytes the setup has.
    pub to: String,
    /// The bytes the radio has after, if it said: the truth (Principle X).
    pub after: Option<String>,
    /// `put back`, `refused` (the radio said no), `differs` (the radio
    /// took it but has something else now), or `not written` (the mode and
    /// filter it belongs to didn't go back).
    pub outcome: &'static str,
    /// What went wrong, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// The record of one restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Restore {
    /// The record format's version.
    pub format: u32,
    /// The radio's maker.
    pub maker: String,
    /// The radio's model.
    pub model: String,
    /// The radio's serial number.
    pub serial: String,
    /// The setup put back.
    pub setup: String,
    /// When the restore began, in UTC.
    pub started_utc: String,
    /// The computer it was done on.
    pub computer: String,
    /// Every setting written, in the order written.
    pub changes: Vec<Done>,
}

impl Restore {
    /// The restore in lines to show the operator.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let back = self
            .changes
            .iter()
            .filter(|done| done.outcome == "put back")
            .count();
        let mut lines = vec![format!(
            "setup {}: put back {back} of {} setting{}.",
            self.setup,
            self.changes.len(),
            if self.changes.len() == 1 { "" } else { "s" }
        )];
        lines.extend(
            self.changes
                .iter()
                .filter(|done| done.outcome != "put back")
                .map(|done| {
                    format!(
                        "  {}: {} ({})",
                        done.label,
                        done.outcome,
                        done.why.as_deref().unwrap_or("")
                    )
                }),
        );
        lines
    }
}

/// Puts back what `plan` says, once the operator has confirmed it. Reads
/// each setting just before writing it, so the record says what the radio
/// really had, and again after, so it says what the radio really took.
pub fn apply(
    radio: &mut dyn Exposes,
    plan: &Plan,
    serial: &str,
    now: SystemTime,
    computer: &str,
) -> Restore {
    let definition = radio.definition().clone();
    let mut changes: Vec<Done> = Vec::new();
    for put in &plan.put {
        let selector_failed = changes
            .iter()
            .zip(&plan.put)
            .any(|(done, put)| put.setting.selects && done.outcome != "put back");
        if put.setting.per_selection && selector_failed {
            changes.push(Done {
                id: put.setting.id.clone(),
                label: put.setting.label.clone(),
                from: None,
                to: civ::hex(&put.to),
                after: None,
                outcome: "not written",
                why: Some(
                    "the mode and filter didn't go back, so it would land on the wrong one"
                        .to_owned(),
                ),
            });
        } else if put.setting.selects {
            // The mode and filter must be right before anything that
            // belongs to them, so a second try is made at once.
            let first = put_one(radio, put);
            changes.push(if first.outcome == "differs" {
                second_try(radio, put, first)
            } else {
                first
            });
        } else {
            changes.push(put_one(radio, put));
        }
    }
    // One setting can depend on another, as the IC-7300's Twin PBT did on
    // the filter width at FieldLab: written first, it lands on a different
    // step. So each one the radio took but that came out different is
    // written once more, after all the others.
    for (put, done) in plan.put.iter().zip(changes.iter_mut()) {
        if done.outcome == "differs" && !put.setting.selects {
            *done = second_try(radio, put, done.clone());
        }
    }
    Restore {
        format: 1,
        maker: definition.maker.clone(),
        model: definition.model.clone(),
        serial: serial.to_owned(),
        setup: plan.setup.clone(),
        started_utc: settings_record::utc_text(now),
        computer: computer.to_owned(),
        changes,
    }
}

/// Writes a setting that came out different once more. Keeps the first
/// try's record if the second doesn't work either.
fn second_try(radio: &mut dyn Exposes, put: &Put, first: Done) -> Done {
    let again = put_one(radio, put);
    if again.outcome == "put back" {
        Done {
            from: first.from,
            why: Some("on a second try".to_owned()),
            ..again
        }
    } else {
        first
    }
}

/// Writes one setting and reads it back.
fn put_one(radio: &mut dyn Exposes, put: &Put) -> Done {
    let from = radio.read_exposed(&put.setting).ok();
    let written = radio.write_exposed(&put.setting, &put.to);
    let after = radio.read_exposed(&put.setting).ok();
    let (outcome, why) = match (&written, &after) {
        (Err(error), _) => ("refused", Some(error.to_string())),
        (Ok(()), Some(after)) if *after == put.to => ("put back", None),
        (Ok(()), Some(after)) => (
            "differs",
            Some(format!("the radio has {} now", civ::hex(after))),
        ),
        (Ok(()), None) => (
            "differs",
            Some("the radio took it but wouldn't say what it has now".to_owned()),
        ),
    };
    Done {
        id: put.setting.id.clone(),
        label: put.setting.label.clone(),
        from: from.as_deref().map(civ::hex),
        to: civ::hex(&put.to),
        after: after.as_deref().map(civ::hex),
        outcome,
        why,
    }
}

/// The file a restore is recorded in, under `dir`: one per restore, by the
/// radio and when it began.
#[must_use]
pub fn path_for(dir: &Path, restore: &Restore) -> PathBuf {
    let name: String = format!(
        "restore-{}-{}-{}-{}",
        restore.maker, restore.model, restore.serial, restore.started_utc
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
    use std::time::{Duration, UNIX_EPOCH};

    use super::{apply, parse_hex, path_for};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::rig::Exposes;
    use crate::rigdef::{self, Definition, SettingSummary};
    use crate::settings_record::{self, Record};

    const SERIAL: &str = "IC-7300_02065962";

    /// The IC-7300's definition, with only the `checked` settings marked as
    /// taken by a real radio.
    fn ic_7300(checked: &[&str]) -> Definition {
        let mut definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        for setting in &mut definition.settings {
            setting.write_checked = checked.contains(&setting.id.as_str());
        }
        definition
    }

    fn radio(definition: &Definition) -> CivRadio<PretendPort> {
        let port = PretendPort::new(PretendCiv::new(definition), 115_200);
        CivRadio::find(port, std::slice::from_ref(definition))
            .unwrap_or_else(|error| unreachable!("{error}"))
    }

    fn read(radio: &mut CivRadio<PretendPort>) -> Record {
        settings_record::read(radio, Some(SERIAL), UNIX_EPOCH, "FL1")
    }

    fn kept(record: &Record) -> Vec<(&str, &str, Option<&str>)> {
        record
            .settings
            .iter()
            .map(|e| (e.id.as_str(), e.label.as_str(), e.bytes.as_deref()))
            .collect()
    }

    /// A menu item with named values the Agent may change, and a second
    /// value for it.
    fn named(definition: &Definition) -> (Vec<u8>, u8) {
        let setting = definition
            .settings
            .iter()
            .find(|s| {
                s.write && s.item().is_some() && s.values.as_ref().is_some_and(|v| v.len() > 1)
            })
            .unwrap_or_else(|| unreachable!("the IC-7300 has one"));
        let code = setting
            .values
            .iter()
            .flatten()
            .map(|(_, code)| *code)
            .max()
            .unwrap_or_default();
        (setting.read.clone(), code)
    }

    #[test]
    fn puts_back_only_verified_settings_the_agent_may_change() {
        let att_id = id_of(&ic_7300(&[]), |s| s.read == [0x11]);
        let definition = ic_7300(&[&att_id]);
        let (read_command, code) = named(&definition);
        let mut radio = radio(&definition);
        let setup = read(&mut radio);
        assert!(radio.port_mut().radio().set(&[0x11], 0x0020), "ATT on");
        assert!(radio.port_mut().radio().set(&read_command, u16::from(code)));
        let now = read(&mut radio);
        let plan = super::plan("FT8", kept(&setup), &now, &definition);
        assert_eq!(plan.put.len(), 1, "{plan:?}");
        assert_eq!(plan.put[0].setting.id, att_id);
        assert_eq!(plan.put[0].to, [0x00]);
        assert_eq!(plan.kept_back.len(), 1, "{plan:?}");
        assert!(plan.kept_back[0].why.contains("not checked"), "{plan:?}");
        assert!(plan.lines()[0].starts_with("Putting setup FT8 back would change 1 setting:"));

        let done = apply(
            &mut radio,
            &plan,
            SERIAL,
            UNIX_EPOCH + Duration::from_mins(1),
            "FL2",
        );
        assert_eq!(done.changes.len(), 1);
        let change = &done.changes[0];
        assert_eq!(change.outcome, "put back", "{change:?}");
        assert_eq!(
            (
                change.from.as_deref(),
                change.to.as_str(),
                change.after.as_deref()
            ),
            (Some("00 20"), "00", Some("00"))
        );
        assert_eq!(done.started_utc, "1970-01-01T00:01:00Z");
        assert_eq!(done.lines()[0], "setup FT8: put back 1 of 1 setting.");
        let again = super::plan("FT8", kept(&setup), &read(&mut radio), &definition);
        assert!(
            again.put.is_empty(),
            "only the unverified one still differs"
        );
        assert_eq!(again.kept_back.len(), 1);
    }

    /// The id of the first setting that matches.
    fn id_of(definition: &Definition, which: impl Fn(&SettingSummary) -> bool) -> String {
        definition
            .settings
            .iter()
            .find(|s| which(s))
            .map_or_else(|| unreachable!("the IC-7300 has one"), |s| s.id.clone())
    }

    #[test]
    fn nothing_unverified_is_ever_written() {
        let definition = ic_7300(&[]);
        let mut radio = radio(&definition);
        let setup = read(&mut radio);
        assert!(radio.port_mut().radio().set(&[0x11], 0x0020));
        let now = read(&mut radio);
        let plan = super::plan("FT8", kept(&setup), &now, &definition);
        assert!(plan.put.is_empty(), "{plan:?}");
        let att = definition
            .settings
            .iter()
            .find(|s| s.read == [0x11])
            .unwrap_or_else(|| unreachable!());
        assert!(
            radio.write_exposed(att, &[0x00]).is_err(),
            "the driver refuses too"
        );
        assert_eq!(read(&mut radio), now, "the radio is as it was");
    }

    #[test]
    fn says_when_the_radio_refuses() {
        let level = id_of(&ic_7300(&[]), |s| {
            s.write && s.item().is_none() && s.range.is_some_and(|[_, high]| high > 99)
        });
        let definition = ic_7300(&[&level]);
        let mut radio = radio(&definition);
        let now = read(&mut radio);
        let before = now
            .settings
            .iter()
            .find(|e| e.id == level)
            .and_then(|e| e.bytes.clone());
        let setup = vec![(level.as_str(), "A level", Some("AA AA"))];
        let plan = super::plan("Odd", setup, &now, &definition);
        assert_eq!(plan.put.len(), 1, "{plan:?}");
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert_eq!(done.changes[0].outcome, "refused", "{done:?}");
        assert_eq!(
            done.changes[0].after, before,
            "the radio's own value stands"
        );
    }

    #[test]
    fn a_matching_radio_needs_nothing() {
        let definition = ic_7300(&[]);
        let mut radio = radio(&definition);
        let now = read(&mut radio);
        let plan = super::plan("FT8", kept(&now), &now, &definition);
        assert!(plan.put.is_empty() && plan.kept_back.is_empty());
        assert!(plan.lines()[0].contains("already matches"));
    }

    #[test]
    fn reads_hex_strictly_and_names_each_restore_its_own_file() {
        assert_eq!(parse_hex("00 20"), Some(vec![0x00, 0x20]));
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("0020"), None);
        assert_eq!(parse_hex("zz"), None);
        let definition = ic_7300(&[]);
        let mut radio = radio(&definition);
        let now = read(&mut radio);
        let plan = super::plan("FT8", kept(&now), &now, &definition);
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert_eq!(
            path_for(std::path::Path::new("records"), &done),
            std::path::Path::new(
                "records/restore-icom-ic-7300-ic-7300-02065962-1970-01-01t00-00-00z.json"
            )
        );
    }

    /// A radio whose filter width and PBT belong to the selected mode and
    /// filter, as the IC-7300's did in the field lab.
    struct Slots {
        definition: Definition,
        mode: Vec<u8>,
        widths: std::collections::HashMap<Vec<u8>, Vec<u8>>,
        pbt: Vec<u8>,
        refuse_mode: bool,
        /// PBT lands one step off this many more times.
        pbt_misses: u8,
    }

    impl Exposes for Slots {
        fn definition(&self) -> &Definition {
            &self.definition
        }

        fn read_exposed(
            &mut self,
            setting: &SettingSummary,
        ) -> Result<Vec<u8>, crate::rig::RadioError> {
            Ok(match setting.id.as_str() {
                "mode_and_filter" => self.mode.clone(),
                "filter_width" => self.widths.get(&self.mode).cloned().unwrap_or_default(),
                "pbt_inner" => self.pbt.clone(),
                _ => vec![0x00],
            })
        }

        fn write_exposed(
            &mut self,
            setting: &SettingSummary,
            bytes: &[u8],
        ) -> Result<(), crate::rig::RadioError> {
            match setting.id.as_str() {
                "mode_and_filter" if self.refuse_mode => {
                    return Err(crate::rig::RadioError("FA".to_owned()));
                }
                "mode_and_filter" => self.mode = bytes.to_vec(),
                "filter_width" => {
                    self.widths.insert(self.mode.clone(), bytes.to_vec());
                }
                "pbt_inner" if self.pbt_misses > 0 => {
                    self.pbt_misses -= 1;
                    self.pbt = vec![bytes[0], bytes[1].wrapping_sub(1)];
                }
                "pbt_inner" => self.pbt = bytes.to_vec(),
                _ => {}
            }
            Ok(())
        }
    }

    const FIL2: [u8; 2] = [0x01, 0x02];
    const FIL3: [u8; 2] = [0x01, 0x03];

    fn slots(definition: &Definition) -> Slots {
        Slots {
            definition: definition.clone(),
            mode: FIL3.to_vec(),
            widths: [(FIL2.to_vec(), vec![0x28]), (FIL3.to_vec(), vec![0x22])]
                .into_iter()
                .collect(),
            pbt: vec![0x01, 0x28],
            refuse_mode: false,
            pbt_misses: 0,
        }
    }

    /// A record of the radio's mode, filter width and PBT now.
    fn slots_record(radio: &mut Slots) -> Record {
        let definition = radio.definition.clone();
        let entries = [
            "mode_and_filter",
            "data_mode_and_filter",
            "filter_width",
            "pbt_inner",
        ]
        .iter()
        .map(|id| {
            let setting = definition
                .settings
                .iter()
                .find(|s| s.id == *id)
                .unwrap_or_else(|| unreachable!("the IC-7300 has {id}"));
            let bytes = radio
                .read_exposed(setting)
                .ok()
                .map(|b| crate::civ::hex(&b));
            settings_record::Entry {
                id: (*id).to_owned(),
                label: setting.label.clone(),
                value: settings_record::Value::Raw,
                bytes,
            }
        })
        .collect();
        Record {
            format: 1,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            address: 0x94,
            serial: Some(SERIAL.to_owned()),
            read_utc: "1970-01-01T00:00:00Z".to_owned(),
            computer: "FL1".to_owned(),
            settings: entries,
        }
    }

    const PER_FILTER: [&str; 4] = [
        "mode_and_filter",
        "data_mode_and_filter",
        "filter_width",
        "pbt_inner",
    ];

    #[test]
    fn puts_the_filter_back_before_what_belongs_to_it() {
        let definition = ic_7300(&PER_FILTER);
        let mut radio = slots(&definition);
        // The setup was kept on FIL2, PBT 122; the radio is on FIL3 now,
        // whose own width is 22. FIL2's width still matches the setup.
        radio.mode = FIL2.to_vec();
        radio.pbt = vec![0x01, 0x22];
        let setup = slots_record(&mut radio);
        radio.mode = FIL3.to_vec();
        radio.pbt = vec![0x01, 0x28];
        let now = slots_record(&mut radio);

        let plan = super::plan("TEST", kept(&setup), &now, &definition);
        let order: Vec<&str> = plan.put.iter().map(|p| p.setting.id.as_str()).collect();
        assert_eq!(
            order,
            ["mode_and_filter", "filter_width", "pbt_inner"],
            "{plan:?}"
        );
        assert!(plan.kept_back.is_empty(), "{plan:?}");

        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert!(
            done.changes.iter().all(|d| d.outcome == "put back"),
            "{done:?}"
        );
        assert_eq!(radio.mode, FIL2);
        assert_eq!(radio.widths[&FIL2.to_vec()], [0x28]);
        assert_eq!(
            radio.widths[&FIL3.to_vec()],
            [0x22],
            "FIL3 is left as it was"
        );
        assert_eq!(radio.pbt, [0x01, 0x22]);
    }

    #[test]
    fn writes_nothing_per_filter_when_the_filter_wont_go_back() {
        let definition = ic_7300(&PER_FILTER);
        let mut radio = slots(&definition);
        radio.mode = FIL2.to_vec();
        let setup = slots_record(&mut radio);
        radio.mode = FIL3.to_vec();
        let now = slots_record(&mut radio);
        radio.refuse_mode = true;

        let plan = super::plan("TEST", kept(&setup), &now, &definition);
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        let outcomes: Vec<(&str, &str)> = done
            .changes
            .iter()
            .map(|d| (d.id.as_str(), d.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("mode_and_filter", "refused"),
                ("filter_width", "not written"),
                ("pbt_inner", "not written"),
            ]
        );
        assert_eq!(radio.widths[&FIL3.to_vec()], [0x22], "FIL3 untouched");
    }

    #[test]
    fn keeps_per_filter_settings_back_when_the_filter_is_unknown_or_unwritable() {
        // A setup kept before the mode and filter were kept says nothing
        // about them.
        let definition = ic_7300(&PER_FILTER);
        let mut radio = slots(&definition);
        radio.mode = FIL2.to_vec();
        let setup = slots_record(&mut radio);
        radio.mode = FIL3.to_vec();
        let now = slots_record(&mut radio);
        let without_mode = kept(&setup)
            .into_iter()
            .filter(|(id, _, _)| *id != "mode_and_filter");
        let plan = super::plan("TEST", without_mode, &now, &definition);
        assert!(plan.put.is_empty(), "{plan:?}");
        assert!(
            plan.kept_back
                .iter()
                .any(|b| b.label.starts_with("IF filter width")
                    && b.why.contains("keep the setup again")),
            "{plan:?}"
        );

        // And when the mode and filter weren't checked on a real radio.
        let unchecked = ic_7300(&["filter_width", "pbt_inner", "data_mode_and_filter"]);
        let plan = super::plan("TEST", kept(&setup), &now, &unchecked);
        assert!(plan.put.is_empty(), "{plan:?}");
        assert!(
            plan.kept_back
                .iter()
                .any(|b| b.label.starts_with("IF filter width") && b.why.contains("wrong one")),
            "{plan:?}"
        );
    }

    #[test]
    fn tries_once_more_a_setting_that_lands_off() {
        let definition = ic_7300(&PER_FILTER);
        let mut radio = slots(&definition);
        radio.pbt = vec![0x01, 0x22];
        let setup = slots_record(&mut radio);
        radio.pbt = vec![0x01, 0x28];
        let now = slots_record(&mut radio);
        radio.pbt_misses = 1;
        let plan = super::plan("TEST", kept(&setup), &now, &definition);
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert_eq!(done.changes.len(), 1, "{done:?}");
        assert_eq!(done.changes[0].outcome, "put back");
        assert_eq!(done.changes[0].why.as_deref(), Some("on a second try"));
        assert_eq!(done.changes[0].from.as_deref(), Some("01 28"));
        assert_eq!(radio.pbt, [0x01, 0x22]);

        // Twice off is said, not hidden.
        radio.pbt = vec![0x01, 0x28];
        radio.pbt_misses = 2;
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert_eq!(done.changes[0].outcome, "differs");
    }

    #[test]
    fn sets_the_mode_and_filter_with_06() {
        let definition = ic_7300(&PER_FILTER);
        let mut radio = radio(&definition);
        let setup = read(&mut radio);
        let filter = |r: &Record| {
            r.settings
                .iter()
                .find(|e| e.id == "mode_and_filter")
                .and_then(|e| e.bytes.clone())
        };
        let kept_mode = filter(&setup);
        assert!(kept_mode.is_some(), "the mode and filter are read with 04");
        let setting = definition
            .settings
            .iter()
            .find(|s| s.id == "mode_and_filter")
            .cloned()
            .unwrap_or_else(|| unreachable!("listed"));
        assert!(radio.write_exposed(&setting, &[0x03, 0x02]).is_ok());
        assert_eq!(filter(&read(&mut radio)).as_deref(), Some("03 02"));
        let plan = super::plan("TEST", kept(&setup), &read(&mut radio), &definition);
        let done = apply(&mut radio, &plan, SERIAL, UNIX_EPOCH, "FL1");
        assert!(
            done.changes.iter().all(|d| d.outcome == "put back"),
            "{done:?}"
        );
        assert_eq!(filter(&read(&mut radio)), kept_mode);
    }
}
