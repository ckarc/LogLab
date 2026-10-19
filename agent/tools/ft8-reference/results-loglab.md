# LogLab decoder on the FT8 bench

Stage 1 of LogLab's own FT8 decoder (`agent/src/ft8/decode.rs`), produced
2026-10-19 by `run-loglab.sh`, against WSJT-X 3.0.2 (`jt9 -8 -d 3`) on the
same recordings. The decoder is LogLab's own design from the published FT8
description; it is not a port of WSJT-X.

## Found, by strength band

| SNR (dB) | Seed 1 sent | WSJT-X | LogLab | Seed 4 sent | WSJT-X | LogLab |
|---|---|---|---|---|---|---|
| -26 to -24 | 49 | 0 | 0 | 63 | 0 | 0 |
| -24 to -22 | 60 | 2 | 0 | 54 | 3 | 1 |
| -22 to -20 | 71 | 42 | 28 | 57 | 30 | 16 |
| -20 to -18 | 53 | 49 | 50 | 39 | 39 | 39 |
| -18 to -16 | 43 | 43 | 43 | 73 | 73 | 73 |
| -16 to -14 | 53 | 53 | 53 | 53 | 53 | 53 |
| -14 to -12 | 55 | 54 | 55 | 42 | 42 | 42 |
| -12 to -10 | 59 | 58 | 59 | 55 | 55 | 55 |
| -10 to -8 | 40 | 40 | 40 | 64 | 63 | 64 |
| -8 to -6 | 55 | 55 | 55 | 46 | 46 | 46 |
| -6 to -4 | 60 | 60 | 60 | 53 | 53 | 53 |
| -4 to -2 | 2 | 2 | 2 | 1 | 1 | 1 |
| **All** | **600** | **458** | **445** | **600** | **458** | **443** |

False decodes: none from either decoder on either seed, and none from
LogLab on 40 recordings of noise alone (seed 3, no signals).

## What this shows

- From -20 dB up, LogLab finds at least as many as WSJT-X in every band,
  four more in total, including strong signals WSJT-X missed.
- The gap is the weakest signals, -24 to -20 dB: about 15 fewer per 600.
  Stage 2 work (a priori decoding of expected messages, better signal
  subtraction) is aimed there.
- Speed: about 0.45 s per 15-second recording on 4 threads (slowest 0.56 s),
  well inside one FT8 cycle.

## Reproducing it

```sh
tools/ft8-reference/run-loglab.sh 1      # LogLab only; needs no WSJT-X
tools/ft8-reference/run-reference.sh 1   # WSJT-X, after build-wsjtx.sh
```
