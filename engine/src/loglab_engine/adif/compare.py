# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Field-by-field comparison of two ADIF documents.

Used to prove a round trip is lossless: every field present, in order, with the
same name, data, and type indicator (Principle IV).
"""

from __future__ import annotations

from itertools import zip_longest
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from loglab_engine.adif.model import AdifDocument, Field


def _describe(field: Field | None) -> str:
    if field is None:
        return "(missing)"
    indicator = f":{field.type_indicator}" if field.type_indicator else ""
    return f"<{field.name}:{len(field.value)}{indicator}>{field.value!r}"


def _compare_fields(
    where: str,
    left: tuple[Field, ...],
    right: tuple[Field, ...],
    out: list[str],
) -> None:
    for index, (a, b) in enumerate(zip_longest(left, right), start=1):
        if a != b:
            out.append(f"{where}, field {index}: {_describe(a)} != {_describe(b)}")


def differences(original: AdifDocument, other: AdifDocument) -> list[str]:
    """List every difference between two documents; empty means identical."""
    out: list[str] = []
    left_header, right_header = original.header, other.header
    if (left_header is None) != (right_header is None):
        out.append("header: present in one document only")
    elif left_header is not None and right_header is not None:
        if left_header.preamble != right_header.preamble:
            out.append("header: text differs")
        _compare_fields("header", left_header.fields, right_header.fields, out)
    if len(original.records) != len(other.records):
        out.append(f"record count: {len(original.records)} != {len(other.records)}")
    for number, (a, b) in enumerate(zip(original.records, other.records, strict=False), start=1):
        _compare_fields(f"record {number}", a.fields, b.fields, out)
    return out
