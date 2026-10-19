# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Stand-in FCC and ISED downloads, laid out as the agencies publish them."""

from __future__ import annotations

import zipfile
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable
    from pathlib import Path


def hd(usi: str, call: str, status: str = "A", expires: str = "03/01/2034") -> str:
    """One HD.dat record: a license."""
    fields = ["HD", usi, "", "", call, status, "HA", "03/01/2024", expires, "", ""]
    return "|".join(fields + [""] * (59 - len(fields)))


def en(  # noqa: PLR0913 - one argument per field the Engine reads
    usi: str,
    call: str,
    first: str = "",
    last: str = "",
    *,
    middle: str = "",
    suffix: str = "",
    city: str = "NEWINGTON",
    state: str = "CT",
    entity: str = "",
    kind: str = "L",
) -> str:
    """One EN.dat record: a licensee, with a street address the Engine must not keep."""
    fields = [
        "EN",
        usi,
        "",
        "",
        call,
        kind,
        "L00001",
        entity,
        first,
        middle,
        last,
        suffix,
        "8605550100",
        "",
        "ham@example.org",
        "225 MAIN ST",
        city,
        state,
        "06111",
        "",
        "",
        "",
        "0001234567",
        "I",
    ]
    return "|".join(fields + [""] * (30 - len(fields)))


def am(usi: str, call: str, license_class: str = "E") -> str:
    """One AM.dat record: an operator class."""
    fields = ["AM", usi, "", "", call, license_class, "D", "1", "", ""]
    return "|".join(fields + [""] * (18 - len(fields)))


def fcc_zip(
    path: Path,
    headers: Iterable[str],
    entities: Iterable[str] = (),
    classes: Iterable[str] = (),
) -> Path:
    """Write an FCC download (weekly or daily) holding these records."""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("HD.dat", "".join(f"{r}\r\n" for r in headers))
        archive.writestr("EN.dat", "".join(f"{r}\r\n" for r in entities))
        archive.writestr("AM.dat", "".join(f"{r}\r\n" for r in classes))
        archive.writestr("counts", "File Creation Date: Sun Oct 18 2026\r\n")
    return path


ISED_HEADER = (
    "callsign;first_name;surname;address_line;city;prov_cd;postal_code;qual_a;qual_b;"
    "qual_c;qual_d;qual_e;club_name;club_name_2;club_address;club_city;club_prov_cd;"
    "club_postal_code"
)


def ised(  # noqa: PLR0913 - one argument per field the Engine reads
    call: str,
    first: str = "",
    last: str = "",
    city: str = "",
    *,
    province: str = "",
    qualifications: str = "A",
    club: str = "",
    club_city: str = "",
    club_province: str = "",
) -> str:
    """One line of ISED's amateur_delim.txt."""
    quals = [q if q in qualifications else "" for q in "ABCDE"]
    fields = [
        call,
        first,
        last,
        "1 RUE PRINCIPALE",
        city,
        province,
        "K1A0B1",
        *quals,
        club,
        "",
        "2 CLUB ST" if club else "",
        club_city,
        club_province,
        "K1A0B2" if club else "",
    ]
    return ";".join(fields)


def ised_zip(path: Path, lines: Iterable[str]) -> Path:
    """Write ISED's download holding these lines, after its column names."""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("amateur_delim.txt", "".join(f"{r}\r\n" for r in [ISED_HEADER, *lines]))
        archive.writestr("readme_amat_delim.txt", "Field layout\r\n")
    return path
