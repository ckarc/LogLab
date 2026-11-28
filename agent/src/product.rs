// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Which program this is: the LabLog Station Agent, or RadioAgent.
//!
//! Both are built from this crate and share every line that owns a radio.
//! RadioAgent is the one for handing out: it stands alone on one computer,
//! with no LabLog, no MQTT broker, no QSOs carried and no pairing (brian,
//! 2026-11-21). Its radio, sound, COM port, program settings files, setups,
//! Lab Stock and lab profiles are all kept on that computer.

use std::sync::OnceLock;

/// One program built from the Agent's code.
#[derive(Debug, PartialEq, Eq)]
pub struct Product {
    /// Its name, as its tray, its boxes and Windows' startup list show it.
    pub name: &'static str,
    /// Its executable's name, without `.exe`: what is typed to run it.
    pub command: &'static str,
    /// Its log's file name, beside it.
    pub log_name: &'static str,
    /// The file its log is shown in, newest first, in the temp folder.
    pub log_view_name: &'static str,
    /// The temp folder its tray icons are written to.
    pub icon_folder: &'static str,
    /// Whether it stands alone: no LabLog, no MQTT broker, and Lab Stock and
    /// the lab's profiles kept and changed on this computer.
    pub standalone: bool,
}

/// The LabLog Station Agent: links to LabLog.
pub const AGENT: Product = Product {
    name: "LabLog Station Agent",
    command: "loglab-agent",
    log_name: "loglab-agent.log",
    log_view_name: "loglab-agent-newest-first.txt",
    icon_folder: "lablog-agent-icons",
    standalone: false,
};

/// RadioAgent: the radio side alone, for any station, needing no server.
pub const RADIO_AGENT: Product = Product {
    name: "RadioAgent",
    command: "RadioAgent",
    log_name: "RadioAgent.log",
    log_view_name: "RadioAgent-newest-first.txt",
    icon_folder: "radioagent-icons",
    standalone: true,
};

static RUNNING: OnceLock<&'static Product> = OnceLock::new();

/// Says which program is running; the first call wins.
pub fn set(product: &'static Product) {
    let _ = RUNNING.set(product);
}

/// The program running: the Agent unless [`set`] said otherwise.
#[must_use]
pub fn current() -> &'static Product {
    RUNNING.get().copied().unwrap_or(&AGENT)
}

#[cfg(test)]
mod tests {
    use super::{AGENT, RADIO_AGENT};

    #[test]
    fn the_two_programs_keep_nothing_under_the_same_name() {
        const { assert!(!AGENT.standalone) };
        const { assert!(RADIO_AGENT.standalone) };
        assert_ne!(AGENT.name, RADIO_AGENT.name);
        assert_ne!(AGENT.command, RADIO_AGENT.command);
        assert_ne!(AGENT.log_name, RADIO_AGENT.log_name);
        assert_ne!(AGENT.log_view_name, RADIO_AGENT.log_view_name);
        assert_ne!(AGENT.icon_folder, RADIO_AGENT.icon_folder);
    }
}
