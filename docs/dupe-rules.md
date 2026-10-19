# What counts as a dupe

Research for LabLog's contest overlays, 18 October 2026. Each rule here is encoded in
the overlay files in `engine/src/loglab_engine/contests/templates/`. Dupe checks run only in contest overlays; Everyday never checks.

## ARRL Field Day

Source: [2026 ARRL Field Day Rules](https://www.arrl.org/files/file/Field-Day/2026/2026-Field-Day-Rules.pdf)
(revised 1 March 2026; ARRL says there are no changes from 2025).

What the rules say:

- Rule 6.3: "Field Day contacts are allowed using Phone, CW and Digital (non-CW)
  modes." Stations "can be worked once per band per mode under this rule."
- Rules 6.6 and 6.7: all voice contacts are equivalent, and all non-CW digital
  contacts are equivalent. So SSB, FM and AM are one mode (Phone), and RTTY,
  PSK31 and FT8 are one mode (Digital).
- Rule 2: the Field Day bands are 160, 80, 40, 20, 15 and 10 m, and every band
  from 6 m up. 60, 30, 17 and 12 m are not Field Day bands.
- Rule 7.3.7: satellite QSOs are listed as their own "band" on the summary.
- Rule 6.8: cross-band contacts are not allowed (satellites are exempt).
- Rule 4.1.1.1: the GOTA station uses a different callsign from the main station.

So a QSO is a Field Day dupe when the log already has a QSO with:

1. the same call worked,
2. on the same band (satellite counts as its own band),
3. in the same mode group (Phone, CW or Digital),
4. made by the same station callsign during the same Field Day.

The GOTA station has its own callsign, so its QSOs are checked on their own:
a station the main station worked on 20 m CW can still be worked by the GOTA
station on 20 m CW.

## Winter Field Day

Source: [Winter Field Day 2026 Rules, V3](https://winterfieldday.org/downloads/2026-rules-v3.pdf)
(dated 8 September 2025). The 2027 rules were not posted yet when I checked.

What the rules say:

- Dupe rule: "You may only contact other field day stations once per band-mode
  combination, for a maximum of three times per band." A repeat on the same band
  and mode earns no points.
- Modes: Phone (SSB, AM, FM, DMR, C4FM and other voice), CW, and Digital (PSK,
  RTTY, Olivia, Packet, SSTV, JS8Call and other soundcard modes).
- WSJT modes are not allowed at all: FT8, FT4, FST4, JT9, JT65, Q65, MSK144 and
  WSPR.
- Bands: every amateur band except 12, 17, 30 and 60 m.
- Satellite QSOs earn no QSO points; they count only toward the satellite
  objectives.

So a QSO is a Winter Field Day dupe under the same four tests as Field Day:
same call, same band, same mode group, same station callsign, during the same
Winter Field Day. The difference from Field Day is which bands count, and that
FT8, FT4 and the other WSJT modes don't count at all.

## Decisions LabLog makes where the rules are silent

- **Portable and mobile suffixes.** The rules count stations by the call they
  sign, so K9KSU and K9KSU/M are different stations. LabLog compares the call
  exactly as logged, ignoring upper or lower case.
- **Dupes are warned, not blocked.** Neither contest penalizes a dupe, and ARRL
  asks for a log of stations worked. LabLog shows a dupe warning before you
  store the QSO, and you can still store it (the other station may have lost
  your first QSO). A stored dupe scores nothing.
- **Which Field Day.** Field Day is in June and Winter Field Day in January, so
  "the same Field Day" means the same contest in the same calendar year.
- **Off-band and off-mode QSOs.** A QSO on a band or mode the contest doesn't
  count (17 m at Field Day, FT8 at Winter Field Day) gets its own warning:
  "doesn't count in this contest". It is not called a dupe.
