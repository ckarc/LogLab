# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Signed LoTW upload files (``.tq8``), built as TrustedQSL's published source builds them.

A ``.tq8`` file is gzip-compressed text in ADIF-style ``<NAME:length>value``
fields, in records LoTW calls GABBI records:

* ``TQSL_IDENT``: which program wrote the file;
* ``tCERT``: the callsign certificate, without its PEM armour lines;
* ``tSTATION``: the station location (call, DXCC entity, grid, zones, and a
  state and county or province);
* one ``tCONTACT`` per QSO, with its signature.

Each QSO is signed under the ``LOTW`` V2.0 signature specification: the
station location's values in a fixed order, then the QSO's, each trimmed,
joined with nothing between them, and the whole upper-cased. That text is sent
as ``SIGNDATA``, and its RSA SHA-1 signature, in base64, as ``SIGN_LOTW_V2.0``.

Nothing here touches the log, the database or the network.
"""

from __future__ import annotations

import base64
import gzip
import re
from dataclasses import dataclass
from datetime import date
from typing import TYPE_CHECKING, Final

from loglab_engine.lotw.tables import CANADA_DXCC, US_DXCC, tables

if TYPE_CHECKING:
    from collections.abc import Iterable

    from loglab_engine.adif import Record
    from loglab_engine.lotw.certificate import CallsignCertificate

SIGSPEC: Final = "SIGN_LOTW_V2.0"
_CALL: Final = re.compile(r"^[A-Z0-9]+(/[A-Z0-9]+)*$")
_GRID: Final = re.compile(r"^[A-R]{2}[0-9]{2}([A-X]{2})?$")
_DATE: Final = re.compile(r"^[0-9]{8}$")
_TIME: Final = re.compile(r"^([01][0-9]|2[0-3])[0-5][0-9]([0-5][0-9])?$")
_FREQ: Final = re.compile(r"^[0-9]{1,6}(\.[0-9]{1,6})?$")
_TEXT: Final = re.compile(r"^[A-Z0-9 ./_-]{1,32}$")
# The station fields of the LOTW V2.0 signature specification, in its order.
_STATION_SIGN_ORDER: Final = (
    "AU_STATE",
    "CA_PROVINCE",
    "CA_US_PARK",
    "CN_PROVINCE",
    "CQZ",
    "DX_US_PARK",
    "FI_KUNTA",
    "GRIDSQUARE",
    "IOTA",
    "ITUZ",
    "JA_CITY_GUN_KU",
    "JA_PREFECTURE",
    "RU_OBLAST",
    "US_COUNTY",
    "US_PARK",
    "US_STATE",
)


class NotSignableError(ValueError):
    """A QSO lacks something LoTW needs, so it cannot be signed; the message says what."""


class LocationError(ValueError):
    """A station location is incomplete or names something LoTW does not know."""


@dataclass(frozen=True, slots=True)
class StationLocation:
    """Where the station was: what TrustedQSL calls a station location."""

    call: str
    dxcc: int
    grid: str
    ituz: int
    cqz: int
    us_state: str | None = None
    us_county: str | None = None
    ca_province: str | None = None

    def __post_init__(self) -> None:
        """Refuse a location LoTW would not accept.

        Raises:
            LocationError: Something is missing, or is not on LoTW's lists.
        """
        if not _GRID.match(self.grid):
            raise LocationError(f"{self.grid!r} is not a 4- or 6-character grid square")
        if not 1 <= self.ituz <= 90:  # noqa: PLR2004 - ITU zones
            raise LocationError("the ITU zone is 1 to 90")
        if not 1 <= self.cqz <= 40:  # noqa: PLR2004 - CQ zones
            raise LocationError("the CQ zone is 1 to 40")
        t = tables()
        if self.dxcc in US_DXCC:
            state = t.us_states.get(self.us_state or "")
            if state is None or state.dxcc != self.dxcc:
                raise LocationError("choose the state the station is in")
            if self.us_county is not None and self.us_county not in state.counties:
                raise LocationError(
                    f"{self.us_county!r} is not a county LoTW lists in {state.name}"
                )
        elif self.us_state is not None or self.us_county is not None:
            raise LocationError("a state and county are only named for US stations")
        if self.dxcc == CANADA_DXCC:
            if self.ca_province not in t.ca_provinces:
                raise LocationError("choose the province the station is in")
        elif self.ca_province is not None:
            raise LocationError("a province is only named for Canadian stations")

    def gabbi_fields(self) -> list[tuple[str, str]]:
        """The ``tSTATION`` fields, in TrustedQSL's station-location page order."""
        fields = [
            ("CALL", self.call),
            ("DXCC", str(self.dxcc)),
            ("GRIDSQUARE", self.grid),
            ("ITUZ", str(self.ituz)),
            ("CQZ", str(self.cqz)),
        ]
        if self.us_state is not None:
            fields.append(("US_STATE", self.us_state))
        if self.us_county is not None:
            fields.append(("US_COUNTY", self.us_county))
        if self.ca_province is not None:
            fields.append(("CA_PROVINCE", self.ca_province))
        return fields

    def sign_text(self) -> str:
        """The location's part of the signed text, in the V2.0 specification's order."""
        values = {
            "CA_PROVINCE": self.ca_province,
            "CQZ": str(self.cqz),
            "GRIDSQUARE": self.grid,
            "ITUZ": str(self.ituz),
            "US_COUNTY": self.us_county,
            "US_STATE": self.us_state,
        }
        return "".join((values.get(name) or "").strip() for name in _STATION_SIGN_ORDER)


@dataclass(frozen=True, slots=True)
class Contact:
    """One QSO as LoTW signs it."""

    call: str
    band: str
    mode: str
    day: date
    time: str  # HH:MM:SSZ
    freq: str | None = None
    freq_rx: str | None = None
    band_rx: str | None = None
    prop_mode: str | None = None
    sat_name: str | None = None

    def sign_text(self) -> str:
        """The QSO's part of the signed text, in the V2.0 specification's order."""
        values = (
            self.band,
            self.band_rx,
            self.call,
            self.freq,
            self.freq_rx,
            self.mode,
            self.prop_mode,
            self.day.isoformat(),
            self.time,
            self.sat_name,
        )
        return "".join((v or "").strip() for v in values)


def _optional(record: Record, name: str, pattern: re.Pattern[str]) -> str | None:
    value = (record.get(name) or "").strip().upper()
    if not value:
        return None
    if not pattern.match(value):
        raise NotSignableError(f"its {name} {value!r} is not something LoTW accepts")
    return value


def contact_from_record(record: Record) -> Contact:
    """The QSO LoTW would sign for an ADIF record.

    Raises:
        NotSignableError: The record lacks a call, band, mode, date or time
            LoTW accepts.
    """
    t = tables()
    call = (record.get("CALL") or "").strip().upper()
    if not _CALL.match(call):
        raise NotSignableError("it has no valid call")
    band = (record.get("BAND") or "").strip().upper()
    if band not in t.bands:
        raise NotSignableError(f"LoTW does not know the band {band or '(none)'!r}")
    mode = t.lotw_mode(record.get("MODE") or "", record.get("SUBMODE"))
    if mode is None:
        raise NotSignableError(f"LoTW does not know the mode {record.get('MODE') or '(none)'!r}")
    when = (record.get("QSO_DATE") or "").strip()
    try:
        day = date(int(when[:4]), int(when[4:6]), int(when[6:])) if _DATE.match(when) else None
    except ValueError:
        day = None
    if day is None:
        raise NotSignableError("it has no valid date")
    clock = (record.get("TIME_ON") or "").strip()
    if not _TIME.match(clock):
        raise NotSignableError("it has no valid time")
    seconds = clock[4:6] or "00"
    band_rx = _optional(record, "BAND_RX", _TEXT)
    if band_rx is not None and band_rx not in t.bands:
        raise NotSignableError(f"LoTW does not know the receive band {band_rx!r}")
    return Contact(
        call=call,
        band=band,
        mode=mode,
        day=day,
        time=f"{clock[:2]}:{clock[2:4]}:{seconds}Z",
        freq=_optional(record, "FREQ", _FREQ),
        freq_rx=_optional(record, "FREQ_RX", _FREQ),
        band_rx=band_rx,
        prop_mode=_optional(record, "PROP_MODE", _TEXT),
        sat_name=_optional(record, "SAT_NAME", _TEXT),
    )


def _field(name: str, value: str, type_indicator: str | None = None) -> str:
    length = len(value.encode())
    spec = f"{name}:{length}" if type_indicator is None else f"{name}:{length}:{type_indicator}"
    return f"<{spec}>{value}"


def _base64_lines(data: bytes) -> str:
    """Base64 in 64-character lines, each ending in a newline, as OpenSSL writes it."""
    text = base64.b64encode(data).decode("ascii")
    return "".join(text[i : i + 64] + "\n" for i in range(0, len(text), 64))


def signed_contact(cert: CallsignCertificate, location: StationLocation, contact: Contact) -> str:
    """One ``tCONTACT`` record, signed with ``cert``."""
    sign_data = (location.sign_text() + contact.sign_text()).upper()
    signature = _base64_lines(cert.sign(sign_data.encode()))
    lines = [
        "<Rec_Type:8>tCONTACT",
        _field("STATION_UID", "1"),
        _field("CALL", contact.call),
        _field("BAND", contact.band),
        _field("MODE", contact.mode),
    ]
    for name, value in (
        ("FREQ", contact.freq),
        ("FREQ_RX", contact.freq_rx),
        ("PROP_MODE", contact.prop_mode),
        ("SAT_NAME", contact.sat_name),
        ("BAND_RX", contact.band_rx),
    ):
        if value:
            lines.append(_field(name, value))
    lines += [
        _field("QSO_DATE", contact.day.isoformat()),
        _field("QSO_TIME", contact.time),
        # The signature ends in a newline, so SIGNDATA follows it directly.
        _field(SIGSPEC, signature, "6") + _field("SIGNDATA", sign_data),
        "<eor>",
    ]
    return "\n".join(lines) + "\n"


def build_tq8(
    cert: CallsignCertificate,
    location: StationLocation,
    contacts: Iterable[Contact],
    writer: str,
) -> bytes:
    """A complete, gzip-compressed ``.tq8`` file signing ``contacts``.

    Raises:
        LocationError: The location's call or DXCC entity is not the certificate's.
    """
    if location.call != cert.call or location.dxcc != cert.dxcc:
        raise LocationError(
            f"the station location is for {location.call}, entity {location.dxcc}; "
            f"the certificate is for {cert.call}, entity {cert.dxcc}"
        )
    ident = f"{writer} Signing: LOTW V2.0 AllowDupes: false Source: {writer}"
    parts = [
        _field("TQSL_IDENT", ident) + "\n",
        "<Rec_Type:5>tCERT\n" + _field("CERT_UID", "1") + "\n",
        _field("CERTIFICATE", cert.pem_body()) + "<eor>\n",
        "<Rec_Type:8>tSTATION\n"
        + _field("STATION_UID", "1")
        + "\n"
        + _field("CERT_UID", "1")
        + "\n"
        + "".join(_field(n, v) + "\n" for n, v in location.gabbi_fields())
        + "<eor>\n",
    ]
    parts += [signed_contact(cert, location, c) for c in contacts]
    return gzip.compress("".join(parts).encode(), compresslevel=9, mtime=0)
