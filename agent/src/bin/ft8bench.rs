// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `ft8bench`: the FT8 decoder test bench (a development tool, not part of
//! the running Agent).
//!
//! ```text
//! ft8bench generate DIR [--seed N] [--slots N] [--signals N]
//!                       [--min-snr DB] [--max-snr DB] [--channel CHANNEL]
//!     Write slot-NNNN.wav recordings and truth.tsv into DIR. CHANNEL is a
//!     simulated HF channel every signal passes through: a preset (quiet,
//!     moderate, disturbed, flutter) and/or name=value settings separated by
//!     commas (spread, k, deepfade, fadelen, drift, bend, wobble, period,
//!     phasenoise, echo, echodb, echoshift); see `ft8::channel`.
//! ft8bench score TRUTH DECODES-DIR [--name NAME]
//!     Score slot-NNNN.txt decoder output against the truth file.
//! ft8bench decode DIR [--out OUT-DIR]
//!     Decode every slot-NNNN.wav in DIR with LogLab's decoder, writing
//!     slot-NNNN.txt decoder output (default OUT-DIR: DIR/loglab).
//! ft8bench diagnose DIR [--decodes DECODES-DIR]
//!     For every signal in DIR's truth file, how far LogLab's decoder gets
//!     with it, step by step, as tab-separated lines (see
//!     `ft8::decode::diagnose`); with DECODES-DIR, also whether the full
//!     decoder found it there.
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
use loglab_agent::ft8::channel::Channel;
use loglab_agent::ft8::{decode, encode};

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
        ("decode", [dir]) => decode(Path::new(dir), &options),
        ("diagnose", [dir]) => diagnose(Path::new(dir), &options),
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
        min_snr_db: number(options, "min-snr", standard.min_snr_db)?,
        max_snr_db: number(options, "max-snr", standard.max_snr_db)?,
        channel: options
            .get("channel")
            .map_or(Ok(Channel::CLEAR), |c| Channel::parse(c))?,
        ..standard
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let slots = bench::generate(&spec);
    for slot in &slots {
        let audio =
            bench::render_through(slot, spec.seed, &spec.channel).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.wav", slot.file_stem()));
        write(&path, &bench::wav_bytes(&audio))?;
    }
    write(
        &dir.join("truth.tsv"),
        bench::truth_tsv(&spec, &slots).as_bytes(),
    )?;
    Ok(format!(
        "wrote {} slots with {} signals each to {} (channel: {})\n",
        spec.slots,
        spec.signals_per_slot,
        dir.display(),
        spec.channel
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
    // Every slot-NNNN.txt counts, including noise-only slots the truth
    // file has no lines for.
    let mut decoded = BTreeMap::new();
    let entries =
        std::fs::read_dir(decodes_dir).map_err(|e| format!("{}: {e}", decodes_dir.display()))?;
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        let index = path.file_name().and_then(|n| n.to_str()).and_then(|n| {
            n.strip_prefix("slot-")?
                .strip_suffix(".txt")?
                .parse::<usize>()
                .ok()
        });
        if let (Some(index), Ok(output)) = (index, std::fs::read_to_string(&path)) {
            let messages = output
                .lines()
                .filter_map(bench::parse_decode_line)
                .collect();
            decoded.insert(index, messages);
        }
    }
    let name = options.get("name").map_or("Decoder", String::as_str);
    Ok(bench::report(name, &bench::score(&truth, &decoded)))
}

fn decode(dir: &Path, options: &BTreeMap<String, String>) -> Result<String, String> {
    let out = options
        .get("out")
        .map_or_else(|| dir.join("loglab"), PathBuf::from);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut wavs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    wavs.sort();
    let started = std::time::Instant::now();
    let mut slowest = std::time::Duration::ZERO;
    for wav in &wavs {
        let bytes = std::fs::read(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
        let audio = bench::wav_samples(&bytes)
            .ok_or_else(|| format!("{}: not a 12,000 Hz mono 16-bit WAV", wav.display()))?;
        let one = std::time::Instant::now();
        let decodes = decode::decode_slot(&audio, &decode::Options::default());
        slowest = slowest.max(one.elapsed());
        let text: String = decodes.iter().map(|d| d.to_line() + "\n").collect();
        let stem = wav
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        write(&out.join(format!("{stem}.txt")), text.as_bytes())?;
    }
    Ok(format!(
        "decoded {} recordings into {} in {:.1} s (slowest {:.2} s); {} wrong codewords were caught by the CRC alone, so about {:.4} false decodes were to be expected\n",
        wavs.len(),
        out.display(),
        started.elapsed().as_secs_f64(),
        slowest.as_secs_f64(),
        decode::crc_misses(),
        to_f64(decode::crc_misses()) / 16_383.0
    ))
}

fn diagnose(dir: &Path, options: &BTreeMap<String, String>) -> Result<String, String> {
    use loglab_agent::ft8::decode::diagnose::{self, Truth};
    let truth_path = dir.join("truth.tsv");
    let truth = bench::parse_truth(&read(&truth_path)?)
        .map_err(|line| format!("{}: line {line} is not a truth line", truth_path.display()))?;
    let decoded = |slot: usize| -> Option<Vec<String>> {
        let decodes = PathBuf::from(options.get("decodes")?);
        let text = std::fs::read_to_string(decodes.join(format!("slot-{slot:04}.txt"))).ok()?;
        Some(text.lines().filter_map(bench::parse_decode_line).collect())
    };
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = truth.len().div_ceil(threads).max(1);
    let lines: Vec<Result<String, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = truth
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|slot| {
                            let path = dir.join(format!("{}.wav", slot.file_stem()));
                            let bytes = std::fs::read(&path)
                                .map_err(|e| format!("{}: {e}", path.display()))?;
                            let audio = bench::wav_samples(&bytes)
                                .ok_or_else(|| format!("{}: not a bench WAV", path.display()))?;
                            let truths: Vec<Truth> = slot
                                .signals
                                .iter()
                                .filter_map(|s| Truth::new(&s.message, s.base_hz, s.dt_s))
                                .collect();
                            let found = decoded(slot.index);
                            let single = decode::Options {
                                threads: 1,
                                ..decode::Options::default()
                            };
                            let mut out = String::new();
                            for (signal, d) in slot
                                .signals
                                .iter()
                                .zip(diagnose::diagnose(&audio, &truths, &single))
                            {
                                let mark = match &found {
                                    Some(f) if f.contains(&signal.message) => "found",
                                    Some(_) => "missed",
                                    None => "-",
                                };
                                out += &format!(
                                    "{}\t{}\t{}\t{}\t{}\t{mark}\t{}\n",
                                    slot.index,
                                    signal.snr_db,
                                    signal.base_hz,
                                    signal.dt_s,
                                    signal.message,
                                    d.to_tsv()
                                );
                            }
                            Ok(out)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut out = format!(
        "slot\tsnr\tbase_hz\tdt\tmessage\tdecoder\t{}\n",
        diagnose::tsv_header()
    );
    for line in lines {
        out += &line?;
    }
    Ok(out)
}

#[allow(clippy::cast_precision_loss)] // a count
fn to_f64(n: u64) -> f64 {
    n as f64
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
