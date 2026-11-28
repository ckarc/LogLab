// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `SoundCardTest`: plugs in the Agent's own sound card, "LabLog Radio",
//! plays a tone through it and checks it comes back (`src/sound_card.rs`).

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    loglab_agent::sound_card::start(&args)
}
