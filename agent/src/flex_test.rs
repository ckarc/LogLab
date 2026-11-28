// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`: a small tray program of its own for trying the Agent's
//! FlexRadio work one piece at a time, away from the Agent's tray (brian,
//! 2026-11-27: "our own stand alone tray app specifically for Flex
//! Testing ... this will eventually be incorporated into the agent tray
//! app"). It never opens a radio and never keys one.
//!
//! Step 1: it notices when SmartSDR opens or closes on this computer.
//! Step 2: it notices the station apps opening and closing too. Each change
//! is a tray notice.
//!
//! It finds them in Windows' list of running programs (`tasklist`) by
//! their program files' names, as found on FL-1 (2026-11-26 and
//! 2026-11-27): `SmartSDR.exe`; `wsjtx.exe` (`C:\WSJT\wsjtx\bin`);
//! `JS8Call.exe` (`C:\Program Files\JS8Call`); `fldigi.exe`
//! (`C:\Program Files\Fldigi-4.2.13`); `MMSSTV.EXE` (`C:\Ham\MMSSTV`);
//! `VARA.exe` and `VARA Chat.exe` (`C:\VARA`); `VARAFM.exe`
//! (`C:\VARA FM`); `VarAC.exe` (`C:\VarAC`). Winlink Express (`RMS
//! Express.exe`, as `winlink` names it) isn't on FL-1 but is watched for
//! too. Windows doesn't care about the case of a file's name, so neither
//! does FlexTest.
//!
//! What it decides lives here, apart from Windows, so it is tested on every
//! platform; the tray itself is `flex_test_windows`.

use std::process::ExitCode;

/// A program FlexTest watches for: its name as people say it, and its
/// program file's name as Windows lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Program {
    /// Its name, as notices show it.
    pub name: &'static str,
    /// Its program file's name.
    pub image: &'static str,
}

/// SmartSDR, which owns the Flex's slice and screen.
pub const SMARTSDR: Program = Program {
    name: "SmartSDR",
    image: "SmartSDR.exe",
};

/// The station apps, in the order the FieldLab panel lists them.
pub const STATION_APPS: [Program; 9] = [
    Program {
        name: "WSJT-X",
        image: "wsjtx.exe",
    },
    Program {
        name: "JS8Call",
        image: "JS8Call.exe",
    },
    Program {
        name: "fldigi",
        image: "fldigi.exe",
    },
    Program {
        name: "MMSSTV",
        image: "MMSSTV.EXE",
    },
    Program {
        name: "VARA HF",
        image: "VARA.exe",
    },
    Program {
        name: "VARA Chat",
        image: "VARA Chat.exe",
    },
    Program {
        name: "VARA FM",
        image: "VARAFM.exe",
    },
    Program {
        name: "VarAC",
        image: "VarAC.exe",
    },
    Program {
        name: "Winlink Express",
        image: "RMS Express.exe",
    },
];

/// Every program watched: SmartSDR first, then the station apps.
#[must_use]
pub fn watched() -> Vec<Program> {
    std::iter::once(SMARTSDR).chain(STATION_APPS).collect()
}

/// How often FlexTest looks at what is running, in seconds.
pub const LOOK_EVERY_SECONDS: u64 = 2;

/// The program files' names in `tasklist /NH /FO CSV`'s answer, `listed`:
/// one line per running program, its name first, in quotes.
#[must_use]
pub fn running(listed: &str) -> Vec<String> {
    listed
        .lines()
        .filter_map(|line| {
            line.trim_start()
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                .map(str::to_owned)
        })
        .collect()
}

/// Whether `program` is among `running`.
#[must_use]
pub fn is_running(running: &[String], program: Program) -> bool {
    running
        .iter()
        .any(|name| name.eq_ignore_ascii_case(program.image))
}

/// A program opening or closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Which program.
    pub program: Program,
    /// Whether it is open now.
    pub open: bool,
}

impl Change {
    /// The notice for it.
    #[must_use]
    pub fn notice(self) -> String {
        let did = if self.open { "opened" } else { "closed" };
        format!("{} {did} on this computer.", self.program.name)
    }
}

/// What FlexTest has seen so far.
#[derive(Debug, Default)]
pub struct Watch {
    /// The programs open at the last look; `None` before the first.
    open: Option<Vec<Program>>,
}

/// What one look found.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// The first look: the watched programs open now.
    First(Vec<Program>),
    /// A later look: what opened or closed since the one before.
    Changes(Vec<Change>),
}

impl Watch {
    /// Takes one look at `running`, the program files' names now running.
    pub fn saw(&mut self, running: &[String]) -> Seen {
        let now: Vec<Program> = watched()
            .into_iter()
            .filter(|&program| is_running(running, program))
            .collect();
        let seen = match self.open.as_ref() {
            None => Seen::First(now.clone()),
            Some(before) => Seen::Changes(
                watched()
                    .into_iter()
                    .filter_map(|program| {
                        let (was, is) = (before.contains(&program), now.contains(&program));
                        (was != is).then_some(Change { program, open: is })
                    })
                    .collect(),
            ),
        };
        self.open = Some(now);
        seen
    }

    /// Whether SmartSDR was open at the last look.
    #[must_use]
    pub fn smartsdr_open(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.contains(&SMARTSDR))
    }
}

/// The notice for the first look: which watched programs are open.
#[must_use]
pub fn first_notice(open: &[Program]) -> String {
    if open.is_empty() {
        "Watching for SmartSDR and the station apps to open on this computer.".to_owned()
    } else {
        let names: Vec<&str> = open.iter().map(|program| program.name).collect();
        format!("Open on this computer now: {}.", names.join(", "))
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
    use super::{Change, SMARTSDR, STATION_APPS, Seen, Watch, first_notice, running};

    /// Answers in the shape of `tasklist /NH /FO CSV`: a CSV line per
    /// program (name, PID, session name, session number, memory). The
    /// numbers here are made up; the names are the ones found on FL-1.
    fn listing(names: &[&str]) -> Vec<String> {
        let lines: Vec<String> = names
            .iter()
            .map(|name| format!("\"{name}\",\"100\",\"Console\",\"1\",\"1,000 K\""))
            .collect();
        running(&lines.join("\r\n"))
    }

    fn app(name: &str) -> super::Program {
        STATION_APPS
            .into_iter()
            .find(|program| program.name == name)
            .unwrap_or(SMARTSDR)
    }

    #[test]
    fn reads_program_names_from_tasklist() {
        assert_eq!(
            listing(&["System", "VARA Chat.exe", "SmartSDR.exe"]),
            ["System", "VARA Chat.exe", "SmartSDR.exe"]
        );
        assert!(
            running("INFO: No tasks are running which match the specified criteria.").is_empty()
        );
        assert!(running("").is_empty());
    }

    #[test]
    fn the_first_look_says_what_is_open() {
        let mut watch = Watch::default();
        let seen = watch.saw(&listing(&["explorer.exe", "smartsdr.exe", "WSJTX.EXE"]));
        assert_eq!(seen, Seen::First(vec![SMARTSDR, app("WSJT-X")]));
        assert!(watch.smartsdr_open());
        assert_eq!(
            first_notice(&[SMARTSDR, app("WSJT-X")]),
            "Open on this computer now: SmartSDR, WSJT-X."
        );
        assert_eq!(
            first_notice(&[]),
            "Watching for SmartSDR and the station apps to open on this computer."
        );
    }

    #[test]
    fn later_looks_say_only_what_opened_or_closed() {
        let mut watch = Watch::default();
        assert_eq!(
            watch.saw(&listing(&["SmartSDR.exe"])),
            Seen::First(vec![SMARTSDR])
        );
        assert_eq!(
            watch.saw(&listing(&["SmartSDR.exe"])),
            Seen::Changes(vec![])
        );
        let seen = watch.saw(&listing(&["SmartSDR.exe", "VARA.exe", "VarAC.exe"]));
        assert_eq!(
            seen,
            Seen::Changes(vec![
                Change {
                    program: app("VARA HF"),
                    open: true
                },
                Change {
                    program: app("VarAC"),
                    open: true
                },
            ])
        );
        let seen = watch.saw(&listing(&["VARA.exe"]));
        assert_eq!(
            seen,
            Seen::Changes(vec![
                Change {
                    program: SMARTSDR,
                    open: false
                },
                Change {
                    program: app("VarAC"),
                    open: false
                },
            ])
        );
        assert!(!watch.smartsdr_open());
    }

    #[test]
    fn vara_and_vara_chat_are_told_apart() {
        let mut watch = Watch::default();
        assert_eq!(
            watch.saw(&listing(&["VARA Chat.exe"])),
            Seen::First(vec![app("VARA Chat")])
        );
        assert_eq!(
            watch.saw(&listing(&["VARA.exe"])),
            Seen::Changes(vec![
                Change {
                    program: app("VARA HF"),
                    open: true
                },
                Change {
                    program: app("VARA Chat"),
                    open: false
                },
            ])
        );
    }

    #[test]
    fn a_program_whose_name_only_starts_the_same_is_not_watched() {
        let mut watch = Watch::default();
        assert_eq!(
            watch.saw(&listing(&[
                "SmartSDR.exe.bak",
                "SmartSDR CAT.exe",
                "wsjtx_app.exe"
            ])),
            Seen::First(vec![])
        );
    }

    #[test]
    fn notices_name_the_program() {
        let opened = Change {
            program: app("JS8Call"),
            open: true,
        };
        assert_eq!(opened.notice(), "JS8Call opened on this computer.");
        let closed = Change {
            program: SMARTSDR,
            open: false,
        };
        assert_eq!(closed.notice(), "SmartSDR closed on this computer.");
    }
}
