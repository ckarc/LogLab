# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Placeholder test proving the Engine package imports and the test gate runs."""

from importlib.metadata import version

import loglab_engine


def test_package_version_matches_installed_metadata() -> None:
    """The package's version string agrees with its installed distribution metadata."""
    assert loglab_engine.__version__ == version("loglab-engine")
