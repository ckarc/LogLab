// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Getting a radio's USB serial driver onto a Windows computer.
//!
//! Some radios' USB serial bridges have no driver in Windows: the IC-7300's
//! Silicon Labs `CP210x` is one. Without it the radio shows up in Device
//! Manager under "Other devices" with no COM port, and nothing can control
//! it. The radio's definition names the driver (`[usb.serial.driver]`), and
//! the Agent:
//!
//! 1. Asks Windows for connected devices with a problem
//!    (`pnputil /enum-devices /connected /problem`, Windows 10 1903 and
//!    later) and looks for the bridge's USB IDs among them.
//! 2. Looks for the driver's package in `drivers` beside the Agent, where it
//!    keeps a copy, and in the operator's Downloads folder.
//! 3. Fetches the package from its maker with Windows' own `curl.exe`, with a
//!    time limit, if it has none.
//! 4. If that fails, opens the maker's download page in the browser and
//!    waits for the package to arrive in Downloads.
//! 5. Unpacks it with Windows' own `tar.exe` and installs it with
//!    `pnputil /add-driver ... /install`, which Windows asks the operator to
//!    allow. Windows itself refuses a driver whose signature doesn't check
//!    out.
//!
//! The package is kept in `drivers`, so the next computer can be given the
//! folder and install with no internet (Principle V). The maker's license
//! allows that copy; it doesn't allow shipping the driver with the Agent.

use std::path::{Path, PathBuf};

use crate::rigdef::DriverSummary;

/// Whether `listing`, what `pnputil /enum-devices /problem` printed, holds a
/// device with these USB IDs: a device Windows has no working driver for.
/// Only the IDs are looked for, so it works whatever language Windows
/// prints its labels in.
#[must_use]
pub fn missing(listing: &str, vid: u16, pid: u16) -> bool {
    let ids = format!("VID_{vid:04X}&PID_{pid:04X}");
    listing
        .lines()
        .any(|line| line.to_ascii_uppercase().contains(&ids))
}

/// The driver's package in `folder`, if it is there: saved under its own
/// name, or as a browser renames a second download of it ("name (1).zip").
#[must_use]
pub fn package_in(folder: &Path, driver: &DriverSummary) -> Option<PathBuf> {
    let exact = folder.join(&driver.file);
    if exact.is_file() {
        return Some(exact);
    }
    let stem = driver.file.strip_suffix(".zip").unwrap_or(&driver.file);
    let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| renamed(name, stem))
        })
        .collect();
    found.sort();
    found.pop()
}

/// Whether `name` is `stem` as a browser saves another copy of it:
/// `stem (2).zip`.
fn renamed(name: &str, stem: &str) -> bool {
    name.strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix(" ("))
        .and_then(|rest| rest.strip_suffix(").zip"))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The driver's INF file somewhere under `folder`, the unpacked package.
#[must_use]
pub fn inf_in(folder: &Path, inf: &str) -> Option<PathBuf> {
    let mut folders = vec![folder.to_path_buf()];
    while let Some(here) = folders.pop() {
        for entry in std::fs::read_dir(&here).ok()?.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(inf))
            {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(windows)]
pub use windows::{downloads, ensure, fetch, keep, kept_folder};

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::{inf_in, missing, package_in};
    use crate::rigdef::{Definition, DriverSummary};

    /// How long the Agent's own fetch may take.
    const FETCH_SECONDS: &str = "90";
    /// How long the Agent waits for the operator's browser download.
    const BROWSER_WAIT: Duration = Duration::from_mins(15);

    /// Whether the Agent already tried this run, so Windows asks the operator
    /// to allow an install at most once each time the Agent starts.
    static TRIED: AtomicBool = AtomicBool::new(false);

    /// Installs the driver of any known radio that is plugged in without one.
    /// Says what it is doing as it goes. Returns whether it installed one,
    /// so the caller looks for the radio's port again.
    ///
    /// # Errors
    ///
    /// The driver is missing and couldn't be fetched or installed; the text
    /// says what to do by hand.
    pub fn ensure(definitions: &[Definition], running: &AtomicBool) -> Result<bool, String> {
        let needed: Vec<(u16, u16, &DriverSummary)> = definitions
            .iter()
            .filter_map(|d| Some((d.usb_serial?, d.usb_driver.as_ref()?)))
            .map(|((vid, pid), driver)| (vid, pid, driver))
            .collect();
        if needed.is_empty() || TRIED.swap(true, Ordering::SeqCst) {
            return Ok(false);
        }
        let listing = problem_devices()?;
        let Some((_, _, driver)) = needed
            .into_iter()
            .find(|(vid, pid, _)| missing(&listing, *vid, *pid))
        else {
            return Ok(false);
        };
        println!(
            "Driver: the radio is plugged in, but Windows has no {} driver for it.",
            driver.name
        );
        let kept = kept_folder();
        let downloaded = downloads().and_then(|folder| package_in(&folder, driver));
        let package = if let Some(package) = package_in(&kept, driver) {
            println!("Driver: using the copy in {}.", kept.display());
            package
        } else if let Some(package) = downloaded {
            println!("Driver: using {} from Downloads.", package.display());
            keep(&package, &kept, driver);
            package
        } else {
            let package = fetch(driver, &kept)
                .or_else(|why| {
                    println!("Driver: couldn't fetch it ({why}).");
                    from_browser(driver, running)
                })
                .map_err(|why| by_hand(driver, &why))?;
            keep(&package, &kept, driver);
            package
        };
        install(&package, &kept, driver).map_err(|why| by_hand(driver, &why))?;
        println!("Driver: {} is installed.", driver.name);
        Ok(true)
    }

    /// `drivers` beside the Agent.
    pub fn kept_folder() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|folder| folder.join("drivers")))
            .unwrap_or_else(|| "drivers".into())
    }

    /// The operator's Downloads folder.
    pub fn downloads() -> Option<PathBuf> {
        std::env::var_os("USERPROFILE").map(|home| Path::new(&home).join("Downloads"))
    }

    fn problem_devices() -> Result<String, String> {
        let listed = Command::new("pnputil")
            .args(["/enum-devices", "/connected", "/problem"])
            .output()
            .map_err(|error| format!("can't ask Windows which devices have no driver: {error}"))?;
        Ok(String::from_utf8_lossy(&listed.stdout).into_owned())
    }

    /// Fetches the package into `folder` with Windows' own curl.exe.
    pub fn fetch(driver: &DriverSummary, folder: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("can't make {}: {error}", folder.display()))?;
        let package = folder.join(&driver.file);
        let partial = folder.join(format!("{}.part", driver.file));
        println!("Driver: fetching it from {} ...", driver.download);
        let fetched = Command::new("curl.exe")
            .args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--max-time", FETCH_SECONDS, "--output"])
            .arg(&partial)
            .arg(&driver.download)
            .output()
            .map_err(|error| format!("can't run curl.exe: {error}"))?;
        if !fetched.status.success() {
            let _ = std::fs::remove_file(&partial);
            return Err(String::from_utf8_lossy(&fetched.stderr).trim().to_owned());
        }
        std::fs::rename(&partial, &package)
            .map_err(|error| format!("can't keep {}: {error}", package.display()))?;
        Ok(package)
    }

    /// Opens the maker's download page and waits for the package to arrive
    /// in Downloads.
    fn from_browser(driver: &DriverSummary, running: &AtomicBool) -> Result<PathBuf, String> {
        let folder = downloads().ok_or("can't tell where your Downloads folder is")?;
        if let Some(package) = package_in(&folder, driver) {
            println!("Driver: found {} in Downloads.", driver.file);
            return Ok(package);
        }
        println!(
            "Driver: opening {} in your browser.\n  -> Download \"{}\" there. The Agent \
             watches Downloads and carries on when it arrives.",
            driver.page, driver.file
        );
        // `start` opens the address in the default browser; "" is the window
        // title it would otherwise take the address for.
        let _ = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&driver.page)
            .status();
        let until = Instant::now() + BROWSER_WAIT;
        while running.load(Ordering::SeqCst) && Instant::now() < until {
            if let Some(package) = package_in(&folder, driver) {
                // A browser writes the file under another name until it is
                // done, so this is the whole file.
                return Ok(package);
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        Err(format!(
            "{} didn't arrive in Downloads within {} minutes",
            driver.file,
            BROWSER_WAIT.as_secs() / 60
        ))
    }

    /// Keeps a copy of the package in `drivers`, for the next computer.
    pub fn keep(package: &Path, kept: &Path, driver: &DriverSummary) {
        let copy = kept.join(&driver.file);
        if package == copy {
            return;
        }
        match std::fs::create_dir_all(kept).and_then(|()| std::fs::copy(package, &copy)) {
            Ok(_) => println!(
                "Driver: kept a copy in {}, for the next computer.",
                kept.display()
            ),
            Err(error) => println!(
                "Driver: couldn't keep a copy in {}: {error}",
                kept.display()
            ),
        }
    }

    /// Unpacks the package and installs its driver, which Windows asks the
    /// operator to allow.
    fn install(package: &Path, kept: &Path, driver: &DriverSummary) -> Result<(), String> {
        let stem = driver.file.strip_suffix(".zip").unwrap_or(&driver.file);
        let unpacked = kept.join(stem);
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
        let inf = inf_in(&unpacked, &driver.inf)
            .ok_or_else(|| format!("{} has no {}", package.display(), driver.inf))?;
        println!(
            "Driver: installing {}.\n  -> Windows asks whether to allow it: choose Yes.",
            inf.display()
        );
        // pnputil needs an administrator, so it is started through
        // PowerShell's "run as administrator", which is what asks.
        let script = format!(
            "$p = Start-Process -FilePath pnputil -ArgumentList '/add-driver','\"{}\"','/install' \
             -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode",
            inf.display().to_string().replace('\'', "''")
        );
        let installed = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map_err(|error| format!("can't start the install: {error}"))?;
        // pnputil's return values (learn.microsoft.com, "PnPUtil return
        // values"): 0 done; 3010 done, and Windows wants a restart; 259 no
        // device matched the driver. PowerShell exits 1 when the operator
        // chose No.
        match installed.code() {
            Some(0 | 3010) => Ok(()),
            Some(259) => Err("Windows found no device the driver fits".to_owned()),
            Some(1) => Err("Windows wasn't allowed to install it".to_owned()),
            Some(code) => Err(format!("the install didn't finish (it said {code})")),
            None => Err("the install was stopped".to_owned()),
        }
    }

    /// What to do by hand when the Agent couldn't.
    fn by_hand(driver: &DriverSummary, why: &str) -> String {
        format!(
            "the radio's {} driver isn't installed, and the Agent couldn't install it: {why}.\n\
             -> Download \"{}\" from {} and put it in Downloads or in {}; the Agent installs it \
             the next time it starts.",
            driver.name,
            driver.file,
            driver.page,
            kept_folder().display()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{inf_in, missing, package_in, renamed};
    use crate::rigdef::DriverSummary;

    /// What pnputil printed for an IC-7300 with no driver, in its usual shape.
    const LISTING: &str = "Microsoft PnP Utility\r\n\r\n\
        Instance ID:                USB\\VID_10C4&PID_EA60\\IC-7300_02065962\r\n\
        Device Description:         CP2102 USB to UART Bridge Controller\r\n\
        Class Name:                 Other devices\r\n\
        Status:                     Problem\r\n\
        Problem Code:               28 (0x1C) [CM_PROB_FAILED_INSTALL]\r\n";

    fn driver() -> DriverSummary {
        DriverSummary {
            name: "Silicon Labs CP210x".to_owned(),
            download: "https://example.invalid/x.zip".to_owned(),
            page: "https://example.invalid/".to_owned(),
            file: "CP210x_Universal_Windows_Driver.zip".to_owned(),
            inf: "silabser.inf".to_owned(),
        }
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let folder =
            std::env::temp_dir().join(format!("lablog-driver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        folder
    }

    #[test]
    fn finds_the_radio_among_devices_with_no_driver() {
        assert!(missing(LISTING, 0x10C4, 0xEA60));
        assert!(missing(&LISTING.to_ascii_lowercase(), 0x10C4, 0xEA60));
        assert!(!missing(LISTING, 0x10C4, 0xEA63), "another bridge");
        assert!(!missing(
            "Microsoft PnP Utility\r\n\r\nNo devices were found.",
            0x10C4,
            0xEA60
        ));
    }

    #[test]
    fn finds_the_package_as_a_browser_saves_it() {
        let downloads = folder("downloads");
        assert_eq!(package_in(&downloads, &driver()), None);
        let second = downloads.join("CP210x_Universal_Windows_Driver (1).zip");
        std::fs::write(&second, b"zip").unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(package_in(&downloads, &driver()), Some(second));
        let first = downloads.join("CP210x_Universal_Windows_Driver.zip");
        std::fs::write(&first, b"zip").unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(package_in(&downloads, &driver()), Some(first));
        assert!(renamed("x (12).zip", "x"));
        assert!(!renamed("x (1).zip.crdownload", "x"), "still downloading");
        assert!(!renamed("x ().zip", "x"));
        assert!(!renamed("y (1).zip", "x"));
        let _ = std::fs::remove_dir_all(&downloads);
        assert_eq!(package_in(Path::new("/no/such/folder"), &driver()), None);
    }

    #[test]
    fn finds_the_inf_anywhere_in_the_package() {
        let unpacked = folder("unpacked");
        let deep = unpacked.join("CP210x_Universal_Windows_Driver").join("x64");
        std::fs::create_dir_all(&deep).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(inf_in(&unpacked, "silabser.inf"), None);
        std::fs::write(deep.join("SILABSER.INF"), b"inf")
            .unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            inf_in(&unpacked, "silabser.inf"),
            Some(deep.join("SILABSER.INF"))
        );
        let _ = std::fs::remove_dir_all(&unpacked);
    }
}
