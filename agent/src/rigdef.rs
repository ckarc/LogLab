// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Rig definition files (Principle IX).
//!
//! A command-driven radio is described by a rig definition file: one TOML
//! file per radio model, under `rigs/` beside this crate's `Cargo.toml`. The
//! format is described in `docs/station-agent-usb-radios.md`.
//!
//! A definition file is untrusted input, never code (Principle XII). Loading
//! one happens in two passes:
//!
//! 1. **Parse.** The TOML is read into structures that refuse unknown fields,
//!    so a misspelled key is an error, not something silently ignored.
//! 2. **Check.** Everything the structures can't say on their own is checked
//!    here: protocol bytes, references between parts of the file, mode names,
//!    and the rule that a requirement may only offer to change a setting the
//!    file marks writable. Every problem in the file is reported at once.
//!
//! Only a file that passes both becomes a [`Definition`]. This version only
//! checks files; drivers that use them come later.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The only schema version this Agent understands.
const SCHEMA: u32 = 1;
/// The protocol families this Agent has drivers for, or will.
const FAMILIES: [&str; 1] = ["icom-civ"];
/// The largest definition file the Agent will read.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The longest text value (a label, a note, an explanation).
const MAX_TEXT: usize = 2000;
/// The longest protocol command, in bytes.
const MAX_COMMAND_BYTES: usize = 8;
/// The most settings one radio may list.
const MAX_SETTINGS: usize = 1000;
/// CI-V's start-of-message and end-of-message bytes. Neither may appear
/// inside a command, an address, or a value.
const CIV_FRAMING: [u8; 2] = [0xFE, 0xFD];
/// The serial speeds a definition may list.
const BAUD_RATES: [u32; 7] = [1200, 4800, 9600, 19200, 38400, 57600, 115_200];

/// The rig model's voice and CW modes, by the names the Agent reports them
/// with (`rig::Mode`). A definition maps the radio's mode codes to these.
pub const VOICE_MODES: [&str; 8] = ["LSB", "USB", "AM", "CW", "RTTY", "FM", "CW-R", "RTTY-R"];
/// The rig model's data modes. A definition maps each to the voice mode it
/// is with data mode turned on.
pub const DATA_MODES: [&str; 3] = ["DATA-USB", "DATA-LSB", "DATA-FM"];
/// The rig-model operations a definition may give commands for.
const OPERATIONS: [&str; 10] = [
    "read_frequency",
    "read_mode",
    "set_frequency",
    "set_mode",
    "vfo_frequency",
    "vfo_mode",
    "select_vfo_a",
    "select_vfo_b",
    "split",
    "ptt",
];
/// The ways a radio may echo commands back.
const ECHO: [&str; 3] = ["detect", "on", "off"];
/// How far a definition has been tested.
const STATUSES: [&str; 2] = ["draft", "tested"];

// ---------------------------------------------------------------------------
// Pass 1: the file as written. Every structure refuses unknown fields.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    definition: Header,
    usb: Option<Usb>,
    link: Link,
    identity: Identity,
    modes: Modes,
    data_mode: Option<DataModeTable>,
    #[serde(default)]
    commands: BTreeMap<String, CommandEntry>,
    #[serde(default)]
    settings: Vec<Setting>,
    #[serde(default)]
    requirements: Vec<Requirement>,
    meters: Option<MetersTable>,
}

/// What the radio's transmit meters' readings mean: each a list of points,
/// the level the radio sends and what it means, in rising order.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetersTable {
    source: String,
    /// Power output, in percent of full power.
    power: Vec<[u16; 2]>,
    /// SWR, in tenths (15 is 1.5).
    swr: Vec<[u16; 2]>,
    /// ALC, in percent of its scale.
    alc: Vec<[u16; 2]>,
    /// The level setting that sets how hard the Agent's transmit audio
    /// drives the radio, offered lower when ALC runs high.
    drive: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: u32,
    maker: String,
    model: String,
    family: String,
    status: String,
    #[serde(default)]
    sources: Vec<Source>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    title: String,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Usb {
    serial: Option<UsbSerial>,
    audio: Option<UsbAudio>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsbSerial {
    vid: u16,
    pid: u16,
    role: String,
    verified: bool,
    driver: Option<UsbDriver>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsbDriver {
    name: String,
    download: String,
    page: String,
    file: String,
    inf: String,
    source: String,
    verified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsbAudio {
    name_contains: String,
    verified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    default_address: u8,
    controller_address: u8,
    baud_rates: Vec<u32>,
    echo: String,
    verified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    query: String,
    source: String,
    verified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Modes {
    source: String,
    verified: bool,
    codes: BTreeMap<String, u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataModeTable {
    command: String,
    data_modes: BTreeMap<String, String>,
    source: String,
    verified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandEntry {
    command: String,
    off: Option<u8>,
    on: Option<u8>,
    source: String,
    verified: bool,
}

// One field per key of a `[[settings]]` entry, and four of the keys are
// yes-or-no.
#[allow(clippy::struct_excessive_bools)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setting {
    id: String,
    item: Option<String>,
    command: Option<String>,
    label: String,
    values: Option<BTreeMap<String, u8>>,
    range: Option<[u8; 2]>,
    #[serde(default)]
    percent: bool,
    #[serde(default)]
    raw: bool,
    write: bool,
    source: String,
    verified: bool,
    /// The source that shows the radio takes this setting's own value
    /// written back, such as a `check-writes` run on a real radio.
    write_checked: Option<String>,
    /// Whether it changes by itself, as a clock does: never compared, never
    /// put back.
    #[serde(default)]
    moves: bool,
    /// Whether it chooses which mode and filter the radio is on, which
    /// settings marked `per_selection` belong to: put back before them.
    #[serde(default)]
    selects: bool,
    /// Whether it belongs to the selected mode or filter, such as the
    /// selected filter's width: put back only once the radio is on the
    /// setup's mode and filter, or it would land on another one.
    #[serde(default)]
    per_selection: bool,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Requirement {
    job: String,
    setting: String,
    accept: Vec<String>,
    offer: Option<String>,
    explain: String,
}

// ---------------------------------------------------------------------------
// The result: a definition that passed every check.
// ---------------------------------------------------------------------------

/// A rig definition that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    /// The maker, such as "Icom".
    pub maker: String,
    /// The model, such as "IC-7300".
    pub model: String,
    /// The protocol family, such as `icom-civ`.
    pub family: String,
    /// How far it has been tested: `draft` or `tested`.
    pub status: String,
    /// The radio's default address on its link (its CI-V address).
    pub default_address: u8,
    /// The address the Agent uses for itself on the link.
    pub controller_address: u8,
    /// The serial speeds to try, in order.
    pub baud_rates: Vec<u32>,
    /// The vendor and product IDs of the radio's USB serial bridge, if it
    /// has one, so the Agent can find its port.
    pub usb_serial: Option<(u16, u16)>,
    /// Part of the name of the radio's USB sound device, if it has one.
    pub usb_audio: Option<String>,
    /// The Windows driver its USB serial bridge needs, if Windows has none.
    pub usb_driver: Option<DriverSummary>,
    /// The radio's mode codes, by rig-model mode name.
    pub mode_codes: BTreeMap<String, u8>,
    /// The menu settings the Agent may read, in file order.
    pub settings: Vec<SettingSummary>,
    /// The settings jobs need, in file order.
    pub requirements: Vec<RequirementSummary>,
    /// How many values in the file carry a `verified` flag.
    pub checked_values: usize,
    /// How many of them are marked verified.
    pub verified_values: usize,
    /// What its transmit meters' readings mean, if the definition says.
    pub meters: Option<Meters>,
}

/// What a radio's transmit meters' readings mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meters {
    /// Power output, in percent of full power.
    pub power: Scale,
    /// SWR, in tenths (15 is 1.5).
    pub swr: Scale,
    /// ALC, in percent of its scale.
    pub alc: Scale,
    /// The id of the level setting that sets how hard the Agent's transmit
    /// audio drives the radio, if the definition names one.
    pub drive: Option<String>,
}

/// Points along a meter's scale: the level the radio sends, and what it
/// means. Levels between points are read along the straight line between
/// them; levels past the last point mean what the last point does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scale(pub Vec<[u16; 2]>);

impl Scale {
    /// What `level` means on this scale.
    #[must_use]
    pub fn value(&self, level: u16) -> u16 {
        let points = &self.0;
        let Some(first) = points.first() else {
            return 0;
        };
        if level <= first[0] {
            return first[1];
        }
        for pair in points.windows(2) {
            let ([low, from], [high, to]) = (pair[0], pair[1]);
            if level <= high {
                let span = u32::from(high - low).max(1);
                let into = u32::from(level - low);
                let (from, to) = (u32::from(from), u32::from(to));
                let value = if to >= from {
                    from + (to - from) * into / span
                } else {
                    from - (from - to) * into / span
                };
                return u16::try_from(value).unwrap_or(u16::MAX);
            }
        }
        points.last().map_or(0, |last| last[1])
    }
}

/// The Windows driver a radio's USB serial bridge needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverSummary {
    /// Its name, such as "Silicon Labs `CP210x`".
    pub name: String,
    /// Where the maker publishes its package, to fetch directly.
    pub download: String,
    /// The maker's download page, to open in a browser when fetching fails.
    pub page: String,
    /// The package's file name, as the download saves it.
    pub file: String,
    /// The driver's INF file inside the package.
    pub inf: String,
}

/// One setting the radio exposes: a menu item or a front-panel control.
// Each yes-or-no is one key of the setting's entry in the rig definition.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingSummary {
    /// The setting's id, such as `data_mod`.
    pub id: String,
    /// Its name on the radio's menu, such as "DATA MOD".
    pub label: String,
    /// The command that reads it, with no value after it: `1A 05 00 67` for
    /// menu item 0067, `14 02` for the RF gain.
    pub read: Vec<u8>,
    /// Its named values, if it has them.
    pub values: Option<BTreeMap<String, u8>>,
    /// Its lowest and highest value, if it is a level.
    pub range: Option<[u8; 2]>,
    /// Whether its range is a percentage scale (0 to 255 meaning 0 to 100%).
    pub percent: bool,
    /// Whether it is kept as the bytes the radio sent, without a meaning.
    pub raw: bool,
    /// Whether the Agent may ever change it.
    pub write: bool,
    /// Whether its values' meanings were checked on a real radio.
    pub verified: bool,
    /// Whether a real radio took its own value written back with the
    /// command that sets it. The Agent never writes a setting that wasn't
    /// checked so.
    pub write_checked: bool,
    /// Whether it changes by itself, as a clock does.
    pub moves: bool,
    /// Whether it chooses which mode and filter the radio is on.
    pub selects: bool,
    /// Whether it belongs to the selected mode or filter.
    pub per_selection: bool,
}

impl SettingSummary {
    /// Its menu item number, such as `[0x00, 0x67]`, if it is a menu item.
    #[must_use]
    pub fn item(&self) -> Option<[u8; 2]> {
        match self.read.as_slice() {
            [0x1A, 0x05, a, b] => Some([*a, *b]),
            _ => None,
        }
    }
}

/// A setting a job can't work without.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementSummary {
    /// The job, such as `digital_modes`.
    pub job: String,
    /// The setting's id.
    pub setting: String,
    /// The value names that let the job work.
    pub accept: Vec<String>,
    /// The value the Agent would offer to change it to, if it may.
    pub offer: Option<String>,
    /// What goes wrong otherwise, in plain words.
    pub explain: String,
}

impl Definition {
    /// One line describing the definition, for `check-rig`.
    #[must_use]
    pub fn summary(&self) -> String {
        let writable = self.settings.iter().filter(|s| s.write).count();
        let speeds: Vec<String> = self.baud_rates.iter().map(u32::to_string).collect();
        format!(
            "{} {} ({}, {}): address {:02X}h, speeds {}; {} settings ({} writable), \
             requirements for {}; {} of {} values verified",
            self.maker,
            self.model,
            self.family,
            self.status,
            self.default_address,
            speeds.join("/"),
            self.settings.len(),
            writable,
            if self.requirements.is_empty() {
                "nothing".to_owned()
            } else {
                self.requirements
                    .iter()
                    .map(|r| r.job.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            self.verified_values,
            self.checked_values,
        )
    }
}

/// Why a definition file was refused: every problem found in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionError {
    /// The file.
    pub file: PathBuf,
    /// Each problem, in plain words.
    pub problems: Vec<String>,
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.file.display())?;
        for problem in &self.problems {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DefinitionError {}

// ---------------------------------------------------------------------------
// Loading.
// ---------------------------------------------------------------------------

/// The definition files built into the Agent, by their path under `rigs/`.
/// The Agent is one executable with nothing to install beside it, so the
/// definitions travel inside it; a test fails if a file in `rigs/` is
/// missing here.
const BUILT_IN: [(&str, &str); 1] = [(
    "icom/ic-7300.toml",
    include_str!("../rigs/icom/ic-7300.toml"),
)];

/// Every definition built into the Agent.
///
/// # Errors
///
/// A built-in definition failed its checks, which the Agent's tests rule out.
pub fn built_in() -> Result<Vec<Definition>, String> {
    BUILT_IN
        .iter()
        .map(|(path, text)| {
            parse(text).map_err(|problems| format!("built-in {path}: {}", problems.join("; ")))
        })
        .collect()
}

/// Read and check one definition file.
///
/// # Errors
///
/// The file could not be read, is larger than the Agent accepts, is not
/// valid TOML in the expected shape, or fails a check (Principle XII).
pub fn load(path: &Path) -> Result<Definition, DefinitionError> {
    let refuse = |problem: String| DefinitionError {
        file: path.to_path_buf(),
        problems: vec![problem],
    };
    let file = fs::File::open(path).map_err(|error| refuse(format!("can't open it: {error}")))?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| refuse(format!("can't read it as text: {error}")))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(refuse(format!(
            "it is larger than the {} KiB a definition may be",
            MAX_FILE_BYTES / 1024
        )));
    }
    parse(&text).map_err(|problems| DefinitionError {
        file: path.to_path_buf(),
        problems,
    })
}

/// One file and what checking it found.
pub type Checked = (PathBuf, Result<Definition, DefinitionError>);

/// Every `.toml` file in a folder and the folders inside it, each read and
/// checked, in path order.
///
/// # Errors
///
/// The folder, or a folder inside it, could not be listed.
pub fn load_dir(dir: &Path) -> Result<Vec<Checked>, String> {
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    Ok(files
        .into_iter()
        .map(|file| {
            let result = load(&file);
            (file, result)
        })
        .collect())
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("can't list {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("can't list {}: {error}", dir.display()))?
            .path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            files.push(path);
        }
    }
    Ok(())
}

/// Check the text of a definition file.
///
/// # Errors
///
/// Every problem found, in plain words.
pub fn parse(text: &str) -> Result<Definition, Vec<String>> {
    let file: File = toml::from_str(text).map_err(|error| vec![error.to_string()])?;
    let mut check = Check::default();
    check.file(&file);
    if check.problems.is_empty() {
        Ok(summarize(&file, &check))
    } else {
        Err(check.problems)
    }
}

fn summarize(file: &File, check: &Check) -> Definition {
    Definition {
        maker: file.definition.maker.clone(),
        model: file.definition.model.clone(),
        family: file.definition.family.clone(),
        status: file.definition.status.clone(),
        default_address: file.link.default_address,
        controller_address: file.link.controller_address,
        baud_rates: file.link.baud_rates.clone(),
        usb_serial: file
            .usb
            .as_ref()
            .and_then(|usb| usb.serial.as_ref())
            .map(|serial| (serial.vid, serial.pid)),
        usb_audio: file
            .usb
            .as_ref()
            .and_then(|usb| usb.audio.as_ref())
            .map(|audio| audio.name_contains.clone()),
        usb_driver: file
            .usb
            .as_ref()
            .and_then(|usb| usb.serial.as_ref())
            .and_then(|serial| serial.driver.as_ref())
            .map(|driver| DriverSummary {
                name: driver.name.clone(),
                download: driver.download.clone(),
                page: driver.page.clone(),
                file: driver.file.clone(),
                inf: driver.inf.clone(),
            }),
        mode_codes: file.modes.codes.clone(),
        settings: file
            .settings
            .iter()
            .map(|s| SettingSummary {
                id: s.id.clone(),
                label: s.label.clone(),
                // Already checked.
                read: read_command(s).unwrap_or_default(),
                values: s.values.clone(),
                range: s.range,
                percent: s.percent,
                raw: s.raw,
                write: s.write,
                verified: s.verified,
                write_checked: s.write_checked.is_some(),
                moves: s.moves,
                selects: s.selects,
                per_selection: s.per_selection,
            })
            .collect(),
        requirements: file
            .requirements
            .iter()
            .map(|r| RequirementSummary {
                job: r.job.clone(),
                setting: r.setting.clone(),
                accept: r.accept.clone(),
                offer: r.offer.clone(),
                explain: r.explain.clone(),
            })
            .collect(),
        checked_values: check.checked,
        verified_values: check.verified,
        meters: file.meters.as_ref().map(|m| Meters {
            power: Scale(m.power.clone()),
            swr: Scale(m.swr.clone()),
            alc: Scale(m.alc.clone()),
            drive: m.drive.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Pass 2: the checks.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Check {
    problems: Vec<String>,
    sources: BTreeSet<String>,
    checked: usize,
    verified: usize,
}

impl Check {
    fn problem(&mut self, problem: String) {
        self.problems.push(problem);
    }

    fn file(&mut self, file: &File) {
        self.header(&file.definition);
        if let Some(usb) = &file.usb {
            self.usb(usb);
        }
        self.link(&file.link);
        self.identity(&file.identity);
        let voice = self.modes(&file.modes);
        if let Some(data_mode) = &file.data_mode {
            self.data_mode(data_mode, &voice);
        }
        self.commands(&file.commands);
        let settings = self.settings(&file.settings);
        self.requirements(&file.requirements, &settings);
        if let Some(meters) = &file.meters {
            self.meters(meters, &settings);
        }
    }

    fn meters(&mut self, meters: &MetersTable, settings: &BTreeMap<&str, &Setting>) {
        self.source("meters", &meters.source);
        if let Some(drive) = &meters.drive {
            match settings.get(drive.as_str()) {
                Some(setting) if setting.range.is_some() && setting.write => {}
                Some(_) => self.problem(format!(
                    "meters.drive: setting {drive:?} must be a level (a range) the Agent may write"
                )),
                None => self.problem(format!(
                    "meters.drive: setting {drive:?} is not one of the file's settings"
                )),
            }
        }
        for (name, points) in [
            ("power", &meters.power),
            ("swr", &meters.swr),
            ("alc", &meters.alc),
        ] {
            if points.len() < 2 {
                self.problem(format!("meters.{name}: give at least two points"));
            }
            if points.windows(2).any(|pair| pair[1][0] <= pair[0][0]) {
                self.problem(format!("meters.{name}: the levels must rise"));
            }
            if points.iter().any(|point| point[0] > 255) {
                self.problem(format!("meters.{name}: a meter level is 0 to 255"));
            }
        }
    }

    fn header(&mut self, header: &Header) {
        if header.schema != SCHEMA {
            self.problem(format!(
                "definition.schema is {}, but this Agent only understands schema {SCHEMA}",
                header.schema
            ));
        }
        self.text("definition.maker", &header.maker);
        self.text("definition.model", &header.model);
        self.one_of("definition.family", &header.family, &FAMILIES);
        self.one_of("definition.status", &header.status, &STATUSES);
        if header.sources.is_empty() {
            self.problem(
                "definition.sources is empty: say where the file's facts came from".to_owned(),
            );
        }
        for source in &header.sources {
            self.id("a source id", &source.id);
            self.text(&format!("source {:?} title", source.id), &source.title);
            if let Some(note) = &source.note {
                self.text(&format!("source {:?} note", source.id), note);
            }
            if !self.sources.insert(source.id.clone()) {
                self.problem(format!("source {:?} is listed twice", source.id));
            }
        }
    }

    fn usb(&mut self, usb: &Usb) {
        if let Some(serial) = &usb.serial {
            if serial.vid == 0 || serial.pid == 0 {
                self.problem("usb.serial: vid and pid must not be 0".to_owned());
            }
            self.text("usb.serial.role", &serial.role);
            self.flag(serial.verified);
            if let Some(driver) = &serial.driver {
                self.driver(driver);
            }
        }
        if let Some(audio) = &usb.audio {
            self.text("usb.audio.name_contains", &audio.name_contains);
            self.flag(audio.verified);
        }
    }

    fn driver(&mut self, driver: &UsbDriver) {
        self.text("usb.serial.driver.name", &driver.name);
        self.source("usb.serial.driver", &driver.source);
        self.flag(driver.verified);
        for (field, url) in [("download", &driver.download), ("page", &driver.page)] {
            let host = url
                .strip_prefix("https://")
                .and_then(|rest| rest.split('/').next());
            if !host.is_some_and(|host| !host.is_empty() && !url.contains(char::is_whitespace)) {
                self.problem(format!(
                    "usb.serial.driver.{field} must be an https:// address, not {url:?}"
                ));
            }
        }
        for (field, name, extension) in
            [("file", &driver.file, ".zip"), ("inf", &driver.inf, ".inf")]
        {
            let plain = name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if !plain || !name.to_ascii_lowercase().ends_with(extension) || name.starts_with('.') {
                self.problem(format!(
                    "usb.serial.driver.{field} must be a plain {extension} file name, not {name:?}"
                ));
            }
        }
    }

    fn link(&mut self, link: &Link) {
        for (field, address) in [
            ("link.default_address", link.default_address),
            ("link.controller_address", link.controller_address),
        ] {
            if address == 0 || CIV_FRAMING.contains(&address) {
                self.problem(format!(
                    "{field} is {address:02X}h, which can't be an address (00h is broadcast; FEh and FDh frame every message)"
                ));
            }
        }
        if link.default_address == link.controller_address {
            self.problem(
                "link.default_address and link.controller_address are the same".to_owned(),
            );
        }
        if link.baud_rates.is_empty() {
            self.problem("link.baud_rates is empty: give at least one speed to try".to_owned());
        }
        let mut seen = BTreeSet::new();
        for baud in &link.baud_rates {
            if !BAUD_RATES.contains(baud) {
                self.problem(format!(
                    "link.baud_rates has {baud}, which is not a serial speed radios use"
                ));
            }
            if !seen.insert(baud) {
                self.problem(format!("link.baud_rates lists {baud} twice"));
            }
        }
        self.one_of("link.echo", &link.echo, &ECHO);
        self.flag(link.verified);
    }

    fn identity(&mut self, identity: &Identity) {
        self.command_bytes("identity.query", &identity.query);
        self.source("identity", &identity.source);
        self.flag(identity.verified);
    }

    /// Checks the mode table; returns the voice modes it names.
    fn modes(&mut self, modes: &Modes) -> BTreeSet<String> {
        self.source("modes", &modes.source);
        self.flag(modes.verified);
        if modes.codes.is_empty() {
            self.problem("modes.codes is empty".to_owned());
        }
        let mut codes = BTreeMap::new();
        for (name, code) in &modes.codes {
            if DATA_MODES.contains(&name.as_str()) {
                self.problem(format!(
                    "modes.codes has {name}: data modes go in [data_mode], as the voice mode with data on"
                ));
            } else if !VOICE_MODES.contains(&name.as_str()) {
                self.problem(format!(
                    "modes.codes has {name:?}, which is not a mode the rig model knows ({})",
                    VOICE_MODES.join(", ")
                ));
            }
            if CIV_FRAMING.contains(code) {
                self.problem(format!(
                    "modes.codes.{name} is {code:02X}h, a CI-V framing byte"
                ));
            }
            if let Some(other) = codes.insert(*code, name) {
                self.problem(format!(
                    "modes.codes gives {other} and {name} the same code {code:02X}h"
                ));
            }
        }
        modes.codes.keys().cloned().collect()
    }

    fn data_mode(&mut self, data_mode: &DataModeTable, voice: &BTreeSet<String>) {
        self.command_bytes("data_mode.command", &data_mode.command);
        self.source("data_mode", &data_mode.source);
        self.flag(data_mode.verified);
        for (data, base) in &data_mode.data_modes {
            if !DATA_MODES.contains(&data.as_str()) {
                self.problem(format!(
                    "data_mode.data_modes has {data:?}, which is not a data mode ({})",
                    DATA_MODES.join(", ")
                ));
            }
            if !voice.contains(base) {
                self.problem(format!(
                    "data_mode.data_modes maps {data} to {base:?}, which is not in modes.codes"
                ));
            }
        }
    }

    fn commands(&mut self, commands: &BTreeMap<String, CommandEntry>) {
        for (operation, command) in commands {
            let field = format!("commands.{operation}");
            if !OPERATIONS.contains(&operation.as_str()) {
                self.problem(format!(
                    "{field}: {operation:?} is not an operation the rig model has ({})",
                    OPERATIONS.join(", ")
                ));
            }
            self.command_bytes(&field, &command.command);
            self.source(&field, &command.source);
            self.flag(command.verified);
            match (command.off, command.on) {
                (Some(off), Some(on)) => {
                    if off == on {
                        self.problem(format!("{field}: off and on are both {off:02X}h"));
                    }
                    for value in [off, on] {
                        if CIV_FRAMING.contains(&value) {
                            self.problem(format!("{field}: {value:02X}h is a CI-V framing byte"));
                        }
                    }
                }
                (None, None) => {}
                _ => self.problem(format!("{field}: give both off and on, or neither")),
            }
        }
    }

    /// Checks the settings; returns them by id.
    fn settings<'a>(&mut self, settings: &'a [Setting]) -> BTreeMap<&'a str, &'a Setting> {
        if settings.len() > MAX_SETTINGS {
            self.problem(format!(
                "there are {} settings; the most allowed is {MAX_SETTINGS}",
                settings.len()
            ));
        }
        let mut by_id = BTreeMap::new();
        let mut reads = BTreeSet::new();
        for setting in settings {
            self.setting(setting);
            if by_id.insert(setting.id.as_str(), setting).is_some() {
                self.problem(format!("setting {:?} is listed twice", setting.id));
            }
            if let Ok(read) = read_command(setting)
                && !reads.insert(read.clone())
            {
                self.problem(format!(
                    "setting {:?}: another setting is read with {} too",
                    setting.id,
                    crate::civ::hex(&read)
                ));
            }
        }
        by_id
    }

    fn setting(&mut self, setting: &Setting) {
        let field = format!("setting {:?}", setting.id);
        self.id("a setting id", &setting.id);
        if let Err(problem) = read_command(setting) {
            self.problem(format!("{field}: {problem}"));
        }
        self.text(&format!("{field} label"), &setting.label);
        if let Some(note) = &setting.note {
            self.text(&format!("{field} note"), note);
        }
        self.source(&field, &setting.source);
        self.flag(setting.verified);
        if let Some(checked) = &setting.write_checked {
            self.source(&format!("{field} write_checked"), checked);
            if !setting.write {
                self.problem(format!(
                    "{field}: write_checked goes with write = true; the Agent never writes it"
                ));
            }
        }
        if setting.selects && !setting.write {
            self.problem(format!(
                "{field}: selects goes with write = true; it is put back first"
            ));
        }
        if setting.selects && setting.per_selection {
            self.problem(format!(
                "{field}: a setting can't both choose the selection and belong to it"
            ));
        }
        if setting.moves && setting.write {
            self.problem(format!(
                "{field}: a setting that moves by itself is never put back, so write = false"
            ));
        }
        match (&setting.values, setting.range) {
            (Some(_), _) | (_, Some(_)) if setting.raw => {
                self.problem(format!("{field}: raw means no named values and no range"));
            }
            (None, None) if setting.raw => {}
            (Some(values), None) => {
                if setting.percent {
                    self.problem(format!(
                        "{field}: percent goes with a range, not named values"
                    ));
                }
                self.values(&field, values);
            }
            (None, Some([low, high])) => {
                if low > high {
                    self.problem(format!("{field}: range starts at {low} and ends at {high}"));
                }
            }
            (Some(_), Some(_)) => {
                self.problem(format!("{field}: give named values or a range, not both"));
            }
            (None, None) => self.problem(format!(
                "{field}: give its named values or its range, or say raw = true"
            )),
        }
    }

    fn values(&mut self, field: &str, values: &BTreeMap<String, u8>) {
        if values.len() < 2 {
            self.problem(format!(
                "{field}: a setting with named values needs at least two"
            ));
        }
        let mut seen = BTreeMap::new();
        for (name, value) in values {
            self.text(&format!("{field} value name"), name);
            if CIV_FRAMING.contains(value) {
                self.problem(format!(
                    "{field}: value {name} is {value:02X}h, a CI-V framing byte"
                ));
            }
            if let Some(other) = seen.insert(*value, name) {
                self.problem(format!("{field}: {other} and {name} are both {value:02X}h"));
            }
        }
    }

    fn requirements(&mut self, requirements: &[Requirement], settings: &BTreeMap<&str, &Setting>) {
        for requirement in requirements {
            let field = format!("requirement for {:?}", requirement.job);
            self.id("a requirement job", &requirement.job);
            self.text(&format!("{field} explain"), &requirement.explain);
            let Some(setting) = settings.get(requirement.setting.as_str()) else {
                self.problem(format!(
                    "{field}: setting {:?} is not one of the file's settings",
                    requirement.setting
                ));
                continue;
            };
            let Some(values) = &setting.values else {
                self.problem(format!(
                    "{field}: setting {:?} has a range, not named values to accept",
                    setting.id
                ));
                continue;
            };
            if requirement.accept.is_empty() {
                self.problem(format!("{field}: accept is empty"));
            }
            for name in &requirement.accept {
                if !values.contains_key(name) {
                    self.problem(format!(
                        "{field}: accept has {name:?}, which is not a value of {:?}",
                        setting.id
                    ));
                }
            }
            if let Some(offer) = &requirement.offer {
                if !requirement.accept.contains(offer) {
                    self.problem(format!(
                        "{field}: offer {offer:?} is not one of the accepted values"
                    ));
                }
                if !setting.write {
                    self.problem(format!(
                        "{field}: offers to change {:?}, but that setting has write = false",
                        setting.id
                    ));
                }
            }
        }
    }

    // --- small checks used above ---

    fn flag(&mut self, verified: bool) {
        self.checked += 1;
        if verified {
            self.verified += 1;
        }
    }

    fn source(&mut self, field: &str, source: &str) {
        if !self.sources.contains(source) {
            self.problem(format!(
                "{field}: source {source:?} is not in definition.sources"
            ));
        }
    }

    fn command_bytes(&mut self, field: &str, text: &str) {
        if let Err(problem) = bytes(text) {
            self.problem(format!("{field} {problem}"));
        }
    }

    fn text(&mut self, field: &str, text: &str) {
        if text.trim().is_empty() {
            self.problem(format!("{field} is empty"));
        } else if text.chars().count() > MAX_TEXT {
            self.problem(format!("{field} is longer than {MAX_TEXT} characters"));
        } else if text.chars().any(|c| c.is_control() && c != '\n') {
            self.problem(format!("{field} has a control character in it"));
        }
    }

    fn id(&mut self, what: &str, id: &str) {
        let good = id.starts_with(|c: char| c.is_ascii_lowercase())
            && id.len() <= 64
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !good {
            self.problem(format!(
                "{what} {id:?} must be lower case letters, digits and _, starting with a letter"
            ));
        }
    }

    fn one_of(&mut self, field: &str, value: &str, allowed: &[&str]) {
        if !allowed.contains(&value) {
            self.problem(format!(
                "{field} is {value:?}; it must be one of {}",
                allowed.join(", ")
            ));
        }
    }
}

/// The commands a setting may be read with. Each one, sent with no value
/// after it, only asks the radio ("Send/read" in Icom's command tables).
/// Commands that act when sent bare, such as 07 00 (select VFO A), and the
/// 1C family, which keys the transmitter and starts the tuner, are not here,
/// so a definition can't make the Agent change a radio while reading it.
const READABLE: [&[u8]; 10] = [
    &[0x04],
    &[0x11],
    &[0x14],
    &[0x16],
    &[0x1A, 0x03],
    &[0x1A, 0x04],
    &[0x1A, 0x06],
    &[0x1B],
    &[0x21],
    &[0x1A, 0x05],
];

/// The command that reads a setting: `1A 05` and the item number for a menu
/// item, or the setting's own command.
fn read_command(setting: &Setting) -> Result<Vec<u8>, String> {
    match (&setting.item, &setting.command) {
        (Some(item), None) => {
            let item = bytes(item).map_err(|problem| format!("item {problem}"))?;
            if item.len() != 2 {
                return Err(format!(
                    "item is {} bytes; a menu item is 2, as in \"00 71\"",
                    item.len()
                ));
            }
            Ok([&[0x1A, 0x05][..], &item].concat())
        }
        (None, Some(command)) => {
            let command = bytes(command).map_err(|problem| format!("command {problem}"))?;
            let readable = READABLE.iter().find(|prefix| command.starts_with(prefix));
            match readable {
                Some(&[0x1A, 0x05]) => Err("menu items are given as item, not command".to_owned()),
                // 04 and 11 are read bare; the others need one byte saying
                // which.
                Some(&[0x04] | &[0x11]) if command.len() == 1 => Ok(command),
                Some(&[0x04] | &[0x11]) => Err(format!(
                    "command {} has nothing after it",
                    crate::civ::hex(&command[..1])
                )),
                Some(prefix) if command.len() == prefix.len() + usize::from(prefix.len() == 1) => {
                    Ok(command)
                }
                Some(_) => Err(format!(
                    "command {} has the wrong number of bytes",
                    crate::civ::hex(&command)
                )),
                None => Err(format!(
                    "command {} is not one the Agent reads settings with",
                    crate::civ::hex(&command)
                )),
            }
        }
        (Some(_), Some(_)) => Err("give item or command, not both".to_owned()),
        (None, None) => Err("give its menu item or its command".to_owned()),
    }
}

/// Reads protocol bytes written as hexadecimal pairs, such as `"1A 05"`.
fn bytes(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for pair in text.split_whitespace() {
        if pair.len() != 2 {
            return Err(format!(
                "{text:?}: write each byte as two hex digits, as in \"1A 05\""
            ));
        }
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("{text:?}: {pair:?} is not a hex byte"))?;
        if CIV_FRAMING.contains(&byte) {
            return Err(format!("{text:?}: {byte:02X}h is a CI-V framing byte"));
        }
        out.push(byte);
    }
    if out.is_empty() {
        return Err("is empty".to_owned());
    }
    if out.len() > MAX_COMMAND_BYTES {
        return Err(format!("{text:?} is longer than {MAX_COMMAND_BYTES} bytes"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BUILT_IN, DATA_MODES, VOICE_MODES, built_in, bytes, load, load_dir, parse};
    use crate::rig::Mode;

    /// The smallest file that passes every check.
    const MINIMAL: &str = r#"
[definition]
schema = 1
maker = "Test"
model = "TR-1"
family = "icom-civ"
status = "draft"

[[definition.sources]]
id = "manual"
title = "The TR-1 manual"

[link]
default_address = 0x94
controller_address = 0xE0
baud_rates = [19200]
echo = "detect"
verified = false

[identity]
query = "19 00"
source = "manual"
verified = false

[modes]
source = "manual"
verified = true

[modes.codes]
USB = 0x01
LSB = 0x00

[data_mode]
command = "1A 06"
data_modes = { DATA-USB = "USB" }
source = "manual"
verified = false

[commands]
ptt = { command = "1C 00", off = 0x00, on = 0x01, source = "manual", verified = false }

[[settings]]
id = "data_mod"
item = "00 67"
label = "DATA MOD"
values = { MIC = 0x00, USB = 0x03 }
write = true
source = "manual"
verified = false

[[requirements]]
job = "digital_modes"
setting = "data_mod"
accept = ["USB"]
offer = "USB"
explain = "Data mode audio must come from USB."
"#;

    /// `MINIMAL` with one piece of text swapped for another.
    fn changed(from: &str, to: &str) -> String {
        assert!(MINIMAL.contains(from), "test is out of date: {from:?}");
        MINIMAL.replacen(from, to, 1)
    }

    /// The problems a text has; panics if it passes.
    fn problems(text: &str) -> Vec<String> {
        match parse(text) {
            Ok(definition) => unreachable!("should have been refused: {definition:?}"),
            Err(problems) => problems,
        }
    }

    fn refused_for(text: &str, words: &str) {
        let problems = problems(text);
        assert!(
            problems.iter().any(|p| p.contains(words)),
            "expected a problem mentioning {words:?}, got {problems:?}"
        );
    }

    #[test]
    fn every_definition_in_the_repository_passes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rigs");
        let results = load_dir(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(
            !results.is_empty(),
            "no definitions found in {}",
            dir.display()
        );
        let failures: Vec<String> = results
            .iter()
            .filter_map(|(_, result)| result.as_ref().err().map(ToString::to_string))
            .collect();
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn every_file_in_rigs_is_built_in() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rigs");
        let files: Vec<String> = load_dir(&dir)
            .unwrap_or_else(|error| unreachable!("{error}"))
            .into_iter()
            .filter_map(|(path, _)| {
                path.strip_prefix(&dir)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        let built: Vec<&str> = BUILT_IN.iter().map(|(path, _)| *path).collect();
        assert_eq!(
            files, built,
            "add each file in rigs/ to BUILT_IN in rigdef.rs"
        );
        assert_eq!(built_in().map(|d| d.len()), Ok(BUILT_IN.len()));
    }

    #[test]
    fn reads_the_ic_7300() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rigs/icom/ic-7300.toml");
        let definition = load(&path).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(definition.maker, "Icom");
        assert_eq!(definition.model, "IC-7300");
        assert_eq!(definition.default_address, 0x94);
        assert_eq!(definition.baud_rates.first(), Some(&115_200));
        assert_eq!(definition.usb_serial, Some((0x10C4, 0xEA60)));
        assert_eq!(definition.usb_audio.as_deref(), Some("USB Audio CODEC"));
        let driver = definition
            .usb_driver
            .clone()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(driver.inf, "silabser.inf");
        assert!(driver.download.starts_with("https://www.silabs.com/"));
        // Every menu item firmware 1.40 has, 0001 to 0216, then the
        // front-panel controls.
        let items: Vec<[u8; 2]> = definition
            .settings
            .iter()
            .filter_map(super::SettingSummary::item)
            .collect();
        let numbers: Vec<u16> = items
            .iter()
            .filter_map(|item| crate::civ::level_from_bcd(item))
            .collect();
        assert_eq!(numbers, (1..=216).collect::<Vec<u16>>());
        assert_eq!(definition.settings.len(), 216 + 44);
        let read = |id: &str| {
            definition
                .settings
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.read.clone())
        };
        assert_eq!(read("rf_gain"), Some(vec![0x14, 0x02]));
        assert_eq!(read("noise_reduction"), Some(vec![0x16, 0x40]));
        assert_eq!(read("attenuator"), Some(vec![0x11]));
        assert_eq!(read("data_mod"), Some(vec![0x1A, 0x05, 0x00, 0x67]));
        // Never written: the CI-V and USB link, and the clock.
        let fixed: Vec<&str> = definition
            .settings
            .iter()
            .filter(|s| !s.write)
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(
            fixed,
            [
                "civ_transceive",
                "civ_usb_to_remote_address",
                "civ_output_for_ant",
                "civ_usb_port",
                "civ_usb_echo_back",
                "usb_serial_function",
                "usb_send",
                "usb_keying_cw",
                "usb_keying_rtty",
                "date",
                "time",
                "utc_offset",
            ]
        );
        let jobs: Vec<&str> = definition
            .requirements
            .iter()
            .map(|r| r.job.as_str())
            .collect();
        assert_eq!(jobs, ["control", "digital_modes", "computer_voice"]);
        assert_eq!(
            definition.verified_values, 0,
            "nothing is checked on a radio yet"
        );
    }

    #[test]
    fn reads_settings_only_with_commands_that_only_ask() {
        let control =
            |command: &str| changed("item = \"00 67\"", &format!("command = \"{command}\""));
        for good in ["14 02", "16 40", "11", "1A 03", "1B 00", "21 01"] {
            assert!(parse(&control(good)).is_ok(), "{good}");
        }
        // 07 00 selects VFO A and 1C keys the radio or starts the tuner, even
        // with nothing after them.
        refused_for(&control("07 00"), "not one the Agent reads settings with");
        refused_for(&control("1C 01"), "not one the Agent reads settings with");
        refused_for(&control("05"), "not one the Agent reads settings with");
        refused_for(&control("1A 05 00 67"), "given as item");
        refused_for(&control("14"), "wrong number of bytes");
        refused_for(&control("14 02 01"), "wrong number of bytes");
        refused_for(&control("11 00"), "nothing after it");
        refused_for(
            &changed("item = \"00 67\"", "item = \"00 67\"\ncommand = \"14 02\""),
            "not both",
        );
        refused_for(
            &changed("item = \"00 67\"\n", ""),
            "menu item or its command",
        );
    }

    #[test]
    fn keeps_a_setting_raw_only_without_meanings() {
        let raw = changed("values = { MIC = 0x00, USB = 0x03 }", "raw = true");
        // A requirement needs named values, so drop it for this one.
        let raw = raw
            .split("[[requirements]]")
            .next()
            .unwrap_or_default()
            .to_owned();
        let definition = parse(&raw).unwrap_or_else(|problems| unreachable!("{problems:?}"));
        assert!(definition.settings[0].raw);
        refused_for(
            &changed(
                "values = { MIC = 0x00, USB = 0x03 }",
                "values = { MIC = 0x00, USB = 0x03 }\nraw = true",
            ),
            "raw means no named values",
        );
        refused_for(
            &changed("values = { MIC = 0x00, USB = 0x03 }\n", ""),
            "or say raw = true",
        );
    }

    #[test]
    fn a_driver_comes_from_an_https_address_as_plain_files() {
        let driver = |download: &str, file: &str, inf: &str| {
            changed(
                "[[settings]]",
                &format!(
                    "[usb.serial]\nvid = 0x10C4\npid = 0xEA60\nrole = \"civ\"\nverified = false\n\n\
                     [usb.serial.driver]\nname = \"x\"\ndownload = \"{download}\"\n\
                     page = \"https://example.com/\"\nfile = \"{file}\"\ninf = \"{inf}\"\n\
                     source = \"manual\"\nverified = false\n\n[[settings]]"
                ),
            )
        };
        let good = parse(&driver("https://example.com/d.zip", "d.zip", "d.inf"))
            .unwrap_or_else(|problems| unreachable!("{problems:?}"));
        assert_eq!(good.usb_driver.map(|d| d.inf), Some("d.inf".to_owned()));
        refused_for(
            &driver("http://example.com/d.zip", "d.zip", "d.inf"),
            "https://",
        );
        refused_for(&driver("https:///d.zip", "d.zip", "d.inf"), "https://");
        refused_for(
            &driver("https://example.com/d.zip", "../d.zip", "d.inf"),
            "plain .zip",
        );
        refused_for(
            &driver("https://example.com/d.zip", "d.exe", "d.inf"),
            "plain .zip",
        );
        refused_for(
            &driver("https://example.com/d.zip", "d.zip", "a/d.inf"),
            "plain .inf",
        );
    }

    #[test]
    fn the_minimal_file_passes() {
        let definition = parse(MINIMAL).unwrap_or_else(|problems| unreachable!("{problems:?}"));
        assert_eq!(definition.model, "TR-1");
        assert_eq!(definition.checked_values, 6);
        assert_eq!(definition.verified_values, 1);
        assert!(definition.summary().contains("1 of 6 values verified"));
    }

    #[test]
    fn mode_names_match_the_rig_model() {
        let names = |modes: &[Mode]| -> Vec<String> {
            modes
                .iter()
                .map(|mode| {
                    serde_json::to_value(mode)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect()
        };
        let voice = [
            Mode::Lsb,
            Mode::Usb,
            Mode::Am,
            Mode::Cw,
            Mode::Rtty,
            Mode::Fm,
            Mode::CwReverse,
            Mode::RttyReverse,
        ];
        assert_eq!(names(&voice), VOICE_MODES);
        assert_eq!(
            names(&[Mode::DataUsb, Mode::DataLsb, Mode::DataFm]),
            DATA_MODES
        );
    }

    #[test]
    fn refuses_a_misspelled_key() {
        refused_for(&changed("verified = true", "verifed = true"), "verifed");
    }

    #[test]
    fn refuses_civ_framing_bytes_and_bad_hex() {
        refused_for(
            &changed("query = \"19 00\"", "query = \"19 FD\""),
            "framing",
        );
        refused_for(
            &changed("query = \"19 00\"", "query = \"1900\""),
            "two hex digits",
        );
        refused_for(
            &changed("query = \"19 00\"", "query = \"19 G0\""),
            "not a hex byte",
        );
        refused_for(&changed("query = \"19 00\"", "query = \"\""), "is empty");
        refused_for(
            &changed(
                "query = \"19 00\"",
                "query = \"01 02 03 04 05 06 07 08 09\"",
            ),
            "longer than",
        );
        refused_for(&changed("USB = 0x01", "USB = 0xFE"), "framing");
        refused_for(
            &changed("default_address = 0x94", "default_address = 0xFE"),
            "can't be an address",
        );
    }

    #[test]
    fn refuses_a_source_that_is_not_listed() {
        refused_for(
            &changed(
                "source = \"manual\"\nverified = true",
                "source = \"guess\"\nverified = true",
            ),
            "\"guess\"",
        );
    }

    #[test]
    fn write_checked_needs_a_listed_source_and_a_writable_setting() {
        refused_for(
            &changed("write = true", "write = true\nwrite_checked = \"guess\""),
            "\"guess\"",
        );
        refused_for(
            &changed("write = true", "write = false\nwrite_checked = \"manual\""),
            "write_checked goes with write = true",
        );
        refused_for(
            &changed("write = true", "write = true\nmoves = true"),
            "never put back, so write = false",
        );
    }

    #[test]
    fn refuses_unknown_modes_and_shared_codes() {
        refused_for(&changed("LSB = 0x00", "LSB = 0x01"), "same code");
        refused_for(
            &changed("LSB = 0x00", "SSB = 0x00"),
            "not a mode the rig model knows",
        );
        refused_for(
            &changed("LSB = 0x00", "DATA-LSB = 0x00"),
            "go in [data_mode]",
        );
        refused_for(
            &changed("DATA-USB = \"USB\"", "DATA-USB = \"FM\""),
            "not in modes.codes",
        );
    }

    #[test]
    fn refuses_an_unknown_operation() {
        refused_for(&changed("ptt = {", "transmit_now = {"), "not an operation");
        refused_for(
            &changed("off = 0x00, on = 0x01, ", "on = 0x01, "),
            "both off and on",
        );
    }

    #[test]
    fn refuses_bad_speeds() {
        refused_for(
            &changed("baud_rates = [19200]", "baud_rates = []"),
            "baud_rates is empty",
        );
        refused_for(
            &changed("baud_rates = [19200]", "baud_rates = [19201]"),
            "not a serial speed",
        );
        refused_for(
            &changed("baud_rates = [19200]", "baud_rates = [19200, 19200]"),
            "twice",
        );
    }

    #[test]
    fn refuses_bad_settings() {
        refused_for(
            &changed("item = \"00 67\"", "item = \"67\""),
            "a menu item is 2",
        );
        refused_for(
            &changed(
                "values = { MIC = 0x00, USB = 0x03 }",
                "values = { MIC = 0x00, USB = 0x00 }",
            ),
            "are both",
        );
        refused_for(
            &changed("values = { MIC = 0x00, USB = 0x03 }", "range = [10, 5]"),
            "range starts at 10",
        );
        refused_for(
            &changed(
                "values = { MIC = 0x00, USB = 0x03 }",
                "values = { MIC = 0x00, USB = 0x03 }\nrange = [0, 255]",
            ),
            "not both",
        );
        refused_for(
            &changed("id = \"data_mod\"", "id = \"Data Mod\""),
            "lower case",
        );
    }

    #[test]
    fn a_requirement_offers_only_a_writable_accepted_value() {
        refused_for(&changed("write = true", "write = false"), "write = false");
        refused_for(
            &changed("offer = \"USB\"", "offer = \"MIC\""),
            "not one of the accepted values",
        );
        refused_for(
            &changed("accept = [\"USB\"]", "accept = [\"LAN\"]"),
            "not a value of",
        );
        refused_for(
            &changed("setting = \"data_mod\"", "setting = \"usb_mod\""),
            "not one of the file's settings",
        );
    }

    /// A `[meters]` table, put before the requirements.
    fn with_meters(power: &str) -> String {
        changed(
            "[[requirements]]",
            &format!(
                "[meters]\nsource = \"manual\"\npower = {power}\n\
                 swr = [[0, 10], [48, 15], [80, 20], [120, 30]]\n\
                 alc = [[0, 0], [120, 100]]\n\n[[requirements]]"
            ),
        )
    }

    #[test]
    fn reads_meter_scales_and_checks_them() {
        let definition = parse(&with_meters("[[0, 0], [143, 50], [213, 100]]"))
            .unwrap_or_else(|problems| unreachable!("{problems:?}"));
        let meters = definition
            .meters
            .unwrap_or_else(|| unreachable!("the table was given"));
        assert_eq!(meters.power.value(0), 0);
        assert_eq!(meters.power.value(143), 50);
        assert_eq!(meters.power.value(178), 75, "halfway between points");
        assert_eq!(meters.power.value(255), 100, "held at the last point");
        assert_eq!(meters.swr.value(64), 17, "SWR in tenths");
        assert_eq!(meters.alc.value(40), 33);
        assert!(parse(MINIMAL).is_ok_and(|d| d.meters.is_none()));
        refused_for(&with_meters("[[0, 0]]"), "at least two points");
        refused_for(&with_meters("[[143, 50], [0, 0]]"), "must rise");
        refused_for(&with_meters("[[0, 0], [300, 100]]"), "0 to 255");
        refused_for(
            &with_meters("[[0, 0], [213, 100]]").replacen(
                "source = \"manual\"\npower",
                "source = \"nowhere\"\npower",
                1,
            ),
            "nowhere",
        );
        let drive = |id: &str| {
            with_meters("[[0, 0], [213, 100]]").replacen(
                "alc = [[0, 0], [120, 100]]",
                &format!("alc = [[0, 0], [120, 100]]\ndrive = \"{id}\""),
                1,
            )
        };
        refused_for(&drive("data_mod"), "must be a level");
        refused_for(&drive("usb_mod"), "not one of the file's settings");
    }

    #[test]
    fn reports_every_problem_at_once() {
        let text = changed("baud_rates = [19200]", "baud_rates = [19201]")
            .replacen("status = \"draft\"", "status = \"done\"", 1)
            .replacen("offer = \"USB\"", "offer = \"MIC\"", 1);
        assert_eq!(problems(&text).len(), 3, "{:?}", problems(&text));
    }

    #[test]
    fn refuses_an_unknown_schema_or_family() {
        refused_for(
            &changed("schema = 1", "schema = 2"),
            "only understands schema 1",
        );
        refused_for(
            &changed("family = \"icom-civ\"", "family = \"yaesu-cat\""),
            "definition.family",
        );
    }

    #[test]
    fn refuses_an_oversized_file() {
        let dir = std::env::temp_dir().join(format!("loglab-rigdef-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        let path = dir.join("big.toml");
        let padding = format!("{MINIMAL}\n# {}\n", "x".repeat(300 * 1024));
        std::fs::write(&path, padding).unwrap_or_else(|error| unreachable!("{error}"));
        let error = load(&path)
            .err()
            .unwrap_or_else(|| unreachable!("should be refused"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(error.to_string().contains("larger than"), "{error}");
    }

    #[test]
    fn reads_hex_bytes() {
        assert_eq!(bytes("1A 05 00 71"), Ok(vec![0x1A, 0x05, 0x00, 0x71]));
        assert_eq!(bytes(" 03 "), Ok(vec![0x03]));
    }
}
