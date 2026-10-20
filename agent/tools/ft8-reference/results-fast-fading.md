# LabLog decoder under fast fading

Produced 2026-10-20 (pull request #73) with the sweep of `channels.txt`:
seed 1, 40 recordings of 10 signals per condition, −24 to −8 dB, the same
recordings for every decoder; four LabLog decoders and WSJT-X 3.0.2 (`jt9 -8
-d 3`). "Before #68", "#68" and "Fading (#71)" are the three earlier
decoders; "Fast fading (#73)" is the one that came of this study. Every
LabLog decode is *received* (Principle XX); WSJT-X's a-priori decodes (its
`a1`…`a7` marks) are in its "all" column and left out of "received only".
The full study, with the diagnosis, the ablations and what was tried and
dropped, is `/mnt/project-files/ft8/fast-fading/report.md` in the project.

## What changed

- **Lock.** The Costas phases are fitted lag by lag (neighbouring symbols
  only, weight ρ^(k²) with ρ = 0.3), which holds when fading scrambles the
  phase over a whole array; then, once the channel has been read, frequency
  and drift are fitted again with the tones asked to agree over as many
  steps as that channel keeps them in phase, the channel is read again, and
  the fit repeated until the reading settles (`matched_fits`).
- **Channel classifier.** A maximum-likelihood reading of the symbol-to-
  symbol phase correlation from all three sync arrays: steady, or a Doppler
  spread of about 0.5, 1, 2 or 3 Hz (`classify_fading`).
- **Matched combining.** The one- to four-symbol tone sums are built by a
  combiner matched to that class (`Combiner`): the coherent sum on a steady
  channel, a power sum when the phase is scrambled, the Gaussian-channel
  optimum in between. Belief propagation still tries all four lengths.
- **Deep search by class.** Ordered-statistics decoding runs on the three-
  and four-symbol sets, or on the one- and three-symbol sets when the
  channel reads as 2 Hz or faster, where a lock that could not settle hurts
  the long sums most.
- **CPU.** The deep pass keeps the quick pass's likelihoods instead of
  re-locking and re-demodulating every candidate (`Leftover`); per-stage
  timers, counters and peak memory are reported by `ft8bench decode`
  (`decode/profile.rs`).
- Nothing that decides whether a decode is reported changed: the
  differential- and run-coherence thresholds, the deep-search margin and
  evenness rules, the agreement rule and the CRC are as in #71.

## Decodes per condition

| Condition | Before #68 | #68 | Fading (#71) | Fast fading (#73) | WSJT-X all | WSJT-X received only | Near misses, #71 → #73 |
|---|---:|---:|---:|---:|---:|---:|---:|
| Clean | 323 | 337 | 337 | **336** | 323 | 314 | 24 → 21 |
| Rayleigh, 0.05 Hz spread | 268 | 281 | 275 | **275** | 268 (2 false) | 260 (2 false) | 71 → 87 |
| Rayleigh, 0.2 Hz | 263 | 276 | 269 | **268** | 272 (1 false) | 264 (1 false) | 185 → 197 |
| Rayleigh, 0.5 Hz | 270 | 278 | 271 | **273** | 271 | 264 | 250 → 244 |
| Rayleigh, 1 Hz | 257 | 254 | 252 | **256** | 266 (1 false) | 258 | 358 → 273 |
| Rayleigh, 2 Hz | 245 (1 false) | 215 | 225 | **238** | 253 | 245 | 331 → 197 |
| Rician K=10 dB, 0.5 Hz | 310 | 333 | 332 | **329** | 314 | 313 | 51 → 50 |
| Rician K=6 dB, 1 Hz | 301 | 320 | 315 | **313** | 302 (1 false) | 299 (1 false) | 88 → 52 |
| Rician K=3 dB, 2 Hz | 285 | 301 | 294 | **293** | 292 | 284 | 95 → 94 |
| Deep fade 20 dB, 1 s | 308 | 327 | 326 | **326** | 314 | 305 | 44 → 53 |
| Deep fade 20 dB, 3 s | 269 | 293 | 287 | **284** | 276 | 270 | 118 → 103 |
| Deep fade 30 dB, 5 s | 174 | 187 | 173 | **165** | 181 | 165 | 2,387 → 2,882 |
| Drift 0.05 Hz/s | 321 | 337 | 341 | **340** | 326 | 322 | 41 → 34 |
| Drift 0.1 Hz/s | 317 | 317 | 342 | **340** | 315 (1 false) | 313 | 47 → 44 |
| Drift 0.2 Hz/s | 303 (1 false) | 292 | 339 | **337** | 307 | 301 | 36 → 33 |
| Drift 0.5 Hz/s | 263 (1 false) | 151 | 307 | **311** | 273 (1 false) | 262 (1 false) | 74 → 95 |
| Curved drift, 1 Hz | 317 | 313 | 313 | **321** | 313 | 311 | 11 → 18 |
| Curved drift, 3 Hz | 296 | 248 | 260 | **258** | 294 | 292 | 22 → 42 |
| Wobble 0.5 Hz, 4 s period | 313 | 321 | 316 | **320** | 308 (1 false) | 305 (1 false) | 15 → 24 |
| Wobble 1 Hz, 2 s period | 299 | 281 | 298 | **298** | 299 | 294 | 19 → 14 |
| Phase noise 0.1 Hz | 319 | 336 | 336 | **335** | 318 | 315 | 21 → 7 |
| Phase noise 0.5 Hz | 300 | 307 | 306 | **311** | 301 | 297 | 18 → 19 |
| Phase noise 2 Hz | 250 | 227 | 227 | **238** | 259 (1 false) | 255 (1 false) | 20 → 12 |
| Echo 3 ms | 271 | 278 | 278 | **277** | 269 | 264 | 89 → 64 |
| Echo 20 ms, -3 dB | 271 | 284 | 275 | **279** | 284 | 273 | 399 → 274 |
| Echo 2 ms, +1 Hz Doppler | 274 | 270 | 271 | **287** | 283 | 273 | 231 → 119 |
| ITU quiet (0.1 Hz, 0.5 ms) | 257 | 273 | 268 | **267** | 262 | 252 | 188 → 182 |
| ITU moderate (0.5 Hz, 1 ms) | 263 | 277 | 269 | **269** | 268 | 260 | 257 → 217 |
| ITU disturbed (1 Hz, 2 ms) | 250 | 248 | 247 | **250** | 265 | 255 | 254 → 198 |
| ITU flutter (10 Hz, 1 ms) | 2 | 1 | 3 | **1** | 15 | 8 | 1,543 → 1,370 |
| Combination, mild | 254 | 263 | 261 | **257** | 264 | 255 | 251 → 325 |
| Combination, rough | 186 | 131 | 166 | **166** | 192 | 183 | 807 → 676 |
| **All 32** | **8599** | **8557** | **8779** | **8818** | **8747** | **8531** | **8,345 → 8,020** |

Near misses are wrong codewords that reached the CRC and were stopped by it
alone; divided by 16,383 they give the false decodes to expect. No LabLog
decoder made a false decode in this study.

## Held-out seed 9

Fresh recordings never used while tuning:

| Condition | #68 | Fading (#71) | Fast fading (#73) | WSJT-X all | WSJT-X received only | Near misses, #71 → #73 |
|---|---:|---:|---:|---:|---:|---:|
| Rayleigh, 0.5 Hz | 279 | 273 | **269** | 272 (1 false) | 266 (1 false) | 267 → 245 |
| Rayleigh, 1 Hz | 257 | 256 | **259** | 261 (1 false) | 252 (1 false) | 279 → 226 |
| Rayleigh, 2 Hz | 225 | 225 | **240** | 255 (2 false) | 242 (2 false) | 335 → 213 |
| ITU disturbed (1 Hz, 2 ms) | 262 | 259 | **259** | 266 | 256 | 375 → 287 |
| Phase noise 0.5 Hz | 317 | 317 | **321** | 309 | 303 | 22 → 10 |
| Phase noise 2 Hz | 220 | 226 | **237** | 262 | 256 | 19 → 16 |
| Drift 0.2 Hz/s | 308 | 344 | **343** | 321 (2 false) | 315 (2 false) | 24 → 13 |
| Deep fade 20 dB, 3 s | 308 | 303 | **300** | 279 | 275 | 135 → 116 |
| Combination, rough | 132 | 171 | **174** | 196 (1 false) | 185 (1 false) | 736 → 731 |
| **All 9** | **2308** | **2374** | **2402** | **2421** | **2350** | |

## Clean regression and noise-only

| Set | #68 | Fading (#71) | Fast fading (#73) | WSJT-X all | WSJT-X received only | Near misses, #71 → #73 |
|---|---:|---:|---:|---:|---:|---:|
| Clean seed 1 (600 signals, −26 to −4 dB) | 485 | 485 | **484** | 458 | 452 | 34 → 24 |
| Clean seed 9 | 491 | 491 | **492** | 470 | 465 | 22 → 23 |
| Clean seed 10 | 481 | 479 | **480** | 461 | 455 | 31 → 29 |
| Noise only, seed 7 (200 recordings) | – | 0 | **0** | – | – | 0 → 0 |
| Noise only, seed 8 (200 recordings) | – | 0 | **0** | – | – | 0 → 0 |

## CPU and memory

| Condition | Decoder | Decodes | Wall time, 40 recordings | Per recording | Slowest recording | Thread time per decode | Peak memory |
|---|---|---:|---:|---:|---:|---:|---:|
| Clean | #68 | 337 | 122 s | 3.04 s | 3.72 s | – | – |
|  | Fading (#71) | 337 | 119 s | 2.97 s | 3.68 s | – | – |
|  | First new | 336 | 154 s | 3.85 s | 5.03 s | 1.51 s | 30 MB |
|  | **Final** | 336 | 156 s | 3.89 s | 5.01 s | 1.52 s | 30 MB |
|  | First new with the deep search on all four sets (E) | 335 | 164 s | 4.10 s | 4.91 s | 1.73 s | – |
| Rayleigh, 0.5 Hz | #68 | 278 | 119 s | 2.98 s | 3.72 s | – | – |
|  | Fading (#71) | 271 | 114 s | 2.86 s | 3.43 s | – | – |
|  | First new | 273 | 144 s | 3.59 s | 4.38 s | 1.74 s | 30 MB |
|  | **Final** | 273 | 138 s | 3.45 s | 4.51 s | 1.70 s | 31 MB |
| Rayleigh, 2 Hz | #68 | 215 | 107 s | 2.68 s | 3.60 s | – | – |
|  | Fading (#71) | 225 | 111 s | 2.78 s | 4.17 s | – | – |
|  | First new | 242 | 146 s | 3.64 s | 4.66 s | 1.97 s | 30 MB |
|  | **Final** | 238 | 138 s | 3.44 s | 4.19 s | 1.92 s | 31 MB |
|  | First new with the deep search on all four sets (E) | 249 | 162 s | 4.05 s | 4.97 s | 2.30 s | – |
| Drift 0.1 Hz/s | #68 | 317 | 112 s | 2.81 s | 3.54 s | – | – |
|  | Fading (#71) | 342 | 109 s | 2.73 s | 3.52 s | – | – |
|  | First new | 323 | 137 s | 3.42 s | 4.19 s | 1.44 s | 30 MB |
|  | **Final** | 340 | 144 s | 3.61 s | 4.51 s | 1.43 s | 31 MB |
| Combination, rough | #68 | 131 | 85 s | 2.13 s | 2.91 s | – | – |
|  | Fading (#71) | 166 | 92 s | 2.31 s | 3.22 s | – | – |
|  | First new | 158 | 116 s | 2.91 s | 3.85 s | 2.42 s | 30 MB |
|  | **Final** | 166 | 120 s | 3.01 s | 4.26 s | 2.37 s | 31 MB |

## Against WSJT-X, honestly

A little better overall and clearly better on steady and drifting signals;
still behind under fast fading. Seed 1: 8818 to 8747 (
8531 received), 20 conditions won and 12 lost. WSJT-X
leads on the rough combination (166 to 192), phase noise 2 Hz (238 to 259),
curved drift 3 Hz (258 to 294), ITU disturbed (250 to 265) and 2 Hz Rayleigh
(238 to 253, 245 received). The 2 Hz gap has narrowed from 28 to 15 since
#71. These are LabLog's own synthetic channels: a fair test between
decoders on identical recordings, not proof about the air.
