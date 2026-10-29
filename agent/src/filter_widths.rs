// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Every filter's width on the selected mode: FIL1, FIL2 and FIL3, not only
//! the selected one (brian, 2026-10-27: a restore puts the radio back
//! exactly as found, each filter's width and which filter is selected).
//!
//! An Icom radio reads and sets only the selected filter's width (`1A 03`,
//! per the IC-7300MK2 CI-V guide the rig definition cites), so the Agent
//! selects each filter in turn and then puts the selection back as it was,
//! reading it again to be sure. It does this only when the operator asked
//! and said yes, and never while a program has the radio keyed: `Shared`
//! refuses settings work then (Principle X). In data mode the filter is
//! chosen with `1A 06` (data mode and filter), since choosing the mode with
//! `06` turns data mode off on Icom radios; otherwise with `06`.

use crate::civ;
use crate::restore::Done;
use crate::rig::Exposes;
use crate::rigdef::{Definition, SettingSummary};
use crate::settings_record;

/// The filters an Icom radio has on each mode: FIL1 to FIL3 (the rig
/// definition's note on `mode_and_filter`).
pub const FILTERS: [u8; 3] = [1, 2, 3];

/// One filter's width on the selected mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterWidth {
    /// The filter: 1 for FIL1.
    pub filter: u8,
    /// Its width's bytes, as `1A 03` sends them.
    pub width: Vec<u8>,
}

impl FilterWidth {
    /// The id a Lab Stock keeps it under: `filter_width_fil1`.
    #[must_use]
    pub fn id(filter: u8) -> String {
        format!("filter_width_fil{filter}")
    }

    /// Its name, as shown: `IF filter width, FIL1`.
    #[must_use]
    pub fn label(filter: u8) -> String {
        format!("IF filter width, FIL{filter}")
    }

    /// Which filter a Lab Stock id is for, if it is one of these.
    #[must_use]
    pub fn filter_of(id: &str) -> Option<u8> {
        let filter = id.strip_prefix("filter_width_fil")?.parse().ok()?;
        FILTERS.contains(&filter).then_some(filter)
    }
}

/// The settings that choose the filter and read its width, if the
/// definition has them and a real radio took each one written back.
struct Selectors {
    mode: SettingSummary,
    data: SettingSummary,
    width: SettingSummary,
}

impl Selectors {
    fn of(definition: &Definition) -> Result<Self, String> {
        let find = |id: &str| {
            definition
                .settings
                .iter()
                .find(|s| s.id == id && s.write && s.write_checked)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "the {}'s definition has no {id} checked on a real radio, so the Agent \
                         can't choose each filter",
                        definition.model
                    )
                })
        };
        Ok(Self {
            mode: find("mode_and_filter")?,
            data: find("data_mode_and_filter")?,
            width: find("filter_width")?,
        })
    }
}

/// Which mode and filter the radio is on, as `04` and `1A 06` say.
#[derive(Clone, PartialEq, Eq)]
struct Selection {
    mode: Vec<u8>,
    data: Vec<u8>,
}

impl Selection {
    fn read(radio: &mut dyn Exposes, s: &Selectors) -> Result<Self, String> {
        let read = |radio: &mut dyn Exposes, setting: &SettingSummary| {
            radio
                .read_exposed(setting)
                .map_err(|error| format!("{} couldn't be read: {error}", setting.label))
        };
        let mode = read(radio, &s.mode)?;
        let data = read(radio, &s.data)?;
        if mode.len() != 2 || data.len() != 2 {
            return Err(format!(
                "the radio said {} and {} for its mode and filter, not a mode and a filter",
                civ::hex(&mode),
                civ::hex(&data)
            ));
        }
        Ok(Self { mode, data })
    }

    fn data_on(&self) -> bool {
        self.data.first().is_some_and(|data| *data != 0)
    }

    /// The filter it is on.
    fn filter(&self) -> u8 {
        if self.data_on() {
            self.data[1]
        } else {
            self.mode[1]
        }
    }

    /// The setting and bytes that choose `filter` on this mode.
    fn choosing<'a>(&self, s: &'a Selectors, filter: u8) -> (&'a SettingSummary, Vec<u8>) {
        if self.data_on() {
            (&s.data, vec![self.data[0], filter])
        } else {
            (&s.mode, vec![self.mode[0], filter])
        }
    }
}

/// Chooses `filter` on the mode of `from`, and checks the radio is on it.
fn choose(
    radio: &mut dyn Exposes,
    s: &Selectors,
    from: &Selection,
    filter: u8,
) -> Result<(), String> {
    let (setting, bytes) = from.choosing(s, filter);
    radio
        .write_exposed(setting, &bytes)
        .map_err(|error| format!("FIL{filter} couldn't be chosen: {error}"))?;
    let now = Selection::read(radio, s)?;
    if now.filter() == filter && now.mode[0] == from.mode[0] && now.data[0] == from.data[0] {
        Ok(())
    } else {
        Err(format!(
            "FIL{filter} was asked for, but the radio is on mode {} data {}",
            civ::hex(&now.mode),
            civ::hex(&now.data)
        ))
    }
}

/// Puts the radio back on the mode and filter of `was`, and checks it.
fn choose_back(radio: &mut dyn Exposes, s: &Selectors, was: &Selection) -> Result<(), String> {
    let (setting, bytes) = was.choosing(s, was.filter());
    let mut tries = 0;
    loop {
        tries += 1;
        let written = radio.write_exposed(setting, &bytes);
        let now = Selection::read(radio, s)?;
        if now == *was {
            return Ok(());
        }
        if tries == 2 {
            return Err(format!(
                "the radio should be back on FIL{} (mode {}, data {}) but is on mode {}, data {}{}",
                was.filter(),
                civ::hex(&was.mode),
                civ::hex(&was.data),
                civ::hex(&now.mode),
                civ::hex(&now.data),
                written.err().map(|e| format!(" ({e})")).unwrap_or_default()
            ));
        }
    }
}

/// Reads each filter's width on the selected mode, choosing each filter in
/// turn, then puts the radio back on the filter it was on. Only after the
/// operator's yes.
///
/// # Errors
///
/// The radio's definition can't choose filters, or the radio wouldn't
/// choose one, read a width, or go back; says which. If it couldn't go
/// back, the error says what filter it is on.
pub fn read(radio: &mut dyn Exposes) -> Result<Vec<FilterWidth>, String> {
    let s = Selectors::of(radio.definition())?;
    let was = Selection::read(radio, &s)?;
    let mut widths = Vec::new();
    let mut failed = None;
    for filter in FILTERS {
        let width = choose(radio, &s, &was, filter).and_then(|()| {
            radio
                .read_exposed(&s.width)
                .map_err(|error| format!("FIL{filter}'s width couldn't be read: {error}"))
        });
        match width {
            Ok(width) => widths.push(FilterWidth { filter, width }),
            Err(why) => {
                failed = Some(why);
                break;
            }
        }
    }
    choose_back(radio, &s, &was)?;
    match failed {
        Some(why) => Err(why),
        None => Ok(widths),
    }
}

/// Puts each of `widths` on its filter of the selected mode, choosing each
/// filter in turn and reading the width back, then puts the radio back on
/// the filter it was on. Says what became of each, for the restore's
/// record. Only in a restore the operator confirmed.
pub fn put(radio: &mut dyn Exposes, widths: &[FilterWidth]) -> Vec<Done> {
    let not_done = |id: String, label: String, to: String, why: String| Done {
        id,
        label,
        from: None,
        to,
        after: None,
        outcome: "not written",
        why: Some(why),
    };
    let selected = Selectors::of(radio.definition())
        .and_then(|s| Selection::read(radio, &s).map(|was| (s, was)));
    let (s, was) = match selected {
        Ok(found) => found,
        Err(why) => {
            return widths
                .iter()
                .map(|w| {
                    not_done(
                        FilterWidth::id(w.filter),
                        FilterWidth::label(w.filter),
                        civ::hex(&w.width),
                        why.clone(),
                    )
                })
                .collect();
        }
    };
    let mut done = Vec::new();
    for width in widths {
        let id = FilterWidth::id(width.filter);
        let label = FilterWidth::label(width.filter);
        if let Err(why) = choose(radio, &s, &was, width.filter) {
            done.push(not_done(id, label, civ::hex(&width.width), why));
            continue;
        }
        let from = radio.read_exposed(&s.width).ok();
        let written = radio.write_exposed(&s.width, &width.width);
        let after = radio.read_exposed(&s.width).ok();
        let (outcome, why) = match (&written, &after) {
            (Err(error), _) => ("refused", Some(error.to_string())),
            (Ok(()), Some(after)) if *after == width.width => ("put back", None),
            (Ok(()), Some(after)) => (
                "differs",
                Some(format!("the radio has {} now", civ::hex(after))),
            ),
            (Ok(()), None) => (
                "differs",
                Some("the radio took it but wouldn't say what it has now".to_owned()),
            ),
        };
        done.push(Done {
            id,
            label,
            from: from.as_deref().map(civ::hex),
            to: civ::hex(&width.width),
            after: after.as_deref().map(civ::hex),
            outcome,
            why,
        });
    }
    if let Err(why) = choose_back(radio, &s, &was) {
        done.push(Done {
            id: s.mode.id.clone(),
            label: "Selected filter, after setting each filter's width".to_owned(),
            from: None,
            to: format!("FIL{}", was.filter()),
            after: None,
            outcome: "differs",
            why: Some(why),
        });
    }
    done
}

/// A width in plain words, as the definition shows the selected filter's.
#[must_use]
pub fn shown(definition: &Definition, width: &[u8]) -> String {
    settings_record::shown(definition, "filter_width", Some(&civ::hex(width)))
}

#[cfg(test)]
mod tests {
    use super::{FilterWidth, put, read};
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::rig::Exposes;
    use crate::rigdef;

    fn radio() -> CivRadio<PretendPort> {
        let definition = rigdef::built_in()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.model == "IC-7300")
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        CivRadio::find(
            PretendPort::new(PretendCiv::new(&definition), 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"))
    }

    fn setting(radio: &CivRadio<PretendPort>, id: &str) -> rigdef::SettingSummary {
        radio
            .definition()
            .settings
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .unwrap_or_else(|| unreachable!("{id} is in the definition"))
    }

    fn read_now(radio: &mut CivRadio<PretendPort>, id: &str) -> Vec<u8> {
        let setting = setting(radio, id);
        radio.read_exposed(&setting).unwrap_or_default()
    }

    fn write(radio: &mut CivRadio<PretendPort>, id: &str, bytes: &[u8]) {
        let setting = setting(radio, id);
        assert!(radio.write_exposed(&setting, bytes).is_ok(), "{id}");
    }

    #[test]
    fn reads_each_filters_width_and_goes_back_to_the_filter_it_was_on() {
        let mut radio = radio();
        // DATA-USB on FIL2, with FIL1 2.4 kHz, FIL2 1.8 kHz, FIL3 500 Hz.
        write(&mut radio, "mode_and_filter", &[0x01, 0x01]);
        write(&mut radio, "data_mode_and_filter", &[0x01, 0x01]);
        write(&mut radio, "filter_width", &[0x28]);
        write(&mut radio, "data_mode_and_filter", &[0x01, 0x03]);
        write(&mut radio, "filter_width", &[0x09]);
        write(&mut radio, "data_mode_and_filter", &[0x01, 0x02]);
        write(&mut radio, "filter_width", &[0x22]);
        let widths = read(&mut radio).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(
            widths,
            vec![
                FilterWidth {
                    filter: 1,
                    width: vec![0x28]
                },
                FilterWidth {
                    filter: 2,
                    width: vec![0x22]
                },
                FilterWidth {
                    filter: 3,
                    width: vec![0x09]
                },
            ]
        );
        assert_eq!(read_now(&mut radio, "data_mode_and_filter"), [0x01, 0x02]);
        assert_eq!(read_now(&mut radio, "mode_and_filter"), [0x01, 0x02]);
        assert_eq!(read_now(&mut radio, "filter_width"), [0x22]);
    }

    #[test]
    fn puts_each_filters_width_on_and_goes_back_to_the_filter_it_was_on() {
        let mut radio = radio();
        // USB, not data mode, on FIL3.
        write(&mut radio, "mode_and_filter", &[0x01, 0x03]);
        let widths = [
            FilterWidth {
                filter: 1,
                width: vec![0x30],
            },
            FilterWidth {
                filter: 2,
                width: vec![0x21],
            },
            FilterWidth {
                filter: 3,
                width: vec![0x15],
            },
        ];
        let done = put(&mut radio, &widths);
        let outcomes: Vec<(&str, &str)> = done.iter().map(|d| (d.id.as_str(), d.outcome)).collect();
        assert_eq!(
            outcomes,
            vec![
                ("filter_width_fil1", "put back"),
                ("filter_width_fil2", "put back"),
                ("filter_width_fil3", "put back"),
            ]
        );
        assert_eq!(read_now(&mut radio, "mode_and_filter"), [0x01, 0x03]);
        assert_eq!(read(&mut radio).unwrap_or_default(), widths);
    }

    #[test]
    fn knows_its_lab_stock_ids() {
        assert_eq!(FilterWidth::id(2), "filter_width_fil2");
        assert_eq!(FilterWidth::filter_of("filter_width_fil3"), Some(3));
        assert_eq!(FilterWidth::filter_of("filter_width_fil4"), None);
        assert_eq!(FilterWidth::filter_of("filter_width"), None);
    }
}
