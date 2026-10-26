# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Signing QSOs for LoTW: the certificate, the QSO's signed text, and the ``.tq8`` file."""

from __future__ import annotations

import gzip
from datetime import date

import pytest

from loglab_engine.adif import Field, Record
from loglab_engine.lotw import (
    CertificateError,
    LocationError,
    NotSignableError,
    StationLocation,
    build_tq8,
    contact_from_record,
    load_certificate,
    tables,
)
from loglab_engine.lotw.certificate import MAX_FILE

from .fake import CERT_PASSWORD, make_p12, read_gabbi, verify


def _record(**fields: str) -> Record:
    base = {"CALL": "W1AW", "QSO_DATE": "20261017", "TIME_ON": "1805", "BAND": "20m", "MODE": "SSB"}
    base.update(fields)
    return Record(tuple(Field(k, v) for k, v in base.items() if v))


KANSAS = StationLocation(
    call="K9KSU", dxcc=291, grid="EM18", ituz=7, cqz=4, us_state="KS", us_county="Ellis"
)


def test_a_certificate_is_read_from_its_p12_file() -> None:
    """The call, entity and QSO dates come from the certificate's own extensions."""
    cert = load_certificate(make_p12(last="2030-12-31"), CERT_PASSWORD)
    assert (cert.call, cert.dxcc) == ("K9KSU", 291)
    assert (cert.qso_first, cert.qso_last) == (date(1990, 1, 1), date(2030, 12, 31))
    assert cert.covers(date(2026, 10, 17))
    assert not cert.covers(date(1989, 12, 31))
    assert not cert.covers(date(2031, 1, 1))
    assert cert.pem_body().endswith("\n")
    assert "-----" not in cert.pem_body()


@pytest.mark.parametrize(
    ("p12", "password", "problem"),
    [
        (make_p12(), "wrong", "password is wrong"),
        (b"not a p12 file", CERT_PASSWORD, "not a .p12 file"),
        (b"x" * (MAX_FILE + 1), CERT_PASSWORD, "larger than"),
        (make_p12(call=""), CERT_PASSWORD, "names no callsign"),
        (make_p12(dxcc="none"), CERT_PASSWORD, "no DXCC entity"),
        (make_p12(first="soon"), CERT_PASSWORD, "not a date"),
    ],
)
def test_a_file_that_is_not_a_usable_certificate_is_refused(
    p12: bytes, password: str, problem: str
) -> None:
    """The file is untrusted input (Principle XII)."""
    with pytest.raises(CertificateError, match=problem):
        load_certificate(p12, password)


def test_adif_modes_and_bands_become_what_lotw_calls_them() -> None:
    """As TrustedQSL maps them: a submode's own entry first, then the mode's."""
    t = tables()
    assert t.lotw_mode("SSB", "USB") == "SSB"
    assert t.lotw_mode("MFSK", "FT4") == "FT4"
    assert t.lotw_mode("ft8", None) == "FT8"
    assert t.lotw_mode("MFSK", "JS8") == "DATA"
    assert t.lotw_mode("NOSUCH", None) is None
    assert t.group("FT8") == "DATA"
    assert t.group("SSB") == "PHONE"
    contact = contact_from_record(
        _record(MODE="MFSK", SUBMODE="FT4", FREQ="14.080", TIME_ON="180559")
    )
    assert (contact.call, contact.band, contact.mode) == ("W1AW", "20M", "FT4")
    assert (contact.day, contact.time, contact.freq) == (date(2026, 10, 17), "18:05:59Z", "14.080")


@pytest.mark.parametrize(
    ("fields", "problem"),
    [
        ({"CALL": ""}, "no valid call"),
        ({"CALL": "W1AW!"}, "no valid call"),
        ({"BAND": "21m"}, "band '21M'"),
        ({"BAND": ""}, "band '\\(none\\)'"),
        ({"MODE": "NOSUCH"}, "mode 'NOSUCH'"),
        ({"QSO_DATE": "20261317"}, "no valid date"),
        ({"QSO_DATE": "2026-10-17"}, "no valid date"),
        ({"TIME_ON": "2405"}, "no valid time"),
        ({"FREQ": "fourteen"}, "FREQ"),
    ],
)
def test_a_qso_lotw_could_not_accept_is_not_signed(fields: dict[str, str], problem: str) -> None:
    """The reason says what is missing."""
    with pytest.raises(NotSignableError, match=problem):
        contact_from_record(_record(**fields))


def test_the_signed_text_follows_the_v2_specification() -> None:
    """Location values in the spec's order, then the QSO's, joined and upper-cased."""
    contact = contact_from_record(_record(FREQ="14.205"))
    assert (
        KANSAS.sign_text() + contact.sign_text()
        == "4EM187EllisKS20MW1AW14.205SSB2026-10-1718:05:00Z"
    )


def test_a_tq8_file_carries_the_certificate_location_and_signed_qsos() -> None:
    """Every signature verifies against the certificate in the file."""
    cert = load_certificate(make_p12(), CERT_PASSWORD)
    contacts = [
        contact_from_record(_record(FREQ="14.205")),
        contact_from_record(_record(CALL="N0OY")),
    ]
    tq8 = build_tq8(cert, KANSAS, contacts, "LabLog Engine test")
    records = read_gabbi(tq8)
    assert records[0]["TQSL_IDENT"].startswith("LabLog Engine test")
    assert records[0]["Rec_Type"] == "tCERT"
    station = records[1]
    assert station == {
        "Rec_Type": "tSTATION",
        "STATION_UID": "1",
        "CERT_UID": "1",
        "CALL": "K9KSU",
        "DXCC": "291",
        "GRIDSQUARE": "EM18",
        "ITUZ": "7",
        "CQZ": "4",
        "US_STATE": "KS",
        "US_COUNTY": "Ellis",
    }
    signed = verify(tq8)
    assert [c["SIGNDATA"] for c in signed] == [
        "4EM187ELLISKS20MW1AW14.205SSB2026-10-1718:05:00Z",
        "4EM187ELLISKS20MN0OYSSB2026-10-1718:05:00Z",
    ]
    assert signed[0]["QSO_DATE"] == "2026-10-17"
    assert signed[0]["QSO_TIME"] == "18:05:00Z"
    assert "FREQ" not in signed[1]
    # The signature is base64 in 64-character lines, as OpenSSL writes it.
    text = gzip.decompress(tq8).decode()
    assert "<SIGN_LOTW_V2.0:" in text
    assert ":6>" in text


def test_a_location_must_be_the_certificate_s_own() -> None:
    """A file never names a different call or entity than the certificate signs for."""
    cert = load_certificate(make_p12(call="W0XYZ"), CERT_PASSWORD)
    with pytest.raises(LocationError, match="certificate is for W0XYZ"):
        build_tq8(cert, KANSAS, [], "test")


@pytest.mark.parametrize(
    ("values", "problem"),
    [
        ({"grid": "EM1"}, "grid square"),
        ({"ituz": 0}, "ITU zone"),
        ({"cqz": 41}, "CQ zone"),
        ({"us_state": None}, "choose the state"),
        ({"us_state": "XX"}, "choose the state"),
        ({"us_state": "HI"}, "choose the state"),  # Hawaii is its own entity
        ({"us_county": "Nowhere"}, "not a county LoTW lists in Kansas"),
        ({"ca_province": "ON"}, "only named for Canadian"),
    ],
)
def test_a_location_lotw_would_not_accept_is_refused(
    values: dict[str, object], problem: str
) -> None:
    """States and counties are checked against LoTW's own lists."""
    fields: dict[str, object] = {
        "call": "K9KSU",
        "dxcc": 291,
        "grid": "EM18",
        "ituz": 7,
        "cqz": 4,
        "us_state": "KS",
        "us_county": "Ellis",
    }
    fields.update(values)
    with pytest.raises(LocationError, match=problem):
        StationLocation(**fields)  # type: ignore[arg-type]


def test_a_canadian_location_names_a_province() -> None:
    """And no state or county."""
    StationLocation(call="VE3XX", dxcc=1, grid="FN03", ituz=4, cqz=4, ca_province="ON")
    with pytest.raises(LocationError, match="province"):
        StationLocation(call="VE3XX", dxcc=1, grid="FN03", ituz=4, cqz=4)
