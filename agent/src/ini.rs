// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Changing a few keys of another program's INI-style settings file, every
//! other line left exactly as it was.
//!
//! Used for the programs the Agent sets up (see `programs`): WSJT-X and
//! `JS8Call` (Qt's INI format) and MMSSTV (`Mmsstv.ini`). Values are written
//! as given; each program's module encodes them its own way first.

/// The value of `key` in `[section]` of `text`, as written in the file.
/// Section names are matched ignoring case; keys exactly.
#[must_use]
pub fn value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = section_name(trimmed) {
            inside = name.eq_ignore_ascii_case(section);
            continue;
        }
        if inside
            && let Some((found, value)) = trimmed.split_once('=')
            && found.trim() == key
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// `text` with each `(key, value)` of `wanted` set in `[section]`, every
/// other line left exactly as it was, line endings included. A key that is
/// missing is added at the end of the section, and a missing section at the
/// end of the file.
#[must_use]
pub fn with_keys(text: &str, section: &str, wanted: &[(&str, String)]) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out: Vec<String> = Vec::new();
    let mut written = vec![false; wanted.len()];
    let mut inside = false;
    let mut seen_section = false;
    // The missing keys, added after the section's last non-blank line.
    let finish = |out: &mut Vec<String>, written: &mut [bool]| {
        let blank_tail = out
            .iter()
            .rev()
            .take_while(|line| line.trim().is_empty())
            .count();
        let at = out.len() - blank_tail;
        let missing: Vec<String> = wanted
            .iter()
            .zip(written.iter_mut())
            .filter(|(_, done)| !**done)
            .map(|((key, value), done)| {
                *done = true;
                format!("{key}={value}")
            })
            .collect();
        out.splice(at..at, missing);
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = section_name(trimmed) {
            if inside {
                finish(&mut out, &mut written);
            }
            inside = name.eq_ignore_ascii_case(section);
            seen_section |= inside;
            out.push(line.to_owned());
            continue;
        }
        let key = trimmed.split_once('=').map(|(key, _)| key.trim());
        let found = key.and_then(|key| wanted.iter().position(|(want, _)| *want == key));
        match found {
            Some(index) if inside => {
                if written[index] {
                    // A second copy of the key: drop it, so the file says
                    // one thing.
                    continue;
                }
                out.push(format!("{}={}", wanted[index].0, wanted[index].1));
                written[index] = true;
            }
            _ => out.push(line.to_owned()),
        }
    }
    if inside {
        finish(&mut out, &mut written);
    }
    if !seen_section {
        out.push(format!("[{section}]"));
        finish(&mut out, &mut written);
    }
    let mut joined = out.join(newline);
    joined.push_str(newline);
    joined
}

/// The name of the section `line` starts, if it starts one.
fn section_name(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.strip_suffix(']')
}

#[cfg(test)]
mod tests {
    use super::{value, with_keys};

    #[test]
    fn reads_a_value_in_its_section_only() {
        let text = "[A]\nKey=one\n[B]\nKey = two\n";
        assert_eq!(value(text, "B", "Key").as_deref(), Some("two"));
        assert_eq!(value(text, "a", "Key").as_deref(), Some("one"));
        assert_eq!(value(text, "C", "Key"), None);
    }

    #[test]
    fn changes_only_the_wanted_lines() {
        let text = "[A]\r\nKey=one\r\n\r\n[B]\r\nKey=two\r\nOther=x\r\n";
        let changed = with_keys(text, "B", &[("Key", "three".to_owned())]);
        assert_eq!(
            changed,
            "[A]\r\nKey=one\r\n\r\n[B]\r\nKey=three\r\nOther=x\r\n"
        );
    }

    #[test]
    fn adds_missing_keys_at_the_end_of_their_section() {
        let text = "[A]\nKey=one\n\n[B]\nOther=x\n";
        let changed = with_keys(
            text,
            "A",
            &[("New", "y".to_owned()), ("Key", "z".to_owned())],
        );
        assert_eq!(changed, "[A]\nKey=z\nNew=y\n\n[B]\nOther=x\n");
    }

    #[test]
    fn adds_a_missing_section() {
        let changed = with_keys("[A]\nKey=one\n", "B", &[("Key", "two".to_owned())]);
        assert_eq!(changed, "[A]\nKey=one\n[B]\nKey=two\n");
        assert_eq!(with_keys("", "B", &[("K", "v".to_owned())]), "[B]\nK=v\n");
    }

    #[test]
    fn leaves_one_copy_of_a_repeated_key() {
        let changed = with_keys("[A]\nKey=1\nKey=2\n", "A", &[("Key", "3".to_owned())]);
        assert_eq!(changed, "[A]\nKey=3\n");
    }
}
