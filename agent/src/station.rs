// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent station`: everything programs on this computer need to use
//! the radio, from one command.
//!
//! 1. Finds the radio: the serial port whose USB IDs match a known radio's,
//!    confirmed by the radio's own answer to "who are you?" at the speed it
//!    answers at.
//! 2. Reads every setting the radio's definition lists, keeps the first
//!    reading of each radio (by its serial number) as its as-received record
//!    in `records` beside the Agent, and says what has changed since. Checks
//!    the settings against what digital modes need, and says what is wrong
//!    and how to fix it on the radio. It changes nothing on the radio: its
//!    definition's settings aren't verified on a real radio yet, and the
//!    Agent never writes an unverified setting.
//! 3. Carries the radio's audio both ways (Windows): receive into VB-CABLE,
//!    and a program's transmit audio from a second virtual cable to the
//!    radio, only while the Agent has it keyed for that program.
//! 4. Points VARA's sound devices at those cables, and sets up WSJT-X,
//!    `JS8Call`, fldigi and MMSSTV to use the cables and the virtual radio,
//!    each only if it isn't running (see `vara` and `programs`).
//! 5. Serves the radio to programs on this computer, which may tune it and
//!    key it (`serve --allow-transmit`).
//! 6. With `--engine` and `--key-file`, keeps the radio's settings on the
//!    Engine, so every computer it is plugged into knows them: sends its
//!    as-received record, lists what differs from each setup an operator
//!    kept (FT8, say), and keeps the radio's settings as a setup when the
//!    operator types `keep NAME`. `restore NAME` puts a setup back, only
//!    after the operator types yes, only settings verified on a real radio,
//!    and records it (Principle X).
//! 7. With `--engine`, also carries what the programs and LabLog need from
//!    each other (see `carrier`): each QSO the programs log goes to LabLog
//!    through the Agent's journal, the radio's live state goes to LabLog's
//!    entry screen, and the Agent says who LabLog will log QSOs as.
//!
//! Starting `station` is the operator's choice that programs on this
//! computer may key the radio, as `--allow-transmit` is for `serve`
//! (Principle IX).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::auto_mode;
use crate::civ_radio::{CivRadio, Port};
use crate::civ_serial::{self, SerialPort};
use crate::engine::{KeptRadio, KeptReading, Uplink};
use crate::rig::{Radio, RadioError, RadioIdentity, Reading, Tuning};
use crate::rigctld_server::{self, Shared, Source};
use crate::rigdef::{Definition, SettingSummary};
use crate::settings_record::Record;
use crate::setups::Setups;
use crate::{enrollment, profiles, program_id, rigdef, settings_record};

/// The radio, found and checked.
struct Found<P: Port> {
    radio: CivRadio<P>,
    port: String,
    serial: Option<String>,
}

/// Finds the radio on `port`, or, with none given, on whichever serial port
/// looks like a known radio's and answers as one. On Windows, a radio
/// plugged in with no driver for its USB serial bridge gets one first (see
/// `driver`).
fn find(port: Option<&str>, running: &AtomicBool) -> Result<Found<SerialPort>, String> {
    let definitions = rigdef::built_in()?;
    if let Some(port) = port {
        let opened = SerialPort::open(port)?;
        let radio = CivRadio::find(opened, &definitions).map_err(|e| e.to_string())?;
        return Ok(Found {
            radio,
            port: port.to_owned(),
            serial: None,
        });
    }
    let candidates = civ_serial::radio_ports(&definitions)?;
    #[cfg(windows)]
    let candidates = if candidates.is_empty() && crate::driver::ensure(&definitions, running)? {
        civ_serial::radio_ports(&definitions)?
    } else {
        candidates
    };
    #[cfg(not(windows))]
    let _ = running;
    if candidates.is_empty() {
        return Err("no radio's USB serial port is on this computer.\n\
             -> Is the radio on, with its USB cable in? If it is, unplug the USB cable and \
             plug it in again. On Windows, Device Manager > Ports (COM & LPT) should show \
             \"Silicon Labs CP210x USB to UART Bridge\" for an IC-7300; if it is under \
             Other devices instead, its driver isn't installed."
            .to_owned());
    }
    let mut tried = Vec::new();
    for candidate in candidates {
        let answered = SerialPort::open(&candidate.port).and_then(|opened| {
            CivRadio::find(opened, &definitions).map_err(|error: RadioError| error.to_string())
        });
        match answered {
            Ok(radio) => {
                return Ok(Found {
                    radio,
                    port: candidate.port,
                    serial: candidate.serial,
                });
            }
            Err(why) => tried.push(format!("{}: {why}", candidate.port)),
        }
    }
    Err(format!(
        "no radio answered on any port that looked like one's:\n  {}\n\
         -> Is the radio on? Is another program (VarAC, WSJT-X, Hamlib's rigctld) holding \
         its port? Close it: the Agent is the radio's one owner.",
        tried.join("\n  ")
    ))
}

/// How often the radio is looked for again while it isn't there.
const LOOK_AGAIN: Duration = Duration::from_secs(5);

/// Looks for the radio until it is found or `running` turns false, so the
/// Agent can start before the radio is turned on (at Windows sign-in, for
/// example). Says why it isn't found once, and again only when the reason
/// changes.
fn wait_for_radio(port: Option<&str>, running: &AtomicBool) -> Option<Found<SerialPort>> {
    let mut said = String::new();
    while running.load(Ordering::SeqCst) {
        match find(port, running) {
            Ok(found) => return Some(found),
            Err(why) => {
                if why != said {
                    crate::print_event(&format!(
                        "waiting for the radio: {why}\n-> The Agent keeps looking every {} s; Ctrl-C stops.",
                        LOOK_AGAIN.as_secs()
                    ));
                    said = why;
                }
            }
        }
        let until = std::time::Instant::now() + LOOK_AGAIN;
        while running.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

/// Where settings records are kept: `records` beside the Agent.
pub(crate) fn records_folder() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|folder| folder.join("records")))
        .unwrap_or_else(|| "records".into())
}

/// Reads the radio's settings, keeps the first reading of this radio as its
/// as-received record, and says what changed since and what in them stops
/// digital modes working. Returns the reading.
fn check_settings<P: Port>(radio: &mut CivRadio<P>, serial: Option<&str>) -> Record {
    let definition = radio.definition().clone();
    let record = settings_record::read(
        radio,
        serial,
        std::time::SystemTime::now(),
        &settings_record::computer_name(),
    );
    let unread = record
        .settings
        .iter()
        .filter(|entry| matches!(entry.value, settings_record::Value::Unread { .. }))
        .count();
    println!(
        "Radio settings: read {} of {}.",
        record.settings.len() - unread,
        record.settings.len()
    );
    let path = settings_record::path_for(&records_folder(), &record);
    match settings_record::load(&path) {
        Ok(None) => match settings_record::save(&path, &record) {
            Ok(()) => println!(
                "Radio settings: kept as this radio's as-received record, {}",
                path.display()
            ),
            Err(error) => println!("Radio settings: {error}"),
        },
        Ok(Some(first)) => {
            let changed = settings_record::changes(&first, &record, &definition);
            if changed.is_empty() {
                println!("Radio settings: nothing has changed since the radio arrived.");
            } else {
                println!(
                    "Radio settings: {} changed since the radio arrived ({} on {}):",
                    changed.len(),
                    first.read_utc,
                    first.computer
                );
                for change in changed {
                    println!("  {}: {} -> {}", change.label, change.before, change.after);
                }
            }
        }
        Err(error) => println!("Radio settings: {error}"),
    }
    let found = settings_record::findings(&record, &definition);
    if found.is_empty() {
        println!("Radio settings: every one digital modes need is right.");
    }
    let fixable = crate::restore::fixes(&record, &definition);
    for finding in found {
        let wanted = finding.offer.unwrap_or_else(|| finding.accept.join(" or "));
        let then = if fixable
            .put
            .iter()
            .any(|put| put.setting.id == finding.setting)
        {
            format!("The Agent can set it to {wanted}: it asks below.")
        } else {
            format!("On the radio, set {} to {wanted}.", finding.label)
        };
        println!(
            "Radio settings: {} is {}. {}\n  -> {then}",
            finding.label, finding.now, finding.explain
        );
    }
    record
}

/// The link to the Engine, from `--engine` and the key in `--key-file`, or
/// a new key from joining as `station` when the file has none.
fn uplink(
    engine: &str,
    key_file: &str,
    station: Option<&str>,
    running: &AtomicBool,
) -> Result<Uplink, String> {
    let key = crate::station_key(engine, key_file, station, running, false)?;
    Uplink::new(engine, &key)
}

/// The radio's settings as a setup kept on the Engine has them, in lines to
/// show: each setting that differs, or that it matches.
fn compared(kept: &KeptReading, now: &Record, definition: &Definition) -> Vec<String> {
    let differ = settings_record::differences(
        kept.settings
            .iter()
            .map(|s| (s.id.as_str(), s.label.as_str(), s.bytes.as_deref())),
        now,
        definition,
    );
    let what = kept.name.as_deref().map_or_else(
        || "the as-received record".to_owned(),
        |name| format!("setup {name}"),
    );
    let whence = format!(
        "kept {} at {}{}",
        kept.saved_at,
        kept.station,
        kept.operator
            .as_deref()
            .map(|who| format!(" by {who}"))
            .unwrap_or_default()
    );
    if differ.is_empty() {
        return vec![format!("the radio matches {what} ({whence}).")];
    }
    let mut lines = vec![format!(
        "{} differ{} from {what} ({whence}):",
        differ.len(),
        if differ.len() == 1 { "s" } else { "" }
    )];
    lines.extend(differ.into_iter().map(|change| {
        format!(
            "  {}: the radio has {}; {what} has {}",
            change.label, change.after, change.before
        )
    }));
    lines
}

/// Every setup kept for the radio, compared with what it has now.
fn compared_all(kept: &KeptRadio, now: &Record, definition: &Definition) -> Vec<String> {
    if kept.setups.is_empty() {
        return vec![
            "no setups are kept for this radio yet.\n-> Set the radio up for a job, then \
             type keep NAME here (keep FT8, say)."
                .to_owned(),
        ];
    }
    let mut lines = Vec::new();
    for setup in &kept.setups {
        lines.extend(compared(setup, now, definition));
    }
    lines.push(
        "-> Type restore NAME to put a setup back. Settings the Agent can't write on this \
         radio are left for you to set by hand, and it says which."
            .to_owned(),
    );
    lines
}

/// Sends the radio's as-received record to the Engine, and says how the
/// radio compares with each setup kept there.
fn sync(uplink: &Uplink, record: &Record, definition: &Definition) {
    let Some(serial) = record.serial.as_deref() else {
        println!(
            "Engine: the radio's settings aren't kept there, because its serial number isn't \
             known (it comes with the USB serial port; leave out --civ to find it)."
        );
        return;
    };
    match uplink.keep_reading(record, None) {
        Ok(true) => {
            println!("Engine: kept this as the radio's as-received record, for every computer.");
        }
        Ok(false) => println!("Engine: already has this radio's as-received record."),
        Err(error) => {
            println!("Engine: {error}");
            return;
        }
    }
    match uplink.radio_settings(&record.maker, &record.model, serial) {
        Ok(kept) => {
            for line in compared_all(&kept, record, definition) {
                println!("Engine: {line}");
            }
        }
        Err(error) => println!("Engine: {error}"),
    }
}

/// A line typed at the Agent's console.
#[derive(Debug, PartialEq, Eq)]
enum Typed {
    /// Keep the radio's settings on the Engine as this setup.
    Keep(String),
    /// Compare the radio with every setup kept for it.
    Compare,
    /// Read the settings whose name is or holds this, and say what they are.
    Show(String),
    /// Offer again to fix what stops a job working.
    Fix,
    /// Show what putting this setup back would change, and wait for a yes.
    Restore(String),
    /// Yes: go ahead with the change just shown, or (`Some`) with the
    /// change asked as that question number, and only if it is still the
    /// one waiting.
    Yes(Option<u32>),
    /// No: leave the radio as it is.
    No,
    /// Stop the Agent, as Ctrl-C does.
    Quit,
    /// Turn auto mode on or off on this computer, or (`None`) say which.
    Auto(Option<bool>),
    /// Enroll this radio in lab settings control or leave it out, or
    /// (`None`) say which.
    Enroll(Option<bool>),
    /// Say what can be typed.
    Help,
    /// Nothing typed.
    Nothing,
}

/// What a typed line asks for.
fn typed(line: &str) -> Result<Typed, String> {
    let line = line.trim();
    let (word, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(word, rest)| (word, rest.trim()));
    match (word.to_ascii_lowercase().as_str(), rest) {
        ("", _) => Ok(Typed::Nothing),
        ("keep", "") => Err("keep needs a setup name, like keep FT8".to_owned()),
        ("keep", name) => {
            crate::engine::check_setup_name(name)?;
            Ok(Typed::Keep(name.to_owned()))
        }
        ("compare", "") => Ok(Typed::Compare),
        ("fix", "") => Ok(Typed::Fix),
        ("show", "") => Err("show needs a setting's name, like show DATA MOD".to_owned()),
        ("show", name) => Ok(Typed::Show(name.to_owned())),
        ("restore", "") => Err("restore needs a setup name, like restore FT8".to_owned()),
        ("restore", name) => {
            crate::engine::check_setup_name(name)?;
            Ok(Typed::Restore(name.to_owned()))
        }
        ("yes", "") => Ok(Typed::Yes(None)),
        ("yes", number) => number
            .parse()
            .map(|number| Typed::Yes(Some(number)))
            .map_err(|_| format!("yes takes a question number or nothing, not {number:?}")),
        ("no", "") => Ok(Typed::No),
        ("quit" | "exit", "") => Ok(Typed::Quit),
        ("auto", rest) => match rest.to_ascii_lowercase().as_str() {
            "" => Ok(Typed::Auto(None)),
            "on" => Ok(Typed::Auto(Some(true))),
            "off" => Ok(Typed::Auto(Some(false))),
            _ => Err("auto takes on or off, like auto on".to_owned()),
        },
        ("enroll", rest) => match rest.to_ascii_lowercase().as_str() {
            "" => Ok(Typed::Enroll(None)),
            "on" => Ok(Typed::Enroll(Some(true))),
            "off" => Ok(Typed::Enroll(Some(false))),
            _ => Err("enroll takes on or off, like enroll on".to_owned()),
        },
        ("help" | "?", "") => Ok(Typed::Help),
        _ => Err(format!("unknown command {line:?}")),
    }
}

/// What can be typed at the Agent's console.
const TYPE_HELP: &str = "Type keep NAME to keep the radio's settings as a setup (keep FT8, \
                         say), compare to see what differs from each setup, restore NAME to put \
                         one back, show NAME to read a setting (show DATA MOD, say), fix to \
                         fix what stops a job working, auto on to let the Agent make its \
                         fixes without asking, enroll on to put the lab's profiles on this \
                         radio as programs start, or quit to stop the Agent.";

/// What putting setup `name` back would change, from the radio's settings
/// now and the setup as it is kept.
fn restore_plan(
    now: &Record,
    kept: &KeptRadio,
    name: &str,
    definition: &Definition,
) -> Result<crate::restore::Plan, String> {
    let setup = kept
        .setups
        .iter()
        .find(|setup| setup.name.as_deref() == Some(name))
        .or_else(|| {
            kept.setups.iter().find(|setup| {
                setup
                    .name
                    .as_deref()
                    .is_some_and(|kept| kept.eq_ignore_ascii_case(name))
            })
        })
        .ok_or_else(|| {
            let names: Vec<&str> = kept
                .setups
                .iter()
                .filter_map(|s| s.name.as_deref())
                .collect();
            if names.is_empty() {
                "no setups are kept for this radio yet".to_owned()
            } else {
                format!(
                    "no setup {name} for this radio; it has {}",
                    names.join(", ")
                )
            }
        })?;
    Ok(crate::restore::plan(
        setup.name.as_deref().unwrap_or(name),
        setup
            .settings
            .iter()
            .map(|s| (s.id.as_str(), s.label.as_str(), s.bytes.as_deref())),
        now,
        definition,
    ))
}

/// What putting the lab's profile `name` on would change, if the lab has
/// one by that name for this radio's model: `restore VARA` puts the lab's
/// VARA profile on, after a yes.
fn lab_plan(now: &Record, name: &str, definition: &Definition) -> Option<crate::restore::Plan> {
    let profile = profiles::find(&now.maker, &now.model, name)?;
    Some(crate::restore::plan(
        &format!("{} lab profile", profile.name),
        profile.kept(definition),
        now,
        definition,
    ))
}

/// Why `keep NAME` is refused, when NAME is one of the lab's profiles for
/// this radio's model: those are locked, and only a new build of the Agent
/// changes them (brian's decision, 2026-10-28).
fn lab_locked(shared: &Shared, name: &str) -> Option<String> {
    let identity = shared.identity();
    let profile = profiles::find(&identity.maker, &identity.model, name)?;
    Some(format!(
        "setup {name} not kept: {} is the lab's locked profile for the {}, and only an \
         administrator changes it, with a new build of the Agent.\n-> Keep your own under \
         another name, like keep My {}; restore My {} puts it back.",
        profile.name, identity.model, profile.name, profile.name
    ))
}

/// Puts a confirmed plan back on the radio and records it beside the
/// settings records (Principle X).
fn restore(shared: &Shared, plan: &crate::restore::Plan, serial: &str) -> String {
    let done = shared.settings(|radio| {
        crate::restore::apply(
            radio,
            plan,
            serial,
            std::time::SystemTime::now(),
            &settings_record::computer_name(),
        )
    });
    let done = match done {
        Ok(done) => done,
        Err(why) => return format!("setup {} not put back: {why}", plan.setup),
    };
    let mut lines = done.lines();
    let path = crate::restore::path_for(&records_folder(), &done);
    lines.push(match settings_record::save(&path, &done) {
        Ok(()) => format!("Recorded in {}.", path.display()),
        Err(error) => format!("The restore wasn't recorded: {error}"),
    });
    lines.join("\n")
}

/// What the operator types, read from the moment the Agent starts: quit
/// (or the input closing, as when the tray stops) stops the Agent at any
/// time; every other line goes to the console once the radio is found.
#[derive(Clone)]
struct Typing {
    to_console: Arc<Mutex<Option<mpsc::Sender<String>>>>,
}

/// Starts reading what the operator types, until quit or the input
/// closes; either turns `running` false.
fn start_typing(running: Arc<AtomicBool>) -> Typing {
    let typing = Typing {
        to_console: Arc::new(Mutex::new(None)),
    };
    let slot = Arc::clone(&typing.to_console);
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if typed(&line) == Ok(Typed::Quit) {
                break;
            }
            let console = slot.lock().ok().and_then(|slot| slot.clone());
            match console {
                Some(console) => {
                    if console.send(line).is_err() {
                        break;
                    }
                }
                None if typed(&line) == Ok(Typed::Nothing) => {}
                None => crate::print_event(
                    "not yet: the Agent is still finding the radio (quit stops it).",
                ),
            }
        }
        crate::print_event("stopping: asked to quit.");
        running.store(false, Ordering::SeqCst);
    });
    typing
}

/// Starts the operator's console. Setups are kept on the Engine when there
/// is one, otherwise on this computer, so keep and restore work with no
/// network (Principle V).
fn start_console(
    shared: &Arc<Shared>,
    uplink: Option<Uplink>,
    serial: String,
    definition: Definition,
    record: &Record,
    typing: &Typing,
) -> Desk {
    let setups: Box<dyn Setups> = match uplink {
        Some(uplink) => Box::new(uplink),
        None => Box::new(crate::setups::Local::new(records_folder())),
    };
    println!("Setups: kept {}.", setups.place());
    println!("{TYPE_HELP}");
    let desk = Desk {
        waiting: Arc::new(Mutex::new(None)),
        asked: Arc::new(AtomicU32::new(0)),
        auto: Arc::new(AtomicBool::new(auto_mode::load(&auto_mode::path(
            &records_folder(),
        )))),
        serial,
    };
    if desk.auto.load(Ordering::SeqCst) {
        println!("Auto mode: on, so the Agent makes its fixes without asking (auto off asks).");
    }
    let enrolled = enrollment::load(&enrollment::path(&records_folder(), &desk.serial));
    let lab_model = profiles::any_for(&record.maker, &record.model);
    let plan = crate::restore::fixes(record, &definition);
    if lab_model && enrolled.is_none() {
        // One question waits at a time, so the fix offer waits until fix
        // is typed; enrolled, the lab's profile sets the rest anyway.
        let number = desk.ask(Asked::Enroll);
        println!(
            "{}\n{}",
            enroll_about(&record.model, &desk.serial),
            numbered(ENROLL_ASK, number)
        );
    } else {
        if lab_model {
            println!("{}", enrollment::said(enrolled));
        }
        if !plan.put.is_empty() {
            println!(
                "{}",
                desk.propose(shared, &fix_about(&plan), &fix_ask(&plan), plan)
            );
        }
    }
    let (lines, typed_lines) = mpsc::channel();
    if let Ok(mut slot) = typing.to_console.lock() {
        *slot = Some(lines);
    }
    let shared = Arc::clone(shared);
    let console_desk = desk.clone();
    std::thread::spawn(move || {
        console(
            &shared,
            setups.as_ref(),
            &definition,
            &console_desk,
            &typed_lines,
        );
    });
    desk
}

/// What a question asks the operator.
enum Asked {
    /// To make this change to the radio.
    Change(crate::restore::Plan),
    /// Whether the radio is one of the lab's, to enroll in lab settings
    /// control.
    Enroll,
}

/// What waits for the operator's yes, and its question number: shown last,
/// taken by the next line they type, whatever it is (Principle X).
type Waiting = Arc<Mutex<Option<(u32, Asked)>>>;

/// The operator's side of the Agent: what waits for their yes, how many
/// questions have been asked, whether auto mode is on, and the radio's
/// serial number.
#[derive(Clone)]
struct Desk {
    waiting: Waiting,
    asked: Arc<AtomicU32>,
    auto: Arc<AtomicBool>,
    serial: String,
}

impl Desk {
    /// Turns auto mode on or off on this computer, and says which it is.
    fn set_auto(&self, on: bool) -> String {
        match auto_mode::save(&auto_mode::path(&records_folder()), on) {
            Ok(()) => {
                self.auto.store(on, Ordering::SeqCst);
                auto_mode::said(on)
            }
            Err(error) => format!("auto mode not changed: {error}"),
        }
    }

    /// With auto mode on, makes the change `plan` says and says what
    /// became of it; otherwise leaves it waiting for a yes. `about` says
    /// what the change is and why, `ask` how to say yes (Principle X).
    fn propose(
        &self,
        shared: &Shared,
        about: &str,
        ask: &str,
        plan: crate::restore::Plan,
    ) -> String {
        if self.auto.load(Ordering::SeqCst) {
            format!(
                "{about}\nAuto mode: {}",
                restore(shared, &plan, &self.serial)
            )
        } else {
            let number = self.wait_for_yes(plan);
            format!("{about}\n{}", numbered(ask, number))
        }
    }

    /// Leaves `plan` waiting for the operator's yes, in place of anything
    /// that was, and returns its question number.
    fn wait_for_yes(&self, plan: crate::restore::Plan) -> u32 {
        self.ask(Asked::Change(plan))
    }

    /// Leaves `asked` waiting for the operator's answer, in place of
    /// anything that was, and returns its question number.
    fn ask(&self, asked: Asked) -> u32 {
        let number = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.waiting.lock() {
            *slot = Some((number, asked));
        }
        number
    }

    /// Enrolls this radio in lab settings control, or leaves it out, and
    /// says which it is.
    fn enroll(&self, on: bool) -> String {
        let path = enrollment::path(&records_folder(), &self.serial);
        match enrollment::save(&path, on) {
            Ok(()) => enrollment::said(Some(on)),
            Err(error) => format!("Lab profile: enrollment not changed: {error}"),
        }
    }
}

/// The question asked once of each radio of a model the lab has profiles
/// for, until it is answered.
const ENROLL_ASK: &str = "-> Type yes to enroll it in lab settings control; no leaves it \
                          alone for good (enroll on changes that later).";

/// What the question to enroll a radio says first.
fn enroll_about(model: &str, serial: &str) -> String {
    format!(
        "Lab profile: is this {model} (serial {serial}) a FieldLab radio? Enrolled, it gets \
         the lab's profile for each program put on as that program starts (VARA for VarAC, \
         say). Left out (no), such as a visitor's radio, the Agent never changes it on its own; \
         the tray's Lab radio menu changes that later."
    )
}

/// How to say yes, with the question's number, so an answer given to one
/// question (by the tray, say) is never taken for a newer one.
fn numbered(ask: &str, number: u32) -> String {
    format!("{ask} (question {number})")
}

/// How far each offer moves the drive level, in percent of its scale.
const DRIVE_STEP: u32 = 10;

/// ALC at or under this, in percent of its scale, leaves room for more
/// drive.
const ALC_LOW: u16 = 10;

/// Po under this many tenths of the power the radio is set to, with ALC
/// low, means too little drive.
const POWER_SHORT_TENTHS: u32 = 7;

/// Po under this, in percent, means no audio reached the radio, not too
/// little drive: the Agent never raises the drive for silence.
const POWER_SOME: u16 = 5;

/// The fewest meter readings (a quarter second apart) a transmission needs
/// before a low Po counts: a short burst may be read on its way up.
const READINGS_FOR_LOW: u16 = 4;

/// What the drive levels tried so far showed, in percent of the setting's
/// scale, so the next step never goes back past one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DriveTried {
    /// The latest level whose Po fell short.
    too_low: Option<u32>,
    /// The latest level whose ALC ran over a third.
    too_high: Option<u32>,
}

/// A level setting's value, and that value in percent of its scale.
fn level_percent(shared: &Shared, setting: &SettingSummary) -> Option<(Vec<u8>, u32)> {
    let [_, high] = setting.range?;
    let bytes = shared
        .settings(|radio| radio.read_exposed(setting))
        .ok()?
        .ok()?;
    let level = crate::civ::level_from_bcd(&bytes)?;
    let percent = (u32::from(level) * 100 + u32::from(high) / 2) / u32::from(high.max(1));
    Some((bytes, percent))
}

/// After a transmission whose ALC ran over a third of its scale, or whose
/// Po fell well short of the power the radio is set to with ALC near zero,
/// the change that moves the radio's drive level one step, and what it is;
/// `None` when there is nothing to change. A transmission while the
/// radio's own tuner was tuning says nothing about the drive. A step never
/// goes back past a level already tried: it stops halfway instead.
fn drive_offer(
    shared: &Shared,
    definition: &Definition,
    tried: &mut DriveTried,
) -> Option<(String, crate::restore::Plan)> {
    let peak = shared.last_peak()?;
    if peak.tuning {
        return None;
    }
    let meters = definition.meters.as_ref()?;
    let id = meters.drive.as_ref()?;
    let setting = definition.settings.iter().find(|s| &s.id == id)?;
    let [low, high] = setting.range?;
    if !(setting.write && setting.write_checked) {
        return None;
    }
    let (bytes, now) = level_percent(shared, setting)?;
    let set_power = meters
        .power_setting
        .as_ref()
        .and_then(|id| definition.settings.iter().find(|s| &s.id == id))
        .and_then(|power| level_percent(shared, power))
        .map(|(_, percent)| percent);
    let (target, why) = if peak.alc > rigctld_server::ALC_HIGHEST {
        tried.too_high = Some(now);
        let step = now.saturating_sub(DRIVE_STEP);
        let target = match tried.too_low {
            Some(floor) if step <= floor && floor < now => (floor + now).div_ceil(2),
            _ => step,
        };
        (target, format!("ALC peaked at {}% of its scale", peak.alc))
    } else if let Some(set) = set_power
        && peak.alc <= ALC_LOW
        && peak.power >= POWER_SOME
        && peak.readings >= READINGS_FOR_LOW
        && u32::from(peak.power) * 10 < set * POWER_SHORT_TENTHS
    {
        tried.too_low = Some(now);
        let step = (now + DRIVE_STEP).min(100);
        let target = match tried.too_high {
            Some(ceiling) if step >= ceiling && ceiling > now => u32::midpoint(now, ceiling),
            _ => step,
        };
        (
            target,
            format!(
                "Po reached {}% with the radio set to {set}% and ALC at {}%",
                peak.power, peak.alc
            ),
        )
    } else {
        return None;
    };
    if target == now {
        return None;
    }
    let to = u16::try_from((target * u32::from(high) + 50) / 100)
        .ok()?
        .clamp(u16::from(low), u16::from(high));
    let to = crate::civ::level_to_bcd(to)?.to_vec();
    if to == bytes {
        return None;
    }
    let shown =
        |bytes: &[u8]| settings_record::shown(definition, id, Some(&crate::civ::hex(bytes)));
    let plan = crate::restore::Plan {
        setup: "drive".to_owned(),
        fixes: true,
        put: vec![crate::restore::Put {
            setting: setting.clone(),
            now: shown(&bytes),
            setup: shown(&to),
            to,
        }],
        kept_back: Vec::new(),
    };
    let about = format!(
        "{why}: {} {} from {} to {}.",
        if target > now { "raise" } else { "lower" },
        setting.label,
        plan.put[0].now,
        plan.put[0].setup
    );
    Some((about, plan))
}

/// How to say yes to moving the drive.
const DRIVE_ASK: &str = "-> Type yes to set it, then transmit again to see the new meters; \
                         anything else leaves the radio as it is.";

/// What the Agent can fix.
fn fix_about(plan: &crate::restore::Plan) -> String {
    let mut lines = vec!["Radio settings: the Agent can fix what stops a job working.".to_owned()];
    lines.extend(plan.lines().into_iter().map(|line| format!("  {line}")));
    if plan.put.iter().any(|put| !put.setting.verified) {
        lines.push(
            "  The values' codes come from the radio's manual and haven't been checked on a \
             real radio yet: look at the radio's menu afterwards to see it says the same."
                .to_owned(),
        );
    }
    lines.join("\n")
}

/// How to say yes to the fixes.
fn fix_ask(plan: &crate::restore::Plan) -> String {
    format!(
        "-> Type yes to set {}; anything else leaves the radio as it is.",
        if plan.put.len() == 1 { "it" } else { "them" }
    )
}

/// The most settings `show` reads at once.
const SHOW_AT_MOST: usize = 12;

/// The settings `name` means: the one whose menu name or id it is, or else
/// every one whose menu name holds it.
fn named<'a>(definition: &'a Definition, name: &str) -> Vec<&'a SettingSummary> {
    let wanted = name.to_lowercase();
    let exact: Vec<&SettingSummary> = definition
        .settings
        .iter()
        .filter(|s| s.label.to_lowercase() == wanted || s.id == wanted)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    definition
        .settings
        .iter()
        .filter(|s| s.label.to_lowercase().contains(&wanted))
        .collect()
}

/// Reads the settings `name` means from the radio now, and says what they
/// are.
fn show(shared: &Shared, definition: &Definition, name: &str) -> String {
    let settings = named(definition, name);
    match settings.len() {
        0 => return format!("no setting is called {name:?}; its name on the radio's menu works"),
        n if n > SHOW_AT_MOST => {
            let names: Vec<&str> = settings.iter().map(|s| s.label.as_str()).collect();
            return format!(
                "{n} settings have {name:?} in their name; say which: {}",
                names.join(", ")
            );
        }
        _ => {}
    }
    let read = shared.settings(|radio| {
        settings
            .iter()
            .map(|setting| {
                let shown = match radio.read_exposed(setting) {
                    Ok(bytes) => settings_record::shown(
                        definition,
                        &setting.id,
                        Some(&crate::civ::hex(&bytes)),
                    ),
                    Err(error) => format!("not read: {error}"),
                };
                format!("{}: {shown}", setting.label)
            })
            .collect::<Vec<String>>()
    });
    match read {
        Ok(lines) => lines.join("\n"),
        Err(why) => format!("not read: {why}"),
    }
}

/// What to look at each time round: whenever the Agent has just unkeyed,
/// moves the drive a step if the meters show it too high or too low.
fn after_unkey<'a>(
    shared: &'a Shared,
    definition: &'a Definition,
    desk: Option<Desk>,
    on_air: &'a AtomicBool,
) -> impl FnMut() + 'a {
    let mut was_keyed = false;
    let mut tried = DriveTried::default();
    move || {
        let keyed = on_air.load(Ordering::SeqCst);
        if std::mem::replace(&mut was_keyed, keyed)
            && !keyed
            && let Some(desk) = &desk
            && let Some((about, plan)) = drive_offer(shared, definition, &mut tried)
        {
            crate::print_event(&desk.propose(shared, &about, DRIVE_ASK, plan));
        }
    }
}

/// Takes what waits for the operator's yes, if anything, and answers it
/// with what they typed: a change goes ahead only on a yes typed right
/// after it was shown and, with a question number, only if that question is
/// still the one waiting. Anything else leaves the radio as it is. Returns
/// whether the line was used up (a yes, or a no), or is still to be done
/// as typed.
fn answer_waiting(shared: &Shared, desk: &Desk, typed: &Result<Typed, String>) -> bool {
    let pending = desk.waiting.lock().ok().and_then(|mut slot| slot.take());
    let Some((number, asked)) = pending else {
        return false;
    };
    let yes = match typed {
        Ok(Typed::Yes(None)) => true,
        Ok(Typed::Yes(Some(answered))) if *answered == number => true,
        Ok(Typed::Yes(Some(answered))) => {
            crate::print_event(&format!(
                "question {answered} was replaced by question {number}; nothing changed."
            ));
            if let Ok(mut slot) = desk.waiting.lock() {
                slot.get_or_insert((number, asked));
            }
            return true;
        }
        _ => false,
    };
    let no = matches!(typed, Ok(Typed::No));
    crate::print_event(&match (asked, yes) {
        (Asked::Change(plan), true) => restore(shared, &plan, &desk.serial),
        (Asked::Change(plan), false) if plan.fixes => {
            "nothing fixed; the radio is as it was.".to_owned()
        }
        (Asked::Change(plan), false) => {
            format!("setup {} not put back; nothing changed.", plan.setup)
        }
        (Asked::Enroll, true) => desk.enroll(true),
        (Asked::Enroll, false) if no => desk.enroll(false),
        (Asked::Enroll, false) => {
            "Lab profile: not enrolled yet, so the Agent leaves the radio alone; it asks again \
             next time it starts (enroll on enrolls it now)."
                .to_owned()
        }
    });
    yes || no
}

/// Answers the operator's commands, from what they type, until the Agent
/// stops.
fn console(
    shared: &Shared,
    setups: &dyn Setups,
    definition: &Definition,
    desk: &Desk,
    lines: &mpsc::Receiver<String>,
) {
    let serial = desk.serial.as_str();
    let read = || {
        shared.settings(|radio| {
            settings_record::read(
                radio,
                Some(serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            )
        })
    };
    for line in lines {
        let typed = typed(&line);
        if answer_waiting(shared, desk, &typed) {
            continue;
        }
        let said = match typed {
            // Quit is read, and acted on, where the operator's typing is.
            Ok(Typed::Nothing | Typed::Quit) => continue,
            Ok(Typed::Yes(_) | Typed::No) => "nothing is waiting for a yes.".to_owned(),
            Ok(Typed::Restore(name)) => {
                match read().and_then(|now| match lab_plan(&now, &name, definition) {
                    Some(plan) => Ok(Ok(plan)),
                    None => setups
                        .kept(&now.maker, &now.model, serial)
                        .map(|kept| restore_plan(&now, &kept, &name, definition)),
                }) {
                    Ok(Ok(plan)) => {
                        let mut lines = plan.lines();
                        if !plan.put.is_empty() {
                            let ask = format!(
                                "-> Type yes to put {} back on the radio; anything else leaves it as it is.",
                                if plan.put.len() == 1 { "it" } else { "them" }
                            );
                            let number = desk.wait_for_yes(plan);
                            lines.push(numbered(&ask, number));
                        }
                        lines.join("\n")
                    }
                    Ok(Err(why)) => format!("not put back: {why}"),
                    Err(error) => format!("not put back: {error}"),
                }
            }
            Ok(Typed::Help) => TYPE_HELP.to_owned(),
            Ok(Typed::Auto(None)) => auto_mode::said(desk.auto.load(Ordering::SeqCst)),
            Ok(Typed::Auto(Some(on))) => desk.set_auto(on),
            Ok(Typed::Enroll(None)) => enrollment::said(enrollment::load(&enrollment::path(
                &records_folder(),
                serial,
            ))),
            Ok(Typed::Enroll(Some(on))) => desk.enroll(on),
            Ok(Typed::Show(name)) => show(shared, definition, &name),
            Ok(Typed::Fix) => match read() {
                Ok(now) => {
                    let plan = crate::restore::fixes(&now, definition);
                    if plan.put.is_empty() {
                        let mut lines =
                            vec!["nothing the Agent can set stops a job working.".to_owned()];
                        lines.extend(
                            plan.kept_back
                                .iter()
                                .map(|kept| format!("  {}: {}", kept.label, kept.why)),
                        );
                        lines.join("\n")
                    } else {
                        desk.propose(shared, &fix_about(&plan), &fix_ask(&plan), plan)
                    }
                }
                Err(why) => format!("not checked: {why}"),
            },
            Err(why) => format!("{why}\n-> {TYPE_HELP}"),
            Ok(Typed::Keep(name)) if lab_locked(shared, &name).is_some() => {
                lab_locked(shared, &name).unwrap_or_default()
            }
            Ok(Typed::Keep(name)) => match read() {
                Ok(record) => match setups.keep(&record, &name) {
                    Ok(()) => format!(
                        "kept the radio's settings {} as setup {name} ({} settings)",
                        setups.place(),
                        record.settings.len()
                    ),
                    Err(error) => format!("setup {name} not kept: {error}"),
                },
                Err(why) => format!("setup {name} not kept: {why}"),
            },
            Ok(Typed::Compare) => match read().and_then(|now| {
                setups
                    .kept(&now.maker, &now.model, serial)
                    .map(|kept| compared_all(&kept, &now, definition))
            }) {
                Ok(lines) => lines.join("\n"),
                Err(error) => format!("not compared: {error}"),
            },
        };
        crate::print_event(&said);
    }
}

/// The radio's audio, both ways, and VARA pointed at it. Returns what to
/// keep running and what to show while it runs.
#[cfg(windows)]
fn audio(sound: Option<&str>, on_air: Arc<AtomicBool>) -> Option<crate::audio_windows::Station> {
    use crate::audio_windows::Station;
    let Some(sound) = sound else {
        println!("Audio: this radio's definition doesn't name its USB sound device.");
        return None;
    };
    let station = match Station::start(sound, on_air) {
        Ok(station) => station,
        Err(why) => {
            println!("Audio: not carried ({why})");
            return None;
        }
    };
    println!(
        "Audio: the radio's receive audio plays into \"{}\".",
        crate::audio::RECEIVE_CABLE
    );
    match (&station.transmit_cable, &station.no_transmit) {
        (Some(cable), _) => println!(
            "Audio: programs' transmit audio, played into \"{cable}\", goes to the radio while it is keyed."
        ),
        (None, why) => println!(
            "Audio: transmit audio doesn't go through the Agent: {}.\n  -> Until it does, \
             a program's sound output must go straight to the radio's USB sound device.",
            why.as_deref().unwrap_or("no transmit cable")
        ),
    }
    vara(&station);
    Some(station)
}

/// Points VARA's sound devices at the Agent's cables, if VARA isn't running.
#[cfg(windows)]
fn vara(station: &crate::audio_windows::Station) {
    use crate::vara::{self, Outcome, Soundcard};
    let Some(path) = vara::find() else {
        println!(
            "VARA: not found in C:\\VARA or Program Files, so its sound devices were left alone."
        );
        return;
    };
    match vara::running() {
        Ok(false) => {}
        Ok(true) => {
            println!(
                "VARA: running, so its sound devices were left alone (it keeps its settings \
                 while it runs).\n  -> Close VARA: the Agent sets it up as soon as it closes."
            );
            return;
        }
        Err(why) => {
            println!("VARA: its sound devices were left alone ({why}).");
            return;
        }
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let now = vara::soundcard(&text);
    let (recording, playing) = crate::audio_windows::device_names();
    let wanted = Soundcard {
        input: Some(vara::as_vara_names_it(
            crate::audio::RECEIVE_CABLE_OUT,
            now.input.as_deref(),
            &recording,
        )),
        // Without a transmit cable, VARA plays straight into the radio's
        // own USB sound device, so it doesn't transmit silence.
        output: Some(vara::as_vara_names_it(
            station
                .transmit_cable
                .as_deref()
                .unwrap_or(&station.radio_playing),
            now.output.as_deref(),
            &playing,
        )),
    };
    match vara::set(&path, &wanted) {
        Ok(Outcome::AlreadySet) => println!(
            "VARA: its sound devices are already right (input {:?}, output {:?}).",
            wanted.input.as_deref().unwrap_or(""),
            wanted.output.as_deref().unwrap_or("")
        ),
        Ok(Outcome::Changed(before)) => println!(
            "VARA: sound devices set in {} (input {:?} was {:?}, output {:?} was {:?}). \
             The file as first found is kept as VARA.ini.lablog-original.",
            path.display(),
            wanted.input.as_deref().unwrap_or(""),
            before.input.as_deref().unwrap_or(""),
            wanted.output.as_deref().unwrap_or("(left alone)"),
            before.output.as_deref().unwrap_or(""),
        ),
        Err(why) => println!("VARA: its sound devices weren't set ({why})."),
    }
}

/// How the Agent tells which program connected: its process id and
/// executable, from where it connected and where the Agent listens.
type Identify = Box<dyn Fn(SocketAddr, SocketAddr) -> Result<(u32, String), String> + Send + Sync>;

/// How long a program's first transmission may wait for its lab profile
/// to go on.
const PROFILE_WAIT: Duration = Duration::from_secs(15);

/// The radio this `station` serves, and what the lab's profiles need to
/// know about it.
#[derive(Clone)]
struct LabRadio {
    shared: Arc<Shared>,
    serial: String,
    definition: Definition,
    /// The records folder, where a radio's enrollment is kept.
    records: std::path::PathBuf,
    /// How many profiles are going on now; a program's transmission waits
    /// until none are.
    going_on: Arc<(Mutex<u32>, Condvar)>,
}

/// The radio as programs reach it: the shared radio, with the lab's
/// profile for a program put on, once, as that program starts, on a radio
/// enrolled in lab settings control (Principle X, second exception). The
/// first program to start keeps the radio: a second one starting while it
/// is connected changes nothing.
struct Programs {
    lab: LabRadio,
    /// Whether the radio has a serial number to enroll it by.
    enrollable: bool,
    identify: Identify,
    /// Each connected program's process, where it could be told.
    connected: Mutex<HashMap<SocketAddr, u32>>,
    /// The processes whose profile has gone on: once per program start.
    started: Mutex<HashSet<u32>>,
}

impl Programs {
    /// The radio `station` serves, with serial number `serial` if known,
    /// telling programs apart the way Windows can.
    fn for_station(shared: &Arc<Shared>, serial: Option<&str>, definition: &Definition) -> Self {
        Self::new(
            LabRadio {
                shared: Arc::clone(shared),
                serial: serial.unwrap_or_default().to_owned(),
                definition: definition.clone(),
                records: records_folder(),
                going_on: Arc::new((Mutex::new(0), Condvar::new())),
            },
            serial.is_some(),
            Box::new(program_id::identify),
        )
    }

    fn new(lab: LabRadio, enrollable: bool, identify: Identify) -> Self {
        Self {
            lab,
            enrollable,
            identify,
            connected: Mutex::new(HashMap::new()),
            started: Mutex::new(HashSet::new()),
        }
    }

    /// Why the lab's profile for program `name` (executable `exe`,
    /// process `pid`) doesn't go on now, if it doesn't.
    fn not_now(&self, pid: u32, exe: &str, name: &str) -> Option<String> {
        let started = self.started.lock().ok()?;
        if started.contains(&pid) {
            return Some(String::new());
        }
        let first = self.connected.lock().ok().and_then(|connected| {
            connected
                .values()
                .find(|other| **other != pid && started.contains(other))
                .copied()
        });
        drop(started);
        if let Some(first) = first {
            return Some(format!(
                "Lab profile: {exe} ({name}) started while another program (process {first}) \
                 has the radio; the first program keeps it, so nothing was changed."
            ));
        }
        let identity = self.lab.shared.identity();
        let enrolled = self.enrollable
            && enrollment::load(&enrollment::path(&self.lab.records, &self.lab.serial))
                == Some(true);
        if !enrolled {
            return Some(format!(
                "Lab profile: {exe} is {name}, but this radio isn't enrolled in lab settings \
                 control, so the Agent left it as it is."
            ));
        }
        if profiles::find(&identity.maker, &identity.model, name).is_none() {
            return Some(format!(
                "Lab profile: {exe} is {name}; the lab has no {name} profile for the {} yet, so \
                 the radio is as it was.",
                identity.model
            ));
        }
        None
    }
}

impl LabRadio {
    /// Puts the lab's profile for program `name` on the radio, says what
    /// became of it, and records it like any restore.
    fn put_on(&self, name: &str, exe: &str) -> String {
        let identity = self.shared.identity();
        let Some(profile) = profiles::find(&identity.maker, &identity.model, name) else {
            return format!("Lab profile: no {name} profile for the {}.", identity.model);
        };
        let ids: Vec<&str> = profile.ids().collect();
        let serial = self.serial.as_str();
        let now = self.shared.settings(|radio| {
            settings_record::read_these(
                radio,
                &ids,
                Some(serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            )
        });
        let now = match now {
            Ok(now) => now,
            Err(why) => return format!("Lab profile: {name} not put on for {exe}: {why}"),
        };
        let plan = crate::restore::plan(
            &format!("{name} lab profile"),
            profile.kept(&self.definition),
            &now,
            &self.definition,
        );
        let head = format!("Lab profile: {exe} started, so the {name} profile goes on.");
        if plan.put.is_empty() && plan.kept_back.is_empty() {
            return format!("{head}\nThe radio already matches it; nothing changed.");
        }
        let mut lines = vec![head];
        lines.extend(plan.lines());
        if !plan.put.is_empty() {
            lines.push(restore(&self.shared, &plan, serial));
        }
        lines.join("\n")
    }

    /// Waits, up to [`PROFILE_WAIT`], until no profile is going on.
    /// Returns whether none is.
    fn ready(&self) -> bool {
        let (count, done) = &*self.going_on;
        let Ok(count) = count.lock() else {
            return true;
        };
        done.wait_timeout_while(count, PROFILE_WAIT, |count| *count > 0)
            .is_ok_and(|(count, _)| *count == 0)
    }

    /// Counts a profile going on (`on`), or done.
    fn going(&self, on: bool) {
        let (count, done) = &*self.going_on;
        if let Ok(mut count) = count.lock() {
            *count = if on {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
        }
        done.notify_all();
    }
}

impl Source for Programs {
    fn identity(&self) -> RadioIdentity {
        self.lab.shared.identity()
    }

    fn reading(&self) -> Result<Reading, String> {
        self.lab.shared.reading()
    }

    fn can_tune(&self) -> bool {
        self.lab.shared.can_tune()
    }

    fn tune(&self, tuning: Tuning) -> Result<(), String> {
        self.lab.shared.tune(tuning)
    }

    fn can_transmit(&self) -> bool {
        self.lab.shared.can_transmit()
    }

    fn transmit(&self, program: SocketAddr, on: bool) -> Result<(), String> {
        if on && !self.lab.ready() {
            return Err(
                "the lab profile is still going on the radio; transmit again in a moment"
                    .to_owned(),
            );
        }
        self.lab.shared.transmit(program, on)
    }

    fn connected(&self, program: SocketAddr, server: SocketAddr) {
        let (pid, exe) = match (self.identify)(program, server) {
            Ok(found) => found,
            Err(why) => {
                self.note(&format!(
                    "Lab profile: couldn't tell which program connected from {program} ({why}), \
                     so the radio is as it was."
                ));
                return;
            }
        };
        // The executable's own name, logged each time: the names the Agent
        // knows programs by are checked against these.
        let short = exe.rsplit(['\\', '/']).next().unwrap_or(&exe).to_owned();
        let Some(name) = program_id::setup_for(&exe) else {
            self.note(&format!(
                "Lab profile: {short} connected ({exe}); the lab has no profile for it, so the \
                 radio is as it was."
            ));
            return;
        };
        let why_not = self.not_now(pid, &short, name);
        if let Ok(mut connected) = self.connected.lock() {
            connected.insert(program, pid);
        }
        match why_not {
            Some(why) if why.is_empty() => {}
            Some(why) => self.note(&why),
            None => {
                if let Ok(mut started) = self.started.lock() {
                    started.insert(pid);
                }
                // Counted before the program is answered at all, so its
                // first transmission waits for the profile.
                self.lab.going(true);
                let lab = self.lab.clone();
                std::thread::spawn(move || {
                    let said = lab.put_on(name, &short);
                    lab.shared.note(&said);
                    lab.going(false);
                });
            }
        }
    }

    fn gone(&self, program: SocketAddr) {
        if let Ok(mut connected) = self.connected.lock() {
            connected.remove(&program);
        }
        self.lab.shared.gone(program);
    }

    fn watch(&self) {
        self.lab.shared.watch();
    }

    fn note(&self, event: &str) {
        self.lab.shared.note(event);
    }
}

/// The programs whose settings the Agent sets up, by executable, and the
/// names they are shown by.
#[cfg(any(windows, test))]
const SET_UP_PROGRAMS: [(&str, &str); 5] = [
    ("vara.exe", "VARA"),
    ("wsjtx.exe", "WSJT-X"),
    ("js8call.exe", "JS8Call"),
    ("fldigi.exe", "fldigi"),
    ("mmsstv.exe", "MMSSTV"),
];

/// How often the Agent looks for a program that has closed.
#[cfg(windows)]
const LOOK_FOR_CLOSED: Duration = Duration::from_secs(5);

/// Which of the programs the Agent sets up are open, looked at every
/// [`LOOK_FOR_CLOSED`].
#[cfg(windows)]
struct ProgramsOpen {
    open: HashSet<&'static str>,
    looked: std::time::Instant,
}

#[cfg(windows)]
impl ProgramsOpen {
    fn now() -> Self {
        Self {
            open: open_programs(&tasklist()),
            looked: std::time::Instant::now(),
        }
    }

    /// The name of a program that was open when last looked and is closed
    /// now, if one is (and it's time to look again).
    fn closed_since(&mut self) -> Option<&'static str> {
        if self.looked.elapsed() < LOOK_FOR_CLOSED {
            return None;
        }
        self.looked = std::time::Instant::now();
        let list = tasklist();
        if list.is_empty() {
            return None;
        }
        let now = open_programs(&list);
        let closed = self.open.difference(&now).next().copied();
        self.open = now;
        closed
    }
}

/// Windows' list of running programs, as `tasklist` writes it in CSV, or
/// nothing when it can't be read.
#[cfg(windows)]
fn tasklist() -> String {
    std::process::Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .output()
        .map(|listed| String::from_utf8_lossy(&listed.stdout).into_owned())
        .unwrap_or_default()
}

/// The programs the Agent sets up that `tasklist`'s CSV `list` shows
/// running, by the names they are shown by.
#[cfg(any(windows, test))]
fn open_programs(list: &str) -> HashSet<&'static str> {
    let images: HashSet<String> = list
        .lines()
        .filter_map(|line| line.split(',').next())
        .map(|image| image.trim_matches('"').to_ascii_lowercase())
        .collect();
    SET_UP_PROGRAMS
        .iter()
        .filter(|(image, _)| images.contains(*image))
        .map(|(_, name)| *name)
        .collect()
}

/// Tells the operator how programs on this computer reach the radio.
fn say_how_to_connect(model: &str, address: SocketAddr, limit: Duration) {
    println!(
        "Programs on this computer may tune and key the {} at {}: one at a time, for at most \
         {} s at once, and the radio's own PTT wins.",
        model,
        address,
        limit.as_secs()
    );
    println!(
        "  VarAC: Settings > RIG: Hamlib for PTT and Frequency Control, host localhost, port {}.",
        address.port()
    );
    println!(
        "  WSJT-X: Settings > Radio: Rig Hamlib NET rigctl, Network Server {address}, PTT Method CAT."
    );
}

/// Runs the station until Ctrl-C.
///
/// # Errors
///
/// The radio isn't found, the virtual radio can't listen, or `engine` (the
/// Engine's address, the key file, and the station to join as) doesn't
/// make a link to the Engine.
pub fn run(
    port: Option<&str>,
    pretend: bool,
    listen: SocketAddr,
    limit: Duration,
    engine: Option<(&str, &str, Option<&str>)>,
) -> Result<(), String> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    let typing = start_typing(Arc::clone(&running));
    // A radio unplugged, turned off or swapped for another is looked for
    // again, so any radio can be moved to this computer while the Agent runs.
    loop {
        let uplink = engine
            .map(|(engine, key_file, station)| uplink(engine, key_file, station, &running))
            .transpose()?;
        let ended = if pretend {
            let found = Found {
                radio: crate::civ_pretend_radio().map_err(|error| error.to_string())?,
                port: "the pretend port".to_owned(),
                serial: Some(PRETEND_SERIAL.to_owned()),
            };
            run_found(found, listen, limit, uplink, &running, &typing)?
        } else {
            let Some(found) = wait_for_radio(port, &running) else {
                break;
            };
            run_found(found, listen, limit, uplink, &running, &typing)?
        };
        if ended == Ended::Stopped {
            break;
        }
        crate::print_event(&format!(
            "Radio lost: it hasn't answered for {} s (unplugged, turned off, or swapped).\n\
             -> The Agent looks for a radio again; plug in any radio it knows.",
            LOST_AFTER.as_secs()
        ));
    }
    println!("stopped");
    Ok(())
}

/// Starts carrying QSOs, the radio's state and who LabLog logs as between
/// the programs and the Engine, when there is one.
fn start_carrier(uplink: Option<&Uplink>, shared: &Arc<Shared>) -> Option<crate::carrier::Carrier> {
    let Some(uplink) = uplink else {
        println!(
            "QSOs: stay in each program's own log (start the Agent with --engine to send them \
             to LabLog)."
        );
        return None;
    };
    let radio = Arc::clone(shared) as Arc<dyn Source>;
    crate::carrier::start(uplink.clone(), radio, &records_folder())
        .map_err(|why| println!("QSOs: not sent to LabLog ({why})."))
        .ok()
}

/// The serial number the pretend radio's settings are kept under.
const PRETEND_SERIAL: &str = "PRETEND";

/// Runs the station with the radio found, until stopped.
fn run_found<P: Port + Send + 'static>(
    mut found: Found<P>,
    listen: SocketAddr,
    limit: Duration,
    uplink: Option<Uplink>,
    running: &AtomicBool,
    typing: &Typing,
) -> Result<Ended, String> {
    let identity = found.radio.identity().clone();
    println!(
        "Found: {} {} on {}{}, at {} baud.",
        identity.maker,
        identity.model,
        found.port,
        found
            .serial
            .as_deref()
            .map(|serial| format!(" (USB serial {serial})"))
            .unwrap_or_default(),
        found.radio.baud()
    );
    let record = check_settings(&mut found.radio, found.serial.as_deref());
    let definition = found.radio.definition().clone();
    if let Some(uplink) = &uplink {
        sync(uplink, &record, &definition);
    }
    let sound = definition.usb_audio.clone();
    let (notes, heard) = mpsc::channel();
    let shared = Arc::new(Shared::with_transmit(Box::new(found.radio), limit).with_notes(notes));
    let on_air = shared.on_air();
    #[cfg(windows)]
    let station = audio(sound.as_deref(), Arc::clone(&on_air));
    #[cfg(not(windows))]
    {
        let _ = (&sound, &on_air);
        println!("Audio: carried on Windows only.");
    }
    let programs = Programs::for_station(&shared, found.serial.as_deref(), &definition);
    let server = rigctld_server::Server::start(listen, Arc::new(programs) as Arc<dyn Source>)?;
    say_how_to_connect(&identity.model, server.address(), limit);
    #[cfg(windows)]
    if let Some(station) = &station {
        crate::programs::set_up(station, server.address());
    }
    let carrier = start_carrier(uplink.as_ref(), &shared);
    let desk = found
        .serial
        .clone()
        .map(|serial| start_console(&shared, uplink, serial, definition.clone(), &record, typing));
    let mut offer_drive = after_unkey(&shared, &definition, desk, &on_air);
    println!("Ctrl-C or quit stops.");
    #[cfg(windows)]
    let mut watching = Watching::new();
    let serving = AtomicBool::new(true);
    let mut lost = Lost::default();
    crate::show_until_stopped(shared.as_ref(), &heard, &serving, || {
        offer_drive();
        #[cfg(windows)]
        if let Some(station) = &station {
            watching.watch(station, &on_air, server.address());
        }
        if !running.load(Ordering::SeqCst) || lost.now(shared.as_ref()) {
            serving.store(false, Ordering::SeqCst);
        }
    });
    drop(carrier);
    drop(server);
    #[cfg(windows)]
    drop(station);
    // The console lets go of the radio, so it can be found again.
    if let Ok(mut slot) = typing.to_console.lock() {
        *slot = None;
    }
    Ok(if running.load(Ordering::SeqCst) {
        Ended::Lost
    } else {
        Ended::Stopped
    })
}

/// How a station run ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Asked to stop.
    Stopped,
    /// The radio stopped answering: unplugged, turned off, or swapped.
    Lost,
}

/// How long the radio may go unread before the Agent counts it gone and
/// looks for a radio again.
const LOST_AFTER: Duration = Duration::from_secs(15);

/// How often the Agent checks that the radio still answers.
const LOOK_FOR_LOST: Duration = Duration::from_secs(2);

/// Whether the radio has stopped answering for `after` ([`LOST_AFTER`]),
/// looked at every `every` ([`LOOK_FOR_LOST`]).
struct Lost {
    after: Duration,
    every: Duration,
    looked: Option<std::time::Instant>,
    failing_since: Option<std::time::Instant>,
}

impl Default for Lost {
    fn default() -> Self {
        Self {
            after: LOST_AFTER,
            every: LOOK_FOR_LOST,
            looked: None,
            failing_since: None,
        }
    }
}

impl Lost {
    fn now(&mut self, radio: &dyn Source) -> bool {
        if self.looked.is_some_and(|at| at.elapsed() < self.every) {
            return false;
        }
        self.looked = Some(std::time::Instant::now());
        if radio.reading().is_ok() {
            self.failing_since = None;
            return false;
        }
        self.failing_since
            .get_or_insert_with(std::time::Instant::now)
            .elapsed()
            >= self.after
    }
}

/// What the station watches on Windows while it runs: sound device errors,
/// the transmit and receive audio levels, and programs closing.
#[cfg(windows)]
struct Watching {
    hint: Option<&'static str>,
    last: std::time::Instant,
    was_on_air: bool,
    open: ProgramsOpen,
}

#[cfg(windows)]
impl Watching {
    fn new() -> Self {
        Self {
            hint: None,
            last: std::time::Instant::now(),
            was_on_air: false,
            open: ProgramsOpen::now(),
        }
    }

    fn watch(
        &mut self,
        station: &crate::audio_windows::Station,
        on_air: &AtomicBool,
        radio: SocketAddr,
    ) {
        // A program open when the Agent started keeps its settings while it
        // runs and writes them as it closes, so the Agent sets it up right
        // after it closes, ready for its next start.
        if let Some(closed) = self.open.closed_since() {
            crate::print_event(&format!(
                "{closed} closed, so the Agent sets up the programs that are closed now."
            ));
            vara(station);
            crate::programs::set_up(station, radio);
        }
        for error in station.errors.try_iter() {
            crate::print_event(&format!("sound device error: {error}"));
        }
        // How loud a program's transmit audio was, each time the radio is
        // unkeyed: silence means the program isn't playing into the
        // transmit cable.
        if let (Some(transmit), Some(cable)) = (&station.transmit, &station.transmit_cable) {
            let on_air_now = on_air.load(Ordering::SeqCst);
            if on_air_now && !self.was_on_air {
                let _ = transmit.take_peak_dbfs();
            }
            if self.was_on_air && !on_air_now {
                crate::print_event(&match transmit.take_peak_dbfs() {
                    Some(db) => format!("transmit audio: {db:.0} dBFS peak"),
                    None => format!(
                        "transmit audio: silent\n-> The program's sound output should be \"{cable}\"."
                    ),
                });
            }
            self.was_on_air = on_air_now;
        }
        // The receive level, said only when what to do about it changes.
        if self.last.elapsed() >= Duration::from_secs(5) {
            self.last = std::time::Instant::now();
            let now = station.receive_hint();
            if now != self.hint {
                crate::print_event(&match now {
                    Some(what) => format!("receive audio: {what}"),
                    None => "receive audio: level good".to_owned(),
                });
                self.hint = now;
            }
        }
    }
}

/// `check-writes`: finds the radio, and once the operator says yes, writes
/// each setting the Agent may change back to its own value and says which
/// the radio took. Nothing on the radio changes (Principle X).
///
/// # Errors
///
/// The radio isn't found, or the console can't be read.
pub fn check_writes(port: Option<&str>) -> Result<(), String> {
    let running = AtomicBool::new(true);
    let mut found = find(port, &running)?;
    let definition = found.radio.definition().clone();
    let changeable = definition.settings.iter().filter(|s| s.write).count();
    println!(
        "Found: {} {} on {}{}.",
        definition.maker,
        definition.model,
        found.port,
        found
            .serial
            .as_deref()
            .map(|serial| format!(" (USB serial {serial})"))
            .unwrap_or_default()
    );
    println!(
        "This writes each of {changeable} settings back to the value the radio has for it, \
         and reads it again, to find out which the Agent can set. The radio should end as it \
         began. Don't transmit while it runs.\n-> Type yes to start; anything else stops."
    );
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("can't read the console: {error}"))?;
    if !answer.trim().eq_ignore_ascii_case("yes") {
        println!("Stopped; nothing was written.");
        return Ok(());
    }
    let report = crate::write_check::check(
        &mut found.radio,
        found.serial.as_deref(),
        std::time::SystemTime::now(),
        &settings_record::computer_name(),
        |done, of| {
            if done % 25 == 0 || done == of {
                println!("  {done} of {of}");
            }
        },
    );
    for line in report.lines() {
        println!("{line}");
    }
    let path = crate::write_check::path_for(&records_folder(), &report);
    match settings_record::save(&path, &report) {
        Ok(()) => println!("Report kept in {}", path.display()),
        Err(error) => println!("The report wasn't kept: {error}"),
    }
    Ok(())
}

/// Where Windows keeps the programs it runs at each sign-in, for this user
/// (Microsoft Learn, "Run and `RunOnce` Registry Keys").
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// The Agent's entry under it.
#[cfg(windows)]
const RUN_NAME: &str = "LabLog Station Agent";

/// The command line Windows runs at sign-in: this program's tray, with
/// `options` for the `station` it runs. Windows takes at most 260
/// characters there.
#[cfg(any(windows, test))]
fn startup_command(exe: &std::path::Path, options: &[String]) -> Result<String, String> {
    let mut command = format!("\"{}\" tray", exe.display());
    for option in options {
        command.push(' ');
        command.push_str(&crate::tray::quoted(option)?);
    }
    if command.chars().count() > 260 {
        return Err(format!(
            "the command is too long for Windows to start it at sign-in ({command})"
        ));
    }
    Ok(command)
}

/// `startup on`, `startup off` or `startup status`: whether Windows starts
/// the tray each time this user signs in. `startup on` also starts it now,
/// with `options` for its `station`.
///
/// # Errors
///
/// Windows' registry couldn't be read or changed, or this isn't Windows.
pub fn startup(what: &str, options: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        let reg = |args: &[&str]| {
            std::process::Command::new("reg")
                .args(args)
                .output()
                .map_err(|error| format!("can't run reg: {error}"))
        };
        match what {
            "on" => {
                let exe = std::env::current_exe()
                    .map_err(|error| format!("can't tell where the Agent is: {error}"))?;
                let command = startup_command(&exe, options)?;
                let done = reg(&[
                    "add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &command, "/f",
                ])?;
                if !done.status.success() {
                    return Err(format!(
                        "Windows refused: {}",
                        String::from_utf8_lossy(&done.stderr).trim()
                    ));
                }
                println!("Windows will run {command} each time you sign in.");
                println!("Windows may wait a little after sign-in before starting it.");
                crate::tray::start_hidden(&exe, options)?;
                println!(
                    "The Agent is starting now too: look for its icon by the clock (it may be \
                     under the ^ arrow)."
                );
                Ok(())
            }
            "off" => {
                let done = reg(&["delete", RUN_KEY, "/v", RUN_NAME, "/f"])?;
                if done.status.success() {
                    println!("Windows won't start the Agent at sign-in any more.");
                } else {
                    println!("Windows wasn't starting the Agent at sign-in.");
                }
                println!("If the Agent's icon is by the clock, its menu's Quit stops it.");
                Ok(())
            }
            "status" => {
                let done = reg(&["query", RUN_KEY, "/v", RUN_NAME])?;
                let text = String::from_utf8_lossy(&done.stdout);
                match text.lines().find(|line| line.contains("REG_SZ")) {
                    Some(line) if done.status.success() => println!(
                        "Windows starts the Agent at sign-in: {}",
                        line.split("REG_SZ").nth(1).unwrap_or_default().trim()
                    ),
                    _ => println!("Windows doesn't start the Agent at sign-in."),
                }
                Ok(())
            }
            other => Err(format!("startup takes on, off or status, not {other:?}")),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (what, options);
        Err(
            "startup is for Windows; elsewhere, use the system's own way to start programs"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        DRIVE_ASK, Desk, DriveTried, LabRadio, Lost, Programs, Typed, compared, compared_all,
        drive_offer, lab_locked, lab_plan, open_programs, restore_plan, startup_command, typed,
    };
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::engine::{KeptRadio, KeptReading, KeptSetting};
    use crate::rigctld_server::{Shared, Source};
    use crate::rigdef;
    use crate::settings_record;

    /// What the pretend radio has for the setting `id` now, in hex.
    fn setting_now(shared: &Shared, id: &str) -> Option<String> {
        shared
            .settings(|radio| {
                settings_record::read_these(radio, &[id], Some("PRETEND"), UNIX_EPOCH, "test")
            })
            .ok()
            .and_then(|record| record.settings.into_iter().next())
            .and_then(|entry| entry.bytes)
    }

    #[test]
    fn puts_the_lab_profile_on_an_enrolled_radio_once_as_varac_starts() {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let radio = CivRadio::find(
            PretendPort::new(PretendCiv::new(&definition), 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let (notes, heard) = std::sync::mpsc::channel();
        let shared = Arc::new(
            Shared::with_transmit(Box::new(radio), Duration::from_mins(1)).with_notes(notes),
        );
        let records = std::env::temp_dir().join(format!("lablog-lab-{}", std::process::id()));
        let lab = LabRadio {
            shared: Arc::clone(&shared),
            serial: "PRETEND".to_owned(),
            definition,
            records: records.clone(),
            going_on: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
        };
        // VarAC from one port, WSJT-X from another; anything else unknown.
        let programs = Programs::new(
            lab,
            true,
            Box::new(|program: SocketAddr, _| match program.port() {
                50_001 | 50_002 => Ok((4242, r"C:\VarAC\VarAC.exe".to_owned())),
                50_003 => Ok((777, r"C:\WSJT\wsjtx\bin\wsjtx.exe".to_owned())),
                _ => Err("not found".to_owned()),
            }),
        );
        let server = SocketAddr::from(([127, 0, 0, 1], 4532));
        let from = |port: u16| SocketAddr::from(([127, 0, 0, 1], port));
        // The next note that says `what`, passing over the others.
        let note_with = |what: &str| loop {
            match heard.recv_timeout(Duration::from_secs(10)) {
                Ok(note) if note.contains(what) => break note,
                Ok(_) => {}
                Err(_) => break String::new(),
            }
        };
        assert_eq!(setting_now(&shared, "filter_width").as_deref(), Some("24"));

        // Not enrolled: left as it is.
        programs.connected(from(50_001), server);
        assert!(note_with("Lab profile").contains("isn't enrolled"));
        programs.gone(from(50_001));
        assert_eq!(setting_now(&shared, "filter_width").as_deref(), Some("24"));

        // Enrolled: the VARA profile goes on, and a transmission waits for it.
        assert!(
            crate::enrollment::save(&crate::enrollment::path(&records, "PRETEND"), true).is_ok()
        );
        programs.connected(from(50_001), server);
        assert!(programs.transmit(from(50_001), true).is_ok());
        assert!(programs.transmit(from(50_001), false).is_ok());
        let said = note_with("Lab profile");
        assert!(
            said.contains("VarAC.exe started, so the VARA profile goes on"),
            "{said}"
        );
        assert_eq!(setting_now(&shared, "filter_width").as_deref(), Some("27"));
        assert_eq!(
            setting_now(&shared, "usb_mod_level").as_deref(),
            Some("01 02")
        );
        assert_eq!(setting_now(&shared, "data_mod").as_deref(), Some("03"));

        // The operator's change stands while VarAC runs, even when it
        // connects again; a second program doesn't take the radio.
        assert!(
            shared
                .settings(|radio| {
                    let width = radio
                        .definition()
                        .settings
                        .iter()
                        .find(|s| s.id == "filter_width")
                        .cloned()
                        .unwrap_or_else(|| unreachable!());
                    radio.write_exposed(&width, &[0x30])
                })
                .is_ok_and(|written| written.is_ok())
        );
        programs.connected(from(50_002), server);
        programs.connected(from(50_003), server);
        assert!(note_with("Lab profile").contains("the first program keeps it"));
        programs.connected(from(50_004), server);
        assert!(note_with("Lab profile").contains("couldn't tell which program"));
        assert_eq!(setting_now(&shared, "filter_width").as_deref(), Some("30"));
        let _ = std::fs::remove_dir_all(&records);
    }

    /// A radio that answers or doesn't, as the test says.
    struct Fickle(AtomicBool, crate::rig::RadioIdentity);

    impl Source for Fickle {
        fn identity(&self) -> crate::rig::RadioIdentity {
            self.1.clone()
        }

        fn reading(&self) -> Result<crate::rig::Reading, String> {
            if self.0.load(Ordering::SeqCst) {
                return Err("unplugged".to_owned());
            }
            Ok(crate::rig::Reading {
                frequency_hz: 14_074_000,
                mode: None,
                passband_hz: None,
                ptt: None,
                vfo: None,
            })
        }
    }

    #[test]
    fn counts_the_radio_lost_only_after_it_stops_answering_for_a_while() {
        let radio = Fickle(AtomicBool::new(false), transmitted([0, 0, 0]).0.identity());
        let mut lost = Lost {
            after: Duration::from_millis(50),
            every: Duration::ZERO,
            looked: None,
            failing_since: None,
        };
        assert!(!lost.now(&radio), "it answers");
        radio.0.store(true, Ordering::SeqCst);
        assert!(!lost.now(&radio), "one miss isn't lost");
        std::thread::sleep(Duration::from_millis(60));
        assert!(lost.now(&radio), "still not answering: lost");
        radio.0.store(false, Ordering::SeqCst);
        assert!(!lost.now(&radio), "answering again");
        radio.0.store(true, Ordering::SeqCst);
        assert!(!lost.now(&radio), "the clock starts again");
    }

    #[test]
    fn reads_which_programs_are_open_from_windows_list() {
        let list = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\
                    \"VARA.exe\",\"4120\",\"Console\",\"1\",\"30,112 K\"\r\n\
                    \"wsjtx.exe\",\"5004\",\"Console\",\"1\",\"80,000 K\"\r\n";
        let open = open_programs(list);
        assert_eq!(open.len(), 2);
        assert!(open.contains("VARA") && open.contains("WSJT-X"));
        assert!(open_programs("").is_empty());
    }

    #[test]
    fn the_labs_profiles_are_locked_and_restore_puts_one_on() {
        let (shared, definition) = transmitted([0, 0, 0]);
        let locked = lab_locked(&shared, "vara").unwrap_or_default();
        assert!(
            locked.contains("the lab's locked profile for the IC-7300"),
            "{locked}"
        );
        assert!(locked.contains("keep My VARA"));
        assert_eq!(lab_locked(&shared, "My VARA"), None);
        assert_eq!(lab_locked(&shared, "Field Day"), None);
        let now = shared
            .settings(|radio| settings_record::read(radio, Some("PRETEND"), UNIX_EPOCH, "test"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        let plan = lab_plan(&now, "VARA", &definition);
        let put: Vec<String> = plan
            .iter()
            .flat_map(|plan| plan.put.iter().map(|put| put.setting.id.clone()))
            .collect();
        assert!(put.contains(&"filter_width".to_owned()), "{put:?}");
        assert!(put.contains(&"usb_mod_level".to_owned()), "{put:?}");
        assert!(lab_plan(&now, "My VARA", &definition).is_none());
    }

    /// The pretend IC-7300, shared for transmitting, after one transmission
    /// whose meters read `meters` (raw Po, SWR, ALC levels).
    fn transmitted(meters: [u16; 3]) -> (Shared, rigdef::Definition) {
        transmitted_for(meters, false, 1)
    }

    /// The same, with its tuner tuning or not, keyed for `readings` meter
    /// readings.
    fn transmitted_for(
        meters: [u16; 3],
        tuning: bool,
        readings: u32,
    ) -> (Shared, rigdef::Definition) {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let mut pretend = PretendCiv::new(&definition);
        pretend.set_tx_meters(meters);
        pretend.set_tuning(tuning);
        let radio = CivRadio::find(
            PretendPort::new(pretend, 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let shared = Shared::with_transmit(Box::new(radio), Duration::from_mins(1));
        let program = SocketAddr::from(([127, 0, 0, 1], 50_001));
        assert!(shared.transmit(program, true).is_ok());
        for _ in 0..readings {
            std::thread::sleep(Duration::from_millis(270));
            shared.watch();
        }
        assert!(shared.transmit(program, false).is_ok());
        (shared, definition)
    }

    #[test]
    fn offers_to_lower_the_drive_only_when_alc_ran_high() {
        // ALC 84 of 120 is 70% of its scale.
        let (shared, definition) = transmitted([60, 20, 84]);
        let Some((say, plan)) = drive_offer(&shared, &definition, &mut DriveTried::default())
        else {
            unreachable!("ALC ran high")
        };
        assert!(
            say.starts_with("ALC peaked at 70% of its scale: lower USB MOD Level from"),
            "{say}"
        );
        assert!(say.contains("(50%) to 102 of 255 (40%)"), "{say}");
        assert_eq!(plan.put.len(), 1);
        assert_eq!(plan.put[0].setting.id, "usb_mod_level");
        assert_eq!(plan.put[0].to, [0x01, 0x02]);
        let done = shared
            .settings(|radio| crate::restore::apply(radio, &plan, "test", UNIX_EPOCH, "test-pc"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(done.lines()[0], "fixed 1 of 1 setting.");
        let Some((say, _)) = drive_offer(&shared, &definition, &mut DriveTried::default()) else {
            unreachable!("still the same peak")
        };
        assert!(
            say.contains("from 102 of 255 (40%) to 77 of 255 (30%)"),
            "{say}"
        );
        // ALC 40 of 120 is a third: nothing to offer.
        let (shared, definition) = transmitted([60, 20, 40]);
        assert!(drive_offer(&shared, &definition, &mut DriveTried::default()).is_none());
    }

    #[test]
    fn raises_the_drive_when_po_falls_short_and_alc_stays_low() {
        // Po 29 of 213 is 10%; the pretend RF power is 50%.
        let (shared, definition) = transmitted_for([29, 0, 0], false, 4);
        let mut tried = DriveTried::default();
        let Some((say, _)) = drive_offer(&shared, &definition, &mut tried) else {
            unreachable!("Po fell short")
        };
        assert_eq!(
            say,
            "Po reached 10% with the radio set to 50% and ALC at 0%: raise USB MOD Level \
             from 127 of 255 (50%) to 153 of 255 (60%)."
        );
        assert_eq!(tried.too_low, Some(50));
        // Never back past a level whose ALC ran high: halfway instead.
        let mut tried = DriveTried {
            too_low: None,
            too_high: Some(55),
        };
        let halfway = drive_offer(&shared, &definition, &mut tried).map(|(say, _)| say);
        assert!(
            halfway
                .as_deref()
                .is_some_and(|say| say.ends_with("(52%).")),
            "{halfway:?}"
        );
        // Silence, and a burst too short to judge, change nothing.
        let (shared, definition) = transmitted_for([0, 0, 0], false, 4);
        assert!(drive_offer(&shared, &definition, &mut DriveTried::default()).is_none());
        let (shared, definition) = transmitted_for([29, 0, 0], false, 1);
        assert!(drive_offer(&shared, &definition, &mut DriveTried::default()).is_none());
    }

    #[test]
    fn says_nothing_about_the_drive_while_the_tuner_tunes() {
        let (shared, definition) = transmitted_for([60, 20, 84], true, 1);
        assert!(shared.last_peak().is_some_and(|peak| peak.tuning));
        assert!(drive_offer(&shared, &definition, &mut DriveTried::default()).is_none());
    }

    #[test]
    fn auto_mode_makes_the_change_and_otherwise_waits_for_a_yes() {
        assert_eq!(typed("auto"), Ok(Typed::Auto(None)));
        assert_eq!(typed("AUTO ON"), Ok(Typed::Auto(Some(true))));
        assert_eq!(typed("auto off"), Ok(Typed::Auto(Some(false))));
        assert!(typed("auto maybe").is_err());
        let (shared, definition) = transmitted([60, 20, 84]);
        let desk = Desk {
            waiting: Arc::new(Mutex::new(None)),
            asked: Arc::new(AtomicU32::new(0)),
            auto: Arc::new(AtomicBool::new(false)),
            serial: "test".to_owned(),
        };
        let Some((about, plan)) = drive_offer(&shared, &definition, &mut DriveTried::default())
        else {
            unreachable!("ALC ran high")
        };
        let said = desk.propose(&shared, &about, DRIVE_ASK, plan.clone());
        assert!(
            said.ends_with(&format!("{DRIVE_ASK} (question 1)")),
            "asks first, numbered: {said}"
        );
        assert!(
            desk.waiting
                .lock()
                .is_ok_and(|slot| slot.as_ref().is_some_and(|(number, _)| *number == 1)),
            "and waits for the yes"
        );
        let again = desk.propose(&shared, &about, DRIVE_ASK, plan.clone());
        assert!(again.ends_with("(question 2)"), "a new number: {again}");
        let unchanged =
            drive_offer(&shared, &definition, &mut DriveTried::default()).map(|(about, _)| about);
        assert_eq!(
            unchanged.as_deref(),
            Some(about.as_str()),
            "nothing written"
        );
        desk.auto.store(true, Ordering::SeqCst);
        let said = desk.propose(&shared, &about, DRIVE_ASK, plan);
        assert!(
            said.contains("\nAuto mode: fixed 1 of 1 setting."),
            "makes it and says so: {said}"
        );
        let next =
            drive_offer(&shared, &definition, &mut DriveTried::default()).map(|(about, _)| about);
        assert!(
            next.is_some_and(|about| about.contains("from 102 of 255 (40%)")),
            "the radio has the new level"
        );
    }

    #[test]
    fn show_finds_a_setting_by_its_menu_name() {
        assert_eq!(
            typed("show data mod"),
            Ok(Typed::Show("data mod".to_owned()))
        );
        assert!(typed("show").is_err());
        assert_eq!(typed("FIX"), Ok(Typed::Fix));
        assert!(typed("fix now").is_err());
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let exact: Vec<&str> = super::named(&definition, "data mod")
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(
            exact,
            ["data_mod"],
            "the exact name wins over others holding it"
        );
        let held: Vec<&str> = super::named(&definition, "PBT")
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(held, ["pbt_shift_value_display", "pbt_inner", "pbt_outer"]);
        assert!(super::named(&definition, "no such thing").is_empty());
    }

    #[test]
    fn reads_what_the_operator_types() {
        assert_eq!(typed("  "), Ok(Typed::Nothing));
        assert_eq!(typed("keep FT8"), Ok(Typed::Keep("FT8".to_owned())));
        assert_eq!(
            typed("KEEP  Field Day SSB "),
            Ok(Typed::Keep("Field Day SSB".to_owned()))
        );
        assert_eq!(typed("compare"), Ok(Typed::Compare));
        assert_eq!(typed("help"), Ok(Typed::Help));
        assert_eq!(typed("?"), Ok(Typed::Help));
        assert!(typed("keep").is_err());
        assert!(typed(&format!("keep {}", "x".repeat(41))).is_err());
        assert_eq!(typed("restore FT8"), Ok(Typed::Restore("FT8".to_owned())));
        assert!(typed("restore").is_err());
        assert_eq!(typed(" YES "), Ok(Typed::Yes(None)));
        assert_eq!(typed("yes 12"), Ok(Typed::Yes(Some(12))));
        assert!(typed("yes please").is_err());
        assert_eq!(typed("No"), Ok(Typed::No));
        assert_eq!(typed("quit"), Ok(Typed::Quit));
        assert_eq!(typed("exit"), Ok(Typed::Quit));
        assert!(typed("quit now").is_err());
        assert!(typed("compare FT8").is_err());
        assert_eq!(typed("enroll"), Ok(Typed::Enroll(None)));
        assert_eq!(typed("Enroll ON"), Ok(Typed::Enroll(Some(true))));
        assert_eq!(typed("enroll off"), Ok(Typed::Enroll(Some(false))));
        assert!(typed("enroll maybe").is_err());
    }

    #[test]
    fn shows_how_the_radio_differs_from_each_setup() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let ic_7300 = definitions
            .iter()
            .find(|d| d.model == "IC-7300")
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let port = PretendPort::new(PretendCiv::new(ic_7300), 115_200);
        let mut radio =
            CivRadio::find(port, &definitions).unwrap_or_else(|error| unreachable!("{error}"));
        let now = settings_record::read(
            &mut radio,
            Some("IC-7300_02065962"),
            UNIX_EPOCH + Duration::from_mins(1),
            "FL2",
        );
        let setup = |name: &str, att: &str| KeptReading {
            name: Some(name.to_owned()),
            settings: now
                .settings
                .iter()
                .map(|e| KeptSetting {
                    id: e.id.clone(),
                    label: e.label.clone(),
                    bytes: if e.label == "Attenuator (ATT)" {
                        Some(att.to_owned())
                    } else {
                        e.bytes.clone()
                    },
                })
                .collect(),
            station: "fieldlab-1".to_owned(),
            operator: Some("K9KSU".to_owned()),
            saved_at: "2026-10-27T18:00:00Z".to_owned(),
        };
        assert_eq!(
            compared(&setup("FT8", "00"), &now, ic_7300),
            ["the radio matches setup FT8 (kept 2026-10-27T18:00:00Z at fieldlab-1 by K9KSU)."]
        );
        assert_eq!(
            compared(&setup("SSB", "00 20"), &now, ic_7300),
            [
                "1 differs from setup SSB (kept 2026-10-27T18:00:00Z at fieldlab-1 by K9KSU):",
                "  Attenuator (ATT): the radio has 00; setup SSB has 00 20"
            ]
        );
        let none = KeptRadio {
            as_received: None,
            setups: Vec::new(),
        };
        assert!(compared_all(&none, &now, ic_7300)[0].starts_with("no setups"));
        let two = KeptRadio {
            as_received: None,
            setups: vec![setup("FT8", "00"), setup("SSB", "00 20")],
        };
        assert_eq!(compared_all(&two, &now, ic_7300).len(), 4);
        let plan =
            restore_plan(&now, &two, "ssb", ic_7300).unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(plan.setup, "SSB", "found whatever its case");
        assert!(plan.kept_back.is_empty(), "{plan:?}");
        assert_eq!(
            plan.put.len(),
            1,
            "the attenuator was checked on a real radio"
        );
        assert_eq!(plan.put[0].to, [0x00, 0x20]);
        assert_eq!(
            restore_plan(&now, &two, "RTTY", ic_7300).err().as_deref(),
            Some("no setup RTTY for this radio; it has FT8, SSB")
        );
        assert!(restore_plan(&now, &none, "FT8", ic_7300).is_err());
    }

    #[test]
    fn the_sign_in_command_runs_the_tray_and_fits_windows() {
        let exe = std::path::Path::new(r"C:\LabLog\loglab-agent.exe");
        assert_eq!(
            startup_command(exe, &[]).as_deref(),
            Ok(r#""C:\LabLog\loglab-agent.exe" tray"#)
        );
        let options: Vec<String> = [
            "--engine",
            "http://10.73.10.10:8073",
            "--key-file",
            r"C:\LabLog\FL 1.key",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            startup_command(exe, &options).as_deref(),
            Ok(
                r#""C:\LabLog\loglab-agent.exe" tray --engine http://10.73.10.10:8073 --key-file "C:\LabLog\FL 1.key""#
            )
        );
        assert!(startup_command(exe, &["say \"hi\"".to_owned()]).is_err());
        let deep = std::path::PathBuf::from("x".repeat(300));
        assert!(startup_command(&deep, &[]).is_err(), "over 260 characters");
    }
}
