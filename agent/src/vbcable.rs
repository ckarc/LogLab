// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Getting VB-CABLE onto a Windows computer that hasn't got it, so the
//! radio's receive audio reaches programs (see `audio_windows`).
//!
//! VB-CABLE is VB-Audio's (www.vb-cable.com), not LabLog's: VB-CABLE is a
//! donationware, all participations are welcome. VB-Audio allows "the
//! distribution of the VB-CABLE package AS IS together with your
//! application", embedded in its installer, where the end user can tell it
//! is VB-Audio's and can donate (vb-audio.com, Services, Licensing,
//! "VB-CABLE Distribution with another product", read 2026-11-21). So the
//! Agent doesn't carry it: it fetches the package unchanged from VB-Audio,
//! checks it is the one it was built for, keeps a copy in `drivers` beside
//! it for the next computer (Principle V), names VB-Audio as it does, and
//! runs VB-Audio's own setup, which Windows asks the operator to allow.
//!
//! The package's readme says to "launch the Setup program from there (! IN
//! ADMINISTRATOR MODE !)", that `VBCABLE_Setup_x64.exe` is for 64-bit
//! Windows and `VBCABLE_Setup.exe` for 32-bit, and that "To finalize
//! installation, you must reboot your computer". It names no silent
//! install, so VB-Audio's setup window shows: its Install Driver button is
//! the operator's to press.
//!
//! VB-CABLE A+B, the second cable the Agent carries transmit audio on, may
//! not be bundled ("Other VB-CABLEs like VB-CABLE A+B or C+D cannot be
//! distributed or bundled with other product", same page), so it is never
//! fetched: without it, a program's transmit audio goes straight to the
//! radio's own USB sound device.

/// Where VB-CABLE comes from, and what VB-Audio asks to be said of it.
pub const CREDIT: &str = "VB-CABLE is VB-Audio's (www.vb-cable.com): VB-CABLE is a donationware, \
                          all participations are welcome.";

/// VB-Audio's setup for Windows on `architecture` (as
/// `PROCESSOR_ARCHITECTURE` names it), as the package's readme names them.
#[must_use]
pub fn setup_for(architecture: &str) -> Option<&'static str> {
    match architecture.to_ascii_uppercase().as_str() {
        "AMD64" => Some("VBCABLE_Setup_x64.exe"),
        "X86" => Some("VBCABLE_Setup.exe"),
        _ => None,
    }
}

#[cfg(windows)]
pub use windows::install;

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{CREDIT, setup_for};
    use crate::driver::{downloads, fetch, inf_in, keep, kept_folder, package_in};
    use crate::rigdef::DriverSummary;

    /// The package's SHA-256, as fetched from download.vb-audio.com on
    /// 2026-11-21 (1,324,680 bytes; its setups dated 2024).
    const SHA256: &str = "BC44E8974191A5A2427D6C979E2DAB08DBB09925F3F435DFA2F0BA3E45B6EB90";

    /// Whether the Agent already tried this run, so Windows asks the
    /// operator to allow an install at most once each time it starts.
    static TRIED: AtomicBool = AtomicBool::new(false);

    fn package() -> DriverSummary {
        DriverSummary {
            name: "VB-CABLE".to_owned(),
            download: "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip"
                .to_owned(),
            page: "https://vb-audio.com/Cable/index.htm".to_owned(),
            file: "VBCABLE_Driver_Pack46.zip".to_owned(),
            inf: String::new(),
        }
    }

    /// Fetches VB-CABLE's package (or uses the copy in `drivers`, or one in
    /// Downloads), checks it, and runs VB-Audio's setup as administrator.
    /// Tried once each time the Agent starts. Returns what to tell the
    /// operator: the computer must restart before the cable works.
    ///
    /// # Errors
    ///
    /// It was tried already, or couldn't be fetched, checked or installed;
    /// the text says what to do by hand.
    pub fn install() -> Result<String, String> {
        if TRIED.swap(true, Ordering::SeqCst) {
            return Err("VB-CABLE's install was already tried since the Agent started".to_owned());
        }
        let architecture = std::env::var("PROCESSOR_ARCHITEW6432")
            .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
            .unwrap_or_default();
        let setup = setup_for(&architecture).ok_or_else(|| {
            by_hand(&format!(
                "the Agent has no VB-CABLE setup for {architecture} Windows"
            ))
        })?;
        let driver = package();
        let kept = kept_folder();
        println!(
            "VB-CABLE: not installed, so the radio's audio can't reach programs yet.\n  {CREDIT}"
        );
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
        run(&package, &kept, setup).map_err(|why| by_hand(&why))?;
        Ok(format!(
            "VB-CABLE: VB-Audio's setup has finished.\n  -> Restart this computer: VB-CABLE works \
             once Windows has restarted, and the Agent carries the radio's audio from then on.\n  \
             {CREDIT}"
        ))
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
                "{} isn't the VB-CABLE package the Agent was built for (SHA-256 {hash}), so it was \
                 removed",
                package.display()
            ))
        }
    }

    /// Unpacks the package and runs VB-Audio's setup as administrator.
    fn run(package: &Path, kept: &Path, setup: &str) -> Result<(), String> {
        let unpacked = kept.join("VBCABLE_Driver_Pack46");
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
        let setup = inf_in(&unpacked, setup)
            .ok_or_else(|| format!("{} has no {setup}", package.display()))?;
        println!(
            "VB-CABLE: starting VB-Audio's setup.\n  -> Windows asks whether to allow it: choose \
             Yes. Then press Install Driver in VB-Audio's window, and OK when it says it is done."
        );
        let script = format!(
            "$p = Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs -Wait -PassThru; \
             exit $p.ExitCode",
            setup.display().to_string().replace('\'', "''"),
            unpacked.display().to_string().replace('\'', "''")
        );
        let finished = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map_err(|error| format!("can't start VB-Audio's setup: {error}"))?;
        // VB-Audio documents no exit codes for its setup, so anything but
        // PowerShell's own 1 (the operator chose No) counts as having run;
        // the Agent sees whether the cable is there after the restart.
        match finished.code() {
            Some(1) => Err("Windows wasn't allowed to run VB-Audio's setup".to_owned()),
            Some(_) => Ok(()),
            None => Err("VB-Audio's setup was stopped".to_owned()),
        }
    }

    /// What to do by hand when the Agent couldn't.
    fn by_hand(why: &str) -> String {
        let driver = package();
        format!(
            "{why}.\n  -> Download \"{}\" from {}, unzip it, and run {} as administrator, then \
             restart the computer. {CREDIT}",
            driver.file,
            driver.page,
            setup_for("AMD64").unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{CREDIT, setup_for};

    #[test]
    fn runs_vb_audios_own_setup_for_this_windows_and_names_vb_audio() {
        assert_eq!(setup_for("AMD64"), Some("VBCABLE_Setup_x64.exe"));
        assert_eq!(setup_for("x86"), Some("VBCABLE_Setup.exe"));
        assert_eq!(setup_for("ARM64"), None);
        assert!(CREDIT.contains("www.vb-cable.com"));
        assert!(CREDIT.contains("donationware"));
    }
}
