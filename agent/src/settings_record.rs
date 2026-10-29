// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The settings record: what a radio's menu said the first time the Agent
//! met it.
//!
//! On a radio's first connection, before the Agent changes anything, it
//! reads every menu setting its rig definition lists and keeps the result as
//! the radio's **as-received record**: when (UTC, Principle VIII), on which
//! computer, every value read, and which settings it couldn't read. The
//! record is never overwritten: a later reading is compared against it
//! instead, so the owner can see what changed since the radio arrived.
//!
//! The record is also checked against the definition's requirements, so a
//! setting that stops a job from working (data-mode audio not from USB, for
//! example) is named in plain words. Nothing here changes a setting:
//! offering a change, with the operator's approval, comes later.
//!
//! Records are JSON files, one per radio, written whole and renamed into
//! place (Principle XIV). Reading one back is reading untrusted input
//! (Principle XII): it is capped in size and must have exactly the expected
//! shape. A radio is known by its maker, model and address until the Agent
//! can read its USB serial number, which comes with the serial port.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::civ;
use crate::rig::Exposes;
use crate::rigdef::{Definition, SettingSummary};

/// The largest record file the Agent will read back.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
/// The version of the record format this Agent writes. Format 1 had no
/// serial number and no bytes; it is still read.
const FORMAT: u32 = 2;
/// The record formats this Agent reads.
const READS: [u32; 2] = [1, 2];

/// One setting's value, as read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Value {
    /// One of the setting's named values.
    Named {
        /// Its name, such as "USB".
        name: String,
        /// The code the radio sent.
        code: u8,
    },
    /// A level within the setting's range.
    Level {
        /// The level the radio sent.
        level: u16,
        /// The top of the setting's range.
        high: u16,
        /// Whether the range is a percentage scale.
        percent: bool,
    },
    /// Bytes the definition keeps as they are, without a meaning.
    Raw,
    /// The radio wouldn't say, or said something the definition doesn't
    /// know.
    Unread {
        /// Why, in plain words.
        why: String,
    },
}

impl Value {
    /// The value in plain words.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Named { name, code } => format!("{name} ({code:02X}h)"),
            Self::Level {
                level,
                high,
                percent: true,
            } => {
                let percent =
                    (u32::from(*level) * 100 + u32::from(*high) / 2) / u32::from((*high).max(1));
                format!("{level} of {high} ({percent}%)")
            }
            Self::Level { level, high, .. } => format!("{level} (of 0 to {high})"),
            Self::Raw => "as read".to_owned(),
            Self::Unread { why } => format!("not read: {why}"),
        }
    }
}

/// One setting in a record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The setting's id, such as `data_mod`.
    pub id: String,
    /// Its name on the radio's menu.
    pub label: String,
    /// Its value.
    pub value: Value,
    /// The bytes the radio sent, in hex, exactly: enough to put the setting
    /// back whatever it means. None if it couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
}

impl Entry {
    /// The setting's value in plain words, with the bytes when the value
    /// alone doesn't say.
    #[must_use]
    pub fn text(&self) -> String {
        match (&self.value, &self.bytes) {
            (Value::Raw, Some(bytes)) => bytes.clone(),
            (value, _) => value.text(),
        }
    }
}

/// What a radio's menu said, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The record format's version.
    pub format: u32,
    /// The radio's maker.
    pub maker: String,
    /// The radio's model.
    pub model: String,
    /// The radio's CI-V address.
    pub address: u8,
    /// The radio's own serial number, as its USB serial port reports it,
    /// when the Agent knows it: the one thing that tells two radios of the
    /// same model apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// When it was read, in UTC, as `2026-10-24T20:15:03Z`.
    pub read_utc: String,
    /// The computer it was read on.
    pub computer: String,
    /// Every setting the definition lists, in its order.
    pub settings: Vec<Entry>,
}

/// A requirement the radio's settings don't meet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The job that won't work, such as `digital_modes`.
    pub job: String,
    /// The setting's id.
    pub setting: String,
    /// The setting's name on the radio's menu.
    pub label: String,
    /// What it is now.
    pub now: String,
    /// The values that would let the job work.
    pub accept: Vec<String>,
    /// The value the Agent will offer to change it to, if it may.
    pub offer: Option<String>,
    /// What goes wrong, in plain words.
    pub explain: String,
}

/// A setting that differs between two records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The setting's name on the radio's menu.
    pub label: String,
    /// What it was when the radio arrived.
    pub before: String,
    /// What it is now.
    pub after: String,
}

/// Reads every setting the radio's definition lists. `serial` is the
/// radio's serial number, if the Agent knows it.
pub fn read(
    radio: &mut dyn Exposes,
    serial: Option<&str>,
    now: SystemTime,
    computer: &str,
) -> Record {
    read_where(radio, |_| true, serial, now, computer)
}

/// Reads only the settings whose ids are in `ids`, in the definition's
/// order: enough to plan putting a few settings back, quickly.
pub fn read_these(
    radio: &mut dyn Exposes,
    ids: &[&str],
    serial: Option<&str>,
    now: SystemTime,
    computer: &str,
) -> Record {
    read_where(radio, |id| ids.contains(&id), serial, now, computer)
}

fn read_where(
    radio: &mut dyn Exposes,
    wanted: impl Fn(&str) -> bool,
    serial: Option<&str>,
    now: SystemTime,
    computer: &str,
) -> Record {
    let definition = radio.definition().clone();
    let settings = definition
        .settings
        .iter()
        .filter(|setting| wanted(&setting.id))
        .map(|setting| {
            let (value, bytes) = match radio.read_exposed(setting) {
                Ok(bytes) => (decode(setting, &bytes), Some(civ::hex(&bytes))),
                Err(error) => (
                    Value::Unread {
                        why: error.to_string(),
                    },
                    None,
                ),
            };
            Entry {
                id: setting.id.clone(),
                label: setting.label.clone(),
                value,
                bytes,
            }
        })
        .collect();
    Record {
        format: FORMAT,
        maker: definition.maker.clone(),
        model: definition.model.clone(),
        address: definition.default_address,
        serial: serial.map(str::to_owned),
        read_utc: utc_text(now),
        computer: computer.to_owned(),
        settings,
    }
}

/// What a setting's bytes mean, by its definition.
fn decode(setting: &SettingSummary, bytes: &[u8]) -> Value {
    if setting.raw {
        return Value::Raw;
    }
    match (&setting.values, setting.range, bytes) {
        (Some(values), _, [code]) => values.iter().find(|(_, c)| **c == *code).map_or_else(
            || Value::Unread {
                why: format!("{code:02X}h is not a value the definition lists"),
            },
            |(name, _)| Value::Named {
                name: name.clone(),
                code: *code,
            },
        ),
        (None, Some([low, high]), bcd) => match level(bcd) {
            Some(level) if (u16::from(low)..=u16::from(high)).contains(&level) => Value::Level {
                level,
                high: u16::from(high),
                percent: setting.percent,
            },
            _ => Value::Unread {
                why: format!("{} is not a level from {low} to {high}", civ::hex(bcd)),
            },
        },
        _ => Value::Unread {
            why: format!("{} is not what the definition expects", civ::hex(bytes)),
        },
    }
}

/// A level as the radio sends it: two BCD bytes ("0000 to 0255"), or one
/// when the range is short ("00 to 10").
fn level(bcd: &[u8]) -> Option<u16> {
    match bcd {
        [one] if one >> 4 <= 9 && one & 0x0F <= 9 => {
            Some(u16::from(one >> 4) * 10 + u16::from(one & 0x0F))
        }
        [_] => None,
        two => civ::level_from_bcd(two),
    }
}

/// The requirements a record doesn't meet. A setting that couldn't be read
/// is a finding too: nobody can say the job will work.
#[must_use]
pub fn findings(record: &Record, definition: &Definition) -> Vec<Finding> {
    let mut out = Vec::new();
    for requirement in &definition.requirements {
        let Some(entry) = record.settings.iter().find(|e| e.id == requirement.setting) else {
            continue;
        };
        let met =
            matches!(&entry.value, Value::Named { name, .. } if requirement.accept.contains(name));
        if !met {
            out.push(Finding {
                job: requirement.job.clone(),
                setting: requirement.setting.clone(),
                label: entry.label.clone(),
                now: entry.value.text(),
                accept: requirement.accept.clone(),
                offer: requirement.offer.clone(),
                explain: requirement.explain.clone(),
            });
        }
    }
    out
}

/// The settings that differ between the as-received record and a later one,
/// leaving out any the definition says change by themselves (a clock).
#[must_use]
pub fn changes(as_received: &Record, now: &Record, definition: &Definition) -> Vec<Change> {
    now.settings
        .iter()
        .filter(|entry| !moves(definition, &entry.id))
        .filter_map(|entry| {
            let before = as_received.settings.iter().find(|e| e.id == entry.id)?;
            let differs = before.value != entry.value
                || (entry.value == Value::Raw && before.bytes != entry.bytes);
            differs.then(|| Change {
                label: entry.label.clone(),
                before: before.text(),
                after: entry.text(),
            })
        })
        .collect()
}

/// Whether the definition says a setting changes by itself.
fn moves(definition: &Definition, id: &str) -> bool {
    definition
        .settings
        .iter()
        .any(|setting| setting.id == id && setting.moves)
}

/// The settings where `now` differs from `kept`, a reading kept elsewhere
/// (on the Engine, say) as each setting's id, label and bytes. Each value is
/// shown in plain words where the definition says what its bytes mean.
#[must_use]
pub fn differences<'a>(
    kept: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
    now: &Record,
    definition: &Definition,
) -> Vec<Change> {
    kept.into_iter()
        .filter(|(id, _, _)| !moves(definition, id))
        .filter_map(|(id, label, bytes)| {
            let entry = now.settings.iter().find(|e| e.id == id)?;
            (entry.bytes.as_deref() != bytes).then(|| Change {
                label: label.to_owned(),
                before: shown(definition, id, bytes),
                after: entry.text(),
            })
        })
        .collect()
}

/// A setting's bytes, as hex pairs, in plain words.
pub fn shown(definition: &Definition, id: &str, bytes: Option<&str>) -> String {
    let Some(text) = bytes else {
        return "not read".to_owned();
    };
    let parsed: Option<Vec<u8>> = text
        .split_whitespace()
        .map(|pair| u8::from_str_radix(pair, 16).ok())
        .collect();
    match (definition.settings.iter().find(|s| s.id == id), parsed) {
        (Some(setting), Some(parsed)) if !setting.raw => match decode(setting, &parsed) {
            Value::Unread { .. } => text.to_owned(),
            value => value.text(),
        },
        _ => text.to_owned(),
    }
}

/// The file a radio's record lives in, under `dir`: by its serial number
/// when the Agent knows it, so each radio has its own, or else by its
/// address.
#[must_use]
pub fn path_for(dir: &Path, record: &Record) -> PathBuf {
    let which = record
        .serial
        .clone()
        .unwrap_or_else(|| format!("{:02x}", record.address));
    let name: String = format!("{}-{}-{which}", record.maker, record.model)
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

/// Keeps a record, written whole and renamed into place (Principle XIV).
///
/// # Errors
///
/// The file could not be written, or one is already there: a record, the
/// as-received record above all, is never overwritten.
pub fn save(path: &Path, record: &impl Serialize) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("can't save {}: {error}", path.display());
    if path.exists() {
        return Err(format!(
            "{} already holds a record; it is never overwritten",
            path.display()
        ));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(record).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temp).map_err(fail)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(fail)?;
    drop(file);
    fs::rename(&temp, path).map_err(fail)
}

/// Reads a record back, if there is one.
///
/// # Errors
///
/// The file is there but can't be read, is too large, or isn't a record in
/// this format (Principle XII).
pub fn load(path: &Path) -> Result<Option<Record>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("can't open {}: {error}", path.display())),
    };
    let mut text = String::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("can't read {}: {error}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
        return Err(format!(
            "{} is too large to be a settings record",
            path.display()
        ));
    }
    let record: Record = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a settings record: {error}", path.display()))?;
    if !READS.contains(&record.format) {
        return Err(format!(
            "{} is record format {}; this Agent reads formats 1 and 2",
            path.display(),
            record.format
        ));
    }
    Ok(Some(record))
}

/// The record in plain words, with what it found.
#[must_use]
pub fn report(
    record: &Record,
    found: &[Finding],
    as_received: Option<&Record>,
    definition: &Definition,
) -> Vec<String> {
    let mut lines = Vec::new();
    // A radio with no link address (Yaesu CAT) is named without one.
    let at = if record.address == 0 {
        String::new()
    } else {
        format!(" at {:02X}h", record.address)
    };
    match as_received {
        None => lines.push(format!(
            "As received: {} {}{at}, read {} on {}",
            record.maker, record.model, record.read_utc, record.computer
        )),
        Some(first) => lines.push(format!(
            "{} {}{at}, read {} on {} (as received {} on {})",
            record.maker,
            record.model,
            record.read_utc,
            record.computer,
            first.read_utc,
            first.computer
        )),
    }
    let width = record
        .settings
        .iter()
        .map(|e| e.label.chars().count())
        .max()
        .unwrap_or(0);
    for entry in &record.settings {
        lines.push(format!("  {:width$}  {}", entry.label, entry.text()));
    }
    let unread = record
        .settings
        .iter()
        .filter(|e| matches!(e.value, Value::Unread { .. }))
        .count();
    lines.push(format!(
        "{} settings read, {unread} not read.",
        record.settings.len() - unread
    ));
    if let Some(first) = as_received {
        let changed = changes(first, record, definition);
        if changed.is_empty() {
            lines.push("Nothing has changed since the radio arrived.".to_owned());
        } else {
            lines.push("Changed since the radio arrived:".to_owned());
            for change in changed {
                lines.push(format!(
                    "  {}: {} -> {}",
                    change.label, change.before, change.after
                ));
            }
        }
    }
    if found.is_empty() {
        lines.push("Every requirement is met.".to_owned());
    } else {
        lines.push("Needs attention:".to_owned());
        for finding in found {
            lines.push(format!(
                "  {} won't work: {} is {}. {} It needs {}.{}",
                finding.job,
                finding.label,
                finding.now,
                finding.explain,
                finding.accept.join(" or "),
                finding
                    .offer
                    .as_ref()
                    .map(|v| format!(" (The Agent will offer to set it to {v}, with your OK, once it can change settings.)"))
                    .unwrap_or_default()
            ));
        }
    }
    lines
}

/// A time as UTC text, like `2026-10-24T20:15:03Z` (Principle VIII).
#[must_use]
pub fn utc_text(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's method).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// This computer's name, for the record.
#[must_use]
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|name| !name.is_empty() && name.len() <= 64)
        .unwrap_or_else(|| "this computer".to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        Value, changes, differences, findings, load, path_for, read, report, save, utc_text,
    };
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::rigdef::{self, Definition};

    fn ic_7300() -> Definition {
        rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"))
    }

    fn radio() -> CivRadio<PretendPort> {
        let definition = ic_7300();
        let port = PretendPort::new(PretendCiv::new(&definition), 115_200);
        CivRadio::find(port, &[definition]).unwrap_or_else(|error| unreachable!("{error}"))
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("loglab-record-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn writes_utc_times() {
        assert_eq!(utc_text(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            utc_text(at(951_782_400)),
            "2000-02-29T00:00:00Z",
            "a leap day"
        );
        assert_eq!(utc_text(at(1_793_045_703)), "2026-10-26T20:15:03Z");
        assert_eq!(utc_text(at(4_102_444_799)), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn reads_every_setting_the_definition_lists() {
        let mut radio = radio();
        let record = read(&mut radio, None, at(0), "FL1");
        assert_eq!(
            (record.maker.as_str(), record.model.as_str()),
            ("Icom", "IC-7300")
        );
        assert_eq!(record.settings.len(), 216 + 44);
        let value = |id: &str| {
            record
                .settings
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.value.clone())
        };
        assert_eq!(
            value("data_mod"),
            Some(Value::Named {
                name: "MIC".to_owned(),
                code: 0
            })
        );
        assert_eq!(
            value("usb_mod_level"),
            Some(Value::Level {
                level: 127,
                high: 255,
                percent: true,
            })
        );
        assert_eq!(
            value("civ_transceive"),
            Some(Value::Named {
                name: "ON".to_owned(),
                code: 1
            })
        );
        // Front-panel controls, each by its own command.
        assert_eq!(
            value("agc"),
            Some(Value::Named {
                name: "OFF".to_owned(),
                code: 0
            })
        );
        assert_eq!(
            value("filter_width"),
            Some(Value::Level {
                level: 24,
                high: 49,
                percent: false,
            }),
            "a short level is one BCD byte"
        );
        // A raw setting keeps exactly the bytes the radio sent.
        let entry = |id: &str| record.settings.iter().find(|e| e.id == id).cloned();
        let attenuator = entry("attenuator").unwrap_or_else(|| unreachable!());
        assert_eq!(attenuator.value, Value::Raw);
        assert_eq!(attenuator.bytes.as_deref(), Some("00"));
        assert_eq!(attenuator.text(), "00");
        assert_eq!(
            entry("rf_gain").and_then(|e| e.bytes),
            Some("01 27".to_owned())
        );
        assert!(
            record
                .settings
                .iter()
                .all(|e| !matches!(e.value, Value::Unread { .. }) && e.bytes.is_some())
        );
    }

    #[test]
    fn a_change_to_raw_bytes_is_a_change() {
        let mut radio = radio();
        let first = read(&mut radio, Some("IC-7300_02065962"), at(0), "FL1");
        assert!(
            radio.port_mut().radio().set(&[0x11], 0x0020),
            "ATT on, as bytes"
        );
        let later = read(&mut radio, Some("IC-7300_02065962"), at(60), "FL2");
        let changed = changes(&first, &later, &ic_7300());
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].label, "Attenuator (ATT)");
        assert_eq!(
            (changed[0].before.as_str(), changed[0].after.as_str()),
            ("00", "00 20")
        );
    }

    #[test]
    fn differences_from_a_kept_reading_are_by_bytes_in_plain_words() {
        let definition = ic_7300();
        let mut radio = radio();
        let kept = read(&mut radio, Some("IC-7300_02065962"), at(0), "FL1");
        let named = definition
            .settings
            .iter()
            .find(|s| s.item().is_some() && s.values.as_ref().is_some_and(|v| v.len() > 1))
            .unwrap_or_else(|| unreachable!("the IC-7300 has a menu item with named values"));
        let before = kept
            .settings
            .iter()
            .find(|e| e.id == named.id)
            .unwrap_or_else(|| unreachable!("every setting is read"))
            .clone();
        let other = named
            .values
            .iter()
            .flatten()
            .map(|(_, code)| *code)
            .find(|code| before.bytes != Some(format!("{code:02X}")))
            .unwrap_or_else(|| unreachable!("it has another value"));
        assert!(radio.port_mut().radio().set(&named.read, u16::from(other)));
        assert!(radio.port_mut().radio().set(&[0x11], 0x0020), "ATT on");
        let now = read(&mut radio, Some("IC-7300_02065962"), at(60), "FL2");
        let as_kept = kept
            .settings
            .iter()
            .map(|e| (e.id.as_str(), e.label.as_str(), e.bytes.as_deref()));
        let differ = differences(as_kept, &now, &definition);
        assert_eq!(differ.len(), 2, "{differ:?}");
        let item = differ
            .iter()
            .find(|c| c.label == named.label)
            .unwrap_or_else(|| unreachable!("{differ:?}"));
        assert_eq!(item.before, before.value.text(), "shown in words");
        assert_ne!(item.before, item.after);
        let att = differ
            .iter()
            .find(|c| c.label == "Attenuator (ATT)")
            .unwrap_or_else(|| unreachable!("{differ:?}"));
        assert_eq!((att.before.as_str(), att.after.as_str()), ("00", "00 20"));
        let unknown = [("no_such", "No such", Some("01"))];
        assert!(differences(unknown, &now, &definition).is_empty());
        let unread = [(named.id.as_str(), named.label.as_str(), None)];
        assert_eq!(differences(unread, &now, &definition)[0].before, "not read");
    }

    #[test]
    fn the_clock_is_never_a_change() {
        let definition = ic_7300();
        let time = definition
            .settings
            .iter()
            .find(|s| s.moves)
            .unwrap_or_else(|| unreachable!("the IC-7300's clock moves"))
            .clone();
        let mut radio = radio();
        let first = read(&mut radio, Some("IC-7300_02065962"), at(0), "FL1");
        let mut later = first.clone();
        for entry in &mut later.settings {
            if entry.id == time.id {
                entry.bytes = Some("07 25".to_owned());
                entry.value = Value::Raw;
            }
        }
        assert!(changes(&first, &later, &definition).is_empty());
        let as_kept = first
            .settings
            .iter()
            .map(|e| (e.id.as_str(), e.label.as_str(), e.bytes.as_deref()));
        assert!(differences(as_kept, &later, &definition).is_empty());
    }

    #[test]
    fn each_radio_has_its_own_record_by_serial_number() {
        let mut radio = radio();
        let dir = std::path::Path::new("records");
        let one = read(&mut radio, Some("IC-7300_02065962"), at(0), "FL1");
        let other = read(&mut radio, Some("IC-7300_03000001"), at(0), "FL1");
        assert!(path_for(dir, &one).ends_with("icom-ic-7300-ic-7300-02065962.json"));
        assert_ne!(path_for(dir, &one), path_for(dir, &other));
        // Without one, by address, as before.
        let unknown = read(&mut radio, None, at(0), "FL1");
        assert!(path_for(dir, &unknown).ends_with("icom-ic-7300-94.json"));
    }

    #[test]
    fn still_reads_a_format_1_record() {
        let dir = temp_dir("format1");
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        let path = dir.join("old.json");
        let old = r#"{"format":1,"maker":"Icom","model":"IC-7300","address":148,
            "read_utc":"2026-10-24T20:15:03Z","computer":"FL1","settings":[
            {"id":"data_mod","label":"DATA MOD","value":{"kind":"named","name":"ACC","code":1}}]}"#;
        std::fs::write(&path, old).unwrap_or_else(|error| unreachable!("{error}"));
        let record = load(&path).ok().flatten().unwrap_or_else(|| unreachable!());
        assert_eq!(record.serial, None);
        assert_eq!(record.settings[0].bytes, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_what_stops_a_job_from_working() {
        let mut radio = radio();
        let record = read(&mut radio, None, at(0), "FL1");
        let found = findings(&record, radio.definition());
        let jobs: Vec<&str> = found.iter().map(|f| f.job.as_str()).collect();
        // DATA MOD and DATA OFF MOD start on MIC in the pretend radio.
        assert_eq!(jobs, ["digital_modes", "computer_voice"]);
        assert_eq!(found[0].offer.as_deref(), Some("USB"));
        assert_eq!(found[0].now, "MIC (00h)");
    }

    #[test]
    fn a_met_requirement_is_not_a_finding() {
        let mut radio = radio();
        assert!(
            radio
                .definition()
                .settings
                .iter()
                .any(|s| s.id == "data_mod")
        );
        let mut record = read(&mut radio, None, at(0), "FL1");
        for entry in &mut record.settings {
            if entry.id == "data_mod" || entry.id == "data_off_mod" {
                entry.value = Value::Named {
                    name: "USB".to_owned(),
                    code: 3,
                };
            }
        }
        assert!(findings(&record, radio.definition()).is_empty());
    }

    #[test]
    fn keeps_the_first_record_and_compares_later_ones() {
        let dir = temp_dir("keep");
        let mut radio = radio();
        let first = read(&mut radio, None, at(0), "FL1");
        let path = path_for(&dir, &first);
        assert!(path.ends_with("icom-ic-7300-94.json"));
        assert_eq!(load(&path), Ok(None));
        assert_eq!(save(&path, &first), Ok(()));
        assert_eq!(
            load(&path),
            Ok(Some(first.clone())),
            "it reads back exactly"
        );
        // Someone changes DATA MOD to USB on the radio, later.
        let mut later = read(&mut radio, None, at(3_600), "FL2");
        if let Some(entry) = later.settings.iter_mut().find(|e| e.id == "data_mod") {
            entry.value = Value::Named {
                name: "USB".to_owned(),
                code: 3,
            };
        }
        let error = save(&path, &later).err().unwrap_or_default();
        assert!(error.contains("never overwritten"), "{error}");
        assert_eq!(
            load(&path),
            Ok(Some(first.clone())),
            "the first record is untouched"
        );
        let changed = changes(&first, &later, &ic_7300());
        assert_eq!(changed.len(), 1);
        assert_eq!(
            (changed[0].before.as_str(), changed[0].after.as_str()),
            ("MIC (00h)", "USB (03h)")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_file_that_is_not_a_record() {
        let dir = temp_dir("bad");
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        let path = dir.join("x.json");
        for text in ["{}", "not json", r#"{"format":1,"extra":true}"#] {
            std::fs::write(&path, text).unwrap_or_else(|error| unreachable!("{error}"));
            assert!(load(&path).is_err(), "{text} should be refused");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_in_plain_words() {
        let mut radio = radio();
        let record = read(&mut radio, None, at(0), "FL1");
        let found = findings(&record, radio.definition());
        let text = report(&record, &found, None, &ic_7300()).join("\n");
        assert!(
            text.starts_with("As received: Icom IC-7300 at 94h, read 1970-01-01T00:00:00Z on FL1"),
            "{text}"
        );
        assert!(text.contains("DATA MOD"), "{text}");
        assert!(text.contains("127 of 255 (50%)"), "{text}");
        assert!(
            text.contains("111 (of 0 to 223)"),
            "an address range is not a percentage: {text}"
        );
        assert!(text.contains("260 settings read, 0 not read."), "{text}");
        assert!(
            text.contains("digital_modes won't work: DATA MOD is MIC (00h)."),
            "{text}"
        );
        assert!(text.contains("offer to set it to USB"), "{text}");
        let again = report(&record, &found, Some(&record), &ic_7300()).join("\n");
        assert!(
            again.contains("Nothing has changed since the radio arrived."),
            "{again}"
        );
    }
}
