// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LogLab's FT8 encoder must send exactly what WSJT-X sends.
//!
//! `tests/data/ft8-wsjtx-tones.tsv` holds the tones WSJT-X's own `ft8code`
//! produced for over a thousand messages: random QSO messages from the bench
//! generator plus hand-picked edge cases. Every one must match tone for tone.

use loglab_agent::ft8::encode;

const FIXTURE: &str = include_str!("data/ft8-wsjtx-tones.tsv");

#[test]
fn every_message_encodes_to_the_same_tones_as_wsjtx() {
    let mut checked = 0;
    let mut wrong = Vec::new();
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let Some((message, expected)) = line.split_once('\t') else {
            wrong.push(format!("malformed fixture line: {line}"));
            continue;
        };
        match encode::tones(message) {
            Ok(tones) if encode::tones_to_string(&tones) == expected => {}
            Ok(tones) => wrong.push(format!(
                "{message}: got {}, WSJT-X {expected}",
                encode::tones_to_string(&tones)
            )),
            Err(e) => wrong.push(format!("{message}: {e}")),
        }
        checked += 1;
    }
    assert!(checked > 1_000, "only {checked} messages in the fixture");
    assert!(
        wrong.is_empty(),
        "{} mismatches:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
