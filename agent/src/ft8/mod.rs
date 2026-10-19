// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! FT8: building transmissions and measuring decoders.
//!
//! FT8 was designed by Joe Taylor, K1JT, Steve Franke, K9AN, and Bill
//! Somerville, G4WJS, and is described in "The FT4 and FT8 Communication
//! Protocols" (QEX, July/August 2020). This module turns a message into the
//! 79 tones FT8 sends ([`encode`]) and into audio ([`wave`]), and holds the
//! test bench ([`bench`]) that every FT8 decoder in LogLab must pass before it
//! is trusted: synthetic recordings with known signals, and a score of what a
//! decoder found against what was really there.
//!
//! Nothing here keys a transmitter. Audio built here is only ever written to
//! files by the bench; sending it is the Agent's transmit path's job, under
//! Principles XI and XIII.

pub mod bench;
pub mod encode;
mod ldpc_generator;
pub mod message;
pub mod wave;

/// Audio sample rate FT8 is defined at, in samples per second.
pub const SAMPLE_RATE: u32 = 12_000;

/// Audio samples in one FT8 symbol (0.16 s).
pub const SAMPLES_PER_SYMBOL: usize = 1_920;

/// Symbols in one FT8 transmission: 58 data symbols and three 7-symbol sync
/// (Costas) arrays.
pub const SYMBOLS: usize = 79;

/// Spacing between FT8's eight tones, in hertz.
pub const TONE_SPACING_HZ: f64 = 6.25;

/// Length of one FT8 time slot, in seconds.
pub const SLOT_SECONDS: u32 = 15;

/// Audio samples in one 15-second slot.
pub const SLOT_SAMPLES: usize = 180_000;

/// Seconds from the start of a slot to the nominal start of a transmission.
pub const NOMINAL_START_SECONDS: f64 = 0.5;
