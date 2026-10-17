# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""ADIF support: the field-level document model, ADI reading and writing, and ghosts."""

from loglab_engine.adif.adi import (
    DEFAULT_LIMITS,
    AdiParseError,
    ParseLimits,
    ParseResult,
    ParseWarning,
    parse_adi,
    write_adi,
)
from loglab_engine.adif.compare import differences
from loglab_engine.adif.model import AdifDocument, AdifModelError, Field, Header, Record

__all__ = [
    "DEFAULT_LIMITS",
    "AdiParseError",
    "AdifDocument",
    "AdifModelError",
    "Field",
    "Header",
    "ParseLimits",
    "ParseResult",
    "ParseWarning",
    "Record",
    "differences",
    "parse_adi",
    "write_adi",
]
