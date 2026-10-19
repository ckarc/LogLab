// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FT8 message text to its 77-bit payload ("source encoding").
//!
//! Only the standard message (type 1) is supported so far: two standard
//! callsigns, or CQ (with an optional modifier such as `CQ DX`) and a
//! callsign, followed by a grid square, a signal report, `RRR`, `RR73`, `73`,
//! or nothing. That covers ordinary FT8 QSOs. Hashed and compound callsigns,
//! free text, and the Field Day and contest exchanges come later; until then
//! they are refused with [`MessageError::Unsupported`] rather than guessed.

use std::fmt;

/// Count of special 28-bit tokens (DE, QRZ, CQ and its modifiers) that sit
/// below the 22-bit hashed-callsign range.
const NTOKENS: u32 = 2_063_592;

/// Size of the 22-bit hashed-callsign range.
const MAX22: u32 = 4_194_304;

/// Count of four-character grid squares; values above it are special fields.
const MAXGRID4: u16 = 32_400;

/// Payload type of the standard message.
const I3_STANDARD: u128 = 1;

/// Characters allowed in each of the six positions of a standard callsign,
/// after it is right-aligned on its digit.
const CALL_ALPHANUM_SPACE: &str = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const CALL_ALPHANUM: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const CALL_NUMERIC: &str = "0123456789";
const CALL_LETTERS_SPACE: &str = " ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The 77 information bits of one FT8 transmission, most significant bit
/// first (bit 76 of the value is the first bit sent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payload(u128);

impl Payload {
    /// Number of bits in a payload.
    pub const BITS: u32 = 77;

    /// The payload as an integer; only the low 77 bits are used.
    #[must_use]
    pub fn value(self) -> u128 {
        self.0
    }

    /// The payload as a string of `0` and `1`, first bit sent first.
    #[must_use]
    pub fn to_bit_string(self) -> String {
        format!("{:077b}", self.0)
    }
}

/// Why a message could not be turned into a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageError {
    /// The message had no words in it.
    Empty,
    /// The message is valid FT8 in principle, but this encoder does not
    /// handle its kind yet. Carries a description of what was refused.
    Unsupported(String),
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the message is empty"),
            Self::Unsupported(what) => write!(f, "not supported yet: {what}"),
        }
    }
}

impl std::error::Error for MessageError {}

/// Puts message text into the form FT8 compares messages in: upper case,
/// single spaces, no leading or trailing space.
#[must_use]
pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Packs message text into its 77-bit payload.
///
/// # Errors
///
/// [`MessageError::Empty`] for blank text, and
/// [`MessageError::Unsupported`] for anything other than a standard message
/// with standard callsigns (see the module documentation).
pub fn pack(text: &str) -> Result<Payload, MessageError> {
    let normalized = normalize(text);
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Err(MessageError::Empty);
    }

    // "CQ DX K9KSU EN52": the modifier is part of the first field.
    let (to, rest): (u32, &[&str]) = if words[0] == "CQ" {
        match words.get(1).and_then(|w| cq_modifier(w)) {
            Some(modifier) if words.len() >= 3 => (3 + modifier, &words[2..]),
            _ => (2, &words[1..]),
        }
    } else {
        let to = match words[0] {
            "DE" => 0,
            "QRZ" => 1,
            call => standard_call(call)?,
        };
        (to, &words[1..])
    };

    let Some((de_word, extra)) = rest.split_first() else {
        return Err(MessageError::Unsupported(format!(
            "a message with one word: {normalized}"
        )));
    };
    let de = standard_call(de_word)?;
    let (ir, g15) = extra_field(extra)?;

    let value = (u128::from(to) << 1) << 48
        | (u128::from(de) << 1) << 19
        | u128::from(ir) << 18
        | u128::from(g15) << 3
        | I3_STANDARD;
    Ok(Payload(value))
}

/// The 28-bit field value for a standard callsign.
fn standard_call(call: &str) -> Result<u32, MessageError> {
    basecall(call)
        .map(|n| NTOKENS + MAX22 + n)
        .ok_or_else(|| MessageError::Unsupported(format!("callsign {call}")))
}

/// Packs a standard callsign (one to two prefix characters, a digit, and one
/// to three letters) into its 28-bit number, before the token offset.
///
/// Like WSJT-X, calls starting with Q are refused (no country issues them),
/// so LogLab never sends a call in a form WSJT-X would not.
fn basecall(call: &str) -> Option<u32> {
    let bytes = call.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 || bytes[0] == b'Q' {
        return None;
    }
    if !(bytes[0].is_ascii_uppercase() || bytes[1].is_ascii_uppercase()) {
        return None;
    }
    let digit = if bytes[2].is_ascii_digit() { 2 } else { 1 };
    let suffix = &bytes[digit + 1..];
    if !(1..=3).contains(&suffix.len()) || !suffix.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    // Right-align the call on its digit: "K9KSU" becomes " K9KSU".
    let mut c6 = [b' '; 6];
    if bytes[2].is_ascii_digit() {
        c6[..bytes.len()].copy_from_slice(bytes);
    } else if bytes[1].is_ascii_digit() && bytes.len() <= 5 {
        c6[1..=bytes.len()].copy_from_slice(bytes);
    } else {
        return None;
    }
    let tables = [
        CALL_ALPHANUM_SPACE,
        CALL_ALPHANUM,
        CALL_NUMERIC,
        CALL_LETTERS_SPACE,
        CALL_LETTERS_SPACE,
        CALL_LETTERS_SPACE,
    ];
    let mut n: u32 = 0;
    for (&c, table) in c6.iter().zip(tables) {
        let index = table.bytes().position(|t| t == c)?;
        let radix = u32::try_from(table.len()).ok()?;
        n = n * radix + u32::try_from(index).ok()?;
    }
    Some(n)
}

/// The value a CQ modifier adds: `CQ 123` (three digits) or `CQ DX` (one to
/// four letters). `None` when the word is not a modifier.
fn cq_modifier(word: &str) -> Option<u32> {
    let bytes = word.as_bytes();
    if bytes.len() == 3 && bytes.iter().all(u8::is_ascii_digit) {
        return word.parse().ok();
    }
    if (1..=4).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_uppercase) {
        let m = bytes
            .iter()
            .fold(0_u32, |m, &b| 27 * m + u32::from(b - b'A' + 1));
        return Some(1000 + m);
    }
    None
}

/// The `R` flag and 15-bit field after the two callsigns.
fn extra_field(extra: &[&str]) -> Result<(u8, u16), MessageError> {
    match extra {
        [] => Ok((0, MAXGRID4 + 1)),
        ["RRR"] => Ok((0, MAXGRID4 + 2)),
        // RR73 is also a valid grid square, and WSJT-X sends it as that
        // grid; receivers show that square as "RR73".
        ["73"] => Ok((0, MAXGRID4 + 4)),
        ["R", grid] => grid4(grid)
            .map(|g| (1, g))
            .ok_or_else(|| MessageError::Unsupported(format!("R {grid}"))),
        [word] => {
            if let Some(g) = grid4(word) {
                return Ok((0, g));
            }
            if let Some(report) = word.strip_prefix('R').and_then(signal_report) {
                return Ok((1, MAXGRID4 + report));
            }
            signal_report(word)
                .map(|report| (0, MAXGRID4 + report))
                .ok_or_else(|| MessageError::Unsupported(format!("field {word}")))
        }
        _ => Err(MessageError::Unsupported(format!(
            "fields {}",
            extra.join(" ")
        ))),
    }
}

/// A four-character Maidenhead grid square (`EN52`) as its 15-bit value.
fn grid4(word: &str) -> Option<u16> {
    let b = word.as_bytes();
    if b.len() != 4
        || !(b'A'..=b'R').contains(&b[0])
        || !(b'A'..=b'R').contains(&b[1])
        || !b[2].is_ascii_digit()
        || !b[3].is_ascii_digit()
    {
        return None;
    }
    let [a, b1, c, d] = [b[0] - b'A', b[1] - b'A', b[2] - b'0', b[3] - b'0'].map(u16::from);
    Some(((a * 18 + b1) * 10 + c) * 10 + d)
}

/// A signal report written `+05` or `-12`, offset into the field's range.
/// Reports from -50 to +49 dB are accepted; -50 to -31 share field values
/// with the top of the range, as WSJT-X sends them.
fn signal_report(word: &str) -> Option<u16> {
    let b = word.as_bytes();
    if b.len() != 3 || !(b[0] == b'+' || b[0] == b'-') || !b[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let report: i16 = word.parse().ok()?;
    let wrapped = match report {
        -50..=-31 => report + 101,
        -30..=49 => report,
        _ => return None,
    };
    u16::try_from(wrapped + 35).ok()
}

#[cfg(test)]
mod tests {
    use super::{MessageError, normalize, pack};

    #[test]
    fn packs_a_cq_exactly_as_wsjtx_does() -> Result<(), MessageError> {
        // From WSJT-X's ft8code "CQ K9KSU EN52".
        assert_eq!(
            pack("CQ K9KSU EN52")?.to_bit_string(),
            "00000000000000000000000000100000010011110011000111010010000010000101101000001"
        );
        Ok(())
    }

    #[test]
    fn case_and_spacing_do_not_matter() -> Result<(), MessageError> {
        assert_eq!(pack(" cq  k9ksu en52 ")?, pack("CQ K9KSU EN52")?);
        assert_eq!(normalize(" cq  k9ksu en52 "), "CQ K9KSU EN52");
        Ok(())
    }

    #[test]
    fn refuses_what_it_cannot_encode_yet() {
        assert_eq!(pack("   "), Err(MessageError::Empty));
        assert!(matches!(
            pack("K9KSU/7 W1AW -10"),
            Err(MessageError::Unsupported(_))
        ));
        assert!(matches!(
            pack("TNX FOR QSO 73"),
            Err(MessageError::Unsupported(_))
        ));
        for refused in [
            "QA1BC W1AW -10",
            "K9KSU W1AW +55",
            "AB1 W1AW",
            "K9KSUX W1AW",
        ] {
            assert!(
                matches!(pack(refused), Err(MessageError::Unsupported(_))),
                "{refused}"
            );
        }
        assert!(matches!(pack("K9KSU"), Err(MessageError::Unsupported(_))));
    }
}
