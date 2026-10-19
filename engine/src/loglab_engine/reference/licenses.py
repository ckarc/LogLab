# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading the US and Canadian amateur license lists, as their agencies publish them.

* The FCC's Universal Licensing System publishes every amateur license weekly
  as ``l_amat.zip``, and each day's changes as ``l_am_<day>.zip``. Both hold
  pipe-separated files: ``HD.dat`` (the license: call, status, expiry),
  ``EN.dat`` (the licensee: name and address) and ``AM.dat`` (operator
  class), joined by the FCC's unique system identifier. FCC data is a work of
  the US government, in the public domain.
* ISED Canada publishes every amateur call in ``amateur_delim.zip``: one
  semicolon-separated text file, column names first. It is published under
  the Open Government Licence - Canada, which asks for :data:`ISED_CREDIT`.

Both downloads are untrusted (Principle XII). A zip member larger than its
cap is refused; a line longer than :data:`MAX_LINE` is skipped and counted;
every value kept is cleaned and length-capped; a record without a valid call
or identifier is skipped and counted, so the importer can refuse a file that
is mostly unreadable. Street addresses, email and phone numbers are never
read into the Engine: only what fills in a QSO is kept.
"""

from __future__ import annotations

import io
import re
import unicodedata
from dataclasses import dataclass, field
from datetime import date, datetime
from typing import TYPE_CHECKING, Final, cast

if TYPE_CHECKING:
    import zipfile
    from collections.abc import Iterator

FCC_CREDIT: Final = "US licenses: FCC Universal Licensing System (public domain)."
ISED_CREDIT: Final = (
    "Canadian licenses: contains information licensed under the Open Government"
    " Licence - Canada (Innovation, Science and Economic Development Canada)."
)
MAX_LINE: Final = 4096
MAX_MEMBER: Final = 2 * 1024 * 1024 * 1024
GRACE_YEARS: Final = 2  # an expired US license can be renewed for two years

_CALL: Final = re.compile(r"^[A-Z0-9]{3,10}$")
_USI: Final = re.compile(r"^[0-9]{1,12}$")
_ROMAN: Final = frozenset({"II", "III", "IV", "V", "VI", "VII", "VIII"})
_FCC_CLASS: Final = {
    "A": "Advanced",
    "E": "Extra",
    "G": "General",
    "N": "Novice",
    "P": "Technician Plus",
    "T": "Technician",
}


class LicenseFileError(Exception):
    """A license download is not the file it should be."""


@dataclass(slots=True)
class Tally:
    """How many records a file held, and how many could not be used."""

    read: int = 0
    skipped: int = 0
    examples: list[str] = field(default_factory=list)

    def skip(self, why: str) -> None:
        """Count one unusable record, keeping a few reasons to report."""
        self.skipped += 1
        if len(self.examples) < 3:  # noqa: PLR2004 - a few examples are enough
            self.examples.append(why)


@dataclass(frozen=True, slots=True)
class Header:
    """One FCC license (``HD.dat``). ``kept`` is false for one LogLab does not keep."""

    usi: str
    call: str
    active: bool
    expires: date | None
    kept: bool


@dataclass(frozen=True, slots=True)
class Entity:
    """One FCC licensee (``EN.dat``). ``licensee`` is false for another kind of entity."""

    usi: str
    name: str | None
    city: str | None
    state: str | None
    licensee: bool


@dataclass(frozen=True, slots=True)
class OperatorClass:
    """One FCC operator class (``AM.dat``)."""

    usi: str
    license_class: str | None


@dataclass(frozen=True, slots=True)
class CanadianLicense:
    """One call from ISED's list."""

    call: str
    name: str | None
    city: str | None
    province: str | None
    license_class: str | None


def clean_text(value: str | None, limit: int = 64) -> str | None:
    """``value`` with control characters removed, spaces collapsed, and length capped."""
    if value is None:
        return None
    kept = "".join(" " if unicodedata.category(c).startswith("C") else c for c in value)
    collapsed = " ".join(kept.split())[:limit].strip()
    return collapsed or None


def tidy_case(value: str | None) -> str | None:
    """``value`` in title case if it is written all in capitals, as the FCC writes names."""
    if value is None or value != value.upper():
        return value
    words = [w if w in _ROMAN else w.title() for w in value.split(" ")]
    return " ".join(words)


def _call(value: str) -> str | None:
    text = clean_text(value, 20)
    if text is None:
        return None
    text = text.upper()
    return text if _CALL.match(text) else None


def _fcc_date(value: str) -> date | None:
    try:
        return datetime.strptime(value.strip(), "%m/%d/%Y").date()  # noqa: DTZ007 - a date only
    except ValueError:
        return None


def _member(archive: zipfile.ZipFile, name: str, cap: int = MAX_MEMBER) -> zipfile.ZipInfo:
    wanted = name.lower()
    info = next(
        (i for i in archive.infolist() if i.filename.rsplit("/", 1)[-1].lower() == wanted), None
    )
    if info is None:
        raise LicenseFileError(f"the download has no {name}")
    if info.file_size > cap:
        raise LicenseFileError(f"{name} is larger than {cap} bytes")
    return info


def _decode(line: bytes) -> str:
    """A line as text.

    Both agencies write UTF-8; an odd line in an older single-byte code page
    is read as Latin-1, which decodes any byte. Cleaning later drops control
    characters.
    """
    try:
        return line.decode("utf-8")
    except UnicodeDecodeError:
        return line.decode("latin-1")


def _lines(archive: zipfile.ZipFile, info: zipfile.ZipInfo, tally: Tally) -> Iterator[str]:
    """Each line of a member, decoded; an overlong line is skipped and counted."""
    # A zip member's own readline can return more than it is asked for.
    with io.BufferedReader(cast("io.RawIOBase", archive.open(info)), 1024 * 1024) as raw:
        while chunk := raw.readline(MAX_LINE + 1):
            if len(chunk) > MAX_LINE and not chunk.endswith(b"\n"):
                while (rest := raw.readline(MAX_LINE + 1)) and not rest.endswith(b"\n"):
                    pass
                tally.read += 1
                tally.skip(f"a line in {info.filename} is longer than {MAX_LINE} bytes")
                continue
            yield _decode(chunk.rstrip(b"\r\n"))


def _fcc_records(
    archive: zipfile.ZipFile, name: str, kind: str, width: int, tally: Tally
) -> Iterator[list[str]]:
    """The records of one FCC file, each split into at least ``width`` fields.

    A record that runs over a line break (some licensee addresses do) is
    joined back together: every record starts with its kind, such as ``HD|``.
    """
    prefix = f"{kind}|"
    current: str | None = None

    def finish(record: str) -> list[str] | None:
        tally.read += 1
        fields = record.split("|")
        if len(fields) < width:
            tally.skip(f"a {kind} record has {len(fields)} fields, fewer than {width}")
            return None
        if _USI.match(fields[1]) is None:
            tally.skip(f"a {kind} record has no valid identifier")
            return None
        return fields

    for line in _lines(archive, _member(archive, name), tally):
        if line.startswith(prefix):
            if current is not None and (fields := finish(current)) is not None:
                yield fields
            current = line
        elif current is not None and line:
            current = f"{current} {line}"
            if len(current) > MAX_LINE:
                tally.read += 1
                tally.skip(f"a {kind} record is longer than {MAX_LINE} bytes")
                current = None
        elif line:
            tally.read += 1
            tally.skip(f"a line in {name} does not start with {prefix}")
    if current is not None and (fields := finish(current)) is not None:
        yield fields


def fcc_headers(archive: zipfile.ZipFile, today: date, tally: Tally) -> Iterator[Header]:
    """Every license in an FCC download, marking the ones LogLab keeps.

    An active license is kept; so is an expired one still within the two-year
    grace period in which it can be renewed. Cancelled and terminated ones are not.
    """
    oldest = today.replace(year=today.year - GRACE_YEARS, day=min(today.day, 28))
    for fields in _fcc_records(archive, "HD.dat", "HD", 10, tally):
        call = _call(fields[4])
        if call is None:
            tally.skip("an HD record has no valid call")
            continue
        status = fields[5].strip().upper()
        expires = _fcc_date(fields[8])
        active = status == "A"
        kept = active or (status == "E" and expires is not None and expires >= oldest)
        yield Header(fields[1], call, active, expires, kept)


def fcc_entities(archive: zipfile.ZipFile, tally: Tally) -> Iterator[Entity]:
    """Every licensee name and place in an FCC download (never the street address)."""
    for fields in _fcc_records(archive, "EN.dat", "EN", 19, tally):
        first, middle, last, suffix = (clean_text(f) for f in fields[8:12])
        name: str | None
        if first or last:
            name = " ".join(p for p in (first, middle, last) if p)
            name = tidy_case(name)
            if suffix:
                name = f"{name} {tidy_case(suffix)}"
        else:
            name = clean_text(fields[7], 128)  # a club or other entity, written as filed
        state = clean_text(fields[17], 32)
        yield Entity(
            usi=fields[1],
            name=clean_text(name, 128),
            city=tidy_case(clean_text(fields[16])),
            state=None if state is None else state.upper(),
            licensee=fields[5].strip().upper() == "L",
        )


def fcc_classes(archive: zipfile.ZipFile, tally: Tally) -> Iterator[OperatorClass]:
    """Every operator class in an FCC download; a club station has none."""
    for fields in _fcc_records(archive, "AM.dat", "AM", 6, tally):
        yield OperatorClass(fields[1], _FCC_CLASS.get(fields[5].strip().upper()))


# ISED's amateur_delim.txt, as its readme describes it: call, given name,
# surname, street, city, province, postal code, the five qualifications (A
# Basic, B and C Morse, D Advanced, E Basic with Honours), then a club's name,
# second name, street, city, province and postal code.
_ISED_FIELDS: Final = 18
_ISED_CLUB_NAME: Final = 12
_ISED_CLUB_CITY: Final = 15


def _ised_class(qualifications: list[str]) -> str | None:
    held = {q.strip().upper() for q in qualifications if q.strip()}
    if "D" in held:
        level = "Advanced"
    elif "E" in held:
        level = "Basic with Honours"
    elif "A" in held:
        level = "Basic"
    else:
        return None
    return f"{level}, Morse" if held & {"B", "C"} else level


def ised_licenses(archive: zipfile.ZipFile, tally: Tally) -> Iterator[CanadianLicense]:
    """Every call in ISED's download (never the street address or postal code)."""
    info = next((i for i in archive.infolist() if i.filename.lower().endswith(".txt")), None)
    if info is None:
        raise LicenseFileError("the download has no amateur_delim.txt")
    if info.file_size > MAX_MEMBER:
        raise LicenseFileError(f"{info.filename} is larger than {MAX_MEMBER} bytes")
    first = True
    for line in _lines(archive, info, tally):
        if not line.strip():
            continue
        fields = [f.strip() for f in line.split(";")]
        if first and fields[0].lower() == "callsign":
            first = False
            continue  # the column names
        first = False
        tally.read += 1
        if len(fields) < _ISED_FIELDS:
            tally.skip(f"an ISED line has {len(fields)} fields, fewer than {_ISED_FIELDS}")
            continue
        call = _call(fields[0])
        if call is None:
            tally.skip("an ISED line has no valid call")
            continue
        given, surname = clean_text(fields[1]), clean_text(fields[2])
        if given or surname:
            name = tidy_case(" ".join(p for p in (given, surname) if p))
            city = clean_text(fields[4])
            province = clean_text(fields[5], 32)
        else:
            club = (clean_text(fields[_ISED_CLUB_NAME]), clean_text(fields[_ISED_CLUB_NAME + 1]))
            name = " ".join(p for p in club if p) or None
            city = clean_text(fields[_ISED_CLUB_CITY])
            province = clean_text(fields[_ISED_CLUB_CITY + 1], 32)
        yield CanadianLicense(
            call=call,
            name=clean_text(name, 128),
            city=tidy_case(city),
            province=None if province is None else province.upper(),
            license_class=_ised_class(fields[7:12]),
        )
