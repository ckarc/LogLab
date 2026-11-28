// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent`: the LabLog Station Agent.

fn main() -> std::process::ExitCode {
    loglab_agent::start(&loglab_agent::product::AGENT)
}
