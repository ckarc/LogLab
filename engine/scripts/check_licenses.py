# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""License gate for the Engine's Python dependencies.

Run inside the Engine's environment (``uv run python scripts/check_licenses.py``).
Every installed distribution other than the Engine itself must carry a license
compatible with the AGPL-3.0 (see CONSTITUTION.md, Technology Constraints,
Licensing). A distribution whose license cannot be determined fails the gate:
a human reviews it and, if appropriate, adds an alias below.

The allowlist here must stay identical to ``[licenses].allow`` in
``agent/deny.toml``; a test enforces that.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from importlib.metadata import Distribution, distributions
from typing import Final

# SPDX identifiers compatible with distribution under the AGPL-3.0.
ALLOWED: Final[frozenset[str]] = frozenset(
    {
        "0BSD",
        "AGPL-3.0-only",
        "AGPL-3.0-or-later",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "BSL-1.0",
        "CC0-1.0",
        "GPL-2.0-or-later",
        "GPL-3.0-only",
        "GPL-3.0-or-later",
        "ISC",
        "LGPL-2.1-or-later",
        "LGPL-3.0-only",
        "LGPL-3.0-or-later",
        "MIT",
        "MIT-0",
        "MPL-2.0",
        "PSF-2.0",
        "Unicode-3.0",
        "Unlicense",
        "Zlib",
    },
)

# Non-SPDX license strings and trove classifiers, mapped to SPDX identifiers.
# Only unambiguous names belong here ("BSD License" does not: which BSD?).
ALIASES: Final[dict[str, str]] = {
    "MIT License": "MIT",
    "Apache 2.0": "Apache-2.0",
    "Apache License 2.0": "Apache-2.0",
    "Apache Software License 2.0": "Apache-2.0",
    "Mozilla Public License 2.0 (MPL 2.0)": "MPL-2.0",
    "Python Software Foundation License": "PSF-2.0",
    "ISC License (ISCL)": "ISC",
    "The Unlicense (Unlicense)": "Unlicense",
    "GPL-3.0": "GPL-3.0-only",
    "GPL-3.0+": "GPL-3.0-or-later",
    "LGPL-3.0": "LGPL-3.0-only",
    "LGPL-3.0+": "LGPL-3.0-or-later",
    "AGPL-3.0": "AGPL-3.0-only",
    "AGPL-3.0+": "AGPL-3.0-or-later",
}

CLASSIFIER_PREFIX: Final = "License :: OSI Approved :: "
SELF: Final = "loglab-engine"
_TOKEN: Final = re.compile(r"\(|\)|[^\s()]+")


class LicenseError(ValueError):
    """A license string could not be parsed or mapped to SPDX identifiers."""


@dataclass(frozen=True)
class Verdict:
    """The license-gate result for one distribution."""

    name: str
    version: str
    license: str
    allowed: bool
    reason: str


def normalize_name(name: str) -> str:
    """Normalize a distribution name per PEP 503."""
    return re.sub(r"[-_.]+", "-", name).lower()


def is_allowed(expression: str) -> bool:
    """Evaluate an SPDX license expression against the allowlist.

    ``A OR B`` is allowed if either side is; ``A AND B`` only if both are.
    ``X WITH exception`` is allowed if ``X`` is, since exceptions only add
    permissions.

    Raises:
        LicenseError: The expression is malformed.
    """
    tokens = _TOKEN.findall(expression)
    if not tokens:
        msg = "empty license expression"
        raise LicenseError(msg)
    parser = _Parser(tokens)
    result = parser.parse_or()
    if parser.position != len(tokens):
        msg = f"unexpected token {tokens[parser.position]!r} in {expression!r}"
        raise LicenseError(msg)
    return result


class _Parser:
    """Recursive-descent parser for SPDX license expressions."""

    def __init__(self, tokens: list[str]) -> None:
        self.tokens = tokens
        self.position = 0

    def _peek(self) -> str | None:
        return self.tokens[self.position] if self.position < len(self.tokens) else None

    def _take(self) -> str:
        word = self._peek()
        if word is None:
            msg = "license expression ended early"
            raise LicenseError(msg)
        self.position += 1
        return word

    def parse_or(self) -> bool:
        result = self.parse_and()
        while self._peek() == "OR":
            self.position += 1
            right = self.parse_and()
            result = result or right
        return result

    def parse_and(self) -> bool:
        result = self.parse_atom()
        while self._peek() == "AND":
            self.position += 1
            right = self.parse_atom()
            result = result and right
        return result

    def parse_atom(self) -> bool:
        word = self._take()
        if word == "(":
            result = self.parse_or()
            if self._take() != ")":
                msg = "unbalanced parentheses in license expression"
                raise LicenseError(msg)
            return result
        if word in {")", "AND", "OR", "WITH"}:
            msg = f"unexpected {word!r} in license expression"
            raise LicenseError(msg)
        if self._peek() == "WITH":
            self.position += 1
            self._take()  # The exception identifier only adds permissions.
        return ALIASES.get(word, word) in ALLOWED


def license_expression(dist: Distribution) -> str:
    """Determine a distribution's license as an SPDX expression.

    Prefers the PEP 639 ``License-Expression`` field, then a short ``License``
    field, then OSI trove classifiers (several classifiers mean a choice).

    Raises:
        LicenseError: No license information could be mapped to SPDX.
    """
    meta = dist.metadata
    expression = meta.get("License-Expression")
    if expression:
        return expression.strip()
    field = (meta.get("License") or "").strip()
    if field and "\n" not in field:
        return ALIASES.get(field, field)
    classifiers = [
        c.removeprefix(CLASSIFIER_PREFIX)
        for c in meta.get_all("Classifier") or []
        if c.startswith(CLASSIFIER_PREFIX)
    ]
    mapped = [ALIASES[c] for c in classifiers if c in ALIASES]
    if mapped and len(mapped) == len(classifiers):
        return " OR ".join(sorted(set(mapped)))
    msg = "no SPDX expression, short License field, or recognized classifier"
    raise LicenseError(msg)


def check(dist: Distribution) -> Verdict:
    """Apply the license gate to one installed distribution."""
    name = dist.metadata["Name"]
    try:
        expression = license_expression(dist)
        allowed = is_allowed(expression)
    except LicenseError as error:
        return Verdict(name, dist.version, "?", allowed=False, reason=str(error))
    reason = "allowed" if allowed else "not compatible with AGPL-3.0"
    return Verdict(name, dist.version, expression, allowed=allowed, reason=reason)


def main() -> int:
    """Check every installed distribution; return a process exit code."""
    verdicts = sorted(
        (check(d) for d in distributions() if normalize_name(d.metadata["Name"]) != SELF),
        key=lambda v: normalize_name(v.name),
    )
    for v in verdicts:
        mark = "ok  " if v.allowed else "FAIL"
        sys.stdout.write(f"{mark} {v.name} {v.version}: {v.license} ({v.reason})\n")
    failures = [v for v in verdicts if not v.allowed]
    if failures:
        sys.stdout.write(f"\n{len(failures)} dependency license(s) failed the gate.\n")
        return 1
    sys.stdout.write(f"\nAll {len(verdicts)} dependency licenses are AGPL-3.0 compatible.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
