// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Sending the radio's live state to the Engine.
//!
//! Truth flows radio → Agent → Engine (Principle X). The Agent sends what the
//! radio last reported, or that it can't reach the radio, and the Engine
//! never sends anything back that the Agent would apply to the radio.
//!
//! The Agent signs in as a registered station with the station key an
//! administrator gave it (Principle XXI).
//!
//! It also hands the Engine the QSOs a program on this computer logged
//! (WSJT-X, say). The Engine logs each as the operator signed in at this
//! station, and the Agent keeps it in its journal until then (Principle XV).
//!
//! And it keeps readings of the radio's settings on the Engine, so every
//! computer the radio is plugged into knows them: the radio's as-received
//! record, and the setups an operator kept by name. Fetching them back only
//! shows them; nothing here puts a setting on the radio (Principle X).

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::journal::Entry;
use crate::rig::{RadioIdentity, Reading};
use crate::settings_record::Record;

/// How long one send may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest station key the Agent accepts.
const MAX_KEY: usize = 128;
/// The most of an Engine answer the Agent reads.
const MAX_ANSWER: u64 = 64 * 1024;
/// The most of a radio's kept settings the Agent reads: its as-received
/// record and every setup, each up to 2000 settings.
const MAX_SETTINGS_ANSWER: u64 = 4 * 1024 * 1024;
/// The longest name a setup may have, as the Engine takes it.
const MAX_SETUP_NAME: usize = 40;

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

/// What became of a QSO sent to the Engine.
#[derive(Debug, PartialEq, Eq)]
pub enum Handed {
    /// The Engine has it: logged now, or already in the log. Says which.
    Logged(String),
    /// Not yet: the Engine can't be reached, or nobody is signed in at this
    /// station. The QSO stays in the journal and is sent again.
    NotYet(String),
    /// The Engine refused it for good, and says why.
    Refused(String),
    /// The Engine refused this station key.
    KeyRefused(u16),
}

/// A QSO as the Engine takes it.
#[derive(Serialize)]
struct QsoBody<'a> {
    id: &'a str,
    adif: &'a str,
    program: &'a str,
}

/// The parts of the Engine's answer the Agent shows.
#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    detail: Option<serde_json::Value>,
}

/// One setting of a reading, as the Engine keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptSetting {
    /// The rig definition's id for it, such as `data_mod`.
    pub id: String,
    /// Its name on the radio's menu.
    pub label: String,
    /// The bytes the radio sent, in hex, or None if it couldn't be read.
    pub bytes: Option<String>,
}

/// A reading of a radio's settings as the Agent sends it.
#[derive(Serialize)]
struct ReadingBody<'a> {
    maker: &'a str,
    model: &'a str,
    serial: &'a str,
    name: Option<&'a str>,
    settings: Vec<KeptSetting>,
}

/// A reading the Engine keeps.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KeptReading {
    /// The setup's name, or None for the as-received record.
    pub name: Option<String>,
    /// Every setting read.
    pub settings: Vec<KeptSetting>,
    /// The station it was read at.
    pub station: String,
    /// The operator signed in there then, if any.
    pub operator: Option<String>,
    /// When the Engine kept it, as it says it.
    pub saved_at: String,
}

/// Everything the Engine keeps for one radio.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KeptRadio {
    /// Its as-received record, if any Agent has sent one.
    pub as_received: Option<KeptReading>,
    /// Its setups, the newest of each name.
    pub setups: Vec<KeptReading>,
}

/// One setting of a Lab Stock, as the Engine keeps it: always read, so
/// always with its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StockSetting {
    /// The rig definition's id for it, such as `data_mod`.
    pub id: String,
    /// Its name on the radio's menu.
    pub label: String,
    /// The bytes the radio sent, in hex.
    pub bytes: String,
    /// Its value in words, as the Agent shows it, for the administrator who
    /// approves it.
    #[serde(default)]
    pub shown: Option<String>,
}

/// A model's Lab Stock, as an administrator approved it on the Engine.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct LabStock {
    /// Its number on the Engine: Lab Stock #3.
    pub id: u64,
    /// The radio's maker.
    pub maker: String,
    /// The radio's model.
    pub model: String,
    /// Every setting it puts on.
    pub settings: Vec<StockSetting>,
}

/// What the Engine answers when asked for a model's Lab Stock.
#[derive(Deserialize)]
struct LabStockAnswer {
    lab_stock: Option<LabStock>,
}

/// A radio's settings proposed as its model's Lab Stock.
#[derive(Serialize)]
struct ProposalBody<'a> {
    maker: &'a str,
    model: &'a str,
    serial: &'a str,
    settings: &'a [StockSetting],
}

/// What the Engine said to a proposal: its number.
#[derive(Deserialize)]
struct ProposalAnswer {
    id: u64,
}

/// What the Engine said to a reading it was sent.
#[derive(Deserialize)]
struct KeptAnswer {
    created: bool,
}

/// A program's profile for a model, as an administrator saved it in LabLog:
/// it takes the place of the one built into the Agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedProfile {
    /// Its version number in LabLog.
    pub id: u64,
    /// The radio's maker.
    pub maker: String,
    /// The radio's model.
    pub model: String,
    /// The program it is for: VARA, FT8, JS8, fldigi or SSTV.
    pub program: String,
    /// The settings it puts on.
    pub settings: Vec<StockSetting>,
    /// Who saved it.
    pub saved_by: String,
    /// When, as the Engine says it.
    pub saved_at: String,
}

/// What the Engine answers when asked for a model's saved profiles.
#[derive(Deserialize)]
struct SavedProfilesAnswer {
    profiles: Vec<SavedProfile>,
}

/// An administrator's request that this station's Agent read its radio for
/// a program's profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ReadAsked {
    /// The request's id.
    pub id: String,
    /// The program whose profile it is for.
    pub program: String,
}

/// The requests waiting for this station.
#[derive(Deserialize)]
struct ReadsAnswer {
    reads: Vec<ReadAsked>,
}

/// What the Agent answers a request to read its radio with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ReadAnswer {
    /// What the radio has now.
    Read {
        /// The radio's maker.
        maker: String,
        /// The radio's model.
        model: String,
        /// The radio's serial number.
        serial: String,
        /// Every setting the Agent may write, as the radio has it.
        settings: Vec<StockSetting>,
        /// The profile the Agent puts on for the program now.
        profile: Vec<StockSetting>,
        /// Where that profile is from, in words.
        profile_from: String,
        /// The ids of settings changed on the radio since the Agent started
        /// or last put a profile on.
        changed: Vec<String>,
    },
    /// Why the radio couldn't be read.
    Failed {
        /// The reason, in words.
        problem: String,
    },
}

/// Who programs at this station log as, as the Engine says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationInfo {
    /// The station's name.
    pub station: String,
    /// Who is signed in at it, if anyone.
    pub operator: Option<String>,
    /// The call QSOs logged there are made as.
    pub station_call: Option<String>,
    /// The operation the operator chose, if any.
    pub operation: Option<String>,
    /// The station's 6-character grid square, if the Engine knows it.
    pub grid: Option<String>,
    /// Where the grid came from: "gps" (measured) or "saved".
    pub grid_source: Option<String>,
}

/// The Agent's link to the Engine.
#[derive(Clone)]
pub struct Uplink {
    url: String,
    station_url: String,
    qsos_url: String,
    settings_url: String,
    lab_stock_url: String,
    lab_profiles_url: String,
    reads_url: String,
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
            station_url: format!("{base}/api/v1/agent/station"),
            qsos_url: format!("{base}/api/v1/agent/qsos"),
            settings_url: format!("{base}/api/v1/agent/radio-settings"),
            lab_stock_url: format!("{base}/api/v1/agent/lab-stock"),
            lab_profiles_url: format!("{base}/api/v1/agent/lab-profiles"),
            reads_url: format!("{base}/api/v1/agent/profile-reads"),
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

    /// Who programs at this station log as, and where it is.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, can't be reached, or sent something that
    /// isn't an answer to this.
    pub fn station(&self) -> Result<StationInfo, SendError> {
        let mut response = self
            .http
            .get(&self.station_url)
            .header("Authorization", &self.authorization)
            .call()
            .map_err(failed)?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))
    }

    /// Hand the Engine one QSO from the journal.
    pub fn log_qso(&self, entry: &Entry) -> Handed {
        let body = QsoBody {
            id: &entry.id,
            adif: &entry.adif,
            program: &entry.program,
        };
        let sent = self
            .http
            .post(&self.qsos_url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &self.authorization)
            .send_json(&body);
        let mut response = match sent {
            Ok(response) => response,
            Err(error) => return Handed::NotYet(format!("could not reach the Engine: {error}")),
        };
        let code = response.status().as_u16();
        let answer: Option<Answer> = response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_json()
            .ok();
        let said = answer
            .and_then(|a| {
                a.message.or(match a.detail {
                    Some(serde_json::Value::String(text)) => Some(text),
                    Some(other) => Some(other.to_string()),
                    None => None,
                })
            })
            .unwrap_or_else(|| format!("the Engine answered HTTP {code}"));
        match code {
            200..=299 => Handed::Logged(said),
            401 | 403 => Handed::KeyRefused(code),
            400..=499 => Handed::Refused(said),
            _ => Handed::NotYet(said),
        }
    }

    /// Keep a reading of the radio's settings on the Engine: as `name`, a
    /// setup like FT8, or with no name as the radio's as-received record.
    /// Returns false when that was already kept: a radio's as-received
    /// record is kept once and never replaced.
    ///
    /// # Errors
    ///
    /// The record has no serial number to know the radio by, the name isn't
    /// one the Engine takes, or the Engine refused it or can't be reached.
    pub fn keep_reading(&self, record: &Record, name: Option<&str>) -> Result<bool, SendError> {
        let serial = record.serial.as_deref().ok_or_else(|| {
            SendError::Unreachable(
                "the radio's serial number isn't known, so the Engine can't tell it from \
                 another of its model"
                    .to_owned(),
            )
        })?;
        if let Some(name) = name {
            check_setup_name(name).map_err(SendError::Unreachable)?;
        }
        let body = ReadingBody {
            maker: &record.maker,
            model: &record.model,
            serial,
            name,
            settings: record
                .settings
                .iter()
                .map(|entry| KeptSetting {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    bytes: entry.bytes.clone(),
                })
                .collect(),
        };
        let mut response = self
            .http
            .post(&self.settings_url)
            .header("Authorization", &self.authorization)
            .send_json(&body)
            .map_err(failed)?;
        let answer: KeptAnswer = response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER * 16)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))?;
        Ok(answer.created)
    }

    /// What the Engine keeps for one radio, by its maker, model and serial
    /// number.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, can't be reached, or sent something that
    /// isn't an answer to this.
    pub fn radio_settings(
        &self,
        maker: &str,
        model: &str,
        serial: &str,
    ) -> Result<KeptRadio, SendError> {
        let mut response = self
            .http
            .get(&self.settings_url)
            .query("maker", maker)
            .query("model", model)
            .query("serial", serial)
            .header("Authorization", &self.authorization)
            .call()
            .map_err(failed)?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_SETTINGS_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))
    }

    /// The Lab Stock an administrator approved for the radio `maker`
    /// `model`, if there is one.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, can't be reached, or sent something that
    /// isn't an answer to this.
    pub fn lab_stock(&self, maker: &str, model: &str) -> Result<Option<LabStock>, SendError> {
        let mut response = self
            .http
            .get(&self.lab_stock_url)
            .query("maker", maker)
            .query("model", model)
            .header("Authorization", &self.authorization)
            .call()
            .map_err(failed)?;
        let answer: LabStockAnswer = response
            .body_mut()
            .with_config()
            .limit(MAX_SETTINGS_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))?;
        Ok(answer.lab_stock)
    }

    /// Propose `settings`, read from the radio of `record`, as its model's
    /// Lab Stock. Returns the proposal's number. It changes nothing until an
    /// administrator approves it on the Engine.
    ///
    /// # Errors
    ///
    /// The record has no serial number, or the Engine refused it or can't
    /// be reached.
    pub fn propose_lab_stock(
        &self,
        record: &Record,
        settings: &[StockSetting],
    ) -> Result<u64, SendError> {
        let serial = record.serial.as_deref().ok_or_else(|| {
            SendError::Unreachable(
                "the radio's serial number isn't known, so the Engine can't say which radio \
                 the proposal came from"
                    .to_owned(),
            )
        })?;
        let body = ProposalBody {
            maker: &record.maker,
            model: &record.model,
            serial,
            settings,
        };
        let mut response = self
            .http
            .post(&self.lab_stock_url)
            .header("Authorization", &self.authorization)
            .send_json(&body)
            .map_err(failed)?;
        let answer: ProposalAnswer = response
            .body_mut()
            .with_config()
            .limit(MAX_SETTINGS_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))?;
        Ok(answer.id)
    }

    /// The profiles an administrator saved in LabLog for the radio `maker`
    /// `model`, one for each program that has one.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, can't be reached, or sent something that
    /// isn't an answer to this.
    pub fn lab_profiles(&self, maker: &str, model: &str) -> Result<Vec<SavedProfile>, SendError> {
        let mut response = self
            .http
            .get(&self.lab_profiles_url)
            .query("maker", maker)
            .query("model", model)
            .header("Authorization", &self.authorization)
            .call()
            .map_err(failed)?;
        let answer: SavedProfilesAnswer = response
            .body_mut()
            .with_config()
            .limit(MAX_SETTINGS_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))?;
        Ok(answer.profiles)
    }

    /// The requests from LabLog waiting for this station's Agent to read
    /// its radio.
    ///
    /// # Errors
    ///
    /// The Engine refused the key, can't be reached, or sent something that
    /// isn't an answer to this.
    pub fn profile_reads(&self) -> Result<Vec<ReadAsked>, SendError> {
        let mut response = self
            .http
            .get(&self.reads_url)
            .header("Authorization", &self.authorization)
            .call()
            .map_err(failed)?;
        let answer: ReadsAnswer = response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_json()
            .map_err(|error| SendError::Unreachable(format!("the Engine's answer: {error}")))?;
        Ok(answer
            .reads
            .into_iter()
            .filter(|read| is_request_id(&read.id))
            .collect())
    }

    /// Answers the request `id` to read the radio.
    ///
    /// # Errors
    ///
    /// The id isn't one the Engine gives, or the Engine refused the answer
    /// or can't be reached.
    pub fn answer_read(&self, id: &str, answer: &ReadAnswer) -> Result<(), SendError> {
        if !is_request_id(id) {
            return Err(SendError::Unreachable(format!("{id:?} isn't a request id")));
        }
        self.http
            .post(&format!("{}/{id}", self.reads_url))
            .header("Authorization", &self.authorization)
            .send_json(answer)
            .map_err(failed)?;
        Ok(())
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

/// Whether `id` is a request id as the Engine gives them (a UUID), so it
/// can go in an address as it is.
fn is_request_id(id: &str) -> bool {
    id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// What a failed request says.
fn failed(error: ureq::Error) -> SendError {
    match error {
        ureq::Error::StatusCode(code @ (401 | 403)) => SendError::Refused(code),
        ureq::Error::StatusCode(code) => {
            SendError::Unreachable(format!("the Engine answered HTTP {code}"))
        }
        error => SendError::Unreachable(error.to_string()),
    }
}

/// Refuse a setup name the Engine wouldn't take: empty, longer than 40
/// characters, or with control characters.
///
/// # Errors
///
/// It is one of those.
pub fn check_setup_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > MAX_SETUP_NAME {
        return Err(format!(
            "a setup's name is 1 to {MAX_SETUP_NAME} characters, like FT8"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("a setup's name can't have control characters".to_owned());
    }
    Ok(())
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
             'lablog-operator agent-key STATION'"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    use super::{RadioReport, Report, SendError, Uplink, check_key, check_setup_name};
    use crate::rig::{ControlPath, Mode, RadioIdentity, Reading};
    use crate::settings_record::{Entry, Record, Value};

    /// An Engine that answers one request with `status` and `body`, and
    /// hands back the request it got: its first line and its body.
    fn engine_once(
        status: &'static str,
        body: &'static str,
    ) -> (String, JoinHandle<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| unreachable!("{e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| unreachable!("{e}"));
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap_or_else(|e| unreachable!("{e}"));
            let mut reader = BufReader::new(stream);
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let mut length = 0;
            loop {
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut sent = vec![0; length];
            let _ = reader.read_exact(&mut sent);
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            (
                first.trim_end().to_owned(),
                String::from_utf8_lossy(&sent).into_owned(),
            )
        });
        (format!("http://{address}"), handle)
    }

    fn record(serial: Option<&str>) -> Record {
        Record {
            format: 2,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            address: 0x94,
            serial: serial.map(str::to_owned),
            read_utc: "2026-10-27T18:00:00Z".to_owned(),
            computer: "FL1".to_owned(),
            settings: vec![
                Entry {
                    id: "att".to_owned(),
                    label: "Attenuator (ATT)".to_owned(),
                    value: Value::Raw,
                    bytes: Some("00 20".to_owned()),
                },
                Entry {
                    id: "nr_level".to_owned(),
                    label: "NR level".to_owned(),
                    value: Value::Unread {
                        why: "no answer".to_owned(),
                    },
                    bytes: None,
                },
            ],
        }
    }

    const KEY: &str = "lla_0123456789abcdef";

    #[test]
    fn keeps_a_reading_on_the_engine() {
        let (engine, handle) = engine_once("201 Created", r#"{"created":true,"reading":{}}"#);
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let kept = uplink.keep_reading(&record(Some("IC-7300_02065962")), Some("FT8"));
        assert!(matches!(kept, Ok(true)), "{kept:?}");
        let (first, body) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(first, "POST /api/v1/agent/radio-settings HTTP/1.1");
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        assert_eq!(
            sent.to_string(),
            r#"{"maker":"Icom","model":"IC-7300","name":"FT8","serial":"IC-7300_02065962","settings":[{"bytes":"00 20","id":"att","label":"Attenuator (ATT)"},{"bytes":null,"id":"nr_level","label":"NR level"}]}"#
        );
    }

    #[test]
    fn an_as_received_record_is_kept_once() {
        let (engine, handle) = engine_once("200 OK", r#"{"created":false,"reading":{}}"#);
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let kept = uplink.keep_reading(&record(Some("IC-7300_02065962")), None);
        assert!(matches!(kept, Ok(false)), "{kept:?}");
        let (_, body) = handle.join().unwrap_or_else(|_| unreachable!());
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        assert_eq!(sent["name"], serde_json::Value::Null, "{body}");
        assert!(sent.get("name").is_some(), "{body}");
    }

    #[test]
    fn wont_keep_a_reading_it_cant_name_the_radio_or_setup_of() {
        let uplink = Uplink::new("http://127.0.0.1:9", KEY).unwrap_or_else(|e| unreachable!("{e}"));
        assert!(uplink.keep_reading(&record(None), None).is_err());
        assert!(
            uplink
                .keep_reading(&record(Some("IC-7300_02065962")), Some(""))
                .is_err()
        );
        assert!(check_setup_name("FT8").is_ok());
        assert!(check_setup_name("Field Day SSB").is_ok());
        assert!(check_setup_name("").is_err());
        assert!(check_setup_name(&"x".repeat(41)).is_err());
        assert!(check_setup_name("a\tb").is_err());
    }

    #[test]
    fn fetches_what_the_engine_keeps_for_a_radio() {
        let (engine, handle) = engine_once(
            "200 OK",
            r#"{"as_received":null,"setups":[{"maker":"Icom","model":"IC-7300","serial":"IC-7300_02065962","name":"FT8","settings":[{"id":"att","label":"Attenuator (ATT)","bytes":"00"}],"station":"fieldlab-1","operator":"K9KSU","saved_at":"2026-10-27T18:00:00Z"}]}"#,
        );
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let kept = uplink
            .radio_settings("Icom", "IC-7300", "IC-7300_02065962")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert!(kept.as_received.is_none());
        assert_eq!(kept.setups.len(), 1);
        assert_eq!(kept.setups[0].name.as_deref(), Some("FT8"));
        assert_eq!(kept.setups[0].settings[0].bytes.as_deref(), Some("00"));
        let (first, _) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first,
            "GET /api/v1/agent/radio-settings?maker=Icom&model=IC-7300&serial=IC-7300_02065962 HTTP/1.1"
        );
    }

    #[test]
    fn fetches_the_approved_lab_stock_and_proposes_one() {
        let (engine, handle) = engine_once(
            "200 OK",
            r#"{"lab_stock":{"id":3,"maker":"Icom","model":"IC-7300","status":"approved","current":true,"settings":[{"id":"data_mod","label":"DATA MOD","bytes":"03","shown":"USB (03h)"}],"note":null,"station":"fl-1","serial":"IC-7300_02065962","again_of":null,"proposed_by":null,"proposed_at":"2026-10-29T19:00:00Z","decision":{"approved":true,"by":"K9KSU","at":"2026-10-29T19:05:00Z"}}}"#,
        );
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let stock = uplink
            .lab_stock("Icom", "IC-7300")
            .unwrap_or_else(|e| unreachable!("{e}"))
            .unwrap_or_else(|| unreachable!("one is approved"));
        assert_eq!((stock.id, stock.settings[0].bytes.as_str()), (3, "03"));
        let (first, _) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first,
            "GET /api/v1/agent/lab-stock?maker=Icom&model=IC-7300 HTTP/1.1"
        );

        let (engine, handle) = engine_once("200 OK", r#"{"lab_stock":null}"#);
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        assert!(matches!(uplink.lab_stock("Icom", "IC-7300"), Ok(None)));
        let _ = handle.join();

        let (engine, handle) = engine_once("201 Created", r#"{"id":7,"status":"proposed"}"#);
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let settings = [super::StockSetting {
            id: "data_mod".to_owned(),
            label: "DATA MOD".to_owned(),
            bytes: "03".to_owned(),
            shown: Some("USB (03h)".to_owned()),
        }];
        let number = uplink.propose_lab_stock(&record(Some("IC-7300_02065962")), &settings);
        assert!(matches!(number, Ok(7)), "{number:?}");
        let (first, body) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(first, "POST /api/v1/agent/lab-stock HTTP/1.1");
        let sent: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(sent["serial"], "IC-7300_02065962");
        assert_eq!(sent["settings"][0]["shown"], "USB (03h)");
        assert!(
            uplink.propose_lab_stock(&record(None), &settings).is_err(),
            "no serial, no proposal"
        );
    }

    #[test]
    fn fetches_saved_profiles_and_answers_a_request_to_read_the_radio() {
        let (engine, handle) = engine_once(
            "200 OK",
            r#"{"profiles":[{"id":4,"maker":"Icom","model":"IC-7300","program":"VARA","current":true,"settings":[{"id":"rf_power","label":"RF power","bytes":"00 69","shown":"69%"}],"note":null,"station":"fl-1","serial":"IC-7300_02065962","again_of":null,"replaces":null,"saved_by":"K9KSU","saved_at":"2026-10-30T00:40:00Z"}]}"#,
        );
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let saved = uplink
            .lab_profiles("Icom", "IC-7300")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            (
                saved[0].id,
                saved[0].program.as_str(),
                saved[0].settings[0].bytes.as_str()
            ),
            (4, "VARA", "00 69")
        );
        let (first, _) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first,
            "GET /api/v1/agent/lab-profiles?maker=Icom&model=IC-7300 HTTP/1.1"
        );

        let id = "e81ddc07-21dd-4992-88b2-520d613ae543";
        let (engine, handle) = engine_once(
            "200 OK",
            r#"{"reads":[{"id":"e81ddc07-21dd-4992-88b2-520d613ae543","program":"VARA"},{"id":"../keys","program":"FT8"}]}"#,
        );
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let asked = uplink
            .profile_reads()
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            asked.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            [id]
        );
        let _ = handle.join();

        let (engine, handle) = engine_once("204 No Content", "");
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let answer = super::ReadAnswer::Failed {
            problem: "the radio is transmitting".to_owned(),
        };
        assert!(uplink.answer_read(id, &answer).is_ok());
        let (first, body) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first,
            format!("POST /api/v1/agent/profile-reads/{id} HTTP/1.1")
        );
        let sent: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            sent,
            serde_json::json!({"problem": "the radio is transmitting"})
        );
        assert!(uplink.answer_read("../keys", &answer).is_err());
    }

    #[test]
    fn asks_who_programs_log_as() {
        let (engine, handle) = engine_once(
            "200 OK",
            r#"{"station":"fieldlab-1","operator":"K9KSU","station_call":"W0CK","operation":"Field Day","grid":"EM18bm","grid_source":"gps"}"#,
        );
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let info = uplink.station().unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(info.station_call.as_deref(), Some("W0CK"));
        assert_eq!(info.grid.as_deref(), Some("EM18bm"));
        let (first, _) = handle.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(first, "GET /api/v1/agent/station HTTP/1.1");
    }

    #[test]
    fn says_when_the_engine_refuses_the_key() {
        let (engine, handle) = engine_once("401 Unauthorized", r#"{"detail":"no"}"#);
        let uplink = Uplink::new(&engine, KEY).unwrap_or_else(|e| unreachable!("{e}"));
        let fetched = uplink.radio_settings("Icom", "IC-7300", "IC-7300_02065962");
        assert!(
            matches!(fetched, Err(SendError::Refused(401))),
            "{fetched:?}"
        );
        let _ = handle.join();
    }

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
