// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Putting the LabLog Radio sound card (see `sound_card`) in front of
//! Windows: serve it over USB/IP on this computer, and have usbip-win2's
//! `usbip.exe` attach it; detach it again when done.
//!
//! The commands are usbip-win2's (its README, "Usage"): `usbip --tcp-port
//! <port> attach --remote 127.0.0.1 --bus-id <id> --once --terse` prints
//! the port it attached to, and `usbip detach --port <n>` lets go of only
//! that port, so other programs' devices stay attached. `usbip.exe` is a
//! console program, so it is started with `CREATE_NO_WINDOW` (Microsoft
//! Learn, "Process Creation Flags") and no console flashes up.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use usbip_audio::uac2::Uac2Device;
use usbip_audio::{MonotonicClock, Server};

use crate::sound_card::{self, BUS_ID, Silence};

/// Microsoft Learn, "Process Creation Flags": the process is a console
/// application that is being run without a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// usbip-win2's client, where its installer puts it.
fn usbip_exe() -> Result<PathBuf, String> {
    ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join("USBip").join("usbip.exe"))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "usbip-win2 isn't installed (no USBip\\usbip.exe under Program Files)".to_owned()
        })
}

/// Runs `usbip.exe` with `args`, and returns what it printed.
fn usbip(exe: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("couldn't run {}: {error}", exe.display()))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(said)
    } else {
        Err(format!("usbip {} failed: {}", args.join(" "), said.trim()))
    }
}

/// The LabLog Radio sound card, attached to Windows until dropped.
pub struct SoundCard {
    server: Server,
    usbip: PathBuf,
    port: u32,
    silence: Arc<Silence>,
}

impl SoundCard {
    /// Serves the card and attaches it.
    ///
    /// # Errors
    ///
    /// usbip-win2 isn't installed, the server couldn't start, or `usbip.exe`
    /// wouldn't attach the card (and why).
    pub fn start() -> Result<Self, String> {
        let usbip_exe = usbip_exe()?;
        let server = Server::start(0, Arc::new(MonotonicClock::new()))
            .map_err(|error| format!("the sound card's USB/IP server didn't start: {error}"))?;
        let silence = Arc::new(Silence::default());
        let device = Uac2Device::new(sound_card::config(), Arc::clone(&silence))?;
        server.add(BUS_ID, Arc::new(device));
        let tcp_port = server.port().to_string();
        let said = usbip(
            &usbip_exe,
            &[
                "--tcp-port",
                &tcp_port,
                "attach",
                "--remote",
                "127.0.0.1",
                "--bus-id",
                BUS_ID,
                "--once",
                "--terse",
            ],
        )?;
        let port = said
            .trim()
            .parse()
            .map_err(|_| format!("usbip attach didn't say which port: {}", said.trim()))?;
        Ok(Self {
            server,
            usbip: usbip_exe,
            port,
            silence,
        })
    }

    /// The usbip-win2 port the card is attached to.
    #[must_use]
    pub fn port(&self) -> u32 {
        self.port
    }

    /// Frames played into the card and recorded from it so far.
    #[must_use]
    pub fn frames(&self) -> (u64, u64) {
        (self.silence.played(), self.silence.recorded())
    }

    /// Detaches the card from Windows.
    ///
    /// # Errors
    ///
    /// `usbip.exe` wouldn't detach it (and why).
    pub fn stop(self) -> Result<(), String> {
        let result = self.detach();
        self.server.remove(BUS_ID);
        result
    }

    fn detach(&self) -> Result<(), String> {
        usbip(&self.usbip, &["detach", "--port", &self.port.to_string()]).map(|_| ())
    }
}
