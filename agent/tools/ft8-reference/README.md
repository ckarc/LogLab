# FT8 reference bench

LabLog will have its own FT8 decoder. Before any LabLog decoder is trusted, it
has to do at least as well as WSJT-X's, which is the proven standard. This
folder is how that is measured.

## What is here

- `build-wsjtx.sh` builds WSJT-X's own decoder (`jt9`) and encoder
  (`ft8code`) from a pinned commit. WSJT-X is GPL-3.0; it is only built here
  as a development tool and never ships with LabLog.
- `run-reference.sh` makes the standard bench (60 recordings of 15 seconds,
  each with 10 FT8 signals from -26 to -4 dB in noise), decodes every
  recording with WSJT-X at its deepest setting, and prints how many signals
  it found at each strength.
- `make-tone-fixture.sh` rebuilds `../../tests/data/ft8-wsjtx-tones.tsv`, the
  tones WSJT-X sends for over a thousand messages. LabLog's encoder is tested
  against that file on every build, so it must send exactly what WSJT-X
  sends.
- `baseline-wsjtx.md` is WSJT-X's score on the standard bench, seed 1. It is
  the bar a LabLog decoder must meet.
- `run-loglab.sh` scores LabLog's own decoder on the same bench. It needs
  nothing from WSJT-X.
- `results-loglab.md` is LabLog's score next to WSJT-X's, band by band.

The recordings are made by `ft8bench` (`../../src/bin/ft8bench.rs`) from a
seed, so they are the same on every machine and nothing large is stored.

## Running it

```sh
tools/ft8-reference/build-wsjtx.sh      # once; needs the packages listed in it
tools/ft8-reference/run-reference.sh 1  # seed 1, takes a few minutes
tools/ft8-reference/run-loglab.sh 1     # LabLog's decoder, same seed
```

## The bar for a LabLog decoder

On the same recordings, a LabLog decoder must:

1. find every signal WSJT-X finds, in every strength band;
2. report nothing that was not sent (WSJT-X reports zero false decodes);
3. finish each 15-second recording well inside one FT8 cycle.

## Propagation conditions

Real HF signals fade, drift and flutter; the standard bench is a clean
channel. `channels.txt` lists simulated conditions (Rayleigh and Rician
fading at several Doppler spreads, deep fades, steady and curved drift,
phase noise, a delayed second path, and combinations), and
`run-channels.sh` sweeps decoders across them:

```sh
tools/ft8-reference/run-channels.sh 1 /tmp/sweep \
  loglab=target/release/ft8bench wsjtx=/path/to/jt9 -- clear rayleigh-0.5 drift-0.2
```

Every condition uses the same seed's messages, placing and noise, so the
conditions differ only in what the channel did to the signals, and every
decoder sees the same recordings. `ft8bench generate --channel` makes one
such set by hand, and `ft8bench diagnose DIR` reports, signal by signal,
where the decoder lost each one it missed (no candidate, a bad lock, too
many symbol errors, or a codeword the phase check refused), both from its
own lock and from an oracle lock at the true frequency and timing.
`results-fading.md` is what the sweep showed and what changed because of it.
