// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pointing fldigi at the Agent: its sound devices at the Agent's virtual
//! cables, and its radio at the Agent's virtual radio through Hamlib's
//! network client, keyed by Hamlib's PTT command.
//!
//! fldigi has no command line or XML-RPC command for these settings (its
//! XML-RPC server tunes and switches modes, but has no sound-device or
//! rig-setup method). It keeps them in `fldigi_def.xml`, read when it starts
//! and written when it closes, so the Agent changes the file only while
//! fldigi isn't running. Where everything is was read from fldigi's source
//! (`src/main.cxx`, `src/include/configuration.h`,
//! `src/misc/configuration.cxx`, `src/rigcontrol/hamlib.cxx`,
//! `src/soundcard/sound.cxx`):
//!
//! - The file is `%USERPROFILE%\fldigi.files\fldigi_def.xml` on Windows.
//! - Each setting is one element, `<TAG>value</TAG>`, inside
//!   `<FLDIGI_DEFS>`; strings escape `& < > " '` as XML entities.
//! - `AUDIOIO` 1 is `PortAudio`; `PORTINDEVICE` and `PORTOUTDEVICE` are the
//!   devices, matched by name (`PORTININDEX`/`PORTOUTINDEX` -1: no index).
//! - `CHKUSEHAMLIBIS` 1 turns Hamlib on; fldigi tries flrig
//!   (`CLIENT_TO_FLRIG`) and `RigCAT` (`CHKUSERIGCATIS`) first, so those are
//!   turned off, and so is XML-RPC rig control (`CHKUSEXMLRPCIS`).
//! - `HAMRIGMODEL` is Hamlib's model number: 2 is NET rigctl (Hamlib's
//!   `riglist.h`). `HAMRIGNAME` is left empty, because fldigi picks a rig by
//!   that name first when it has one. `HAMRIGDEVICE` is passed to Hamlib as
//!   the rig's path, which for NET rigctl is the server's address.
//! - `HAMLIBCMDPTT` 1 keys through Hamlib.
//!
//! The maintainer decided (2026-10-27) the Agent sets these up itself (see
//! `qt_settings` for the same decision about WSJT-X and `JS8Call`). Nothing
//! else in the file is touched, the file as first found is kept beside it,
//! and the new file is written whole (see `settings_file`).

use std::net::SocketAddr;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use crate::settings_file::{self, Change, Outcome};

/// The element every setting is inside.
const ROOT_END: &str = "</FLDIGI_DEFS>";
/// Hamlib's model number for its network client, NET rigctl.
const NET_RIGCTL: &str = "2";

/// What the Agent wants fldigi's settings to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    /// The device fldigi records from, or `None` to leave it alone.
    pub input: Option<String>,
    /// The device fldigi plays into, or `None` to leave it alone.
    pub output: Option<String>,
    /// The Agent's virtual radio.
    pub radio: SocketAddr,
}

/// `value` escaped as fldigi escapes a string setting.
#[must_use]
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The string an escaped setting stands for.
#[must_use]
pub fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The value of setting `tag` in `text`, as written in the file.
#[must_use]
pub fn value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(text[start..end].to_owned())
}

/// `text` with setting `tag` set to `raw` (already escaped). A missing
/// setting is added just before the end of `<FLDIGI_DEFS>`.
fn with_value(text: &str, tag: &str, raw: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(at) = text.find(&open) {
        let start = at + open.len();
        if let Some(length) = text[start..].find(&close) {
            return format!("{}{raw}{}", &text[..start], &text[start + length..]);
        }
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let element = format!("{open}{raw}{close}{newline}{newline}");
    match text.rfind(ROOT_END) {
        Some(at) => format!("{}{element}{}", &text[..at], &text[at..]),
        None => format!("{text}{element}"),
    }
}

/// `text` changed to say what `wanted` says, and what changed.
#[must_use]
pub fn with_station(text: &str, wanted: &Wanted) -> (String, Vec<Change>) {
    let mut lines: Vec<(&str, String)> = vec![("AUDIOIO", "1".to_owned())];
    if let Some(input) = &wanted.input {
        lines.push(("PORTINDEVICE", escape(input)));
        lines.push(("PORTININDEX", "-1".to_owned()));
    }
    if let Some(output) = &wanted.output {
        lines.push(("PORTOUTDEVICE", escape(output)));
        lines.push(("PORTOUTINDEX", "-1".to_owned()));
    }
    lines.extend([
        ("CLIENT_TO_FLRIG", "0".to_owned()),
        ("CHKUSERIGCATIS", "0".to_owned()),
        ("CHKUSEXMLRPCIS", "0".to_owned()),
        ("CHKUSEHAMLIBIS", "1".to_owned()),
        ("HAMRIGMODEL", NET_RIGCTL.to_owned()),
        ("HAMRIGNAME", String::new()),
        ("HAMRIGDEVICE", escape(&wanted.radio.to_string())),
        ("HAMLIBCMDPTT", "1".to_owned()),
    ]);
    let mut changed = text.to_owned();
    let mut diffs = Vec::new();
    for (tag, raw) in lines {
        let before = value(&changed, tag);
        if before.as_deref() == Some(raw.as_str()) {
            continue;
        }
        changed = with_value(&changed, tag, &raw);
        diffs.push(Change {
            key: tag.to_owned(),
            before: before.map(|before| unescape(&before)),
            after: unescape(&raw),
        });
    }
    (changed, diffs)
}

/// Sets fldigi's settings file at `path` to say what `wanted` says. fldigi
/// must not be running: it writes its settings when it closes.
///
/// # Errors
///
/// The file can't be read or written.
pub fn set(path: &Path, wanted: &Wanted) -> Result<Outcome, String> {
    settings_file::update(path, |text| Ok(with_station(text, wanted)))
}

/// Where fldigi's settings file is: `%USERPROFILE%\fldigi.files\fldigi_def.xml`.
#[cfg(windows)]
#[must_use]
pub fn find() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("USERPROFILE")?)
        .join("fldigi.files")
        .join("fldigi_def.xml");
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::{Wanted, escape, set, unescape, value, with_station};
    use crate::settings_file::Outcome;

    /// Part of an `fldigi_def.xml`, laid out as fldigi writes it.
    const XML: &str = "<FLDIGI_DEFS>\n\n<!-- Use Hamlib rig control -->\n<CHKUSEHAMLIBIS>0</CHKUSEHAMLIBIS>\n\n<!-- Hamlib rig name -->\n<HAMRIGNAME>Icom IC-7300  (Stable)</HAMRIGNAME>\n\n<!-- Hamlib rig model -->\n<HAMRIGMODEL>3073</HAMRIGMODEL>\n\n<!-- Hamlib rig device -->\n<HAMRIGDEVICE>COM3</HAMRIGDEVICE>\n\n<!-- PortAudio input device name -->\n<PORTINDEVICE>Microphone (USB Audio CODEC )</PORTINDEVICE>\n\n<!-- My name -->\n<MYNAME>Bob &amp; Sue</MYNAME>\n\n</FLDIGI_DEFS>\n";

    fn wanted() -> Wanted {
        Wanted {
            input: Some("CABLE Output (VB-Audio Virtual Cable)".to_owned()),
            output: Some("CABLE-A Input (VB-Audio Cable A)".to_owned()),
            radio: "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!()),
        }
    }

    #[test]
    fn escapes_as_fldigi_does() {
        assert_eq!(
            escape("a & <b> \"c\" 'd'"),
            "a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;"
        );
        assert_eq!(
            unescape(&escape("a & <b> \"c\" 'd' &amp;")),
            "a & <b> \"c\" 'd' &amp;"
        );
    }

    #[test]
    fn sets_sound_and_hamlib_and_nothing_else() {
        let (changed, diffs) = with_station(XML, &wanted());
        let get = |tag| value(&changed, tag);
        assert_eq!(get("CHKUSEHAMLIBIS").as_deref(), Some("1"));
        assert_eq!(get("HAMRIGMODEL").as_deref(), Some("2"));
        assert_eq!(get("HAMRIGNAME").as_deref(), Some(""));
        assert_eq!(get("HAMRIGDEVICE").as_deref(), Some("127.0.0.1:4532"));
        assert_eq!(get("HAMLIBCMDPTT").as_deref(), Some("1"));
        assert_eq!(
            get("PORTINDEVICE").as_deref(),
            Some("CABLE Output (VB-Audio Virtual Cable)")
        );
        assert_eq!(
            get("PORTOUTDEVICE").as_deref(),
            Some("CABLE-A Input (VB-Audio Cable A)")
        );
        assert_eq!(get("CLIENT_TO_FLRIG").as_deref(), Some("0"));
        assert_eq!(get("MYNAME").as_deref(), Some("Bob &amp; Sue"), "untouched");
        assert!(changed.contains("<!-- Hamlib rig model -->\n<HAMRIGMODEL>2</HAMRIGMODEL>\n"));
        assert!(
            changed.ends_with("<HAMLIBCMDPTT>1</HAMLIBCMDPTT>\n\n</FLDIGI_DEFS>\n"),
            "{changed}"
        );
        let model = diffs.iter().find(|change| change.key == "HAMRIGMODEL");
        assert_eq!(
            model.and_then(|change| change.before.as_deref()),
            Some("3073")
        );
        // Run again: nothing more to change.
        let (again, diffs) = with_station(&changed, &wanted());
        assert_eq!(again, changed);
        assert!(diffs.is_empty());
    }

    #[test]
    fn leaves_a_device_alone_when_not_wanted() {
        let mut only_input = wanted();
        only_input.output = None;
        let (changed, _) = with_station(XML, &only_input);
        assert_eq!(value(&changed, "PORTOUTDEVICE"), None);
        assert_eq!(value(&changed, "PORTOUTINDEX"), None);
    }

    #[test]
    fn writes_the_file_and_keeps_the_original() {
        let folder = std::env::temp_dir().join(format!("lablog-fldigi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap_or_else(|error| unreachable!("{error}"));
        let path = folder.join("fldigi_def.xml");
        std::fs::write(&path, XML).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(set(&path, &wanted()), Ok(Outcome::Changed(_))));
        assert_eq!(set(&path, &wanted()), Ok(Outcome::AlreadySet));
        let kept = std::fs::read_to_string(folder.join("fldigi_def.xml.lablog-original"))
            .unwrap_or_default();
        assert_eq!(kept, XML);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
