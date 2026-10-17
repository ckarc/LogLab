# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Ghost QSOs: synthetic, deterministic test data shaped by the ADIF specification.

Ghosts let LogLab be exercised without anyone's real log. Every ghost is
unmistakably fake:

* callsigns use the ``Q`` prefix, which the ITU does not issue to stations;
* every record carries ``<APP_LOGLAB_GHOST:1:B>Y``;
* the file header says the contacts are synthetic and must not be uploaded.

The same seed and settings always produce the same document. Records rotate
through ADI edge cases (data containing ``<EOR>``, multiline notes, empty data,
lowercase names, type indicators, user-defined and application-defined fields,
shuffled field order, long values) so any file of ten or more ghosts exercises
all of them.

Band edges, modes, submodes, and contest IDs are taken from the ADIF 3.1.7
specification (III.B.4 Band, III.B.10 Mode, III.B.11 Submode, III.B.5 Contest ID).
"""

# Ghosts need reproducible pseudo-randomness, not cryptographic randomness.
# ruff: noqa: S311

from __future__ import annotations

import random
import string
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Final

from loglab_engine import __version__
from loglab_engine.adif.model import AdifDocument, Field, Header, Record

ADIF_VERSION: Final = "3.1.7"
GHOST_FLAG: Final = "APP_LOGLAB_GHOST"
STATION_CALLSIGN: Final = "Q0LAB"
EDGE_CASES: Final = 10

_PREAMBLE: Final = (
    "LogLab ghost QSOs - synthetic test data, not real contacts.\n"
    "Do not upload to LoTW, Club Log, eQSL, QRZ, or any other service.\n"
)

# (band, lower MHz, upper MHz) from ADIF 3.1.7 III.B.4.
_BANDS: Final = (
    ("160m", 1.8, 2.0),
    ("80m", 3.5, 4.0),
    ("40m", 7.0, 7.3),
    ("30m", 10.1, 10.15),
    ("20m", 14.0, 14.35),
    ("17m", 18.068, 18.168),
    ("15m", 21.0, 21.45),
    ("12m", 24.89, 24.99),
    ("10m", 28.0, 29.7),
    ("6m", 50.0, 54.0),
    ("2m", 144.0, 148.0),
    ("70cm", 420.0, 450.0),
)


@dataclass(frozen=True, slots=True)
class _Mode:
    mode: str
    submode: str | None
    report: str  # "rst" (599), "rs" (59), or "db" (-12)


# Modes and submodes from ADIF 3.1.7 III.B.10-11. SSB's submode follows the
# usual sideband convention for the band.
_MODES: Final = (
    _Mode("CW", None, "rst"),
    _Mode("SSB", "USB", "rs"),
    _Mode("FM", None, "rs"),
    _Mode("RTTY", None, "rst"),
    _Mode("FT8", None, "db"),
    _Mode("MFSK", "FT4", "db"),
    _Mode("PSK", "PSK31", "rst"),
)

_CONTESTS: Final = ("ARRL-FIELD-DAY", "WFD", "KS-QSO-PARTY", "CQ-WW-SSB", "ARRL-SS-CW")
_NAMES: Final = ("Wisp", "Shade", "Echo", "Mist", "Vapor", "Haze", "Murk", "Gloam")
_QTHS: Final = (
    "Ghost Hollow",
    "Spectre Falls",
    "Wraithburg",
    "Phantom Ridge",
    "Haunt Creek",
    "Mistvale",
)
_USERDEF: Final = Field("USERDEF1", "GHOST_LEVEL,{1:9}", "N")


def _callsign(rng: random.Random) -> str:
    suffix = "".join(rng.choices(string.ascii_uppercase, k=rng.choice((2, 3))))
    call = f"Q{rng.randint(0, 9)}{suffix}"
    return call + "/P" if rng.random() < 0.05 else call  # noqa: PLR2004


def _grid(rng: random.Random) -> str:
    field = "".join(rng.choices("ABCDEFGHIJKLMNOPQR", k=2))
    square = "".join(rng.choices(string.digits, k=2))
    if rng.random() < 0.5:  # noqa: PLR2004
        return field + square
    return field + square + "".join(rng.choices("abcdefghijklmnopqrstuvwx", k=2))


def _report(rng: random.Random, style: str) -> str:
    if style == "db":
        return f"{rng.randint(-24, 10):+03d}"
    strength = rng.randint(3, 9)
    return f"5{strength}9" if style == "rst" else f"5{strength}"


def _qso(rng: random.Random, index: int, when: datetime) -> list[Field]:
    band, low, high = rng.choice(_BANDS)
    spec = rng.choice(_MODES)
    submode = spec.submode
    if spec.mode == "SSB":
        submode = "LSB" if high < 10.0 else "USB"  # noqa: PLR2004
    off = when + timedelta(seconds=rng.randint(15, 300))
    fields = [
        Field("CALL", _callsign(rng)),
        Field("QSO_DATE", when.strftime("%Y%m%d")),
        Field("TIME_ON", when.strftime("%H%M")),
        Field("TIME_OFF", off.strftime("%H%M%S")),
        Field("BAND", band),
        Field("FREQ", f"{rng.uniform(low, high):.3f}"),
        Field("MODE", spec.mode),
    ]
    if submode is not None:
        fields.append(Field("SUBMODE", submode))
    fields += [
        Field("RST_SENT", _report(rng, spec.report)),
        Field("RST_RCVD", _report(rng, spec.report)),
        Field("NAME", rng.choice(_NAMES)),
        Field("QTH", rng.choice(_QTHS)),
        Field("GRIDSQUARE", _grid(rng)),
        Field("TX_PWR", str(rng.choice((5, 10, 50, 100, 500)))),
        Field("STATION_CALLSIGN", STATION_CALLSIGN),
    ]
    return [*_edge_case(rng, index, fields), Field(GHOST_FLAG, "Y", "B")]


def _edge_case(rng: random.Random, index: int, fields: list[Field]) -> list[Field]:  # noqa: PLR0911
    """Give record ``index`` one of the ADI edge cases, cycling through all of them."""
    case = index % EDGE_CASES
    if case == 0:  # Data that looks like markup; only the length says where it ends.
        return [*fields, Field("COMMENT", "<EOR> is not the end <CALL:4>FAKE <EOH>")]
    if case == 1:  # MultilineString with CRLF line breaks.
        return [*fields, Field("NOTES", "Ghost line one\r\nGhost line two\r\n", "M")]
    if case == 2:  # noqa: PLR2004 - zero-length data.
        return [*fields, Field("COMMENT", "")]
    if case == 3:  # noqa: PLR2004 - field names are case-insensitive.
        return [Field(f.name.lower(), f.value, f.type_indicator) for f in fields]
    if case == 4:  # noqa: PLR2004 - explicit data type indicators.
        indicators = {"QSO_DATE": "D", "TIME_ON": "T", "TIME_OFF": "T", "FREQ": "N", "CALL": "S"}
        return [Field(f.name, f.value, indicators.get(f.name)) for f in fields]
    if case == 5:  # noqa: PLR2004 - contest exchange.
        return [
            *fields,
            Field("CONTEST_ID", rng.choice(_CONTESTS)),
            Field("STX", str(index + 1)),
            Field("SRX", str(rng.randint(1, 2000))),
        ]
    if case == 6:  # noqa: PLR2004 - a user-defined field declared in the header.
        return [*fields, Field("ghost_level", str(rng.randint(1, 9)))]
    if case == 7:  # noqa: PLR2004 - an application-defined field without an indicator.
        return [*fields, Field("APP_LOGLAB_HAUNTING", "lightly")]
    if case == 8:  # noqa: PLR2004 - fields may appear in any order.
        shuffled = list(fields)
        rng.shuffle(shuffled)
        return shuffled
    long_notes = "\r\n".join(f"Ghost note line {n:03d}." for n in range(200))
    return [*fields, Field("NOTES", long_notes)]


DEFAULT_START: Final = datetime(2026, 6, 27, 18, 0, tzinfo=UTC)


def generate(count: int, seed: int = 1, start: datetime = DEFAULT_START) -> AdifDocument:
    """Generate ``count`` ghost QSOs starting at ``start`` (UTC), deterministically.

    Raises:
        ValueError: ``count`` is negative or ``start`` is not in UTC.
    """
    if count < 0:
        msg = "count must not be negative"
        raise ValueError(msg)
    if start.utcoffset() != timedelta(0):
        msg = "start must be a UTC datetime (Principle VIII)"
        raise ValueError(msg)
    rng = random.Random(seed)
    header = Header(
        _PREAMBLE,
        (
            Field("ADIF_VER", ADIF_VERSION),
            Field("PROGRAMID", "LogLab"),
            Field("PROGRAMVERSION", __version__),
            Field("CREATED_TIMESTAMP", start.strftime("%Y%m%d %H%M%S")),
            _USERDEF,
        ),
    )
    records: list[Record] = []
    when = start
    for index in range(count):
        records.append(Record(tuple(_qso(rng, index, when))))
        when += timedelta(seconds=rng.randint(20, 240))
    return AdifDocument(header, tuple(records))
