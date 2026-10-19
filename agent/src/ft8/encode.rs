// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FT8 payload to channel tones: CRC, LDPC forward error correction, and the
//! Gray-coded 8-tone symbols with their three Costas sync arrays.

use super::SYMBOLS;
use super::ldpc_generator::GENERATOR;
use super::message::{self, MessageError, Payload};

/// The 7-tone Costas array sent at the start, middle, and end.
pub const COSTAS: [u8; 7] = [3, 1, 4, 0, 6, 5, 2];

/// Symbol positions where a Costas array starts.
pub const COSTAS_POSITIONS: [usize; 3] = [0, 36, 72];

/// Gray code from three codeword bits to a tone number.
const GRAY: [u8; 8] = [0, 1, 3, 2, 5, 6, 4, 7];

/// FT8's CRC-14 polynomial, without its leading bit.
const CRC_POLYNOMIAL: u16 = 0x2757;

/// The 79 tones (each 0 to 7) of one FT8 transmission.
pub type Tones = [u8; SYMBOLS];

/// The 14-bit CRC FT8 appends to a payload: computed over the 77 payload bits
/// followed by five zero bits.
#[must_use]
pub fn crc14(payload: Payload) -> u16 {
    let mut remainder: u16 = 0;
    let bits = (0..Payload::BITS)
        .rev()
        .map(|i| (payload.value() >> i) & 1 == 1)
        .chain(std::iter::repeat_n(false, 5));
    for bit in bits {
        let top = (remainder >> 13) & 1 == 1;
        remainder = (remainder << 1) & 0x3fff;
        if top != bit {
            remainder ^= CRC_POLYNOMIAL;
        }
    }
    remainder
}

/// The 174-bit codeword: 77 payload bits, the 14-bit CRC, and 83 parity bits,
/// first bit sent first, as a list of bits.
#[must_use]
pub fn codeword(payload: Payload) -> [bool; 174] {
    let message91 = payload.value() << 14 | u128::from(crc14(payload));
    let mut bits = [false; 174];
    for (i, bit) in bits.iter_mut().take(91).enumerate() {
        *bit = (message91 >> (90 - i)) & 1 == 1;
    }
    for (bit, row) in bits[91..].iter_mut().zip(GENERATOR) {
        *bit = (message91 & row).count_ones() % 2 == 1;
    }
    bits
}

/// The tones for a payload.
#[must_use]
pub fn tones_for_payload(payload: Payload) -> Tones {
    let code = codeword(payload);
    let mut tones = [0_u8; SYMBOLS];
    let mut data = code.chunks_exact(3).map(|three| {
        let index = usize::from(three[0]) << 2 | usize::from(three[1]) << 1 | usize::from(three[2]);
        GRAY[index]
    });
    for (position, tone) in tones.iter_mut().enumerate() {
        let sync = COSTAS_POSITIONS
            .iter()
            .find(|&&start| (start..start + COSTAS.len()).contains(&position));
        *tone = match sync {
            Some(&start) => COSTAS[position - start],
            None => data.next().unwrap_or(0),
        };
    }
    tones
}

/// The tones for message text.
///
/// # Errors
///
/// Whatever [`message::pack`] refuses.
pub fn tones(text: &str) -> Result<Tones, MessageError> {
    message::pack(text).map(tones_for_payload)
}

/// Tones written as 79 digits, as WSJT-X's `ft8code` prints them.
#[must_use]
pub fn tones_to_string(tones: &Tones) -> String {
    tones.iter().map(|t| char::from(b'0' + t)).collect()
}

#[cfg(test)]
mod tests {
    use super::{crc14, tones, tones_to_string};
    use crate::ft8::message::{MessageError, pack};

    #[test]
    fn crc_matches_wsjtx() -> Result<(), MessageError> {
        // ft8code "CQ K9KSU EN52": 14-bit CRC 00110100111100.
        assert_eq!(
            format!("{:014b}", crc14(pack("CQ K9KSU EN52")?)),
            "00110100111100"
        );
        Ok(())
    }

    #[test]
    fn tones_match_wsjtx() -> Result<(), MessageError> {
        assert_eq!(
            tones_to_string(&tones("CQ K9KSU EN52")?),
            "3140652000000001005754145505123032323140652415747514366256037622025302753140652"
        );
        Ok(())
    }
}
