# WSJT-X baseline on the FT8 bench

Produced 2026-10-19 by `run-reference.sh 1` with WSJT-X 3.0.2 (commit
`967c85a`), `jt9 -8 -d 3` (FT8, deepest decoding, no a priori callsign).
60 recordings, 10 signals each, white noise, signals at least 60 Hz apart,
timing offsets from -0.4 to +1.0 s.

## WSJT-X 3.0.2 (jt9 -d 3), seed 1

| SNR (dB) | Sent | Found | Found % |
|---|---|---|---|
| -26 to -24 | 49 | 0 | 0.0 |
| -24 to -22 | 60 | 2 | 3.3 |
| -22 to -20 | 71 | 42 | 59.2 |
| -20 to -18 | 53 | 49 | 92.5 |
| -18 to -16 | 43 | 43 | 100.0 |
| -16 to -14 | 53 | 53 | 100.0 |
| -14 to -12 | 55 | 54 | 98.2 |
| -12 to -10 | 59 | 58 | 98.3 |
| -10 to -8 | 40 | 40 | 100.0 |
| -8 to -6 | 55 | 55 | 100.0 |
| -6 to -4 | 60 | 60 | 100.0 |
| -4 to -2 | 2 | 2 | 100.0 |
| **All** | **600** | **458** | **76.3** |

False decodes: 0

Half of the signals are found at about -21 dB, which matches FT8's published
sensitivity. That confirms the bench's signal levels are right.

WSJT-X missed two strong signals: `A7WS E7V RRR` (slot 10, 873 Hz, -10.2 dB)
and `G9OSE RQ4ZH RRR` (slot 54, 1200 Hz, -14.0 dB). A LabLog decoder that
finds them as well is doing better than WSJT-X, not worse.
