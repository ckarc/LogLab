// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`'s tray on Windows: an icon by the clock, a menu with Quit, and
//! a notice each time SmartSDR opens or closes on this computer (see
//! `flex_test`). The icon is grey while SmartSDR is closed and green while
//! it is open. Each notice is also written, with the time in UTC, to
//! `FlexTest.log` beside the program.
//!
//! While it runs it also keeps the LabLog Radio sound card attached to
//! Windows (see `sound_card`), and detaches it at Quit.
//!
//! The icon, menu and notices come from `native-windows-gui` (Windows'
//! `Shell_NotifyIconW` and popup menus), as in the Agent's own tray. Windows'
//! `tasklist` is asked which programs run, started with `CREATE_NO_WINDOW`
//! (Microsoft Learn, "Process Creation Flags") so no console window flashes
//! up each time.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use native_windows_gui as nwg;

use crate::flex_test::{LOOK_EVERY_SECONDS, SMARTSDR, Watch, listed};
use crate::sound_card::NAME;
use crate::sound_card_windows::SoundCard;

/// Microsoft Learn, "Process Creation Flags": the process is a console
/// application that is being run without a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The name its icon, notices and boxes show.
const TITLE: &str = "FlexTest";
/// Its log's file name, beside it.
const LOG: &str = "FlexTest.log";
/// The icons' colours: grey while SmartSDR is closed, green while open
/// (the Agent tray's waiting and receiving colours).
const CLOSED_COLOUR: [u8; 3] = [0x8a, 0x8a, 0x8a];
const OPEN_COLOUR: [u8; 3] = [0x2e, 0xa0, 0x43];

/// Whether SmartSDR is running on this computer, asking `tasklist`.
fn smartsdr_running() -> Result<bool, String> {
    let answer = Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {SMARTSDR}"),
            "/NH",
            "/FO",
            "CSV",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("can't list running programs: {error}"))?;
    Ok(listed(&String::from_utf8_lossy(&answer.stdout), SMARTSDR))
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
                .text("Quit")
                .parent(&face.menu)
                .build(&mut face.quit),
        )?;
        Ok(face)
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

/// Adds `text` to the log, after the time in UTC.
fn log(file: Option<&mut File>, text: &str) {
    if let Some(file) = file {
        let now = crate::settings_record::utc_text(SystemTime::now());
        let _ = writeln!(file, "{now}  {text}");
    }
}

/// Attaches the LabLog Radio sound card, and says so; or says why not.
fn attach_card() -> Result<(SoundCard, String), String> {
    match SoundCard::start() {
        Ok(card) => {
            let text = format!(
                "The {NAME} sound card is attached (usbip port {}).",
                card.port()
            );
            Ok((card, text))
        }
        Err(why) => Err(format!("The {NAME} sound card couldn't be attached: {why}")),
    }
}

/// Detaches the LabLog Radio sound card, and says how it went.
fn detach_card(card: SoundCard) -> String {
    let (played, recorded) = card.frames();
    match card.stop() {
        Ok(()) => format!(
            "The {NAME} sound card is detached ({played} frames played into it, {recorded} recorded from it)."
        ),
        Err(why) => format!("The {NAME} sound card couldn't be detached: {why}"),
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
    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(LOG))
        .ok();
    log(log_file.as_mut(), "FlexTest started.");
    let face = Rc::new(Face::build()?);

    // The LabLog Radio sound card, attached for as long as FlexTest runs.
    let card = attach_card();
    let said = card
        .as_ref()
        .map_or_else(Clone::clone, |(_, text)| text.clone());
    log(log_file.as_mut(), &said);
    face.notify(&said);
    let card = card.ok().map(|(card, _)| card);
    let card = RefCell::new(card);

    // Looks for SmartSDR on its own thread, so a slow `tasklist` never
    // holds up the tray, and wakes the tray with each answer.
    let answers: Arc<Mutex<Vec<Result<bool, String>>>> = Arc::default();
    let (to_tray, wake) = (Arc::clone(&answers), face.wake.sender());
    std::thread::spawn(move || {
        loop {
            let answer = smartsdr_running();
            if let Ok(mut answers) = to_tray.lock() {
                answers.push(answer);
            }
            wake.notice();
            std::thread::sleep(Duration::from_secs(LOOK_EVERY_SECONDS));
        }
    });

    let watch = RefCell::new(Watch::default());
    let last_error = RefCell::new(String::new());
    let log_file = RefCell::new(log_file);
    let handler_face = Rc::clone(&face);
    let handler = nwg::full_bind_event_handler(&face.window.handle, move |event, _data, handle| {
        let face = &handler_face;
        match event {
            nwg::Event::OnContextMenu if handle == face.tray.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                face.menu.popup(x, y);
            }
            nwg::Event::OnMenuItemSelected if handle == face.quit.handle => {
                if let Some(card) = card.borrow_mut().take() {
                    log(log_file.borrow_mut().as_mut(), &detach_card(card));
                }
                log(log_file.borrow_mut().as_mut(), "FlexTest quit.");
                nwg::stop_thread_dispatch();
            }
            nwg::Event::OnNotice => {
                let taken: Vec<_> = answers
                    .lock()
                    .map(|mut answers| answers.drain(..).collect())
                    .unwrap_or_default();
                for answer in taken {
                    match answer {
                        Ok(open) => {
                            last_error.borrow_mut().clear();
                            if let Some(change) = watch.borrow_mut().saw(open) {
                                let text = change.notice();
                                face.tray.set_icon(if change.open() {
                                    &face.open
                                } else {
                                    &face.closed
                                });
                                face.tray.set_tip(&format!("{TITLE}\n{text}"));
                                face.notify(text);
                                log(log_file.borrow_mut().as_mut(), text);
                            }
                        }
                        Err(why) => {
                            // Said once, not every two seconds.
                            if *last_error.borrow() != why {
                                face.notify(&why);
                                log(log_file.borrow_mut().as_mut(), &why);
                                last_error.borrow_mut().clone_from(&why);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    });
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}
