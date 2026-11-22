// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Opening SmartSDR, FlexRadio's own program, for the "Use FlexRadio"
//! checkmark: a FlexRadio has a slice to tune only while SmartSDR (or
//! another GUI client) has one open, so the Agent opens it when a radio
//! program opens and it isn't open already.
//!
//! SmartSDR installs into `Program Files\SmartSDR vX.Y.Z\SmartSDR\
//! SmartSDR.exe`, one folder per version (FieldLab's FL-1 has
//! `SmartSDR v4.3.19`); the newest one installed is opened. With its
//! `AutoConnect` setting on, SmartSDR reconnects to the radio it used last
//! and reopens its slice by itself.

/// The program SmartSDR runs as.
#[cfg(windows)]
pub const IMAGE: &str = "SmartSDR.exe";

/// The newest SmartSDR among the folder `names` in Program Files: the
/// folder name of the highest version, as `SmartSDR v4.3.19`.
#[must_use]
#[cfg(any(windows, test))]
pub fn newest<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names
        .into_iter()
        .filter_map(|name| Some((version(name)?, name)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, name)| name)
}

/// The version in a folder name like `SmartSDR v4.3.19`, as numbers.
#[cfg(any(windows, test))]
fn version(name: &str) -> Option<Vec<u32>> {
    let rest = name.strip_prefix("SmartSDR v")?;
    rest.split('.')
        .map(|part| part.trim().parse().ok())
        .collect::<Option<Vec<u32>>>()
        .filter(|parts| !parts.is_empty())
}

/// Where the newest SmartSDR installed on this computer is, if one is.
#[cfg(windows)]
#[must_use]
pub fn find() -> Option<std::path::PathBuf> {
    let program_files = std::path::PathBuf::from(
        std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()),
    );
    let names: Vec<String> = std::fs::read_dir(&program_files)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let folder = newest(names.iter().map(String::as_str))?;
    let exe = program_files.join(folder).join("SmartSDR").join(IMAGE);
    exe.is_file().then_some(exe)
}

/// Opens SmartSDR unless it is open already. Says what it did, or `None`
/// when it was already open.
///
/// # Errors
///
/// SmartSDR isn't installed, or Windows wouldn't start it.
#[cfg(windows)]
pub fn open() -> Result<Option<String>, String> {
    if crate::settings_file::running(IMAGE)? {
        return Ok(None);
    }
    let exe = find().ok_or_else(|| {
        "SmartSDR isn't installed on this computer (looked in Program Files for \
         SmartSDR v...).\n-> Install SmartSDR from flexradio.com, or open it with a slice."
            .to_owned()
    })?;
    let mut command = std::process::Command::new(&exe);
    if let Some(folder) = exe.parent() {
        command.current_dir(folder);
    }
    command
        .spawn()
        .map_err(|error| format!("can't open {}: {error}", exe.display()))?;
    Ok(Some(exe.display().to_string()))
}

/// Away from Windows there is no SmartSDR to open.
///
/// # Errors
///
/// Always: SmartSDR runs only on Windows.
#[cfg(not(windows))]
pub fn open() -> Result<Option<String>, String> {
    Err("SmartSDR runs only on Windows; open a slice on the radio another way.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_newest_smartsdr_installed() {
        let names = [
            "Common Files",
            "SmartSDR v3.10.15",
            "SmartSDR v4.3.19",
            "SmartSDR v4.3.5",
            "SmartSDR vX",
            "SmartSDR v",
        ];
        assert_eq!(newest(names), Some("SmartSDR v4.3.19"));
        assert_eq!(newest(["SmartSDR v3.1"]), Some("SmartSDR v3.1"));
        assert_eq!(newest(["FlexRadio Systems"]), None);
    }
}
