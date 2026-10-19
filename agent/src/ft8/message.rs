// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FT8 message text to its 77-bit payload ("source encoding").
//!
//! Two kinds of message are supported so far:
//!
//! - The standard message (type 1): two standard callsigns, or CQ (with an
//!   optional modifier such as `CQ DX` or `CQ FD`) and a callsign, followed
//!   by a grid square, a signal report, `RRR`, `RR73`, `73`, or nothing.
//!   That covers ordinary FT8 QSOs.
//! - The ARRL Field Day exchange (type 0.3, or 0.4 above 16 transmitters):
//!   two standard callsigns, an optional `R`, the number of transmitters and
//!   the class (`3A`), and the ARRL or RAC section (`EMA`), or `DX`.
//!
//! Hashed and compound callsigns, free text, and the other contest
//! exchanges come later; until then they are refused with
//! [`MessageError::Unsupported`] rather than guessed.

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

/// Payload subtypes (with type 0) of the Field Day exchange, for 1 to 16
/// transmitters and for 17 to 32.
const N3_FIELD_DAY: u128 = 3;
const N3_FIELD_DAY_LARGE: u128 = 4;

/// The sections a Field Day exchange can carry, in the order of their
/// 7-bit numbers, which start at 1. The order is fixed by the protocol (it
/// is not alphabetical: a few places were reused when sections changed).
const SECTIONS: [&str; 86] = [
    "AB", "AK", "AL", "AR", "AZ", "BC", "CO", "CT", "DE", "EB", "EMA", "ENY", "EPA", "EWA", "GA",
    "GH", "IA", "ID", "IL", "IN", "KS", "KY", "LA", "LAX", "NS", "MB", "MDC", "ME", "MI", "MN",
    "MO", "MS", "MT", "NC", "ND", "NE", "NFL", "NH", "NL", "NLI", "NM", "NNJ", "NNY", "TER", "NTX",
    "NV", "OH", "OK", "ONE", "ONN", "ONS", "OR", "ORG", "PAC", "PR", "QC", "RI", "SB", "SC", "SCV",
    "SD", "SDG", "SF", "SFL", "SJV", "SK", "SNJ", "STX", "SV", "TN", "UT", "VA", "VI", "VT", "WCF",
    "WI", "WMA", "WNY", "WPA", "WTX", "WV", "WWA", "WY", "DX", "PE", "NB",
];

/// Field Day classes, by their 3-bit number.
const CLASSES: &str = "ABCDEF";

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

    /// A payload from its 77 bits, first bit sent first.
    #[must_use]
    pub fn from_bits(bits: &[bool; 77]) -> Self {
        Self(bits.iter().fold(0, |v, &b| v << 1 | u128::from(b)))
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
    if let Some(payload) = field_day(&words)? {
        return Ok(payload);
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

/// The Field Day exchange `CALL CALL [R] 3A EMA`, or `None` if the words
/// are not shaped like one.
fn field_day(words: &[&str]) -> Result<Option<Payload>, MessageError> {
    let (calls, roger, exchange) = match words {
        [to, de, "R", class, section] => ([to, de], true, [class, section]),
        [to, de, class, section] => ([to, de], false, [class, section]),
        _ => return Ok(None),
    };
    let Some((transmitters, class)) = field_day_class(exchange[0]) else {
        return Ok(None);
    };
    let Some(section) = SECTIONS.iter().position(|s| s == exchange[1]) else {
        return Ok(None);
    };
    let (n3, count) = if transmitters <= 16 {
        (N3_FIELD_DAY, transmitters - 1)
    } else {
        (N3_FIELD_DAY_LARGE, transmitters - 17)
    };
    let to = standard_call(calls[0])?;
    let de = standard_call(calls[1])?;
    let value = u128::from(to) << 49
        | u128::from(de) << 21
        | u128::from(roger) << 20
        | u128::from(count) << 16
        | u128::from(class) << 13
        | (section as u128 + 1) << 6
        | n3 << 3;
    Ok(Some(Payload(value)))
}

/// `3A`: the number of transmitters (1 to 32) and the class's number.
fn field_day_class(word: &str) -> Option<(u32, u32)> {
    let (digits, letter) = word.split_at(word.len().checked_sub(1)?);
    let class = CLASSES.find(letter)?;
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let transmitters: u32 = digits.parse().ok()?;
    ((1..=32).contains(&transmitters)).then_some((transmitters, u32::try_from(class).ok()?))
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

/// The text of a received payload, as WSJT-X shows it. `None` for message
/// kinds this decoder does not read yet (anything but a standard message),
/// and for values no encoder produces.
#[must_use]
pub fn unpack(payload: Payload) -> Option<String> {
    let v = payload.value();
    let i3 = v & 0b111;
    let n3 = (v >> 3) & 0b111;
    if i3 == 0 && (n3 == N3_FIELD_DAY || n3 == N3_FIELD_DAY_LARGE) {
        return unpack_field_day(v, n3 == N3_FIELD_DAY_LARGE);
    }
    if i3 != I3_STANDARD {
        return None;
    }
    let field = |shift: u32, bits: u32| u32::try_from((v >> shift) & ((1 << bits) - 1)).ok();
    let first = field(48, 29)?;
    let second = field(19, 29)?;
    let ir = field(18, 1)? == 1;
    let g15 = u16::try_from(field(3, 15)?).ok()?;

    let mut to = unpack_call(first >> 1)?;
    let mut de = unpack_call(second >> 1)?;
    if first & 1 == 1 {
        to.push_str("/R");
    }
    if second & 1 == 1 {
        de.push_str("/R");
    }
    if de == "CQ" || de.starts_with("CQ ") || de == "DE" || de == "QRZ" {
        return None;
    }
    let extra = unpack_extra(ir, g15)?;
    Some(
        [to, de, extra]
            .iter()
            .filter(|w| !w.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// A Field Day exchange back to text.
fn unpack_field_day(v: u128, large: bool) -> Option<String> {
    let field = |shift: u32, bits: u32| u32::try_from((v >> shift) & ((1 << bits) - 1)).ok();
    let to = unpack_call(field(49, 28)?)?;
    let de = unpack_call(field(21, 28)?)?;
    // Only real callsigns: the special tokens and hashes cannot appear in
    // this exchange.
    if field(49, 28)? < NTOKENS + MAX22 || field(21, 28)? < NTOKENS + MAX22 {
        return None;
    }
    let roger = field(20, 1)? == 1;
    let transmitters = field(16, 4)? + if large { 17 } else { 1 };
    let class = CLASSES.chars().nth(usize::try_from(field(13, 3)?).ok()?)?;
    let section = SECTIONS.get(usize::try_from(field(6, 7)?).ok()?.checked_sub(1)?)?;
    let mut words = vec![to, de];
    if roger {
        words.push("R".to_owned());
    }
    words.push(format!("{transmitters}{class}"));
    words.push((*section).to_owned());
    Some(words.join(" "))
}

/// A 28-bit callsign field back to text.
fn unpack_call(n28: u32) -> Option<String> {
    match n28 {
        0 => return Some("DE".to_owned()),
        1 => return Some("QRZ".to_owned()),
        2 => return Some("CQ".to_owned()),
        3..=1002 => return Some(format!("CQ {:03}", n28 - 3)),
        1003..NTOKENS => {
            let mut m = n28 - 1003;
            let mut letters = Vec::new();
            while m > 0 {
                letters.push(u8::try_from(m % 27).ok()?);
                m /= 27;
            }
            if letters.is_empty() || letters.contains(&0) {
                return None;
            }
            let word: String = letters
                .iter()
                .rev()
                .map(|&l| char::from(b'A' + l - 1))
                .collect();
            return Some(format!("CQ {word}"));
        }
        _ => {}
    }
    if n28 < NTOKENS + MAX22 {
        // A hashed callsign: the full call was sent earlier, in another
        // message type this decoder does not read yet.
        return Some("<...>".to_owned());
    }
    let mut n = n28 - NTOKENS - MAX22;
    let tables = [
        CALL_ALPHANUM_SPACE,
        CALL_ALPHANUM,
        CALL_NUMERIC,
        CALL_LETTERS_SPACE,
        CALL_LETTERS_SPACE,
        CALL_LETTERS_SPACE,
    ];
    let mut chars = [b' '; 6];
    for (c, table) in chars.iter_mut().zip(tables).rev() {
        let radix = u32::try_from(table.len()).ok()?;
        *c = *table.as_bytes().get(usize::try_from(n % radix).ok()?)?;
        n /= radix;
    }
    if n != 0 {
        return None;
    }
    let call = std::str::from_utf8(&chars).ok()?.trim();
    if call.contains(' ') || call.is_empty() {
        return None;
    }
    Some(call.to_owned())
}

/// The grid, report, or acknowledgement field back to text.
fn unpack_extra(ir: bool, g15: u16) -> Option<String> {
    if g15 <= MAXGRID4 {
        let mut n = g15;
        let d4 = n % 10;
        n /= 10;
        let d3 = n % 10;
        n /= 10;
        let b = n % 18;
        let a = n / 18;
        if a >= 18 {
            return None;
        }
        let [a, b, d3, d4] = [a, b, d3, d4].map(|x| u8::try_from(x).unwrap_or(0));
        let grid = format!(
            "{}{}{}{}",
            char::from(b'A' + a),
            char::from(b'A' + b),
            char::from(b'0' + d3),
            char::from(b'0' + d4)
        );
        return Some(if ir { format!("R {grid}") } else { grid });
    }
    let special = match g15 - MAXGRID4 {
        1 => Some(""),
        2 => Some("RRR"),
        3 => Some("RR73"),
        4 => Some("73"),
        _ => None,
    };
    if let Some(word) = special {
        return Some(word.to_owned());
    }
    let mut report = i32::from(g15 - MAXGRID4) - 35;
    if report > 49 {
        report -= 101;
    }
    Some(if ir {
        format!("R{report:+03}")
    } else {
        format!("{report:+03}")
    })
}

#[cfg(test)]
mod tests {
    use super::{MessageError, Payload, normalize, pack, unpack};

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

    #[test]
    fn unpacking_returns_the_message() -> Result<(), MessageError> {
        let mut rng = crate::ft8::bench::Rng::new(11);
        let mut messages: Vec<String> = (0..2_000)
            .map(|_| crate::ft8::bench::random_message(&mut rng))
            .collect();
        messages.extend(
            [
                "CQ DX K9KSU EN52",
                "CQ POTA W1AW FN31",
                "CQ 145 N0X AA00",
                "QRZ K9KSU EN52",
                "W1AW K9KSU",
                "W1AW K9KSU R EN52",
                "W1AW K9KSU -45",
                "W1AW K9KSU R+49",
                "K9KSU W1AW RR73",
            ]
            .map(str::to_owned),
        );
        for message in messages {
            assert_eq!(unpack(pack(&message)?).as_deref(), Some(message.as_str()));
        }
        Ok(())
    }

    #[test]
    fn packs_field_day_exactly_as_wsjtx_does() -> Result<(), MessageError> {
        // From WSJT-X's ft8code "K9KSU W1AW 2A EMA" (type 0.3).
        assert_eq!(
            pack("K9KSU W1AW 2A EMA")?.to_bit_string(),
            "00001001111001100011101001000000101111111110101011010001000010000001011011000"
        );
        Ok(())
    }

    #[test]
    fn field_day_round_trips_and_refuses_what_it_cannot_carry() -> Result<(), MessageError> {
        for message in [
            "K9KSU W1AW 1A EMA",
            "W1AW K9KSU R 16F KS",
            "K9KSU W1AW 17A DX",
            "K9KSU W1AW R 32D NB",
            "VE3XYZ K9KSU 3B GH",
        ] {
            assert_eq!(unpack(pack(message)?).as_deref(), Some(message));
        }
        for refused in [
            "K9KSU W1AW 33A EMA",
            "K9KSU W1AW 0A EMA",
            "K9KSU W1AW 3A XX",
        ] {
            assert!(pack(refused).is_err(), "{refused}");
        }
        // A class that is not A to F is not a Field Day exchange.
        assert!(pack("K9KSU W1AW 3G EMA").is_err());
        Ok(())
    }

    #[test]
    fn bits_round_trip() -> Result<(), MessageError> {
        let payload = pack("CQ K9KSU EN52")?;
        let bits: Vec<bool> = payload.to_bit_string().bytes().map(|b| b == b'1').collect();
        let bits: [bool; 77] = bits.try_into().map_err(|_| MessageError::Empty)?;
        assert_eq!(Payload::from_bits(&bits), payload);
        Ok(())
    }
}
