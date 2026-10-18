# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The fields an operation adds to each new QSO logged in it.

A QSO logged in an operation is made as the operation's station callsign (its
GOTA callsign at the GOTA station, ARRL Field Day Rule 4.1.1.1), names the
operator who made it in ADIF ``OPERATOR``, carries the overlay's
``CONTEST_ID``, and records the exchange the station sent. A field the QSO
already has is never replaced: what the operator typed is kept as typed.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING

from loglab_engine.adif import Field, Record

if TYPE_CHECKING:
    from loglab_engine.contests.overlay import Overlay


@dataclass(frozen=True, slots=True)
class Station:
    """Who a QSO is made as and by, and what the station sends."""

    station_call: str
    operator: str
    sent: Mapping[str, str]


def stamp_fields(overlay: Overlay, station: Station) -> tuple[tuple[str, str], ...]:
    """The ADIF fields, in order, that ``station`` gives each QSO under ``overlay``."""
    fields = [("STATION_CALLSIGN", station.station_call), ("OPERATOR", station.operator)]
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


def stamp(record: Record, overlay: Overlay, station: Station) -> Record:
    """``record`` with every field ``station`` gives it that it does not already have."""
    have = {f.name.upper() for f in record.fields}
    extra = tuple(
        Field(name, value) for name, value in stamp_fields(overlay, station) if name not in have
    )
    return Record(record.fields + extra) if extra else record
