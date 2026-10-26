# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Settings read from environment variables.

Each setting is ``$LABLOG_<NAME>``. Stations set up before the project was
renamed have ``$LOGLAB_<NAME>`` instead, and that still works: the new name
wins when both are set, and an empty value counts as not set.
"""

from __future__ import annotations

import os
from typing import Final

PREFIX: Final = "LABLOG_"
OLD_PREFIX: Final = "LOGLAB_"


def setting(name: str) -> str | None:
    """``$LABLOG_<name>``, else ``$LOGLAB_<name>``, else ``None``."""
    return os.environ.get(PREFIX + name) or os.environ.get(OLD_PREFIX + name) or None
