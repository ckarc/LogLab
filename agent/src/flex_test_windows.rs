// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`'s tray on Windows: an icon by the clock, a menu with Quit, and
//! a notice each time SmartSDR or a station app opens or closes on this
//! computer (see `flex_test`). The icon is grey while SmartSDR is closed and
//! green while it is open. Each notice is also written, with the time in UTC, to
//! `FlexTest.log` beside the program.
//!
//! Milestone 1 of the sound card plan: while it runs, it keeps the Agent's
//! own sound card, "LabLog Radio", plugged in (`sound_card_windows::Card`),
//! with no radio behind it yet: what apps play to it goes nowhere, and its
//! input is silence. Its menu ticks "LabLog Radio plugged in" while it is, and
//! can try again if it isn't. Quit unplugs it.
//!
//! Milestone 2: ticking "Use FlexRadio" finds the FlexRadio that SmartSDR
//! on this computer uses and puts its slice's receive audio into the
//! card's input (`flex_rx`). Unticking it, Quit, or losing the radio stops
//! the audio and puts the slice back. It is unticked at every start and
//! never ticks itself. Once a minute, while on, the log gets the audio's
//! level and how the buffer is doing.
//!
//! Rig control: while "Use FlexRadio" is on, programs can also control the
//! slice through Hamlib's `rigctld` protocol on 127.0.0.1:4532 (WSJT-X:
//! Rig "Hamlib NET rigctl", PTT method CAT): frequency, mode, and keying
//! the transmitter, with the Agent's PTT rules (`rigctld_server::Shared`:
//! one program keyed at a time, unkeyed if it goes, at most a minute at
//! once). The connection that does it is bound to SmartSDR on this computer
//! and keeps to its slice only (`flex::FlexRadio::bind_to`). While on, the
//! transmitter takes its audio from DAX (FlexTest's own stream), as
//! SmartSDR's DAX button shows; unticking puts that back as it was.
//!
//! The icon, menu and notices come from `native-windows-gui` (Windows'
//! `Shell_NotifyIconW` and popup menus), as in the Agent's own tray. Windows'
//! `tasklist` is asked which programs run, started with `CREATE_NO_WINDOW`
//! (Microsoft Learn, "Process Creation Flags") so no console window flashes
//! up each time.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use native_windows_gui as nwg;

use crate::flex::FlexRadio;
use crate::flex_find::{LISTEN, discover, here_toward};
use crate::flex_rx::{FlexRx, pick};
use crate::flex_test::{LOOK_EVERY_SECONDS, Seen, Watch, first_notice, running};
use crate::rigctld_server::{Server, Shared};
use crate::sound_card::{Feed, NAME as CARD};
use crate::sound_card_windows::Card;

/// Microsoft Learn, "Process Creation Flags": the process is a console
/// application that is being run without a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The name its icon, notices and boxes show.
const TITLE: &str = "FlexTest";
/// Its log's file name, beside it.
const LOG: &str = "FlexTest.log";
/// Windows' tooltip holds at most 127 characters.
const TIP_MOST: usize = 120;
/// The icons' colours: grey while SmartSDR is closed, green while open
/// (the Agent tray's waiting and receiving colours).
const CLOSED_COLOUR: [u8; 3] = [0x8a, 0x8a, 0x8a];
const OPEN_COLOUR: [u8; 3] = [0x2e, 0xa0, 0x43];

/// One look's answer: the program files' names running, or why it failed.
type Look = Result<Vec<String>, String>;

/// The program files' names running on this computer, asking `tasklist`.
fn running_now() -> Look {
    let answer = Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("can't list running programs: {error}"))?;
    Ok(running(&String::from_utf8_lossy(&answer.stdout)))
}

/// Holds a file beside FlexTest that no other process may open while it
/// runs, so only one runs at once. Windows lets go of it when FlexTest
/// ends, however it ends.
fn only_one(folder: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(folder.join("FlexTest.lock"))
        .map_err(|_| "FlexTest is already running: look for its icon by the clock.".to_owned())
}

fn built(result: Result<(), nwg::NwgError>) -> Result<(), String> {
    result.map_err(|error| format!("the tray couldn't be made: {error}"))
}

/// Writes an icon of `colour` where Windows can load it, and loads it.
fn icon(name: &str, colour: [u8; 3]) -> Result<nwg::Icon, String> {
    let folder = std::env::temp_dir().join("flextest-icons");
    std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let path = folder.join(format!("{name}.ico"));
    std::fs::write(&path, crate::tray::icon_file(colour))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut icon = nwg::Icon::default();
    built(
        nwg::Icon::builder()
            .source_file(Some(&path.to_string_lossy()))
            .strict(true)
            .build(&mut icon),
    )?;
    Ok(icon)
}

/// The tray's icon and menu.
#[derive(Default)]
struct Face {
    window: nwg::MessageWindow,
    wake: nwg::Notice,
    tray: nwg::TrayNotification,
    closed: nwg::Icon,
    open: nwg::Icon,
    menu: nwg::Menu,
    card: nwg::MenuItem,
    replug: nwg::MenuItem,
    flex: nwg::MenuItem,
    quit: nwg::MenuItem,
}

impl Face {
    fn build() -> Result<Self, String> {
        let mut face = Self {
            closed: icon("closed", CLOSED_COLOUR)?,
            open: icon("open", OPEN_COLOUR)?,
            ..Self::default()
        };
        built(nwg::MessageWindow::builder().build(&mut face.window))?;
        built(
            nwg::Notice::builder()
                .parent(&face.window)
                .build(&mut face.wake),
        )?;
        built(
            nwg::TrayNotification::builder()
                .parent(&face.window)
                .icon(Some(&face.closed))
                .tip(Some(&format!("{TITLE}: starting")))
                .build(&mut face.tray),
        )?;
        built(
            nwg::Menu::builder()
                .popup(true)
                .parent(&face.window)
                .build(&mut face.menu),
        )?;
        built(
            nwg::MenuItem::builder()
                .text(&format!("{CARD} plugged in"))
                .disabled(true)
                .parent(&face.menu)
                .build(&mut face.card),
        )?;
        built(
            nwg::MenuItem::builder()
                .text(&format!("Plug in {CARD} again"))
                .disabled(true)
                .parent(&face.menu)
                .build(&mut face.replug),
        )?;
        built(
            nwg::MenuItem::builder()
                .text("Use FlexRadio")
                .parent(&face.menu)
                .build(&mut face.flex),
        )?;
        built(
            nwg::MenuItem::builder()
                .text("Quit")
                .parent(&face.menu)
                .build(&mut face.quit),
        )?;
        Ok(face)
    }

    /// Shows `text` as a notice and as the tip, with the icon for whether
    /// SmartSDR is open.
    fn show(&self, text: &str, smartsdr_open: bool) {
        self.tray.set_icon(if smartsdr_open {
            &self.open
        } else {
            &self.closed
        });
        let mut tip = format!("{TITLE}\n{text}");
        if tip.chars().count() > TIP_MOST {
            tip = tip.chars().take(TIP_MOST).collect();
        }
        self.tray.set_tip(&tip);
        self.notify(text);
    }

    fn notify(&self, text: &str) {
        self.tray.show(
            text,
            Some(TITLE),
            Some(nwg::TrayNotificationFlags::INFO_ICON),
            None,
        );
    }
}

/// Plugs in the card carrying `feed`, says how that went in the menu, and
/// returns it and what to tell the user.
fn plug_in(face: &Face, feed: &Arc<Feed>) -> (Option<Card>, String) {
    match Card::plug_in(Arc::clone(feed)) {
        Ok(card) => {
            face.card.set_checked(true);
            face.replug.set_enabled(false);
            let said = format!(
                "{CARD} is plugged in (usbip port {}): apps can pick \"Speakers ({CARD})\" and \"Line ({CARD})\".",
                card.port().unwrap_or_default()
            );
            (Some(card), said)
        }
        Err(why) => {
            face.card.set_checked(false);
            face.replug.set_enabled(true);
            (None, format!("Couldn't plug in {CARD}: {why}"))
        }
    }
}

/// Unplugs the card; what to log.
fn unplug(mut card: Card) -> String {
    match card.unplug() {
        Ok(()) => format!("{CARD} unplugged."),
        Err(why) => format!("Couldn't unplug {CARD}: {why}"),
    }
}

/// Looks at what runs every few seconds, on a thread of its own, adding
/// each answer to `answers` and waking the tray.
fn look_for_programs(answers: Arc<Mutex<Vec<Look>>>, wake: nwg::NoticeSender) {
    std::thread::spawn(move || {
        loop {
            let answer = running_now();
            if let Ok(mut answers) = answers.lock() {
                answers.push(answer);
            }
            wake.notice();
            std::thread::sleep(Duration::from_secs(LOOK_EVERY_SECONDS));
        }
    });
}

/// Where programs reach the slice's rig control.
const RIG_AT: ([u8; 4], u16) = ([127, 0, 0, 1], 4532);
/// How long a program may keep the radio keyed at once (the Agent's
/// default).
const TX_LIMIT: Duration = Duration::from_mins(1);

/// Programs' rig control of the slice, while "Use FlexRadio" is on.
struct Rig {
    /// Dropped first: no program can reach the radio after.
    server: Server,
    /// What programs did, for the log.
    notes: Receiver<String>,
}

/// Opens a second connection to the radio, bound to SmartSDR's client
/// like the audio's and kept to its slice, and serves it to programs.
fn serve_rig(radio: SocketAddr, rx: &FlexRx) -> Result<Rig, String> {
    let mut flex = FlexRadio::open(&radio.to_string()).map_err(|error| error.0)?;
    flex.bind_to(&rx.client_id, rx.slice.number)
        .map_err(|error| error.0)?;
    let (notes, heard) = mpsc::channel();
    let shared = Arc::new(Shared::with_transmit(Box::new(flex), TX_LIMIT).with_notes(notes));
    let server = Server::start(SocketAddr::from(RIG_AT), shared)?;
    Ok(Rig {
        server,
        notes: heard,
    })
}

/// The radio's audio starting, or why it couldn't, with the radio's model,
/// and programs' rig control or why it couldn't start.
type Started = Result<(FlexRx, Result<Rig, String>, String), String>;

/// Finds the radio and starts its audio and rig control, on a thread of its
/// own, then wakes the tray with the outcome.
fn start_flex(into: Arc<Mutex<Vec<Started>>>, wake: nwg::NoticeSender) {
    std::thread::spawn(move || {
        let started = discover(LISTEN)
            .and_then(|heard| pick(&heard, here_toward))
            .and_then(|(radio, owner, model)| {
                FlexRx::start(radio, owner).map(|rx| {
                    let rig = serve_rig(radio, &rx);
                    (rx, rig, model)
                })
            });
        if let Ok(mut into) = into.lock() {
            into.push(started);
        }
        wake.notice();
    });
}

/// Stops programs' rig control (unkeying the radio if a program has it
/// keyed), then the audio, putting the slice back: what was done, for the
/// log.
fn stop_both(rx: impl Into<Box<FlexRx>>, rig: Option<Rig>) -> String {
    let rx = *rx.into();
    let mut done = Vec::new();
    if let Some(rig) = rig {
        drop(rig.server);
        done.extend(rig.notes.try_iter());
        done.push("Rig control stopped.".to_owned());
    }
    done.extend(rx.stop());
    done.join("\n")
}

/// How often the log hears how the radio's audio is doing.
const STATUS_EVERY: Duration = Duration::from_mins(1);

/// Where "Use FlexRadio" stands.
enum Flex {
    Off,
    Starting,
    On {
        rx: Box<FlexRx>,
        rig: Option<Rig>,
        told: std::time::Instant,
    },
}

/// Everything the tray keeps between events.
struct Tray {
    face: Rc<Face>,
    log_file: Option<File>,
    feed: Arc<Feed>,
    card: Option<Card>,
    /// Said with the first look's notice, which would otherwise replace it.
    card_said: Option<String>,
    watch: Watch,
    last_error: String,
    answers: Arc<Mutex<Vec<Look>>>,
    flex: Flex,
    started: Arc<Mutex<Vec<Started>>>,
}

impl Tray {
    fn log(&mut self, text: &str) {
        for line in text.lines() {
            log(self.log_file.as_mut(), line);
        }
    }

    fn say(&mut self, text: &str) {
        self.face.notify(text);
        self.log(text);
    }

    fn replug(&mut self) {
        let (card, said) = plug_in(&self.face, &self.feed);
        self.card = card;
        self.say(&said);
    }

    /// "Use FlexRadio" clicked: ticks it and starts, or stops and unticks.
    fn flex_clicked(&mut self) {
        match self.flex {
            Flex::Off => {
                self.flex = Flex::Starting;
                self.face.flex.set_checked(true);
                self.say("Looking for the FlexRadio SmartSDR uses on this computer...");
                start_flex(Arc::clone(&self.started), self.face.wake.sender());
            }
            // Its outcome is stopped as soon as it arrives.
            Flex::Starting => {
                self.flex = Flex::Off;
                self.face.flex.set_checked(false);
            }
            Flex::On { .. } => self.stop_flex("Stopped using the FlexRadio."),
        }
    }

    fn stop_flex(&mut self, why: &str) {
        self.feed.disconnect();
        if let Flex::On { rx, rig, .. } = std::mem::replace(&mut self.flex, Flex::Off) {
            let done = stop_both(rx, rig);
            self.log(&done);
        }
        self.flex = Flex::Off;
        self.face.flex.set_checked(false);
        self.say(why);
    }

    fn take_started(&mut self) {
        let taken: Vec<_> = self
            .started
            .lock()
            .map(|mut started| started.drain(..).collect())
            .unwrap_or_default();
        for started in taken {
            match (started, &self.flex) {
                (Ok((rx, rig, model)), Flex::Starting) => {
                    self.feed
                        .connect(Arc::clone(&rx.buffer), Arc::clone(&rx.transmit));
                    self.log(&format!(
                        "Flex audio streams: receive 0x{:08X}, transmit 0x{:08X}; slice {} (was on DAX channel {}; transmit DAX was {}).",
                        rx.streams.rx, rx.streams.tx, rx.slice.number, rx.slice.dax_before, rx.slice.tx_dax_before
                    ));
                    let control = match &rig {
                        Ok(rig) => format!(
                            " Programs control it at {} (WSJT-X: Rig \"Hamlib NET rigctl\", PTT method CAT).",
                            rig.server.address()
                        ),
                        Err(why) => format!(" Rig control couldn't start: {why}"),
                    };
                    let said = format!(
                        "Hearing slice {} of the {model} in \"Line ({CARD})\"; what apps play to \"Speakers ({CARD})\" goes out when it transmits.{control}",
                        rx.slice.letter
                    );
                    self.flex = Flex::On {
                        rx: Box::new(rx),
                        rig: rig.ok(),
                        told: std::time::Instant::now(),
                    };
                    self.say(&said);
                }
                (Ok((rx, rig, _)), _) => {
                    let done = stop_both(rx, rig.ok());
                    self.log(&done);
                }
                (Err(why), Flex::Starting) => {
                    self.flex = Flex::Off;
                    self.face.flex.set_checked(false);
                    self.say(&format!("Couldn't use the FlexRadio: {why}"));
                }
                (Err(_), _) => {}
            }
        }
    }

    /// While the radio's audio is on: stops if the radio went, and now and
    /// then logs how it is doing.
    fn check_flex(&mut self) {
        let Flex::On { rig, .. } = &mut self.flex else {
            return;
        };
        let notes: Vec<String> = rig
            .as_ref()
            .map(|rig| rig.notes.try_iter().collect())
            .unwrap_or_default();
        for note in notes {
            self.log(&format!("Rig control: {note}"));
        }
        let Flex::On { rx, told, .. } = &mut self.flex else {
            return;
        };
        if !rx.connected() {
            self.stop_flex("Lost the FlexRadio: its audio stopped.");
            return;
        }
        if told.elapsed() >= STATUS_EVERY {
            *told = std::time::Instant::now();
            let peak = rx
                .buffer
                .take_peak_dbfs()
                .map_or_else(|| "silent".to_owned(), |db| format!("peak {db:.1} dBFS"));
            let tx_peak = rx.transmit.take_peak_dbfs().map_or_else(
                || "apps played silence".to_owned(),
                |db| format!("apps' peak {db:.1} dBFS"),
            );
            let listening = if self.feed.recording() {
                "an app is recording"
            } else {
                "no app recording"
            };
            let line = format!(
                "Flex audio: {listening}; {} packets in, {peak}, {} ms waiting, {} dropouts, {} dropped, {} refused; {} packets out, {tx_peak}, {} ms waiting to go",
                rx.counts.packets.load(std::sync::atomic::Ordering::Relaxed),
                rx.buffer.level().as_millis(),
                rx.buffer.gaps(),
                rx.buffer.dropped(),
                rx.counts.refused.load(std::sync::atomic::Ordering::Relaxed),
                rx.counts.sent.load(std::sync::atomic::Ordering::Relaxed),
                rx.transmit.level().as_millis(),
            );
            let levels = format!("Flex levels: {}", rx.levels.describe());
            self.log(&line);
            self.log(&levels);
        }
    }

    fn take_answers(&mut self) {
        let taken: Vec<_> = self
            .answers
            .lock()
            .map(|mut answers| answers.drain(..).collect())
            .unwrap_or_default();
        for answer in taken {
            match answer {
                Ok(names) => {
                    self.last_error.clear();
                    // Several changes in one look make one notice: a newer
                    // notice replaces the one showing.
                    let text = match self.watch.saw(&names) {
                        Seen::First(open) => match self.card_said.take() {
                            Some(said) => format!("{said}\n{}", first_notice(&open)),
                            None => first_notice(&open),
                        },
                        Seen::Changes(changes) => changes
                            .iter()
                            .map(|change| change.notice())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    if !text.is_empty() {
                        self.face.show(&text, self.watch.smartsdr_open());
                        self.log(&text);
                    }
                }
                // Said once, not every two seconds.
                Err(why) if self.last_error != why => {
                    self.say(&why);
                    self.last_error = why;
                }
                Err(_) => {}
            }
        }
        self.check_flex();
    }

    fn quit(&mut self) {
        if !matches!(self.flex, Flex::Off) {
            self.feed.disconnect();
            if let Flex::On { rx, rig, .. } = std::mem::replace(&mut self.flex, Flex::Off) {
                let done = stop_both(rx, rig);
                self.log(&done);
            }
        }
        if let Some(said) = self.card.take().map(unplug) {
            self.log(&said);
        }
        self.log("FlexTest quit.");
    }
}

/// Adds `text` to the log, after the time in UTC.
fn log(file: Option<&mut File>, text: &str) {
    if let Some(file) = file {
        let now = crate::settings_record::utc_text(SystemTime::now());
        let _ = writeln!(file, "{now}  {text}");
    }
}

/// Runs the tray until Quit.
///
/// # Errors
///
/// FlexTest is already running, or Windows wouldn't make the tray.
pub fn run() -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("can't tell where it is: {error}"))?;
    let folder = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    built(nwg::init())?;
    let _only = match only_one(&folder) {
        Ok(lock) => lock,
        Err(why) => {
            nwg::simple_message(TITLE, &why);
            return Err(why);
        }
    };
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(LOG))
        .ok();
    let face = Rc::new(Face::build()?);
    let feed = Arc::new(Feed::default());
    let mut tray = Tray {
        face: Rc::clone(&face),
        log_file,
        feed,
        card: None,
        card_said: None,
        watch: Watch::default(),
        last_error: String::new(),
        answers: Arc::default(),
        flex: Flex::Off,
        started: Arc::default(),
    };
    tray.log("FlexTest started.");
    // Logged with the first look's notice.
    let (card, card_said) = plug_in(&face, &tray.feed);
    tray.card = card;
    tray.card_said = Some(card_said);

    // Looks for SmartSDR on its own thread, so a slow `tasklist` never
    // holds up the tray, and wakes the tray with each answer.
    look_for_programs(Arc::clone(&tray.answers), face.wake.sender());

    let tray = RefCell::new(tray);
    let handler_face = Rc::clone(&face);
    let handler = nwg::full_bind_event_handler(&face.window.handle, move |event, _data, handle| {
        let face = &handler_face;
        // Borrowed only while one event is handled: the menu's popup runs
        // its own message loop, in which other events arrive. One that
        // finds it already borrowed waits for the next notice.
        let Ok(mut tray) = tray.try_borrow_mut() else {
            return;
        };
        match event {
            nwg::Event::OnContextMenu if handle == face.tray.handle => {
                drop(tray);
                let (x, y) = nwg::GlobalCursor::position();
                face.menu.popup(x, y);
            }
            nwg::Event::OnMenuItemSelected if handle == face.replug.handle => tray.replug(),
            nwg::Event::OnMenuItemSelected if handle == face.flex.handle => tray.flex_clicked(),
            nwg::Event::OnMenuItemSelected if handle == face.quit.handle => {
                tray.quit();
                nwg::stop_thread_dispatch();
            }
            nwg::Event::OnNotice => {
                tray.take_started();
                tray.take_answers();
            }
            _ => {}
        }
    });
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}
