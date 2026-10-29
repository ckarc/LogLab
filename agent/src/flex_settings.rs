// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A FlexRadio's settings, as the Agent reads, records and puts back.
//!
//! A FlexRadio has a dedicated driver, not a rig definition file
//! (Principle IX): every FLEX-6000 and -8000 series radio speaks the same
//! SmartSDR API, and its settings are named words in its status lines
//! rather than menu numbers. This list is the whole set of settings the
//! Agent knows; [`definition`] presents it as a [`Definition`], so the
//! settings record, restore, lab profiles and Lab Stock work on a
//! FlexRadio as on any other radio.
//!
//! Each setting is read from the radio's own status lines (`transmit` for
//! the transmitter, `slice` for the slice the Agent reports) and set with
//! `transmit set`, `slice set` or `filt`. Status and set names come from
//! FlexRadio's API wiki (TCPIP-slice, TCPIP-transmit, TCPIP-filt) and, where
//! the wiki is silent, from `AetherSDR`'s reading of the radio (facts only,
//! no code: Principle XVI). Where the two names differ (`mic_level` is set
//! as `miclevel`), both are kept. None of it is checked on a real radio,
//! so none of it is `write_checked`: the Agent reads and records, but writes
//! nothing until `check-writes` has run on one.
//!
//! Values are kept as the other drivers keep theirs, so the record and
//! restore code reads them the same way: a level as two BCD bytes, a
//! switch or a named choice as one code byte, and anything else as the
//! radio's own text.

use std::collections::BTreeMap;

use crate::civ;
use crate::rigdef::{Definition, SettingSummary};

/// The protocol family of a FlexRadio's definition.
pub const FAMILY: &str = "flex-api";
/// The longest value text the Agent keeps.
const MAX_VALUE: usize = 32;

/// Where a setting lives on the radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Object {
    /// The transmitter: `transmit` status, `transmit set`.
    Transmit,
    /// The slice the Agent reports: `slice` status, `slice set <n>`.
    Slice,
    /// That slice's filter edges: `filter_lo` and `filter_hi` in its
    /// status, set together with `filt <n> <lo> <hi>`.
    Filter,
}

/// What a setting's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A whole number from 0 to the given top.
    Level(u8),
    /// On or off: the radio says `1` or `0`.
    Switch,
    /// One of the radio's words.
    Words(&'static [&'static str]),
    /// The radio's own text, kept and put back as it is.
    Raw,
}

/// One FlexRadio setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlexSetting {
    /// The Agent's id for it.
    pub id: &'static str,
    /// Its name in SmartSDR's words.
    pub label: &'static str,
    /// Where it lives.
    pub object: Object,
    /// Its name in the radio's status lines.
    pub status: &'static str,
    /// Its name when set.
    pub set: &'static str,
    /// What its value is.
    pub kind: Kind,
}

const fn setting(
    id: &'static str,
    label: &'static str,
    object: Object,
    status: &'static str,
    set: &'static str,
    kind: Kind,
) -> FlexSetting {
    FlexSetting {
        id,
        label,
        object,
        status,
        set,
        kind,
    }
}

use Kind::{Level, Raw, Switch, Words};
use Object::{Filter, Slice, Transmit};

/// Every setting the Agent knows, in the order it reads and puts them
/// back. The slice's mode comes first: its filter and offsets belong to it.
pub const SETTINGS: &[FlexSetting] = &[
    setting("mode", "Slice mode", Slice, "mode", "mode", Raw),
    setting(
        "filter",
        "Slice filter (low, high)",
        Filter,
        "filter",
        "filt",
        Raw,
    ),
    setting(
        "audio_level",
        "Slice AF gain",
        Slice,
        "audio_level",
        "audio_level",
        Level(100),
    ),
    setting(
        "audio_pan",
        "Slice audio pan",
        Slice,
        "audio_pan",
        "audio_pan",
        Level(100),
    ),
    setting(
        "audio_mute",
        "Slice mute",
        Slice,
        "audio_mute",
        "audio_mute",
        Switch,
    ),
    setting(
        "rf_gain",
        "Slice RF gain (preamp)",
        Slice,
        "rfgain",
        "rf_gain",
        Raw,
    ),
    setting(
        "agc_mode",
        "AGC",
        Slice,
        "agc_mode",
        "agc_mode",
        Words(&["off", "slow", "med", "fast"]),
    ),
    setting(
        "agc_threshold",
        "AGC threshold",
        Slice,
        "agc_threshold",
        "agc_threshold",
        Level(100),
    ),
    setting(
        "agc_off_level",
        "AGC off level",
        Slice,
        "agc_off_level",
        "agc_off_level",
        Level(100),
    ),
    setting("nb", "NB", Slice, "nb", "nb", Switch),
    setting(
        "nb_level",
        "NB level",
        Slice,
        "nb_level",
        "nb_level",
        Level(100),
    ),
    setting("wnb", "WNB", Slice, "wnb", "wnb", Switch),
    setting(
        "wnb_level",
        "WNB level",
        Slice,
        "wnb_level",
        "wnb_level",
        Level(100),
    ),
    setting("nr", "NR", Slice, "nr", "nr", Switch),
    setting(
        "nr_level",
        "NR level",
        Slice,
        "nr_level",
        "nr_level",
        Level(100),
    ),
    setting("anf", "ANF", Slice, "anf", "anf", Switch),
    setting(
        "anf_level",
        "ANF level",
        Slice,
        "anf_level",
        "anf_level",
        Level(100),
    ),
    setting("apf", "APF", Slice, "apf", "apf", Switch),
    setting(
        "apf_level",
        "APF level",
        Slice,
        "apf_level",
        "apf_level",
        Level(100),
    ),
    setting("squelch", "Squelch", Slice, "squelch", "squelch", Switch),
    setting(
        "squelch_level",
        "Squelch level",
        Slice,
        "squelch_level",
        "squelch_level",
        Level(100),
    ),
    setting(
        "slice_dax",
        "Slice DAX channel",
        Slice,
        "dax",
        "dax",
        Level(8),
    ),
    setting("rxant", "Slice RX antenna", Slice, "rxant", "rxant", Raw),
    setting("txant", "Slice TX antenna", Slice, "txant", "txant", Raw),
    setting(
        "digu_offset",
        "DIGU offset",
        Slice,
        "digu_offset",
        "digu_offset",
        Raw,
    ),
    setting(
        "digl_offset",
        "DIGL offset",
        Slice,
        "digl_offset",
        "digl_offset",
        Raw,
    ),
    setting(
        "rf_power",
        "RF power",
        Transmit,
        "rfpower",
        "rfpower",
        Level(100),
    ),
    setting(
        "tune_power",
        "Tune power",
        Transmit,
        "tunepower",
        "tunepower",
        Level(100),
    ),
    setting(
        "max_power_level",
        "Max power",
        Transmit,
        "max_power_level",
        "max_power_level",
        Level(100),
    ),
    setting(
        "mic_selection",
        "Mic input",
        Transmit,
        "mic_selection",
        "mic_selection",
        Raw,
    ),
    setting(
        "mic_level",
        "Mic level",
        Transmit,
        "mic_level",
        "miclevel",
        Level(100),
    ),
    setting(
        "tx_dax",
        "DAX (transmit audio from DAX)",
        Transmit,
        "dax",
        "dax",
        Switch,
    ),
    setting(
        "tx_filter_low",
        "TX filter low",
        Transmit,
        "lo",
        "filter_low",
        Raw,
    ),
    setting(
        "tx_filter_high",
        "TX filter high",
        Transmit,
        "hi",
        "filter_high",
        Raw,
    ),
    setting(
        "speech_processor",
        "PROC",
        Transmit,
        "speech_processor_enable",
        "speech_processor_enable",
        Switch,
    ),
    setting(
        "speech_processor_level",
        "PROC level",
        Transmit,
        "speech_processor_level",
        "speech_processor_level",
        Raw,
    ),
    setting(
        "compander",
        "DEXP/compander",
        Transmit,
        "compander",
        "compander",
        Switch,
    ),
    setting(
        "compander_level",
        "Compander level",
        Transmit,
        "compander_level",
        "compander_level",
        Level(100),
    ),
    setting("vox", "VOX", Transmit, "vox_enable", "vox_enable", Switch),
    setting(
        "vox_level",
        "VOX level",
        Transmit,
        "vox_level",
        "vox_level",
        Level(100),
    ),
    setting(
        "vox_delay",
        "VOX delay",
        Transmit,
        "vox_delay",
        "vox_delay",
        Level(100),
    ),
    setting(
        "am_carrier",
        "AM carrier",
        Transmit,
        "am_carrier_level",
        "am_carrier",
        Level(100),
    ),
    setting(
        "monitor_level",
        "SB monitor level",
        Transmit,
        "mon_gain_sb",
        "mon_gain_sb",
        Level(100),
    ),
    setting(
        "met_in_rx",
        "Show TX meters in receive",
        Transmit,
        "met_in_rx",
        "met_in_rx",
        Switch,
    ),
];

/// The setting the Agent keeps as `id`.
#[must_use]
pub fn find(id: &str) -> Option<&'static FlexSetting> {
    SETTINGS.iter().find(|s| s.id == id)
}

/// What the Agent reads a setting by: the object and the status name, as
/// text, such as `transmit rfpower`.
fn read_name(setting: &FlexSetting) -> String {
    let object = match setting.object {
        Transmit => "transmit",
        Slice => "slice",
        Filter => "filt",
    };
    format!("{object} {}", setting.status)
}

/// A FlexRadio of `model` as a [`Definition`]: its settings, no serial
/// link, no USB devices, no requirements yet.
#[must_use]
pub fn definition(model: &str) -> Definition {
    let settings: Vec<SettingSummary> = SETTINGS
        .iter()
        .map(|setting| SettingSummary {
            id: setting.id.to_owned(),
            label: setting.label.to_owned(),
            read: read_name(setting).into_bytes(),
            values: match setting.kind {
                Switch => Some(BTreeMap::from([
                    ("off".to_owned(), 0),
                    ("on".to_owned(), 1),
                ])),
                Words(words) => Some(
                    words
                        .iter()
                        .zip(0_u8..)
                        .map(|(word, code)| ((*word).to_owned(), code))
                        .collect(),
                ),
                Level(_) | Raw => None,
            },
            range: match setting.kind {
                Level(high) => Some([0, high]),
                _ => None,
            },
            percent: matches!(setting.kind, Level(100)),
            raw: setting.kind == Raw,
            digits: None,
            write: true,
            verified: false,
            write_checked: false,
            moves: false,
            selects: setting.id == "mode",
            per_selection: setting.object == Filter
                || matches!(setting.id, "digu_offset" | "digl_offset"),
        })
        .collect();
    let checked_values = settings.len();
    Definition {
        maker: "FlexRadio".to_owned(),
        model: model.to_owned(),
        family: FAMILY.to_owned(),
        status: "draft".to_owned(),
        default_address: 0,
        controller_address: 0,
        baud_rates: Vec::new(),
        stop_bits: 1,
        rts_on: false,
        identity_answer: None,
        usb_serial: None,
        usb_serial_number_ends: None,
        usb_serial_interface: None,
        usb_audio: None,
        usb_driver: None,
        mode_codes: BTreeMap::new(),
        settings,
        requirements: Vec::new(),
        checked_values,
        verified_values: 0,
        meters: None,
    }
}

/// Whether `text` is a value the Agent keeps and sends back: short,
/// printable, and without the API's separators.
fn plain(text: &str, spaces: bool) -> bool {
    !text.is_empty()
        && text.len() <= MAX_VALUE
        && text
            .bytes()
            .all(|b| (b.is_ascii_graphic() && b != b'|' && b != b'=') || (spaces && b == b' '))
}

/// A value as the radio reported it, as the Agent keeps it.
///
/// # Errors
///
/// The text is not a value of this setting.
pub fn to_bytes(setting: &FlexSetting, text: &str) -> Result<Vec<u8>, String> {
    let odd = || format!("{text:?} is not a value for {}", setting.label);
    match setting.kind {
        Level(high) => text
            .parse::<u16>()
            .ok()
            .filter(|level| *level <= u16::from(high))
            .and_then(civ::level_to_bcd)
            .map(|bcd| bcd.to_vec())
            .ok_or_else(odd),
        Switch => match text {
            "1" | "T" | "on" | "true" => Ok(vec![1]),
            "0" | "F" | "off" | "false" => Ok(vec![0]),
            _ => Err(odd()),
        },
        Words(words) => words
            .iter()
            .zip(0_u8..)
            .find(|(word, _)| word.eq_ignore_ascii_case(text))
            .map(|(_, code)| vec![code])
            .ok_or_else(odd),
        Raw if plain(text, setting.object == Filter) => Ok(text.as_bytes().to_vec()),
        Raw => Err(odd()),
    }
}

/// A value the Agent keeps, as the text sent to the radio.
///
/// # Errors
///
/// The bytes are not a value of this setting.
pub fn to_text(setting: &FlexSetting, bytes: &[u8]) -> Result<String, String> {
    let odd = || format!("{} is not a value for {}", civ::hex(bytes), setting.label);
    match setting.kind {
        Level(high) => civ::level_from_bcd(bytes)
            .filter(|level| *level <= u16::from(high))
            .map(|level| level.to_string())
            .ok_or_else(odd),
        Switch => match bytes {
            [0] => Ok("0".to_owned()),
            [1] => Ok("1".to_owned()),
            _ => Err(odd()),
        },
        Words(words) => match bytes {
            [code] => words
                .get(usize::from(*code))
                .map(|word| (*word).to_owned())
                .ok_or_else(odd),
            _ => Err(odd()),
        },
        Raw => String::from_utf8(bytes.to_vec())
            .ok()
            .filter(|text| plain(text, setting.object == Filter))
            .ok_or_else(odd),
    }
}

/// The command that sets `setting` to `text` on slice `slice`.
#[must_use]
pub fn set_command(setting: &FlexSetting, slice: u8, text: &str) -> String {
    match setting.object {
        Transmit => format!("transmit set {}={text}", setting.set),
        Slice => format!("slice set {slice} {}={text}", setting.set),
        Filter => format!("filt {slice} {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{FAMILY, SETTINGS, definition, find, set_command, to_bytes, to_text};

    #[test]
    fn every_setting_has_its_own_id_and_reads_back_as_it_was_read() {
        let mut ids: Vec<&str> = SETTINGS.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SETTINGS.len());
        let samples = [
            "0", "1", "50", "100", "med", "ANT1", "-10", "100 2900", "USB",
        ];
        for setting in SETTINGS {
            for sample in samples {
                if let Ok(bytes) = to_bytes(setting, sample) {
                    let text = to_text(setting, &bytes);
                    assert!(text.is_ok(), "{} {sample}", setting.id);
                    assert_eq!(to_bytes(setting, &text.unwrap_or_default()), Ok(bytes));
                }
            }
        }
    }

    #[test]
    fn keeps_values_as_the_other_drivers_do() {
        let level = find("rf_power").unwrap_or_else(|| unreachable!());
        assert_eq!(to_bytes(level, "50"), Ok(vec![0x00, 0x50]));
        assert_eq!(to_bytes(level, "100"), Ok(vec![0x01, 0x00]));
        assert!(to_bytes(level, "101").is_err());
        assert!(to_bytes(level, "-1").is_err());
        let switch = find("nr").unwrap_or_else(|| unreachable!());
        assert_eq!(to_bytes(switch, "1"), Ok(vec![1]));
        assert_eq!(to_text(switch, &[0]), Ok("0".to_owned()));
        let agc = find("agc_mode").unwrap_or_else(|| unreachable!());
        assert_eq!(to_bytes(agc, "med"), Ok(vec![2]));
        assert_eq!(to_text(agc, &[3]), Ok("fast".to_owned()));
        assert!(to_text(agc, &[4]).is_err());
        let antenna = find("txant").unwrap_or_else(|| unreachable!());
        assert_eq!(to_bytes(antenna, "ANT2"), Ok(b"ANT2".to_vec()));
        // Nothing that would end a command or start another gets through.
        for bad in ["ANT1|xmit 1", "ANT 1", "a=b", "", "\u{7}"] {
            assert!(to_bytes(antenna, bad).is_err(), "{bad:?}");
        }
        let filter = find("filter").unwrap_or_else(|| unreachable!());
        assert_eq!(to_bytes(filter, "100 2900"), Ok(b"100 2900".to_vec()));
    }

    #[test]
    fn sets_each_where_it_lives() {
        let set =
            |id: &str, text: &str| set_command(find(id).unwrap_or_else(|| unreachable!()), 1, text);
        assert_eq!(set("mic_level", "40"), "transmit set miclevel=40");
        assert_eq!(set("agc_mode", "slow"), "slice set 1 agc_mode=slow");
        assert_eq!(set("rf_gain", "-10"), "slice set 1 rf_gain=-10");
        assert_eq!(set("filter", "100 2900"), "filt 1 100 2900");
    }

    #[test]
    fn is_a_definition_like_any_other() {
        let flex = definition("FLEX-6600");
        assert_eq!(
            (flex.maker.as_str(), flex.model.as_str()),
            ("FlexRadio", "FLEX-6600")
        );
        assert_eq!(flex.family, FAMILY);
        assert_eq!(flex.settings.len(), SETTINGS.len());
        assert!(
            flex.settings
                .iter()
                .all(|s| s.write && !s.write_checked && !s.verified)
        );
        let mode = flex.settings.iter().find(|s| s.id == "mode");
        assert!(mode.is_some_and(|m| m.selects && m.raw));
        let filter = flex.settings.iter().find(|s| s.id == "filter");
        assert!(filter.is_some_and(|f| f.per_selection));
        let power = flex.settings.iter().find(|s| s.id == "rf_power");
        assert!(power.is_some_and(|p| p.range == Some([0, 100]) && p.percent));
    }
}
