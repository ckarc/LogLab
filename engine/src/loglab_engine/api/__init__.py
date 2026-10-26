# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Engine's HTTP API (Principle I) and the web client it serves."""

from loglab_engine.api.app import create_app

__all__ = ["create_app"]
