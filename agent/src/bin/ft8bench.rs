// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `ft8bench`: the FT8 decoder test bench (a development tool, not part of
//! the running Agent).
//!
//! ```text
//! ft8bench generate DIR [--seed N] [--slots N] [--signals N]
//!     Write slot-NNNN.wav recordings and truth.tsv into DIR.
//! ft8bench score TRUTH DECODES-DIR [--name NAME]
//!     Score slot-NNNN.txt decoder output against the truth file.
//! ft8bench tones MESSAGE
//!     Print the 79 tones of a message.
//! ft8bench messages [--seed N] [--count N]
//!     Print random standard messages, one per line.
//! ```

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use loglab_agent::ft8::bench::{self, Rng, ScenarioSpec};
use loglab_agent::ft8::encode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "ft8bench: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one command and returns what it prints.
fn run(args: &[String]) -> Result<String, String> {
    let (command, rest) = args
        .split_first()
        .ok_or("usage: ft8bench generate|score|tones|messages ...")?;
    let (positional, options) = split_options(rest)?;
    match (command.as_str(), positional.as_slice()) {
        ("generate", [dir]) => generate(Path::new(dir), &options),
        ("score", [truth, decodes]) => score(Path::new(truth), Path::new(decodes), &options),
        ("tones", [message]) => encode::tones(message)
            .map(|t| format!("{}\n", encode::tones_to_string(&t)))
            .map_err(|e| e.to_string()),
        ("messages", []) => {
            let mut rng = Rng::new(number(&options, "seed", 1)?);
            Ok((0..number(&options, "count", 100)?)
                .map(|_| bench::random_message(&mut rng) + "\n")
                .collect())
        }
        _ => Err(format!(
            "unknown command or wrong arguments: {}",
            args.join(" ")
        )),
    }
}

/// Splits `--name value` options from positional arguments.
fn split_options(args: &[String]) -> Result<(Vec<String>, BTreeMap<String, String>), String> {
    let mut positional = Vec::new();
    let mut options = BTreeMap::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = iter.next().ok_or(format!("--{name} needs a value"))?;
            options.insert(name.to_owned(), value.clone());
        } else {
            positional.push(arg.clone());
        }
    }
    Ok((positional, options))
}

fn number<T: std::str::FromStr>(
    options: &BTreeMap<String, String>,
    name: &str,
    default: T,
) -> Result<T, String> {
    options.get(name).map_or(Ok(default), |v| {
        v.parse().map_err(|_| format!("--{name} must be a number"))
    })
}

fn generate(dir: &Path, options: &BTreeMap<String, String>) -> Result<String, String> {
    let standard = ScenarioSpec::standard(number(options, "seed", 1)?);
    let spec = ScenarioSpec {
        slots: number(options, "slots", standard.slots)?,
        signals_per_slot: number(options, "signals", standard.signals_per_slot)?,
        ..standard
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let slots = bench::generate(&spec);
    for slot in &slots {
        let audio = bench::render(slot, spec.seed).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.wav", slot.file_stem()));
        write(&path, &bench::wav_bytes(&audio))?;
    }
    write(
        &dir.join("truth.tsv"),
        bench::truth_tsv(&spec, &slots).as_bytes(),
    )?;
    Ok(format!(
        "wrote {} slots with {} signals each to {}\n",
        spec.slots,
        spec.signals_per_slot,
        dir.display()
    ))
}

fn score(
    truth_path: &Path,
    decodes_dir: &Path,
    options: &BTreeMap<String, String>,
) -> Result<String, String> {
    let text = read(truth_path)?;
    let truth = bench::parse_truth(&text)
        .map_err(|line| format!("{}: line {line} is not a truth line", truth_path.display()))?;
    let mut decoded = BTreeMap::new();
    for slot in &truth {
        let path: PathBuf = decodes_dir.join(format!("{}.txt", slot.file_stem()));
        if let Ok(output) = std::fs::read_to_string(&path) {
            let messages = output
                .lines()
                .filter_map(bench::parse_decode_line)
                .collect();
            decoded.insert(slot.index, messages);
        }
    }
    let name = options.get("name").map_or("Decoder", String::as_str);
    Ok(bench::report(name, &bench::score(&truth, &decoded)))
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
