# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Typed, immutable model of an ADIF document at the field level.

This model holds exactly what an ADI file says: every field, in order, with its
name as written, its data, and its optional data type indicator. It does not
interpret fields, so fields the Engine does not use survive unchanged
(Principle IV). Interpreting fields into QSOs is a separate, later layer.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Final

# ADIF 3.1.7 IV.A.4-5: names may not contain these characters.
_FORBIDDEN_NAME_CHARS: Final = frozenset(",:<>{}")
_RESERVED_TAGS: Final = frozenset({"EOH", "EOR"})


class AdifModelError(ValueError):
    """A value cannot be represented in an ADIF document."""


def validate_field_name(name: str) -> None:
    """Check that ``name`` can appear as an ADI field name.

    Field names are printable ASCII without ``, : < > { }``, do not begin or end
    with a space, and are not the reserved ``EOH``/``EOR`` tags.

    Raises:
        AdifModelError: The name is not a valid ADI field name.
    """
    if not name:
        msg = "field name is empty"
        raise AdifModelError(msg)
    if name != name.strip(" "):
        msg = f"field name {name!r} begins or ends with a space"
        raise AdifModelError(msg)
    for char in name:
        if not " " <= char <= "~" or char in _FORBIDDEN_NAME_CHARS:
            msg = f"field name {name!r} contains forbidden character {char!r}"
            raise AdifModelError(msg)
    if name.upper() in _RESERVED_TAGS:
        msg = f"{name!r} is a reserved tag, not a field name"
        raise AdifModelError(msg)


def validate_type_indicator(indicator: str) -> None:
    """Check that ``indicator`` is a single ASCII letter.

    Indicators are preserved as written, including ones newer ADIF versions may
    add, so only their shape is checked here.

    Raises:
        AdifModelError: The indicator is not a single ASCII letter.
    """
    if len(indicator) != 1 or not ("A" <= indicator <= "Z" or "a" <= indicator <= "z"):
        msg = f"data type indicator {indicator!r} is not a single ASCII letter"
        raise AdifModelError(msg)


@dataclass(frozen=True, slots=True)
class Field:
    """One ADIF data specifier: ``<name:length:type>value``."""

    name: str
    value: str
    type_indicator: str | None = None

    def __post_init__(self) -> None:
        """Validate the name and indicator so an invalid field cannot exist."""
        validate_field_name(self.name)
        if self.type_indicator is not None:
            validate_type_indicator(self.type_indicator)

    @property
    def key(self) -> str:
        """The field name normalized for lookup (ADIF names are case-insensitive)."""
        return self.name.upper()

    @property
    def is_ascii(self) -> bool:
        """Whether the value stays within the ASCII that ADI files are limited to."""
        return self.value.isascii()


@dataclass(frozen=True, slots=True)
class Record:
    """One QSO record: its fields in file order, duplicates included."""

    fields: tuple[Field, ...]

    def __post_init__(self) -> None:
        """A record carries at least one field (ADIF 3.1.7 IV.A.6)."""
        if not self.fields:
            msg = "a record must contain at least one field"
            raise AdifModelError(msg)

    def get(self, name: str) -> str | None:
        """Return the value of the first field called ``name``, ignoring case."""
        key = name.upper()
        return next((f.value for f in self.fields if f.key == key), None)


@dataclass(frozen=True, slots=True)
class Header:
    """An ADI header: free text before its first field, then header fields."""

    preamble: str
    fields: tuple[Field, ...] = ()

    def __post_init__(self) -> None:
        """A header's text must be non-empty and free of ``<`` (ADIF 3.1.7 IV.A.3)."""
        if not self.preamble:
            msg = "a header must begin with at least one character of text"
            raise AdifModelError(msg)
        if "<" in self.preamble:
            msg = "header text cannot contain '<'"
            raise AdifModelError(msg)

    def get(self, name: str) -> str | None:
        """Return the value of the first header field called ``name``, ignoring case."""
        key = name.upper()
        return next((f.value for f in self.fields if f.key == key), None)


@dataclass(frozen=True, slots=True)
class AdifDocument:
    """A whole ADIF document: an optional header and its records."""

    header: Header | None
    records: tuple[Record, ...]

    @property
    def field_count(self) -> int:
        """Total number of fields across the header and all records."""
        header_fields = len(self.header.fields) if self.header else 0
        return header_fields + sum(len(r.fields) for r in self.records)
