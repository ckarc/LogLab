// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Where the decoder's time goes: process-wide counters of the time spent
//! in each stage and of the work done, for the bench. Reading them costs
//! nothing worth measuring (one atomic add per stage per candidate), so
//! they are always on.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A stage of decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Spectrogram, baseband transform and sync search.
    Search,
    /// Fine timing and frequency.
    Lock,
    /// Bit likelihoods from the tone amplitudes.
    Likelihoods,
    /// Belief propagation.
    BeliefPropagation,
    /// Ordered-statistics decoding.
    DeepSearch,
    /// The phase and acceptance checks of a codeword.
    Validation,
}

/// Something counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// Sync-search candidates tried (over every pass).
    Candidates,
    /// Belief-propagation runs (one per likelihood set per lock).
    BeliefPropagation,
    /// Ordered-statistics runs (one per likelihood set it worked on).
    DeepSearches,
    /// Decodes belief propagation delivered.
    DecodedByBp,
    /// Decodes the deep search delivered.
    DecodedByDeep,
    /// Deep decodes believed because both likelihood sets agreed on them.
    DeepByAgreement,
    /// Deep decodes believed because the signal arrived evenly.
    DeepByEvenness,
    /// Locks demodulated (each tried with up to four likelihood sets).
    Locks,
}

const STAGES: usize = 6;
const COUNTS: usize = 8;

static NANOS: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];
static COUNTERS: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];

/// Measures one stretch of one stage.
pub struct Timer(Instant);

impl Timer {
    /// Starts timing now.
    #[must_use]
    pub fn start() -> Self {
        Self(Instant::now())
    }

    /// Credits the time since the start (or last lap) to `stage`.
    pub fn stop(self, stage: Stage) {
        NANOS[stage as usize].fetch_add(
            u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Credits the time so far to `stage` and starts again.
    #[must_use]
    pub fn lap(self, stage: Stage) -> Self {
        self.stop(stage);
        Self::start()
    }
}

/// Adds `n` to `count`.
pub fn count(count: Count, n: usize) {
    COUNTERS[count as usize].fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// The stages' times so far, in seconds of thread time (stages run on
/// several threads at once, so they add up to more than the wall time).
#[must_use]
pub fn stage_seconds() -> [(Stage, f64); STAGES] {
    let stages = [
        Stage::Search,
        Stage::Lock,
        Stage::Likelihoods,
        Stage::BeliefPropagation,
        Stage::DeepSearch,
        Stage::Validation,
    ];
    #[allow(clippy::cast_precision_loss)] // nanoseconds, reported to a tenth
    stages.map(|s| (s, NANOS[s as usize].load(Ordering::Relaxed) as f64 / 1e9))
}

/// The counts so far.
#[must_use]
pub fn counts() -> [(Count, u64); COUNTS] {
    let counts = [
        Count::Candidates,
        Count::BeliefPropagation,
        Count::DeepSearches,
        Count::DecodedByBp,
        Count::DecodedByDeep,
        Count::DeepByAgreement,
        Count::DeepByEvenness,
        Count::Locks,
    ];
    counts.map(|c| (c, COUNTERS[c as usize].load(Ordering::Relaxed)))
}

/// The most memory this process has held so far, in megabytes, where the
/// system says (Linux); otherwise `None`.
#[must_use]
pub fn peak_memory_mb() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

/// A short report of all of it.
#[must_use]
pub fn report() -> String {
    let mut out = String::from("thread time by stage:");
    for (stage, seconds) in stage_seconds() {
        let _ = write!(out, " {stage:?} {seconds:.1} s;");
    }
    out.push_str("\ncounts:");
    for (count, n) in counts() {
        let _ = write!(out, " {count:?} {n};");
    }
    if let Some(mb) = peak_memory_mb() {
        let _ = write!(out, "\npeak memory: {mb:.0} MB");
    }
    out
}
