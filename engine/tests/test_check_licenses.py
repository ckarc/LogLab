# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Tests for the dependency license gate (scripts/check_licenses.py)."""

from __future__ import annotations

import tomllib
from importlib.metadata import Distribution
from pathlib import Path
from typing import TYPE_CHECKING, override

import pytest

import check_licenses
from check_licenses import ALLOWED, LicenseError, check, is_allowed, license_expression

if TYPE_CHECKING:
    from os import PathLike

DENY_TOML = Path(__file__).resolve().parents[2] / "agent" / "deny.toml"


class FakeDistribution(Distribution):
    """An in-memory distribution with hand-written METADATA."""

    def __init__(self, metadata: str) -> None:
        """Hold the METADATA text this distribution will report."""
        self._metadata = metadata

    @override
    def read_text(self, filename: str) -> str | None:
        return self._metadata if filename == "METADATA" else None

    @override
    def locate_file(self, path: str | PathLike[str]) -> Path:
        return Path(path)


def fake(*headers: str) -> FakeDistribution:
    """Build a fake distribution named ``pkg`` 1.0 with extra metadata headers."""
    return FakeDistribution(
        "\n".join(["Metadata-Version: 2.4", "Name: pkg", "Version: 1.0", *headers])
    )


@pytest.mark.parametrize(
    ("expression", "expected"),
    [
        ("MIT", True),
        ("Apache-2.0 OR BSD-2-Clause", True),
        ("MIT AND Apache-2.0", True),
        ("GPL-2.0-only", False),
        ("GPL-2.0-only OR MIT", True),
        ("GPL-2.0-only AND MIT", False),
        ("(MIT OR GPL-2.0-only) AND Apache-2.0", True),
        ("(GPL-2.0-only OR LicenseRef-Proprietary) AND MIT", False),
        ("Apache-2.0 WITH LLVM-exception", True),
        ("GPL-3.0", True),
        ("LicenseRef-Proprietary", False),
    ],
)
def test_is_allowed_evaluates_spdx_expressions(expression: str, *, expected: bool) -> None:
    """OR needs one allowed side, AND needs both, WITH defers to the license."""
    assert is_allowed(expression) is expected


@pytest.mark.parametrize("expression", ["", "MIT OR", "(MIT", "MIT)", "AND MIT", "MIT WITH"])
def test_is_allowed_rejects_malformed_expressions(expression: str) -> None:
    """Malformed expressions fail closed with an error rather than passing."""
    with pytest.raises(LicenseError):
        is_allowed(expression)


def test_license_expression_prefers_pep639_field() -> None:
    """License-Expression wins over the License field and classifiers."""
    dist = fake(
        "License-Expression: Apache-2.0 OR BSD-2-Clause",
        "License: GPL-2.0-only",
        "Classifier: License :: OSI Approved :: MIT License",
    )
    assert license_expression(dist) == "Apache-2.0 OR BSD-2-Clause"


def test_license_expression_maps_aliased_license_field() -> None:
    """A short, known License field maps to its SPDX identifier."""
    assert license_expression(fake("License: MIT License")) == "MIT"


def test_license_expression_falls_back_to_classifiers() -> None:
    """Recognized OSI classifiers become an OR expression."""
    dist = fake(
        "Classifier: License :: OSI Approved :: MIT License",
        "Classifier: License :: OSI Approved :: Apache Software License 2.0",
    )
    assert license_expression(dist) == "Apache-2.0 OR MIT"


def test_license_expression_rejects_unrecognized_classifier() -> None:
    """An ambiguous classifier such as plain 'BSD License' fails closed."""
    with pytest.raises(LicenseError):
        license_expression(fake("Classifier: License :: OSI Approved :: BSD License"))


def test_check_fails_package_with_no_license_information() -> None:
    """A distribution with no usable license metadata fails the gate."""
    verdict = check(fake())
    assert not verdict.allowed


def test_check_fails_incompatible_license() -> None:
    """GPL-2.0-only cannot be combined with AGPL-3.0 code."""
    verdict = check(fake("License-Expression: GPL-2.0-only"))
    assert not verdict.allowed
    assert verdict.license == "GPL-2.0-only"


def test_allowlist_matches_agent_cargo_deny_policy() -> None:
    """Python and Rust dependencies are held to one license policy."""
    with DENY_TOML.open("rb") as file:
        deny = tomllib.load(file)
    assert set(deny["licenses"]["allow"]) == ALLOWED


def test_main_passes_on_this_environment(capsys: pytest.CaptureFixture[str]) -> None:
    """The Engine's own locked environment passes the gate."""
    assert check_licenses.main() == 0
    assert "FAIL" not in capsys.readouterr().out
