# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Online callbooks: an optional enhancement that fills in who and where a call is."""

from loglab_engine.callbook.qrz import (
    CallbookEntry,
    QrzClient,
    QrzError,
    QrzLoginError,
    QrzReplyError,
    QrzSessionError,
    QrzUnreachableError,
    http_fetch,
    parse_reply,
)
from loglab_engine.callbook.service import Callbook, Link, LoginCheck, LookupResult, LookupStatus

__all__ = [
    "Callbook",
    "CallbookEntry",
    "Link",
    "LoginCheck",
    "LookupResult",
    "LookupStatus",
    "QrzClient",
    "QrzError",
    "QrzLoginError",
    "QrzReplyError",
    "QrzSessionError",
    "QrzUnreachableError",
    "http_fetch",
    "parse_reply",
]
