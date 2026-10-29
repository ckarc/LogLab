// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Lab Stock: a radio model's everyday settings, kept on the Engine, which
//! any of the lab's radios of that model can be put back to between users.
//!
//! `stock save` sends the radio's settings to the Engine as a proposal. It
//! changes nothing until an administrator approves it in LabLog (the
//! Engine's gate). `stock` shows what putting the approved Lab Stock on would
//! change and waits for the operator's yes; only then does the restore path
//! write it, and only settings a real radio was checked to take
//! (Principle X, first exception). Auto mode never answers for it.

use crate::engine::{LabStock, StockSetting};
use crate::filter_widths::{self, FilterWidth};
use crate::restore::{self, Plan};
use crate::rigdef::Definition;
use crate::settings_record::{self, Record};

/// The longest value in words the Engine keeps for a setting.
const MAX_SHOWN: usize = 200;

/// The settings of `record` a Lab Stock holds: every one the Agent may
/// write, that a real radio took when written back, that doesn't change by
/// itself (the clock), and that the radio said what it has; then each
/// filter's width on the selected mode, from `widths`.
#[must_use]
pub fn from_record(
    record: &Record,
    definition: &Definition,
    widths: &[FilterWidth],
) -> Vec<StockSetting> {
    let mut settings = checked(record, definition);
    if settings.is_empty() {
        return settings;
    }
    settings.extend(widths.iter().map(|w| StockSetting {
        id: FilterWidth::id(w.filter),
        label: FilterWidth::label(w.filter),
        bytes: crate::civ::hex(&w.width),
        shown: Some(filter_widths::shown(definition, &w.width)),
    }));
    settings
}

/// The settings of `record` that a Lab Stock holds by themselves.
#[must_use]
pub fn checked(record: &Record, definition: &Definition) -> Vec<StockSetting> {
    record
        .settings
        .iter()
        .filter_map(|entry| {
            let setting = definition.settings.iter().find(|s| s.id == entry.id)?;
            if !setting.write || !setting.write_checked || setting.moves {
                return None;
            }
            let bytes = entry.bytes.clone()?;
            let shown = settings_record::shown(definition, &entry.id, Some(&bytes));
            let shown = (!shown.is_empty()
                && shown.chars().count() <= MAX_SHOWN
                && !shown.chars().any(char::is_control))
            .then_some(shown);
            Some(StockSetting {
                id: entry.id.clone(),
                label: entry.label.clone(),
                bytes,
                shown,
            })
        })
        .collect()
}

/// The name a Lab Stock goes by on the console: `Lab Stock #3`.
#[must_use]
pub fn name(stock: &LabStock) -> String {
    format!("Lab Stock #{}", stock.id)
}

/// What putting `stock` on the radio, whose settings are `now`, would
/// change: the restore path's plan, so the mode and filter go first and a
/// setting the Agent may not write is only listed.
#[must_use]
pub fn plan(stock: &LabStock, now: &Record, definition: &Definition) -> Plan {
    let mut plan = restore::plan(
        &name(stock),
        stock
            .settings
            .iter()
            .filter(|s| FilterWidth::filter_of(&s.id).is_none())
            .map(|s| (s.id.as_str(), s.label.as_str(), Some(s.bytes.as_str()))),
        now,
        definition,
    );
    plan.filters = stock
        .settings
        .iter()
        .filter_map(|s| {
            let filter = FilterWidth::filter_of(&s.id)?;
            let width = restore::parse_hex(&s.bytes)?;
            let shown = filter_widths::shown(definition, &width);
            Some((FilterWidth { filter, width }, shown))
        })
        .collect();
    plan
}

#[cfg(test)]
mod tests {
    use super::{from_record, plan};
    use crate::engine::{LabStock, StockSetting};
    use crate::rigdef;
    use crate::settings_record::{Entry, Record, Value};

    fn ic7300() -> rigdef::Definition {
        rigdef::built_in()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.model == "IC-7300")
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"))
    }

    fn entry(id: &str, bytes: Option<&str>) -> Entry {
        Entry {
            id: id.to_owned(),
            label: id.to_owned(),
            value: Value::Raw,
            bytes: bytes.map(str::to_owned),
        }
    }

    fn record(settings: Vec<Entry>) -> Record {
        Record {
            format: 2,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            address: 0x94,
            serial: Some("IC-7300_02065962".to_owned()),
            read_utc: "2026-10-29T19:00:00Z".to_owned(),
            computer: "FL1".to_owned(),
            settings,
        }
    }

    #[test]
    fn holds_only_settings_the_agent_may_write_and_a_radio_took() {
        let definition = ic7300();
        let writable = definition
            .settings
            .iter()
            .find(|s| s.write && s.write_checked && !s.moves)
            .map(|s| s.id.clone())
            .unwrap_or_default();
        let unchecked = definition
            .settings
            .iter()
            .find(|s| !(s.write && s.write_checked))
            .map(|s| s.id.clone())
            .unwrap_or_default();
        let clock = definition
            .settings
            .iter()
            .find(|s| s.moves)
            .map(|s| s.id.clone());
        let mut entries = vec![
            entry(&writable, Some("03")),
            entry(&unchecked, Some("01")),
            entry("data_mod", None),
            entry("no_such_setting", Some("00")),
        ];
        if let Some(clock) = &clock {
            entries.push(entry(clock, Some("12 00")));
        }
        let stock = from_record(&record(entries), &definition, &[]);
        let ids: Vec<&str> = stock.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec![writable.as_str()]);
        assert_eq!(stock[0].bytes, "03");
        assert!(stock[0].shown.is_some());
    }

    #[test]
    fn a_full_reading_of_an_ic_7300_makes_a_lab_stock_of_every_checked_setting() {
        let definition = ic7300();
        let entries = definition
            .settings
            .iter()
            .map(|s| entry(&s.id, Some("00")))
            .collect();
        let stock = from_record(&record(entries), &definition, &[]);
        let wanted = definition
            .settings
            .iter()
            .filter(|s| s.write && s.write_checked && !s.moves)
            .count();
        assert_eq!(stock.len(), wanted);
        assert!(wanted > 200, "only {wanted} IC-7300 settings are checked");
    }

    #[test]
    fn putting_it_on_changes_only_what_differs_and_names_the_lab_stock() {
        let definition = ic7300();
        let now = record(vec![
            entry("data_mod", Some("00")),
            entry("usb_mod_level", Some("01 02")),
        ]);
        let stock = LabStock {
            id: 3,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            settings: vec![
                StockSetting {
                    id: "data_mod".to_owned(),
                    label: "DATA MOD".to_owned(),
                    bytes: "03".to_owned(),
                    shown: Some("USB".to_owned()),
                },
                StockSetting {
                    id: "usb_mod_level".to_owned(),
                    label: "USB MOD Level".to_owned(),
                    bytes: "01 02".to_owned(),
                    shown: None,
                },
            ],
        };
        let plan = plan(&stock, &now, &definition);
        assert_eq!(plan.setup, "Lab Stock #3");
        let put: Vec<(&str, &[u8])> = plan
            .put
            .iter()
            .map(|p| (p.setting.id.as_str(), p.to.as_slice()))
            .collect();
        assert_eq!(put, vec![("data_mod", &[0x03][..])]);
    }

    #[test]
    fn putting_it_on_sets_each_filters_width_and_selects_the_lab_stocks_filter() {
        use crate::civ_pretend::PretendCiv;
        use crate::civ_radio::{CivRadio, PretendPort};
        use crate::filter_widths::{self, FilterWidth};
        use crate::rig::Exposes;

        let definition = ic7300();
        let mut radio = CivRadio::find(
            PretendPort::new(PretendCiv::new(&definition), 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let now = crate::settings_record::read(
            &mut radio,
            Some("PRETEND"),
            std::time::UNIX_EPOCH,
            "test",
        );
        let setting = |id: &str, bytes: &str| StockSetting {
            id: id.to_owned(),
            label: id.to_owned(),
            bytes: bytes.to_owned(),
            shown: None,
        };
        // DATA-USB on FIL2, FIL1 2.4 kHz, FIL2 1.8 kHz, FIL3 500 Hz.
        let stock = LabStock {
            id: 5,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            settings: vec![
                setting("mode_and_filter", "01 02"),
                setting("data_mode_and_filter", "01 02"),
                setting("filter_width_fil1", "28"),
                setting("filter_width_fil2", "22"),
                setting("filter_width_fil3", "09"),
            ],
        };
        let plan = plan(&stock, &now, &definition);
        assert_eq!(plan.filters.len(), 3);
        assert!(
            plan.lines().iter().any(|line| line.starts_with("  FIL3: ")),
            "{:?}",
            plan.lines()
        );
        let done =
            crate::restore::apply(&mut radio, &plan, "PRETEND", std::time::UNIX_EPOCH, "test");
        assert!(
            done.changes.iter().all(|d| d.outcome == "put back"),
            "{:?}",
            done.changes
        );
        let widths = filter_widths::read(&mut radio).unwrap_or_default();
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
        let data = definition
            .settings
            .iter()
            .find(|s| s.id == "data_mode_and_filter")
            .cloned()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(radio.read_exposed(&data).unwrap_or_default(), [0x01, 0x02]);
    }
}
