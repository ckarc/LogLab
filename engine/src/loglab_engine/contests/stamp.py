# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The fields an operation adds to each new QSO logged in it.

A QSO logged in an operation is made as the operation's station callsign (its
GOTA callsign at the GOTA station, ARRL Field Day Rule 4.1.1.1), names the
operator who made it in ADIF ``OPERATOR``, carries the overlay's
``CONTEST_ID``, records the exchange the station sent, and names the
operation in ``APP_LOGLAB_OPERATION``, so its QSOs can be found together.
Under a program like POTA, a QSO whose other station gave its park
(``SIG_INFO``) is marked as that program's (``SIG``), and for POTA the park is
also copied to ``POTA_REF``: both say only what the operator logged. A field
the QSO already has is never replaced: what the operator typed is kept as
typed.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING

from loglab_engine.adif import Field, Record

# The ADIF application-defined field that names the operation a QSO was logged in.
OPERATION_FIELD = "APP_LOGLAB_OPERATION"

if TYPE_CHECKING:
    from loglab_engine.contests.overlay import Overlay


@dataclass(frozen=True, slots=True)
class Station:
    """Who a QSO is made as and by, and what the station sends."""

    station_call: str
    operator: str
    sent: Mapping[str, str]
    operation: str | None = None


def stamp_fields(overlay: Overlay, station: Station) -> tuple[tuple[str, str], ...]:
    """The ADIF fields, in order, that ``station`` gives each QSO under ``overlay``."""
    fields = [("STATION_CALLSIGN", station.station_call), ("OPERATOR", station.operator)]
    if station.operation is not None:
        fields.append((OPERATION_FIELD, station.operation))
    if overlay.contest_id is not None:
        fields.append(("CONTEST_ID", overlay.contest_id))
    if overlay.exchange is not None:
        for name, template in overlay.exchange.sent_fields.items():
            value = re.sub(
                r"\{([a-z0-9_]+)\}",
                lambda m: station.sent.get(m.group(1), "").strip().upper(),
                template,
            ).strip()
            if value:
                fields.append((name, value))
    return tuple(fields)


def _their_program(record: Record, overlay: Overlay) -> tuple[tuple[str, str], ...]:
    if overlay.program is None:
        return ()
    their = next(
        (f.value.strip().upper() for f in record.fields if f.name.upper() == "SIG_INFO"), ""
    )
    if not their:
        return ()
    fields: list[tuple[str, str]] = [("SIG", overlay.program.sig)]
    if overlay.program.sig == "POTA":
        fields.append(("POTA_REF", their))
    return tuple(fields)


def stamp(record: Record, overlay: Overlay, station: Station) -> Record:
    """``record`` with every field ``station`` gives it that it does not already have."""
    have = {f.name.upper() for f in record.fields}
    given = stamp_fields(overlay, station) + _their_program(record, overlay)
    extra = tuple(Field(name, value) for name, value in given if name not in have)
    return Record(record.fields + extra) if extra else record
