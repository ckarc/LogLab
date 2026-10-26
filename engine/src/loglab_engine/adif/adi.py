# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""ADI reader and writer (ADIF 3.1.7, section IV.A).

The reader is the boundary for untrusted ADI input (Principle XII): it checks
every length, caps every allocation, and raises :class:`AdiParseError` on
anything malformed instead of guessing. The writer emits a canonical form that
the reader turns back into an identical document (Principle IV).

Lengths count bytes. ADI content is limited to printable ASCII plus CR and LF,
where bytes and characters are the same. Out-of-spec UTF-8 text is still read
and written byte-for-byte, with a warning; anything that is not valid UTF-8 is
rejected.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Final, Literal

from loglab_engine.adif.model import (
    AdifDocument,
    AdifModelError,
    Field,
    Header,
    Record,
    validate_field_name,
    validate_type_indicator,
)

_LT: Final = ord("<")
_DIGITS: Final = frozenset("0123456789")


@dataclass(frozen=True, slots=True)
class ParseLimits:
    """Upper bounds the reader enforces before allocating anything (Principle XII)."""

    max_document_bytes: int = 256 * 1024 * 1024
    max_value_bytes: int = 1024 * 1024
    max_tag_bytes: int = 256
    max_fields_per_record: int = 1024
    max_records: int = 10_000_000


DEFAULT_LIMITS: Final = ParseLimits()


class AdiParseError(ValueError):
    """The ADI input is malformed or exceeds a limit; nothing was imported."""

    def __init__(self, message: str, *, offset: int, record: int | None) -> None:
        """Record where in the input the problem was found.

        Args:
            message: What is wrong.
            offset: Byte offset into the input.
            record: 1-based record number, or ``None`` for the header.
        """
        where = "header" if record is None else f"record {record}"
        super().__init__(f"{message} ({where}, byte {offset})")
        self.offset = offset
        self.record = record


@dataclass(frozen=True, slots=True)
class ParseWarning:
    """Something accepted on import that a compliant ADI file would not contain."""

    record: int | None
    field: str | None
    message: str


@dataclass(frozen=True, slots=True)
class ParseResult:
    """A parsed document plus any import-only or out-of-spec findings."""

    document: AdifDocument
    warnings: tuple[ParseWarning, ...]


@dataclass(frozen=True, slots=True)
class _Marker:
    kind: Literal["EOH", "EOR"]
    end: int


class _Reader:
    """Single-pass reader over the input bytes."""

    def __init__(self, data: bytes, limits: ParseLimits) -> None:
        self.data = data
        self.limits = limits
        self.pos = 0
        self.record: int | None = None
        self.warnings: list[ParseWarning] = []

    def fail(self, message: str, offset: int) -> AdiParseError:
        return AdiParseError(message, offset=offset, record=self.record)

    def warn(self, field: str | None, message: str) -> None:
        self.warnings.append(ParseWarning(self.record, field, message))

    def next_tag_start(self) -> int:
        """Offset of the next ``<`` at or after the cursor, or -1 at end of input."""
        return self.data.find(b"<", self.pos)

    def read_tag(self, start: int) -> Field | _Marker:
        """Read the tag at ``start`` (a ``<``) and, for a field, its data."""
        limit = start + 1 + self.limits.max_tag_bytes
        close = self.data.find(b">", start + 1, limit + 1)
        if close == -1:
            msg = f"tag is not closed by '>' within {self.limits.max_tag_bytes} bytes"
            raise self.fail(msg, start)
        raw = self.data[start + 1 : close]
        if b"<" in raw:
            raise self.fail("'<' inside a tag", start)
        try:
            text = raw.decode("ascii")
        except UnicodeDecodeError:
            raise self.fail("non-ASCII byte inside a tag", start) from None
        parts = text.split(":")
        if len(parts) == 1:
            kind = text.upper()
            if kind == "EOH":
                return _Marker("EOH", close + 1)
            if kind == "EOR":
                return _Marker("EOR", close + 1)
            raise self.fail(f"unknown tag <{text}>", start)
        if len(parts) > 3:  # noqa: PLR2004 - name, length, type indicator
            raise self.fail(f"too many ':' in tag <{text}>", start)
        return self.read_field(parts, start, close + 1)

    def read_field(self, parts: list[str], start: int, data_start: int) -> Field:
        name, length_text = parts[0], parts[1]
        indicator = parts[2] if len(parts) == 3 else None  # noqa: PLR2004
        try:
            validate_field_name(name)
            if indicator is not None:
                validate_type_indicator(indicator)
        except AdifModelError as error:
            raise self.fail(str(error), start) from None
        if not length_text or not set(length_text) <= _DIGITS:
            raise self.fail(f"data length {length_text!r} is not an unsigned integer", start)
        if len(length_text) > 1 and length_text.startswith("0"):
            self.warn(name, f"data length {length_text!r} has leading zeros (import-only)")
        length = int(length_text)
        if length > self.limits.max_value_bytes:
            msg = f"{name} data length {length} exceeds {self.limits.max_value_bytes} bytes"
            raise self.fail(msg, start)
        data_end = data_start + length
        if data_end > len(self.data):
            raise self.fail(f"{name} data runs past the end of the input", start)
        try:
            value = self.data[data_start:data_end].decode("utf-8")
        except UnicodeDecodeError:
            msg = f"{name} data is not valid UTF-8 (ADI data must be ASCII)"
            raise self.fail(msg, data_start) from None
        if not value.isascii():
            self.warn(name, "data contains non-ASCII characters, which ADI does not allow")
        self.pos = data_end
        return Field(name, value, indicator)

    def read_header(self) -> Header:
        first = self.next_tag_start()
        if first == -1:
            raise self.fail("header is not terminated by <EOH>", len(self.data))
        try:
            preamble = self.data[:first].decode("utf-8")
        except UnicodeDecodeError:
            raise self.fail("header text is not valid UTF-8", 0) from None
        fields: list[Field] = []
        self.pos = first
        while True:
            start = self.next_tag_start()
            if start == -1:
                raise self.fail("header is not terminated by <EOH>", len(self.data))
            item = self.read_tag(start)
            if isinstance(item, _Marker):
                if item.kind == "EOR":
                    raise self.fail("<EOR> inside the header", start)
                self.pos = item.end
                return Header(preamble, tuple(fields))
            if len(fields) >= self.limits.max_fields_per_record:
                msg = f"header has more than {self.limits.max_fields_per_record} fields"
                raise self.fail(msg, start)
            fields.append(item)

    def read_records(self) -> tuple[Record, ...]:
        records: list[Record] = []
        current: list[Field] = []
        self.record = 1
        while (start := self.next_tag_start()) != -1:
            item = self.read_tag(start)
            if isinstance(item, _Marker):
                if item.kind == "EOH":
                    raise self.fail("<EOH> after the header", start)
                if not current:
                    raise self.fail("<EOR> ends a record with no fields", start)
                if len(records) >= self.limits.max_records:
                    raise self.fail(f"more than {self.limits.max_records} records", start)
                records.append(Record(tuple(current)))
                current = []
                self.record += 1
                self.pos = item.end
                continue
            if len(current) >= self.limits.max_fields_per_record:
                msg = f"record has more than {self.limits.max_fields_per_record} fields"
                raise self.fail(msg, start)
            current.append(item)
        if current:
            raise self.fail("last record is not terminated by <EOR>", len(self.data))
        return tuple(records)


def parse_adi(data: bytes, limits: ParseLimits = DEFAULT_LIMITS) -> ParseResult:
    """Parse ADI bytes into a document, failing closed on malformed input.

    Raises:
        AdiParseError: The input is malformed or exceeds ``limits``.
    """
    reader = _Reader(data, limits)
    if len(data) > limits.max_document_bytes:
        msg = f"input is larger than {limits.max_document_bytes} bytes"
        raise reader.fail(msg, 0)
    header = reader.read_header() if data and data[0] != _LT else None
    records = reader.read_records()
    return ParseResult(AdifDocument(header, records), tuple(reader.warnings))


def _specifier(field: Field) -> bytes:
    value = field.value.encode("utf-8")
    indicator = f":{field.type_indicator}" if field.type_indicator else ""
    return f"<{field.name}:{len(value)}{indicator}>".encode("ascii") + value


def write_adi(document: AdifDocument) -> bytes:
    """Write a document as canonical ADI: one header field per line, one record per line.

    ``parse_adi(write_adi(doc)).document == doc`` for every document.
    """
    out = bytearray()
    if document.header is not None:
        out += document.header.preamble.encode("utf-8")
        for field in document.header.fields:
            out += _specifier(field) + b"\n"
        out += b"<EOH>\n"
    for record in document.records:
        out += b" ".join(_specifier(f) for f in record.fields) + b" <EOR>\n"
    return bytes(out)
