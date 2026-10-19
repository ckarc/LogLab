// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Sending the radio's live state to the Engine.
//!
//! Truth flows radio → Agent → Engine (Principle X). The Agent sends what the
//! radio last reported, or that it can't reach the radio, and the Engine
//! never sends anything back that the Agent would apply to the radio.
//!
//! The Agent signs in as a registered station with the station key an
//! administrator gave it (Principle XIII).

use std::fmt;
use std::time::Duration;

use serde::Serialize;

use crate::rig::{RadioIdentity, Reading};

/// How long one send may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest station key the Agent accepts.
const MAX_KEY: usize = 128;

/// What the Agent sends: the radio's state, or why it has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report<'a> {
    /// The Agent's name and version.
    pub agent: &'a str,
    /// The radio and its live state, or `None` when it can't be read.
    pub radio: Option<RadioReport<'a>>,
    /// Why the radio can't be read, when it can't.
    pub problem: Option<&'a str>,
}

/// A radio and what it last reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RadioReport<'a> {
    /// What the radio is.
    #[serde(flatten)]
    pub identity: &'a RadioIdentity,
    /// Its live state.
    #[serde(flatten)]
    pub reading: &'a Reading,
}

/// What the Agent heard on FT8: waterfall rows as they arrive, or a slot's
/// decodes once it is over.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ft8Report<'a> {
    /// The Agent's name and version.
    pub agent: &'a str,
    /// Where the audio came from: `"pretend"` for the pretend FT8 band.
    pub source: &'a str,
    /// The radio's dial frequency, in hertz; audio frequencies add to it.
    pub dial_hz: u64,
    /// New waterfall rows, if any.
    pub waterfall: Option<Ft8Waterfall>,
    /// A finished slot's decodes, if any.
    pub decodes: Option<Ft8Decodes>,
}

/// Waterfall rows of one slot: two a second, one hex byte per 6.25 Hz
/// column from 0 to 3,000 Hz.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ft8Waterfall {
    /// The slot: seconds since 1970 (UTC) divided by 15.
    pub slot: u64,
    /// The first row's number within the slot (0 to 29).
    pub first_row: usize,
    /// The rows, in order.
    pub rows: Vec<String>,
}

/// Everything decoded in one slot. Every decode is received (Principle XX).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ft8Decodes {
    /// The slot: seconds since 1970 (UTC) divided by 15.
    pub slot: u64,
    /// What was decoded.
    pub decodes: Vec<Ft8Decode>,
}

/// One decoded message, as the decoder measured it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ft8Decode {
    /// The message text.
    pub message: String,
    /// Signal-to-noise ratio in 2,500 Hz, in decibels.
    pub snr_db: f64,
    /// Start time against the slot's nominal start, in seconds.
    pub dt_s: f64,
    /// Audio frequency of the lowest tone, in hertz.
    pub base_hz: f64,
}

/// Why a send failed.
#[derive(Debug)]
pub enum SendError {
    /// The Engine refused the station key.
    Refused(u16),
    /// The Engine could not be reached, or answered with an error.
    Unreachable(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(code) => write!(
                f,
                "the Engine refused this station key (HTTP {code}); ask an administrator for a new one"
            ),
            Self::Unreachable(why) => write!(f, "could not reach the Engine: {why}"),
        }
    }
}

/// The Agent's link to the Engine.
pub struct Uplink {
    url: String,
    ft8_url: String,
    authorization: String,
    http: ureq::Agent,
}

impl Uplink {
    /// A link to the Engine at `engine` (such as `http://127.0.0.1:8073`).
    ///
    /// # Errors
    ///
    /// The address is not a plain `http://` address, or the key is not a
    /// station key.
    pub fn new(engine: &str, key: &str) -> Result<Self, String> {
        let base = engine.trim_end_matches('/');
        let Some(host) = base.strip_prefix("http://") else {
            return Err(format!(
                "the Engine address must start with http://, not {engine:?}"
            ));
        };
        if host.is_empty() || host.contains(['/', '?', '#', '@', ' ']) {
            return Err(format!(
                "{engine:?} is not an Engine address like http://127.0.0.1:8073"
            ));
        }
        check_key(key)?;
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(SEND_TIMEOUT))
            .build()
            .into();
        Ok(Self {
            url: format!("{base}/api/v1/agent/rig"),
            ft8_url: format!("{base}/api/v1/agent/ft8"),
            authorization: format!("Bearer {key}"),
            http,
        })
    }

    /// Send one report.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, or could not be reached.
    pub fn send(&self, report: &Report<'_>) -> Result<(), SendError> {
        self.put(&self.url, report)
    }

    /// Send what was heard on FT8.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, or could not be reached.
    pub fn send_ft8(&self, report: &Ft8Report<'_>) -> Result<(), SendError> {
        self.put(&self.ft8_url, report)
    }

    fn put(&self, url: &str, body: &impl Serialize) -> Result<(), SendError> {
        match self
            .http
            .put(url)
            .header("Authorization", &self.authorization)
            .send_json(body)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(code @ (401 | 403))) => Err(SendError::Refused(code)),
            Err(ureq::Error::StatusCode(code)) => Err(SendError::Unreachable(format!(
                "the Engine answered HTTP {code}"
            ))),
            Err(error) => Err(SendError::Unreachable(error.to_string())),
        }
    }
}

/// Refuse anything that is not a station key.
///
/// # Errors
///
/// It is empty, too long, or has characters a station key never has.
pub fn check_key(key: &str) -> Result<(), String> {
    let valid = !key.is_empty()
        && key.len() <= MAX_KEY
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(
            "that is not a station key; an administrator makes one with \
             'loglab-operator agent-key STATION'"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Ft8Decode, Ft8Decodes, Ft8Report, Ft8Waterfall, RadioReport, Report, Uplink, check_key,
    };
    use crate::rig::{ControlPath, Mode, RadioIdentity, Reading};

    #[test]
    fn reports_the_radio_as_the_engine_expects() {
        let identity = RadioIdentity {
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            path: ControlPath::Hamlib { model: 3073 },
        };
        let reading = Reading {
            frequency_hz: 14_074_000,
            mode: Some(Mode::DataUsb),
            passband_hz: Some(3000),
            ptt: Some(false),
            vfo: None,
        };
        let report = Report {
            agent: "loglab-agent 0.1.0",
            radio: Some(RadioReport {
                identity: &identity,
                reading: &reading,
            }),
            problem: None,
        };
        let json = serde_json::to_string(&report).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"agent":"loglab-agent 0.1.0","radio":{"maker":"Icom","model":"IC-7300","path":{"kind":"hamlib","model":3073},"frequency_hz":14074000,"mode":"DATA-USB","passband_hz":3000,"ptt":false,"vfo":null},"problem":null}"#
        );
    }

    #[test]
    fn reports_a_missing_radio() {
        let report = Report {
            agent: "loglab-agent 0.1.0",
            radio: None,
            problem: Some("the radio did not answer"),
        };
        let json = serde_json::to_string(&report).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"agent":"loglab-agent 0.1.0","radio":null,"problem":"the radio did not answer"}"#
        );
    }

    #[test]
    fn reports_ft8_as_the_engine_expects() {
        let report = Ft8Report {
            agent: "loglab-agent 0.1.0",
            source: "pretend",
            dial_hz: 14_074_000,
            waterfall: Some(Ft8Waterfall {
                slot: 117_333_334,
                first_row: 4,
                rows: vec!["00ff".to_owned()],
            }),
            decodes: Some(Ft8Decodes {
                slot: 117_333_333,
                decodes: vec![Ft8Decode {
                    message: "CQ K9KSU EN52".to_owned(),
                    snr_db: -12.0,
                    dt_s: 0.25,
                    base_hz: 1_200.5,
                }],
            }),
        };
        let json = serde_json::to_string(&report).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"agent":"loglab-agent 0.1.0","source":"pretend","dial_hz":14074000,"waterfall":{"slot":117333334,"first_row":4,"rows":["00ff"]},"decodes":{"slot":117333333,"decodes":[{"message":"CQ K9KSU EN52","snr_db":-12.0,"dt_s":0.25,"base_hz":1200.5}]}}"#
        );
    }

    #[test]
    fn checks_addresses_and_keys() {
        let key = "lla_0123456789abcdefABCDEF-_";
        assert!(Uplink::new("http://127.0.0.1:8073", key).is_ok());
        assert!(Uplink::new("http://192.168.1.10:8073/", key).is_ok());
        assert!(Uplink::new("https://127.0.0.1:8073", key).is_err());
        assert!(Uplink::new("127.0.0.1:8073", key).is_err());
        assert!(Uplink::new("http://", key).is_err());
        assert!(Uplink::new("http://host/path", key).is_err());
        assert!(check_key("").is_err());
        assert!(check_key("has space").is_err());
        assert!(check_key(&"a".repeat(200)).is_err());
    }
}
