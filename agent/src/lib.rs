// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LogLab Station Agent library.
//!
//! The Agent owns the radios: it is the only component that opens a radio's
//! port or control connection, it presents every radio through one common rig
//! model, and it is the only code that can key a transmitter. See
//! `CONSTITUTION.md` at the repository root.

pub mod ft8;
