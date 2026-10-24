// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The Icom CI-V protocol: messages and numbers.
//!
//! Every CI-V message has the same shape:
//!
//! ```text
//! FE FE  to  from  command [sub-command] [data...]  FD
//! ```
//!
//! Two `FE` bytes start a message, the address it is for comes next, then the
//! address it is from, then what it says, and `FD` ends it. A radio answers a
//! command that changes something with `FB` ("done") or `FA` ("can't").
//!
//! Numbers are binary-coded decimal: each byte holds two decimal digits, one
//! per half. A frequency is five such bytes, lowest digits first, so
//! 14.074000 MHz is `00 40 07 14 00`. A level (0 to 255) is two bytes,
//! highest digits first, so 128 is `01 28`.
//!
//! The byte stream from a radio is untrusted (Principle XII): the [`Decoder`]
//! caps how long a message may be, skips anything between messages, and
//! never panics on what it is given.

/// The byte that, twice, starts every message.
pub const PREAMBLE: u8 = 0xFE;
/// The byte that ends every message.
pub const END: u8 = 0xFD;
/// A radio's answer meaning "done".
pub const OK: u8 = 0xFB;
/// A radio's answer meaning "can't".
pub const NG: u8 = 0xFA;
/// The address meaning "everyone": a radio reporting a change on its own
/// sends to it.
pub const BROADCAST: u8 = 0x00;
/// The longest message the decoder accepts, from `to` to the last data byte.
pub const MAX_MESSAGE: usize = 64;
/// The highest frequency five BCD bytes can hold, in hertz.
pub const MAX_BCD_HZ: u64 = 9_999_999_999;

/// One CI-V message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The address it is for.
    pub to: u8,
    /// The address it is from.
    pub from: u8,
    /// The command, any sub-command, and the data.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A message from `from` to `to`.
    #[must_use]
    pub fn new(to: u8, from: u8, payload: &[u8]) -> Self {
        Self {
            to,
            from,
            payload: payload.to_vec(),
        }
    }

    /// The bytes that go on the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.payload.len() + 5);
        bytes.extend([PREAMBLE, PREAMBLE, self.to, self.from]);
        bytes.extend(&self.payload);
        bytes.push(END);
        bytes
    }
}

/// Turns a stream of bytes, arriving in pieces of any size, into messages.
#[derive(Debug, Default)]
pub struct Decoder {
    message: Vec<u8>,
    preambles: u8,
    inside: bool,
}

impl Decoder {
    /// Takes the next bytes and returns every message they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if !self.inside {
                if byte == PREAMBLE {
                    self.preambles = self.preambles.saturating_add(1);
                    if self.preambles >= 2 {
                        self.inside = true;
                        self.message.clear();
                    }
                } else {
                    self.preambles = 0;
                }
                continue;
            }
            match byte {
                // Extra FE bytes before a message are allowed.
                PREAMBLE if self.message.is_empty() => {}
                // An FE in the middle starts over: the message was cut off.
                PREAMBLE => self.restart(1),
                END => {
                    if let [to, from, payload @ ..] = self.message.as_slice()
                        && !payload.is_empty()
                    {
                        frames.push(Frame::new(*to, *from, payload));
                    }
                    self.restart(0);
                }
                _ if self.message.len() >= MAX_MESSAGE => self.restart(0),
                _ => self.message.push(byte),
            }
        }
        frames
    }

    fn restart(&mut self, preambles: u8) {
        self.message.clear();
        self.inside = false;
        self.preambles = preambles;
    }
}

/// One decimal digit as a byte.
fn digit(n: u64) -> u8 {
    u8::try_from(n % 10).unwrap_or(0)
}

/// A frequency as five BCD bytes, lowest digits first. `None` above
/// [`MAX_BCD_HZ`].
#[must_use]
pub fn frequency_to_bcd(hz: u64) -> Option<[u8; 5]> {
    if hz > MAX_BCD_HZ {
        return None;
    }
    let mut out = [0; 5];
    let mut rest = hz;
    for byte in &mut out {
        let low = digit(rest);
        let high = digit(rest / 10);
        *byte = (high << 4) | low;
        rest /= 100;
    }
    Some(out)
}

/// A frequency from five BCD bytes, lowest digits first. `None` if there
/// aren't exactly five or any half-byte isn't a decimal digit.
#[must_use]
pub fn frequency_from_bcd(bytes: &[u8]) -> Option<u64> {
    if bytes.len() != 5 {
        return None;
    }
    let mut hz = 0;
    for &byte in bytes.iter().rev() {
        let (high, low) = (byte >> 4, byte & 0x0F);
        if high > 9 || low > 9 {
            return None;
        }
        hz = hz * 100 + u64::from(high) * 10 + u64::from(low);
    }
    Some(hz)
}

/// A level (0 to 9999) as two BCD bytes, highest digits first. `None`
/// above 9999.
#[must_use]
pub fn level_to_bcd(level: u16) -> Option<[u8; 2]> {
    if level > 9999 {
        return None;
    }
    let n = u64::from(level);
    Some([
        (digit(n / 1000) << 4) | digit(n / 100),
        (digit(n / 10) << 4) | digit(n),
    ])
}

/// A level from two BCD bytes, highest digits first. `None` if there aren't
/// exactly two or any half-byte isn't a decimal digit.
#[must_use]
pub fn level_from_bcd(bytes: &[u8]) -> Option<u16> {
    let [a, b] = bytes else { return None };
    let mut level = 0;
    for half in [a >> 4, a & 0x0F, b >> 4, b & 0x0F] {
        if half > 9 {
            return None;
        }
        level = level * 10 + u16::from(half);
    }
    Some(level)
}

/// Bytes as hexadecimal pairs, the way CI-V documents write them.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        Decoder, Frame, MAX_BCD_HZ, MAX_MESSAGE, frequency_from_bcd, frequency_to_bcd, hex,
        level_from_bcd, level_to_bcd,
    };

    #[test]
    fn frequencies_are_bcd_lowest_digits_first() {
        assert_eq!(
            frequency_to_bcd(14_074_000),
            Some([0x00, 0x40, 0x07, 0x14, 0x00])
        );
        assert_eq!(
            frequency_to_bcd(146_520_000),
            Some([0x00, 0x00, 0x52, 0x46, 0x01])
        );
        assert_eq!(
            frequency_from_bcd(&[0x00, 0x40, 0x07, 0x14, 0x00]),
            Some(14_074_000)
        );
        for hz in [0, 1, 30_000, 7_074_000, 74_800_000, MAX_BCD_HZ] {
            let bytes = frequency_to_bcd(hz).unwrap_or_else(|| unreachable!());
            assert_eq!(frequency_from_bcd(&bytes), Some(hz));
        }
        assert_eq!(frequency_to_bcd(MAX_BCD_HZ + 1), None);
    }

    #[test]
    fn refuses_frequencies_that_are_not_bcd() {
        assert_eq!(frequency_from_bcd(&[0x0A, 0, 0, 0, 0]), None);
        assert_eq!(frequency_from_bcd(&[0, 0, 0, 0, 0xA0]), None);
        assert_eq!(frequency_from_bcd(&[0, 0, 0, 0]), None);
        assert_eq!(frequency_from_bcd(&[0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn levels_are_bcd_highest_digits_first() {
        assert_eq!(level_to_bcd(128), Some([0x01, 0x28]));
        assert_eq!(level_to_bcd(255), Some([0x02, 0x55]));
        assert_eq!(level_from_bcd(&[0x02, 0x55]), Some(255));
        assert_eq!(level_from_bcd(&[0x02, 0x5A]), None);
        assert_eq!(level_from_bcd(&[0x02]), None);
        assert_eq!(level_to_bcd(10_000), None);
    }

    #[test]
    fn a_frame_encodes_with_preamble_and_end() {
        let frame = Frame::new(0x94, 0xE0, &[0x19, 0x00]);
        assert_eq!(hex(&frame.encode()), "FE FE 94 E0 19 00 FD");
    }

    #[test]
    fn decodes_messages_arriving_in_pieces() {
        let mut decoder = Decoder::default();
        let wire = [
            Frame::new(0xE0, 0x94, &[0x03, 0x00, 0x40, 0x07, 0x14, 0x00]).encode(),
            Frame::new(0xE0, 0x94, &[0xFB]).encode(),
        ]
        .concat();
        let mut frames = Vec::new();
        for byte in &wire {
            frames.extend(decoder.push(std::slice::from_ref(byte)));
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].payload, [0xFB]);
    }

    #[test]
    fn skips_noise_and_cut_off_messages() {
        let mut decoder = Decoder::default();
        // Noise, a message cut off by a new one, extra preambles, and a
        // message too short to have a command.
        let wire = [
            vec![0x12, 0x34, 0xFD, 0xFE, 0x00],
            vec![0xFE, 0xFE, 0x94, 0xE0, 0x03],
            vec![0xFE, 0xFE, 0xFE, 0x94, 0xE0, 0x04, 0xFD],
            vec![0xFE, 0xFE, 0x94, 0xE0, 0xFD],
        ]
        .concat();
        let frames = decoder.push(&wire);
        assert_eq!(frames, [Frame::new(0x94, 0xE0, &[0x04])]);
    }

    #[test]
    fn refuses_a_message_that_never_ends() {
        let mut decoder = Decoder::default();
        let mut wire = vec![0xFE, 0xFE];
        wire.extend(std::iter::repeat_n(0x11, MAX_MESSAGE * 4));
        wire.push(0xFD);
        assert!(decoder.push(&wire).is_empty());
        // And it recovers for the next message.
        let next = Frame::new(0x94, 0xE0, &[0x03]).encode();
        assert_eq!(decoder.push(&next).len(), 1);
    }
}
