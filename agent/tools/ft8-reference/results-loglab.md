# LabLog decoder on the FT8 bench

LabLog's own FT8 decoder (`agent/src/ft8/decode.rs`), stage 3, produced
2026-10-19 by `run-loglab.sh`, against WSJT-X 3.0.2 (`jt9 -8 -d 3`) on the
same recordings. The decoder is LabLog's own design from the published FT8
description; it is not a port of WSJT-X.

## Received and assisted decodes (Principle XX)

Every LabLog decode here is *received*: all of the message came from the
signal and passed the parity checks, the phase-coherence check and the CRC.
WSJT-X also reports a few *assisted* decodes, marked `a1` in its output: it
assumed the message was a CQ and filled in those bits. Both of WSJT-X's
counts are shown below, and LabLog is compared against each.

## Six standard sets, 3,600 signals

Seeds 1, 4, 5, 6, 9 and 10, each 60 recordings of 10 signals.

| SNR (dB) | Sent | WSJT-X (all) | WSJT-X (received only) | LabLog stage 2 | LabLog stage 3 |
|---|---|---|---|---|---|
| -26 to -24 | 327 | 0 | 0 | 0 | 0 |
| -24 to -22 | 320 | 10 | 4 | 6 | 27 |
| -22 to -20 | 365 | 199 | 180 | 196 | 288 |
| -20 to -18 | 323 | 312 | 312 | 319 | 323 |
| -18 to -16 | 334 | 331 | 331 | 334 | 334 |
| -16 to -14 | 307 | 305 | 305 | 307 | 307 |
| -14 to -12 | 334 | 333 | 333 | 334 | 334 |
| -12 to -10 | 330 | 329 | 329 | 330 | 330 |
| -10 to -8 | 333 | 332 | 332 | 333 | 333 |
| -8 to -6 | 309 | 309 | 309 | 309 | 309 |
| -6 to -4 | 312 | 311 | 311 | 312 | 312 |
| -4 to -2 | 6 | 6 | 6 | 6 | 6 |
| **All** | **3,600** | **2,777** | **2,752** | **2,786** | **2,903** |

| Seed | WSJT-X (all) | WSJT-X (received only) | LabLog stage 2 | LabLog stage 3 |
|---|---|---|---|---|
| 1 | 458 | 452 | 463 | 485 |
| 4 | 458 | 454 | 464 | 478 |
| 5 | 460 | 458 | 462 | 479 |
| 6 | 470 | 468 | 470 | 489 |
| 9 | 470 | 465 | 467 | 491 |
| 10 | 461 | 455 | 460 | 481 |

Stage 3 was tuned on seeds 1 and 4 only. Seeds 5, 6, 9 and 10 were decoded
once, with the tuning finished, and used only to check it.

**False decodes:** none from either decoder in the 3,600 signals, and none
from LabLog in 600 recordings of noise alone (seeds 3, 7 and 8, 200 each).
The bench now also reports how many wrong codewords got as far as the CRC
check, since a wrong codeword passes the CRC about once in 16,000 times and
zero false decodes in a few hundred recordings cannot by itself tell a safe
decoder from a lucky one. In the 600 noise recordings, no codeword reached
the CRC at all; across the 3,600 signals, 415 did (next to real signals,
where a near miss of the right codeword is possible), which is about 0.025
false decodes to expect in the whole run, or one in about 14,000
recordings like these.

## What this shows

- LabLog finds 151 more signals than WSJT-X's received decodes, and 126
  more than WSJT-X even counting its assisted ones; 117 more than stage 2.
- From -20 dB up, LabLog found every signal sent. From -22 to -20 dB it
  found 79 percent against WSJT-X's 54 percent, and below -22 dB 27 signals
  against WSJT-X's 10.
- Speed: about 2.4 s per 15-second recording on 4 threads (slowest 3.5 s),
  up from 1.7 s in stage 2, still well inside one FT8 cycle.

## What changed from stage 2

The weak signals were being found and locked onto, but not finely enough.
The multi-symbol likelihoods add the tones of neighbouring symbols in phase,
and a quarter-hertz frequency error turns that phase by a third of a cycle
across three symbols; the timing was also a sample late more often than not.
Measured with the true frequency and timing, the same signals decoded far
more often. So:

1. **A coherent lock.** After the coarse lock, the phases of the 21 Costas
   (sync) tones settle the frequency to a few hundredths of a hertz, by
   finding the offset that makes each Costas array add up in phase, and the
   timing to one baseband sample, by the largest in-phase sum. On the bench
   this reaches the lock an oracle would give.
2. **Four symbols at a time.** With the lock that precise, a four-symbol
   likelihood is tried after the one-, three- and two-symbol ones.
3. **A deeper ordered-statistics search**: every pair among all 91 pivots
   and every triple among the 25 least reliable, on the three- and
   four-symbol likelihoods (the only ones that rescued anything).
4. **A phase-coherence check on every decode.** The deeper search found one
   false decode in noise, so every codeword, from belief propagation or the
   search, must now be borne out by the signal: the tones it says were sent,
   added in phase over every run of four data symbols, must reach 30 times
   the noise power on average. Noise never passed 24 in 48,000 tries; every
   real decode down to -23 dB is above 30. The check cost nothing on the
   bench and is what lets the deeper search run safely.
5. **Faster likelihoods**, so the four-symbol one is affordable.

## Reproducing it

```sh
tools/ft8-reference/run-loglab.sh 1      # LabLog only; needs no WSJT-X
tools/ft8-reference/run-reference.sh 1   # WSJT-X, after build-wsjtx.sh
```
