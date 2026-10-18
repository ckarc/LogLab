# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operation overlays (contest templates) and the contest checks they drive (Principle III)."""

from loglab_engine.contests.dupes import (
    Check,
    Earlier,
    Problem,
    Status,
    Worked,
    check,
    classify,
    exchange_problems,
    sent_problems,
)
from loglab_engine.contests.overlay import (
    EVERYDAY,
    Overlay,
    OverlayError,
    overlay,
    overlays,
    parse,
)

__all__ = [
    "EVERYDAY",
    "Check",
    "Earlier",
    "Overlay",
    "OverlayError",
    "Problem",
    "Status",
    "Worked",
    "check",
    "classify",
    "exchange_problems",
    "overlay",
    "overlays",
    "parse",
    "sent_problems",
]
