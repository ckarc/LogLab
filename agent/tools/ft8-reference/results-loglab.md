# LogLab decoder on the FT8 bench

LogLab's own FT8 decoder (`agent/src/ft8/decode.rs`), stage 2, produced
2026-10-19 by `run-loglab.sh`, against WSJT-X 3.0.2 (`jt9 -8 -d 3`) on the
same recordings. The decoder is LogLab's own design from the published FT8
description; it is not a port of WSJT-X.

## Received and assisted decodes (Principle XX)

Every LogLab decode here is *received*: all of the message came from the
signal and passed the parity checks and the CRC. WSJT-X also reports a few
*assisted* decodes, marked `a1` in its output: it assumed the message was a
CQ and filled in those bits. Both of WSJT-X's counts are shown below, and
LogLab is compared against each.

## Six standard sets, 3,600 signals

Seeds 1, 4, 5, 6, 9 and 10, each 60 recordings of 10 signals.

| SNR (dB) | Sent | WSJT-X (all) | WSJT-X (received only) | LogLab (received) |
|---|---|---|---|---|
| -26 to -24 | 327 | 0 | 0 | 0 |
| -24 to -22 | 320 | 10 | 4 | 6 |
| -22 to -20 | 365 | 199 | 180 | 196 |
| -20 to -18 | 323 | 312 | 312 | 319 |
| -18 to -16 | 334 | 331 | 331 | 334 |
| -16 to -14 | 307 | 305 | 305 | 307 |
| -14 to -12 | 334 | 333 | 333 | 334 |
| -12 to -10 | 330 | 329 | 329 | 330 |
| -10 to -8 | 333 | 332 | 332 | 333 |
| -8 to -6 | 309 | 309 | 309 | 309 |
| -6 to -4 | 312 | 311 | 311 | 312 |
| -4 to -2 | 6 | 6 | 6 | 6 |
| **All** | **3,600** | **2,777** | **2,752** | **2,786** |

| Seed | WSJT-X (all) | WSJT-X (received only) | LogLab |
|---|---|---|---|
| 1 | 458 | 452 | 463 |
| 4 | 458 | 454 | 464 |
| 5 | 460 | 458 | 462 |
| 6 | 470 | 468 | 470 |
| 9 | 470 | 465 | 467 |
| 10 | 461 | 455 | 460 |

Seeds 9 and 10 were made after the tuning was finished and used only to
check it.

**False decodes:** none from either decoder in the 3,600 signals, and none
from LogLab in 640 recordings of noise alone (seeds 3, 7 and 8; seed 8 also
kept back for checking).

## What this shows

- LogLab finds 34 more signals than WSJT-X's received decodes, and 9 more
  than WSJT-X even counting its assisted ones.
- From -18 dB up, LogLab found every signal sent.
- The weakest band, -24 to -22 dB, is where WSJT-X's assisted decoding
  still finds a few more. LogLab's own assisted decoding will be scored
  separately when it comes.
- Speed: about 1.7 s per 15-second recording on 4 threads (slowest
  2.3 s), well inside one FT8 cycle.

## What changed from stage 1

Stage 1 found 445 and 443 on seeds 1 and 4. The gains came from:

1. **Locking on with the whole signal.** Fine timing and frequency are
   tuned to the power on all 79 symbols, not only the 21 sync symbols, and
   the nearest neighbouring locks are tried when the best one fails. Tests
   with the true timing and frequency showed this was where most weak
   signals were being lost.
2. **Better bit confidence.** Each bit's likelihood weighs every tone
   combination instead of only the strongest one.
3. **A better start for ordered-statistics decoding.** It also tries the
   likelihoods as belief propagation left them.
4. **Removing signals from the audio itself.** Decoded signals are rebuilt
   and subtracted from the recording, and the slower, riskier
   ordered-statistics decoding runs only after that. This removed a false
   decode found next to a strong signal on seed 9.

## Reproducing it

```sh
tools/ft8-reference/run-loglab.sh 1      # LogLab only; needs no WSJT-X
tools/ft8-reference/run-reference.sh 1   # WSJT-X, after build-wsjtx.sh
```
