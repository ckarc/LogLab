// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The tray on Windows: an icon in the taskbar's notification area, its
//! menu, Yes/No boxes and notices, over `station` run with no window.
//!
//! A Windows service can't show anything to the person at the computer
//! (Microsoft Learn, "Interactive Services": "Services cannot directly
//! interact with a user as of Windows Vista"), and the Agent's audio and
//! program settings belong to the person signed in, so the Agent runs as
//! an ordinary program in their sign-in, started by Windows' Run key.
//!
//! Both the tray and its `station` are console programs started with
//! `CREATE_NO_WINDOW`, which Microsoft Learn ("Process Creation Flags")
//! describes as "a console application that is being run without a console
//! window". The icon, menu and notices come from `native-windows-gui`
//! (Windows' `Shell_NotifyIconW` and popup menus); the Yes/No boxes are
//! Windows' own `MessageBoxW`, kept on top of other windows
//! (`MB_TOPMOST`) until answered.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use native_windows_gui as nwg;

use crate::tray::{self, Choice, Grouper, Light, MY_SETUPS, Said};

/// Microsoft Learn, "Process Creation Flags": the process is a console
/// application that is being run without a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Set in the tray's environment once it runs with no window.
const HIDDEN: &str = "LABLOG_TRAY_HIDDEN";
/// The title of the tray's boxes and notices.
const TITLE: &str = "LabLog Station Agent";
/// How long `station` may be quiet before what it said is shown.
const IDLE: Duration = Duration::from_millis(200);
/// How long after `station` stops on its own before it is started again.
const RESTART: Duration = Duration::from_secs(30);
/// The longest log kept before it is moved aside, in bytes.
const LOG_MOST: u64 = 2_000_000;
/// Windows' tooltip holds at most 127 characters.
const TIP_MOST: usize = 120;

/// Starts the tray with no window, with `options` for its `station`.
///
/// # Errors
///
/// Windows wouldn't start it.
pub fn start_hidden(exe: &Path, options: &[String]) -> Result<(), String> {
    Command::new(exe)
        .arg("tray")
        .args(options)
        .env(HIDDEN, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
        .map_err(|error| format!("can't start the tray: {error}"))
}

/// A box on top of every window: a question (Yes or No) or a note (OK).
/// Returns whether the answer was yes.
fn boxed(text: &str, question: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
        MessageBoxW,
    };
    use windows::core::HSTRING;
    let style = if question {
        MB_YESNO | MB_ICONQUESTION
    } else {
        MB_OK | MB_ICONINFORMATION
    } | MB_TOPMOST
        | MB_SETFOREGROUND;
    let (text, title) = (HSTRING::from(text), HSTRING::from(TITLE));
    // A platform interface: Windows' message box has no safe wrapper that
    // keeps it on top of other windows. It takes no owner window and two
    // strings that stay alive, NUL-terminated, until it returns.
    #[allow(unsafe_code)]
    // SAFETY: `text` and `title` are valid wide strings for the whole call,
    // and no window handle is passed.
    let answer = unsafe { MessageBoxW(None, &text, &title, style) };
    answer == IDYES
}

/// What reaches the tray from the threads beside it.
enum Incoming {
    /// Something `station` said.
    Said(String),
    /// `station` stopped, and why.
    Stopped(String),
    /// The operator answered a question.
    Answered { number: u32, yes: bool },
    /// Time to start `station` again.
    Restart,
}

/// The queue to the tray, and how to wake it.
#[derive(Clone)]
struct Inbox {
    queue: Arc<Mutex<VecDeque<Incoming>>>,
    wake: nwg::NoticeSender,
}

impl Inbox {
    fn send(&self, incoming: Incoming) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(incoming);
        }
        self.wake.notice();
    }

    fn take(&self) -> Vec<Incoming> {
        self.queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default()
    }
}

/// `station`, running with no window.
struct Station {
    child: Arc<Mutex<Child>>,
    typing: Option<ChildStdin>,
}

/// Starts `station`, with `options`, and the threads that read it.
fn start_station(
    exe: &Path,
    options: &[String],
    log: &Path,
    inbox: &Inbox,
) -> Result<Station, String> {
    let mut child = Command::new(exe)
        .arg("station")
        .args(options)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("can't start station: {error}"))?;
    let typing = child.stdin.take();
    let (lines, heard) = mpsc::channel::<String>();
    if let Some(out) = child.stdout.take() {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
    }
    drop(lines);
    let child = Arc::new(Mutex::new(child));
    let (watched, to_tray, log) = (Arc::clone(&child), inbox.clone(), log.to_owned());
    std::thread::spawn(move || {
        let mut log = open_log(&log);
        let mut grouper = Grouper::default();
        let mut last = String::new();
        loop {
            match heard.recv_timeout(IDLE) {
                Ok(line) => {
                    if let Some(log) = log.as_mut() {
                        let now = crate::settings_record::utc_text(std::time::SystemTime::now());
                        let _ = writeln!(log, "{now}  {line}");
                    }
                    if let Some(event) = grouper.push(&line) {
                        last.clone_from(&event);
                        to_tray.send(Incoming::Said(event));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(event) = grouper.flush() {
                        last.clone_from(&event);
                        to_tray.send(Incoming::Said(event));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(event) = grouper.flush() {
            last = event;
        }
        let ended = loop {
            let done = watched.lock().map(|mut child| child.try_wait());
            match done {
                Ok(Ok(Some(status))) => break status.to_string(),
                Ok(Ok(None)) => std::thread::sleep(Duration::from_millis(100)),
                Ok(Err(error)) => break error.to_string(),
                Err(_) => break "unknown".to_owned(),
            }
        };
        let why = last.lines().next().unwrap_or("it said nothing").to_owned();
        to_tray.send(Incoming::Stopped(format!("{why} ({ended})")));
    });
    Ok(Station { child, typing })
}

/// The log beside the Agent, opened to add to, after dropping lines older
/// than `tray::LOG_KEEP` and moving a long one aside.
fn open_log(path: &Path) -> Option<File> {
    if let Ok(log) = std::fs::read_to_string(path) {
        let since = std::time::SystemTime::now()
            .checked_sub(tray::LOG_KEEP)
            .map(crate::settings_record::utc_text)
            .unwrap_or_default();
        let kept = tray::log_since(&log, &since);
        if kept.len() != log.len() {
            let _ = std::fs::write(path, kept);
        }
    }
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_MOST) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Holds a file beside the Agent that no other process may open while
/// this tray runs, so only one tray runs at once. Windows lets go of it
/// when the tray ends, however it ends.
fn only_tray(folder: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(folder.join("tray.lock"))
        .map_err(|_| {
            "the Agent's tray is already running: look for its icon by the clock.".to_owned()
        })
}

/// The tray's icon and menu.
#[derive(Default)]
struct Face {
    window: nwg::MessageWindow,
    wake: nwg::Notice,
    icon: nwg::TrayNotification,
    lights: Vec<(Light, nwg::Icon)>,
    menu: nwg::Menu,
    fix: nwg::MenuItem,
    keep_menu: nwg::Menu,
    keep: Vec<nwg::MenuItem>,
    restore_menu: nwg::Menu,
    restore: Vec<nwg::MenuItem>,
    compare: nwg::MenuItem,
    auto: nwg::MenuItem,
    lab_menu: nwg::Menu,
    enroll: nwg::MenuItem,
    leave_out: nwg::MenuItem,
    stock_menu: nwg::Menu,
    stock_put: nwg::MenuItem,
    stock_save: nwg::MenuItem,
    log: nwg::MenuItem,
    quit: nwg::MenuItem,
    separators: Vec<nwg::MenuSeparator>,
}

/// Writes each light's icon where Windows can load it.
fn light_files() -> Result<Vec<(Light, PathBuf)>, String> {
    let folder = std::env::temp_dir().join("lablog-agent-icons");
    std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    Light::ALL
        .iter()
        .map(|&light| {
            let path = folder.join(format!("{}.ico", light.name()));
            std::fs::write(&path, tray::icon_file(light.colour()))
                .map(|()| (light, path.clone()))
                .map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

fn built(result: Result<(), nwg::NwgError>) -> Result<(), String> {
    result.map_err(|error| format!("the tray couldn't be made: {error}"))
}

impl Face {
    fn build() -> Result<Self, String> {
        let mut face = Self::default();
        built(nwg::MessageWindow::builder().build(&mut face.window))?;
        built(
            nwg::Notice::builder()
                .parent(&face.window)
                .build(&mut face.wake),
        )?;
        for (light, path) in light_files()? {
            let mut icon = nwg::Icon::default();
            let path = path.to_string_lossy().into_owned();
            built(
                nwg::Icon::builder()
                    .source_file(Some(&path))
                    .strict(true)
                    .build(&mut icon),
            )?;
            face.lights.push((light, icon));
        }
        let Some((_, first)) = face.lights.first() else {
            return Err("the tray has no icon".to_owned());
        };
        built(
            nwg::TrayNotification::builder()
                .parent(&face.window)
                .icon(Some(first))
                .tip(Some(&format!("{TITLE}: starting")))
                .build(&mut face.icon),
        )?;
        built(
            nwg::Menu::builder()
                .popup(true)
                .parent(&face.window)
                .build(&mut face.menu),
        )?;
        let item = |text: &str, parent: &nwg::Menu, item: &mut nwg::MenuItem| {
            built(
                nwg::MenuItem::builder()
                    .text(text)
                    .parent(parent)
                    .build(item),
            )
        };
        item("Fix radio settings", &face.menu, &mut face.fix)?;
        built(
            nwg::Menu::builder()
                .text("Keep as my setup")
                .parent(&face.menu)
                .build(&mut face.keep_menu),
        )?;
        built(
            nwg::Menu::builder()
                .text("Restore setup")
                .parent(&face.menu)
                .build(&mut face.restore_menu),
        )?;
        for name in MY_SETUPS {
            let mut keep = nwg::MenuItem::default();
            item(name, &face.keep_menu, &mut keep)?;
            face.keep.push(keep);
        }
        for (shown, _) in tray::restore_menu() {
            let mut restore = nwg::MenuItem::default();
            item(&shown, &face.restore_menu, &mut restore)?;
            face.restore.push(restore);
        }
        item("Compare with kept setups", &face.menu, &mut face.compare)?;
        face.separator()?;
        item("Auto mode", &face.menu, &mut face.auto)?;
        built(
            nwg::Menu::builder()
                .text("Lab radio")
                .parent(&face.menu)
                .build(&mut face.lab_menu),
        )?;
        item(
            "Enroll: put the lab's profiles on as programs start",
            &face.lab_menu,
            &mut face.enroll,
        )?;
        item(
            "Leave out: never change it on its own",
            &face.lab_menu,
            &mut face.leave_out,
        )?;
        face.stock_items()?;
        item("Open log", &face.menu, &mut face.log)?;
        face.separator()?;
        item("Quit", &face.menu, &mut face.quit)?;
        Ok(face)
    }

    /// The Lab Stock menu: put the model's Lab Stock on, or send the
    /// radio's settings as a proposal.
    fn stock_items(&mut self) -> Result<(), String> {
        built(
            nwg::Menu::builder()
                .text("Lab Stock")
                .parent(&self.menu)
                .build(&mut self.stock_menu),
        )?;
        for (text, item) in [
            (
                "Put Lab Stock on (shows what changes, then asks)",
                &mut self.stock_put,
            ),
            (
                "Send settings as a Lab Stock proposal",
                &mut self.stock_save,
            ),
        ] {
            built(
                nwg::MenuItem::builder()
                    .text(text)
                    .parent(&self.stock_menu)
                    .build(item),
            )?;
        }
        Ok(())
    }

    fn separator(&mut self) -> Result<(), String> {
        let mut separator = nwg::MenuSeparator::default();
        built(
            nwg::MenuSeparator::builder()
                .parent(&self.menu)
                .build(&mut separator),
        )?;
        self.separators.push(separator);
        Ok(())
    }

    fn show_light(&self, light: Light) {
        if let Some((_, icon)) = self.lights.iter().find(|(l, _)| *l == light) {
            self.icon.set_icon(icon);
        }
    }

    fn notify(&self, text: &str) {
        self.icon.show(
            text,
            Some(TITLE),
            Some(nwg::TrayNotificationFlags::INFO_ICON | nwg::TrayNotificationFlags::QUIET),
            None,
        );
    }

    /// What the menu item `handle` chooses, if it is one that types a line
    /// at `station`.
    fn chosen(&self, handle: nwg::ControlHandle, auto_on: bool) -> Option<Choice> {
        if handle == self.fix.handle {
            return Some(Choice::Fix);
        }
        if handle == self.compare.handle {
            return Some(Choice::Compare);
        }
        if handle == self.auto.handle {
            return Some(Choice::Auto(!auto_on));
        }
        if handle == self.enroll.handle {
            return Some(Choice::Enroll(true));
        }
        if handle == self.leave_out.handle {
            return Some(Choice::Enroll(false));
        }
        if handle == self.stock_put.handle {
            return Some(Choice::Stock { save: false });
        }
        if handle == self.stock_save.handle {
            return Some(Choice::Stock { save: true });
        }
        if handle == self.quit.handle {
            return Some(Choice::Quit);
        }
        for (index, name) in MY_SETUPS.iter().enumerate() {
            if self
                .keep
                .get(index)
                .is_some_and(|item| item.handle == handle)
            {
                return Some(Choice::Keep(name));
            }
        }
        for ((_, name), item) in tray::restore_menu().into_iter().zip(&self.restore) {
            if item.handle == handle {
                return Some(Choice::Restore(name));
            }
        }
        None
    }
}

/// What the tray knows.
struct Tray {
    face: Face,
    inbox: Inbox,
    exe: PathBuf,
    options: Vec<String>,
    log: PathBuf,
    station: Option<Station>,
    /// The radio's light and state, and what the radio is.
    light: Light,
    radio: String,
    model: String,
    /// The programs connected to the radio: (where from, name).
    programs: Vec<(String, String)>,
    /// The question waiting for a yes, and whether its box is showing.
    question: Option<u32>,
    box_open: bool,
    /// What the menu last asked for, until its answer comes.
    awaiting: Option<Choice>,
    quitting: bool,
    said_stopped: String,
}

impl Tray {
    fn auto_on() -> bool {
        crate::auto_mode::load(&crate::auto_mode::path(&crate::station::records_folder()))
    }

    fn refresh(&self) {
        let light = if self.box_open {
            Light::Asking
        } else {
            self.light
        };
        self.face.show_light(light);
        let mut tip = format!("{TITLE}\n{}{}", self.model, self.radio);
        if tip.chars().count() > TIP_MOST {
            tip = tip.chars().take(TIP_MOST).collect();
        }
        self.face.icon.set_tip(&tip);
        self.face.auto.set_checked(Self::auto_on());
    }

    fn type_line(&mut self, line: &str) {
        if let Some(typing) = self.station.as_mut().and_then(|s| s.typing.as_mut()) {
            let _ = writeln!(typing, "{line}");
            let _ = typing.flush();
        }
    }

    fn start(&mut self) {
        match start_station(&self.exe, &self.options, &self.log, &self.inbox) {
            Ok(station) => self.station = Some(station),
            Err(why) => self.face.notify(&why),
        }
    }

    fn choose(&mut self, choice: Choice) {
        if choice == Choice::Quit {
            self.quit();
            return;
        }
        if self.station.is_none() {
            self.face
                .notify("The Agent isn't running just now; it starts again shortly.");
            return;
        }
        self.type_line(&choice.typed());
        self.awaiting = Some(choice);
    }

    fn quit(&mut self) {
        self.quitting = true;
        self.type_line(&Choice::Quit.typed());
        let Some(mut station) = self.station.take() else {
            nwg::stop_thread_dispatch();
            return;
        };
        drop(station.typing.take());
        let child = Arc::clone(&station.child);
        std::thread::spawn(move || {
            let until = Instant::now() + tray::QUIT_WAIT;
            while Instant::now() < until {
                let done = child.lock().map(|mut c| c.try_wait());
                if !matches!(done, Ok(Ok(None))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // It didn't stop when asked: end it. It was given the time to
            // unkey the radio first, as it does whenever it stops.
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        });
    }

    fn ask(&mut self, number: u32, about: &str, yes_does: &str) {
        self.question = Some(number);
        if self.box_open {
            // The open box's answer is for an older question; it is
            // dropped, and this one asked when that box closes.
            return;
        }
        self.box_open = true;
        let (text, inbox) = (tray::question_box(about, yes_does), self.inbox.clone());
        std::thread::spawn(move || {
            let yes = boxed(&text, true);
            inbox.send(Incoming::Answered { number, yes });
        });
    }

    fn handle(&mut self, incoming: Incoming, pending: &mut Option<(u32, String, String)>) {
        match incoming {
            Incoming::Said(event) => self.heard(&event, pending),
            Incoming::Answered { number, yes } => {
                self.box_open = false;
                if self.question == Some(number) {
                    self.question = None;
                    self.type_line(&Choice::Answer { number, yes }.typed());
                    self.awaiting = Some(Choice::Answer { number, yes });
                }
                if let Some((number, about, yes_does)) = pending.take() {
                    self.ask(number, &about, &yes_does);
                }
            }
            Incoming::Stopped(why) => {
                self.station = None;
                self.programs.clear();
                self.light = Light::Waiting;
                if self.quitting {
                    nwg::stop_thread_dispatch();
                    return;
                }
                if why != self.said_stopped {
                    self.face.notify(&format!(
                        "The Agent stopped: {why}. It starts again in {} s; Open log says more.",
                        RESTART.as_secs()
                    ));
                    self.said_stopped = why;
                }
                let inbox = self.inbox.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(RESTART);
                    inbox.send(Incoming::Restart);
                });
            }
            Incoming::Restart => {
                if !self.quitting && self.station.is_none() {
                    self.start();
                }
            }
        }
    }

    fn heard(&mut self, event: &str, pending: &mut Option<(u32, String, String)>) {
        match tray::classify(event) {
            Said::Radio(light, state) => {
                self.light = light;
                self.radio = state;
            }
            Said::Waiting(why) => {
                self.light = Light::Waiting;
                self.radio = format!(
                    "waiting for the radio: {}",
                    why.lines().next().unwrap_or("")
                );
            }
            Said::Found(what) => {
                self.light = Light::Receiving;
                self.model = format!("{}: ", what.split(" on ").next().unwrap_or(&what));
                self.said_stopped.clear();
                self.face.notify(&format!("Found {what}."));
            }
            Said::Question {
                number,
                about,
                yes_does,
            } => {
                self.awaiting = None;
                if self.box_open {
                    self.question = Some(number);
                    *pending = Some((number, about, yes_does));
                } else {
                    self.ask(number, &about, &yes_does);
                }
            }
            said @ (Said::Program(_) | Said::Other(_)) => {
                if let Said::Program(text) = &said {
                    tray::track_programs(text, &mut self.programs);
                }
                let text = match &said {
                    Said::Program(text) | Said::Other(text) => text.clone(),
                    _ => String::new(),
                };
                let answer = matches!(said, Said::Other(_))
                    .then(|| self.awaiting.take())
                    .flatten();
                match answer {
                    Some(Choice::Compare) => {
                        std::thread::spawn(move || boxed(&text, false));
                    }
                    Some(_) => self.face.notify(&text),
                    None if said.needs_a_look() => self.face.notify(&text),
                    None => {}
                }
            }
        }
    }
}

/// Runs the tray, with `options` for its `station`.
///
/// # Errors
///
/// The tray is already running, or Windows wouldn't make it.
pub fn run(options: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("can't tell where the Agent is: {error}"))?;
    if std::env::var_os(HIDDEN).is_none() {
        // Started with a console window (typed, or from the Run key):
        // start again without one, and leave.
        start_hidden(&exe, options)?;
        println!(
            "The Agent's tray is starting: look for its icon by the clock (it may be under the ^ arrow)."
        );
        return Ok(());
    }
    let folder = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let _only = match only_tray(&folder) {
        Ok(lock) => lock,
        Err(why) => {
            boxed(&why, false);
            return Err(why);
        }
    };
    built(nwg::init())?;
    let face = Face::build()?;
    let inbox = Inbox {
        queue: Arc::new(Mutex::new(VecDeque::new())),
        wake: face.wake.sender(),
    };
    let tray = Rc::new(RefCell::new(Tray {
        face,
        inbox,
        exe,
        options: options.to_vec(),
        log: folder.join(tray::LOG_NAME),
        station: None,
        light: Light::Waiting,
        radio: "starting".to_owned(),
        model: String::new(),
        programs: Vec::new(),
        question: None,
        box_open: false,
        awaiting: None,
        quitting: false,
        said_stopped: String::new(),
    }));
    tray.borrow_mut().start();
    tray.borrow().refresh();
    let window = tray.borrow().face.window.handle;
    let handler_tray = Rc::clone(&tray);
    let pending: Rc<RefCell<Option<(u32, String, String)>>> = Rc::new(RefCell::new(None));
    let handler = nwg::full_bind_event_handler(&window, move |event, _data, handle| {
        // An event that comes while the tray is busy with another (a menu
        // open inside a click) waits for nothing: it is skipped.
        let Ok(mut tray) = handler_tray.try_borrow_mut() else {
            return;
        };
        match event {
            nwg::Event::OnContextMenu if handle == tray.face.icon.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                tray.face.menu.popup(x, y);
            }
            nwg::Event::OnMousePress(nwg::MousePressEvent::MousePressLeftUp)
                if handle == tray.face.icon.handle =>
            {
                let status = tray::status(&tray.model, &tray.radio, &tray.programs);
                tray.face.notify(&status);
            }
            nwg::Event::OnMenuItemSelected => {
                if handle == tray.face.log.handle {
                    // The log is written oldest first, so a line is never
                    // lost; Open log shows a copy with the newest first.
                    let view = tray.log.with_file_name(tray::LOG_VIEW_NAME);
                    let log = std::fs::read_to_string(&tray.log).unwrap_or_default();
                    let shown = if std::fs::write(&view, tray::newest_first(&log)).is_ok() {
                        view
                    } else {
                        tray.log.clone()
                    };
                    let _ = Command::new("notepad.exe").arg(shown).spawn();
                } else if let Some(choice) = tray.face.chosen(handle, Tray::auto_on()) {
                    tray.choose(choice);
                }
            }
            _ => {}
        }
        // Whatever came in, including while a menu was open.
        let mut pending = pending.borrow_mut();
        for incoming in tray.inbox.take() {
            tray.handle(incoming, &mut pending);
        }
        tray.refresh();
    });
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}
