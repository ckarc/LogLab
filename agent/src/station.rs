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
//!
//! Starting `station` is the operator's choice that programs on this
//! computer may key the radio, as `--allow-transmit` is for `serve`
//! (Principle IX).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auto_mode;
use crate::civ_radio::CivRadio;
use crate::civ_serial::{self, SerialPort};
use crate::engine::{KeptRadio, KeptReading, Uplink};
use crate::rig::{Radio, RadioError};
use crate::rigctld_server::{self, Shared, Source};
use crate::rigdef::{Definition, SettingSummary};
use crate::settings_record::Record;
use crate::setups::Setups;
use crate::{rigdef, settings_record};

/// The radio, found and checked.
struct Found {
    radio: CivRadio<SerialPort>,
    port: String,
    serial: Option<String>,
}

/// Finds the radio on `port`, or, with none given, on whichever serial port
/// looks like a known radio's and answers as one. On Windows, a radio
/// plugged in with no driver for its USB serial bridge gets one first (see
/// `driver`).
fn find(port: Option<&str>, running: &AtomicBool) -> Result<Found, String> {
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
fn wait_for_radio(port: Option<&str>, running: &AtomicBool) -> Option<Found> {
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
fn records_folder() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|folder| folder.join("records")))
        .unwrap_or_else(|| "records".into())
}

/// Reads the radio's settings, keeps the first reading of this radio as its
/// as-received record, and says what changed since and what in them stops
/// digital modes working. Returns the reading.
fn check_settings(radio: &mut CivRadio<SerialPort>, serial: Option<&str>) -> Record {
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
    /// Yes: go ahead with the restore just shown.
    Yes,
    /// Turn auto mode on or off on this computer, or (`None`) say which.
    Auto(Option<bool>),
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
        ("yes", "") => Ok(Typed::Yes),
        ("auto", rest) => match rest.to_ascii_lowercase().as_str() {
            "" => Ok(Typed::Auto(None)),
            "on" => Ok(Typed::Auto(Some(true))),
            "off" => Ok(Typed::Auto(Some(false))),
            _ => Err("auto takes on or off, like auto on".to_owned()),
        },
        ("help" | "?", "") => Ok(Typed::Help),
        _ => Err(format!("unknown command {line:?}")),
    }
}

/// What can be typed at the Agent's console.
const TYPE_HELP: &str = "Type keep NAME to keep the radio's settings as a setup (keep FT8, \
                         say), compare to see what differs from each setup, restore NAME to put \
                         one back, show NAME to read a setting (show DATA MOD, say), fix to \
                         fix what stops a job working, or auto on to let the Agent make its \
                         fixes without asking.";

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

/// Starts reading the operator's commands. Setups are kept on the Engine
/// when there is one, otherwise on this computer, so keep and restore work
/// with no network (Principle V).
fn start_console(
    shared: &Arc<Shared>,
    uplink: Option<Uplink>,
    serial: String,
    definition: Definition,
    record: &Record,
) -> Desk {
    let setups: Box<dyn Setups> = match uplink {
        Some(uplink) => Box::new(uplink),
        None => Box::new(crate::setups::Local::new(records_folder())),
    };
    println!("Setups: kept {}.", setups.place());
    println!("{TYPE_HELP}");
    let desk = Desk {
        waiting: Arc::new(Mutex::new(None)),
        auto: Arc::new(AtomicBool::new(auto_mode::load(&auto_mode::path(
            &records_folder(),
        )))),
        serial,
    };
    if desk.auto.load(Ordering::SeqCst) {
        println!("Auto mode: on, so the Agent makes its fixes without asking (auto off asks).");
    }
    let plan = crate::restore::fixes(record, &definition);
    if !plan.put.is_empty() {
        println!(
            "{}",
            desk.propose(shared, &fix_about(&plan), &fix_ask(&plan), plan)
        );
    }
    let shared = Arc::clone(shared);
    let console_desk = desk.clone();
    std::thread::spawn(move || console(&shared, setups.as_ref(), &definition, &console_desk));
    desk
}

/// What waits for the operator's yes: shown last, taken by the next line
/// they type, whatever it is (Principle X).
type Waiting = Arc<Mutex<Option<crate::restore::Plan>>>;

/// The operator's side of the Agent: what waits for their yes, whether
/// auto mode is on, and the radio's serial number.
#[derive(Clone)]
struct Desk {
    waiting: Waiting,
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
            wait_for_yes(&self.waiting, plan);
            format!("{about}\n{ask}")
        }
    }
}

/// How far each offer lowers the drive level, in percent of its scale.
const DRIVE_STEP: u32 = 10;

/// After a transmission whose ALC ran over a third of its scale, the change
/// that lowers the radio's drive level one step and what it is, or `None`
/// when there is nothing to change.
fn drive_offer(shared: &Shared, definition: &Definition) -> Option<(String, crate::restore::Plan)> {
    let peak = shared.last_peak()?;
    if peak.alc <= rigctld_server::ALC_HIGHEST {
        return None;
    }
    let id = definition.meters.as_ref()?.drive.as_ref()?;
    let setting = definition.settings.iter().find(|s| &s.id == id)?;
    let [low, high] = setting.range?;
    if !(setting.write && setting.write_checked) {
        return None;
    }
    let bytes = shared
        .settings(|radio| radio.read_exposed(setting))
        .ok()?
        .ok()?;
    let level = crate::civ::level_from_bcd(&bytes)?;
    let percent = (u32::from(level) * 100 + u32::from(high) / 2) / u32::from(high.max(1));
    if percent <= u32::from(low) {
        return None;
    }
    let lower = percent.saturating_sub(DRIVE_STEP);
    let to = u16::try_from((lower * u32::from(high) + 50) / 100)
        .ok()?
        .max(u16::from(low));
    let to = crate::civ::level_to_bcd(to)?.to_vec();
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
        "ALC peaked at {}% of its scale: lower {} from {} to {}.",
        peak.alc, setting.label, plan.put[0].now, plan.put[0].setup
    );
    Some((about, plan))
}

/// How to say yes to lowering the drive.
const DRIVE_ASK: &str = "-> Type yes to set it, then transmit again to see the new ALC; \
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
/// offers to lower the drive if ALC ran high.
fn after_unkey<'a>(
    shared: &'a Shared,
    definition: &'a Definition,
    desk: Option<Desk>,
    on_air: &'a AtomicBool,
) -> impl FnMut() + 'a {
    let mut was_keyed = false;
    move || {
        let keyed = on_air.load(Ordering::SeqCst);
        if std::mem::replace(&mut was_keyed, keyed)
            && !keyed
            && let Some(desk) = &desk
            && let Some((about, plan)) = drive_offer(shared, definition)
        {
            crate::print_event(&desk.propose(shared, &about, DRIVE_ASK, plan));
        }
    }
}

/// Leaves `plan` waiting for the operator's yes, in place of anything
/// that was.
fn wait_for_yes(waiting: &Mutex<Option<crate::restore::Plan>>, plan: crate::restore::Plan) {
    if let Ok(mut slot) = waiting.lock() {
        *slot = Some(plan);
    }
}

/// Reads the operator's commands from the console until it closes.
fn console(shared: &Shared, setups: &dyn Setups, definition: &Definition, desk: &Desk) {
    let (serial, waiting) = (desk.serial.as_str(), &desk.waiting);
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
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { return };
        let typed = typed(&line);
        // A restore goes ahead only on a yes typed right after it was shown.
        // Anything else leaves the radio as it is, and is then done as
        // typed.
        let pending = waiting.lock().ok().and_then(|mut slot| slot.take());
        if let Some(plan) = pending {
            if matches!(typed, Ok(Typed::Yes)) {
                crate::print_event(&restore(shared, &plan, serial));
                continue;
            }
            crate::print_event(&if plan.fixes {
                "nothing fixed; the radio is as it was.".to_owned()
            } else {
                format!("setup {} not put back; nothing changed.", plan.setup)
            });
        }
        let said = match typed {
            Ok(Typed::Nothing) => continue,
            Ok(Typed::Yes) => "nothing is waiting for a yes.".to_owned(),
            Ok(Typed::Restore(name)) => {
                match read().and_then(|now| {
                    setups
                        .kept(&now.maker, &now.model, serial)
                        .map(|kept| restore_plan(&now, &kept, &name, definition))
                }) {
                    Ok(Ok(plan)) => {
                        let mut lines = plan.lines();
                        if !plan.put.is_empty() {
                            lines.push(format!(
                            "-> Type yes to put {} back on the radio; anything else leaves it as it is.",
                            if plan.put.len() == 1 { "it" } else { "them" }
                        ));
                            wait_for_yes(waiting, plan);
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
                 while it runs).\n  -> Close VARA, then start the Agent again."
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
    listen: SocketAddr,
    limit: Duration,
    engine: Option<(&str, &str, Option<&str>)>,
) -> Result<(), String> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    let uplink = engine
        .map(|(engine, key_file, station)| uplink(engine, key_file, station, &running))
        .transpose()?;
    let Some(mut found) = wait_for_radio(port, &running) else {
        println!("stopped");
        return Ok(());
    };
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
    let server = rigctld_server::Server::start(listen, Arc::clone(&shared) as Arc<dyn Source>)?;
    say_how_to_connect(&identity.model, server.address(), limit);
    #[cfg(windows)]
    if let Some(station) = &station {
        crate::programs::set_up(station, server.address());
    }
    let desk = found
        .serial
        .clone()
        .map(|serial| start_console(&shared, uplink, serial, definition.clone(), &record));
    let mut offer_drive = after_unkey(&shared, &definition, desk, &on_air);
    println!("Ctrl-C stops.");
    #[cfg(windows)]
    let mut hint: Option<&'static str> = None;
    #[cfg(windows)]
    let mut last = std::time::Instant::now();
    #[cfg(windows)]
    let mut was_on_air = false;
    crate::show_until_stopped(shared.as_ref(), &heard, &running, || {
        offer_drive();
        #[cfg(windows)]
        if let Some(station) = &station {
            for error in station.errors.try_iter() {
                crate::print_event(&format!("sound device error: {error}"));
            }
            // How loud a program's transmit audio was, each time the radio
            // is unkeyed: silence means the program isn't playing into the
            // transmit cable.
            if let (Some(transmit), Some(cable)) = (&station.transmit, &station.transmit_cable) {
                let on_air_now = on_air.load(Ordering::SeqCst);
                if on_air_now && !was_on_air {
                    let _ = transmit.take_peak_dbfs();
                }
                if was_on_air && !on_air_now {
                    crate::print_event(&match transmit.take_peak_dbfs() {
                        Some(db) => format!("transmit audio: {db:.0} dBFS peak"),
                        None => format!(
                            "transmit audio: silent\n-> The program's sound output should be \"{cable}\"."
                        ),
                    });
                }
                was_on_air = on_air_now;
            }
            // The receive level, said only when what to do about it changes.
            if last.elapsed() >= Duration::from_secs(5) {
                last = std::time::Instant::now();
                let now = station.receive_hint();
                if now != hint {
                    crate::print_event(&match now {
                        Some(what) => format!("receive audio: {what}"),
                        None => "receive audio: level good".to_owned(),
                    });
                    hint = now;
                }
            }
        }
    });
    drop(server);
    #[cfg(windows)]
    drop(station);
    println!("stopped");
    Ok(())
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

/// The command line Windows runs at sign-in: this program's `station`.
/// Windows takes at most 260 characters there.
#[cfg(any(windows, test))]
fn startup_command(exe: &std::path::Path) -> Result<String, String> {
    let command = format!("\"{}\" station", exe.display());
    if command.chars().count() > 260 {
        return Err(format!(
            "the Agent's path is too long for Windows to start it at sign-in ({command})"
        ));
    }
    Ok(command)
}

/// `startup on`, `startup off` or `startup status`: whether Windows starts
/// `station` each time this user signs in.
///
/// # Errors
///
/// Windows' registry couldn't be read or changed, or this isn't Windows.
pub fn startup(what: &str) -> Result<(), String> {
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
                let command = startup_command(&exe)?;
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
                Ok(())
            }
            "off" => {
                let done = reg(&["delete", RUN_KEY, "/v", RUN_NAME, "/f"])?;
                if done.status.success() {
                    println!("Windows won't start the Agent at sign-in any more.");
                } else {
                    println!("Windows wasn't starting the Agent at sign-in.");
                }
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
        let _ = what;
        Err(
            "startup is for Windows; elsewhere, use the system's own way to start programs"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        DRIVE_ASK, Desk, Typed, compared, compared_all, drive_offer, restore_plan, startup_command,
        typed,
    };
    use crate::civ_pretend::PretendCiv;
    use crate::civ_radio::{CivRadio, PretendPort};
    use crate::engine::{KeptRadio, KeptReading, KeptSetting};
    use crate::rigctld_server::{Shared, Source};
    use crate::rigdef;
    use crate::settings_record;

    /// The pretend IC-7300, shared for transmitting, after one transmission
    /// whose meters read `meters` (raw Po, SWR, ALC levels).
    fn transmitted(meters: [u16; 3]) -> (Shared, rigdef::Definition) {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let mut pretend = PretendCiv::new(&definition);
        pretend.set_tx_meters(meters);
        let radio = CivRadio::find(
            PretendPort::new(pretend, 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let shared = Shared::with_transmit(Box::new(radio), Duration::from_mins(1));
        let program = SocketAddr::from(([127, 0, 0, 1], 50_001));
        assert!(shared.transmit(program, true).is_ok());
        std::thread::sleep(Duration::from_millis(300));
        shared.watch();
        assert!(shared.transmit(program, false).is_ok());
        (shared, definition)
    }

    #[test]
    fn offers_to_lower_the_drive_only_when_alc_ran_high() {
        // ALC 84 of 120 is 70% of its scale.
        let (shared, definition) = transmitted([60, 20, 84]);
        let Some((say, plan)) = drive_offer(&shared, &definition) else {
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
        let Some((say, _)) = drive_offer(&shared, &definition) else {
            unreachable!("still the same peak")
        };
        assert!(
            say.contains("from 102 of 255 (40%) to 77 of 255 (30%)"),
            "{say}"
        );
        // ALC 40 of 120 is a third: nothing to offer.
        let (shared, definition) = transmitted([60, 20, 40]);
        assert!(drive_offer(&shared, &definition).is_none());
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
            auto: Arc::new(AtomicBool::new(false)),
            serial: "test".to_owned(),
        };
        let Some((about, plan)) = drive_offer(&shared, &definition) else {
            unreachable!("ALC ran high")
        };
        let said = desk.propose(&shared, &about, DRIVE_ASK, plan.clone());
        assert!(said.ends_with(DRIVE_ASK), "asks first: {said}");
        assert!(
            desk.waiting.lock().is_ok_and(|slot| slot.is_some()),
            "and waits for the yes"
        );
        let unchanged = drive_offer(&shared, &definition).map(|(about, _)| about);
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
        let next = drive_offer(&shared, &definition).map(|(about, _)| about);
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
        assert_eq!(typed(" YES "), Ok(Typed::Yes));
        assert!(typed("yes please").is_err());
        assert!(typed("compare FT8").is_err());
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
    fn the_sign_in_command_runs_station_and_fits_windows() {
        let exe = std::path::Path::new(r"C:\LabLog\loglab-agent.exe");
        assert_eq!(
            startup_command(exe).as_deref(),
            Ok(r#""C:\LabLog\loglab-agent.exe" station"#)
        );
        let deep = std::path::PathBuf::from("x".repeat(300));
        assert!(startup_command(&deep).is_err(), "over 260 characters");
    }
}
