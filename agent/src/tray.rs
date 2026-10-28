// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `loglab-agent tray`: `station` in the background, with an icon by the
//! clock instead of a console window.
//!
//! The tray runs `station` as its own process, with no window, and talks to
//! it exactly as an operator at its console does: it reads what `station`
//! says and types what the operator chooses from the tray's menu. So the
//! tray changes nothing about what the Agent does to a radio; every change
//! still goes through `station`'s console, and a change that needs the
//! operator's yes gets a Yes/No box, answered by its question number so a
//! yes is never taken for a newer question (Principle X).
//!
//! This module holds what the tray reads from `station`'s output and the
//! tray's icons, and is tested on every platform; `tray_windows` is the
//! Windows side.

// Only the tray's Windows side uses most of this; elsewhere it is built
// for its tests.
#![cfg_attr(not(windows), allow(dead_code))]

/// Where `station`'s output is kept, beside the Agent.
pub const LOG_NAME: &str = "loglab-agent.log";

/// How long the tray waits for `station` to stop after quit before it
/// ends it.
pub const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs the tray, with `options` for its `station`.
///
/// # Errors
///
/// This isn't Windows, the tray is already running, or it couldn't start.
pub fn run(options: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::tray_windows::run(options)
    }
    #[cfg(not(windows))]
    {
        let _ = options;
        Err("the tray is for Windows; elsewhere, run station".to_owned())
    }
}

/// Starts the tray with no window, with `options` for its `station`, and
/// returns at once.
///
/// # Errors
///
/// Windows wouldn't start it.
#[cfg(windows)]
pub fn start_hidden(exe: &std::path::Path, options: &[String]) -> Result<(), String> {
    crate::tray_windows::start_hidden(exe, options)
}

/// One thing `station` said: a line, with any lines under it that explain
/// it.
#[derive(Debug, Default)]
pub struct Grouper {
    lines: Vec<String>,
}

/// The width of the time `station` puts before each event: `04:31:02Z  `.
const CLOCK: usize = 11;

/// `line` without the time `station` puts before an event, if it has one.
fn without_clock(line: &str) -> &str {
    let bytes = line.as_bytes();
    let clocked = bytes.len() >= CLOCK
        && bytes[2] == b':'
        && bytes[5] == b':'
        && bytes[8] == b'Z'
        && &bytes[9..CLOCK] == b"  "
        && [0, 1, 3, 4, 6, 7]
            .iter()
            .all(|&i| bytes[i].is_ascii_digit());
    if clocked { &line[CLOCK..] } else { line }
}

impl Grouper {
    /// Takes one line `station` printed. Returns what it said before, once
    /// this line starts something new.
    pub fn push(&mut self, line: &str) -> Option<String> {
        let explains = line.starts_with(char::is_whitespace) || line.starts_with("->");
        if explains && !self.lines.is_empty() {
            // Lines under an event are indented to line up under it.
            let line = line.strip_prefix(&" ".repeat(CLOCK)).unwrap_or(line);
            self.lines.push(line.to_owned());
            return None;
        }
        let done = self.flush();
        self.lines.push(without_clock(line).to_owned());
        done
    }

    /// What it has been told since the last thing it returned, if anything.
    pub fn flush(&mut self) -> Option<String> {
        if self.lines.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.lines).join("\n"))
    }
}

/// What the radio is doing, for the tray's icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    /// Waiting for the radio, or it can't be read: grey.
    Waiting,
    /// Receiving: green.
    Receiving,
    /// Transmitting: red.
    Transmitting,
    /// A change waits for the operator's yes: amber.
    Asking,
}

impl Light {
    /// Every light, for making their icons.
    pub const ALL: [Self; 4] = [
        Self::Waiting,
        Self::Receiving,
        Self::Transmitting,
        Self::Asking,
    ];

    /// The icon's colour, as red, green and blue.
    #[must_use]
    pub fn colour(self) -> [u8; 3] {
        match self {
            Self::Waiting => [0x8a, 0x8a, 0x8a],
            Self::Receiving => [0x2e, 0xa0, 0x43],
            Self::Transmitting => [0xd7, 0x26, 0x26],
            Self::Asking => [0xe8, 0xa3, 0x17],
        }
    }

    /// A short name, for the icon's file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Receiving => "receiving",
            Self::Transmitting => "transmitting",
            Self::Asking => "asking",
        }
    }
}

/// What one thing `station` said means to the tray.
#[derive(Debug, PartialEq, Eq)]
pub enum Said {
    /// The radio's state, and how to say it.
    Radio(Light, String),
    /// The radio was found: what it is and where.
    Found(String),
    /// It is looking for the radio, and why it hasn't found it.
    Waiting(String),
    /// A change waits for a yes: its question number, what it is, and what
    /// a yes does ("set them").
    Question {
        /// The number to answer it by.
        number: u32,
        /// What the change is and why.
        about: String,
        /// What yes does.
        yes_does: String,
    },
    /// A program's use of the radio: connected, keyed, unkeyed. Not an
    /// answer to anything the operator chose.
    Program(String),
    /// Anything else: an answer, a warning, a note.
    Other(String),
}

impl Said {
    /// Whether it tells the operator to do something (`->`), so it is
    /// worth showing even when nobody asked.
    #[must_use]
    pub fn needs_a_look(&self) -> bool {
        match self {
            Self::Program(text) | Self::Other(text) => {
                text.lines().any(|line| line.trim_start().starts_with("->"))
                    || text.contains("Auto mode:")
            }
            _ => false,
        }
    }
}

/// The question number at the end of `station`'s ask: `(question 3)`.
fn question_number(line: &str) -> Option<u32> {
    let start = line.rfind("(question ")? + "(question ".len();
    let rest = &line[start..];
    let end = rest.find(')')?;
    rest[..end].parse().ok()
}

/// What one thing `station` said means.
#[must_use]
pub fn classify(event: &str) -> Said {
    let first = event.lines().next().unwrap_or_default();
    if let Some(state) = first.strip_prefix("radio: ") {
        let light = if state.contains("transmitting") {
            Light::Transmitting
        } else if state.starts_with("can't be read") {
            Light::Waiting
        } else {
            Light::Receiving
        };
        return Said::Radio(light, state.to_owned());
    }
    if let Some(why) = first.strip_prefix("waiting for the radio: ") {
        return Said::Waiting(why.to_owned());
    }
    if let Some(what) = first.strip_prefix("Found: ") {
        return Said::Found(what.trim_end_matches('.').to_owned());
    }
    let ask = event.lines().find(|line| {
        line.trim_start().starts_with("-> Type yes") && question_number(line).is_some()
    });
    if let Some(ask) = ask
        && let Some(number) = question_number(ask)
    {
        let about = event
            .lines()
            .filter(|line| !std::ptr::eq(*line, ask))
            .collect::<Vec<&str>>()
            .join("\n");
        let yes_does = ask
            .trim_start()
            .trim_start_matches("-> Type yes to ")
            .split(';')
            .next()
            .unwrap_or_default()
            .split(", then ")
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        return Said::Question {
            number,
            about,
            yes_does,
        };
    }
    let program = [
        "a program ",
        "asked to ",
        "transmit audio:",
        "receive audio:",
    ];
    if program.iter().any(|start| first.starts_with(start)) {
        return Said::Program(event.to_owned());
    }
    Said::Other(event.to_owned())
}

/// The text of a Yes/No box for a question.
#[must_use]
pub fn question_box(about: &str, yes_does: &str) -> String {
    format!("{about}\n\nYes: {yes_does}.\nNo: leave the radio as it is.")
}

/// What the tray's menu can ask `station` to do, as typed at its console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Fix what stops a job working.
    Fix,
    /// Keep the radio's settings as this setup.
    Keep(&'static str),
    /// Put this setup back (after a yes).
    Restore(&'static str),
    /// Compare the radio with each kept setup.
    Compare,
    /// Turn auto mode on or off.
    Auto(bool),
    /// Answer question `number`.
    Answer {
        /// The question's number.
        number: u32,
        /// Yes or no.
        yes: bool,
    },
    /// Stop.
    Quit,
}

/// The setups the tray offers to keep and put back, named for what the
/// radio is set up for.
pub const SETUPS: [&str; 5] = ["VARA", "FT8", "JS8", "fldigi", "SSTV"];

impl Choice {
    /// The line typed at `station`'s console.
    #[must_use]
    pub fn typed(&self) -> String {
        match self {
            Self::Fix => "fix".to_owned(),
            Self::Keep(name) => format!("keep {name}"),
            Self::Restore(name) => format!("restore {name}"),
            Self::Compare => "compare".to_owned(),
            Self::Auto(on) => format!("auto {}", if *on { "on" } else { "off" }),
            Self::Answer { number, yes: true } => format!("yes {number}"),
            Self::Answer { yes: false, .. } => "no".to_owned(),
            Self::Quit => "quit".to_owned(),
        }
    }
}

/// The side of the tray's icon, in pixels.
const ICON_SIDE: usize = 32;

/// A Windows icon file (`.ico`) of one round light in `colour` (red, green,
/// blue), 32 by 32 pixels, its edge smoothed through transparency.
#[must_use]
pub fn icon_file(colour: [u8; 3]) -> Vec<u8> {
    let side = ICON_SIDE;
    let pixels = side * side * 4;
    // The mask: 1 bit a pixel, rows padded to 4 bytes. All clear: the
    // pixels' own alpha says what shows.
    let mask = side.div_ceil(32) * 4 * side;
    let image = 40 + pixels + mask;
    let mut file = Vec::with_capacity(22 + image);
    // ICONDIR: reserved, type 1 (icon), one image.
    file.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    // ICONDIRENTRY: width, height, no palette, reserved, 1 plane, 32 bits,
    // the image's size and where it starts.
    let side_byte = u8::try_from(side).unwrap_or(0);
    file.extend_from_slice(&[side_byte, side_byte, 0, 0, 1, 0, 32, 0]);
    file.extend_from_slice(&u32::try_from(image).unwrap_or(0).to_le_bytes());
    file.extend_from_slice(&22u32.to_le_bytes());
    // BITMAPINFOHEADER: its size, width, twice the height (colour then
    // mask), 1 plane, 32 bits, no compression, the image data's size.
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&u32::try_from(side).unwrap_or(0).to_le_bytes());
    file.extend_from_slice(&u32::try_from(side * 2).unwrap_or(0).to_le_bytes());
    file.extend_from_slice(&[1, 0, 32, 0]);
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&u32::try_from(pixels + mask).unwrap_or(0).to_le_bytes());
    file.extend_from_slice(&[0; 16]);
    let [red, green, blue] = colour;
    let darker = |c: u8| c / 2;
    let centre = 15.5_f64;
    // Rows go from the bottom up, each pixel blue, green, red, alpha.
    for row in (0..side).rev() {
        for column in 0..side {
            let dx = u32::try_from(column).map_or(0.0, f64::from) - centre;
            let dy = u32::try_from(row).map_or(0.0, f64::from) - centre;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (14.5 - distance).clamp(0.0, 1.0);
            // Between 0 and 255 after the clamp, so the cast keeps it whole.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let alpha = (coverage * 255.0).round() as u8;
            // A darker ring round the edge, so the light shows on any
            // taskbar.
            let ring = distance > 11.5;
            let (r, g, b) = if ring {
                (darker(red), darker(green), darker(blue))
            } else {
                (red, green, blue)
            };
            file.extend_from_slice(&[b, g, r, alpha]);
        }
    }
    file.extend(std::iter::repeat_n(0, mask));
    file
}

/// Quotes `option` for a Windows command line, if it needs it, following
/// Microsoft's rules for how programs split their command line
/// (`CommandLineToArgvW`): backslashes before the closing quote are
/// doubled.
///
/// # Errors
///
/// The option holds a quotation mark.
pub fn quoted(option: &str) -> Result<String, String> {
    if option.contains('"') {
        return Err(format!("an option can't hold a quotation mark: {option}"));
    }
    if !option.is_empty() && !option.contains(char::is_whitespace) {
        return Ok(option.to_owned());
    }
    let trailing = option.len() - option.trim_end_matches('\\').len();
    Ok(format!("\"{option}{}\"", "\\".repeat(trailing)))
}

#[cfg(test)]
mod tests {
    use super::{Choice, Grouper, Light, Said, classify, icon_file, question_box, quoted};

    /// Every event `lines` make, in order.
    fn grouped(lines: &[&str]) -> Vec<String> {
        let mut grouper = Grouper::default();
        let mut events: Vec<String> = lines.iter().filter_map(|l| grouper.push(l)).collect();
        events.extend(grouper.flush());
        events
    }

    #[test]
    fn groups_each_event_with_the_lines_under_it() {
        let events = grouped(&[
            "Found: Icom IC-7300 on COM5 (USB serial IC-7300_02065962), at 115200 baud.",
            "Radio settings: DATA MOD is MIC. VARA transmits silence.",
            "  -> The Agent can set it to USB: it asks below.",
            "Radio settings: the Agent can fix what stops a job working.",
            "  DATA MOD: MIC -> USB",
            "-> Type yes to set it; anything else leaves the radio as it is. (question 1)",
            "04:31:02Z  radio: 14.105000 MHz DATA-USB, receiving",
            "04:31:09Z  asked to stop transmitting: unkeyed after 5.3 s; peak Po 100%",
            "           -> ALC went over a third of its scale.",
        ]);
        assert_eq!(events.len(), 5, "{events:#?}");
        assert_eq!(
            events[1],
            "Radio settings: DATA MOD is MIC. VARA transmits silence.\n  -> The Agent can set it to USB: it asks below."
        );
        assert_eq!(events[3], "radio: 14.105000 MHz DATA-USB, receiving");
        assert_eq!(
            events[4],
            "asked to stop transmitting: unkeyed after 5.3 s; peak Po 100%\n-> ALC went over a third of its scale."
        );
    }

    #[test]
    fn reads_the_radio_and_questions() {
        assert_eq!(
            classify("radio: 14.105000 MHz DATA-USB, transmitting"),
            Said::Radio(
                Light::Transmitting,
                "14.105000 MHz DATA-USB, transmitting".to_owned()
            )
        );
        assert!(matches!(
            classify("radio: 14.105000 MHz DATA-USB, receiving"),
            Said::Radio(Light::Receiving, _)
        ));
        assert!(matches!(
            classify("radio: can't be read (no answer to 03 within 300 ms)"),
            Said::Radio(Light::Waiting, _)
        ));
        assert!(matches!(
            classify(
                "waiting for the radio: no radio's USB serial port is on this computer.\n-> ..."
            ),
            Said::Waiting(_)
        ));
        assert_eq!(
            classify("Found: Icom IC-7300 on COM5, at 115200 baud."),
            Said::Found("Icom IC-7300 on COM5, at 115200 baud".to_owned())
        );
        let asked = classify(
            "ALC peaked at 70% of its scale: lower USB MOD Level from 128 of 255 (50%) to 102 of 255 (40%).\n\
             -> Type yes to set it, then transmit again to see the new ALC; anything else leaves the radio as it is. (question 4)",
        );
        let Said::Question {
            number,
            about,
            yes_does,
        } = asked
        else {
            unreachable!("a question: {asked:?}")
        };
        assert_eq!(number, 4);
        assert!(about.starts_with("ALC peaked at 70%"), "{about}");
        assert!(!about.contains("Type yes"), "{about}");
        assert_eq!(yes_does, "set it");
        assert_eq!(
            question_box(&about, &yes_does).lines().last(),
            Some("No: leave the radio as it is.")
        );
        let restore = classify(
            "putting back setup VARA changes 2 settings:\n  DATA MOD: MIC -> USB\n\
             -> Type yes to put them back on the radio; anything else leaves it as it is. (question 7)",
        );
        assert!(matches!(
            restore,
            Said::Question { number: 7, ref yes_does, .. } if yes_does == "put them back on the radio"
        ));
        // A line that only mentions yes, with no number, isn't a question.
        assert!(matches!(
            classify("nothing is waiting for a yes."),
            Said::Other(_)
        ));
    }

    #[test]
    fn knows_what_needs_a_look() {
        let program = classify(
            "asked to stop transmitting: unkeyed after 5.3 s; peak Po 100%, ALC 92%\n\
             -> ALC went over a third of its scale.",
        );
        assert!(matches!(program, Said::Program(_)));
        assert!(program.needs_a_look());
        assert!(!classify("a program connected (127.0.0.1:57447)").needs_a_look());
        assert!(classify("ALC peaked at 70%.\nAuto mode: fixed 1 of 1 setting.").needs_a_look());
        assert!(!classify("Setups: kept on this computer.").needs_a_look());
    }

    #[test]
    fn types_what_the_menu_chose() {
        assert_eq!(Choice::Keep("VARA").typed(), "keep VARA");
        assert_eq!(Choice::Restore("FT8").typed(), "restore FT8");
        assert_eq!(Choice::Auto(true).typed(), "auto on");
        assert_eq!(
            Choice::Answer {
                number: 3,
                yes: true
            }
            .typed(),
            "yes 3"
        );
        assert_eq!(
            Choice::Answer {
                number: 3,
                yes: false
            }
            .typed(),
            "no"
        );
        assert_eq!(Choice::Quit.typed(), "quit");
    }

    #[test]
    fn makes_a_round_icon() {
        let file = icon_file(Light::Receiving.colour());
        assert_eq!(
            &file[..6],
            &[0, 0, 1, 0, 1, 0],
            "an icon file with one image"
        );
        assert_eq!(file[6], 32);
        let size = u32::from_le_bytes([file[14], file[15], file[16], file[17]]);
        assert_eq!(file.len(), 22 + size as usize);
        assert_eq!(file.len(), 22 + 40 + 32 * 32 * 4 + 32 * 4);
        let pixel = |row: usize, column: usize| {
            // Rows are stored bottom up.
            let at = 22 + 40 + ((31 - row) * 32 + column) * 4;
            [file[at], file[at + 1], file[at + 2], file[at + 3]]
        };
        assert_eq!(pixel(0, 0)[3], 0, "a corner is clear");
        assert_eq!(
            pixel(16, 16),
            [0x43, 0xa0, 0x2e, 255],
            "the middle is green"
        );
    }

    #[test]
    fn quotes_options_for_the_command_line() {
        assert_eq!(quoted("--civ").as_deref(), Ok("--civ"));
        assert_eq!(
            quoted(r"C:\LabLog\FL 1.key").as_deref(),
            Ok(r#""C:\LabLog\FL 1.key""#)
        );
        assert_eq!(quoted(r"C:\My Keys\").as_deref(), Ok(r#""C:\My Keys\\""#));
        assert_eq!(quoted("").as_deref(), Ok(r#""""#));
        assert!(quoted("say \"hi\"").is_err());
    }
}
