# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operation overlays: contest templates, and the everyday template.

An overlay is a declarative data file (Principle III). It says which fields
the New QSO box asks for, how the page is laid out by default, what the
exchange looks like, and what counts as a dupe. Adding a contest means adding
a file to ``templates/``; it never changes Engine code or the database.

Overlay files are untrusted input (Principle XII): each is size-limited and
parsed into strict, frozen models before anything uses it, and a file that
does not parse is refused rather than half-loaded.
"""

from __future__ import annotations

import json
import re
from functools import cache
from importlib.resources import files
from typing import Annotated, Final, Literal, Self

from pydantic import BaseModel, ConfigDict, Field, StringConstraints, model_validator

MAX_FILE: Final = 64 * 1024
TEMPLATES: Final = "templates"

_FIELD = r"^[A-Z][A-Z0-9_]{0,63}$"
_BAND = r"^([0-9]+(\.[0-9]+)?(m|cm|mm)|submm)$"
_MODE = r"^[A-Z0-9-]{1,32}$"

FieldName = Annotated[str, StringConstraints(pattern=_FIELD)]
Band = Annotated[str, StringConstraints(pattern=_BAND)]
ModeName = Annotated[str, StringConstraints(pattern=_MODE)]
Text = Annotated[str, StringConstraints(min_length=1, max_length=400)]
Key = Annotated[str, StringConstraints(pattern=r"^[a-z][a-z0-9_]{0,31}$")]

# The ways a contest groups modes for dupes and points.
ModeGroup = Literal["CW", "Phone", "Digital"]


class OverlayError(Exception):
    """An overlay file is missing, too large, or not a valid overlay."""


class _Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True, frozen=True)


class Rules(_Strict):
    """Where the contest's rules are published, so every rule here can be checked."""

    title: Text
    url: Annotated[str, StringConstraints(pattern=r"^https://[^\s]{1,300}$")]
    revised: Annotated[str, StringConstraints(pattern=r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")]
    period: Text


class Modes(_Strict):
    """How the contest groups ADIF modes.

    A QSO whose ``MODE`` is listed under ``cw`` or ``phone`` is in that group;
    every other mode is Digital. A QSO whose ``MODE`` or ``SUBMODE`` is in
    ``excluded`` does not count in the contest at all.
    """

    cw: Annotated[tuple[ModeName, ...], Field(min_length=1, max_length=20)]
    phone: Annotated[tuple[ModeName, ...], Field(min_length=1, max_length=20)]
    excluded: Annotated[tuple[ModeName, ...], Field(max_length=40)] = ()

    def group(self, mode: str | None, submode: str | None) -> ModeGroup | None:
        """The group of a QSO's mode, or ``None`` if the contest does not count it."""
        names = {n.strip().upper() for n in (mode, submode) if n and n.strip()}
        if not mode or not mode.strip() or names & set(self.excluded):
            return None
        mode = mode.strip().upper()
        if mode in self.cw:
            return "CW"
        if mode in self.phone:
            return "Phone"
        return "Digital"


class Bands(_Strict):
    """The bands a contest counts: only ``allowed``, or every band but ``excluded``.

    With ``satellite``, a QSO whose ``PROP_MODE`` is ``SAT`` counts on a band
    of its own, called ``SAT``, whatever band its uplink was on.
    """

    allowed: Annotated[tuple[Band, ...], Field(max_length=40)] | None = None
    excluded: Annotated[tuple[Band, ...], Field(max_length=40)] = ()
    satellite: bool = False

    @model_validator(mode="after")
    def _one_way(self) -> Self:
        if self.allowed is not None and self.excluded:
            msg = "list the allowed bands or the excluded bands, not both"
            raise ValueError(msg)
        return self

    def key(self, band: str | None, prop_mode: str | None) -> str | None:
        """The band a QSO counts on, or ``None`` if the contest does not count it."""
        if self.satellite and (prop_mode or "").strip().upper() == "SAT":
            return "SAT"
        band = (band or "").strip().lower()
        if not band:
            return None
        if self.allowed is not None:
            return band if band in self.allowed else None
        return None if band in self.excluded else band


DupePer = Literal["band", "mode", "my_park", "their_park"]


class Dupes(_Strict):
    """What makes two QSOs the same contact.

    Two QSOs are dupes when they are with the same call, made as the same
    station callsign, in the same contest (or program, like POTA) in the same
    UTC calendar year (or, ``within`` a ``day``, the same UTC day), and agree
    on everything listed in ``per``: the band, the mode group, the park the
    station was in (``my_park``, ADIF ``MY_SIG_INFO``) and the park the other
    station was in (``their_park``, ADIF ``SIG_INFO``).
    """

    per: Annotated[tuple[DupePer, ...], Field(max_length=4)]
    rule: Text
    within: Literal["year", "day"] = "year"

    @model_validator(mode="after")
    def _once(self) -> Self:
        if len(self.per) != len(set(self.per)):
            msg = "each dupe rule part may appear only once"
            raise ValueError(msg)
        return self


class ExchangePart(_Strict):
    """One part of an exchange.

    ``field`` is the ADIF field it is logged in. A value must match
    ``pattern``, or be a section code when ``sections`` is set, or be one of
    ``also``. An ``optional`` received part may be left out, like the other
    station's park, which only a park-to-park contact has.
    """

    key: Key
    field: FieldName | None = None
    label: Annotated[str, StringConstraints(min_length=1, max_length=40)]
    hint: Annotated[str, StringConstraints(max_length=200)] = ""
    pattern: Annotated[str, StringConstraints(min_length=1, max_length=200)] | None = None
    sections: bool = False
    optional: bool = False
    also: Annotated[
        tuple[Annotated[str, StringConstraints(pattern=r"^[A-Z]{1,8}$")], ...], Field(max_length=10)
    ] = ()

    @model_validator(mode="after")
    def _checkable(self) -> Self:
        if self.pattern is None and not self.sections:
            msg = f"{self.key}: give a pattern or sections"
            raise ValueError(msg)
        if self.pattern is not None:
            try:
                re.compile(self.pattern)
            except re.error as error:
                msg = f"{self.key}: the pattern is not valid: {error}"
                raise ValueError(msg) from error
        return self


class Exchange(_Strict):
    """What a station sends and receives.

    ``sent`` is set once for the whole operation (a club's class and section);
    ``sent_fields`` say which ADIF fields of each QSO it fills, with
    ``{key}`` replaced by that part's value. ``received`` is typed for each QSO.
    """

    sent: Annotated[tuple[ExchangePart, ...], Field(max_length=6)]
    sent_fields: dict[FieldName, Annotated[str, StringConstraints(max_length=100)]]
    received: Annotated[tuple[ExchangePart, ...], Field(max_length=6)]

    @model_validator(mode="after")
    def _keys(self) -> Self:
        keys = [p.key for p in self.sent]
        if len(keys) != len(set(keys)):
            msg = "each sent part needs its own key"
            raise ValueError(msg)
        for template in self.sent_fields.values():
            unknown = set(re.findall(r"\{([a-z0-9_]+)\}", template)) - set(keys)
            if unknown:
                msg = f"sent_fields names parts that are not sent: {sorted(unknown)}"
                raise ValueError(msg)
        if any(p.field is None for p in self.received):
            msg = "each received part names the ADIF field it is logged in"
            raise ValueError(msg)
        return self


ModuleId = Annotated[str, StringConstraints(pattern=r"^[a-z][a-z0-9-]{0,31}$")]


class ModulePlace(_Strict):
    """A module in the overlay's default layout."""

    id: ModuleId
    width: Literal["quarter", "third", "half", "two-thirds", "full"] = "full"
    side: Literal["left", "middle-left", "middle", "middle-right", "right"] = "left"
    hidden: bool = False


class Layout(_Strict):
    """The page an overlay starts from, before an operator moves anything."""

    modules: Annotated[tuple[ModulePlace, ...], Field(min_length=1, max_length=32)]
    log_columns: Annotated[
        tuple[Annotated[str, StringConstraints(pattern=r"^[a-z_]+(:[A-Z][A-Z0-9_]*)?$")], ...],
        Field(min_length=1, max_length=40),
    ]


class Entry(_Strict):
    """The New QSO box: the ADIF fields it asks for, in order.

    With ``live``, the date and time are those of the moment the QSO is stored,
    unless the operator typed them. Tab moves through the fields in order; with
    ``store_after``, the Store QSO button comes right after that field instead
    of after the last one.
    """

    fields: Annotated[tuple[FieldName, ...], Field(min_length=1, max_length=40)]
    live: bool = False
    store_after: FieldName | None = None

    @model_validator(mode="after")
    def _has_call(self) -> Self:
        if "CALL" not in self.fields:
            msg = "the New QSO box must ask for CALL"
            raise ValueError(msg)
        if len(self.fields) != len(set(self.fields)):
            msg = "each field may appear only once"
            raise ValueError(msg)
        if self.store_after is not None and self.store_after not in self.fields:
            msg = "the Store QSO button must follow a field the New QSO box asks for"
            raise ValueError(msg)
        return self


StatId = Literal[
    "rate",
    "hourly",
    "band_mode",
    "operators",
    "dupes",
    "sections",
    "score",
    "activation",
    "p2p",
]
STAT_IDS: Final[tuple[StatId, ...]] = (
    "rate",
    "hourly",
    "band_mode",
    "operators",
    "dupes",
    "sections",
    "score",
    "activation",
    "p2p",
)


class Stats(_Strict):
    """What the Operation stats box shows for operations using this overlay.

    ``show`` picks from the stats the Engine can work out, in the order the
    box shows them: the QSO rate (``rate``), QSOs in each hour (``hourly``),
    QSOs by band and mode (``band_mode``), each operator's QSOs
    (``operators``), dupes (``dupes``), sections worked (``sections``), a
    rough score (``score``), and for a program like POTA, each park's progress
    toward an activation (``activation``) and park-to-park contacts
    (``p2p``). The score is the contest's QSO points: each counting QSO
    that is not a dupe earns the ``points`` of its mode group, and a
    satellite QSO only with ``satellite_points``. Power multipliers and bonus
    points are not part of it.
    """

    show: Annotated[tuple[StatId, ...], Field(max_length=len(STAT_IDS))]
    points: dict[ModeGroup, Annotated[int, Field(ge=0, le=100)]] | None = None
    satellite_points: bool = True

    @model_validator(mode="after")
    def _whole(self) -> Self:
        if len(self.show) != len(set(self.show)):
            msg = "each stat may be shown only once"
            raise ValueError(msg)
        if "score" in self.show and not self.points:
            msg = "a score needs the points each mode group earns"
            raise ValueError(msg)
        return self


class Program(_Strict):
    """An awards program an overlay logs for, like Parks on the Air, rather than a contest.

    Its QSOs carry no ``CONTEST_ID``; the station's own reference (its park)
    is its sent exchange, logged in ``MY_SIG`` and ``MY_SIG_INFO``, and the
    other station's, when it has one, in ``SIG`` and ``SIG_INFO``. ``role``
    says which end the overlay logs: the ``activator`` in the park. An
    activation needs ``goal`` QSOs from one reference in one UTC day.
    """

    sig: Literal["POTA"]
    role: Literal["activator"]
    goal: Annotated[int, Field(ge=1, le=1000)]


class Overlay(_Strict):
    """One operation overlay.

    ``contest_id`` is the ADIF ``CONTEST_ID`` its QSOs carry; the everyday
    overlay has none, no exchange, and never checks for dupes. An overlay for
    a ``program``, like POTA, has no ``CONTEST_ID`` either, but has bands,
    modes, an exchange and its own dupe rule. ``gota`` says the contest has a
    GOTA station, with a callsign of its own.
    """

    format: Literal[1]
    id: Annotated[str, StringConstraints(pattern=r"^[a-z][a-z0-9-]{0,31}$")]
    name: Annotated[str, StringConstraints(min_length=1, max_length=60)]
    summary: Text
    contest_id: Annotated[str, StringConstraints(pattern=r"^[A-Z0-9-]{1,40}$")] | None = None
    rules: Rules | None = None
    bands: Bands | None = None
    modes: Modes | None = None
    dupes: Dupes | None = None
    exchange: Exchange | None = None
    entry: Entry
    layout: Layout | None = None
    gota: bool = False
    stats: Stats | None = None
    program: Program | None = None

    @model_validator(mode="after")
    def _contest_is_whole(self) -> Self:
        contest = (self.contest_id, self.rules, self.bands, self.modes, self.exchange)
        if self.program is not None:
            if self.contest_id is not None or self.gota:
                msg = "a program overlay has no contest_id and no GOTA station"
                raise ValueError(msg)
            if any(part is None for part in contest[1:]):
                msg = "a program overlay needs rules, bands, modes and an exchange"
                raise ValueError(msg)
        elif self.contest_id is None:
            if any(part is not None for part in (*contest, self.dupes)):
                msg = (
                    "an overlay without a contest_id has no rules, bands, modes, dupes or exchange"
                )
                raise ValueError(msg)
        elif any(part is None for part in contest):
            msg = "a contest overlay needs rules, bands, modes and an exchange"
            raise ValueError(msg)
        if self.stats is not None:
            if "dupes" in self.stats.show and self.dupes is None:
                msg = "dupes can be shown only by an overlay with a dupe check"
                raise ValueError(msg)
            if "score" in self.stats.show and self.contest_id is None:
                msg = "a score can be shown only by a contest overlay"
                raise ValueError(msg)
            if {"activation", "p2p"} & set(self.stats.show) and self.program is None:
                msg = "activations and park-to-park contacts are shown only for a program"
                raise ValueError(msg)
        by_day_or_park = self.dupes is not None and (
            self.dupes.within != "year" or bool({"my_park", "their_park"} & set(self.dupes.per))
        )
        if by_day_or_park and self.program is None:
            msg = "only a program overlay checks dupes by day or by park"
            raise ValueError(msg)
        return self

    @property
    def is_contest(self) -> bool:
        """Whether this overlay is a contest."""
        return self.contest_id is not None

    @property
    def checks_dupes(self) -> bool:
        """Whether QSOs logged under this overlay are checked for dupes."""
        return self.dupes is not None and (self.is_contest or self.program is not None)

    @property
    def shown_stats(self) -> tuple[StatId, ...]:
        """The stats the Operation stats box shows: the overlay's choice, or the usual ones."""
        if self.stats is not None:
            return self.stats.show
        usual: tuple[StatId, ...] = ("rate", "hourly", "band_mode", "operators")
        return (*usual, "dupes") if self.dupes is not None else usual


def parse(data: bytes, name: str) -> Overlay:
    """Parse one overlay file, refusing anything that is not a whole, valid overlay.

    Raises:
        OverlayError: The file is too large, not JSON, or not a valid overlay.
    """
    if len(data) > MAX_FILE:
        raise OverlayError(f"{name}: an overlay file is at most {MAX_FILE} bytes")
    try:
        raw = json.loads(data.decode("utf-8"))
        overlay = Overlay.model_validate(raw, strict=False)
    except (UnicodeDecodeError, ValueError) as error:
        raise OverlayError(f"{name}: not a valid overlay: {error}") from error
    if f"{overlay.id}.json" != name:
        raise OverlayError(f"{name}: the file must be named after its id, {overlay.id}.json")
    return overlay


@cache
def overlays() -> tuple[Overlay, ...]:
    """Every overlay the Engine ships, Everyday first, then by name.

    Raises:
        OverlayError: An overlay file is not valid.
    """
    found = [
        parse(entry.read_bytes(), entry.name)
        for entry in files(__package__).joinpath(TEMPLATES).iterdir()
        if entry.name.endswith(".json")
    ]
    found.sort(key=lambda o: (o.is_contest or o.program is not None, o.name))
    if not found or found[0].id != EVERYDAY:
        raise OverlayError(f"the {EVERYDAY} overlay is missing")
    return tuple(found)


EVERYDAY: Final = "everyday"


def overlay(overlay_id: str) -> Overlay | None:
    """The overlay called ``overlay_id``, or ``None``."""
    return next((o for o in overlays() if o.id == overlay_id), None)
