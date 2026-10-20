// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `ft8bench`: the FT8 decoder test bench (a development tool, not part of
//! the running Agent).
//!
//! ```text
//! ft8bench generate DIR [--seed N] [--slots N] [--signals N]
//!                       [--min-snr DB] [--max-snr DB] [--channel CHANNEL]
//!                       [--scenario contest] [--own-call CALL] [--own-hz HZ]
//!                       [--min-signals N] [--max-signals N]
//!     Write slot-NNNN.wav recordings and truth.tsv into DIR. CHANNEL is a
//!     simulated HF channel every signal passes through: a preset (quiet,
//!     moderate, disturbed, flutter) and/or name=value settings separated by
//!     commas (spread, k, deepfade, fadelen, drift, bend, wobble, period,
//!     phasenoise, echo, echodb, echoshift); see `ft8::channel`. The
//!     contest scenario is a crowded Field Day band (50 to 80 signals per
//!     slot, -24 to +16 dB, a pile-up answering the own call; see
//!     `ft8::bench::contest`).
//! ft8bench score TRUTH DECODES-DIR [--name NAME] [--budget SECONDS]
//!     Score slot-NNNN.txt decoder output against the truth file. For a
//!     contest truth file, also what matters in a contest: answers to the
//!     own call, workable signals, and decodes within the budget (default
//!     2 s) by the slot-NNNN.when files `decode` writes beside its output.
//! ft8bench decode DIR [--out OUT-DIR] [--profile lablog|contest]
//!                     [--budget S] [--limit S] [--own-hz HZ]
//!     Decode every slot-NNNN.wav in DIR with LogLab's decoder (under the
//!     contest profile when asked, with its time budget and limit, and
//!     the own frequency from the truth file unless given), writing
//!     slot-NNNN.txt decoder output (default OUT-DIR: DIR/loglab) and
//!     slot-NNNN.when, when each decode was made (seconds from the start
//!     of the slot's decoding, then the message), with the whole slot's
//!     time on its first line.
//! ft8bench diagnose DIR [--decodes DECODES-DIR] [--after-decode yes]
//!     For every signal in DIR's truth file, how far LogLab's decoder gets
//!     with it, step by step, as tab-separated lines (see
//!     `ft8::decode::diagnose`); with DECODES-DIR, also whether the full
//!     decoder found it there. With --after-decode, the diagnosis is made
//!     on what the full decoder left after subtracting everything it
//!     found: how far it would get with each signal it missed, were it to
//!     look again.
//! ft8bench tones MESSAGE
//!     Print the 79 tones of a message.
//! ft8bench messages [--seed N] [--count N]
//!     Print random standard messages, one per line.
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use loglab_agent::ft8::bench::contest::ContestSpec;
use loglab_agent::ft8::bench::{self, ContestScoring, Rng, ScenarioSpec};
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
    let seed = number(options, "seed", 1)?;
    let base = match options.get("scenario").map(String::as_str) {
        None | Some("standard") => ScenarioSpec::standard(seed),
        Some("contest") => ScenarioSpec::contest(seed),
        Some(other) => return Err(format!("unknown scenario: {other}")),
    };
    let contest = base.contest.clone().map(|c| -> Result<_, String> {
        Ok(ContestSpec {
            own_call: options
                .get("own-call")
                .map_or(c.own_call, |call| call.to_uppercase()),
            own_hz: number(options, "own-hz", c.own_hz)?,
            min_signals: number(options, "min-signals", c.min_signals)?,
            max_signals: number(options, "max-signals", c.max_signals)?,
        })
    });
    let contest = contest.transpose()?;
    if contest
        .as_ref()
        .is_some_and(|c| c.min_signals > c.max_signals || c.max_signals == 0)
    {
        return Err("--min-signals must not exceed --max-signals, which must be positive".into());
    }
    let spec = ScenarioSpec {
        slots: number(options, "slots", base.slots)?,
        signals_per_slot: number(options, "signals", base.signals_per_slot)?,
        min_snr_db: number(options, "min-snr", base.min_snr_db)?,
        max_snr_db: number(options, "max-snr", base.max_snr_db)?,
        channel: options
            .get("channel")
            .map_or(Ok(Channel::CLEAR), |c| Channel::parse(c))?,
        contest,
        ..base
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
    let signals: usize = slots.iter().map(|s| s.signals.len()).sum();
    Ok(format!(
        "wrote {} slots with {} signals to {} (channel: {})\n",
        spec.slots,
        signals,
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
    let mut contest = bench::truth_own_call(&text).map(|own_call| ContestScoring {
        own_call,
        budget_s: 2.0,
        when: BTreeMap::new(),
        took: BTreeMap::new(),
    });
    if let Some(c) = &mut contest {
        c.budget_s = number(options, "budget", c.budget_s)?;
    }
    // Every slot-NNNN.txt counts, including noise-only slots the truth
    // file has no lines for.
    let mut decoded = BTreeMap::new();
    let entries =
        std::fs::read_dir(decodes_dir).map_err(|e| format!("{}: {e}", decodes_dir.display()))?;
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        let slot_number = |suffix: &str| {
            path.file_name().and_then(|n| n.to_str()).and_then(|n| {
                n.strip_prefix("slot-")?
                    .strip_suffix(suffix)?
                    .parse::<usize>()
                    .ok()
            })
        };
        if let (Some(index), Ok(output)) = (slot_number(".txt"), std::fs::read_to_string(&path)) {
            let messages = output
                .lines()
                .filter_map(bench::parse_decode_line)
                .collect();
            decoded.insert(index, messages);
        }
        if let (Some(c), Some(index), Ok(output)) = (
            contest.as_mut(),
            slot_number(".when"),
            std::fs::read_to_string(&path),
        ) {
            let (took, when) = parse_when(&output);
            if let Some(took) = took {
                c.took.insert(index, took);
            }
            c.when.insert(index, when);
        }
    }
    let name = options.get("name").map_or("Decoder", String::as_str);
    Ok(bench::report(
        name,
        &bench::score_contest(&truth, &decoded, contest.as_ref()),
    ))
}

/// A slot's `.when` file: the slot's whole decoding time, then when each
/// message was decoded.
fn parse_when(text: &str) -> (Option<f64>, BTreeMap<String, f64>) {
    let mut took = None;
    let mut when = BTreeMap::new();
    for line in text.lines() {
        let Some((seconds, rest)) = line.split_once('\t') else {
            continue;
        };
        let Ok(seconds) = seconds.parse::<f64>() else {
            continue;
        };
        if rest.is_empty() {
            took = Some(seconds);
        } else {
            when.insert(loglab_agent::ft8::message::normalize(rest), seconds);
        }
    }
    (took, when)
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
    let mut decoder = match options.get("profile").map(String::as_str) {
        None | Some("lablog") => decode::Options::default(),
        Some("contest") => decode::Options::contest(),
        Some(other) => return Err(format!("unknown profile: {other}")),
    };
    decoder.budget_s = number(options, "budget", decoder.budget_s)?;
    decoder.limit_s = number(options, "limit", decoder.limit_s)?;
    decoder.own_hz = match options.get("own-hz") {
        Some(hz) => Some(hz.parse().map_err(|_| "--own-hz must be a number")?),
        None => std::fs::read_to_string(dir.join("truth.tsv"))
            .ok()
            .and_then(|t| bench::truth_own_hz(&t)),
    };
    let started = std::time::Instant::now();
    let mut slowest = std::time::Duration::ZERO;
    for wav in &wavs {
        let bytes = std::fs::read(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
        let audio = bench::wav_samples(&bytes)
            .ok_or_else(|| format!("{}: not a 12,000 Hz mono 16-bit WAV", wav.display()))?;
        let one = std::time::Instant::now();
        let mut timed: Vec<(f64, decode::Decode)> = Vec::new();
        decode::decode_slot_each(&audio, &decoder, &mut |d| {
            timed.push((one.elapsed().as_secs_f64(), d));
        });
        let took = one.elapsed();
        slowest = slowest.max(took);
        let mut when = format!("{:.3}\t\n", took.as_secs_f64());
        for (at, d) in &timed {
            let _ = writeln!(when, "{at:.3}\t{}", d.message);
        }
        timed.sort_by(|a, b| a.1.base_hz.total_cmp(&b.1.base_hz));
        let text: String = timed.iter().map(|(_, d)| d.to_line() + "\n").collect();
        let stem = wav
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        write(&out.join(format!("{stem}.txt")), text.as_bytes())?;
        write(&out.join(format!("{stem}.when")), when.as_bytes())?;
    }
    Ok(format!(
        "decoded {} recordings into {} in {:.1} s (slowest {:.2} s); {} wrong codewords were caught by the CRC alone, so about {:.4} false decodes were to be expected\n{}\n",
        wavs.len(),
        out.display(),
        started.elapsed().as_secs_f64(),
        slowest.as_secs_f64(),
        decode::crc_misses(),
        to_f64(decode::crc_misses()) / 16_383.0,
        decode::profile::report()
    ))
}

fn diagnose(dir: &Path, options: &BTreeMap<String, String>) -> Result<String, String> {
    use std::fmt::Write as _;

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
                            let audio = if options.contains_key("after-decode") {
                                decode::decode_slot_residual(&audio, &single, &mut |_| {})
                            } else {
                                audio
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
                                let _ = writeln!(
                                    out,
                                    "{}\t{}\t{}\t{}\t{}\t{mark}\t{}",
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
