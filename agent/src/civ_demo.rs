// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent civ-demo`: a short CI-V conversation with the pretend
//! IC-7300, printed byte by byte with what each message means.
//!
//! It is there to show what the Agent and a radio say to each other, and to
//! let anyone check that the pretend radio answers the way the IC-7300's
//! documentation says the real one does. Nothing here talks to a real radio.

use crate::civ::{self, Decoder, Frame, NG, OK};
use crate::civ_pretend::PretendCiv;
use crate::rigdef::{self, Definition};

/// The IC-7300 definition, built into the Agent so the demo runs anywhere.
const IC_7300: &str = include_str!("../rigs/icom/ic-7300.toml");

/// Runs the conversation and returns what it printed, one line per entry.
///
/// # Errors
///
/// The built-in IC-7300 definition did not pass its checks.
pub fn run() -> Result<Vec<String>, String> {
    let definition = rigdef::parse(IC_7300)
        .map_err(|problems| format!("the built-in IC-7300 definition is refused: {problems:?}"))?;
    let mut radio = PretendCiv::new(&definition);
    let me = definition.controller_address;
    let them = definition.default_address;
    let mut lines = vec![
        format!(
            "A conversation with a pretend {} {} at address {them:02X}h. The Agent is {me:02X}h.",
            definition.maker, definition.model
        ),
        "Every message is FE FE, to, from, what it says, FD.".to_owned(),
        String::new(),
    ];
    let steps: [(&str, Vec<u8>); 10] = [
        ("Who are you?", vec![0x19, 0x00]),
        ("What frequency are you on?", vec![0x03]),
        ("What mode?", vec![0x04]),
        ("Is data mode on?", vec![0x1A, 0x06]),
        (
            "Tune to 7.074 MHz.",
            [[0x05].as_slice(), &bcd(7_074_000)].concat(),
        ),
        ("What frequency are you on now?", vec![0x03]),
        (
            "Where does transmit audio come from in data mode? (DATA MOD)",
            vec![0x1A, 0x05, 0x00, 0x67],
        ),
        (
            "How loud is USB transmit audio? (USB MOD Level)",
            vec![0x1A, 0x05, 0x00, 0x65],
        ),
        (
            "Tune to 80 MHz.",
            [[0x05].as_slice(), &bcd(80_000_000)].concat(),
        ),
        ("Are you transmitting?", vec![0x1C, 0x00]),
    ];
    for (question, payload) in steps {
        let sent = Frame::new(them, me, &payload);
        lines.push(format!(
            "Agent -> {}   {question}",
            civ::hex(&sent.encode())
        ));
        for frame in Decoder::default().push(&radio.receive(&sent.encode())) {
            lines.push(format!(
                "radio <- {}   {}",
                civ::hex(&frame.encode()),
                meaning(&definition, &payload, &frame.payload)
            ));
        }
        lines.push(String::new());
    }
    lines.push("Someone turns the radio's dial to 7.076 MHz.".to_owned());
    for frame in Decoder::default().push(&radio.turn_dial(7_076_000)) {
        lines.push(format!(
            "radio <- {}   (on its own, to everyone: 00h) {}",
            civ::hex(&frame.encode()),
            meaning(&definition, &[0x00], &frame.payload)
        ));
    }
    Ok(lines)
}

fn bcd(hz: u64) -> Vec<u8> {
    civ::frequency_to_bcd(hz).map(Vec::from).unwrap_or_default()
}

/// What the radio's answer means, in plain words.
fn meaning(definition: &Definition, asked: &[u8], answer: &[u8]) -> String {
    match answer {
        [OK] => "Done (FB).".to_owned(),
        [NG] => "Can't do that (FA).".to_owned(),
        [0x19, 0x00, address] => format!("I'm the radio at address {address:02X}h."),
        [0x00 | 0x03, bcd @ ..] => match civ::frequency_from_bcd(bcd) {
            Some(hz) => format!(
                "{} MHz (the frequency, lowest digits first).",
                megahertz(hz)
            ),
            None => "a frequency that isn't valid BCD".to_owned(),
        },
        [0x04, mode, filter] => format!("{}, filter FIL{filter}.", mode_name(definition, *mode)),
        [0x1A, 0x06, data, _] => {
            if *data == 0x01 {
                "Data mode is on.".to_owned()
            } else {
                "Data mode is off.".to_owned()
            }
        }
        [0x1A, 0x05, a, b, value @ ..] => setting_meaning(definition, [*a, *b], value),
        [0x1C, 0x00, on] => {
            if *on == 0x01 {
                "Transmitting.".to_owned()
            } else {
                "Receiving, not transmitting.".to_owned()
            }
        }
        _ => format!("(an answer to {})", civ::hex(asked)),
    }
}

fn megahertz(hz: u64) -> String {
    format!("{}.{:06}", hz / 1_000_000, hz % 1_000_000)
}

fn mode_name(definition: &Definition, code: u8) -> String {
    definition
        .mode_codes
        .iter()
        .find(|(_, c)| **c == code)
        .map_or_else(|| format!("mode {code:02X}h"), |(name, _)| name.clone())
}

fn setting_meaning(definition: &Definition, item: [u8; 2], value: &[u8]) -> String {
    let Some(setting) = definition.settings.iter().find(|s| s.item == item) else {
        return "a setting the definition doesn't list".to_owned();
    };
    if let (Some(values), [code]) = (&setting.values, value) {
        let name = values
            .iter()
            .find(|(_, c)| **c == *code)
            .map_or("an unknown value", |(name, _)| name.as_str());
        return format!("{} is {name} ({code:02X}h).", setting.id);
    }
    match civ::level_from_bcd(value) {
        Some(level) => format!(
            "{} is {level} of 255, about {}% (two BCD bytes).",
            setting.id,
            (u32::from(level) * 100 + 127) / 255
        ),
        None => format!("{} is {}", setting.id, civ::hex(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn the_demo_conversation_runs_and_reads_right() {
        let lines = run().unwrap_or_else(|error| unreachable!("{error}"));
        let text = lines.join("\n");
        for expected in [
            "Agent -> FE FE 94 E0 19 00 FD   Who are you?",
            "radio <- FE FE E0 94 19 00 94 FD   I'm the radio at address 94h.",
            "radio <- FE FE E0 94 03 00 40 07 14 00 FD   14.074000 MHz",
            "USB, filter FIL1.",
            "Data mode is on.",
            "7.074000 MHz",
            "data_mod is MIC (00h).",
            "usb_mod_level is 127 of 255",
            "Can't do that (FA).",
            "Receiving, not transmitting.",
            "radio <- FE FE 00 94 00 00 60 07 07 00 FD   (on its own, to everyone: 00h) 7.076000 MHz",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }
}
