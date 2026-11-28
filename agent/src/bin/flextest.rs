// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`: a small tray program for trying the Agent's FlexRadio work
//! one piece at a time, before it joins the Agent's tray.

// A tray program: no console window when it is double-clicked.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    loglab_agent::flex_test::start()
}
