// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`: a small tray program of its own for trying the Agent's
//! FlexRadio work one piece at a time, away from the Agent's tray (brian,
//! 2026-11-27: "our own stand alone tray app specifically for Flex
//! Testing ... this will eventually be incorporated into the agent tray
//! app"). It never opens a radio and never keys one.
//!
//! Step 1: it notices when SmartSDR opens or closes on this computer and
//! says so in a tray notice. It looks for SmartSDR's program in Windows'
//! list of running programs, `SmartSDR.exe`, the name `tasklist` showed for
//! it on FL-1 (2026-11-26 and 2026-11-27).
//!
//! What it decides lives here, apart from Windows, so it is tested on every
//! platform; the tray itself is `flex_test_windows`.

use std::process::ExitCode;

/// SmartSDR's program, as Windows lists it among running programs.
pub const SMARTSDR: &str = "SmartSDR.exe";

/// How often FlexTest looks for SmartSDR, in seconds.
pub const LOOK_EVERY_SECONDS: u64 = 2;

/// Whether `image` is among the programs in `tasklist /FO CSV /NH`'s
/// answer, `listed`: one line per program, its name first, in quotes.
#[must_use]
pub fn listed(listed: &str, image: &str) -> bool {
    listed.lines().any(|line| {
        line.trim_start()
            .strip_prefix('"')
            .and_then(|rest| rest.split('"').next())
            .is_some_and(|name| name.eq_ignore_ascii_case(image))
    })
}

/// A change in SmartSDR worth a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// SmartSDR was already open when FlexTest started.
    AlreadyOpen,
    /// SmartSDR wasn't open when FlexTest started.
    NotOpen,
    /// SmartSDR opened.
    Opened,
    /// SmartSDR closed.
    Closed,
}

impl Change {
    /// The notice for it.
    #[must_use]
    pub fn notice(self) -> &'static str {
        match self {
            Self::AlreadyOpen => "SmartSDR is open on this computer.",
            Self::NotOpen => "Watching for SmartSDR to open on this computer.",
            Self::Opened => "SmartSDR opened on this computer.",
            Self::Closed => "SmartSDR closed on this computer.",
        }
    }

    /// Whether SmartSDR is open after it.
    #[must_use]
    pub fn open(self) -> bool {
        matches!(self, Self::AlreadyOpen | Self::Opened)
    }
}

/// What FlexTest has seen of SmartSDR so far.
#[derive(Debug, Default)]
pub struct Watch {
    open: Option<bool>,
}

impl Watch {
    /// Takes one look's answer, `open`, and returns what changed, if
    /// anything. The first look always says what it found.
    pub fn saw(&mut self, open: bool) -> Option<Change> {
        let change = match (self.open, open) {
            (None, true) => Some(Change::AlreadyOpen),
            (None, false) => Some(Change::NotOpen),
            (Some(false), true) => Some(Change::Opened),
            (Some(true), false) => Some(Change::Closed),
            (Some(_), _) => None,
        };
        self.open = Some(open);
        change
    }
}

/// Runs `FlexTest`'s tray until Quit.
#[must_use]
pub fn start() -> ExitCode {
    #[cfg(windows)]
    let outcome = crate::flex_test_windows::run();
    #[cfg(not(windows))]
    let outcome: Result<(), String> =
        Err("FlexTest is a Windows tray program; it has nothing to do here".to_owned());
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            println!("FlexTest: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, SMARTSDR, Watch, listed};

    /// Answers in the shape of `tasklist /FI "IMAGENAME eq SmartSDR.exe" /NH
    /// /FO CSV`: a CSV line per program (name, PID, session name, session
    /// number, memory), or a sentence when none matches (FL-1 said "No tasks
    /// are running which match the specified criteria." on 2026-11-26). The
    /// numbers here are made up.
    const RUNNING: &str = "\"SmartSDR.exe\",\"16348\",\"Console\",\"1\",\"312,404 K\"\r\n";
    const CLOSED: &str = "INFO: No tasks are running which match the specified criteria.\r\n";

    #[test]
    fn finds_smartsdr_in_the_list_of_running_programs() {
        assert!(listed(RUNNING, SMARTSDR));
        assert!(listed(&RUNNING.to_ascii_lowercase(), SMARTSDR));
        assert!(!listed(CLOSED, SMARTSDR));
        assert!(!listed("", SMARTSDR));
    }

    #[test]
    fn a_program_whose_name_only_starts_the_same_is_not_smartsdr() {
        assert!(!listed(
            "\"SmartSDR.exe.bak\",\"1\",\"Console\",\"1\",\"1 K\"",
            SMARTSDR
        ));
        assert!(!listed(
            "\"SmartSDR CAT.exe\",\"1\",\"Console\",\"1\",\"1 K\"",
            SMARTSDR
        ));
    }

    #[test]
    fn says_what_it_found_first_then_only_changes() {
        let mut watch = Watch::default();
        assert_eq!(watch.saw(false), Some(Change::NotOpen));
        assert_eq!(watch.saw(false), None);
        assert_eq!(watch.saw(true), Some(Change::Opened));
        assert_eq!(watch.saw(true), None);
        assert_eq!(watch.saw(false), Some(Change::Closed));
        assert_eq!(watch.saw(true), Some(Change::Opened));
    }

    #[test]
    fn smartsdr_open_at_start_is_said_once() {
        let mut watch = Watch::default();
        assert_eq!(watch.saw(true), Some(Change::AlreadyOpen));
        assert!(Change::AlreadyOpen.open());
        assert_eq!(watch.saw(true), None);
        assert_eq!(watch.saw(false), Some(Change::Closed));
        assert!(!Change::Closed.open());
    }
}
