// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Finding a com0com virtual serial pair, so the Agent can answer on one
//! end as a radio (see `kenwood_cat`) for a program such as Winlink Express
//! on the other.
//!
//! Windows lists every serial port it has started under
//! `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM`, as the port's device and its name.
//! com0com's devices are `\Device\com0comNM`: N is 1 for a pair's A end and
//! 2 for its B end, and M is the pair's number. This was read from FL-1
//! (Windows 11 Pro, com0com 3.0.0.0) on 2026-10-29, where
//! `reg query HKLM\HARDWARE\DEVICEMAP\SERIALCOMM` listed
//! `\Device\com0com10 REG_SZ CNCA0`, `\Device\com0com20 REG_SZ CNCB0`,
//! `\Device\com0com11 REG_SZ COM7` and `\Device\com0com21 REG_SZ COM6`.
//!
//! The Agent takes a pair's B end and leaves the A end for the program.
//! Only a pair whose program end is named `COMn` is used, as programs list
//! COM ports by that name; com0com's first pair is `CNCA0`/`CNCB0` unless
//! it is renamed.

/// One com0com pair: the end the Agent answers on, and the end a program
/// opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The B end, the Agent's.
    pub agent: String,
    /// The A end, the program's.
    pub program: String,
}

/// The com0com pairs in a `reg query` listing of `SERIALCOMM`, in pair
/// order, with both ends started.
#[must_use]
pub fn pairs(listing: &str) -> Vec<Pair> {
    let mut ends: Vec<(u32, bool, String)> = listing
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let device = words.next()?;
            if words.next()? != "REG_SZ" {
                return None;
            }
            let name = words.next()?;
            let number = device
                .rsplit('\\')
                .next()?
                .strip_prefix("com0com")
                .filter(|number| number.len() >= 2)?;
            let (side, pair) = number.split_at(1);
            let a_end = match side {
                "1" => true,
                "2" => false,
                _ => return None,
            };
            Some((pair.parse().ok()?, a_end, name.to_owned()))
        })
        .collect();
    ends.sort_by_key(|(pair, _, _)| *pair);
    let mut found = Vec::new();
    for (pair, a_end, name) in &ends {
        if !*a_end {
            continue;
        }
        if let Some((_, _, agent)) = ends
            .iter()
            .find(|(other, other_a, _)| other == pair && !*other_a)
        {
            found.push(Pair {
                agent: agent.clone(),
                program: name.clone(),
            });
        }
    }
    found
}

/// The first pair a program can open by a `COMn` name.
#[must_use]
pub fn for_programs(listing: &str) -> Option<Pair> {
    pairs(listing).into_iter().find(|pair| {
        pair.program
            .strip_prefix("COM")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// This computer's `SERIALCOMM` listing, empty if it can't be read.
#[cfg(windows)]
#[must_use]
pub fn listed() -> String {
    std::process::Command::new("reg")
        .args(["query", r"HKLM\HARDWARE\DEVICEMAP\SERIALCOMM"])
        .output()
        .map(|listed| String::from_utf8_lossy(&listed.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Pair, for_programs, pairs};

    /// FL-1's listing, 2026-10-29.
    const FL1: &str = "\r\nHKEY_LOCAL_MACHINE\\HARDWARE\\DEVICEMAP\\SERIALCOMM\r\n    \\Device\\Serial0    REG_SZ    COM3\r\n    \\Device\\Silabser0    REG_SZ    COM5\r\n    \\Device\\com0com10    REG_SZ    CNCA0\r\n    \\Device\\com0com20    REG_SZ    CNCB0\r\n    \\Device\\com0com21    REG_SZ    COM6\r\n    \\Device\\com0com11    REG_SZ    COM7\r\n\r\n";

    #[test]
    fn finds_each_pair_and_its_ends() {
        assert_eq!(
            pairs(FL1),
            [
                Pair {
                    agent: "CNCB0".to_owned(),
                    program: "CNCA0".to_owned()
                },
                Pair {
                    agent: "COM6".to_owned(),
                    program: "COM7".to_owned()
                },
            ]
        );
    }

    #[test]
    fn picks_the_pair_a_program_can_open_by_a_com_name() {
        assert_eq!(
            for_programs(FL1),
            Some(Pair {
                agent: "COM6".to_owned(),
                program: "COM7".to_owned()
            })
        );
    }

    #[test]
    fn no_com0com_or_half_a_pair_is_none() {
        let radio_only = "    \\Device\\Silabser0    REG_SZ    COM5\r\n";
        assert_eq!(for_programs(radio_only), None);
        let half = "    \\Device\\com0com11    REG_SZ    COM7\r\n";
        assert_eq!(pairs(half), []);
        assert_eq!(for_programs(""), None);
    }
}
