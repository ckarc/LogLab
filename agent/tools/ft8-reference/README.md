# FT8 reference bench

LogLab will have its own FT8 decoder. Before any LogLab decoder is trusted, it
has to do at least as well as WSJT-X's, which is the proven standard. This
folder is how that is measured.

## What is here

- `build-wsjtx.sh` builds WSJT-X's own decoder (`jt9`) and encoder
  (`ft8code`) from a pinned commit. WSJT-X is GPL-3.0; it is only built here
  as a development tool and never ships with LogLab.
- `run-reference.sh` makes the standard bench (60 recordings of 15 seconds,
  each with 10 FT8 signals from -26 to -4 dB in noise), decodes every
  recording with WSJT-X at its deepest setting, and prints how many signals
  it found at each strength.
- `make-tone-fixture.sh` rebuilds `../../tests/data/ft8-wsjtx-tones.tsv`, the
  tones WSJT-X sends for over a thousand messages. LogLab's encoder is tested
  against that file on every build, so it must send exactly what WSJT-X
  sends.
- `baseline-wsjtx.md` is WSJT-X's score on the standard bench, seed 1. It is
  the bar a LogLab decoder must meet.
- `run-loglab.sh` scores LogLab's own decoder on the same bench. It needs
  nothing from WSJT-X.
- `results-loglab.md` is LogLab's score next to WSJT-X's, band by band.

The recordings are made by `ft8bench` (`../../src/bin/ft8bench.rs`) from a
seed, so they are the same on every machine and nothing large is stored.

## Running it

```sh
tools/ft8-reference/build-wsjtx.sh      # once; needs the packages listed in it
tools/ft8-reference/run-reference.sh 1  # seed 1, takes a few minutes
tools/ft8-reference/run-loglab.sh 1     # LogLab's decoder, same seed
```

## The bar for a LogLab decoder

On the same recordings, a LogLab decoder must:

1. find every signal WSJT-X finds, in every strength band;
2. report nothing that was not sent (WSJT-X reports zero false decodes);
3. finish each 15-second recording well inside one FT8 cycle.
