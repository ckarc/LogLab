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
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use crate::auto_mode;
use crate::civ_serial;
use crate::engine::{KeptRadio, KeptReading, Uplink};
use crate::engine_link::EngineLink;
use crate::rig::{RadioIdentity, Reading, Tuning};
use crate::rigctld_server::{self, Shared, Source};
use crate::rigdef::{Definition, SettingSummary};
use crate::serial_radio::{self, SerialRadio};
use crate::settings_record::Record;
use crate::setups::Setups;
use crate::{enrollment, profiles, program_id, rigdef, settings_record};

/// The radio, found and checked.
struct Found {
    radio: Box<dyn SerialRadio>,
    port: String,
    serial: Option<String>,
    /// For a FlexRadio: where its API listens and the slice the Agent
    /// follows, for its audio over the network.
    network_audio: Option<(SocketAddr, u8)>,
    /// The programs' port, held open while the "Use FlexRadio" checkmark
    /// waited for the radio, so a program that opened first waits for it
    /// rather than being refused; the server answers on it.
    held: Option<std::net::TcpListener>,
}

/// Finds the radio on `port`, or, with none given, on whichever serial port
/// looks like a known radio's and answers as one. On Windows, a radio
/// plugged in with no driver for its USB serial bridge gets one first (see
/// `driver`).
fn find(port: Option<&str>, running: &AtomicBool) -> Result<Found, String> {
    let definitions = rigdef::built_in()?;
    if let Some(port) = port {
        let radio = serial_radio::find(port, serial_radio::usb_ids(port), &definitions)?;
        return Ok(Found {
            radio,
            port: port.to_owned(),
            serial: None,
            network_audio: None,
            held: None,
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
        let answered = serial_radio::find(&candidate.port, Some(candidate.ids), &definitions);
        match answered {
            Ok(radio) => {
                return Ok(Found {
                    radio,
                    port: candidate.port,
                    serial: candidate.serial,
                    network_audio: None,
                    held: None,
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

/// What `--flex-pretend` asks `find_flex` for.
pub const PRETEND_FLEX: &str = "pretend";

/// How long the station listens for FlexRadio radios' discovery
/// broadcasts: each sends one every second.
const FLEX_LISTEN: Duration = Duration::from_secs(3);

/// Finds the FlexRadio at `address` on the network, or, with `any`, the one
/// FlexRadio that broadcasts on this network.
fn find_flex(address: &str) -> Result<Found, String> {
    // The pretend FLEX-6600, for trying the station without a radio.
    let pretend;
    let address = if address == PRETEND_FLEX {
        pretend = crate::flex_pretend::PretendFlex::start("FLEX-6600", true)
            .map_err(|error| format!("can't start the pretend FlexRadio: {error}"))?;
        pretend.address()
    } else {
        address
    };
    let address = flex_address(address)?;
    let mut radio = crate::flex::FlexRadio::open(&address).map_err(|error| error.to_string())?;
    // No slice yet: SmartSDR on this computer isn't open, or has none. The
    // station waits for one rather than serving a radio it can't tune.
    crate::rig::Radio::read(&mut radio).map_err(|error| {
        format!(
            "{error}.\n-> Open SmartSDR on this computer, with a slice (flex stop goes back to \
             USB radios)."
        )
    })?;
    let serial = radio.serial().map(str::to_owned);
    let network_audio = radio.slice_index().map(|slice| (radio.address(), slice));
    Ok(Found {
        radio: Box::new(radio),
        port: format!("the network at {address}"),
        serial,
        network_audio,
        held: None,
    })
}

/// Where the FlexRadio `wanted` is: `any`, the one FlexRadio heard on the
/// network; a serial number, the radio with it; otherwise `wanted` itself,
/// an address.
///
/// # Errors
///
/// With `any`, none or more than one FlexRadio is heard; or the discovery
/// port can't be opened.
pub fn flex_address(wanted: &str) -> Result<String, String> {
    if wanted != "any" {
        return Ok(by_serial(wanted)?.unwrap_or_else(|| wanted.to_owned()));
    }
    let heard = crate::flex::discover(FLEX_LISTEN)?;
    match heard.as_slice() {
        [] => Err(format!(
            "no FlexRadio said it is on this network in {} s.\n\
             -> Is it on, and on the same network as this computer? Or give its \
             address: --flex 192.168.1.20",
            FLEX_LISTEN.as_secs()
        )),
        [one] => Ok(one.address.to_string()),
        many => {
            let list: Vec<String> = many
                .iter()
                .map(|r| format!("{} {} at {}", r.model, r.serial, r.address.ip()))
                .collect();
            Err(format!(
                "more than one FlexRadio is on this network: {}.\n\
                 -> Give the address of the one to use with --flex.",
                list.join(", ")
            ))
        }
    }
}

/// A FlexRadio's serial number, as `flex use` picks it, looked for in
/// the radios' discovery broadcasts: the address of the one with it, or
/// `None` when `wanted` looks like an address (it has a dot or colon) or no
/// radio heard has it.
fn by_serial(wanted: &str) -> Result<Option<String>, String> {
    if wanted.contains(['.', ':']) {
        return Ok(None);
    }
    let heard = crate::flex::discover(FLEX_LISTEN)?;
    Ok(heard
        .iter()
        .find(|radio| radio.serial == wanted)
        .map(|radio| radio.address.to_string()))
}

/// Which radio the operator asked the station for: the one on a serial
/// port (`--civ`), a FlexRadio on the network (`--flex`), or, with neither,
/// whichever USB radio is plugged in.
#[derive(Clone, Copy, Debug, Default)]
pub struct Wanted<'a> {
    /// The serial port given with `--civ`.
    pub port: Option<&'a str>,
    /// The FlexRadio's address given with `--flex`, or `any`.
    pub flex: Option<&'a str>,
}

/// How often the radio is looked for again while it isn't there.
const LOOK_AGAIN: Duration = Duration::from_secs(5);

/// How often, with the "Use FlexRadio" checkmark on, the Agent looks again
/// for a radio program opening or SmartSDR's slice: soon enough that a
/// program kept waiting on the held port (see `rigctld_server::hold`) gets
/// its answer within Hamlib's 10 s.
const FOOTWORK_LOOK_AGAIN: Duration = Duration::from_secs(1);

/// Looks for the radio until it is found or `running` turns false, so the
/// Agent can start before the radio is turned on (at Windows sign-in, for
/// example). Says why it isn't found once, and again only when the reason
/// changes.
fn wait_for_radio(
    port: Option<&str>,
    pick: &crate::flex_pick::Pick,
    running: &AtomicBool,
    listen: SocketAddr,
) -> Option<Found> {
    let mut said = String::new();
    // With the checkmark on, the programs' port is held open while the
    // FlexRadio is fetched, so a program that opens first waits for it.
    let mut held: Option<std::net::TcpListener> = None;
    while running.load(Ordering::SeqCst) {
        // A pick made while waiting is acted on at once.
        pick.take_changed();
        let tried = if pick.checked() {
            if held.is_none() {
                held = rigctld_server::hold(listen).ok();
            }
            flex_footwork(pick).map(|mut found| {
                found.held = held.take();
                found
            })
        } else {
            held = None;
            pick.current()
                .as_deref()
                .map_or_else(|| find(port, running), find_flex)
        };
        match tried {
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
        // With the checkmark on, a radio program opening is seen sooner.
        let again = if pick.checked() {
            FOOTWORK_LOOK_AGAIN
        } else {
            LOOK_AGAIN
        };
        let until = std::time::Instant::now() + again;
        while running.load(Ordering::SeqCst)
            && !pick.has_changed()
            && std::time::Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

/// The footwork the "Use FlexRadio" checkmark asks for, one look: once a
/// radio program is open, takes the one free FlexRadio on the network (or
/// asks which, once, when there are several), opens SmartSDR for it if it
/// isn't open, and finds the radio once SmartSDR has its slice.
fn flex_footwork(pick: &crate::flex_pick::Pick) -> Result<Found, String> {
    let programs = radio_programs_open();
    let open_smartsdr = pick.open_smartsdr_now(!programs.is_empty());
    if pick.current().is_none() {
        if programs.is_empty() {
            return Err(
                "Use FlexRadio is on; the Agent takes the FlexRadio when a radio program \
                 opens (WSJT-X, JS8Call, fldigi, VARA, MMSSTV, Winlink Express).\n-> Open \
                 one, or turn Use FlexRadio off for USB radios."
                    .to_owned(),
            );
        }
        let heard = crate::flex::discover(FLEX_LISTEN)?;
        pick.heard(&heard);
        let free = crate::flex_pick::free(&heard, crate::flex::here_toward);
        match free.as_slice() {
            [] if heard.is_empty() => {
                return Err(format!(
                    "Use FlexRadio is on, but no FlexRadio said it is on this network in {} \
                     s.\n-> Is it on, and on this computer's network?",
                    FLEX_LISTEN.as_secs()
                ));
            }
            [] => {
                return Err(
                    "Use FlexRadio is on, but every FlexRadio on the network is in use by \
                     another computer.\n-> The Agent takes one once it is free."
                        .to_owned(),
                );
            }
            [one] => {
                crate::print_event(&format!(
                    "FlexRadio: Use FlexRadio is on and {} is open, so the Agent uses {}.",
                    programs.join(", "),
                    crate::flex_pick::described(one)
                ));
                pick.chose(&one.serial);
            }
            many => {
                let serials: Vec<String> = many.iter().map(|radio| radio.serial.clone()).collect();
                if pick.ask_which(&serials) {
                    crate::print_event(&format!(
                        "{}\n{}",
                        crate::flex_pick::WHICH,
                        crate::flex_pick::offered(&heard, crate::flex::here_toward)
                    ));
                }
                return Err(
                    "Use FlexRadio is on, and more than one FlexRadio is free.\n-> Pick \
                     one (flex use SERIAL)."
                        .to_owned(),
                );
            }
        }
    }
    let flex = pick.current().unwrap_or_default();
    let found = find_flex(&flex);
    if found.is_err() && open_smartsdr {
        match crate::smartsdr::open() {
            Ok(Some(exe)) => crate::print_event(&format!(
                "SmartSDR: opened it for {} ({exe}); the Agent takes the FlexRadio once \
                 SmartSDR has its slice.",
                programs.join(", ")
            )),
            Ok(None) => {}
            Err(why) => crate::print_event(&format!("SmartSDR: {why}")),
        }
    }
    found
}

/// The radio programs the Agent sets up that are open now, by the names
/// they are shown by.
fn radio_programs_open() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        let mut open: Vec<&'static str> = open_programs(&tasklist()).into_iter().collect();
        open.sort_unstable();
        open
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
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
fn check_settings(radio: &mut dyn SerialRadio, serial: Option<&str>) -> Record {
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
    /// Show what putting the model's Lab Stock on would change, and wait
    /// for a yes.
    Stock,
    /// Send the radio's settings to the Engine as a Lab Stock proposal.
    StockSave,
    /// Standing alone: show what saving the radio's settings as this
    /// program's lab profile would keep, and wait for a yes.
    ProfileSave(&'static str),
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
        ("stock", rest) => match rest.to_ascii_lowercase().as_str() {
            "" => Ok(Typed::Stock),
            "save" => Ok(Typed::StockSave),
            _ => Err("stock takes save or nothing: stock, or stock save".to_owned()),
        },
        ("profile", rest) => {
            let (save, program) = rest
                .split_once(char::is_whitespace)
                .map_or((rest, ""), |(save, program)| (save, program.trim()));
            match (
                save.eq_ignore_ascii_case("save"),
                profiles::program_named(program),
            ) {
                (true, Some(program)) => Ok(Typed::ProfileSave(program)),
                _ => Err(format!(
                    "profile takes save and a program, one of {}: profile save VARA, say",
                    profiles::PROGRAMS.join(", ")
                )),
            }
        }
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
                         radio as programs start, stock to put the lab's Lab Stock on, stock \
                         save to send the radio's settings as a Lab Stock proposal (standing \
                         alone, to save them as the Lab Stock), profile save VARA (say) to save \
                         the radio's settings as a lab profile when standing alone, flex to \
                         list the FlexRadios on the network (flex use SERIAL to use one, flex \
                         stop to go back to USB radios), or quit to stop the Agent.";

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
/// one by that name for this radio's model (saved in LabLog, as the copy in
/// `records` has it, or built in): `restore VARA` puts the lab's VARA
/// profile on, after a yes.
fn lab_plan(
    records: &std::path::Path,
    now: &Record,
    name: &str,
    definition: &Definition,
) -> Option<crate::restore::Plan> {
    let profile = profiles::current(records, definition, name)?;
    Some(crate::restore::plan(
        &format!("{} lab profile", profile.name),
        profile.kept(definition),
        now,
        definition,
    ))
}

/// Why `keep NAME` is refused, when NAME is one of the lab's profiles for
/// this radio's model: those are locked, and only an administrator changes
/// them, in LabLog (brian's decisions, 2026-10-28 and 2026-10-30).
fn lab_locked(records: &std::path::Path, definition: &Definition, name: &str) -> Option<String> {
    let profile = profiles::current(records, definition, name)?;
    Some(format!(
        "setup {name} not kept: {} is the lab's locked profile for the {}, and only an \
         administrator changes it, in LabLog (Menu, Admin, Lab Profiles: Read this radio, then \
         Update the default).\n-> Keep your own under another name, like keep My {}; restore \
         My {} puts it back.",
        profile.name, definition.model, profile.name, profile.name
    ))
}

/// What `keep NAME` says: the radio's settings kept as setup NAME, or why
/// not. The lab's profiles are locked: NAME can't be one of them.
fn keep(
    setups: &dyn Setups,
    definition: &Definition,
    name: &str,
    read: impl FnOnce() -> Result<Record, String>,
) -> String {
    if let Some(said) = lab_locked(&records_folder(), definition, name) {
        return if setups.standalone() {
            standalone_locked(&said, name)
        } else {
            said
        };
    }
    match read() {
        Ok(record) => match setups.keep(&record, name) {
            Ok(()) => format!(
                "kept the radio's settings {} as setup {name} ({} settings)",
                setups.place(),
                record.settings.len()
            ),
            Err(error) => format!("setup {name} not kept: {error}"),
        },
        Err(why) => format!("setup {name} not kept: {why}"),
    }
}

/// Why `keep NAME` is refused on an Agent standing alone: `said`, LabLog's
/// way of changing a lab profile swapped for `profile save NAME`.
fn standalone_locked(said: &str, name: &str) -> String {
    let first = said
        .split_once(", and only")
        .map_or(said, |(first, _)| first);
    let program = profiles::program_named(name).unwrap_or(name);
    format!(
        "{first}.\n-> To change it, set the radio up and type profile save {program}; to keep \
         your own beside it, keep My {program}."
    )
}

/// What `profile save PROGRAM` says, standing alone: what the program's
/// lab profile would hold, as the radio has it now, left waiting for a
/// yes, or why it can't.
fn profile_save(
    setups: &dyn Setups,
    desk: &Desk,
    definition: &Definition,
    records: &std::path::Path,
    program: &'static str,
    read: impl FnOnce() -> Result<Record, String>,
) -> String {
    if !setups.standalone() {
        return format!(
            "Lab profile: the {program} profile not saved. At the lab, an administrator \
             changes the lab's profiles in LabLog (Menu, Admin, Lab Profiles); profile save \
             is for an Agent standing alone (--standalone)."
        );
    }
    let record = match read() {
        Ok(record) => record,
        Err(why) => return format!("Lab profile: the {program} profile not saved: {why}"),
    };
    let settings = profiles::chosen(
        records,
        definition,
        program,
        crate::lab_stock::checked(&record, definition),
    );
    if settings.is_empty() {
        return format!(
            "Lab profile: the {program} profile not saved; none of the {}'s settings it would \
             hold has been checked on a real radio yet, so it would put nothing on.",
            record.model
        );
    }
    let now_from = profiles::current(records, definition, program)
        .map_or_else(|| "none yet".to_owned(), |profile| profile.origin());
    let mut lines = vec![format!(
        "Lab profile: saving the {program} profile for the {} ({now_from} now) keeps these {} \
         settings as this radio has them. Every enrolled {} plugged in here gets it as {program}'s \
         program starts. Nothing on the radio changes.",
        record.model,
        settings.len(),
        record.model
    )];
    lines.extend(settings.iter().map(|setting| {
        format!(
            "  {}: {}",
            setting.label,
            setting.shown.as_deref().unwrap_or(&setting.bytes)
        )
    }));
    let number = desk.ask(Asked::ProfileSave {
        program,
        record: Box::new(record),
        settings,
    });
    lines.push(numbered(
        &format!("-> Type yes to save it as the {program} profile; anything else changes nothing."),
        number,
    ));
    lines.join("\n")
}

/// Saves `settings`, read from the radio in `record`, as `program`'s lab
/// profile on this computer, after the operator's yes.
fn profile_saved(
    definition: &Definition,
    records: &std::path::Path,
    program: &str,
    record: &Record,
    settings: Vec<crate::engine::StockSetting>,
) -> String {
    match profiles::save_here(
        records,
        definition,
        program,
        settings,
        &record.computer,
        &record.read_utc,
    ) {
        Ok(id) => format!(
            "Lab profile: saved the {program} profile for the {} on this computer, version #{id}. \
             It goes on as {program}'s program starts from now on (restore {program} puts it on \
             now).",
            record.model
        ),
        Err(why) => format!("Lab profile: the {program} profile not saved: {why}"),
    }
}

/// What `stock` says: what putting the model's Lab Stock on would change,
/// left waiting for a yes, or why it can't.
fn stock(
    setups: &dyn Setups,
    definition: &Definition,
    desk: &Desk,
    now: Result<Record, String>,
) -> String {
    match now.and_then(|now| {
        setups
            .lab_stock(&now.maker, &now.model)
            .map(|stock| (now, stock))
    }) {
        Ok((now, Some(stock))) => {
            stock_offer(desk, crate::lab_stock::plan(&stock, &now, definition))
        }
        Ok((now, None)) if setups.standalone() => format!(
            "Lab Stock: none saved on this computer for the {} yet, so nothing changed.\n-> \
             stock save saves this radio's settings as the {} Lab Stock.",
            now.model, now.model
        ),
        Ok((now, None)) => format!(
            "Lab Stock: none approved for the {} yet, so nothing changed.\n-> stock save \
             sends this radio's settings as a proposal; an administrator approves it in \
             LabLog (Admin, Lab Stock).",
            now.model
        ),
        Err(why) => format!("Lab Stock not put on: {why}"),
    }
}

/// What `stock save` says: what sending the radio's settings as a Lab
/// Stock proposal takes, left waiting for a yes, since reading each
/// filter's width means choosing each filter in turn.
fn stock_save(
    desk: &Desk,
    definition: &Definition,
    standalone: bool,
    now: Result<Record, String>,
) -> String {
    let (sending, send) = if standalone {
        ("saving this radio's", "save them as the Lab Stock")
    } else {
        ("sending this radio's", "send the proposal")
    };
    let record = match now {
        Ok(record) => record,
        Err(why) => return format!("Lab Stock proposal not sent: {why}"),
    };
    let settings = crate::lab_stock::checked(&record, definition);
    if settings.is_empty() {
        return format!(
            "Lab Stock: nothing sent; none of the {}'s settings has been checked on a real \
             radio yet, so a Lab Stock would put nothing on.",
            record.model
        );
    }
    let number = desk.ask(Asked::StockSave);
    let about = if crate::filter_widths::can_choose(definition) {
        format!(
            "Lab Stock: {sending} {} settings{} also reads each filter's width. The Agent \
             chooses FIL1, FIL2 and FIL3 in turn on the mode the radio is on, then puts it back \
             on the filter it is on now. Nothing else changes.",
            settings.len(),
            if standalone { "" } else { " as a proposal" }
        )
    } else {
        format!(
            "Lab Stock: the Agent reads this radio's {} settings and {}; it can't choose each \
             filter on the {}, so filter widths other than the selected one aren't in it. \
             Nothing on the radio changes.",
            settings.len(),
            if standalone {
                "saves them as the Lab Stock"
            } else {
                "sends them as a proposal"
            },
            record.model
        )
    };
    let lines = [
        about,
        numbered(
            &format!("-> Type yes to read them and {send}; anything else changes nothing."),
            number,
        ),
    ];
    lines.join("\n")
}

/// Reads the radio's settings and each filter's width and sends them to
/// the Engine as a Lab Stock proposal, after the operator's yes. It changes
/// nothing until an administrator approves it.
fn stock_send(
    shared: &Shared,
    setups: &dyn Setups,
    definition: &Definition,
    serial: &str,
) -> String {
    let read = shared
        .settings(|radio| {
            let record = settings_record::read(
                radio,
                Some(serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            );
            let widths = if crate::filter_widths::can_choose(radio.definition()) {
                crate::filter_widths::read(radio)?
            } else {
                Vec::new()
            };
            Ok::<_, String>((record, widths))
        })
        .and_then(|read| read);
    let (record, widths) = match read {
        Ok(read) => read,
        Err(why) => return format!("Lab Stock proposal not sent: {why}"),
    };
    let settings = crate::lab_stock::from_record(&record, definition, &widths);
    let filters = if widths.is_empty() {
        ""
    } else {
        ", with FIL1, FIL2 and FIL3's widths (the radio is back on the filter it was on),"
    };
    if setups.standalone() {
        return match setups.propose_lab_stock(&record, &settings) {
            Ok(number) => format!(
                "Lab Stock: saved the radio's {} settings{filters} {} as the {}'s Lab Stock \
                 #{number}. stock puts it on any {} plugged in here, after a yes.",
                settings.len(),
                setups.place(),
                record.model,
                record.model
            ),
            Err(why) => format!("Lab Stock not saved: {why}"),
        };
    }
    match setups.propose_lab_stock(&record, &settings) {
        Ok(number) => format!(
            "Lab Stock: sent the radio's {} settings{} to the Engine as proposal #{number} for \
             the {}. It changes nothing until an administrator approves it in LabLog (Admin, \
             Lab Stock).",
            settings.len(),
            if widths.is_empty() {
                ""
            } else {
                ", with FIL1, FIL2 and FIL3's widths (the radio is back on the filter it was on),"
            },
            record.model
        ),
        Err(why) => format!("Lab Stock proposal not sent: {why}"),
    }
}

/// Shows what putting the Lab Stock on would change and waits for the
/// operator's yes: always a yes, even in auto mode (Principle X, first
/// exception).
fn stock_offer(desk: &Desk, plan: crate::restore::Plan) -> String {
    let mut lines = plan.lines();
    let count = plan.put.len() + plan.filters.len();
    if count == 0 {
        return lines.join("\n");
    }
    let ask = format!(
        "-> Type yes to put {count} Lab Stock setting{} on the radio; anything else leaves it as it is.",
        if count == 1 { "" } else { "s" }
    );
    let number = desk.wait_for_yes(plan);
    lines.push(numbered(&ask, number));
    lines.join("\n")
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
fn start_typing(running: Arc<AtomicBool>, pick: Arc<crate::flex_pick::Pick>) -> Typing {
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
            // Picking a FlexRadio works with or without a radio found.
            if let Some(said) = flex_typed(&line, &pick) {
                crate::print_event(&said);
                continue;
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

/// What a `flex` line asks, answered: `flex` lists the `FlexRadio` radios on the
/// network, `flex on` is the tray's "Use FlexRadio" checkmark, `flex use
/// SERIAL` picks one, `flex off` and `flex stop` go back to USB radios.
/// `None` for any other line.
fn flex_typed(line: &str, pick: &crate::flex_pick::Pick) -> Option<String> {
    let mut words = line.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("flex") {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    Some(match rest.as_slice() {
        [] => match crate::flex::discover(FLEX_LISTEN) {
            Ok(heard) => {
                pick.heard(&heard);
                crate::flex_pick::offered(&heard, crate::flex::here_toward)
            }
            Err(why) => format!("FlexRadio: {why}"),
        },
        [word, serial] if word.eq_ignore_ascii_case("use") => {
            pick.use_serial(serial, crate::flex::here_toward)
        }
        [word] if word.eq_ignore_ascii_case("on") => pick.on(),
        [word] if word.eq_ignore_ascii_case("stop") || word.eq_ignore_ascii_case("off") => {
            pick.stop()
        }
        _ => "flex takes nothing, on, off, use SERIAL, or stop: flex lists the FlexRadios \
              on the network, flex on uses the free one once a radio program opens (opening \
              SmartSDR for it), flex use SERIAL uses one, flex off or flex stop goes back to \
              USB radios."
            .to_owned(),
    })
}

/// Where setups are kept: on the Engine when there is one, otherwise on
/// this computer, so keep and restore work with no network (Principle V);
/// standing alone, Lab Stock too.
fn setups_for(uplink: Option<Uplink>, standalone: bool) -> Box<dyn Setups> {
    match uplink {
        Some(uplink) => Box::new(uplink),
        None if standalone => Box::new(crate::setups::Local::standalone(records_folder())),
        None => Box::new(crate::setups::Local::new(records_folder())),
    }
}

/// Starts the operator's console, with its setups kept in `setups`.
fn start_console(
    shared: &Arc<Shared>,
    setups: Box<dyn Setups>,
    serial: String,
    checks: &Checks,
    record: &Record,
    typing: &Typing,
) -> Desk {
    let definition = now_in(checks);
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
    if setups.standalone()
        && crate::write_check::unchecked(&definition)
        && crate::write_check::worked_here(&records_folder(), &definition.maker, &definition.model)
            .is_none()
    {
        // Nothing of this model is checked yet, so nothing could be fixed:
        // the check comes first, and the other questions after it.
        let number = desk.ask(Asked::CheckWrites {
            record: Box::new(record.clone()),
        });
        println!(
            "{}\n{}",
            check_about(&definition),
            numbered(CHECK_ASK, number)
        );
    } else {
        for line in first_offers(shared, &desk, &definition, record) {
            println!("{line}");
        }
    }
    let (lines, typed_lines) = mpsc::channel();
    if let Ok(mut slot) = typing.to_console.lock() {
        *slot = Some(lines);
    }
    let shared = Arc::clone(shared);
    let console_desk = desk.clone();
    let checks = Arc::clone(checks);
    std::thread::spawn(move || {
        console(
            &shared,
            setups.as_ref(),
            &checks,
            &console_desk,
            &typed_lines,
        );
    });
    desk
}

/// What the Agent offers as it starts with a radio: to enroll it, on a
/// model the lab has profiles for and a radio not yet asked about, or
/// else to fix what stops digital modes working. Returns the lines to show.
fn first_offers(
    shared: &Shared,
    desk: &Desk,
    definition: &Definition,
    record: &Record,
) -> Vec<String> {
    let mut lines = Vec::new();
    let enrolled = enrollment::load(&enrollment::path(&records_folder(), &desk.serial));
    let lab_model = profiles::any_here(&records_folder(), definition);
    let plan = crate::restore::fixes(record, definition);
    if lab_model && enrolled.is_none() {
        // One question waits at a time, so the fix offer waits until fix
        // is typed; enrolled, the lab's profile sets the rest anyway.
        let number = desk.ask(Asked::Enroll);
        lines.push(format!(
            "{}\n{}",
            enroll_about(&record.model, &desk.serial),
            numbered(ENROLL_ASK, number)
        ));
    } else {
        if lab_model {
            lines.push(enrollment::said(enrolled));
        }
        if !plan.put.is_empty() {
            lines.push(desk.propose(shared, &fix_about(&plan), &fix_ask(&plan), plan));
        }
    }
    lines
}

/// How to answer the first-time write check.
const CHECK_ASK: &str = "-> Type yes to check now (a minute or so); no leaves every setting                          alone, and it asks again next time it starts.";

/// What the question to check a model's settings says first.
fn check_about(definition: &Definition) -> String {
    let count = definition.settings.iter().filter(|s| s.write).count();
    format!(
        "Write check: {} hasn't checked which of the {}'s settings it can set, so it sets none          of them yet. Check now? Nothing changes: it writes each of {count} settings' own value          back to it and reads it again. The ones that work count as checked on this computer,          so the fixes digital modes need can go on after your yes.",
        crate::product::current().name,
        definition.model
    )
}

/// Runs the write check on the radio the station holds, one setting at a
/// time so programs still reach it between them, keeps the report, and
/// counts the settings that worked as checked in `checks`. Returns the
/// lines to show. The report is kept in `records`.
fn check_here(
    shared: &Shared,
    checks: &Checks,
    serial: &str,
    records: &std::path::Path,
) -> Vec<String> {
    let definition = now_in(checks);
    crate::print_event(&format!(
        "Write check: writing back {} settings' own values...",
        definition.settings.iter().filter(|s| s.write).count()
    ));
    let report = crate::write_check::check_with(
        &definition,
        |setting| {
            shared
                .settings(|radio| crate::write_check::write_back_exposed(radio, setting))
                .map_err(crate::rig::RadioError)
                .and_then(|back| back)
        },
        Some(serial),
        std::time::SystemTime::now(),
        &settings_record::computer_name(),
        |_, _| {},
    );
    let mut lines = report.lines();
    let path = crate::write_check::path_for(records, &report);
    match settings_record::save(&path, &report) {
        Ok(()) => {
            lines.push(format!("Recorded in {}.", path.display()));
            let checked = crate::write_check::with_checks_here(&definition, records);
            if let Ok(mut now) = checks.write() {
                *now = checked;
            }
        }
        // Unkept, the check isn't evidence, so nothing counts as checked.
        Err(error) => lines.push(format!("Write check not kept, so nothing counts: {error}")),
    }
    lines
}

/// What a question asks the operator.
enum Asked {
    /// To make this change to the radio.
    Change(crate::restore::Plan),
    /// To check which of the radio's settings the Agent can set, writing
    /// each one's own value back, on a model none of whose settings is
    /// checked; then to ask what would have been asked, from `record`.
    CheckWrites { record: Box<Record> },
    /// Whether the radio is one of the lab's, to enroll in lab settings
    /// control.
    Enroll,
    /// To read each filter's width and send the radio's settings as a Lab
    /// Stock proposal.
    StockSave,
    /// Standing alone, to save these settings, as the radio of this record
    /// has them, as this program's lab profile on this computer.
    ProfileSave {
        program: &'static str,
        record: Box<Record>,
        settings: Vec<crate::engine::StockSetting>,
    },
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
    if crate::product::current().standalone {
        return format!(
            "Lab profile: should RadioAgent set up this {model} (serial {serial})? Enrolled, it \
             gets this computer's profile for each program put on as that program starts (VARA \
             for VarAC, say). Left out (no), such as a borrowed radio, RadioAgent never changes \
             it on its own; the tray's Lab radio menu changes that later."
        );
    }
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
    // Without ALC on its scale there is nothing to go by.
    let alc = peak.alc?;
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
    let (target, why) = if alc > rigctld_server::ALC_HIGHEST {
        tried.too_high = Some(now);
        let step = now.saturating_sub(DRIVE_STEP);
        let target = match tried.too_low {
            Some(floor) if step <= floor && floor < now => (floor + now).div_ceil(2),
            _ => step,
        };
        (target, format!("ALC peaked at {alc}% of its scale"))
    } else if let Some(set) = set_power
        && alc <= ALC_LOW
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
                "Po reached {}% with the radio set to {set}% and ALC at {alc}%",
                peak.power
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
        filters: Vec::new(),
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
fn answer_waiting(
    shared: &Shared,
    desk: &Desk,
    typed: &Result<Typed, String>,
    setups: &dyn Setups,
    checks: &Checks,
) -> bool {
    let definition = &now_in(checks);
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
        (Asked::CheckWrites { record }, true) => {
            let mut lines = check_here(shared, checks, &desk.serial, &records_folder());
            lines.extend(first_offers(shared, desk, &now_in(checks), &record));
            lines.join("\n")
        }
        (Asked::CheckWrites { record }, false) => {
            let mut lines = vec![format!(
                "Write check: not run, so {} sets none of the {}'s settings.",
                crate::product::current().name,
                record.model
            )];
            lines.extend(first_offers(shared, desk, definition, &record));
            lines.join("\n")
        }
        (Asked::Change(plan), true) => restore(shared, &plan, &desk.serial),
        (Asked::Change(plan), false) if plan.fixes => {
            "nothing fixed; the radio is as it was.".to_owned()
        }
        (Asked::Change(plan), false) => {
            format!("setup {} not put back; nothing changed.", plan.setup)
        }
        (Asked::StockSave, true) => stock_send(shared, setups, definition, &desk.serial),
        (Asked::StockSave, false) if setups.standalone() => {
            "Lab Stock not saved; nothing changed.".to_owned()
        }
        (Asked::StockSave, false) => "Lab Stock proposal not sent; nothing changed.".to_owned(),
        (
            Asked::ProfileSave {
                program,
                record,
                settings,
            },
            true,
        ) => profile_saved(definition, &records_folder(), program, &record, settings),
        (Asked::ProfileSave { program, .. }, false) => {
            format!("Lab profile: the {program} profile not saved; it is as it was.")
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
    checks: &Checks,
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
        if answer_waiting(shared, desk, &typed, setups, checks) {
            continue;
        }
        let definition = &now_in(checks);
        let said = match typed {
            // Quit is read, and acted on, where the operator's typing is.
            Ok(Typed::Nothing | Typed::Quit) => continue,
            Ok(Typed::Yes(_) | Typed::No) => "nothing is waiting for a yes.".to_owned(),
            Ok(Typed::Restore(name)) => {
                match read().and_then(|now| {
                    match lab_plan(&records_folder(), &now, &name, definition) {
                        Some(plan) => Ok(Ok(plan)),
                        None => setups
                            .kept(&now.maker, &now.model, serial)
                            .map(|kept| restore_plan(&now, &kept, &name, definition)),
                    }
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
            Ok(Typed::Stock) => stock(setups, definition, desk, read()),
            Ok(Typed::StockSave) => stock_save(desk, definition, setups.standalone(), read()),
            Ok(Typed::ProfileSave(p)) => {
                profile_save(setups, desk, definition, &records_folder(), p, read)
            }
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
            Ok(Typed::Keep(name)) => keep(setups, definition, &name, read),
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
    carried(Station::start(sound, on_air))
}

/// Says how the audio is carried, or why it isn't, and points VARA at it.
#[cfg(windows)]
fn carried(
    started: Result<crate::audio_windows::Station, String>,
) -> Option<crate::audio_windows::Station> {
    let station = match started {
        Ok(station) => station,
        Err(why) => {
            println!("Audio: not carried ({why})");
            let (_, playing) = crate::audio_windows::device_names();
            if !playing
                .iter()
                .any(|name| name == crate::audio::RECEIVE_CABLE)
            {
                match crate::vbcable::install() {
                    Ok(said) | Err(said) => println!("{said}"),
                }
            }
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
        (None, why) if station.radio_playing.is_some() => println!(
            "Audio: transmit audio doesn't go through the Agent: {}.\n  -> Until it does, \
             a program's sound output must go straight to the radio's USB sound device.",
            why.as_deref().unwrap_or("no transmit cable")
        ),
        (None, why) => println!(
            "Audio: the radio can't get a program's transmit audio: {}.\n  -> A radio on \
             the network has no sound device here, so programs need the second virtual \
             cable to transmit through; programs' sound outputs are left as they are.",
            why.as_deref().unwrap_or("no transmit cable")
        ),
    }
    vara(&station);
    Some(station)
}

/// A FlexRadio's audio, carried by the Agent over the network (see
/// `flex_audio`), into the same cables a USB radio's goes through, so
/// programs need no change between the two.
#[cfg(windows)]
fn flex_audio(
    radio: SocketAddr,
    slice: u8,
    on_air: Arc<AtomicBool>,
) -> (
    Option<crate::audio_windows::Station>,
    Option<crate::flex_audio::FlexAudio>,
) {
    let receive = crate::audio::Buffer::new(crate::flex_audio::RATE);
    let Some(station) = carried(crate::audio_windows::Station::start_network(Arc::clone(
        &receive,
    ))) else {
        return (None, None);
    };
    match crate::flex_audio::FlexAudio::start(
        radio,
        slice,
        receive,
        station.transmit.clone(),
        on_air,
    ) {
        Ok(network) => {
            println!(
                "Audio: the FlexRadio's slice audio comes over the network (DAX channel {}), \
                 with no DAX program.\n  -> For transmit audio, the radio's transmit audio \
                 source must be DAX (SmartSDR's transmit panel: DAX on; the setting \
                 \"tx_dax\" here).",
                crate::flex_audio::CHANNEL
            );
            (Some(station), Some(network))
        }
        Err(why) => {
            println!("Audio: the FlexRadio's audio isn't carried ({why}).");
            (Some(station), None)
        }
    }
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
        // A radio on the network with no transmit cable has nowhere VARA
        // could play, so its sound output is left as it is.
        output: station
            .transmit_cable
            .as_deref()
            .or(station.radio_playing.as_deref())
            .map(|to| vara::as_vara_names_it(to, now.output.as_deref(), &playing)),
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

/// The radio's definition as the station uses it, shared so that a write
/// check run at the console counts at once everywhere.
type Checks = Arc<RwLock<Definition>>;

/// The definition in `checks`, as it is now.
fn now_in(checks: &Checks) -> Definition {
    checks.read().map_or_else(
        |held| held.into_inner().clone(),
        |definition| definition.clone(),
    )
}

/// The radio this `station` serves, and what the lab's profiles need to
/// know about it.
#[derive(Clone)]
struct LabRadio {
    shared: Arc<Shared>,
    serial: String,
    /// The radio's definition, with the settings a check on this computer
    /// found the Agent can set counted as checked once it has been run.
    definition: Checks,
    /// The records folder, where a radio's enrollment is kept.
    records: std::path::PathBuf,
    /// How many profiles are going on now; a program's transmission waits
    /// until none are.
    going_on: Arc<(Mutex<u32>, Condvar)>,
    /// The radio's settings as the Agent last knew them: read as the
    /// station started, with each profile's settings as it put them on.
    /// What differs from this was changed on the radio since, which LabLog
    /// shows when an administrator reads the radio for a profile.
    known: Arc<Mutex<Option<Record>>>,
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
    /// fldigi, while it is connected, so the radio follows it onto CW and
    /// off it again (see `follow_fldigi`).
    fldigi: Mutex<Option<FldigiHeld>>,
}

/// fldigi as the Agent follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FldigiHeld {
    /// Its process.
    pid: u32,
    /// The profile the Agent last put on for it, if any.
    on: Option<&'static str>,
    /// What was last said about why nothing changed, so it is said once.
    said: Option<&'static str>,
}

/// How often the Agent asks fldigi which modem it is on.
const FLDIGI_EVERY: Duration = Duration::from_secs(2);

/// How long the Agent waits for fldigi's answer.
const FLDIGI_WAIT: Duration = Duration::from_millis(500);

/// Follows fldigi onto CW and off it, for as long as `programs` is served.
fn follow_fldigi_while_served(programs: std::sync::Weak<Programs>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(FLDIGI_EVERY);
            let Some(programs) = programs.upgrade() else {
                return;
            };
            if let Some(said) = programs.follow_fldigi(|| {
                crate::fldigi_rpc::modem_name(crate::fldigi_rpc::ADDRESS, FLDIGI_WAIT)
            }) {
                programs.note(&said);
            }
        }
    });
}

impl Programs {
    /// The radio `station` serves, with serial number `serial` if known,
    /// telling programs apart the way Windows can.
    fn for_station(shared: &Arc<Shared>, serial: Option<&str>, definition: &Checks) -> Self {
        Self::new(
            LabRadio {
                shared: Arc::clone(shared),
                serial: serial.unwrap_or_default().to_owned(),
                definition: Arc::clone(definition),
                records: records_folder(),
                going_on: Arc::new((Mutex::new(0), Condvar::new())),
                known: Arc::new(Mutex::new(None)),
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
            fldigi: Mutex::new(None),
        }
    }

    /// With fldigi holding an enrolled radio, puts the `fldigi CW` profile
    /// on once fldigi is on a CW modem, and the `fldigi` one once it is off
    /// it again, as `modem` (asking fldigi for its modem's name) says; never
    /// while transmitting, and never while another program holds the radio
    /// (Principle X, second exception: the lab's profile for the program in
    /// use). Returns what to say, if anything.
    fn follow_fldigi(&self, modem: impl FnOnce() -> Result<String, String>) -> Option<String> {
        let mut held = (*self.fldigi.lock().ok()?)?;
        let connected: Vec<u32> = self.connected.lock().ok()?.values().copied().collect();
        if !connected.contains(&held.pid) {
            *self.fldigi.lock().ok()? = None;
            return None;
        }
        let others = {
            let started = self.started.lock().ok()?;
            connected
                .iter()
                .any(|pid| *pid != held.pid && started.contains(pid))
        };
        let enrolled = self.enrollable
            && enrollment::load(&enrollment::path(&self.lab.records, &self.lab.serial))
                == Some(true);
        if others || !enrolled || self.lab.shared.on_air().load(Ordering::SeqCst) {
            return None;
        }
        let keep = |held: FldigiHeld| {
            if let Ok(mut slot) = self.fldigi.lock()
                && slot.is_some_and(|now| now.pid == held.pid)
            {
                *slot = Some(held);
            }
        };
        let name = match modem() {
            Ok(name) => name,
            Err(why) => {
                if held.said == Some("asked") {
                    return None;
                }
                held.said = Some("asked");
                keep(held);
                return Some(format!(
                    "Lab profile: couldn't ask fldigi which modem it is on ({why}), so the radio \
                     stays as it is for CW."
                ));
            }
        };
        let wanted = if crate::fldigi_rpc::is_cw(&name) {
            profiles::FLDIGI_CW
        } else {
            "fldigi"
        };
        // With nothing put on yet, the radio is as fldigi found it: set up
        // for fldigi's data modes, whatever they are.
        if held.on.unwrap_or("fldigi") == wanted {
            return None;
        }
        let definition = now_in(&self.lab.definition);
        if profiles::current(&self.lab.records, &definition, wanted).is_none() {
            if held.said == Some(wanted) {
                return None;
            }
            held.said = Some(wanted);
            keep(held);
            return Some(format!(
                "Lab profile: fldigi is on {name}; there is no {wanted} profile for the {} yet, so \
                 the radio is as it was. To make one, set the radio up as you want it for \
                 {wanted} (a narrow filter for CW, say), then pick the tray's Save as lab \
                 profile > {wanted} and answer Yes; it goes on from then on.",
                definition.model
            ));
        }
        if let Ok(mut started) = self.started.lock() {
            started.insert(held.pid);
        }
        self.lab.going(true);
        let said = self
            .lab
            .put_on_as(wanted, "fldigi", &format!("went to {name}"));
        self.lab.going(false);
        held.on = Some(wanted);
        held.said = None;
        keep(held);
        Some(said)
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
        if profiles::current(&self.lab.records, &now_in(&self.lab.definition), name).is_none() {
            return Some(format!(
                "Lab profile: {exe} is {name}; the lab has no {name} profile for the {} yet, so \
                 the radio is as it was.{}",
                identity.model,
                if crate::product::current().standalone {
                    format!(
                        " To make one, set the radio up as you want it for {name}, then pick \
                         the tray's Save as lab profile > {name} and answer Yes; it goes on \
                         each time {name} starts from then on."
                    )
                } else {
                    String::new()
                }
            ));
        }
        None
    }
}

impl LabRadio {
    /// Puts the lab's profile for program `name` on the radio, says what
    /// became of it, and records it like any restore.
    fn put_on(&self, name: &str, exe: &str) -> String {
        self.put_on_as(name, exe, "started")
    }

    /// Puts the lab's profile for program `name` on the radio because
    /// `exe` did `what` ("started", say), and says what became of it.
    fn put_on_as(&self, name: &str, exe: &str, what: &str) -> String {
        let definition = now_in(&self.definition);
        let Some(profile) = profiles::current(&self.records, &definition, name) else {
            return format!(
                "Lab profile: no {name} profile for the {}.",
                definition.model
            );
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
            profile.kept(&definition),
            &now,
            &definition,
        );
        let head = format!(
            "Lab profile: {exe} {what}, so the {name} profile goes on ({}).",
            profile.origin()
        );
        if plan.put.is_empty() && plan.kept_back.is_empty() {
            return format!("{head}\nThe radio already matches it; nothing changed.");
        }
        let mut lines = vec![head];
        lines.extend(plan.lines());
        if !plan.put.is_empty() {
            lines.push(restore(&self.shared, &plan, serial));
            self.learn(&ids);
        }
        lines.join("\n")
    }

    /// Reads the settings `ids` again into what the Agent knows of the
    /// radio, after it changed them.
    fn learn(&self, ids: &[&str]) {
        let read = self.shared.settings(|radio| {
            settings_record::read_these(
                radio,
                ids,
                Some(&self.serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            )
        });
        let (Ok(read), Ok(mut known)) = (read, self.known.lock()) else {
            return;
        };
        if let Some(known) = known.as_mut() {
            for entry in read.settings {
                if let Some(old) = known.settings.iter_mut().find(|old| old.id == entry.id) {
                    *old = entry;
                }
            }
        }
    }

    /// Reads the whole radio for LabLog, which asked for it to make the
    /// lab's `program` profile: every setting the Agent may write, the
    /// profile it puts on now, and what changed since it last knew. Changes
    /// nothing on the radio.
    fn read_for_lablog(&self, program: &str) -> crate::engine::ReadAnswer {
        let failed = |problem: String| crate::engine::ReadAnswer::Failed { problem };
        let read = self.shared.settings(|radio| {
            settings_record::read(
                radio,
                Some(&self.serial),
                std::time::SystemTime::now(),
                &settings_record::computer_name(),
            )
        });
        let record = match read {
            Ok(record) => record,
            Err(why) => return failed(why),
        };
        let definition = now_in(&self.definition);
        let settings = crate::lab_stock::checked(&record, &definition);
        if settings.is_empty() {
            return failed(format!(
                "none of the {}'s settings has been checked on a real radio yet, so a profile \
                 would put nothing on",
                record.model
            ));
        }
        let profile = profiles::current(&self.records, &definition, program);
        let changed = self.known.lock().ok().and_then(|known| {
            known.as_ref().map(|known| {
                settings
                    .iter()
                    .filter(|now| {
                        known
                            .settings
                            .iter()
                            .find(|old| old.id == now.id)
                            .is_some_and(|old| old.bytes.as_deref() != Some(now.bytes.as_str()))
                    })
                    .map(|now| now.id.clone())
                    .collect::<Vec<_>>()
            })
        });
        crate::engine::ReadAnswer::Read {
            maker: record.maker.clone(),
            model: record.model.clone(),
            serial: self.serial.clone(),
            settings,
            profile: profile
                .as_ref()
                .map(|p| p.shown(&definition))
                .unwrap_or_default(),
            profile_from: profile
                .as_ref()
                .map_or_else(|| "none yet".to_owned(), profiles::Profile::origin),
            changed: changed.unwrap_or_default(),
        }
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
        if name == "fldigi"
            && let Ok(mut fldigi) = self.fldigi.lock()
            && fldigi.is_none_or(|held| held.pid != pid)
        {
            *fldigi = Some(FldigiHeld {
                pid,
                on: why_not.is_none().then_some("fldigi"),
                said: None,
            });
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

/// How often the Agent asks LabLog whether an administrator wants its
/// radio read for a lab profile.
const READS_EVERY: Duration = Duration::from_secs(5);
/// How often the Agent fetches the lab profiles saved in LabLog.
const PROFILES_EVERY: Duration = Duration::from_secs(30);

/// The Agent's side of LabLog's Lab Profiles page, on its own thread while
/// the station runs with `--engine`: it keeps a copy of the profiles saved
/// there, and reads the radio when an administrator asks. Dropping it
/// stops it.
struct ProfileDesk {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ProfileDesk {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the Agent's side of LabLog's Lab Profiles page for `lab`.
fn start_profile_desk(uplink: Uplink, lab: LabRadio) -> ProfileDesk {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let mut fetched: Option<std::time::Instant> = None;
        let mut asked: Option<std::time::Instant> = None;
        let mut said: Option<String> = None;
        while !flag.load(Ordering::SeqCst) {
            if fetched.is_none_or(|at| at.elapsed() >= PROFILES_EVERY) {
                fetched = Some(std::time::Instant::now());
                fetch_profiles(&uplink, &lab, &mut said);
            }
            if asked.is_none_or(|at| at.elapsed() >= READS_EVERY) {
                asked = Some(std::time::Instant::now());
                answer_reads(&uplink, &lab);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    ProfileDesk {
        stop,
        thread: Some(thread),
    }
}

/// Keeps a copy of the profiles saved in LabLog for the radio's model, and
/// says when one changes. `said` is the last problem said, so it is said
/// once.
fn fetch_profiles(uplink: &Uplink, lab: &LabRadio, said: &mut Option<String>) {
    let definition = &now_in(&lab.definition);
    let saved = match uplink.lab_profiles(&definition.maker, &definition.model) {
        Ok(saved) => saved,
        Err(error) => {
            let why = format!(
                "Lab profile: couldn't hear from LabLog ({error}), so programs get the {} \
                 profiles last heard from it, or the Agent's own.",
                definition.model
            );
            if said.as_deref() != Some(why.as_str()) {
                lab.shared.note(&why);
                *said = Some(why);
            }
            return;
        }
    };
    *said = None;
    let path = profiles::saved_path(&lab.records, &definition.maker, &definition.model);
    let before = profiles::load_saved(&path);
    if before == saved {
        return;
    }
    if let Err(why) = profiles::keep_saved(&path, &saved) {
        lab.shared.note(&format!("Lab profile: {why}"));
        return;
    }
    for profile in &saved {
        if before.contains(profile) {
            continue;
        }
        let said = if profiles::from_saved(profile, definition).is_some() {
            format!(
                "Lab profile: LabLog's {} profile for the {} is version #{}, saved by {} at {}; \
                 it goes on as {} starts from now on.",
                profile.program,
                definition.model,
                profile.id,
                profile.saved_by,
                profile.saved_at,
                profile.program
            )
        } else {
            format!(
                "Lab profile: LabLog's {} profile for the {} (version #{}) has a setting this \
                 Agent may not write, so it isn't used; the Agent's own is.",
                profile.program, definition.model, profile.id
            )
        };
        lab.shared.note(&said);
    }
}

/// Reads the radio for each request waiting in LabLog, and answers it.
fn answer_reads(uplink: &Uplink, lab: &LabRadio) {
    let Ok(asked) = uplink.profile_reads() else {
        return;
    };
    for ask in asked {
        let answer = lab.read_for_lablog(&ask.program);
        let said = match &answer {
            crate::engine::ReadAnswer::Read { settings, .. } => format!(
                "Lab profile: LabLog asked for this radio's settings, for the {} profile; read \
                 {} and sent them. Nothing on the radio changed.",
                ask.program,
                settings.len()
            ),
            crate::engine::ReadAnswer::Failed { problem } => format!(
                "Lab profile: LabLog asked for this radio's settings, for the {} profile, but \
                 they couldn't be read: {problem}",
                ask.program
            ),
        };
        lab.shared.note(&said);
        if let Err(error) = uplink.answer_read(&ask.id, &answer) {
            lab.shared.note(&format!(
                "Lab profile: the reading didn't reach LabLog: {error}"
            ));
        }
    }
}

/// The programs whose settings the Agent sets up, by executable, and the
/// names they are shown by.
#[cfg(any(windows, test))]
const SET_UP_PROGRAMS: [(&str, &str); 6] = [
    ("vara.exe", "VARA"),
    ("rms express.exe", "Winlink Express"),
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

/// Stands between VARA and its client (Winlink Express) so the Agent keys
/// the radio when VARA asks: moves VARA's command port to the Agent's, if
/// VARA isn't running, and listens on the one VARA's clients use (see
/// `vara_proxy`).
#[cfg(windows)]
fn vara_proxy(
    rigctld: SocketAddr,
    notes: mpsc::Sender<String>,
) -> Option<crate::vara_proxy::Proxy> {
    use crate::vara::{self, MOVED_COMMAND_PORT, Moved};
    use crate::vara_proxy::{Ports, Proxy};
    let path = vara::find()?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let clients = vara::clients_port(&text, vara::original(&path).as_deref());
    // Only Winlink Express needs the Agent between it and VARA. Without it,
    // VARA stays on its own port, as VarAC has always found it.
    if crate::winlink::find().is_none() {
        if vara::command_port(&text) == Some(MOVED_COMMAND_PORT) && vara::running() == Ok(false) {
            match vara::set_command_port(&path, clients) {
                Ok(_) => println!(
                    "VARA: command port put back to {clients} in {} (Winlink Express isn't \
                     installed, so the Agent doesn't stand between VARA and its client).",
                    path.display()
                ),
                Err(why) => println!("VARA: its command port wasn't put back ({why})."),
            }
        }
        return None;
    }
    if vara::command_port(&text) != Some(MOVED_COMMAND_PORT) {
        match vara::running() {
            Ok(false) => match vara::set_command_port(&path, MOVED_COMMAND_PORT) {
                Ok(Moved::Already) => {}
                Ok(Moved::From(before)) => println!(
                    "VARA: command port set to {MOVED_COMMAND_PORT} in {} (was {}), so the Agent \
                     can stand between VARA and Winlink Express on {clients}.",
                    path.display(),
                    before.map_or_else(|| "not set".to_owned(), |port| port.to_string()),
                ),
                Err(why) => {
                    println!(
                        "VARA: its command port wasn't moved ({why}), so the Agent can't key \
                         for Winlink Express."
                    );
                    return None;
                }
            },
            Ok(true) => {
                println!(
                    "VARA: running on its own port, so the Agent can't key for Winlink Express \
                     yet.\n  -> Close VARA, then start the Agent again."
                );
                return None;
            }
            Err(why) => {
                println!("VARA: its command port was left alone ({why}).");
                return None;
            }
        }
    }
    let ports = Ports {
        clients,
        vara: MOVED_COMMAND_PORT,
        rigctld,
    };
    match Proxy::start(&ports, Arc::new(program_id::keys_itself), notes) {
        Ok(proxy) => {
            println!(
                "VARA: the Agent passes VARA's clients through on 127.0.0.1:{clients} (and {}), \
                 and keys the radio when VARA asks, for Winlink Express. VARA itself is on \
                 {MOVED_COMMAND_PORT}.",
                clients + 1
            );
            Some(proxy)
        }
        Err(why) => {
            println!(
                "VARA: the Agent can't stand between VARA and Winlink Express ({why}).\n  -> If \
                 VARA is running, close it and start the Agent again."
            );
            None
        }
    }
}

/// Where the virtual Kenwood answers, and the program's end of its
/// com0com pair when known: `--kenwood PORT` if given, else, where
/// Winlink Express is installed, the first com0com pair a program can open
/// by a `COMn` name, installing com0com with one if there is none (see
/// `com0com`).
#[cfg(windows)]
fn kenwood_ports(asked: Option<&str>) -> Option<(String, Option<String>)> {
    let listing = crate::com0com::listed();
    if let Some(port) = asked {
        let program = crate::com0com::pairs(&listing)
            .into_iter()
            .find(|pair| pair.agent.eq_ignore_ascii_case(port))
            .map(|pair| pair.program);
        return Some((port.to_owned(), program));
    }
    crate::winlink::find()?;
    let pair = match crate::com0com::for_programs(&listing) {
        Some(pair) => pair,
        None => match crate::com0com::install() {
            Ok(pair) => {
                println!(
                    "com0com: installed, with the pair {} <-> {}.",
                    pair.program, pair.agent
                );
                pair
            }
            Err(why) => {
                println!(
                    "Virtual Kenwood: not started, as there is no com0com pair: {why}\n  \
                     Winlink Express still keys through the Agent; it tunes the radio only \
                     through a COM port."
                );
                return None;
            }
        },
    };
    Some((pair.agent, Some(pair.program)))
}

/// How Winlink Express is set to tune, with or without the virtual
/// Kenwood.
#[cfg(windows)]
fn kenwood_tuning(kenwood: Option<&(String, Option<String>)>) -> crate::winlink::Tuning<'_> {
    kenwood.map_or(crate::winlink::Tuning::Agent, |(_, program)| {
        crate::winlink::Tuning::Kenwood(program.as_deref())
    })
}

/// Answers as a Kenwood TS-2000 on `port`, one end of a com0com pair, for
/// a program such as Winlink Express on `program`, the other end (see
/// `kenwood_cat`).
#[cfg(windows)]
fn virtual_kenwood(
    port: &str,
    program: Option<&str>,
    rigctld: SocketAddr,
) -> Option<crate::kenwood_cat::Virtual> {
    match crate::kenwood_cat::Virtual::start(port, rigctld) {
        Ok(running) => {
            let other = program.map_or_else(
                || "the other port of the pair".to_owned(),
                |program| format!("{program} (set by the Agent while Winlink Express is closed)"),
            );
            println!(
                "Virtual Kenwood: answering as a TS-2000 on {port}. In Winlink Express, \
                 Radio Setup: Kenwood, Control Port {other}."
            );
            Some(running)
        }
        Err(why) => {
            println!(
                "Virtual Kenwood: not started ({why}).\n  -> Is com0com installed, and is \
                 {port} one end of its pair?"
            );
            None
        }
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
    println!(
        "  Winlink Express: VARA HF TNC at 127.0.0.1, ports as usual (8300, 8301); the Agent \
         sets its Radio Setup to Manual, no control port, PTT External, and keys when VARA asks."
    );
}

/// Runs the station until Ctrl-C.
///
/// # Errors
///
/// The radio isn't found, the virtual radio can't listen, or an Engine
/// given with `--engine` (its address, the key file, and the station to
/// join as) doesn't make a link. FieldLab's own Engine, linked to with no
/// `--engine`, never stops the station (see `engine_link`).
pub fn run(
    wanted: Wanted<'_>,
    pretend: bool,
    listen: SocketAddr,
    limit: Duration,
    engine: &EngineLink,
    kenwood: Option<&str>,
    stated: &crate::sends_as::Stated,
) -> Result<(), String> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))
        .map_err(|error| format!("can't watch for Ctrl-C: {error}"))?;
    let pick = Arc::new(crate::flex_pick::Pick::starting_with(wanted.flex));
    let typing = start_typing(Arc::clone(&running), Arc::clone(&pick));
    let mut stated = stated.clone();
    if stated.no_bus {
        stated.mqtt = None;
    } else if stated.mqtt.is_none() {
        // FieldLab's broker, as the Engine takes it, else FieldLab's own.
        stated.mqtt = std::env::var("LABLOG_MQTT")
            .ok()
            .filter(|broker| !broker.trim().is_empty())
            .and_then(|broker| crate::sends_as::broker(broker.trim()).ok())
            .or_else(|| {
                Some((
                    crate::sends_as::FIELDLAB_BROKER.to_owned(),
                    crate::sends_as::MQTT_PORT,
                ))
            });
    }
    let _whereabouts = crate::sends_as::Whereabouts::start(stated, records_folder());
    // A radio unplugged, turned off or swapped for another is looked for
    // again, so any radio can be moved to this computer while the Agent runs.
    let mut fieldlab = (*engine == EngineLink::FieldLab).then(|| {
        crate::engine_link::FieldLab::new(
            &records_folder()
                .parent()
                .map_or_else(|| ".".into(), std::path::Path::to_path_buf),
        )
    });
    let joined = fieldlab.as_ref().map(crate::engine_link::FieldLab::joined);
    loop {
        let uplink = match engine {
            EngineLink::Given {
                engine,
                key_file,
                station,
            } => Some(uplink(engine, key_file, station.as_deref(), &running)?),
            EngineLink::FieldLab => fieldlab.as_mut().and_then(|link| link.uplink(&running)),
            EngineLink::Off | EngineLink::Standalone => None,
        };
        let ended = if pretend {
            let found = Found {
                radio: Box::new(crate::civ_pretend_radio().map_err(|error| error.to_string())?),
                port: "the pretend port".to_owned(),
                serial: Some(PRETEND_SERIAL.to_owned()),
                network_audio: None,
                held: None,
            };
            run_found(
                found,
                listen,
                limit,
                Link {
                    uplink,
                    joined: joined.as_deref(),
                    standalone: *engine == EngineLink::Standalone,
                    pick: &pick,
                },
                &running,
                &typing,
                kenwood,
            )?
        } else {
            let Some(found) = wait_for_radio(wanted.port, &pick, &running, listen) else {
                break;
            };
            run_found(
                found,
                listen,
                limit,
                Link {
                    uplink,
                    joined: joined.as_deref(),
                    standalone: *engine == EngineLink::Standalone,
                    pick: &pick,
                },
                &running,
                &typing,
                kenwood,
            )?
        };
        if !said_after(ended, &pick) {
            break;
        }
    }
    println!("stopped");
    Ok(())
}

/// Whether the station lets go of the radio for a new FlexRadio pick now:
/// only once the radio is off the air, so no transmission is cut off.
fn switch_now(pick: &crate::flex_pick::Pick, on_air: &AtomicBool) -> bool {
    pick.has_changed() && !on_air.load(Ordering::SeqCst)
}

/// Starts answering the programs: on the port held while the radio was
/// fetched, if it was, otherwise at `listen`.
fn serve_programs(
    held: Option<std::net::TcpListener>,
    listen: SocketAddr,
    programs: Arc<Programs>,
) -> Result<rigctld_server::Server, String> {
    let source = programs as Arc<dyn Source>;
    match held {
        Some(held) => rigctld_server::Server::start_on(held, source),
        None => rigctld_server::Server::start(listen, source),
    }
}

/// Says why the station let go of the radio, and whether it goes on to
/// look for one again (`false`: asked to stop).
fn said_after(ended: Ended, pick: &crate::flex_pick::Pick) -> bool {
    let said = match ended {
        Ended::Stopped => return false,
        Ended::Joined => {
            "LabLog: joined, so the Agent starts again with the link to LabLog.".to_owned()
        }
        Ended::Switched if pick.checked() && pick.current().is_none() => {
            "Radio: let go of it; Use FlexRadio is on, so the Agent takes the FlexRadio when a \
             radio program opens."
                .to_owned()
        }
        Ended::Switched => pick.current().map_or_else(
            || "Radio: let go of the FlexRadio; looking for USB radios.".to_owned(),
            |flex| format!("Radio: let go of it; looking for the FlexRadio {flex}."),
        ),
        Ended::Lost => pick.lost().unwrap_or_else(|| {
            format!(
                "Radio lost: it hasn't answered for {} s (unplugged, turned off, or swapped).\n\
                 -> The Agent looks for a radio again; plug in any radio it knows.",
                LOST_AFTER.as_secs()
            )
        }),
    };
    crate::print_event(&said);
    true
}

/// Starts carrying QSOs, the radio's state and who LabLog logs as between
/// the programs and the Engine, when there is one.
fn start_carrier(uplink: Option<&Uplink>, shared: &Arc<Shared>) -> Option<crate::carrier::Carrier> {
    let Some(uplink) = uplink else {
        if crate::product::current().standalone {
            println!("QSOs: stay in each program's own log.");
        } else {
            println!("QSOs: stay in each program's own log (LabLog isn't linked).");
        }
        return None;
    };
    let radio = Arc::clone(shared) as Arc<dyn Source>;
    crate::carrier::start(uplink.clone(), radio, &records_folder())
        .map_err(|why| println!("QSOs: not sent to LabLog ({why})."))
        .ok()
}

/// The serial number the pretend radio's settings are kept under.
const PRETEND_SERIAL: &str = "PRETEND";

/// The radio's definition as the station uses it: standing alone, with the
/// settings a check on this computer found work counted as checked, that
/// check being the evidence (Principle XVII; see `check_here`).
fn station_definition(definition: &Definition, standalone: bool) -> Definition {
    if standalone {
        crate::write_check::with_checks_here(definition, &records_folder())
    } else {
        definition.clone()
    }
}

/// Says which radio the station found, and where.
fn say_found(found: &Found) {
    let identity = found.radio.identity();
    // A FlexRadio on the network has a serial number but no USB or speed.
    let baud = found.radio.baud();
    println!(
        "Found: {} {} on {}{}{}.",
        identity.maker,
        identity.model,
        found.port,
        found
            .serial
            .as_deref()
            .map(|serial| if baud == 0 {
                format!(" (serial {serial})")
            } else {
                format!(" (USB serial {serial})")
            })
            .unwrap_or_default(),
        if baud == 0 {
            String::new()
        } else {
            format!(", at {baud} baud")
        }
    );
}

/// Runs the station with the radio found, until stopped.
fn run_found(
    mut found: Found,
    listen: SocketAddr,
    limit: Duration,
    link: Link<'_>,
    running: &AtomicBool,
    typing: &Typing,
    kenwood: Option<&str>,
) -> Result<Ended, String> {
    let (uplink, joined, standalone, pick) = (link.uplink, link.joined, link.standalone, link.pick);
    let identity = found.radio.identity().clone();
    say_found(&found);
    let record = check_settings(found.radio.as_mut(), found.serial.as_deref());
    let definition = station_definition(found.radio.definition(), standalone);
    let checks: Checks = Arc::new(RwLock::new(definition.clone()));
    if let Some(uplink) = &uplink {
        sync(uplink, &record, &definition);
    }
    let sound = definition.usb_audio.clone();
    let network_audio = found.network_audio;
    let (notes, heard) = mpsc::channel();
    #[cfg(windows)]
    let vara_notes = notes.clone();
    let shared = Arc::new(Shared::with_transmit(found.radio, limit).with_notes(notes));
    let on_air = shared.on_air();
    #[cfg(windows)]
    let (station, flex_audio) = match network_audio {
        Some((radio, slice)) => flex_audio(radio, slice, Arc::clone(&on_air)),
        None => (audio(sound.as_deref(), Arc::clone(&on_air)), None),
    };
    #[cfg(not(windows))]
    {
        let _ = (&sound, &on_air, network_audio);
        println!("Audio: carried on Windows only.");
    }
    let programs = Programs::for_station(&shared, found.serial.as_deref(), &checks);
    if let Ok(mut known) = programs.lab.known.lock() {
        *known = Some(record.clone());
    }
    let profile_desk = uplink
        .as_ref()
        .filter(|_| found.serial.is_some())
        .map(|uplink| start_profile_desk(uplink.clone(), programs.lab.clone()));
    let programs = Arc::new(programs);
    follow_fldigi_while_served(Arc::downgrade(&programs));
    let server = serve_programs(found.held.take(), listen, programs)?;
    say_how_to_connect(&identity.model, server.address(), limit);
    #[cfg(windows)]
    let kenwood = kenwood_ports(kenwood);
    #[cfg(windows)]
    let tuning = kenwood_tuning(kenwood.as_ref());
    #[cfg(windows)]
    if let Some(station) = &station {
        crate::programs::set_up(station, server.address(), tuning);
    }
    #[cfg(windows)]
    let vara_proxy = vara_proxy(server.address(), vara_notes);
    #[cfg(windows)]
    let virtual_kenwood = kenwood
        .as_ref()
        .and_then(|(port, program)| virtual_kenwood(port, program.as_deref(), server.address()));
    #[cfg(not(windows))]
    if kenwood.is_some() {
        println!("Virtual Kenwood: Windows only, so not started.");
    }
    let carrier = start_carrier(uplink.as_ref(), &shared);
    let setups = setups_for(uplink, standalone);
    let desk = found
        .serial
        .clone()
        .map(|serial| start_console(&shared, setups, serial, &checks, &record, typing));
    let mut offer_drive = after_unkey(&shared, &definition, desk, &on_air);
    println!("Ctrl-C or quit stops.");
    #[cfg(windows)]
    let mut watching = Watching::new(kenwood.clone());
    let serving = AtomicBool::new(true);
    let mut lost = Lost::default();
    let mut relink = false;
    let mut switched = false;
    crate::show_until_stopped(shared.as_ref(), &heard, &serving, || {
        offer_drive();
        #[cfg(windows)]
        if let Some(station) = &station {
            watching.watch(station, &on_air, server.address());
        }
        relink |= joined.is_some_and(|joined| joined.swap(false, Ordering::SeqCst));
        // A new FlexRadio pick waits for the radio to be off the air, so no
        // transmission is cut off.
        switched |= switch_now(pick, &on_air);
        if switched || relink || !running.load(Ordering::SeqCst) || lost.now(shared.as_ref()) {
            serving.store(false, Ordering::SeqCst);
        }
    });
    #[cfg(windows)]
    drop(vara_proxy);
    #[cfg(windows)]
    drop(virtual_kenwood);
    drop(carrier);
    drop(profile_desk);
    drop(server);
    #[cfg(windows)]
    drop(flex_audio);
    #[cfg(windows)]
    drop(station);
    // The console lets go of the radio, so it can be found again.
    if let Ok(mut slot) = typing.to_console.lock() {
        *slot = None;
    }
    if switched && running.load(Ordering::SeqCst) {
        return Ok(Ended::Switched);
    }
    Ok(Ended::after(running, relink))
}

/// How a station run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    /// Asked to stop.
    Stopped,
    /// The radio stopped answering: unplugged, turned off, or swapped.
    Lost,
    /// FieldLab's Engine let this station in, so it starts again linked.
    Joined,
    /// The operator picked a FlexRadio, or stopped using one.
    Switched,
}

impl Ended {
    /// Why the station stopped serving the radio: asked to, `relink`ed, or
    /// else the radio was lost.
    fn after(running: &AtomicBool, relink: bool) -> Self {
        if !running.load(Ordering::SeqCst) {
            Self::Stopped
        } else if relink {
            Self::Joined
        } else {
            Self::Lost
        }
    }
}

/// The station's link to the Engine for one radio: the link, if any, and
/// for FieldLab's Engine, the flag that says joining has just saved a key.
struct Link<'a> {
    uplink: Option<Uplink>,
    joined: Option<&'a AtomicBool>,
    /// Standing alone (`--standalone`): Lab Stock and the lab's profiles
    /// are kept, and changed, on this computer.
    standalone: bool,
    /// The FlexRadio the operator picked, if any: a new pick ends this run.
    pick: &'a crate::flex_pick::Pick,
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
    /// The virtual Kenwood's port and its pair's program end, when
    /// programs tune the radio through it.
    kenwood: Option<(String, Option<String>)>,
}

#[cfg(windows)]
impl Watching {
    fn new(kenwood: Option<(String, Option<String>)>) -> Self {
        Self {
            hint: None,
            last: std::time::Instant::now(),
            was_on_air: false,
            open: ProgramsOpen::now(),
            kenwood,
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
            crate::programs::set_up(station, radio, kenwood_tuning(self.kenwood.as_ref()));
        } else if crate::sends_as::take_changed() {
            // The call or grid changed (see `sends_as`, which says so).
            crate::programs::set_up(station, radio, kenwood_tuning(self.kenwood.as_ref()));
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
    let found = find(port, &running)?;
    check_writes_on(found.radio, &found.port, found.serial.as_deref())
}

/// `check-writes` on a radio already found: on `port` (or a FlexRadio's
/// address), with its USB serial number if known.
///
/// # Errors
///
/// The console can't be read.
pub fn check_writes_on(
    radio: Box<dyn SerialRadio>,
    port: &str,
    serial: Option<&str>,
) -> Result<(), String> {
    let mut found = Found {
        radio,
        port: port.to_owned(),
        serial: serial.map(str::to_owned),
        network_audio: None,
        held: None,
    };
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
        found.radio.as_mut(),
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
                    "add",
                    RUN_KEY,
                    "/v",
                    crate::product::current().name,
                    "/t",
                    "REG_SZ",
                    "/d",
                    &command,
                    "/f",
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
                let done = reg(&[
                    "delete",
                    RUN_KEY,
                    "/v",
                    crate::product::current().name,
                    "/f",
                ])?;
                if done.status.success() {
                    println!("Windows won't start the Agent at sign-in any more.");
                } else {
                    println!("Windows wasn't starting the Agent at sign-in.");
                }
                println!("If the Agent's icon is by the clock, its menu's Quit stops it.");
                Ok(())
            }
            "status" => {
                let done = reg(&["query", RUN_KEY, "/v", crate::product::current().name])?;
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
    use std::sync::{Arc, Mutex, RwLock};
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

    #[test]
    fn a_flexradio_pick_waits_until_the_radio_is_off_the_air() {
        let pick = crate::flex_pick::Pick::default();
        let on_air = AtomicBool::new(false);
        assert!(!super::switch_now(&pick, &on_air));
        assert!(
            super::flex_typed("flex stop", &pick).is_some_and(|said| said.contains("none in use"))
        );
        assert!(
            super::flex_typed("FLEX use 1234", &pick)
                .is_some_and(|said| said.contains("wasn't heard"))
        );
        assert!(
            super::flex_typed("flex use", &pick).is_some_and(|said| said.starts_with("flex takes"))
        );
        assert_eq!(super::flex_typed("keep FT8", &pick), None);
        assert_eq!(super::flex_typed("flexible", &pick), None);
        assert!(
            super::flex_typed("flex on", &pick)
                .is_some_and(|said| said.contains("Use FlexRadio is on"))
        );
        assert!(pick.checked());
        assert!(
            super::flex_typed("Flex OFF", &pick).is_some_and(|said| said.contains("goes back"))
        );
        assert!(!pick.checked());
        let pick = crate::flex_pick::Pick::starting_with(Some("any"));
        assert!(
            super::flex_typed("flex stop", &pick).is_some_and(|said| said.contains("goes back"))
        );
        on_air.store(true, Ordering::SeqCst);
        assert!(
            !super::switch_now(&pick, &on_air),
            "never while transmitting"
        );
        on_air.store(false, Ordering::SeqCst);
        assert!(super::switch_now(&pick, &on_air));
        // Back to USB radios: the station looks again, and says why.
        assert!(super::said_after(super::Ended::Switched, &pick));
        assert!(super::said_after(super::Ended::Lost, &pick));
        assert!(!super::said_after(super::Ended::Stopped, &pick));
    }

    #[test]
    fn finds_a_flexradio_by_its_address() {
        let Ok(pretend) = crate::flex_pretend::PretendFlex::start("FLEX-6600", true) else {
            return;
        };
        let found = super::find_flex(pretend.address());
        assert!(found.is_ok(), "{:?}", found.as_ref().err());
        let Ok(found) = found else { return };
        assert_eq!(found.port, format!("the network at {}", pretend.address()));
        assert_eq!(found.serial.as_deref(), Some("PRETEND-6600"));
        assert_eq!(found.radio.definition().model, "FLEX-6600");
        assert_eq!(found.radio.baud(), 0);
        assert!(super::find_flex("127.0.0.1:1").is_err());
    }

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
            definition: Arc::new(RwLock::new(definition)),
            records: records.clone(),
            going_on: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
            known: Arc::new(Mutex::new(None)),
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

    #[test]
    fn the_radio_follows_fldigi_onto_cw_and_off_it_again() {
        let records = std::env::temp_dir().join(format!("lablog-fldigi-cw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&records);
        let (shared, lab, definition) = pretend_lab(&records);
        let programs = Programs::new(
            lab,
            true,
            Box::new(|program: SocketAddr, _| match program.port() {
                50_010 => Ok((99, r"C:\Program Files\fldigi-4.2.05\fldigi.exe".to_owned())),
                _ => Err("not found".to_owned()),
            }),
        );
        let server = SocketAddr::from(([127, 0, 0, 1], 4532));
        let fldigi = SocketAddr::from(([127, 0, 0, 1], 50_010));
        let modem = |name: &'static str| move || Ok::<_, String>(name.to_owned());

        // Not enrolled: nothing follows fldigi.
        programs.connected(fldigi, server);
        assert_eq!(programs.follow_fldigi(modem("CW")), None);
        programs.gone(fldigi);
        assert_eq!(programs.follow_fldigi(modem("CW")), None);
        assert!(programs.fldigi.lock().is_ok_and(|held| held.is_none()));

        assert!(
            crate::enrollment::save(&crate::enrollment::path(&records, "PRETEND"), true).is_ok()
        );
        programs.connected(fldigi, server);
        assert!(programs.lab.ready(), "the fldigi profile went on");
        let for_data = setting_now(&shared, "agc");
        assert_eq!(programs.follow_fldigi(modem("BPSK31")), None);

        // On CW with no CW profile yet: said once, nothing changed.
        let said = programs.follow_fldigi(modem("CW")).unwrap_or_default();
        assert!(said.contains("no fldigi CW profile"), "{said}");
        assert_eq!(programs.follow_fldigi(modem("CW")), None);
        assert_eq!(setting_now(&shared, "agc"), for_data);

        // With one saved, it goes on, once, and fldigi's goes back after.
        let narrow = crate::engine::StockSetting {
            id: "agc".to_owned(),
            label: "AGC".to_owned(),
            bytes: "03".to_owned(),
            shown: None,
        };
        assert!(
            crate::profiles::save_here(
                &records,
                &definition,
                "fldigi CW",
                vec![narrow],
                "FL-1",
                "now"
            )
            .is_ok()
        );
        let said = programs.follow_fldigi(modem("CW")).unwrap_or_default();
        assert!(
            said.contains("fldigi went to CW, so the fldigi CW profile goes on"),
            "{said}"
        );
        assert_eq!(setting_now(&shared, "agc").as_deref(), Some("03"));
        assert_eq!(programs.follow_fldigi(modem("CW")), None);
        let said = programs.follow_fldigi(modem("RTTY")).unwrap_or_default();
        assert!(
            said.contains("fldigi went to RTTY, so the fldigi profile goes on"),
            "{said}"
        );
        assert_eq!(setting_now(&shared, "agc"), for_data);

        // fldigi not answering is said once; gone, it isn't followed.
        let said = programs
            .follow_fldigi(|| Err("refused".to_owned()))
            .unwrap_or_default();
        assert!(said.contains("couldn't ask fldigi"), "{said}");
        assert_eq!(programs.follow_fldigi(|| Err("refused".to_owned())), None);
        programs.gone(fldigi);
        assert_eq!(programs.follow_fldigi(modem("CW")), None);
        assert_eq!(setting_now(&shared, "agc"), for_data);
        let _ = std::fs::remove_dir_all(&records);
    }

    /// A pretend IC-7300 for the lab's profiles, keeping its enrollment and
    /// LabLog's profiles in `records`, with what the Agent knows of it read
    /// as the station starts.
    fn pretend_lab(records: &std::path::Path) -> (Arc<Shared>, LabRadio, rigdef::Definition) {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-7300"))
            .unwrap_or_else(|| unreachable!("the IC-7300 is built in"));
        let radio = CivRadio::find(
            PretendPort::new(PretendCiv::new(&definition), 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let shared = Arc::new(Shared::with_transmit(
            Box::new(radio),
            Duration::from_mins(1),
        ));
        let started = shared
            .settings(|radio| settings_record::read(radio, Some("PRETEND"), UNIX_EPOCH, "test"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        let lab = LabRadio {
            shared: Arc::clone(&shared),
            serial: "PRETEND".to_owned(),
            definition: Arc::new(RwLock::new(definition.clone())),
            records: records.to_owned(),
            going_on: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
            known: Arc::new(Mutex::new(Some(started))),
        };
        (shared, lab, definition)
    }

    #[test]
    fn reads_the_radio_for_lablog_and_puts_lablogs_saved_profile_on() {
        let records =
            std::env::temp_dir().join(format!("lablog-lablog-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&records);
        let (shared, lab, definition) = pretend_lab(&records);
        let write = |id: &str, bytes: &[u8]| {
            let setting = definition
                .settings
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .unwrap_or_else(|| unreachable!("{id}"));
            assert!(
                shared
                    .settings(|radio| radio.write_exposed(&setting, bytes))
                    .is_ok_and(|written| written.is_ok())
            );
        };

        // brian turns RF power down at the radio; LabLog asks for a reading.
        write("rf_power", &[0x00, 0x69]);
        let crate::engine::ReadAnswer::Read {
            settings,
            profile,
            profile_from,
            changed,
            serial,
            ..
        } = lab.read_for_lablog("VARA")
        else {
            unreachable!("the pretend radio can be read")
        };
        assert_eq!(serial, "PRETEND");
        assert_eq!(changed, ["rf_power"]);
        assert_eq!(profile_from, "built into the Agent");
        assert_eq!(
            profile.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            [
                "mode_and_filter",
                "data_mode_and_filter",
                "filter_width",
                "usb_mod_level",
                "data_mod"
            ]
        );
        assert!(
            settings
                .iter()
                .any(|s| s.id == "rf_power" && s.bytes == "00 69")
        );
        assert!(
            settings.iter().all(|s| definition
                .settings
                .iter()
                .any(|d| d.id == s.id && d.write && d.write_checked)),
            "only settings the Agent may write"
        );

        // The administrator saves the built-in settings plus RF power; the
        // Agent's copy of LabLog's profiles then has it, and it goes on.
        let mut chosen: Vec<crate::engine::StockSetting> = settings
            .into_iter()
            .filter(|s| s.id == "rf_power" || profile.iter().any(|p| p.id == s.id))
            .collect();
        chosen.retain(|s| s.id != "filter_width");
        let saved = crate::engine::SavedProfile {
            id: 4,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            program: "VARA".to_owned(),
            settings: chosen,
            saved_by: "K9KSU".to_owned(),
            saved_at: "2026-10-30T00:40:00Z".to_owned(),
            here: false,
        };
        let path = crate::profiles::saved_path(&records, "Icom", "IC-7300");
        assert!(crate::profiles::keep_saved(&path, std::slice::from_ref(&saved)).is_ok());
        write("rf_power", &[0x02, 0x55]);
        let said = lab.put_on("VARA", "VarAC.exe");
        assert!(said.contains("saved in LabLog, version #4"), "{said}");
        assert_eq!(setting_now(&shared, "rf_power").as_deref(), Some("00 69"));
        // What the Agent put on is what it knows now: nothing changed since.
        let crate::engine::ReadAnswer::Read {
            changed,
            profile_from,
            ..
        } = lab.read_for_lablog("VARA")
        else {
            unreachable!("the pretend radio can be read")
        };
        assert_eq!(profile_from, "saved in LabLog, version #4");
        assert!(!changed.contains(&"rf_power".to_owned()), "{changed:?}");
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
        let records = std::env::temp_dir().join(format!("lablog-locked-{}", std::process::id()));
        let locked = lab_locked(&records, &definition, "vara").unwrap_or_default();
        assert!(
            locked.contains("the lab's locked profile for the IC-7300"),
            "{locked}"
        );
        assert!(locked.contains("keep My VARA"));
        assert!(locked.contains("Lab Profiles"), "{locked}");
        assert_eq!(lab_locked(&records, &definition, "My VARA"), None);
        assert_eq!(lab_locked(&records, &definition, "Field Day"), None);
        let now = shared
            .settings(|radio| settings_record::read(radio, Some("PRETEND"), UNIX_EPOCH, "test"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        let plan = lab_plan(&records, &now, "VARA", &definition);
        let put: Vec<String> = plan
            .iter()
            .flat_map(|plan| plan.put.iter().map(|put| put.setting.id.clone()))
            .collect();
        assert!(put.contains(&"filter_width".to_owned()), "{put:?}");
        assert!(put.contains(&"usb_mod_level".to_owned()), "{put:?}");
        assert!(lab_plan(&records, &now, "My VARA", &definition).is_none());
    }

    /// Lab Stock kept as a test Engine would keep it: one approved, and the
    /// proposals sent.
    struct StockEngine {
        stock: Option<crate::engine::LabStock>,
        proposed: Arc<Mutex<Vec<Vec<crate::engine::StockSetting>>>>,
    }

    impl crate::setups::Setups for StockEngine {
        fn keep(&self, _: &settings_record::Record, _: &str) -> Result<(), String> {
            Err("not kept here".to_owned())
        }

        fn kept(&self, _: &str, _: &str, _: &str) -> Result<KeptRadio, String> {
            Err("not kept here".to_owned())
        }

        fn place(&self) -> String {
            "on the test Engine".to_owned()
        }

        fn lab_stock(&self, _: &str, _: &str) -> Result<Option<crate::engine::LabStock>, String> {
            Ok(self.stock.clone())
        }

        fn propose_lab_stock(
            &self,
            _: &settings_record::Record,
            settings: &[crate::engine::StockSetting],
        ) -> Result<u64, String> {
            let mut proposed = self.proposed.lock().map_err(|e| e.to_string())?;
            proposed.push(settings.to_vec());
            Ok(7)
        }
    }

    /// Types `lines` at the console of the pretend IC-7300, auto mode on,
    /// with `engine` keeping Lab Stock; returns the desk.
    fn typed_at_console(
        shared: &Shared,
        definition: &rigdef::Definition,
        engine: &dyn crate::setups::Setups,
        lines: &[&str],
    ) -> Desk {
        let desk = Desk {
            waiting: Arc::new(Mutex::new(None)),
            asked: Arc::new(AtomicU32::new(0)),
            auto: Arc::new(AtomicBool::new(true)),
            serial: "PRETEND".to_owned(),
        };
        let (send, receive) = std::sync::mpsc::channel();
        for line in lines {
            let _ = send.send((*line).to_owned());
        }
        drop(send);
        let checks = Arc::new(RwLock::new(definition.clone()));
        super::console(shared, engine, &checks, &desk, &receive);
        desk
    }

    /// An empty folder of its own for a test.
    fn test_folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lablog-alone-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn standing_alone_stock_save_saves_the_lab_stock_here_after_a_yes() {
        use crate::setups::Setups as _;
        let dir = test_folder("stock");
        let local = crate::setups::Local::standalone(dir.clone());
        let (shared, definition) = transmitted([0, 0, 0]);
        let desk = typed_at_console(&shared, &definition, &local, &["stock"]);
        assert!(
            desk.waiting.lock().is_ok_and(|slot| slot.is_none()),
            "no Lab Stock yet, so nothing to put on"
        );
        let _ = typed_at_console(&shared, &definition, &local, &["stock save", "no"]);
        assert!(
            local
                .lab_stock("Icom", "IC-7300")
                .is_ok_and(|s| s.is_none()),
            "no yes, nothing saved"
        );
        let _ = typed_at_console(&shared, &definition, &local, &["stock save", "yes"]);
        let saved = local.lab_stock("Icom", "IC-7300");
        let Ok(Some(stock)) = saved else {
            unreachable!("saved after the yes: {saved:?}")
        };
        assert_eq!(stock.id, 1);
        assert!(stock.settings.iter().any(|s| s.id == "data_mod"));
        // Putting it on waits for a yes, as LabLog's does.
        let desk = typed_at_console(&shared, &definition, &local, &["stock"]);
        let waiting = desk.waiting.lock().ok().and_then(|mut slot| slot.take());
        let Some((1, super::Asked::Change(plan))) = waiting else {
            unreachable!("the Lab Stock waits for a yes")
        };
        assert_eq!(plan.setup, "Lab Stock #1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_write_check_counts_at_once_for_everything_the_station_does() {
        let definition = rigdef::built_in()
            .ok()
            .and_then(|all| all.into_iter().find(|d| d.model == "IC-9700"))
            .unwrap_or_else(|| unreachable!("the IC-9700 is built in"));
        assert!(crate::write_check::unchecked(&definition));
        let radio = CivRadio::find(
            PretendPort::new(PretendCiv::new(&definition), 115_200),
            std::slice::from_ref(&definition),
        )
        .unwrap_or_else(|error| unreachable!("{error}"));
        let shared = Shared::with_transmit(Box::new(radio), Duration::from_mins(1));
        let before = shared
            .settings(|radio| settings_record::read(radio, Some("NEW"), UNIX_EPOCH, "FL-1"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        let checks = Arc::new(RwLock::new(definition));
        // As the programs' side of the station holds it.
        let programs_see = Arc::clone(&checks);
        let dir = test_folder("first-check");
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| unreachable!("{error}"));
        let lines = super::check_here(&shared, &checks, "NEW", &dir);
        assert!(lines[0].starts_with("Wrote back"), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.starts_with("Recorded in")),
            "{lines:?}"
        );
        assert!(!crate::write_check::unchecked(&super::now_in(
            &programs_see
        )));
        let after = shared
            .settings(|radio| settings_record::read(radio, Some("NEW"), UNIX_EPOCH, "FL-1"))
            .unwrap_or_else(|why| unreachable!("{why}"));
        assert_eq!(
            before.settings, after.settings,
            "nothing changed on the radio"
        );
        // Kept, it counts again when the station next starts.
        let again = crate::write_check::with_checks_here(&super::now_in(&checks), &dir);
        assert_eq!(again, super::now_in(&checks));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_save_works_only_standing_alone_and_waits_for_a_yes() {
        assert_eq!(typed("profile save vara"), Ok(Typed::ProfileSave("VARA")));
        assert_eq!(
            typed("Profile SAVE fldigi"),
            Ok(Typed::ProfileSave("fldigi"))
        );
        assert!(typed("profile save Winlink").is_err());
        assert!(typed("profile").is_err());
        let dir = test_folder("profile");
        let (shared, definition) = transmitted([0, 0, 0]);
        let read = || {
            shared
                .settings(|radio| settings_record::read(radio, Some("PRETEND"), UNIX_EPOCH, "FL-1"))
        };
        let desk = Desk {
            waiting: Arc::new(Mutex::new(None)),
            asked: Arc::new(AtomicU32::new(0)),
            auto: Arc::new(AtomicBool::new(true)),
            serial: "PRETEND".to_owned(),
        };
        let lab = crate::setups::Local::new(dir.clone());
        let said = super::profile_save(&lab, &desk, &definition, &dir, "VARA", read);
        assert!(said.contains("LabLog"), "{said}");
        assert!(desk.waiting.lock().is_ok_and(|slot| slot.is_none()));

        let alone = crate::setups::Local::standalone(dir.clone());
        let said = super::profile_save(&alone, &desk, &definition, &dir, "VARA", read);
        assert!(said.contains("built into the Agent now"), "{said}");
        let waiting = desk.waiting.lock().ok().and_then(|mut slot| slot.take());
        let Some((
            1,
            super::Asked::ProfileSave {
                program,
                record,
                settings,
            },
        )) = waiting
        else {
            unreachable!("saving a profile waits for a yes, auto mode or not")
        };
        assert_eq!(program, "VARA");
        let built_in: Vec<String> = crate::profiles::find("Icom", "IC-7300", "VARA")
            .map(|p| p.ids().map(str::to_owned).collect())
            .unwrap_or_default();
        let ids: Vec<String> = settings.iter().map(|s| s.id.clone()).collect();
        assert_eq!(
            ids, built_in,
            "the VARA profile's own settings, in its order"
        );
        let said = super::profile_saved(&definition, &dir, program, &record, settings);
        assert!(said.contains("version #1"), "{said}");
        let now = crate::profiles::current(&dir, &definition, "VARA");
        assert_eq!(
            now.map(|p| p.origin()).as_deref(),
            Some("saved on this computer, version #1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lab_stock_waits_for_a_yes_even_in_auto_mode_and_stock_save_proposes_it() {
        assert_eq!(typed("stock"), Ok(Typed::Stock));
        assert_eq!(typed("Stock SAVE"), Ok(Typed::StockSave));
        assert!(typed("stock now").is_err());
        let (shared, definition) = transmitted([0, 0, 0]);
        let before = setting_now(&shared, "data_mod");
        let stock = crate::engine::LabStock {
            id: 3,
            maker: "Icom".to_owned(),
            model: "IC-7300".to_owned(),
            settings: vec![crate::engine::StockSetting {
                id: "data_mod".to_owned(),
                label: "DATA MOD".to_owned(),
                bytes: if before.as_deref() == Some("02") {
                    "03"
                } else {
                    "02"
                }
                .to_owned(),
                shown: None,
            }],
        };
        let engine = StockEngine {
            stock: Some(stock),
            proposed: Arc::new(Mutex::new(Vec::new())),
        };
        let desk = typed_at_console(&shared, &definition, &engine, &["stock"]);
        let waiting = desk.waiting.lock().ok().and_then(|mut slot| slot.take());
        let Some((1, super::Asked::Change(plan))) = waiting else {
            unreachable!("the Lab Stock waits for a yes, auto mode or not")
        };
        assert_eq!(plan.setup, "Lab Stock #3");
        assert_eq!(plan.put.len(), 1);
        assert_eq!(
            setting_now(&shared, "data_mod"),
            before,
            "nothing written yet"
        );

        // Sending reads each filter's width, which chooses each filter: a
        // yes first.
        let selected = setting_now(&shared, "data_mode_and_filter");
        let desk = typed_at_console(&shared, &definition, &engine, &["stock save", "no"]);
        assert!(desk.waiting.lock().is_ok_and(|slot| slot.is_none()));
        assert!(
            engine.proposed.lock().is_ok_and(|p| p.is_empty()),
            "no yes, nothing sent"
        );
        let _ = typed_at_console(&shared, &definition, &engine, &["stock save", "yes"]);
        let proposed = engine
            .proposed
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default();
        assert_eq!(proposed.len(), 1);
        let ids: Vec<&str> = proposed[0].iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"data_mod"), "{ids:?}");
        assert_eq!(
            ids[ids.len() - 3..],
            [
                "filter_width_fil1",
                "filter_width_fil2",
                "filter_width_fil3"
            ]
        );
        assert_eq!(
            setting_now(&shared, "data_mode_and_filter"),
            selected,
            "back on the filter it was on"
        );
        assert!(
            proposed[0]
                .iter()
                .all(|s| s.id.starts_with("filter_width_fil")
                    || definition
                        .settings
                        .iter()
                        .any(|d| d.id == s.id && d.write && d.write_checked && !d.moves)),
            "only settings the Agent may write"
        );

        let none = StockEngine {
            stock: None,
            proposed: Arc::new(Mutex::new(Vec::new())),
        };
        let desk = typed_at_console(&shared, &definition, &none, &["stock"]);
        assert!(desk.waiting.lock().is_ok_and(|slot| slot.is_none()));
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
