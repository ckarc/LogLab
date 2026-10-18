# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reference data the Engine owns and serves itself (Principle VI)."""

from loglab_engine.reference.sections import (
    SECTIONS,
    SOURCE,
    Section,
    SectionTally,
    section_for,
    tally_sections,
)

__all__ = ["SECTIONS", "SOURCE", "Section", "SectionTally", "section_for", "tally_sections"]
