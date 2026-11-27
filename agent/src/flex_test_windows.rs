// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `FlexTest`'s tray on Windows: an icon by the clock, a menu with Quit, and
//! a notice each time SmartSDR or a station app opens or closes on this
//! computer (see `flex_test`). The icon is grey while SmartSDR is closed and
//! green while it is open. Each notice is also written, with the time in UTC, to
//! `FlexTest.log` beside the program.
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

use crate::flex_test::{LOOK_EVERY_SECONDS, Seen, Watch, first_notice, running};

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

    // Looks for SmartSDR on its own thread, so a slow `tasklist` never
    // holds up the tray, and wakes the tray with each answer.
    let answers: Arc<Mutex<Vec<Look>>> = Arc::default();
    let (to_tray, wake) = (Arc::clone(&answers), face.wake.sender());
    std::thread::spawn(move || {
        loop {
            let answer = running_now();
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
                        Ok(names) => {
                            last_error.borrow_mut().clear();
                            let mut watch = watch.borrow_mut();
                            // Several changes in one look make one notice:
                            // a newer notice replaces the one showing.
                            let text = match watch.saw(&names) {
                                Seen::First(open) => first_notice(&open),
                                Seen::Changes(changes) => changes
                                    .iter()
                                    .map(|change| change.notice())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            };
                            if !text.is_empty() {
                                face.tray.set_icon(if watch.smartsdr_open() {
                                    &face.open
                                } else {
                                    &face.closed
                                });
                                let mut tip = format!("{TITLE}\n{text}");
                                if tip.chars().count() > TIP_MOST {
                                    tip = tip.chars().take(TIP_MOST).collect();
                                }
                                face.tray.set_tip(&tip);
                                face.notify(&text);
                                for line in text.lines() {
                                    log(log_file.borrow_mut().as_mut(), line);
                                }
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
