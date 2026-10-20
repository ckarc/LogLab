# The Contest profile on a crowded band

Produced 2026-10-20 in the Contest decoder thread, and parked there at the
maintainer's request after phase 3 of six. This file records what was built,
what it measured and where it stopped, so the work can be picked up or
judged later. The brief it was built under is
`/mnt/project-files/ft8/contest/brief.md` in the project, the running results
`/mnt/project-files/ft8/contest/results.md`.

## What it is

The LabLog decoder has one core and, since this work, two *profiles*
(`decode::Profile`):

- **LabLog** (the default): sensitivity first. Every candidate gets the full
  search, however long it takes. Unchanged by this work: its output on the
  crowded band is byte for byte what main produced before it.
- **Contest**: a crowded band, where the operator wants the strong signals
  and the pile-up on their own frequency within a couple of seconds of the
  slot's end. Same stages, same acceptance rules; it differs only in where
  it spends its time (`decode/contest.rs`):
  1. candidates within 60 Hz of the operator's own frequency come first,
     then the louder before the quieter (Costas power);
  2. *quick rounds*: belief propagation alone on every candidate, locking
     each once, in batches of sixteen so decodes are handed out as they
     come; every decode is subtracted at once and the band searched again,
     because each signal removed uncovers the ones it was hiding; a place
     already tried is tried again once a signal within 75 Hz has been
     removed since; this repeats until a round finds nothing new;
  3. the *deep search* (ordered-statistics decoding) on what is left,
     loudest first, with the neighbouring locks the quick rounds skipped
     and the four-symbol likelihoods built only now; anything it finds is
     subtracted and the quick rounds run again;
  4. a hard limit, 4 s per slot (`Options::limit_s`), and a budget of 2 s
     (`Options::budget_s`) by which the decodes that matter should be out.

`decode_slot_each` hands each decode to the caller the moment it is made;
`decode_slot_residual` returns what was left after subtraction.

## The bench

`ft8bench generate DIR --scenario contest --seed 1` makes a Field Day band:
40 slots, 50 to 80 signals each (2,608 in all), −24 to +16 dB, signals on
top of one another, a population of stations that persists from slot to
slot working Field Day QSOs, and a pile-up answering K9KSU on 1500 Hz, half
of it stations already heard working others. `ft8bench decode --profile
contest --limit 4` writes a `.when` file per slot with the time of each
decode; `ft8bench score` then reports, beside the usual counts, the workable
signals (−18 dB and up) found, the answers to the own call found, and what
arrived within the budget. Every count is of received decodes (Principle
XX); WSJT-X made no a-priori decodes on this band.

## Results, seed 1 (the tuning seed; no held-out seed was run)

Each decoder ran alone on an otherwise idle four-core machine. Near misses
are wrong codewords that reached the CRC and were stopped there alone;
divided by 16,383 they give the false decodes to expect.

| Decoder | Found (of 2,608) | Workable found (of 2,392) | Answers to K9KSU (of 40) | Found within 2 s | Answers within 2 s | False decodes | Near misses | Time per slot |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| LabLog profile (main) | 2,072 (79.4%) | 2,012 (84.1%) | 30 | 883 (33.9%) | 14 | 0 | 12,626 | 9.1 s on four cores |
| WSJT-X 3.0.2, `jt9 -8 -d 3` | 2,357 (90.4%) | 2,300 (96.2%) | 36 | 0 (all at the end) | 0 | 1 | n/a | 10.3 s on one core |
| Contest profile, 4 s limit | 2,425 (93.0%) | 2,359 (98.6%) | 35 | 2,129 (81.6%) | 33 | 0 | 366 | 4.0 s on four cores |
| Contest profile, 6 s limit (first build) | 2,435 (93.4%) | 2,365 (98.9%) | 35 | 1,922 (73.7%) | 32 | 0 | 3,841 | 6.0 s |
| Contest profile, 10 s limit (first build) | 2,460 (94.3%) | 2,376 (99.3%) | 36 | 1,919 (73.6%) | 31 | 0 | 10,650 | 10.0 s |

Peak memory 37 MB (Contest, 4 s). First decode about 0.07 s after the slot
ends; slowest answer to K9KSU 2.2 s. WSJT-X's one false decode was a
nonsense message with two hashed calls.

## What was learned

- Every signal the LabLog profile lost on this band was within 50 Hz of a
  louder one, and nothing further away was lost. After its three passes,
  two thirds of the workable signals it had missed decode from their own
  candidate on the audio it left behind: it stops looking too soon, not
  because its demodulation is weak.
- On a crowded band almost everything comes from subtraction and belief
  propagation. Within the 4 s limit the deep search found one decode on
  2,608 signals; given 10 s it found 36 more at the price of 10,000 near
  misses. The limit is the trade between those.
- Locking (fine frequency and timing per candidate) is the largest cost,
  then likelihoods and belief propagation. Locking once per candidate in
  the quick rounds instead of three times lost nothing and moved the
  within-2-s share from 73% to 82%.
- The deepest costs of the everyday search, the four-symbol likelihoods
  built for every candidate, are wasted on the leftovers and sidelobes of
  loud signals already decoded, which is most of what a crowded band's
  candidate list holds.

## Where it stopped

Phases 1 to 3 of the brief are done: the bench (pull request #74), the
diagnosis, and the Contest profile above. Not done:

- phase 4, the contest exchange grammar and calls heard earlier as
  *assisted* hypotheses, marked and scored apart from received decodes with
  their own false-decode test;
- phase 5, the profile chosen by the Operation Overlay with a manual switch
  in the FT8 box (the Station Agent still runs the LabLog profile; nothing
  in the Engine or the web client selects Contest);
- phase 6, the held-out seed, the 32-condition fading sweep and the clean
  seeds as proof the LabLog profile is unchanged beyond the byte-identical
  check on this band, and CPU and memory on the target machine.

The brief's rule that a change raising near misses more than decodes is
rejected was read per unit of deep search, since the second build's extra
near misses come from the deep search getting time it did not have before,
not from any acceptance rule; that reading is recorded here rather than
decided.

## Reproducing it

```sh
cd agent && cargo build --release --bin ft8bench
B=target/release/ft8bench
$B generate /tmp/contest --scenario contest --seed 1
$B decode /tmp/contest --out /tmp/contest/contest --profile contest --limit 4
$B score /tmp/contest/truth.tsv /tmp/contest/contest
$B decode /tmp/contest --out /tmp/contest/everyday             # the LabLog profile
$B score /tmp/contest/truth.tsv /tmp/contest/everyday
```

WSJT-X: `build-wsjtx.sh`, then `jt9 -8 -d 3 -q -a TMP -t TMP SLOT.wav` on
each recording by absolute path, and `ft8bench score` on the output
directory. Timings mean something only one decoder at a time on an idle
machine.
