# Operation stats and On air

Two boxes for a group operation such as Field Day. Each operator shows or hides
them for themselves; an overlay's starting layout can show them.

## Operation stats

The Engine works out every number from the log (`GET /api/v1/operations/{id}/stats`),
so every workstation shows the same ones. The box asks again every 20 seconds while
it is on screen, and right after a QSO is stored at that workstation.

Which stats show is set by the overlay's `stats.show`, chosen in the overlay editor:

| Stat        | What it shows |
| ----------- | ------------- |
| `rate`      | Total QSOs, QSOs in the last hour, the hourly rate at the pace of the last 10 minutes, the best hour |
| `hourly`    | A bar for each UTC hour of the operation |
| `score`     | QSO points: each counting QSO that is not a dupe earns its mode group's points |
| `band_mode` | Counting QSOs by band and mode group |
| `operators` | Each operator's QSOs, in all and in the last hour, and their points |
| `dupes`     | How many QSOs repeat an earlier contact (needs the overlay's dupe check) |
| `sections`  | ARRL and RAC sections worked, of 85 |

An overlay with no `stats` shows rate, hourly, band and mode, operators, and dupes
when it has a dupe check.

### Points

The score is QSO points only. Power multipliers and bonus points are not counted.

- ARRL Field Day, 2026 rules, Rule 7.1: Phone 1 point, CW 2, Digital 2.
- Winter Field Day, 2026 rules V3: Phone 1, CW 2, Digital 2. Satellite QSOs earn
  no QSO points (`"satellite_points": false`).

Dupes follow [the dupe rules](dupe-rules.md), counted within the operation: the
first QSO counts and later ones are dupes.

## On air

While a browser is in an operation it tells the Engine every 30 seconds, and when
the band or mode changes, what its New QSO box is set to
(`PUT /api/v1/operations/{id}/on-air`). The Engine keeps one line per operator per
station, in memory only, and drops a line it has not heard from in 90 seconds or
whose operator signs out. Nothing about it is written to the log.

A line is **active** when its operator logged a QSO in the operation in the last 15
minutes, and **idle** otherwise. Two lines on the same band in the same mode group
under the same call are both marked, since Field Day allows one transmitter per
band and mode. The band and mode are as the operator set them, not read from a
radio.
