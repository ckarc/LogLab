// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LogLab Station Agent.
//!
//! The Agent owns the radios: it is the only component that opens a radio's
//! port or control connection, it presents every radio through one common rig
//! model, and it is the only code that can key a transmitter. This crate is an
//! empty skeleton; see `CONSTITUTION.md` at the repository root.

/// The Agent's version, from `Cargo.toml`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the line the Agent prints to identify itself.
fn banner() -> String {
    format!("loglab-agent {VERSION}")
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::{VERSION, banner};

    #[test]
    fn banner_names_the_agent_and_its_version() {
        assert_eq!(banner(), format!("loglab-agent {VERSION}"));
        assert!(!VERSION.is_empty());
    }
}
