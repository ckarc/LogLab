# LabLog decoder under fading and drift

Produced 2026-10-19 by `run-channels.sh` (seed 1, 40 recordings of 10
signals per condition, −24 to −8 dB) with three LabLog decoders and WSJT-X
3.0.2 (`jt9 -8 -d 3`) on the identical recordings. "Before #68" is the
decoder before the coherent sync and phase validation of pull request #68,
"#68" that decoder, "Improved" the decoder that came of this study. The
conditions are the ones in `channels.txt`; `agent/src/ft8/channel.rs` says
exactly what each does to a signal.

Every LabLog decode is *received* (Principle XX). WSJT-X's *assisted*
decodes (its `a1` marks, where it assumed a CQ) are shown in its "all"
column and left out of "received only".

## Near misses: the false-decode meter

Zero false decodes in 400 signals cannot tell a safe decoder from a lucky
one. `ft8bench decode` therefore counts the wrong codewords that reached the
CRC and were caught by it alone. A wrong codeword passes the 14-bit CRC
about once in 16,383 times, so that count divided by 16,383 is the number of
false decodes to expect. The last column below gives it for #68 and for the
improved decoder.

## Decodes per condition

| Condition | Before #68 | #68 | Improved | WSJT-X all | WSJT-X received only | Near misses, #68 → improved |
|---|---|---|---|---|---|---|
| Clean | 323 | 337 | 337 | 323 | 314 | 60 → 24 |
| Rayleigh, 0.05 Hz spread | 268 | 281 | 275 | 268 (2 false) | 260 (2 false) | 1,130 → 71 |
| Rayleigh, 0.2 Hz | 263 | 276 | 269 | 272 (1 false) | 264 (1 false) | 2,491 → 185 |
| Rayleigh, 0.5 Hz | 270 | 278 | 271 | 271 | 264 | 2,690 → 250 |
| Rayleigh, 1 Hz | 257 | 254 | 252 | 266 (1 false) | 258 | 3,747 → 358 |
| Rayleigh, 2 Hz | 245 (1 false) | 215 | 225 | 253 | 245 | 5,091 → 331 |
| Rician K=10 dB, 0.5 Hz | 310 | 333 | 332 | 314 | 313 | 132 → 51 |
| Rician K=6 dB, 1 Hz | 301 | 320 | 315 | 302 (1 false) | 299 (1 false) | 311 → 88 |
| Rician K=3 dB, 2 Hz | 285 | 301 | 294 | 292 | 284 | 652 → 95 |
| Deep fade 20 dB, 3 s | 269 | 293 | 287 | 276 | 270 | 576 → 118 |
| Deep fade 30 dB, 5 s | 174 | 187 | 173 | 181 | 165 | 33,257 → 2,387 |
| Drift 0.1 Hz/s | 317 | 317 | 342 | 315 (1 false) | 313 | 60 → 47 |
| Drift 0.2 Hz/s | 303 (1 false) | 292 | 339 | 307 | 301 | 75 → 36 |
| Drift 0.5 Hz/s | 263 (1 false) | 151 | 307 | 273 (1 false) | 262 (1 false) | 5,725 → 74 |
| Curved drift, 3 Hz | 296 | 248 | 260 | 294 | 292 | 2,715 → 22 |
| Wobble 1 Hz, 2 s period | 299 | 281 | 298 | 299 | 294 | 80 → 19 |
| Phase noise 0.5 Hz | 300 | 307 | 306 | 301 | 297 | 44 → 18 |
| Phase noise 2 Hz | 250 | 227 | 227 | 259 (1 false) | 255 (1 false) | 150 → 20 |
| Echo 20 ms, −3 dB | 271 | 284 | 275 | 284 | 273 | 2,002 → 399 |
| ITU moderate (0.5 Hz, 1 ms) | 263 | 277 | 269 | 268 | 260 | 2,468 → 257 |
| ITU disturbed (1 Hz, 2 ms) | 250 | 248 | 247 | 265 | 255 | 3,591 → 254 |
| ITU flutter (10 Hz, 1 ms) | 2 | 1 | 3 | 15 | 8 | 28,267 → 1,543 |
| Combination, mild | 254 | 263 | 261 | 264 | 255 | 3,401 → 251 |
| Combination, rough | 186 | 131 | 166 | 192 | 183 | 17,286 → 807 |

## Decodes from −24 to −20 dB (about 103 sent per condition)

| Condition | Sent | #68 | Improved | WSJT-X all | WSJT-X received only |
|---|---|---|---|---|---|
| Clean | 103 | 41 | 41 | 32 | 24 |
| Rayleigh, 0.05 Hz spread | 103 | 34 | 31 | 28 | 26 |
| Rayleigh, 0.2 Hz | 103 | 19 | 18 | 12 | 10 |
| Rayleigh, 0.5 Hz | 103 | 9 | 8 | 8 | 4 |
| Rayleigh, 1 Hz | 103 | 3 | 3 | 5 | 2 |
| Rayleigh, 2 Hz | 103 | 0 | 0 | 3 | 0 |
| Rician K=10 dB, 0.5 Hz | 103 | 39 | 38 | 23 | 22 |
| Rician K=6 dB, 1 Hz | 103 | 28 | 24 | 17 | 16 |
| Rician K=3 dB, 2 Hz | 103 | 14 | 10 | 10 | 8 |
| Deep fade 20 dB, 3 s | 103 | 9 | 7 | 2 | 1 |
| Deep fade 30 dB, 5 s | 103 | 0 | 0 | 1 | 0 |
| Drift 0.1 Hz/s | 103 | 24 | 46 | 19 | 17 |
| Drift 0.2 Hz/s | 103 | 8 | 42 | 15 | 12 |
| Drift 0.5 Hz/s | 103 | 0 | 17 | 3 | 0 |
| Curved drift, 3 Hz | 103 | 1 | 0 | 9 | 8 |
| Wobble 1 Hz, 2 s period | 103 | 2 | 10 | 11 | 7 |
| Phase noise 0.5 Hz | 103 | 17 | 16 | 13 | 11 |
| Phase noise 2 Hz | 103 | 0 | 0 | 1 | 1 |
| Echo 20 ms, −3 dB | 103 | 9 | 9 | 11 | 6 |
| ITU moderate (0.5 Hz, 1 ms) | 103 | 14 | 11 | 12 | 10 |
| ITU disturbed (1 Hz, 2 ms) | 103 | 3 | 2 | 6 | 2 |
| ITU flutter (10 Hz, 1 ms) | 103 | 0 | 0 | 0 | 0 |
| Combination, mild | 103 | 8 | 6 | 7 | 5 |
| Combination, rough | 103 | 0 | 0 | 0 | 0 |

## Held-out, regression and noise

Every rule in the improved decoder was chosen on seed 1. The sets below
were decoded once, with the tuning finished (clean seed 1 was a tuning seed
and is shown for reference; #68's clean near misses were counted over
3,600 signals, 415 in all, so its average per 600 is given). No false
decodes from LabLog in any of them.

| Set | #68 | Improved | WSJT-X all | Near misses, #68 → improved |
|---|---|---|---|---|
| Rayleigh, 0.5 Hz, seed 9 (400 signals) | 279 | 273 | 272 (1 false) | 2,664 → 267 |
| Rayleigh, 2 Hz, seed 9 (400 signals) | 225 | 225 | 255 (2 false) | 4,529 → 335 |
| Drift 0.2 Hz/s, seed 9 (400 signals) | 308 | 344 | 321 (2 false) | 45 → 24 |
| Deep fade 20 dB, 3 s, seed 9 (400 signals) | 308 | 303 | 279 | 582 → 135 |
| Phase noise 0.5 Hz, seed 9 (400 signals) | 317 | 317 | 309 | 47 → 22 |
| Clean, seed 1 (600 signals) | 485 | 485 | 458 | 69 on average → 34 |
| Clean, seed 9 (600 signals) | 491 | 491 | 470 | 69 on average → 22 |
| Clean, seed 10 (600 signals) | 481 | 479 | 461 | 69 on average → 31 |
| Noise only, seed 7 (200 recordings) | 0 | 0 | – | 0 → 0 |
| Noise only, seed 8 (200 recordings) | 0 | 0 | – | 0 → 0 |

## What the sweep showed

1. **#68's lead survives slow fading and drowns in fast fading.** Up to a
   0.5 Hz Doppler spread #68 finds more than WSJT-X; at 1 Hz it falls behind,
   and at 2 Hz it finds 38 fewer than WSJT-X and 30 fewer than LabLog did
   before #68. The cause is the thing that gives #68 its clean-signal gain:
   it adds the tones of three and four consecutive symbols in phase, and fast
   fading scrambles the phase from symbol to symbol so the sums cancel. From
   an oracle lock at 2 Hz spread, belief propagation on one-symbol
   likelihoods decodes 196 of 399 signals, two-symbol 180, three-symbol 138,
   four-symbol 84.
2. **Slow fading loses signals to symbol errors, not sync.** The missed
   signals under 0.05 to 0.5 Hz fading were locked well; a third of each
   transmission had faded below the noise. Signals with no candidate at all
   could not be decoded from an oracle lock either.
3. **The lock could not follow drift.** #68 lost a third of the signals at
   0.5 Hz/s. Its frequency error also grows with spread: 0.015 Hz typical on
   a clean signal, 0.11 Hz at 0.5 Hz spread, 0.22 Hz at 1 Hz, 0.32 Hz at 2 Hz.
4. **A false-decode hazard.** Under any fading, #68's ordered-statistics
   search built near misses of the true codeword on partially faded
   transmissions, and its phase check (tones added in phase over runs of
   four symbols, 30 times the noise on average) passed them, because most of
   each transmission is real. 60 wrong codewords reached the CRC on the clean
   set; 1,130 under a 0.05 Hz swell; 33,257 under the 5-second deep fade,
   where two false decodes were to be expected per run. The same check
   refused real decodes under phase noise (scores as low as 18 at 0.5 Hz).

## What changed in the decoder

All four rules are named constants in `decode.rs` with their measured
justification in the comments.

1. **A drift-aware lock.** The frequency offset and a linear drift are fitted
   together from the three Costas arrays, which a drift makes want different
   frequencies, and the signal is de-chirped before demodulation. A drift is
   believed only when the drifting fit explains at least a tenth more Costas
   power than the best steady fit (`DRIFT_MARGIN`): on a weak steady signal,
   noise readily favours a small spurious drift, and the clean bench lost
   three signals in 1,800 without the margin. A real drift of 0.05 Hz/s or
   more clears it easily.
2. **Validation by differential coherence, or by power.** Each decoded tone
   is multiplied by the conjugate of the next symbol's; a real signal's
   products share one phase step whatever slow fading or drift does to the
   phase itself. Summed and divided by the noise, noise never reached 1.53
   in 480,000 tries and its tail thins fivefold per 0.1 above 1.2; real
   decodes score 1.9 and up. Threshold 1.8 (`DIFFERENTIAL_COHERENCE_MIN`). A
   decode also passes on #68's run coherence of 30 (`RUN_COHERENCE_MIN`),
   because a 20 ms echo puts a notch every 50 Hz, inside every signal, and
   gives each tone its own phase shift; without that branch strong echoed
   signals were refused. Noise passes neither test.
3. **Likelihood sets by steadiness.** The 21 Costas tones measure how steady
   the phase is (in-phase power over total power: clean 0.84 to 0.98, 1 Hz
   spread about 0.69, 2 Hz about 0.54, noise 0.36). Above 0.8 the deep
   search uses the three- and four-symbol likelihoods; 0.5 to 0.8 two, three
   and four; below 0.5 one and two.
4. **Evidence before belief.** A deep-search codeword is accepted only if it
   beats the runner-up by a margin of at least 1 (`OSD_MIN_MARGIN`) and
   either two likelihood sets agree on it or the signal arrived evenly (the
   faintest Costas array at least 0.4 of the mean, `OSD_MIN_EVENNESS`). This
   rule was picked by replaying 640,000 deep-search candidates: clean keeps
   10 of 10 real rescues and passes 4 near misses; 0.05 Hz spread 21 of 27
   and 33 (533 without); 5-second deep fade 118 of 132 and 1,919 (22,212
   without).

Tried and dropped: weighting likelihoods by per-symbol signal power (+6 at
0.2 Hz, twice the near misses, −4 clean); agreement alone (−18 of 32 clean
rescues); a margin of 3 alone (−2 clean, −14 deep fade); a stricter
agreement-or-(evenness and margin 2) variant (halves the near misses again,
−1 clean and a few more under every fading condition).

## Runtime

Idle machine, 40 recordings, one decoder at a time:

| Set | #68 total / slowest | Improved total / slowest |
|---|---|---|
| Clean, seed 1 | 89.1 s / 2.60 s | 91.9 s / 2.63 s |
| Rayleigh 0.5 Hz, seed 1 | 86.7 s / 2.54 s | 89.7 s / 2.65 s |

About 3 percent more than #68 (2.3 s per 15-second recording against 2.2),
worst recording under 2.7 s in both. The drift search reuses the Costas
power grids the lock already has; the extra likelihood set for wandering
signals is the main added cost.

## What is still open

At 1 to 2 Hz spread and under 2 Hz of phase noise LabLog finds a few percent
fewer than WSJT-X. Coherent combining is the reason: at 2 Hz the phase
correlation between neighbouring symbols is about 0.6, so even two-symbol
sums lose a third of their value and the validation statistic of a real
signal shrinks by the same factor, while WSJT-X combines nothing and has
nothing to lose. Next levers, in order: a noncoherent (power-only) lock
fallback when steadiness is low (8 to 12 signals per 400 are lost to a bad
lock at 1 to 2 Hz spread); the deep search on one-symbol likelihoods for
steadiness between 0.5 and 0.6, which the oracle says would rescue the most
at 2 Hz (232 against 208); a curved-drift fit for `bend-3`, where the linear
fit recovers only part of what #68 lost.

## Reproducing it

```sh
cd agent && cargo build --release --bin ft8bench
tools/ft8-reference/run-channels.sh 1 /tmp/sweep \
  loglab=target/release/ft8bench wsjtx=/path/to/jt9 -- clear rayleigh-0.5 drift-0.5
target/release/ft8bench diagnose /tmp/sweep/rayleigh-0.5 --decodes /tmp/sweep/rayleigh-0.5/loglab
```
