// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Soft-decision decoding of FT8's (174,91) LDPC code by belief propagation
//! (the sum-product algorithm in the log domain).
//!
//! Input is one log-likelihood ratio per codeword bit, `ln(P(0) / P(1))`:
//! positive means the bit is more likely 0. Output, when every parity check
//! is satisfied, is the 174 decided bits.

use super::ldpc_checks::CHECKS;
use super::ldpc_generator::GENERATOR;

/// Bits in a codeword.
pub const N: usize = 174;

/// Largest magnitude a message may take, so `atanh` never sees exactly 1.
const MESSAGE_LIMIT: f32 = 19.0;

/// Rounds without fewer failing checks after which decoding gives up: noise
/// rarely improves, so this saves most of the time spent on false
/// candidates.
const PATIENCE: usize = 10;

/// Decodes `llr` in at most `max_iterations` rounds. Returns the codeword
/// and the number of rounds used, or `None` if the checks were never all
/// satisfied.
#[must_use]
pub fn decode(llr: &[f32; N], max_iterations: usize) -> Option<([bool; N], usize)> {
    let phi = phi_table();
    // Check-to-bit messages, one per edge, in the order of CHECKS.
    let mut to_bit: Vec<Vec<f32>> = CHECKS.iter().map(|c| vec![0.0; c.len()]).collect();
    let mut total = *llr;
    let mut bits = hard_decision(&total);
    if satisfied(&bits) {
        return Some((bits, 0));
    }
    let mut fewest_failing = failing(&bits);
    let mut stale = 0;
    for iteration in 1..=max_iterations {
        for (check, messages) in CHECKS.iter().zip(to_bit.iter_mut()) {
            // Check update in the "phi" form of the sum-product rule: the
            // magnitude is phi(sum of phi(|q|)) over the other bits, the
            // sign the product of their signs. phi is its own inverse.
            let mut phis = [0.0_f32; 7];
            let mut negative = [false; 7];
            let mut phi_sum = 0.0;
            let mut odd = false;
            for (k, (&bit, &previous)) in check.iter().zip(messages.iter()).enumerate() {
                let q = total[bit] - previous;
                phis[k] = phi.at(q.abs());
                negative[k] = q < 0.0;
                phi_sum += phis[k];
                odd ^= negative[k];
            }
            for (k, message) in messages.iter_mut().enumerate() {
                let magnitude = phi.at(phi_sum - phis[k]).min(MESSAGE_LIMIT);
                *message = if odd ^ negative[k] {
                    -magnitude
                } else {
                    magnitude
                };
            }
        }
        total = *llr;
        for (check, messages) in CHECKS.iter().zip(&to_bit) {
            for (&bit, &m) in check.iter().zip(messages) {
                total[bit] += m;
            }
        }
        bits = hard_decision(&total);
        let now = failing(&bits);
        if now == 0 {
            return Some((bits, iteration));
        }
        if now < fewest_failing {
            fewest_failing = now;
            stale = 0;
        } else {
            stale += 1;
            if stale >= PATIENCE {
                return None;
            }
        }
    }
    None
}

/// A codeword-length bit set.
type Bits = [u64; 3];

fn get(bits: &Bits, i: usize) -> bool {
    (bits[i / 64] >> (i % 64)) & 1 == 1
}

fn flip(bits: &mut Bits, i: usize) {
    bits[i / 64] ^= 1 << (i % 64);
}

fn xor(a: &mut Bits, b: &Bits) {
    for (x, y) in a.iter_mut().zip(b) {
        *x ^= y;
    }
}

/// Rows of the code's generator matrix: row `j` is the codeword of the
/// message with only bit `j` set.
fn generator_rows() -> &'static [Bits; 91] {
    static ROWS: std::sync::OnceLock<[Bits; 91]> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        std::array::from_fn(|j| {
            let mut row = [0_u64; 3];
            flip(&mut row, j);
            for (i, g) in GENERATOR.iter().enumerate() {
                if (g >> (90 - j)) & 1 == 1 {
                    flip(&mut row, 91 + i);
                }
            }
            row
        })
    })
}

/// Ordered-statistics decoding, for when belief propagation fails.
///
/// The 91 most reliable bits that determine a codeword are taken as given,
/// and the codeword they imply is tested, along with every codeword that
/// differs from it in one of those bits or in two of the `pair_span` least
/// reliable of them. The codeword that disagrees least with the received
/// likelihoods wins. Returns it with its disagreement (the sum of `|llr|`
/// over the bits where it differs from the hard decisions).
///
/// Unlike belief propagation this always returns some codeword, so callers
/// must check it (the CRC, and the disagreement) before trusting it.
#[must_use]
pub fn osd(llr: &[f32; N], pair_span: usize) -> Option<([bool; N], f32)> {
    let mut order: Vec<usize> = (0..N).collect();
    order.sort_by(|&a, &b| llr[b].abs().total_cmp(&llr[a].abs()));

    // Gaussian elimination: find 91 independent columns in reliability
    // order, each a pivot of one row.
    let mut rows: Vec<Bits> = generator_rows().to_vec();
    let mut pivots: Vec<usize> = Vec::with_capacity(91);
    for &column in &order {
        if pivots.len() == 91 {
            break;
        }
        let r = pivots.len();
        let Some(found) = (r..91).find(|&k| get(&rows[k], column)) else {
            continue;
        };
        rows.swap(r, found);
        let pivot_row = rows[r];
        for (k, row) in rows.iter_mut().enumerate() {
            if k != r && get(row, column) {
                xor(row, &pivot_row);
            }
        }
        pivots.push(column);
    }
    if pivots.len() < 91 {
        return None;
    }

    let hard: Bits = {
        let mut h = [0_u64; 3];
        for (i, &l) in llr.iter().enumerate() {
            if l < 0.0 {
                flip(&mut h, i);
            }
        }
        h
    };
    let cost = |word: &Bits| -> f32 {
        let mut diff = *word;
        xor(&mut diff, &hard);
        (0..N)
            .filter(|&i| get(&diff, i))
            .map(|i| llr[i].abs())
            .sum()
    };

    // The codeword taking the hard decisions at the pivots.
    let mut base = [0_u64; 3];
    for (r, &column) in pivots.iter().enumerate() {
        if get(&hard, column) {
            xor(&mut base, &rows[r]);
        }
    }
    let mut best = (cost(&base), base);
    let mut consider = |word: Bits| {
        let c = cost(&word);
        if c < best.0 {
            best = (c, word);
        }
    };
    for row in &rows {
        let mut word = base;
        xor(&mut word, row);
        consider(word);
    }
    // Pairs among the least reliable pivots (the last ones found).
    let span = pair_span.min(91);
    for a in 91 - span..91 {
        for b in a + 1..91 {
            let mut word = base;
            xor(&mut word, &rows[a]);
            xor(&mut word, &rows[b]);
            consider(word);
        }
    }
    let (distance, word) = best;
    Some((std::array::from_fn(|i| get(&word, i)), distance))
}

/// `phi(x) = ln((e^x + 1) / (e^x - 1))`, tabulated with linear
/// interpolation: the sum-product rule's one transcendental function.
struct Phi {
    values: Vec<f32>,
}

/// Table range and resolution: phi is negligible past 20 and steep near 0.
const PHI_MAX: f32 = 20.0;
const PHI_STEPS: usize = 8_192;

impl Phi {
    fn at(&self, x: f32) -> f32 {
        let position = (x.max(0.0) / PHI_MAX * to_f32(PHI_STEPS)).min(to_f32(PHI_STEPS - 1));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped above
        let i = position as usize;
        let fraction = position - to_f32(i);
        let next = self.values.get(i + 1).copied().unwrap_or(0.0);
        self.values[i] + fraction * (next - self.values[i])
    }
}

fn phi_table() -> &'static Phi {
    static TABLE: std::sync::OnceLock<Phi> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Phi {
        values: (0..=PHI_STEPS)
            .map(|i| {
                // Start half a step in, so phi(0) is large but finite.
                let x = (to_f32(i).max(0.25)) * PHI_MAX / to_f32(PHI_STEPS);
                ((x.exp() + 1.0) / (x.exp() - 1.0)).ln().min(MESSAGE_LIMIT)
            })
            .collect(),
    })
}

#[allow(clippy::cast_precision_loss)] // small
fn to_f32(n: usize) -> f32 {
    n as f32
}

/// How many parity checks have an odd number of ones.
fn failing(bits: &[bool; N]) -> usize {
    CHECKS
        .iter()
        .filter(|check| check.iter().filter(|&&b| bits[b]).count() % 2 == 1)
        .count()
}

/// The bit each likelihood favours.
fn hard_decision(llr: &[f32; N]) -> [bool; N] {
    llr.map(|l| l < 0.0)
}

/// Whether every parity check has an even number of ones.
#[must_use]
pub fn satisfied(bits: &[bool; N]) -> bool {
    failing(bits) == 0
}

#[cfg(test)]
mod tests {
    use super::{N, decode, satisfied};
    use crate::ft8::bench::Rng;
    use crate::ft8::encode::codeword;
    use crate::ft8::message::{MessageError, pack};

    #[test]
    fn every_generated_codeword_passes_every_check() -> Result<(), MessageError> {
        let mut rng = Rng::new(9);
        for _ in 0..500 {
            let message = crate::ft8::bench::random_message(&mut rng);
            assert!(satisfied(&codeword(pack(&message)?)), "{message}");
        }
        Ok(())
    }

    #[test]
    fn osd_corrects_what_belief_propagation_cannot() -> Result<(), MessageError> {
        let sent = codeword(pack("W1AW K9KSU R-15")?);
        let mut rng = Rng::new(21);
        let mut rescued = 0;
        for _ in 0..40 {
            let mut llr = [0.0_f32; N];
            for (l, &bit) in llr.iter_mut().zip(&sent) {
                let clean = if bit { -1.0 } else { 1.0 };
                #[allow(clippy::cast_possible_truncation)]
                let received = (clean + 0.95 * rng.gaussian()) as f32;
                *l = 2.0 * received / 0.9;
            }
            if decode(&llr, 40).is_none() {
                let (word, _) = super::osd(&llr, 40).ok_or(MessageError::Empty)?;
                rescued += usize::from(word == sent);
            }
        }
        assert!(rescued >= 3, "OSD rescued only {rescued}");
        Ok(())
    }

    #[test]
    fn corrects_noisy_bits() -> Result<(), MessageError> {
        let sent = codeword(pack("CQ K9KSU EN52")?);
        let mut rng = Rng::new(4);
        // Moderately noisy channel: about one bit in twelve arrives wrong.
        let mut llr = [0.0_f32; N];
        let mut flipped = 0;
        for (l, &bit) in llr.iter_mut().zip(&sent) {
            let clean = if bit { -1.0 } else { 1.0 };
            #[allow(clippy::cast_possible_truncation)]
            let received = (clean + 0.7 * rng.gaussian()) as f32;
            flipped += usize::from((received < 0.0) != bit);
            *l = 2.0 * received / 0.49;
        }
        assert!(flipped > 5, "the test channel should flip some bits");
        let (decoded, _) = decode(&llr, 30).ok_or(MessageError::Empty)?;
        assert_eq!(decoded, sent);
        Ok(())
    }
}
