// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Asking to join as a station, to get the station's key.
//!
//! An Agent with no key asks the Engine to join as a station. The Engine
//! answers with a short code, which the Agent shows, and a private ticket.
//! An administrator sees the same code in LabLog and approves the request
//! (Principle XXI); the Agent then collects its key with the ticket, once,
//! and keeps it in its key file, readable only by this account. Nobody copies
//! a key by hand.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::check_key;

/// How often the Agent asks how its request stands.
const POLL: Duration = Duration::from_secs(2);
/// How long one question to the Engine may take.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The largest answer the Agent reads from the Engine.
const MAX_ANSWER: u64 = 16 * 1024;

#[derive(Serialize)]
struct Ask<'a> {
    station: &'a str,
    agent: &'a str,
}

#[derive(Deserialize)]
struct Asked {
    id: String,
    code: String,
    ticket: String,
}

#[derive(Deserialize)]
struct Standing {
    state: String,
    key: Option<String>,
}

/// Why joining did not work.
#[derive(Debug)]
pub enum PairError {
    /// An administrator turned the request down.
    Refused,
    /// The Agent was stopped while waiting.
    Stopped,
    /// Something else; the text says what.
    Failed(String),
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => f.write_str("an administrator refused this station's request to join"),
            Self::Stopped => f.write_str("stopped while waiting to join"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// A request's id must be a UUID before it goes into a URL (Principle XII).
fn plain_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// A code is a few capital letters and digits, safe to print.
fn plain_code(code: &str) -> bool {
    (4..=12).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Ask to join as `station` until an administrator decides, and return the key.
///
/// A request that expires is made again, with a new code.
///
/// # Errors
///
/// The request was refused, the Agent was stopped, or the Engine's answers
/// made no sense.
pub fn join(
    base: &str,
    station: &str,
    agent: &str,
    running: &AtomicBool,
) -> Result<String, PairError> {
    let http: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut unreachable_said = false;
    while running.load(Ordering::SeqCst) {
        let asked: Asked = match http
            .post(&format!("{base}/api/v1/agent/pairings"))
            .send_json(Ask { station, agent })
            .and_then(|mut answer| {
                answer
                    .body_mut()
                    .with_config()
                    .limit(MAX_ANSWER)
                    .read_json()
            }) {
            Ok(asked) => asked,
            Err(error) => {
                if !unreachable_said {
                    println!("could not ask the Engine to join: {error}; trying again");
                    unreachable_said = true;
                }
                thread::sleep(POLL * 5);
                continue;
            }
        };
        unreachable_said = false;
        if !plain_id(&asked.id) || !plain_code(&asked.code) || check_key(&asked.ticket).is_err() {
            return Err(PairError::Failed(
                "the Engine's answer made no sense".to_owned(),
            ));
        }
        println!(
            "Waiting to join as station {station}. In LabLog, an administrator opens \
             Menu > Station Agents and approves code {}.",
            asked.code
        );
        let url = format!("{base}/api/v1/agent/pairings/{}", asked.id);
        let authorization = format!("Bearer {}", asked.ticket);
        loop {
            if !running.load(Ordering::SeqCst) {
                return Err(PairError::Stopped);
            }
            thread::sleep(POLL);
            let standing: Standing = match http
                .get(&url)
                .header("Authorization", &authorization)
                .call()
                .and_then(|mut answer| {
                    answer
                        .body_mut()
                        .with_config()
                        .limit(MAX_ANSWER)
                        .read_json()
                }) {
                Ok(standing) => standing,
                Err(_) => continue, // the Engine may be restarting; keep asking
            };
            match (standing.state.as_str(), standing.key) {
                ("waiting", _) => {}
                ("approved", Some(key)) => {
                    check_key(&key).map_err(PairError::Failed)?;
                    return Ok(key);
                }
                ("refused", _) => return Err(PairError::Refused),
                _ => break, // expired: ask again, with a new code
            }
        }
    }
    Err(PairError::Stopped)
}

/// Keep `key` in `path`, readable only by this account, written whole
/// (Principle XIV).
///
/// # Errors
///
/// The file could not be written.
pub fn save_key(path: &Path, key: &str) -> Result<(), String> {
    let fail = |error: std::io::Error| {
        format!("cannot save the station key in {}: {error}", path.display())
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let temp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(fail)?;
    file.write_all(format!("{key}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(fail)?;
    drop(file);
    fs::rename(&temp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::{plain_code, plain_id, save_key};

    #[test]
    fn checks_ids_and_codes() {
        assert!(plain_id("3f2b8c1e-9d4a-4e6f-8a7b-1c2d3e4f5a6b"));
        assert!(!plain_id("../../etc/passwd"));
        assert!(!plain_id("3f2b8c1e-9d4a-4e6f-8a7b-1c2d3e4f5a6b?x"));
        assert!(plain_code("4F7K9Q"));
        assert!(!plain_code("4f7k9q"));
        assert!(!plain_code("4F7\u{1b}[2J"));
        assert!(!plain_code("AB"));
    }

    #[test]
    fn saves_the_key_privately() {
        let dir = std::env::temp_dir().join(format!("loglab-agent-test-{}", std::process::id()));
        let path = dir.join("nested").join("agent-key");
        assert_eq!(save_key(&path, "lls_abc"), Ok(()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "lls_abc\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
            assert_eq!(mode.unwrap_or_default(), 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
