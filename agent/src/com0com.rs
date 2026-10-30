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
//!
//! Where there is no such pair, the Agent installs com0com 3.0.0.0, the
//! build signed in 2017 that Windows 11 loads (checked on FL-1,
//! 2026-10-29), from its project on sourceforge.net
//! (`com0com-3.0.0.0-i386-and-x64-signed.zip`), with one `COM# <-> COM#`
//! pair. Its `ReadMe.txt` (in that installer) says `setup.exe /S` installs
//! with no questions and no pairs, and that `CNC_INSTALL_COMX_COMX_PORTS=YES`
//! and `CNC_INSTALL_CNCA0_CNCB0_PORTS=NO` choose the pairs. The package is
//! checked against the SHA-256 the Agent was built with before Windows is
//! asked to run it as administrator, and it is kept in `drivers` beside the
//! Agent for computers with no internet (Principle V), as the radios' USB
//! drivers are (see `driver`). com0com is GPL; the Agent only runs its
//! installer, and doesn't ship it.

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

/// The installer in the package for this Windows, from its
/// `PROCESSOR_ARCHITECTURE` (and `PROCESSOR_ARCHITEW6432` when the Agent
/// runs as 32-bit on 64-bit Windows). com0com 3.0.0.0 has no ARM build.
#[must_use]
pub fn installer_for(architecture: &str) -> Option<&'static str> {
    match architecture.to_ascii_uppercase().as_str() {
        "AMD64" => Some("Setup_com0com_v3.0.0.0_W7_x64_signed.exe"),
        "X86" => Some("Setup_com0com_v3.0.0.0_W7_x86_signed.exe"),
        _ => None,
    }
}

/// The batch file that installs com0com with one `COM# <-> COM#` pair and
/// nothing else, run as administrator so the choices reach the installer
/// whatever Windows passes on to an elevated program.
#[must_use]
pub fn install_script(installer: &str) -> String {
    format!(
        "@echo off\r\n\
         set CNC_INSTALL_COMX_COMX_PORTS=YES\r\n\
         set CNC_INSTALL_CNCA0_CNCB0_PORTS=NO\r\n\
         set CNC_INSTALL_START_MENU_SHORTCUTS=NO\r\n\
         \"{installer}\" /S\r\n\
         exit /b %ERRORLEVEL%\r\n"
    )
}

#[cfg(windows)]
pub use windows::install;

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::{Pair, for_programs, install_script, installer_for, listed};
    use crate::driver::{downloads, fetch, inf_in, keep, kept_folder, package_in};
    use crate::rigdef::DriverSummary;

    /// The signed package's SHA-256, as fetched from sourceforge.net on
    /// 2026-10-29 (475,063 bytes, two installers dated 2017-07-13).
    const SHA256: &str = "6E5D4359865277430D4AE88C73FB7E648A0ED8E81AEA5002478179CFCB0BB0E1";
    /// How long Windows may take to start the new pair after the install.
    const PAIR_WAIT: Duration = Duration::from_secs(30);

    /// Whether the Agent already tried this run, so Windows asks the operator
    /// to allow an install at most once each time the Agent starts.
    static TRIED: AtomicBool = AtomicBool::new(false);

    fn package() -> DriverSummary {
        DriverSummary {
            name: "com0com".to_owned(),
            download: "https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/\
                       com0com-3.0.0.0-i386-and-x64-signed.zip/download"
                .to_owned(),
            page: "https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/".to_owned(),
            file: "com0com-3.0.0.0-i386-and-x64-signed.zip".to_owned(),
            inf: String::new(),
        }
    }

    /// Installs com0com with one `COM# <-> COM#` pair, which Windows asks
    /// the operator to allow, and returns the pair once Windows has started
    /// it. Tried once each time the Agent starts.
    ///
    /// # Errors
    ///
    /// It was tried already, or couldn't be fetched, checked or installed;
    /// the text says what to do by hand.
    pub fn install() -> Result<Pair, String> {
        if TRIED.swap(true, Ordering::SeqCst) {
            return Err("com0com's install was already tried since the Agent started".to_owned());
        }
        let architecture = std::env::var("PROCESSOR_ARCHITEW6432")
            .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
            .unwrap_or_default();
        let installer = installer_for(&architecture)
            .ok_or_else(|| format!("com0com has no build for {architecture} Windows"))?;
        let driver = package();
        let kept = kept_folder();
        let package = if let Some(package) = package_in(&kept, &driver) {
            package
        } else if let Some(package) = downloads().and_then(|folder| package_in(&folder, &driver)) {
            keep(&package, &kept, &driver);
            package
        } else {
            let package = fetch(&driver, &kept).map_err(|why| by_hand(&why))?;
            keep(&package, &kept, &driver);
            package
        };
        check(&package).map_err(|why| by_hand(&why))?;
        run(&package, &kept, installer).map_err(|why| by_hand(&why))?;
        let until = Instant::now() + PAIR_WAIT;
        loop {
            if let Some(pair) = for_programs(&listed()) {
                return Ok(pair);
            }
            if Instant::now() >= until {
                return Err(by_hand(
                    "com0com installed, but Windows hasn't started its pair",
                ));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Whether `package` is the one the Agent was built to trust.
    fn check(package: &Path) -> Result<(), String> {
        let script = format!(
            "(Get-FileHash -Algorithm SHA256 -LiteralPath '{}').Hash",
            package.display().to_string().replace('\'', "''")
        );
        let hashed = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .map_err(|error| format!("can't check {}: {error}", package.display()))?;
        let hash = String::from_utf8_lossy(&hashed.stdout)
            .trim()
            .to_ascii_uppercase();
        if hash == SHA256 {
            Ok(())
        } else {
            let _ = std::fs::remove_file(package);
            Err(format!(
                "{} isn't the signed com0com 3.0.0.0 package (SHA-256 {hash}), so it was removed",
                package.display()
            ))
        }
    }

    /// Unpacks the package and runs its installer as administrator.
    fn run(package: &Path, kept: &Path, installer: &str) -> Result<(), String> {
        let unpacked = kept.join("com0com-3.0.0.0");
        std::fs::create_dir_all(&unpacked)
            .map_err(|error| format!("can't make {}: {error}", unpacked.display()))?;
        let unpacking = Command::new("tar.exe")
            .arg("-xf")
            .arg(package)
            .arg("-C")
            .arg(&unpacked)
            .output()
            .map_err(|error| format!("can't run tar.exe: {error}"))?;
        if !unpacking.status.success() {
            return Err(format!(
                "can't unpack {}: {}",
                package.display(),
                String::from_utf8_lossy(&unpacking.stderr).trim()
            ));
        }
        let setup: PathBuf = inf_in(&unpacked, installer)
            .ok_or_else(|| format!("{} has no {installer}", package.display()))?;
        let batch = unpacked.join("install-lablog-pair.cmd");
        std::fs::write(&batch, install_script(&setup.display().to_string()))
            .map_err(|error| format!("can't write {}: {error}", batch.display()))?;
        println!(
            "com0com: installing it with one COM port pair, for Winlink Express to tune the \
             radio through the Agent.\n  -> Windows asks whether to allow it: choose Yes."
        );
        let script = format!(
            "$p = Start-Process -FilePath '{}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden; \
             exit $p.ExitCode",
            batch.display().to_string().replace('\'', "''")
        );
        let finished = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map_err(|error| format!("can't start the install: {error}"))?;
        // The installer (NSIS) exits 0 when it finished; its other codes
        // aren't listed in NSIS's documentation, so they are only reported.
        // PowerShell exits 1 when the operator chose No.
        match finished.code() {
            Some(0) => Ok(()),
            Some(code) => Err(format!(
                "the install didn't finish (it said {code}; 1 is usually \"No\" to Windows' question)"
            )),
            None => Err("the install was stopped".to_owned()),
        }
    }

    /// What to do by hand when the Agent couldn't.
    fn by_hand(why: &str) -> String {
        let driver = package();
        format!(
            "{why}.\n  -> Download \"{}\" from {} and put it in Downloads or in {}; the Agent \
             installs it the next time it starts.",
            driver.file,
            driver.page,
            kept_folder().display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Pair, for_programs, install_script, installer_for, pairs};

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
    fn installs_one_com_pair_silently_for_this_windows() {
        assert_eq!(
            installer_for("AMD64"),
            Some("Setup_com0com_v3.0.0.0_W7_x64_signed.exe")
        );
        assert_eq!(
            installer_for("x86"),
            Some("Setup_com0com_v3.0.0.0_W7_x86_signed.exe")
        );
        assert_eq!(installer_for("ARM64"), None);
        let script = install_script(r"C:\LabLog\drivers\com0com-3.0.0.0\Setup.exe");
        assert!(script.contains("set CNC_INSTALL_COMX_COMX_PORTS=YES\r\n"));
        assert!(script.contains("set CNC_INSTALL_CNCA0_CNCB0_PORTS=NO\r\n"));
        assert!(script.contains("\"C:\\LabLog\\drivers\\com0com-3.0.0.0\\Setup.exe\" /S\r\n"));
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
