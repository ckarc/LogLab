// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `RadioAgent`: the radio side of the LabLog Station Agent, standing alone
//! on one computer with no server.

fn main() -> std::process::ExitCode {
    loglab_agent::start(&loglab_agent::product::RADIO_AGENT)
}
