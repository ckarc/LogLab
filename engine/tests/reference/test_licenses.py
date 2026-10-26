# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading the FCC and ISED license downloads: untrusted input, read with care (XII)."""

from __future__ import annotations

import zipfile
from datetime import date
from typing import TYPE_CHECKING

import pytest

from loglab_engine.reference.licenses import (
    MAX_LINE,
    LicenseFileError,
    Tally,
    fcc_classes,
    fcc_entities,
    fcc_headers,
    ised_licenses,
    tidy_case,
)

from .uls import am, en, fcc_zip, hd, ised, ised_zip

if TYPE_CHECKING:
    from pathlib import Path

TODAY = date(2026, 10, 19)


def test_a_license_is_kept_while_active_or_within_the_renewal_grace_period(
    tmp_path: Path,
) -> None:
    """Cancelled licenses, and ones expired more than two years, are left out."""
    path = fcc_zip(
        tmp_path / "l_amat.zip",
        [
            hd("1", "W1AW"),
            hd("2", "K1ABC", "E", "05/01/2025"),
            hd("3", "K1OLD", "E", "05/01/2024"),
            hd("4", "K1CAN", "C"),
            hd("5", "k1low", "A"),
        ],
    )
    tally = Tally()
    with zipfile.ZipFile(path) as archive:
        found = {h.call: h for h in fcc_headers(archive, TODAY, tally)}
    assert found["W1AW"].kept
    assert found["W1AW"].active
    assert found["W1AW"].expires == date(2034, 3, 1)
    assert found["K1ABC"].kept
    assert not found["K1ABC"].active
    assert not found["K1OLD"].kept
    assert not found["K1CAN"].kept
    assert found["K1LOW"].kept
    assert tally.read == 5
    assert tally.skipped == 0


def test_names_are_written_as_people_write_them_and_no_street_address_is_read(
    tmp_path: Path,
) -> None:
    """The FCC files names in capitals; a club's name is kept as filed."""
    path = fcc_zip(
        tmp_path / "l_amat.zip",
        [],
        [
            en("1", "K9KSU", "BRIAN", "KRENZIN", middle="J", city="SAINT LOUIS", state="mo"),
            en("2", "W9AB", "JOHN", "SMITH", suffix="III"),
            en("3", "W1AW", entity="ARRL HQ OPERATORS CLUB"),
            en("4", "W1XY", "Ann", "McDonald"),
        ],
    )
    with zipfile.ZipFile(path) as archive:
        found = {e.usi: e for e in fcc_entities(archive, Tally())}
    assert found["1"].name == "Brian J Krenzin"
    assert found["1"].city == "Saint Louis"
    assert found["1"].state == "MO"
    assert found["2"].name == "John Smith III"
    assert found["3"].name == "ARRL HQ OPERATORS CLUB"
    assert found["4"].name == "Ann McDonald"
    assert "225 MAIN ST" not in repr(found)


def test_a_record_broken_over_two_lines_is_joined_back_together(tmp_path: Path) -> None:
    """Some licensee records carry a line break inside a field."""
    broken = en("1", "K9KSU", "BRIAN", "KRENZIN").replace("225 MAIN ST", "225 MAIN\r\nST")
    path = fcc_zip(tmp_path / "l_amat.zip", [], [broken, en("2", "W9AB", "JOHN", "SMITH")])
    tally = Tally()
    with zipfile.ZipFile(path) as archive:
        found = list(fcc_entities(archive, tally))
    assert [(e.usi, e.city) for e in found] == [("1", "Newington"), ("2", "Newington")]
    assert tally.skipped == 0


def test_unreadable_records_are_skipped_and_counted(tmp_path: Path) -> None:
    """An overlong line, a short record, a bad identifier or call: each is counted."""
    path = fcc_zip(
        tmp_path / "l_amat.zip",
        [
            "stray text before any record",
            hd("1", "W1AW"),
            "HD|" + "x" * (MAX_LINE + 10),
            "HD|2|only|three",
            hd("ABC", "K1ABC"),
            hd("3", "NOT A CALL"),
            hd("4", "K1XYZ"),
        ],
    )
    tally = Tally()
    with zipfile.ZipFile(path) as archive:
        kept = [h.call for h in fcc_headers(archive, TODAY, tally)]
    assert kept == ["W1AW", "K1XYZ"]
    assert tally.skipped == 5
    assert len(tally.examples) == 3
    assert "HD|2|only|three" in tally.examples[2]


def test_operator_classes_are_named(tmp_path: Path) -> None:
    """A club station has no class."""
    path = fcc_zip(tmp_path / "l_amat.zip", [], [], [am("1", "K9KSU", "E"), am("2", "W1AW", "")])
    with zipfile.ZipFile(path) as archive:
        found = {a.usi: a.license_class for a in fcc_classes(archive, Tally())}
    assert found == {"1": "Extra", "2": None}


def test_a_download_without_the_fcc_files_is_refused(tmp_path: Path) -> None:
    """The wrong file is refused before anything is read."""
    path = ised_zip(tmp_path / "l_amat.zip", [])
    with zipfile.ZipFile(path) as archive, pytest.raises(LicenseFileError, match=r"HD\.dat"):
        list(fcc_headers(archive, TODAY, Tally()))


def test_canadian_calls_are_read_with_their_qualifications(tmp_path: Path) -> None:
    """People and clubs; the street address and postal code are never read."""
    path = ised_zip(
        tmp_path / "amateur_delim.zip",
        [
            ised("VE3ABC", "JEAN", "TREMBLAY", "OTTAWA", province="ON", qualifications="ABD"),
            ised("VA7XYZ", "Mary", "Smith", "Victoria", province="bc", qualifications="AE"),
            ised(
                "VE1AA", club="HALIFAX AMATEUR RADIO CLUB", club_city="HALIFAX", club_province="NS"
            ),
            ised("VE9NONE", "NO", "QUALS", qualifications=""),
            "not;enough;fields",
        ],
    )
    tally = Tally()
    with zipfile.ZipFile(path) as archive:
        found = {c.call: c for c in ised_licenses(archive, tally)}
    assert found["VE3ABC"].name == "Jean Tremblay"
    assert found["VE3ABC"].city == "Ottawa"
    assert found["VE3ABC"].province == "ON"
    assert found["VE3ABC"].license_class == "Advanced, Morse"
    assert found["VA7XYZ"].license_class == "Basic with Honours"
    assert found["VA7XYZ"].province == "BC"
    assert found["VE1AA"].name == "HALIFAX AMATEUR RADIO CLUB"
    assert found["VE1AA"].city == "Halifax"
    assert found["VE9NONE"].license_class is None
    assert tally.read == 5
    assert tally.skipped == 1
    assert "PRINCIPALE" not in repr(found)
    assert "K1A0B1" not in repr(found)


@pytest.mark.parametrize(
    ("written", "shown"),
    [
        ("JOHN O'BRIEN", "John O'Brien"),
        ("Ann McDonald", "Ann McDonald"),
        ("HENRY VIII", "Henry VIII"),
    ],
)
def test_names_in_capitals_are_written_in_title_case(written: str, shown: str) -> None:
    """Mixed case is left as it is."""
    assert tidy_case(written) == shown


def test_accented_names_are_read_as_the_utf8_the_agencies_write(tmp_path: Path) -> None:
    """Both agencies write UTF-8; an odd Latin-1 line is still read."""
    path = fcc_zip(
        tmp_path / "l_amat.zip", [], [en("1", "VE2XX", "José", "Pérez", city="Montréal")]
    )
    with zipfile.ZipFile(path) as archive:
        (entity,) = fcc_entities(archive, Tally())
    assert (entity.name, entity.city) == ("José Pérez", "Montréal")
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("HD.dat", b"")
        archive.writestr("EN.dat", en("2", "W1XY", "Ren\xe9", "Roy").encode("latin-1") + b"\r\n")
    with zipfile.ZipFile(path) as archive:
        (entity,) = fcc_entities(archive, Tally())
    assert entity.name == "René Roy"
