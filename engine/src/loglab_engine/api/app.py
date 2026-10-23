# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Engine's HTTP API, version 1, and the web client it serves.

Everything a client can do goes through these operations (Principle I); the
web client served at ``/`` has no other way in. Every request body is parsed
into a typed, size-limited model before anything else happens (Principle XII).
Every request except signing in and the status check is made by a signed-in
operator, who must hold the permission it needs (Principle XIII): ``view`` to
read, ``log`` to change QSOs. All times are UTC (Principle VIII).

Callsign lookup through QRZ.com is optional: with no login saved, or no
internet, a lookup answers that it could not fill anything in, and logging
works exactly the same (Principle V). So is Logbook of the World: QSOs wait
to be sent until an operator sends them, and a send or a fetch with no
internet changes nothing and says so.

A QSO is logged once the Engine has committed it to the database (Principle
XV). A client may enter a QSO under an ID it chose when the QSO was typed, so
sending it again from its journal never creates a duplicate.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import hashlib
import time
import uuid
from collections import Counter
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable, Iterator, Mapping
from dataclasses import replace
from datetime import UTC, date, datetime, timedelta
from importlib.resources import files
from typing import TYPE_CHECKING, Annotated, Final, Literal, Self

import psycopg
from fastapi import (
    APIRouter,
    Depends,
    FastAPI,
    Header,
    HTTPException,
    Query,
    Request,
    Response,
    status,
)
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import (
    AwareDatetime,
    BaseModel,
    ConfigDict,
    Field,
    StringConstraints,
    ValidationError,
    field_validator,
    model_validator,
)
from starlette.concurrency import run_in_threadpool

from loglab_engine import __version__
from loglab_engine.adif import AdifDocument, AdifModelError, AdiParseError, Record, parse_adi
from loglab_engine.adif import Field as AdifField
from loglab_engine.callbook import (
    Callbook,
    Link,
    LoginCheck,
    LookupStatus,
    QrzClient,
    http_fetch,
    http_fetch_large,
)
from loglab_engine.callbook.logbook_sync import LogbookSync
from loglab_engine.callbook.profiles import NetworkSeen, ProfileRefresher, look_at_network
from loglab_engine.callbook.qrz import QrzReplyError, QrzUnreachableError
from loglab_engine.callbook.qrz_logbook import QrzKeyError, QrzLogbookClient, http_post
from loglab_engine.callbook.service import SOURCE as CALLBOOK_SOURCE
from loglab_engine.contests import EVERYDAY, Earlier, Overlay, Worked, overlays, sent_problems
from loglab_engine.contests import check as contest_check
from loglab_engine.contests.overlay import Band, ModeName, StatId
from loglab_engine.contests.stamp import Station, stamp, stamp_fields
from loglab_engine.contests.stats import Logged, operation_stats
from loglab_engine.demo import NAME as DEMO_NAME
from loglab_engine.demo import Demo
from loglab_engine.fieldlab import FieldLabBus, GpsFix
from loglab_engine.lotw import (
    CertificateError,
    LocationError,
    Lotw,
    LotwClient,
    Place,
    Setup,
    Skipped,
    Transport,
    https,
)
from loglab_engine.lotw import Link as LotwLink
from loglab_engine.lotw import tables as lotw_tables
from loglab_engine.onair import OnAir, Report
from loglab_engine.pairing import LIFETIME, Pairing, PairingDesk, PairingError
from loglab_engine.reference import (
    SECTIONS,
    ZONE_FIELDS,
    ZONES,
    ZoneKind,
    distance_km,
    grid_place,
    record_place,
    section_for,
    tally_sections,
    tally_zones,
)
from loglab_engine.reference import SOURCE as SECTIONS_SOURCE
from loglab_engine.reference import Place as MapPlace
from loglab_engine.reference.licenses import FCC_CREDIT, ISED_CREDIT
from loglab_engine.reference.refresh import Downloads, HttpDownloads, Refresher
from loglab_engine.rig import Radio, RigBoard, RigMode, RigState, adif_mode, band_for
from loglab_engine.store import (
    Actor,
    ArchivedError,
    DuplicateImportError,
    IdentityStore,
    LogStore,
    NotAuthorizedError,
    NotSignedInError,
    Permission,
    QsoExistsError,
    QsoFilter,
    QsoNotFoundError,
    QsoVersion,
    SignInError,
    StaleVersionError,
    StoreError,
    WorkspaceStore,
    connect,
)
from loglab_engine.store.agent import AgentKeyError, AgentKeyStore
from loglab_engine.store.callbook import CallbookStore
from loglab_engine.store.licenses import COUNTRY, LicenseStore, Source
from loglab_engine.store.lotw import BatchQso, LotwAccountStore, LotwBatch
from loglab_engine.store.operation import (
    Operation,
    OperationNotFoundError,
    OperationSetup,
    OperationStore,
)
from loglab_engine.store.overlay import OverlayStore
from loglab_engine.store.qrz_logbook import LogbookStore, LogbookTakenError

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

    from loglab_engine.callbook.service import BioResult, LookupResult

SOURCE: Final = "https://github.com/ckarc/loglab"
MAX_FIELDS: Final = 500
MAX_VALUE: Final = 64 * 1024
MAX_NAME: Final = 64
MAX_NOTE: Final = 1000
MAX_LIST: Final = 500
MAX_P12_BASE64: Final = 96 * 1024
# An ADIF file the web client takes in, in base64: about 30 MB of ADIF, or
# some 100,000 QSOs. Bigger logs go in with ``loglab-log import``.
MAX_IMPORT_BASE64: Final = 40 * 1024 * 1024
MAX_IMPORT_WARNINGS: Final = 20
# The web client loads nothing from anywhere else, so it works offline (Principle V).
_CSP: Final = (
    "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
    "connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
)


# -- Request and response models (the boundary, Principle XII) ---------------


class _Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True, frozen=True)


class SignIn(_Strict):
    """Sign an operator in at a station."""

    callsign: Annotated[str, StringConstraints(min_length=1, max_length=20)]
    secret: Annotated[str, StringConstraints(min_length=1, max_length=128)]
    station: Annotated[str, StringConstraints(min_length=1, max_length=64)]


class AdifFieldModel(_Strict):
    """One ADIF field: its name as written, its data, and its optional type indicator."""

    name: Annotated[str, StringConstraints(min_length=1, max_length=MAX_NAME)]
    value: Annotated[str, StringConstraints(max_length=MAX_VALUE)]
    type: Annotated[str, StringConstraints(min_length=1, max_length=1)] | None = None


Note = Annotated[str, StringConstraints(max_length=MAX_NOTE)]


class OperationRef(_Strict):
    """The operation a QSO is logged in, and whether it is the GOTA station's.

    The Engine adds the operation's fields to the QSO (see
    :mod:`loglab_engine.contests.stamp`): its station callsign, or its GOTA
    callsign with ``gota``; the signed-in operator as ``OPERATOR``; its
    ``CONTEST_ID``; and its sent exchange.
    """

    id: Annotated[uuid.UUID, Field(strict=False)]
    gota: bool = False


_CALL_PATTERN: Final = r"^[A-Za-z0-9]+(/[A-Za-z0-9]+)*$"
CallSign = Annotated[str, StringConstraints(max_length=20, pattern=_CALL_PATTERN)]


class OnAirIn(_Strict):
    """What a workstation is set to now: the band and mode in its New QSO box.

    ``gota`` says it is the GOTA station, so it is on the air as the GOTA call.
    """

    band: Band | None = None
    mode: ModeName | None = None
    submode: ModeName | None = None
    gota: bool = False


# Text a radio or the Agent names itself with: printable, no control characters.
_PLAIN: Final = r"^[^\x00-\x1f\x7f]*$"


class RadioPathIn(_Strict):
    """How the Agent reaches the radio: Hamlib (with its model number), a FlexRadio, or pretend."""

    kind: Literal["hamlib", "flex", "pretend"]
    model: Annotated[int, Field(ge=1, le=99_999)] | None = None


class RadioIn(_Strict):
    """A radio and its live state, as the radio reported it to the Agent (Principle X)."""

    maker: Annotated[str, StringConstraints(min_length=1, max_length=40, pattern=_PLAIN)]
    model: Annotated[str, StringConstraints(min_length=1, max_length=40, pattern=_PLAIN)]
    path: RadioPathIn
    frequency_hz: Annotated[int, Field(ge=1, le=300_000_000_000)]
    mode: RigMode | None = None
    passband_hz: Annotated[int, Field(ge=1, le=1_000_000)] | None = None
    ptt: bool | None = None
    vfo: Annotated[str, StringConstraints(max_length=16, pattern=_PLAIN)] | None = None


class AgentReportIn(_Strict):
    """What a station's Agent says: its radio's live state, or why it can't read it."""

    agent: Annotated[str, StringConstraints(min_length=1, max_length=64, pattern=_PLAIN)]
    radio: RadioIn | None = None
    problem: Annotated[str, StringConstraints(max_length=300, pattern=_PLAIN)] | None = None


StationName = Annotated[str, StringConstraints(pattern=r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")]


class PairingAskIn(_Strict):
    """A Station Agent asking to join as a station."""

    station: StationName
    agent: Annotated[str, StringConstraints(min_length=1, max_length=64, pattern=_PLAIN)]


class OperationIn(_Strict):
    """An operation as an administrator sets it up.

    ``sent`` holds the overlay's sent exchange by part, like
    ``{"class": "3A", "section": "KS"}``.
    """

    overlay: Annotated[str, StringConstraints(min_length=1, max_length=32)]
    name: Annotated[str, StringConstraints(min_length=1, max_length=60, strip_whitespace=True)]
    station_call: CallSign
    gota_call: CallSign | None = None
    sent: Annotated[
        dict[
            Annotated[str, StringConstraints(pattern=r"^[a-z][a-z0-9_]{0,31}$")],
            Annotated[str, StringConstraints(max_length=40)],
        ],
        Field(max_length=6),
    ] = Field(default_factory=dict)
    ended: bool = False
    starts_at: Annotated[AwareDatetime, Field(strict=False)] | None = None
    ends_at: Annotated[AwareDatetime, Field(strict=False)] | None = None
    practice: bool = False

    @model_validator(mode="after")
    def _hours(self) -> Self:
        if self.starts_at and self.ends_at and self.ends_at <= self.starts_at:
            msg = "the operation must end after it starts"
            raise ValueError(msg)
        return self


class TallyOut(BaseModel):
    """How many QSOs share one band and mode group, or one operator."""

    band: str | None = None
    mode_group: str | None = None
    operator: str | None = None
    qsos: int


class OperationSummaryOut(BaseModel):
    """What an operation has logged so far.

    ``counted`` QSOs fall within its hours on a band and mode its overlay
    counts; ``outside`` were made outside its hours, ``not_counted`` on a
    band or mode it leaves out. Dupes are counted as logged.
    """

    operation: uuid.UUID
    name: str
    qsos: int
    counted: int
    outside: int
    not_counted: int
    by_band_mode: list[TallyOut]
    by_operator: list[TallyOut]


class HourOut(BaseModel):
    """The QSOs made in one UTC hour."""

    start: datetime
    qsos: int


class BandModeOut(BaseModel):
    """Counting QSOs on one band in one mode group, and the points they earn."""

    band: str
    mode: str
    qsos: int
    points: int


class OperatorStatsOut(BaseModel):
    """One operator's QSOs in an operation, in all and in the last hour."""

    operator: str
    qsos: int
    last_hour: int
    points: int


class OperationStatsOut(BaseModel):
    """How the whole group is doing in an operation, for the Operation stats box.

    ``show`` lists the stats the overlay asks the box to show, in order.
    ``counted`` QSOs count in the contest and are not dupes. ``rate`` is the
    QSOs per hour at the pace of the last ten minutes. ``score`` is the
    overlay's QSO points (``null`` when it gives none), before power
    multipliers and bonus points.
    """

    operation: uuid.UUID
    name: str
    overlay_name: str
    show: list[StatId]
    now: datetime
    starts_at: datetime | None
    ends_at: datetime | None
    qsos: int
    counted: int
    outside: int
    not_counted: int
    dupes: int
    last_hour: int
    last_10: int
    rate: int
    hourly: list[HourOut]
    best_hour: HourOut | None
    by_band_mode: list[BandModeOut]
    by_mode: dict[str, int]
    operators: list[OperatorStatsOut]
    sections_worked: int
    sections_total: int
    score: int | None
    points: dict[str, int]


class OnAirOut(BaseModel):
    """One workstation on the air in an operation.

    ``active`` means its operator logged a QSO in the operation in the last
    ``active_minutes``; ``clash`` that another workstation is on the same band
    in the same mode group as the same call (Field Day allows one transmitter
    per band and mode). ``mode_group`` is ``null`` outside a contest.

    ``from_radio`` means the station's Agent is reporting its radio, so the
    band (and the mode, unless the radio is set up for data) are the radio's,
    not what was typed (Principle X); ``frequency_hz`` and ``transmitting``
    are then the radio's too.
    """

    operator: str
    station: str
    call: str
    band: str | None
    mode: str | None
    mode_group: str | None
    seen_at: datetime
    last_qso: datetime | None
    active: bool
    clash: bool
    you: bool
    from_radio: bool = False
    frequency_hz: int | None = None
    transmitting: bool | None = None
    pretend: bool = False


class DemoIn(_Strict):
    """Turn demo mode on or off."""

    on: bool


class DemoOut(BaseModel):
    """Whether demo mode is on: pretend operators logging made-up QSOs (Principle XX).

    ``operation`` is the practice operation they log in; ``qsos`` how many
    they have logged since it was turned on; ``problem`` why it stopped by
    itself, if it did; ``erased`` how many pretend QSOs turning it off just
    erased.
    """

    on: bool
    operation: uuid.UUID | None
    operation_name: str | None
    started_by: str | None
    started_at: datetime | None
    qsos: int
    stations: list[str]
    problem: str | None
    erased: int | None = None


class RadioOut(BaseModel):
    """A station's radio, live: what the radio last reported, through its Agent.

    ``mode`` is the radio's own (``USB``, ``CW``, ``DATA-USB``...);
    ``adif_mode`` and ``adif_submode`` are what a QSO would record, or
    ``null`` when the radio is set up for data and the operator says which
    data mode. ``transmitting`` is ``null`` when the radio can't say.
    """

    maker: str
    model: str
    path: str
    frequency_hz: int
    mhz: str
    band: str | None
    mode: str | None
    adif_mode: str | None
    adif_submode: str | None
    passband_hz: int | None
    transmitting: bool | None
    vfo: str | None


class RigOut(BaseModel):
    """The radio at the signed-in station, as its Station Agent reports it.

    ``heard`` is false when no Agent at this station has reported lately;
    ``radio`` is ``null`` when the Agent can't read the radio, and ``problem``
    then says why.
    """

    station: str
    heard: bool
    agent: str | None
    radio: RadioOut | None
    problem: str | None
    heard_at: datetime | None


class PairingAskOut(BaseModel):
    """The Agent's request: the code it shows, and its private ticket for asking how it stands."""

    id: uuid.UUID
    code: str
    ticket: str
    expires_at: datetime


class PairingStateOut(BaseModel):
    """How an Agent's request stands; ``key`` comes once, when it is approved."""

    state: Literal["waiting", "approved", "refused", "expired"]
    key: str | None


class PairingOut(BaseModel):
    """A request an administrator can approve. ``replaces`` means the station already has a key."""

    id: uuid.UUID
    station: str
    agent: str
    code: str
    asked_at: datetime
    replaces: bool


class AgentKeysOut(BaseModel):
    """The stations whose Agents have a working key."""

    stations: list[str]


class OnAirListOut(BaseModel):
    """Who is on the air in an operation now."""

    operation: uuid.UUID
    name: str
    active_minutes: int
    stations: list[OnAirOut]


class OperationOut(BaseModel):
    """An operation: its overlay, calls, and the exchange it sends."""

    id: uuid.UUID
    overlay: str
    overlay_name: str
    name: str
    station_call: str
    gota_call: str | None
    sent: dict[str, str]
    ended: bool
    starts_at: datetime | None
    ends_at: datetime | None
    archived_at: datetime | None
    created_at: datetime
    changed_at: datetime
    practice: bool


class QsoCreate(_Strict):
    """A new QSO: its fields, in order, and optionally the ID its client gave it.

    A client that keeps a journal gives each QSO its ID when it is typed, so
    sending the same QSO again never creates a second one (Principle XV).
    """

    # JSON has no UUID type: the ID arrives as text and must parse as one.
    id: Annotated[uuid.UUID, Field(strict=False)] | None = None
    fields: Annotated[list[AdifFieldModel], Field(min_length=1, max_length=MAX_FIELDS)]
    note: Note | None = None
    operation: OperationRef | None = None


class QsoEdit(_Strict):
    """New fields for a QSO, and the version they were based on."""

    base_version: Annotated[int, Field(ge=1)]
    fields: Annotated[list[AdifFieldModel], Field(min_length=1, max_length=MAX_FIELDS)]
    note: Note | None = None


class QsoChange(_Strict):
    """Why a QSO is being deleted or restored."""

    note: Note | None = None


MAX_PATHS: Final = 500  # the most QSOs the world map draws
ModuleId = Literal[
    "entry",
    "profile",
    "journal",
    "log",
    "detail",
    "map",
    "world",
    "cqzones",
    "ituzones",
    "stats",
    "onair",
]
# A column of a table: one of the fixed columns, or ``field:NAME`` for any ADIF field.
_FIELD_COLUMN: Final = r"field:[A-Za-z][A-Za-z0-9_]{0,63}"
ColumnId = Annotated[
    str,
    StringConstraints(
        pattern=rf"^(date|time|call|band|mode|operator|station|version|changed_at|{_FIELD_COLUMN})$",
    ),
]
SortKey = Annotated[
    str,
    StringConstraints(
        pattern=rf"^(entered|qso_start|call|band|mode|operator|station|version|changed_at|{_FIELD_COLUMN})$",
    ),
]
TableId = Literal["log", "profile"]
# A panel inside a module, which the operator places on one side of the
# module's main content: the map's list of sections still to work.
PanelId = Literal["map.to-work"]
PanelPlace = Literal["top", "bottom", "left", "right"]


# How small or large a box may draw its contents, in percent.
MIN_SCALE: Final = 50
MAX_SCALE: Final = 200


class ModuleLayout(_Strict):
    """One module of the workspace: its width, side, scale, and whether it is shown.

    A quarter sits on the left, middle-left, middle-right or right; a third
    on the left, middle or right; a half or two thirds on the left or right;
    a full-width box ignores it. ``scale`` is how large the box draws what is inside it,
    in percent of the normal size.
    """

    id: ModuleId
    width: Literal["quarter", "third", "half", "two-thirds", "full"] = "full"
    side: Literal["left", "middle-left", "middle", "middle-right", "right"] = "left"
    scale: Annotated[int, Field(ge=MIN_SCALE, le=MAX_SCALE)] = 100
    hidden: bool = False

    @model_validator(mode="after")
    def _side_fits_width(self) -> Self:
        if self.side == "middle" and self.width != "third":
            msg = "only a third can sit on the middle side"
            raise ValueError(msg)
        if self.side in ("middle-left", "middle-right") and self.width != "quarter":
            msg = "only a quarter can sit middle-left or middle-right"
            raise ValueError(msg)
        return self


class TableLayout(_Strict):
    """One table's columns, in the order shown, and how it is sorted."""

    columns: Annotated[list[ColumnId], Field(min_length=1, max_length=40)]
    sort: SortKey = "entered"
    descending: bool = True

    @model_validator(mode="after")
    def _each_column_once(self) -> Self:
        if len(self.columns) != len({c.lower() for c in self.columns}):
            msg = "each column may appear only once"
            raise ValueError(msg)
        return self


FieldName = Annotated[str, StringConstraints(pattern=r"^[A-Za-z][A-Za-z0-9_]{0,63}$")]


class DetailLayout(_Strict):
    """The ADIF fields a QSO's detail always shows, in order, even when empty."""

    fields: Annotated[list[FieldName], Field(max_length=80)]

    @model_validator(mode="after")
    def _each_field_once(self) -> Self:
        if len(self.fields) != len({f.upper() for f in self.fields}):
            msg = "each field may appear only once"
            raise ValueError(msg)
        return self


OverlayId = Annotated[str, StringConstraints(pattern=r"^[a-z][a-z0-9-]{0,31}$")]


class OverlayArrangement(_Strict):
    """How an operator arranged the page in one overlay, kept while they use another."""

    modules: Annotated[list[ModuleLayout], Field(min_length=1, max_length=32)]
    tables: dict[TableId, TableLayout] = Field(default_factory=dict)


# The boxes whose growing part folds under a bar, and may be locked open.
ExpandableId = Literal["stats", "profile", "log", "onair"]


def _auto_expand() -> list[ExpandableId]:
    """The boxes that open by themselves when the pointer rests in them, until the operator says."""
    return ["stats", "profile", "onair"]


class Layout(_Strict):
    """An operator's workspace: its modules in order, its tables, panels, and a QSO's fields.

    ``detail`` is ``None`` until the operator chooses, and the page's default is used.
    ``theme`` is light, dark, or ``auto`` to follow the computer's setting.
    ``operation`` is the operation the operator logs in, or ``None`` for
    everyday logging under their own call. ``by_overlay`` keeps how they
    arranged the page in each other overlay, to bring back when they return.
    ``world_qsos`` is how many of the latest QSOs the world map draws.
    ``log_mine`` is whether the Log, in an operation, shows only the
    operator's own QSOs rather than everyone's.
    ``bio_open`` is whether the station profile shows a biography opened
    (closed until the operator opens one). ``stats_open`` is whether the
    Operation stats box shows its hourly chart and tables unfolded (folded
    until the operator opens them); the page now keeps that in ``expanded``,
    the boxes whose growing part the operator locked open, and reads this
    only from layouts saved before. ``auto_expand`` is the boxes that open
    by themselves when the pointer rests in them (the others open from
    their bar). ``distance_unit`` is how the
    station profile gives the distance to a station, in miles or kilometres.
    """

    version: Literal[1] = 1
    modules: Annotated[list[ModuleLayout], Field(min_length=1, max_length=32)]
    tables: dict[TableId, TableLayout] = Field(default_factory=dict)
    panels: dict[PanelId, PanelPlace] = Field(default_factory=dict)
    detail: DetailLayout | None = None
    theme: Literal["auto", "light", "dark"] = "auto"
    operation: OperationRef | None = None
    by_overlay: Annotated[dict[OverlayId, OverlayArrangement], Field(max_length=8)] = Field(
        default_factory=dict
    )
    world_qsos: Annotated[int, Field(ge=1, le=MAX_PATHS)] = 50
    bio_open: bool = False
    stats_open: bool = False
    expanded: Annotated[list[ExpandableId], Field(max_length=8)] = Field(default_factory=list)
    auto_expand: Annotated[list[ExpandableId], Field(max_length=8)] = Field(
        default_factory=_auto_expand
    )
    log_mine: bool = True
    distance_unit: Literal["mi", "km"] = "mi"

    @model_validator(mode="after")
    def _each_module_once(self) -> Self:
        ids = [m.id for m in self.modules]
        if len(ids) != len(set(ids)):
            msg = "each module may appear only once"
            raise ValueError(msg)
        return self


class LookupQuery(_Strict):
    """Which call to look up, and whether to fetch a fresh copy instead of the kept one."""

    model_config = ConfigDict(extra="forbid", strict=False, frozen=True)

    call: Annotated[
        str,
        StringConstraints(min_length=1, max_length=20, pattern=r"^[A-Za-z0-9]+(/[A-Za-z0-9]+)*$"),
    ]
    refresh: bool = False


class LookupOut(BaseModel):
    """What the license lists and the callbook say about a call, or why they could not say.

    ``status`` is ``found``, ``not_found``, ``not_set_up`` (no QRZ.com login),
    ``unreachable`` (no internet, or QRZ.com is down), or ``login_failed``.
    ``sources`` names where the answer came from: ``fcc`` or ``ised`` (the
    license lists the Engine keeps) and ``qrz``.
    """

    status: str
    call: str
    name: str | None
    qth: str | None
    state: str | None
    country: str | None
    grid: str | None
    photo: bool
    bio: bool
    cached: bool
    fetched_at: datetime | None
    message: str
    sources: list[str]
    license_class: str | None
    license_expires: date | None


class BioOut(BaseModel):
    """A call's QRZ.com biography as plain text, or why there is none.

    ``status`` is ``found``, ``not_found`` (no biography), ``not_set_up``,
    ``unreachable``, or ``login_failed``.
    """

    status: str
    call: str
    text: str | None
    fetched_at: datetime | None
    message: str


class QrzLoginIn(_Strict):
    """The operator's own QRZ.com username and password."""

    username: Annotated[str, StringConstraints(min_length=1, max_length=64, strip_whitespace=True)]
    password: Annotated[str, StringConstraints(min_length=1, max_length=128)]


class LicenseSourceOut(BaseModel):
    """How current one of the license lists the Engine keeps is."""

    source: str
    country: str
    calls: int
    published: datetime | None
    imported: datetime | None
    changes_published: datetime | None
    changes_imported: datetime | None
    credit: str


class LicensesOut(BaseModel):
    """The US and Canadian license lists: how current each is, and the last refresh problem."""

    sources: list[LicenseSourceOut]
    problem: str | None


class QrzLoginOut(BaseModel):
    """Whether the operator has a QRZ.com login saved. The password is never returned."""

    username: str | None
    status: str | None
    checked: bool
    message: str


class ProfileRefreshIn(_Strict):
    """When kept QRZ.com profiles are re-checked: ``auto`` (off Starlink), ``always``, ``off``."""

    mode: Literal["auto", "always", "off"]


# At least a week, so this week's lookups are never cleared out from under an operator.
CleanupDays = Annotated[int, Field(ge=7, le=3650)]


class CleanupQuery(_Strict):
    """How old a kept lookup of a never-logged call must be to count."""

    model_config = ConfigDict(extra="forbid", strict=False, frozen=True)

    days: CleanupDays = 90


class CleanupIn(_Strict):
    """Clear kept lookups of never-logged calls older than ``days``."""

    days: CleanupDays


class CleanupOut(BaseModel):
    """Kept lookups of calls never logged, older than ``days``: how many, and their size."""

    days: int
    calls: int
    photos_and_bios: int
    bytes: int


class CleanupDone(BaseModel):
    """How many never-logged calls' kept lookups were cleared."""

    days: int
    removed: int


class ProfileRefreshOut(BaseModel):
    """When kept QRZ.com profiles are re-checked, who said so, and how the last check went."""

    mode: str
    changed_by: str | None
    changed_at: datetime | None
    last_checked_at: datetime | None
    network: str | None
    message: str | None


class QrzLogbookIn(_Strict):
    """A QRZ logbook's API key, and the callsign that logbook is for."""

    api_key: Annotated[
        str, StringConstraints(strip_whitespace=True, pattern=r"^[A-Za-z0-9-]{8,64}$")
    ]
    book_call: (
        Annotated[
            str,
            StringConstraints(
                strip_whitespace=True, max_length=20, pattern=r"^[A-Za-z0-9]+(/[A-Za-z0-9]+)*$"
            ),
        ]
        | None
    ) = None


class QrzLogbookOut(BaseModel):
    """Where the operator's QSOs go on QRZ.com, and how they stand. The key is never sent back."""

    book_call: str | None
    sent: int
    waiting: int
    refused: int
    last_refusal: str | None
    problem: str | None
    message: str


class LayoutOut(BaseModel):
    """The operator's saved layout, or ``None`` to use the page's default."""

    layout: Layout | None


class SessionOut(BaseModel):
    """A signed-in session."""

    token: str | None
    operator: str
    station: str
    permissions: list[str]
    expires_at: datetime | None


class QsoOut(BaseModel):
    """One version of a QSO, with the audit record of the change that made it."""

    id: uuid.UUID
    version: int
    action: str
    deleted: bool
    fields: list[AdifFieldModel]
    call: str | None
    qso_start: datetime | None
    band: str | None
    mode: str | None
    changed_at: datetime
    operator: str
    station: str
    note: str | None
    import_id: int | None
    import_file: str | None


class QsoWritten(BaseModel):
    """The QSO as it stands after a change, which is now logged (Principle XV)."""

    qso: QsoOut
    created: bool
    message: str


class SectionOut(BaseModel):
    """One ARRL or RAC section, and how many QSOs in the log are with it."""

    code: str
    name: str
    division: str
    country: str
    qsos: int


class ValueCount(BaseModel):
    """A field value and how many QSOs carry it."""

    value: str
    qsos: int


class SectionsOut(BaseModel):
    """Every ARRL and RAC section and how many QSOs the log has in each.

    A QSO's section is its ADIF ``ARRL_SECT`` field. ``unrecognized`` lists the
    values that name no section; ``without_section`` counts QSOs with none.
    """

    sections: list[SectionOut]
    worked: int
    unrecognized: list[ValueCount]
    without_section: int
    source: str


class ZoneOut(BaseModel):
    """One CQ or ITU zone, and how many QSOs in the log are with it."""

    zone: int
    qsos: int


class ZonesOut(BaseModel):
    """Every CQ zone, or every ITU zone, and how many QSOs the log has in each.

    A QSO's zone is its ADIF ``CQZ`` or ``ITUZ`` field, named in ``field``.
    ``unrecognized`` lists the values that name no zone; ``without_zone``
    counts QSOs with none.
    """

    kind: ZoneKind
    field: str
    zones: list[ZoneOut]
    worked: int
    unrecognized: list[ValueCount]
    without_zone: int


class PlaceOut(BaseModel):
    """A point on the Earth, in degrees: latitude north, longitude east."""

    lat: float
    lon: float


class PathOut(BaseModel):
    """One QSO drawn on the world map: from the station to the other station."""

    id: uuid.UUID
    call: str | None
    qso_start: datetime | None
    band: str | None
    mode: str | None
    station: PlaceOut
    other: PlaceOut
    km: int


class HomeOut(PlaceOut):
    """Where the station is, and how the Engine knows.

    ``gps``: FieldLab's GPS fix, measured. ``qso``: a QSO's own location.
    ``saved``: the LoTW station location or the callbook's grid for the
    operator's own call. Only ``gps`` is measured; a grid is a square's centre.
    """

    source: Literal["gps", "qso", "saved"]


class PathsOut(BaseModel):
    """The latest QSOs that can be drawn on the world map.

    ``home`` is where the station is: in FieldLab, the trailer's GPS fix when
    a recent one has been heard; else the latest of these QSOs' own location
    (``MY_GRIDSQUARE``, or ``MY_LAT`` and ``MY_LON``), or else the grid of
    the station location saved for LoTW, or else the grid the callbook gave
    for the operator's own call when it was last looked up. ``without_place``
    counts the QSOs among the latest that have no location for the other
    station (no ``GRIDSQUARE``, or ``LAT`` and ``LON``); ``without_home``
    those with none for the station.
    """

    home: HomeOut | None
    paths: list[PathOut]
    without_place: int
    without_home: int


class ContestCheckIn(_Strict):
    """A QSO to check against the log in a contest overlay, before or after it is stored.

    ``station_call`` is the call the QSO is made as; without it, the QSO's
    own ``STATION_CALLSIGN`` or ``OPERATOR``, else the signed-in operator.
    ``exclude`` leaves out the QSO being edited, so it is not its own dupe.
    """

    fields: Annotated[list[AdifFieldModel], Field(min_length=1, max_length=MAX_FIELDS)]
    station_call: Annotated[str, StringConstraints(max_length=20, pattern=_CALL_PATTERN)] | None = (
        None
    )
    exclude: Annotated[uuid.UUID, Field(strict=False)] | None = None
    operation: OperationRef | None = None


class WorkedOut(BaseModel):
    """An earlier QSO with the same call in the same contest."""

    id: uuid.UUID
    band: str | None
    mode_group: str | None
    qso_start: datetime | None
    operator: str


class ProblemOut(BaseModel):
    """A part of the received exchange that does not look right."""

    field: str
    label: str
    message: str


class ContestCheckOut(BaseModel):
    """Whether a QSO is a dupe in the overlay, and whether it counts at all.

    ``status`` is ``new``, ``dupe``, ``not_counted`` (a band or mode the
    contest leaves out), ``incomplete`` (no call, band or mode yet), or
    ``no_check`` (an overlay that never checks for dupes, like Everyday).
    A dupe is only reported: it can still be stored.
    """

    overlay: str
    status: str
    station_call: str
    year: int
    band: str | None
    mode_group: str | None
    dupes: list[WorkedOut]
    worked: list[WorkedOut]
    problems: list[ProblemOut]
    message: str


class OverlayOut(Overlay):
    """An overlay, and where it comes from.

    ``builtin`` overlays ship with the Engine and can only be copied; the
    others were made in the editor, and carry their ``version``, whether they
    are ``retired``, and who saved them last.
    """

    builtin: bool
    version: int = 0
    retired: bool = False
    saved_by: str | None = None
    saved_at: datetime | None = None


class OverlaySave(_Strict):
    """An overlay from the editor, and the version it was opened at (0 when new)."""

    definition: Overlay
    base_version: Annotated[int, Field(ge=0)]

    @field_validator("definition", mode="before")
    @classmethod
    def _as_json(cls, value: object) -> object:
        """Read the definition as JSON, as the shipped overlay files are read."""
        if isinstance(value, dict):
            return Overlay.model_validate(value, strict=False)
        return value


class StatusOut(BaseModel):
    """What this Engine is, and where its source is (AGPL-3.0, Licensing)."""

    name: str
    version: str
    source: str
    license: str
    api: str


def _qso_out(version: QsoVersion) -> QsoOut:
    return QsoOut(
        id=version.qso_id,
        version=version.version,
        action=version.action.value,
        deleted=version.deleted,
        fields=[
            AdifFieldModel(name=f.name, value=f.value, type=f.type_indicator)
            for f in version.record.fields
        ],
        call=version.call,
        qso_start=version.qso_start,
        band=version.band,
        mode=version.mode,
        changed_at=version.changed_at,
        operator=version.operator,
        station=version.station,
        note=version.note,
        import_id=version.import_id,
        import_file=version.import_file,
    )


_DONE: Final = {
    "create": "Logged",
    "edit": "Saved version {version} of",
    "delete": "Marked deleted:",
    "restore": "Restored",
}


def _written(version: QsoVersion, *, created: bool = False) -> QsoWritten:
    what = _DONE[version.action.value].format(version=version.version)
    message = f"{what} {version.call or 'QSO'}, by {version.operator} at {version.station}."
    if version.action.value == "create" and not created:
        message = f"Already logged: {version.call or 'QSO'}. Nothing was added."
    return QsoWritten(qso=_qso_out(version), created=created, message=message)


def _record(fields: list[AdifFieldModel]) -> Record:
    try:
        return Record(tuple(AdifField(f.name, f.value, f.type) for f in fields))
    except AdifModelError as error:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(error)) from error


class AdifImportIn(_Strict):
    """An ADIF file to take into the log, in base64, and the operation to log it in.

    With ``operation``, each QSO gets the operation's fields as if it had
    been typed there (see :class:`OperationRef`), and QSOs made outside the
    operation's hours are left out.
    """

    file_name: Annotated[str, StringConstraints(min_length=1, max_length=255)]
    data_base64: Annotated[str, StringConstraints(min_length=1, max_length=MAX_IMPORT_BASE64)]
    operation: OperationRef | None = None
    note: Annotated[str, StringConstraints(max_length=MAX_NOTE)] | None = None


class AdifImportOut(BaseModel):
    """What happened to each QSO in an imported file.

    Every record is counted once: ``added``, ``already_logged``,
    ``already_deleted``, ``repeated`` (the same contact earlier in the same
    file), ``outside_hours`` or ``no_time``. ``already_imported`` names the
    earlier import when this exact file was taken in before, and then
    nothing is added.
    """

    file_name: str
    records: int
    import_id: int | None
    added: int
    already_logged: int
    already_deleted: int
    repeated: int
    outside_hours: int
    no_time: int
    already_imported: int | None
    warnings: list[str]


class AgentQsoIn(_Strict):
    """A QSO a program on the station's computer logged, sent by its Station Agent.

    ``adif`` is the program's own ADIF for the one QSO, exactly as it sent
    it (WSJT-X's "Logged ADIF" message, say); ``id`` is the one the Agent
    gave it in its journal, so a re-send never logs it twice (Principle XV).
    """

    id: Annotated[uuid.UUID, Field(strict=False)]
    adif: Annotated[str, StringConstraints(min_length=1, max_length=MAX_VALUE)]
    program: Annotated[str, StringConstraints(min_length=1, max_length=MAX_NAME)]


class AgentQsoOut(BaseModel):
    """What became of a QSO an Agent sent."""

    qso_id: uuid.UUID
    created: bool
    operator: str
    message: str


class LotwCertificateIn(_Strict):
    """A LoTW callsign certificate: TrustedQSL's ``.p12`` file, in base64, and its password."""

    p12_base64: Annotated[str, StringConstraints(min_length=1, max_length=MAX_P12_BASE64)]
    password: Annotated[str, StringConstraints(max_length=128)]


class LotwLoginIn(_Strict):
    """The operator's LoTW website username and password, for fetching confirmations."""

    username: Annotated[str, StringConstraints(min_length=1, max_length=64, strip_whitespace=True)]
    password: Annotated[str, StringConstraints(min_length=1, max_length=128)]


class LotwLocationModel(_Strict):
    """Where the station is, as LoTW records it with each upload.

    The call and DXCC entity come from the certificate. US stations name a
    state and may name a county; Canadian stations name a province.
    """

    grid: Annotated[str, StringConstraints(min_length=4, max_length=6, strip_whitespace=True)]
    ituz: Annotated[int, Field(ge=1, le=90)]
    cqz: Annotated[int, Field(ge=1, le=40)]
    us_state: Annotated[str, StringConstraints(max_length=2)] | None = None
    us_county: Annotated[str, StringConstraints(max_length=40)] | None = None
    ca_province: Annotated[str, StringConstraints(max_length=2)] | None = None


class LotwCertificateOut(BaseModel):
    """What the saved certificate says. Its key and password are never returned."""

    call: str
    dxcc: int
    entity: str
    qso_first: date
    qso_last: date | None
    expires: datetime


class LotwBatchQsoOut(BaseModel):
    """A QSO in a LoTW batch, as it was when the batch was prepared."""

    id: uuid.UUID
    version: int
    call: str | None
    qso_start: datetime | None
    band: str | None
    mode: str | None
    sent: bool


class LotwBatchOut(BaseModel):
    """A batch of QSOs for LoTW.

    ``state`` is ``ready`` (waiting for the operator's review), ``sent`` or
    ``cancelled``. LoTW numbers neither uploads nor QSOs, so ``id`` is
    LabLog's own batch number. ``qsos`` is listed only for one batch.
    """

    id: int
    station_call: str
    state: str
    prepared_at: datetime
    finished_at: datetime | None
    answer: str | None
    count: int
    sent: int
    qsos: list[LotwBatchQsoOut] | None = None


class LotwSetupOut(BaseModel):
    """The operator's LoTW set-up, how many of their QSOs wait, and any batch to review.

    ``problem`` says what stops sending, if anything is set up wrongly.
    """

    certificate: LotwCertificateOut | None
    username: str | None
    location: LotwLocationModel | None
    waiting: int
    qsl_since: str | None
    problem: str | None
    message: str
    batch: LotwBatchOut | None


class LotwSkippedOut(BaseModel):
    """A waiting QSO that was not sent, and why."""

    id: uuid.UUID
    call: str | None
    reason: str


class LotwPrepareOut(BaseModel):
    """What preparing a batch did; nothing is sent until the batch is.

    ``status`` is ``done``, ``nothing``, ``not_set_up`` or
    ``certificate_problem``.
    """

    status: str
    batch: LotwBatchOut | None
    skipped: list[LotwSkippedOut]
    remaining: int
    message: str


class LotwUploadOut(BaseModel):
    """What sending a batch did.

    ``status`` is ``done``, ``nothing``, ``not_set_up``, ``unreachable``,
    ``rejected`` or ``certificate_problem``.
    """

    status: str
    sent: int
    skipped: list[LotwSkippedOut]
    remaining: int
    message: str


class LotwDownloadOut(BaseModel):
    """What fetching confirmations did.

    ``status`` is ``done``, ``not_set_up``, ``unreachable`` or ``login_failed``.
    """

    status: str
    confirmed: int
    already: int
    unmatched: int
    message: str


class LotwPlaceOut(BaseModel):
    """A state or province a station location can name, with its (ITU, CQ) zones."""

    code: str
    name: str
    zones: list[tuple[int, int]]
    counties: list[str]


class LotwPlacesOut(BaseModel):
    """The places LoTW lists, for the station-location form."""

    us_states: list[LotwPlaceOut]
    ca_provinces: list[LotwPlaceOut]
    source: str


# -- Dependencies -------------------------------------------------------------


def _connection(request: Request) -> Iterator[psycopg.Connection[TupleRow]]:
    database: str = request.app.state.database
    with connect(database) as conn:
        yield conn


Conn = Annotated[psycopg.Connection["TupleRow"], Depends(_connection)]


def _actor(conn: Conn, authorization: Annotated[str | None, Header()] = None) -> Actor:
    scheme, _, token = (authorization or "").partition(" ")
    if scheme.lower() != "bearer" or not token.strip():
        raise HTTPException(
            status.HTTP_401_UNAUTHORIZED,
            "sign in first (Principle XIII)",
            headers={"WWW-Authenticate": "Bearer"},
        )
    return IdentityStore(conn).authenticate(token.strip())


Signed = Annotated[Actor, Depends(_actor)]


def _sync(request: Request) -> LogbookSync:
    sync: LogbookSync = request.app.state.logbook
    return sync


Sync = Annotated[LogbookSync, Depends(_sync)]


def _require(conn: psycopg.Connection[TupleRow], who: Actor, permission: Permission) -> None:
    IdentityStore(conn).require(who, permission)


def _session_out(conn: psycopg.Connection[TupleRow], who: Actor) -> SessionOut:
    identity = IdentityStore(conn)
    return SessionOut(
        token=None,
        operator=who.operator,
        station=who.station,
        permissions=[p.value for p in Permission if identity.may(who, p)],
        expires_at=None,
    )


class QsoQuery(_Strict):
    """Which QSOs to list. ``mine`` keeps only those the signed-in operator made."""

    model_config = ConfigDict(extra="forbid", strict=False, frozen=True)

    call: Annotated[str, StringConstraints(max_length=20)] | None = None
    limit: Annotated[int, Field(ge=1, le=MAX_LIST)] = 50
    include_deleted: bool = False
    newest_first: bool = True
    sort: SortKey = "entered"
    operation: uuid.UUID | None = None
    mine: bool = False


# -- Operations ---------------------------------------------------------------

api = APIRouter(prefix="/api/v1")


@api.get("/status")
def get_status() -> StatusOut:
    """Name, version, and source location. No sign-in needed."""
    return StatusOut(
        name="LabLog Engine",
        version=__version__,
        source=SOURCE,
        license="AGPL-3.0-only",
        api="v1",
    )


@api.post("/sessions", status_code=status.HTTP_201_CREATED)
def sign_in(body: SignIn, conn: Conn) -> SessionOut:
    """Sign in at a station. Send the token as ``Authorization: Bearer`` from then on."""
    session = IdentityStore(conn).sign_in(body.callsign, body.secret, body.station)
    out = _session_out(conn, session.actor)
    return out.model_copy(update={"token": session.token, "expires_at": session.expires_at})


@api.get("/sessions/current")
def whoami(who: Signed, conn: Conn) -> SessionOut:
    """Who is signed in with this token, and what they may do."""
    return _session_out(conn, who)


@api.delete("/sessions/current", status_code=status.HTTP_204_NO_CONTENT)
def sign_out(
    who: Signed, conn: Conn, board: Board, authorization: Annotated[str, Header()]
) -> None:
    """End this session, and take it off the On air list."""
    IdentityStore(conn).sign_out(authorization.partition(" ")[2].strip())
    board.leave(who.session_id)


# Boxes and tables the page no longer has: the FT8 box and FT8 waterfall,
# removed with LabLog's own FT8 decoder. A layout saved while they existed
# keeps everything else.
_RETIRED_MODULES: Final = frozenset({"ft8", "waterfall"})
_RETIRED_TABLES: Final = frozenset({"ft8"})


def _without_retired(saved: dict[str, object], *, overlays: bool = True) -> dict[str, object]:
    """``saved`` without retired boxes and tables, in its own arrangement and each overlay's."""
    kept = dict(saved)
    modules = kept.get("modules")
    if isinstance(modules, list):
        kept["modules"] = [
            m for m in modules if not (isinstance(m, dict) and m.get("id") in _RETIRED_MODULES)
        ]
    tables = kept.get("tables")
    if isinstance(tables, dict):
        kept["tables"] = {k: v for k, v in tables.items() if k not in _RETIRED_TABLES}
    by_overlay = kept.get("by_overlay")
    if overlays and isinstance(by_overlay, dict):
        kept["by_overlay"] = {
            k: _without_retired(v, overlays=False) if isinstance(v, dict) else v
            for k, v in by_overlay.items()
        }
    return kept


@api.get("/layout")
def get_layout(who: Signed, conn: Conn) -> LayoutOut:
    """The signed-in operator's workspace layout; it follows them to any workstation."""
    saved = WorkspaceStore(conn).layout(who)
    if saved is None:
        return LayoutOut(layout=None)
    try:
        return LayoutOut(layout=Layout.model_validate(_without_retired(saved), strict=False))
    except ValidationError:
        return LayoutOut(layout=None)  # saved by an older page: start from the default


@api.put("/layout")
def put_layout(body: Layout, who: Signed, conn: Conn) -> LayoutOut:
    """Save the signed-in operator's workspace layout."""
    WorkspaceStore(conn).save_layout(who, body.model_dump(mode="json"))
    return LayoutOut(layout=body)


@api.get("/qsos")
def list_qsos(who: Signed, conn: Conn, query: Annotated[QsoQuery, Query()]) -> list[QsoOut]:
    """Current QSOs, newest first by default.

    ``sort`` orders the whole log by a column (``call``, ``band``,
    ``qso_start``, ...) or any ADIF field (``field:FREQ``); ``newest_first``
    then means descending.
    """
    _require(conn, who, Permission.VIEW)
    found = LogStore(conn).find(
        QsoFilter(
            call=query.call or None,
            operation=query.operation,
            operator=who.operator if query.mine else None,
            include_deleted=query.include_deleted,
        ),
        limit=query.limit,
        newest_first=query.newest_first,
        sort=query.sort,
    )
    return [_qso_out(v) for v in found]


@api.get("/sections")
def list_sections(
    who: Signed,
    conn: Conn,
    operation: Annotated[uuid.UUID | None, Query()] = None,
) -> SectionsOut:
    """The ARRL and RAC sections, each with how many current QSOs are in it.

    This is what the Field Day section map colours in. Deleted QSOs do not
    count; with ``operation``, only the QSOs logged in that operation do.
    """
    _require(conn, who, Permission.VIEW)
    tally = tally_sections(LogStore(conn).field_counts("ARRL_SECT", operation))
    return SectionsOut(
        sections=[
            SectionOut(
                code=s.code,
                name=s.name,
                division=s.division,
                country=s.country,
                qsos=tally.worked.get(s.code, 0),
            )
            for s in SECTIONS
        ],
        worked=len(tally.worked),
        unrecognized=[ValueCount(value=v, qsos=n) for v, n in sorted(tally.unrecognized.items())],
        without_section=tally.without_section,
        source=SECTIONS_SOURCE,
    )


@api.get("/zones")
def list_zones(
    who: Signed,
    conn: Conn,
    kind: Annotated[ZoneKind, Query()],
    operation: Annotated[uuid.UUID | None, Query()] = None,
) -> ZonesOut:
    """The CQ or ITU zones, each with how many current QSOs are in it.

    This is what the zone maps colour in. Deleted QSOs do not count; with
    ``operation``, only the QSOs logged in that operation do.
    """
    _require(conn, who, Permission.VIEW)
    tally = tally_zones(kind, LogStore(conn).field_counts(ZONE_FIELDS[kind], operation))
    return ZonesOut(
        kind=kind,
        field=ZONE_FIELDS[kind],
        zones=[ZoneOut(zone=z, qsos=tally.worked.get(z, 0)) for z in range(1, ZONES[kind] + 1)],
        worked=len(tally.worked),
        unrecognized=[ValueCount(value=v, qsos=n) for v, n in sorted(tally.unrecognized.items())],
        without_zone=tally.without_zone,
    )


def _station_home(conn: Conn, who: Actor) -> MapPlace | None:
    """Where the operator's station is, when a QSO does not say."""
    account = LotwAccountStore(conn).get(who)
    grid = (account.location or {}).get("grid") if account is not None else None
    if not isinstance(grid, str):
        cached = CallbookStore(conn).cached(CALLBOOK_SOURCE, who.operator)
        grid = cached.entry.get("grid") if cached is not None and cached.found else None
    return grid_place(grid) if isinstance(grid, str) else None


def _gps_fix(request: Request) -> GpsFix | None:
    bus: FieldLabBus | None = request.app.state.fieldlab
    return None if bus is None else bus.fix()


@api.get("/paths")
def list_paths(
    who: Signed,
    conn: Conn,
    gps: Annotated[GpsFix | None, Depends(_gps_fix)],
    limit: Annotated[int, Query(ge=1, le=MAX_PATHS)] = 50,
    operation: Annotated[uuid.UUID | None, Query()] = None,
) -> PathsOut:
    """The latest ``limit`` QSOs by QSO time, each from the station to the other station.

    This is what the world map draws. Places come from the QSOs' own fields;
    nothing is looked up (Principle V). Deleted QSOs are left out; with
    ``operation``, only the QSOs logged in that operation count.
    """
    _require(conn, who, Permission.VIEW)
    latest = LogStore(conn).find(
        QsoFilter(operation=operation), limit=limit, newest_first=True, sort="qso_start"
    )
    fallback = _station_home(conn, who)
    paths: list[PathOut] = []
    home: MapPlace | None = None
    home_source: Literal["gps", "qso", "saved"] = "qso"
    without_place = without_home = 0
    for v in latest:
        other = record_place(v.record)
        station = record_place(v.record, mine=True) or fallback
        if other is None:
            without_place += 1
            continue
        if station is None:
            without_home += 1
            continue
        home = home or station
        paths.append(
            PathOut(
                id=v.qso_id,
                call=v.call,
                qso_start=v.qso_start,
                band=v.band,
                mode=v.mode,
                station=PlaceOut(lat=station.lat, lon=station.lon),
                other=PlaceOut(lat=other.lat, lon=other.lon),
                km=round(distance_km(station, other)),
            )
        )
    if gps is not None:
        home, home_source = MapPlace(lat=gps.lat, lon=gps.lon), "gps"
    elif home is None:
        home, home_source = fallback, "saved"
    return PathsOut(
        home=None if home is None else HomeOut(lat=home.lat, lon=home.lon, source=home_source),
        paths=paths,
        without_place=without_place,
        without_home=without_home,
    )


def _operation_out(conn: Conn, op: Operation) -> OperationOut:
    found = _find(conn, op.overlay, include_retired=True)
    return OperationOut(
        id=op.id,
        overlay=op.overlay,
        overlay_name=found.name if found is not None else op.overlay,
        name=op.name,
        station_call=op.station_call,
        gota_call=op.gota_call,
        sent=dict(op.sent),
        ended=op.ended,
        starts_at=op.starts_at,
        ends_at=op.ends_at,
        archived_at=op.archived_at,
        created_at=op.created_at,
        changed_at=op.changed_at,
        practice=op.practice,
    )


def _station(
    conn: Conn,
    who: Actor,
    ref: OperationRef,
    made: datetime | None = None,
    *,
    hours: bool = True,
) -> tuple[Overlay, Station, Operation]:
    """The overlay and station a QSO logged in operation ``ref`` is made under.

    With ``hours``, a QSO ``made`` outside the operation's hours is refused,
    and once the end time has passed the operation takes only QSOs made
    during its hours: one typed in a browser that could not reach the Engine
    until later is still logged (Principle XV).
    """
    op = OperationStore(conn).get(ref.id)
    if op.ended:
        raise HTTPException(status.HTTP_409_CONFLICT, f"the operation {op.name} has ended")
    if hours and made is not None and not op.open_at(made):
        raise HTTPException(status.HTTP_409_CONFLICT, _outside(op, made))
    if hours and made is None and op.over(datetime.now(UTC)):
        raise HTTPException(
            status.HTTP_409_CONFLICT, f"the operation {op.name} ended at {_utc(op.ends_at)}"
        )
    found = _find(conn, op.overlay)
    if found is None:
        raise HTTPException(
            status.HTTP_409_CONFLICT, f"the operation {op.name} uses an overlay that was retired"
        )
    if ref.gota and op.gota_call is None:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, f"the operation {op.name} has no GOTA call"
        )
    call = op.gota_call if ref.gota and op.gota_call else op.station_call
    station = Station(station_call=call, operator=who.operator, sent=op.sent, operation=str(op.id))
    return found, station, op


def _outside(op: Operation, made: datetime) -> str:
    return (
        f"a QSO at {_utc(made)} is outside {op.name}'s hours"
        f" ({_utc(op.starts_at)} to {_utc(op.ends_at)}), so it can't be logged in it"
    )


def _utc(when: datetime | None) -> str:
    return when.astimezone(UTC).strftime("%Y-%m-%d %H%M UTC") if when else "an open time"


def _made(fields: Mapping[str, str]) -> datetime | None:
    """When a QSO was made, from its ``QSO_DATE`` and ``TIME_ON``, if both are valid."""
    day = fields.get("QSO_DATE", "").strip()
    time = fields.get("TIME_ON", "").strip().replace(":", "")[:4]
    try:
        return datetime.strptime(f"{day}{time}", "%Y%m%d%H%M").replace(tzinfo=UTC)
    except ValueError:
        return None


def _setup(conn: Conn, body: OperationIn) -> OperationSetup:
    found = _overlay(conn, body.overlay)
    problems = sent_problems(found, body.sent)
    unknown = set(body.sent) - {p.key for p in (found.exchange.sent if found.exchange else ())}
    if problems or unknown:
        detail = " ".join(p.message for p in problems)
        if unknown:
            detail = f"{detail} {found.name} does not send {', '.join(sorted(unknown))}.".strip()
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, detail)
    if body.gota_call is not None and not found.gota:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, f"{found.name} has no GOTA station"
        )
    if body.gota_call is not None and body.gota_call.upper() == body.station_call.upper():
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "the GOTA call must differ from the station call (ARRL Field Day Rule 4.1.1.1)",
        )
    sent = {k: v.strip().upper() for k, v in body.sent.items()}
    return OperationSetup(
        overlay=found.id,
        name=body.name,
        station_call=body.station_call,
        gota_call=body.gota_call,
        sent=sent,
        ended=body.ended,
        starts_at=body.starts_at,
        ends_at=body.ends_at,
        practice=body.practice,
    )


def _may_set_up(conn: Conn, who: Actor, body: OperationIn) -> None:
    if IdentityStore(conn).may(who, Permission.ADMIN):
        return
    _require(conn, who, Permission.LOG)
    if body.station_call.upper() != who.operator or body.gota_call is not None:
        raise NotAuthorizedError(
            f"only an administrator sets up an operation under a call other than {who.operator}"
        )


@api.get("/operations")
def list_operations(
    who: Signed,
    conn: Conn,
    include_ended: Annotated[bool, Query()] = False,  # noqa: FBT002
) -> list[OperationOut]:
    """The operations operators can log in, newest first; ended ones on request."""
    _require(conn, who, Permission.VIEW)
    now = datetime.now(UTC)
    return [
        _operation_out(conn, op)
        for op in OperationStore(conn).list(include_ended=include_ended)
        if include_ended or not op.over(now)
    ]


@api.get("/operations/{operation_id}")
def get_operation(operation_id: uuid.UUID, who: Signed, conn: Conn) -> OperationOut:
    """One operation."""
    _require(conn, who, Permission.VIEW)
    return _operation_out(conn, OperationStore(conn).get(operation_id))


@api.get("/operations/{operation_id}/summary")
def summarize_operation(operation_id: uuid.UUID, who: Signed, conn: Conn) -> OperationSummaryOut:
    """How many QSOs an operation has logged, by band and mode group and by operator."""
    _require(conn, who, Permission.VIEW)
    op = OperationStore(conn).get(operation_id)
    found = _find(conn, op.overlay, include_retired=True)
    by_band_mode: Counter[tuple[str, str]] = Counter()
    by_operator: Counter[str] = Counter()
    qsos = outside = not_counted = 0
    for v in LogStore(conn).operation_qsos(op.id):
        qsos += 1
        fields = {f.name.upper(): f.value for f in reversed(v.record.fields)}
        by_operator[fields.get("OPERATOR", "").strip().upper() or v.operator] += 1
        if v.qso_start is not None and not op.open_at(v.qso_start):
            outside += 1
            continue
        band, group = (v.band or "").lower() or None, None
        if found is not None and found.bands is not None and found.modes is not None:
            band = found.bands.key(v.band, fields.get("PROP_MODE"))
            group = found.modes.group(fields.get("MODE"), fields.get("SUBMODE"))
            if band is None or group is None:
                not_counted += 1
                continue
        by_band_mode[(band or "", group or "")] += 1
    return OperationSummaryOut(
        operation=op.id,
        name=op.name,
        qsos=qsos,
        counted=qsos - outside - not_counted,
        outside=outside,
        not_counted=not_counted,
        by_band_mode=[
            TallyOut(band=b or None, mode_group=g or None, qsos=n)
            for (b, g), n in sorted(by_band_mode.items())
        ],
        by_operator=[TallyOut(operator=o, qsos=n) for o, n in by_operator.most_common()],
    )


@api.get("/operations/{operation_id}/stats")
def operation_stats_out(operation_id: uuid.UUID, who: Signed, conn: Conn) -> OperationStatsOut:
    """The whole group's stats in an operation: rate, hours, bands, operators, score.

    Every operating position asks for the same numbers, so they all agree
    (Principle II). Which stats a box shows is set by the overlay.
    """
    _require(conn, who, Permission.VIEW)
    op = OperationStore(conn).get(operation_id)
    found = _find(conn, op.overlay, include_retired=True)
    logged = [
        Logged(
            fields={f.name.upper(): f.value for f in reversed(v.record.fields)},
            qso_start=v.qso_start,
            operator=v.operator,
        )
        for v in LogStore(conn).operation_qsos(op.id)
    ]
    stats = operation_stats(
        found, logged, starts_at=op.starts_at, ends_at=op.ends_at, now=datetime.now(UTC)
    )
    return OperationStatsOut(
        operation=op.id,
        name=op.name,
        overlay_name=found.name if found is not None else op.overlay,
        show=list(found.shown_stats if found is not None else ("rate", "hourly", "operators")),
        now=stats.now,
        starts_at=op.starts_at,
        ends_at=op.ends_at,
        qsos=stats.qsos,
        counted=stats.counted,
        outside=stats.outside,
        not_counted=stats.not_counted,
        dupes=stats.dupes,
        last_hour=stats.last_hour,
        last_10=stats.last_10,
        rate=stats.last_10 * 6,
        hourly=[HourOut(start=h.start, qsos=h.qsos) for h in stats.hourly],
        best_hour=(
            HourOut(start=stats.best_hour.start, qsos=stats.best_hour.qsos)
            if stats.best_hour is not None
            else None
        ),
        by_band_mode=[
            BandModeOut(band=b.band, mode=b.mode, qsos=b.qsos, points=b.points)
            for b in stats.by_band_mode
        ],
        by_mode=dict(stats.by_mode),
        operators=[
            OperatorStatsOut(
                operator=o.operator, qsos=o.qsos, last_hour=o.last_hour, points=o.points
            )
            for o in stats.operators
        ],
        sections_worked=stats.sections_worked,
        sections_total=stats.sections_total,
        score=stats.score,
        points=dict(stats.points),
    )


ACTIVE: Final = timedelta(minutes=15)


def _on_air(request: Request) -> OnAir:
    board: OnAir = request.app.state.on_air
    return board


Board = Annotated[OnAir, Depends(_on_air)]


def _rigs(request: Request) -> RigBoard:
    rigs: RigBoard = request.app.state.rig
    return rigs


Rigs = Annotated[RigBoard, Depends(_rigs)]


def _from_radio(report: Report, radio: Radio) -> Report:
    """A workstation's line with the band, and the mode unless it is data, from its radio."""
    adif = adif_mode(radio.mode)
    mode, submode = adif if adif is not None else (report.mode, report.submode)
    return replace(report, band=band_for(radio.frequency_hz), mode=mode, submode=submode)


def _on_air_out(
    conn: Conn, who: Actor, board: OnAir, rigs: RigBoard, op: Operation
) -> OnAirListOut:
    now = datetime.now(UTC)
    found = _find(conn, op.overlay, include_retired=True)
    last: dict[str, datetime] = {}
    for v in LogStore(conn).operation_qsos(op.id):
        name = (v.record.get("OPERATOR") or v.operator).strip().upper()
        if v.qso_start is not None and (name not in last or v.qso_start > last[name]):
            last[name] = v.qso_start
    entered = board.current(op.id, now)
    radios: dict[str, Radio] = {}
    for r in entered:
        live = rigs.current(r.station, now)
        if live is not None and live.radio is not None:
            radios[r.station] = live.radio
    reports = [_from_radio(r, radios[r.station]) if r.station in radios else r for r in entered]

    def group(r: Report) -> str | None:
        if found is None or found.modes is None:
            return None
        return found.modes.group(r.mode, r.submode)

    def slot(r: Report) -> tuple[str, str, str] | None:
        g = group(r)
        return (r.call, r.band, g) if r.band and g else None

    slots = Counter(s for s in map(slot, reports) if s is not None)
    return OnAirListOut(
        operation=op.id,
        name=op.name,
        active_minutes=int(ACTIVE.total_seconds() // 60),
        stations=[
            OnAirOut(
                operator=r.operator,
                station=r.station,
                call=r.call,
                band=r.band,
                mode=r.submode or r.mode,
                mode_group=group(r),
                seen_at=r.seen_at,
                last_qso=last.get(r.operator),
                active=r.operator in last and now - last[r.operator] <= ACTIVE,
                clash=(s := slot(r)) is not None and slots[s] > 1,
                you=r.session_id == who.session_id,
                from_radio=r.station in radios,
                frequency_hz=radios[r.station].frequency_hz if r.station in radios else None,
                transmitting=radios[r.station].ptt if r.station in radios else None,
                pretend=r.pretend,
            )
            for r in reports
        ],
    )


@api.get("/operations/{operation_id}/on-air")
def list_on_air(
    operation_id: uuid.UUID, who: Signed, conn: Conn, board: Board, rigs: Rigs
) -> OnAirListOut:
    """The workstations on the air in an operation now, with their band and mode."""
    _require(conn, who, Permission.VIEW)
    return _on_air_out(conn, who, board, rigs, OperationStore(conn).get(operation_id))


@api.put("/operations/{operation_id}/on-air")
def report_on_air(  # noqa: PLR0913 - the board and the rigs are both live state
    operation_id: uuid.UUID,
    body: OnAirIn,
    *,
    who: Signed,
    conn: Conn,
    board: Board,
    rigs: Rigs,
) -> OnAirListOut:
    """Say what this workstation is set to now; the answer is who is on the air.

    A browser sends this every so often while it is in the operation. Live
    state only: nothing is written to the log.
    """
    _require(conn, who, Permission.LOG)
    op = OperationStore(conn).get(operation_id)
    if op.ended or op.over(datetime.now(UTC)):
        board.leave(who.session_id)
        raise HTTPException(status.HTTP_409_CONFLICT, f"the operation {op.name} has ended")
    call = op.gota_call if body.gota and op.gota_call else op.station_call
    board.report(
        Report(
            session_id=who.session_id,
            operation=op.id,
            operator=who.operator,
            station=who.station,
            call=call,
            band=body.band,
            mode=body.mode,
            submode=body.submode,
            seen_at=datetime.now(UTC),
        )
    )
    return _on_air_out(conn, who, board, rigs, op)


@api.delete("/operations/{operation_id}/on-air", status_code=status.HTTP_204_NO_CONTENT)
def leave_on_air(operation_id: uuid.UUID, who: Signed, board: Board) -> None:
    """Take this workstation off the On air list, as when it leaves the operation."""
    del operation_id  # a session is on the air in one operation at a time
    board.leave(who.session_id)


# -- Demo mode -----------------------------------------------------------------


def _demo(request: Request) -> Demo:
    demo: Demo = request.app.state.demo
    return demo


DemoMode = Annotated[Demo, Depends(_demo)]


def _demo_out(demo: Demo) -> DemoOut:
    state = demo.state()
    return DemoOut(
        on=state.on,
        operation=state.operation,
        operation_name=state.operation_name,
        started_by=state.started_by,
        started_at=state.started_at,
        qsos=state.qsos,
        stations=list(state.stations),
        problem=state.problem,
    )


def _demo_operation(conn: Conn, who: Actor) -> Operation:
    """The open practice operation demo mode logs in, set up now if there is none."""
    operations = OperationStore(conn)
    for op in operations.list():
        if op.practice and op.name == DEMO_NAME and op.archived_at is None:
            return op
    found = LicenseStore(conn).find(who.operator)
    section = section_for(found.state) if found is not None and found.state else None
    return operations.create(
        who,
        OperationSetup(
            overlay="arrl-fd",
            name=DEMO_NAME,
            station_call=who.operator,
            gota_call=None,
            sent={"class": "3A", "section": section.code if section is not None else "DX"},
            practice=True,
        ),
    )


@api.get("/demo")
def get_demo(who: Signed, conn: Conn, demo: DemoMode) -> DemoOut:
    """Whether demo mode is on, and where."""
    _require(conn, who, Permission.VIEW)
    return _demo_out(demo)


@api.put("/demo")
def set_demo(body: DemoIn, who: Signed, conn: Conn, demo: DemoMode) -> DemoOut:
    """Turn demo mode on or off (administrators).

    On, pretend operators Q1PRE, Q2PRE and Q3PRE go on the air at stations
    demo-1 to demo-3 with pretend radios, in the practice operation Demo
    Field Day (set up now if need be), and log pretend QSOs with real calls
    from the license lists, each marked pretend, in this administrator's
    session (Principles XIII and XX). Off, they leave the air and every pretend QSO in Demo
    Field Day is erased, the erasure recorded (Principle VII, as amended in
    Constitution 2.1.0). Nothing ever goes to LoTW or QRZ.com.
    """
    _require(conn, who, Permission.ADMIN)
    if not body.on:
        demo.stop()
        store = LogStore(conn)
        erased = sum(
            store.erase_pretend(op.id, actor=who)
            for op in OperationStore(conn).list(include_ended=True)
            if op.practice and op.name == DEMO_NAME
        )
        return _demo_out(demo).model_copy(update={"erased": erased})
    op = _demo_operation(conn, who)
    found = _find(conn, op.overlay, include_retired=True)
    if found is None:
        raise HTTPException(status.HTTP_409_CONFLICT, f"the overlay {op.overlay} is missing")
    demo.start(who, op, found)
    return _demo_out(demo)


# -- The station's radio, through its Station Agent ---------------------------

# How often a stream re-checks that its session is still live.
_RECHECK_SECONDS: Final = 60.0
# How often a stream says something even when nothing has changed, so a
# browser can tell a quiet radio from a lost connection.
_KEEPALIVE_SECONDS: Final = 10.0
_STREAM_POLL_SECONDS: Final = 0.2


def _agent_station(conn: Conn, authorization: Annotated[str | None, Header()] = None) -> str:
    scheme, _, key = (authorization or "").partition(" ")
    if scheme.lower() != "bearer" or not key.strip():
        raise HTTPException(
            status.HTTP_401_UNAUTHORIZED,
            "send this station's key (Principle XIII)",
            headers={"WWW-Authenticate": "Bearer"},
        )
    return AgentKeyStore(conn).authenticate(key.strip())


AgentStation = Annotated[str, Depends(_agent_station)]


def _radio_path(path: RadioPathIn) -> str:
    if path.kind == "pretend":
        return "pretend"
    if path.kind == "flex":
        return "FlexRadio API"
    return f"Hamlib model {path.model}" if path.model is not None else "Hamlib"


def _rig_out(station: str, state: RigState | None) -> RigOut:
    if state is None:
        return RigOut(
            station=station, heard=False, agent=None, radio=None, problem=None, heard_at=None
        )
    radio = state.radio
    out = None
    if radio is not None:
        adif = adif_mode(radio.mode)
        hz = radio.frequency_hz
        out = RadioOut(
            maker=radio.maker,
            model=radio.model,
            path=radio.path,
            frequency_hz=hz,
            mhz=f"{hz // 1_000_000}.{hz % 1_000_000:06d}",
            band=band_for(hz),
            mode=radio.mode,
            adif_mode=adif[0] if adif else None,
            adif_submode=adif[1] if adif else None,
            passband_hz=radio.passband_hz,
            transmitting=radio.ptt,
            vfo=radio.vfo,
        )
    return RigOut(
        station=station,
        heard=True,
        agent=state.agent,
        radio=out,
        problem=state.problem,
        heard_at=state.heard_at,
    )


@api.put("/agent/rig", status_code=status.HTTP_204_NO_CONTENT)
def report_rig(body: AgentReportIn, station: AgentStation, rigs: Rigs) -> None:
    """A Station Agent reports its radio's live state, with its station key.

    The Agent sends this whenever the radio reports a change, and every few
    seconds otherwise. Live state only: nothing is written to the log, and
    nothing goes back to the radio (Principle X).
    """
    radio = body.radio
    rigs.report(
        RigState(
            station=station,
            agent=body.agent,
            radio=None
            if radio is None
            else Radio(
                maker=radio.maker,
                model=radio.model,
                path=_radio_path(radio.path),
                frequency_hz=radio.frequency_hz,
                mode=radio.mode,
                passband_hz=radio.passband_hz,
                ptt=radio.ptt,
                vfo=radio.vfo,
            ),
            problem=body.problem if radio is None else None,
            heard_at=datetime.now(UTC),
        )
    )


def _chosen_operation(conn: Conn, who: Actor) -> OperationRef | None:
    """The operation the operator is logging in now, as their saved layout says."""
    layout = WorkspaceStore(conn).layout(who) or {}
    chosen = layout.get("operation")
    if not isinstance(chosen, dict):
        return None
    try:
        return OperationRef.model_validate(chosen)
    except ValidationError:
        return None


@api.post("/agent/qsos", status_code=status.HTTP_201_CREATED)
def agent_log_qso(
    body: AgentQsoIn, station: AgentStation, conn: Conn, response: Response, sync: Sync
) -> AgentQsoOut:
    """Log a QSO a program at the Agent's station logged, as the operator signed in there.

    The QSO is logged exactly as if that operator had typed it at that
    station: in the operation they chose, with its fields and its hours
    (Principle XIII). With nobody signed in at the station, it is refused
    with 503, and the Agent keeps it in its journal until someone signs in
    (Principle XV). A QSO already in the log, the same call, band and mode
    in the same UTC minute, is answered with that QSO, never logged twice.
    """
    who = IdentityStore(conn).at_station(station)
    if who is None:
        raise HTTPException(
            status.HTTP_503_SERVICE_UNAVAILABLE,
            f"nobody is signed in to LabLog at {station}",
        )
    if not IdentityStore(conn).may(who, Permission.LOG):
        raise HTTPException(
            status.HTTP_409_CONFLICT, f"{who.operator}, signed in at {station}, may not log"
        )
    try:
        parsed = parse_adi(body.adif.encode())
    except AdiParseError as error:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            f"{body.program} sent ADIF LabLog can't read: {error}",
        ) from error
    if len(parsed.document.records) != 1:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            f"{body.program} sent {len(parsed.document.records)} QSOs; one at a time, please",
        )
    record = parsed.document.records[0]
    ref = _chosen_operation(conn, who)
    if ref is not None:
        made = _made({f.name.upper(): f.value for f in reversed(record.fields)})
        found, station_fields, _ = _station(conn, who, ref, made)
        record = stamp(record, found, station_fields)
    store = LogStore(conn)
    match = store.already_logged([record])[0]
    if match is not None and match.qso_id != body.id:
        response.status_code = status.HTTP_200_OK
        return AgentQsoOut(
            qso_id=match.qso_id,
            created=False,
            operator=who.operator,
            message=f"{record.get('CALL') or 'That QSO'} is already in the log; nothing was added.",
        )
    note = f"Logged in {body.program} at {station}; sent by its Station Agent."
    version, created = store.enter_qso(record, actor=who, qso_id=body.id, note=note)
    if not created:
        response.status_code = status.HTTP_200_OK
    sync.wake()
    return AgentQsoOut(
        qso_id=version.qso_id,
        created=created,
        operator=who.operator,
        message=_written(version, created=created).message,
    )


def _desk(request: Request) -> PairingDesk:
    desk: PairingDesk = request.app.state.pairings
    return desk


Desk = Annotated[PairingDesk, Depends(_desk)]


@api.post("/agent/pairings", status_code=status.HTTP_201_CREATED)
def ask_to_pair(body: PairingAskIn, desk: Desk) -> PairingAskOut:
    """A Station Agent with no key asks to join as a station. No sign-in needed.

    The request grants nothing: an administrator must approve it, after
    checking that the code the Agent shows matches (Principle XIII).
    """
    now = datetime.now(UTC)
    try:
        pairing, ticket = desk.ask(body.station, body.agent, now)
    except PairingError as error:
        raise HTTPException(status.HTTP_429_TOO_MANY_REQUESTS, str(error)) from error
    return PairingAskOut(
        id=pairing.id, code=pairing.code, ticket=ticket, expires_at=pairing.asked_at + LIFETIME
    )


@api.get("/agent/pairings/{pairing_id}")
def pairing_state(
    pairing_id: uuid.UUID, desk: Desk, authorization: Annotated[str | None, Header()] = None
) -> PairingStateOut:
    """How an Agent's request stands, asked with its ticket; the key comes once."""
    scheme, _, ticket = (authorization or "").partition(" ")
    if scheme.lower() != "bearer" or not ticket.strip():
        raise HTTPException(status.HTTP_401_UNAUTHORIZED, "send the request's ticket")
    state, key = desk.collect(pairing_id, ticket.strip(), datetime.now(UTC))
    return PairingStateOut(state=state, key=key)


def _pairing_out(pairing: Pairing, keyed: set[str]) -> PairingOut:
    return PairingOut(
        id=pairing.id,
        station=pairing.station,
        agent=pairing.agent,
        code=pairing.code,
        asked_at=pairing.asked_at,
        replaces=pairing.station in keyed,
    )


@api.get("/agent/pairings")
def list_pairings(who: Signed, conn: Conn, desk: Desk) -> list[PairingOut]:
    """The Agents waiting to join, for an administrator to approve or refuse."""
    _require(conn, who, Permission.ADMIN)
    keyed = set(AgentKeyStore(conn).stations())
    return [_pairing_out(p, keyed) for p in desk.waiting(datetime.now(UTC))]


@api.post("/agent/pairings/{pairing_id}/approve", status_code=status.HTTP_204_NO_CONTENT)
def approve_pairing(pairing_id: uuid.UUID, who: Signed, conn: Conn, desk: Desk) -> None:
    """Let the Agent join: make its station's key, which the Agent then collects.

    Any key the station had before stops working.
    """
    _require(conn, who, Permission.ADMIN)
    now = datetime.now(UTC)
    try:
        pairing = desk.find(pairing_id, now)
        key = AgentKeyStore(conn).make(who, pairing.station)
        desk.decide(pairing_id, who.operator, key, now)
    except PairingError as error:
        raise HTTPException(status.HTTP_409_CONFLICT, str(error)) from error


@api.post("/agent/pairings/{pairing_id}/refuse", status_code=status.HTTP_204_NO_CONTENT)
def refuse_pairing(pairing_id: uuid.UUID, who: Signed, conn: Conn, desk: Desk) -> None:
    """Turn an Agent's request down; it gets no key."""
    _require(conn, who, Permission.ADMIN)
    try:
        desk.decide(pairing_id, who.operator, None, datetime.now(UTC))
    except PairingError as error:
        raise HTTPException(status.HTTP_409_CONFLICT, str(error)) from error


@api.get("/agent/keys")
def list_agent_keys(who: Signed, conn: Conn) -> AgentKeysOut:
    """The stations whose Agents have a working key."""
    _require(conn, who, Permission.ADMIN)
    return AgentKeysOut(stations=list(AgentKeyStore(conn).stations()))


@api.delete("/agent/keys/{station}", status_code=status.HTTP_204_NO_CONTENT)
def end_agent_key(station: str, who: Signed, conn: Conn) -> None:
    """End a station's key, so its Agent is refused until it joins again."""
    _require(conn, who, Permission.ADMIN)
    AgentKeyStore(conn).end(who, station)


@api.get("/rig")
def get_rig(who: Signed, conn: Conn, rigs: Rigs) -> RigOut:
    """The radio at the signed-in operator's station, as its Agent last reported it."""
    _require(conn, who, Permission.VIEW)
    return _rig_out(who.station, rigs.current(who.station, datetime.now(UTC)))


@api.get("/rig/stream", response_model=None)
async def stream_rig(
    request: Request,
    max_seconds: Annotated[int, Query(ge=1, le=3600)] = 600,
    authorization: Annotated[str | None, Header()] = None,
) -> StreamingResponse:
    """The radio at the signed-in operator's station, live: one JSON line per change.

    Each line is shaped like ``GET /rig``. A line also comes every few seconds
    when nothing changes. The stream ends after ``max_seconds`` or when the
    session ends; a client then opens a new one.
    """
    database: str = request.app.state.database
    rigs: RigBoard = request.app.state.rig

    def check() -> Actor:
        with connect(database) as conn:
            who = _actor(conn, authorization)
            _require(conn, who, Permission.VIEW)
            return who

    who = await run_in_threadpool(check)

    async def lines() -> AsyncIterator[bytes]:
        started = checked = time.monotonic()
        seen: tuple[int, bool] | None = None
        said = 0.0
        while time.monotonic() - started < max_seconds:
            if time.monotonic() - checked >= _RECHECK_SECONDS:
                try:
                    await run_in_threadpool(check)
                except (StoreError, HTTPException):
                    return
                checked = time.monotonic()
            state = rigs.current(who.station, datetime.now(UTC))
            now_seen = (rigs.version(who.station), state is None)
            if now_seen != seen or time.monotonic() - said >= _KEEPALIVE_SECONDS:
                yield (_rig_out(who.station, state).model_dump_json() + "\n").encode()
                seen, said = now_seen, time.monotonic()
            if await request.is_disconnected():
                return
            await asyncio.sleep(_STREAM_POLL_SECONDS)

    return StreamingResponse(lines(), media_type="application/x-ndjson")


@api.post("/operations", status_code=status.HTTP_201_CREATED)
def create_operation(body: OperationIn, who: Signed, conn: Conn) -> OperationOut:
    """Set up an operation, such as Field Day under a club call.

    Under a club call (any call but your own, or with a GOTA call), only an
    administrator can; under your own call, anyone who may log.
    """
    _may_set_up(conn, who, body)
    return _operation_out(conn, OperationStore(conn).create(who, _setup(conn, body)))


def _may_change(conn: Conn, who: Actor, op: Operation) -> None:
    if IdentityStore(conn).may(who, Permission.ADMIN):
        return
    _require(conn, who, Permission.LOG)
    if op.station_call != who.operator or op.gota_call is not None:
        raise NotAuthorizedError("only an administrator changes a club's operation")


@api.put("/operations/{operation_id}")
def update_operation(
    operation_id: uuid.UUID, body: OperationIn, who: Signed, conn: Conn
) -> OperationOut:
    """Change an operation, ended or not, or end it with ``ended``.

    QSOs already logged keep their fields. An archived operation can't be changed.
    """
    _may_set_up(conn, who, body)
    _may_change(conn, who, OperationStore(conn).get(operation_id))
    return _operation_out(conn, OperationStore(conn).update(who, operation_id, _setup(conn, body)))


@api.post("/operations/{operation_id}/archive")
def archive_operation(operation_id: uuid.UUID, who: Signed, conn: Conn) -> OperationOut:
    """End an operation and archive it, for good, once its log has been submitted.

    From then on neither the operation nor any QSO logged in it can be
    changed, so the log always matches what was sent in.
    """
    store = OperationStore(conn)
    _may_change(conn, who, store.get(operation_id))
    return _operation_out(conn, store.archive(who, operation_id))


def _catalog(conn: Conn) -> list[OverlayOut]:
    """Every overlay: the Engine's own, then those made in the editor, retired ones too."""
    out = [OverlayOut.model_validate({**o.model_dump(), "builtin": True}) for o in overlays()]
    shipped = {o.id for o in out}
    for saved in OverlayStore(conn).current():
        if saved.overlay_id in shipped:
            continue  # a built-in overlay always wins its id
        definition = saved.definition
        if definition is None:
            previous = OverlayStore(conn).latest_definition(saved.overlay_id)
            if previous is None:
                continue
            definition = previous
        try:
            found = Overlay.model_validate(definition, strict=False)
        except ValidationError:
            continue  # saved under rules this Engine no longer accepts
        out.append(
            OverlayOut.model_validate(
                {
                    **found.model_dump(),
                    "builtin": False,
                    "version": saved.version,
                    "retired": saved.retired,
                    "saved_by": saved.saved_by,
                    "saved_at": saved.saved_at,
                }
            )
        )
    out.sort(key=lambda o: (o.id != EVERYDAY, o.retired, o.is_contest, o.name.lower()))
    return out


def _find(conn: Conn, overlay_id: str, *, include_retired: bool = False) -> OverlayOut | None:
    return next(
        (o for o in _catalog(conn) if o.id == overlay_id and (include_retired or not o.retired)),
        None,
    )


def _overlay(conn: Conn, overlay_id: str) -> OverlayOut:
    found = _find(conn, overlay_id)
    if found is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"there is no overlay called {overlay_id}")
    return found


@api.get("/overlays")
def list_overlays(
    who: Signed,
    conn: Conn,
    include_retired: Annotated[bool, Query()] = False,  # noqa: FBT002
) -> list[OverlayOut]:
    """Every operation overlay: Everyday first, then the others (Principle III).

    ``builtin`` overlays ship with the Engine and cannot be changed, only copied.
    """
    _require(conn, who, Permission.VIEW)
    return [o for o in _catalog(conn) if include_retired or not o.retired]


@api.get("/overlays/{overlay_id}")
def get_overlay(overlay_id: str, who: Signed, conn: Conn) -> OverlayOut:
    """One operation overlay, retired or not."""
    _require(conn, who, Permission.VIEW)
    found = _find(conn, overlay_id, include_retired=True)
    if found is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"there is no overlay called {overlay_id}")
    return found


def _editable(conn: Conn, overlay_id: str) -> OverlayOut | None:
    """The editor's overlay ``overlay_id`` as it stands, refusing a built-in one."""
    found = _find(conn, overlay_id, include_retired=True)
    if found is not None and found.builtin:
        raise HTTPException(
            status.HTTP_409_CONFLICT,
            f"{found.name} is built in and cannot be changed; save a copy under another name",
        )
    return found


@api.put("/overlays/{overlay_id}")
def save_overlay(overlay_id: str, body: OverlaySave, who: Signed, conn: Conn) -> OverlayOut:
    """Save an overlay made in the editor, as a new version. Administrators only.

    ``base_version`` is the version it was opened at (0 for a new overlay); a
    save over someone else's newer one is refused. Built-in overlays are
    refused: copy one under a new id instead.
    """
    _require(conn, who, Permission.ADMIN)
    if body.definition.id != overlay_id:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, "the overlay's id must match its address"
        )
    current = _editable(conn, overlay_id)
    name = body.definition.name.strip().lower()
    if any(
        o.id != overlay_id and not o.retired and o.name.strip().lower() == name
        for o in _catalog(conn)
    ):
        raise HTTPException(
            status.HTTP_409_CONFLICT,
            f"another overlay is already called {body.definition.name}; give this one its own name",
        )
    if (current.version if current else 0) != body.base_version:
        raise StaleVersionError(
            f"the overlay {overlay_id} was changed by someone else; open it again"
        )
    OverlayStore(conn).save(
        who, overlay_id, body.base_version + 1, body.definition.model_dump(mode="json")
    )
    return _overlay(conn, overlay_id)


@api.delete("/overlays/{overlay_id}")
def retire_overlay(
    overlay_id: str,
    who: Signed,
    conn: Conn,
    base_version: Annotated[int, Query(ge=1)],
) -> OverlayOut:
    """Retire an overlay made in the editor. Its versions are kept; saving it again brings it back.

    An overlay an operation still uses can't be retired until that operation ends.
    """
    _require(conn, who, Permission.ADMIN)
    current = _editable(conn, overlay_id)
    if current is None or current.retired:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"there is no overlay called {overlay_id}")
    if current.version != base_version:
        raise StaleVersionError(
            f"the overlay {overlay_id} was changed by someone else; open it again"
        )
    using = [op.name for op in OperationStore(conn).list() if op.overlay == overlay_id]
    if using:
        raise HTTPException(
            status.HTTP_409_CONFLICT,
            f"{current.name} is in use by {', '.join(using)}; end that operation first",
        )
    OverlayStore(conn).save(who, overlay_id, base_version + 1, None)
    found = _find(conn, overlay_id, include_retired=True)
    assert found is not None  # noqa: S101 - just saved
    return found


def _year(qso_date: str | None) -> int:
    try:
        return datetime.strptime((qso_date or "").strip(), "%Y%m%d").replace(tzinfo=UTC).year
    except ValueError:
        return datetime.now(UTC).year


def _worked_out(worked: Iterable[Worked]) -> list[WorkedOut]:
    return [
        WorkedOut(
            id=w.qso_id, band=w.band, mode_group=w.group, qso_start=w.qso_start, operator=w.operator
        )
        for w in worked
    ]


@api.post("/overlays/{overlay_id}/check")
def check_qso(overlay_id: str, body: ContestCheckIn, who: Signed, conn: Conn) -> ContestCheckOut:
    """Check a QSO for dupes and its exchange under a contest overlay's rules.

    Two QSOs are dupes when they are with the same call, made as the same
    station callsign, carry the overlay's ``CONTEST_ID``, fall in the same
    UTC year, and agree on whatever the overlay's dupe rule lists (for both
    Field Days, the band and the mode group). Nothing is stored.
    """
    _require(conn, who, Permission.VIEW)
    found = _overlay(conn, overlay_id)
    fields: dict[str, str] = {}
    for f in body.fields:
        fields.setdefault(f.name.upper(), f.value)
    op: Operation | None = None
    if body.operation is not None:
        in_operation, at, op = _station(conn, who, body.operation, hours=False)
        if in_operation.id != found.id:
            raise HTTPException(
                status.HTTP_422_UNPROCESSABLE_CONTENT,
                f"that operation uses the {in_operation.name} overlay, not {found.name}",
            )
        for name, value in stamp_fields(found, at):
            fields.setdefault(name, value)
    station = (
        body.station_call
        or fields.get("STATION_CALLSIGN", "").strip()
        or fields.get("OPERATOR", "").strip()
        or who.operator
    ).upper()
    year = _year(fields.get("QSO_DATE"))
    earlier: tuple[QsoVersion, ...] = ()
    call = fields.get("CALL", "").strip()
    if found.contest_id is not None and call:
        earlier = LogStore(conn).contest_qsos(
            contest_id=found.contest_id,
            station_call=station,
            call=call,
            year=year,
            exclude=body.exclude,
        )
    result = contest_check(
        found,
        fields,
        (
            Earlier(
                qso_id=v.qso_id,
                fields={f.name.upper(): f.value for f in reversed(v.record.fields)},
                qso_start=v.qso_start,
                operator=v.operator,
            )
            for v in earlier
        ),
    )
    status_, message = result.status, result.message
    made = _made(fields)
    if op is not None and made is not None and not op.open_at(made) and status_ != "incomplete":
        status_ = "not_counted"
        outside = _outside(op, made)
        message = f"{outside[:1].upper()}{outside[1:]}."
    return ContestCheckOut(
        overlay=found.id,
        status=status_,
        station_call=station,
        year=year,
        band=result.band,
        mode_group=result.group,
        dupes=_worked_out(result.dupes),
        worked=_worked_out(result.worked),
        problems=[
            ProblemOut(field=p.field, label=p.label, message=p.message) for p in result.problems
        ],
        message=message,
    )


@api.get("/qsos/{qso_id}")
def get_qso(qso_id: uuid.UUID, who: Signed, conn: Conn) -> QsoOut:
    """A QSO as it stands now (it may be deleted)."""
    _require(conn, who, Permission.VIEW)
    return _qso_out(LogStore(conn).current(qso_id))


@api.get("/qsos/{qso_id}/history")
def get_history(qso_id: uuid.UUID, who: Signed, conn: Conn) -> list[QsoOut]:
    """Every version of a QSO, oldest first (Principle VII)."""
    _require(conn, who, Permission.VIEW)
    return [_qso_out(v) for v in LogStore(conn).history(qso_id)]


@api.post("/qsos", status_code=status.HTTP_201_CREATED)
def create_qso(
    body: QsoCreate, who: Signed, conn: Conn, response: Response, sync: Sync
) -> QsoWritten:
    """Log a new QSO, under the ID its client gave it if any (Principle XV).

    Sending a QSO again with the same ID and fields returns it as it stands
    (200, ``created`` false) instead of logging it twice.
    """
    _require(conn, who, Permission.LOG)
    store = LogStore(conn)
    record = _record(body.fields)
    if body.operation is not None:
        made = _made({f.name.upper(): f.value for f in reversed(record.fields)})
        found, station, _ = _station(conn, who, body.operation, made)
        record = stamp(record, found, station)
    if body.id is None:
        version, created = store.create_qso(record, actor=who, note=body.note), True
    else:
        version, created = store.enter_qso(record, actor=who, qso_id=body.id, note=body.note)
    if not created:
        response.status_code = status.HTTP_200_OK
    sync.wake()
    return _written(version, created=created)


def _contact(record: Record) -> tuple[str, datetime, str, str] | None:
    """What makes two records the same contact (as :meth:`LogStore.already_logged` decides)."""
    fields = {f.name.upper(): f.value for f in reversed(record.fields)}
    call = fields.get("CALL", "").strip().upper()
    made = _made(fields)
    if not call or made is None:
        return None
    return (
        call,
        made,
        fields.get("BAND", "").strip().lower(),
        fields.get("MODE", "").strip().upper(),
    )


def _in_operation(
    conn: Conn, who: Actor, ref: OperationRef, records: list[Record], out: AdifImportOut
) -> list[Record]:
    """The records made during the operation's hours, each with the operation's fields added."""
    found, station, op = _station(conn, who, ref, hours=False)
    kept: list[Record] = []
    for record in records:
        made = _made({f.name.upper(): f.value for f in reversed(record.fields)})
        if made is None:
            out.no_time += 1
        elif not op.open_at(made):
            out.outside_hours += 1
        else:
            kept.append(stamp(record, found, station))
    return kept


def _not_yet_logged(store: LogStore, records: list[Record], out: AdifImportOut) -> list[Record]:
    """The records that are not in the log yet, each contact once."""
    new: list[Record] = []
    seen: set[tuple[str, datetime, str, str]] = set()
    for record, match in zip(records, store.already_logged(records), strict=True):
        if match is not None:
            if match.deleted:
                out.already_deleted += 1
            else:
                out.already_logged += 1
            continue
        contact = _contact(record)
        if contact is not None:
            if contact in seen:
                out.repeated += 1
                continue
            seen.add(contact)
        new.append(record)
    return new


@api.post("/imports", status_code=status.HTTP_201_CREATED)
def import_adif(
    body: AdifImportIn, who: Signed, conn: Conn, response: Response, sync: Sync
) -> AdifImportOut:
    """Take in the QSOs of an ADIF file that the log does not already have.

    A QSO already in the log (the same call, band and mode in the same UTC
    minute) is left alone, so a program's log file can be taken in again and
    again and only its new QSOs are added (Principle XV). One that was
    deleted stays deleted (Principle VII). Every field of every QSO taken in
    is kept as the file has it (Principle IV); the import as a whole is one
    audited change (Principle VII), all or nothing (Principle XIV).
    """
    _require(conn, who, Permission.LOG)
    try:
        data = base64.b64decode(body.data_base64, validate=True)
    except binascii.Error as error:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, "the file is not base64"
        ) from error
    try:
        parsed = parse_adi(data)
    except AdiParseError as error:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            f"{body.file_name} is not an ADIF file LabLog can read: {error}",
        ) from error
    warnings = [
        f"{'header' if w.record is None else f'QSO {w.record}'}"
        f"{f' {w.field}' if w.field else ''}: {w.message}"
        for w in parsed.warnings[:MAX_IMPORT_WARNINGS]
    ]
    records = list(parsed.document.records)
    out = AdifImportOut(
        file_name=body.file_name,
        records=len(records),
        import_id=None,
        added=0,
        already_logged=0,
        already_deleted=0,
        repeated=0,
        outside_hours=0,
        no_time=0,
        already_imported=None,
        warnings=warnings,
    )
    sha256 = hashlib.sha256(data).hexdigest()
    store = LogStore(conn)
    with conn.transaction():
        out.already_imported = store.import_of_file(sha256)
        if out.already_imported is not None:
            response.status_code = status.HTTP_200_OK
            return out
        if body.operation is not None:
            records = _in_operation(conn, who, body.operation, records, out)
        new = _not_yet_logged(store, records, out)
        if new:
            try:
                summary = store.import_document(
                    AdifDocument(parsed.document.header, tuple(new)),
                    file_name=body.file_name,
                    file_sha256=sha256,
                    actor=who,
                    note=body.note,
                )
            except DuplicateImportError as error:  # imported meanwhile, from another workstation
                raise HTTPException(status.HTTP_409_CONFLICT, str(error)) from error
            out.import_id = summary.import_id
            out.added = len(summary.qso_ids)
    if not new:
        response.status_code = status.HTTP_200_OK
    sync.wake()
    return out


@api.put("/qsos/{qso_id}")
def edit_qso(qso_id: uuid.UUID, body: QsoEdit, who: Signed, conn: Conn, sync: Sync) -> QsoWritten:
    """Replace a QSO's fields as a new version; refused if it changed since ``base_version``."""
    _require(conn, who, Permission.LOG)
    version = LogStore(conn).edit_qso(
        qso_id,
        _record(body.fields),
        actor=who,
        base_version=body.base_version,
        note=body.note,
    )
    sync.wake()
    return _written(version)


@api.post("/qsos/{qso_id}/delete")
def delete_qso(
    qso_id: uuid.UUID, body: QsoChange, who: Signed, conn: Conn, sync: Sync
) -> QsoWritten:
    """Mark a QSO deleted. Its fields and history are kept; restore brings it back."""
    _require(conn, who, Permission.LOG)
    written = _written(LogStore(conn).delete_qso(qso_id, actor=who, note=body.note))
    sync.wake()
    return written


@api.post("/qsos/{qso_id}/restore")
def restore_qso(
    qso_id: uuid.UUID, body: QsoChange, who: Signed, conn: Conn, sync: Sync
) -> QsoWritten:
    """Recover a deleted QSO."""
    _require(conn, who, Permission.LOG)
    written = _written(LogStore(conn).restore_qso(qso_id, actor=who, note=body.note))
    sync.wake()
    return written


def _callbook(request: Request, conn: Conn) -> Callbook:
    link: Link = request.app.state.qrz
    return Callbook(conn, link)


Books = Annotated[Callbook, Depends(_callbook)]


def _lookup_out(result: LookupResult) -> LookupOut:
    entry = result.entry
    return LookupOut(
        status=result.status.value,
        call=result.call,
        name=None if entry is None else entry.name,
        qth=None if entry is None else entry.qth,
        state=None if entry is None else entry.state,
        country=None if entry is None else entry.country,
        grid=None if entry is None else entry.grid,
        photo=entry is not None and entry.image is not None,
        bio=entry is not None and entry.bio,
        cached=result.cached,
        fetched_at=result.fetched_at,
        message=result.message,
        sources=list(result.sources),
        license_class=None if result.license is None else result.license.license_class,
        license_expires=None if result.license is None else result.license.expires,
    )


@api.get("/lookup")
def lookup(
    who: Signed, conn: Conn, books: Books, query: Annotated[LookupQuery, Query()]
) -> LookupOut:
    """Who and where a call is: the US and Canadian license lists, then QRZ.com.

    Always answers 200; ``status`` says whether anything was found. The license
    lists answer with no internet; QRZ.com, with the operator's login, fills in
    what they lack (Principle VI). A call is fetched from QRZ.com once and
    kept; ``refresh`` fetches it again.
    """
    _require(conn, who, Permission.VIEW)
    return _lookup_out(books.lookup(who, query.call.upper(), refresh=query.refresh))


def _bio_out(result: BioResult) -> BioOut:
    return BioOut(
        status=result.status.value,
        call=result.call,
        text=result.text,
        fetched_at=result.fetched_at,
        message=result.message,
    )


@api.get("/lookup/bio")
def lookup_bio(
    who: Signed, conn: Conn, books: Books, query: Annotated[LookupQuery, Query()]
) -> BioOut:
    """The biography a call wrote on QRZ.com, as plain text, once the call has been looked up.

    Always answers 200. Only the words are kept, never the page's markup
    (Principle XII). It is fetched once and kept (Principle VI); ``refresh``
    fetches it again.
    """
    _require(conn, who, Permission.VIEW)
    return _bio_out(books.biography(who, query.call.upper(), refresh=query.refresh))


@api.get(
    "/lookup/photo",
    response_class=Response,
    responses={200: {"content": {"image/jpeg": {}, "image/png": {}}}, 404: {}},
)
def lookup_photo(
    who: Signed, conn: Conn, books: Books, query: Annotated[LookupQuery, Query()]
) -> Response:
    """The call's photo from QRZ.com, once the call has been looked up; 404 if there is none.

    Only a JPEG, PNG, GIF or WebP picture is ever served. It is fetched once
    and kept (Principle VI), so it shows with no internet; ``refresh``
    fetches it again.
    """
    _require(conn, who, Permission.VIEW)
    photo = books.photo(query.call.upper(), refresh=query.refresh)
    if photo is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"no photo of {query.call.upper()}")
    return Response(photo.content, media_type=photo.content_type)


_LOGIN_MESSAGE: Final = {
    None: "Saved. QRZ.com accepted your login.",
    LookupStatus.LOGIN_FAILED: "Saved, but QRZ.com refused this username and password.",
    LookupStatus.UNREACHABLE: "Saved. QRZ.com can't be reached right now to check it.",
}


@api.get("/callbook/qrz")
def get_qrz_login(who: Signed, books: Books) -> QrzLoginOut:
    """The QRZ.com username the signed-in operator saved, if any."""
    saved = books.login_name(who)
    if saved is None:
        return QrzLoginOut(
            username=None, status=None, checked=False, message="QRZ.com is not set up."
        )
    username, checked = saved
    message = f"Using QRZ.com as {username}."
    if not checked:
        message += " QRZ.com has not accepted this login yet."
    return QrzLoginOut(username=username, status=None, checked=checked, message=message)


@api.put("/callbook/qrz")
def put_qrz_login(body: QrzLoginIn, who: Signed, books: Books) -> QrzLoginOut:
    """Save the signed-in operator's QRZ.com login, and try it if QRZ.com can be reached."""
    result = books.save_login(who, body.username, body.password)
    return QrzLoginOut(
        username=body.username,
        status=None if result.status is None else result.status.value,
        checked=result.status is None,
        message=_login_message(result),
    )


def _login_message(result: LoginCheck) -> str:
    """What to tell the operator, in QRZ.com's own words where it gave any."""
    message = _LOGIN_MESSAGE[result.status]
    if result.status is LookupStatus.LOGIN_FAILED:
        reason = result.note or "no reason given"
        return (
            f"{message} QRZ.com said: {reason}. The username is your callsign, not your"
            " email address, and the password is the one you sign in to qrz.com with."
        )
    if result.note:
        message += f" QRZ.com said: {result.note}"
    return message


@api.delete("/callbook/qrz", status_code=status.HTTP_204_NO_CONTENT)
def delete_qrz_login(who: Signed, books: Books) -> None:
    """Remove the signed-in operator's QRZ.com login."""
    books.remove_login(who)


def _profile_refresh_out(request: Request, conn: Conn) -> ProfileRefreshOut:
    refresher: ProfileRefresher = request.app.state.profiles
    setting = CallbookStore(conn).refresh_setting()
    last = refresher.last_report()
    return ProfileRefreshOut(
        mode=setting.mode,
        changed_by=setting.changed_by,
        changed_at=setting.changed_at,
        last_checked_at=None if last is None else last.at,
        network=None if last is None or last.network is None else last.network.value,
        message=None if last is None else last.message,
    )


@api.get("/callbook/refresh")
def get_profile_refresh(request: Request, who: Signed, conn: Conn) -> ProfileRefreshOut:
    """When the Engine re-checks kept QRZ.com profiles in the background, and how it last went."""
    _require(conn, who, Permission.VIEW)
    return _profile_refresh_out(request, conn)


@api.put("/callbook/refresh")
def put_profile_refresh(
    request: Request, body: ProfileRefreshIn, who: Signed, conn: Conn
) -> ProfileRefreshOut:
    """Set when kept QRZ.com profiles are re-checked (admins only), and check soon."""
    _require(conn, who, Permission.ADMIN)
    CallbookStore(conn).save_refresh_mode(who, body.mode)
    refresher: ProfileRefresher = request.app.state.profiles
    refresher.wake()
    return _profile_refresh_out(request, conn)


@api.get("/callbook/cleanup")
def get_callbook_cleanup(
    who: Signed, conn: Conn, query: Annotated[CleanupQuery, Query()]
) -> CleanupOut:
    """How many kept QRZ.com lookups are of calls never logged, older than ``days`` (admins)."""
    _require(conn, who, Permission.ADMIN)
    count = CallbookStore(conn).unlogged(CALLBOOK_SOURCE, timedelta(days=query.days))
    return CleanupOut(
        days=query.days,
        calls=count.entries,
        photos_and_bios=count.media,
        bytes=count.media_bytes,
    )


@api.post("/callbook/cleanup")
def post_callbook_cleanup(body: CleanupIn, who: Signed, conn: Conn) -> CleanupDone:
    """Clear kept QRZ.com lookups of calls never logged, older than ``days`` (admins).

    Only the lookup cache goes; a call in any QSO, deleted ones too, is kept,
    and the log is never touched. A cleared call is fetched again if typed.
    """
    _require(conn, who, Permission.ADMIN)
    removed = CallbookStore(conn).forget_unlogged(CALLBOOK_SOURCE, timedelta(days=body.days))
    return CleanupDone(days=body.days, removed=removed)


_CREDIT: Final = {Source.FCC: FCC_CREDIT, Source.ISED: ISED_CREDIT}


@api.get("/licenses")
def get_licenses(request: Request, who: Signed, conn: Conn) -> LicensesOut:
    """How current the US (FCC) and Canadian (ISED) license lists are (Principle VI)."""
    _require(conn, who, Permission.VIEW)
    refresher: Refresher = request.app.state.licenses
    report = refresher.last_report()
    problems = [] if report is None else report.problems
    return LicensesOut(
        sources=[
            LicenseSourceOut(
                source=state.source.value,
                country=COUNTRY[state.source],
                calls=state.calls,
                published=state.full_published,
                imported=state.full_imported,
                changes_published=state.daily_published,
                changes_imported=state.daily_imported,
                credit=_CREDIT[state.source],
            )
            for state in LicenseStore(conn).states()
        ],
        problem=problems[0] if problems else None,
    )


@api.post("/licenses/refresh", status_code=status.HTTP_202_ACCEPTED)
def refresh_licenses(request: Request, who: Signed, conn: Conn) -> None:
    """Check for newer license lists now, in the background (admins only)."""
    _require(conn, who, Permission.ADMIN)
    refresher: Refresher = request.app.state.licenses
    refresher.wake()


def _lotw(request: Request, conn: Conn) -> Lotw:
    link: LotwLink = request.app.state.lotw
    return Lotw(conn, link, f"LabLog Engine {__version__}")


LotwService = Annotated[Lotw, Depends(_lotw)]


def _setup_out(setup: Setup) -> LotwSetupOut:
    cert = setup.certificate
    location = setup.location
    if cert is None:
        message = "LoTW is not set up."
    elif location is None:
        message = f"Certificate for {cert.call} saved. Add your station location to send QSOs."
    elif setup.batch is not None:
        n = setup.batch.count
        message = (
            f"Batch {setup.batch.id} ({n} QSO{'' if n == 1 else 's'}) is waiting for your review."
        )
    else:
        waiting = setup.waiting
        message = (
            f"Nothing is waiting to go to LoTW as {cert.call}."
            if waiting == 0
            else f"{waiting} QSO{'' if waiting == 1 else 's'} waiting to go to LoTW as {cert.call}."
        )
    return LotwSetupOut(
        certificate=None
        if cert is None
        else LotwCertificateOut(
            call=cert.call,
            dxcc=cert.dxcc,
            entity=cert.entity,
            qso_first=cert.qso_first,
            qso_last=cert.qso_last,
            expires=cert.expires,
        ),
        username=setup.username,
        location=None
        if location is None
        else LotwLocationModel(
            grid=location.grid,
            ituz=location.ituz,
            cqz=location.cqz,
            us_state=location.us_state,
            us_county=location.us_county,
            ca_province=location.ca_province,
        ),
        waiting=setup.waiting,
        qsl_since=setup.qsl_since,
        problem=setup.problem,
        message=setup.problem or message,
        batch=None if setup.batch is None else _batch_out(setup.batch),
    )


def _batch_out(batch: LotwBatch, qsos: tuple[BatchQso, ...] | None = None) -> LotwBatchOut:
    return LotwBatchOut(
        id=batch.id,
        station_call=batch.station_call,
        state=batch.state,
        prepared_at=batch.prepared_at,
        finished_at=batch.finished_at,
        answer=batch.answer,
        count=batch.count,
        sent=batch.sent,
        qsos=None
        if qsos is None
        else [
            LotwBatchQsoOut(
                id=q.qso_id,
                version=q.version,
                call=q.call,
                qso_start=q.qso_start,
                band=q.band,
                mode=q.mode,
                sent=q.sent,
            )
            for q in qsos
        ],
    )


def _skipped_out(skipped: tuple[Skipped, ...]) -> list[LotwSkippedOut]:
    return [LotwSkippedOut(id=s.qso_id, call=s.call, reason=s.reason) for s in skipped]


@api.get("/lotw")
def get_lotw(who: Signed, lotw: LotwService) -> LotwSetupOut:
    """The signed-in operator's LoTW set-up, and how many QSOs wait to be sent."""
    return _setup_out(lotw.setup(who))


@api.get("/lotw/places")
def get_lotw_places(who: Signed) -> LotwPlacesOut:
    """The US states and counties and Canadian provinces LoTW knows, with their zones."""
    del who  # signed in is enough
    t = lotw_tables()

    def out(place: Place) -> LotwPlaceOut:
        return LotwPlaceOut(
            code=place.code,
            name=place.name,
            zones=list(place.zones),
            counties=list(place.counties),
        )

    return LotwPlacesOut(
        us_states=[out(p) for p in t.us_states.values()],
        ca_provinces=[out(p) for p in t.ca_provinces.values()],
        source=t.source,
    )


@api.put("/lotw/certificate")
def put_lotw_certificate(body: LotwCertificateIn, who: Signed, lotw: LotwService) -> LotwSetupOut:
    """Save the operator's callsign certificate, after checking it opens with its password."""
    try:
        p12 = base64.b64decode(body.p12_base64, validate=True)
    except binascii.Error as error:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, "the certificate file is not base64"
        ) from error
    try:
        lotw.save_certificate(who, p12, body.password)
    except CertificateError as error:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(error)) from error
    return _setup_out(lotw.setup(who))


@api.put("/lotw/login")
def put_lotw_login(body: LotwLoginIn, who: Signed, lotw: LotwService) -> LotwSetupOut:
    """Save the operator's LoTW website login, for fetching confirmations."""
    lotw.save_login(who, body.username, body.password)
    return _setup_out(lotw.setup(who))


@api.put("/lotw/location")
def put_lotw_location(body: LotwLocationModel, who: Signed, lotw: LotwService) -> LotwSetupOut:
    """Save the station location LoTW records with each upload."""
    try:
        lotw.save_location(who, body.model_dump())
    except LocationError as error:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(error)) from error
    return _setup_out(lotw.setup(who))


@api.delete("/lotw", status_code=status.HTTP_204_NO_CONTENT)
def delete_lotw(who: Signed, lotw: LotwService) -> None:
    """Remove the operator's whole LoTW set-up: certificate, login and location."""
    lotw.remove(who)


@api.post("/lotw/batches")
def lotw_prepare(who: Signed, conn: Conn, lotw: LotwService) -> LotwPrepareOut:
    """Gather the operator's waiting QSOs into a batch for review. Nothing is sent.

    Always answers 200; ``status`` says what happened. While a batch waits for
    review, this answers with it rather than making another.
    """
    _require(conn, who, Permission.LOG)
    result = lotw.prepare(who)
    batch = None
    if result.batch is not None:
        found = lotw.batch(who, result.batch.id)
        batch = None if found is None else _batch_out(*found)
    return LotwPrepareOut(
        status=result.outcome.value,
        batch=batch,
        skipped=_skipped_out(result.skipped),
        remaining=result.remaining,
        message=result.message,
    )


@api.get("/lotw/batches")
def lotw_batches(who: Signed, lotw: LotwService) -> list[LotwBatchOut]:
    """The operator's latest LoTW batches, newest first."""
    return [_batch_out(b) for b in lotw.batches(who)]


@api.get("/lotw/batches/{batch_id}")
def lotw_batch(batch_id: int, who: Signed, lotw: LotwService) -> LotwBatchOut:
    """One of the operator's LoTW batches, with its QSOs as they were when prepared."""
    found = lotw.batch(who, batch_id)
    if found is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, f"you have no LoTW batch {batch_id}")
    return _batch_out(*found)


@api.post("/lotw/batches/{batch_id}/send")
def lotw_send(batch_id: int, who: Signed, conn: Conn, lotw: LotwService) -> LotwUploadOut:
    """Sign the QSOs in the operator's reviewed batch and send them to LoTW.

    Always answers 200; ``status`` says what happened. QSOs LoTW took get
    ``LOTW_QSL_SENT`` ``Y``, as a new audited version (Principle VII). A QSO
    changed since the batch was prepared is not sent.
    """
    _require(conn, who, Permission.LOG)
    result = lotw.release(who, batch_id)
    return LotwUploadOut(
        status=result.outcome.value,
        sent=result.sent,
        skipped=_skipped_out(result.skipped),
        remaining=result.remaining,
        message=result.message,
    )


@api.post("/lotw/batches/{batch_id}/cancel")
def lotw_cancel(batch_id: int, who: Signed, conn: Conn, lotw: LotwService) -> LotwBatchOut:
    """Cancel the operator's batch waiting for review; its QSOs keep waiting. Nothing is sent."""
    _require(conn, who, Permission.LOG)
    if not lotw.cancel(who, batch_id):
        raise HTTPException(
            status.HTTP_409_CONFLICT, f"LoTW batch {batch_id} is not waiting for review"
        )
    found = lotw.batch(who, batch_id)
    assert found is not None  # noqa: S101 - it was just cancelled
    return _batch_out(*found)


@api.post("/lotw/download")
def lotw_download(who: Signed, conn: Conn, lotw: LotwService) -> LotwDownloadOut:
    """Fetch new confirmations from LoTW and mark the QSOs they confirm.

    Always answers 200; ``status`` says what happened. Confirmed QSOs get
    ``LOTW_QSL_RCVD`` ``Y``, as a new audited version (Principle VII).
    """
    _require(conn, who, Permission.LOG)
    result = lotw.download(who)
    return LotwDownloadOut(
        status=result.outcome.value,
        confirmed=result.confirmed,
        already=result.already,
        unmatched=result.unmatched,
        message=result.message,
    )


def _logbook_out(
    conn: psycopg.Connection[TupleRow], who: Actor, sync: LogbookSync
) -> QrzLogbookOut:
    store = LogbookStore(conn)
    book = store.mine(who)
    if book is None:
        return QrzLogbookOut(
            book_call=None,
            sent=0,
            waiting=0,
            refused=0,
            last_refusal=None,
            problem=None,
            message="QSOs are not being sent to a QRZ logbook.",
        )
    counts = store.counts(book)
    state = sync.state(book.book_call)
    problem = None if state is None else state.problem
    message = f"New QSOs for {book.book_call} go to its QRZ logbook: {counts.sent} sent"
    message += f", {counts.waiting} waiting." if counts.waiting else "."
    return QrzLogbookOut(
        book_call=book.book_call,
        sent=counts.sent,
        waiting=counts.waiting,
        refused=counts.refused,
        last_refusal=counts.last_refusal,
        problem=problem,
        message=message,
    )


@api.get("/logbook/qrz")
def get_qrz_logbook(who: Signed, conn: Conn, sync: Sync) -> QrzLogbookOut:
    """The QRZ logbook the operator's QSOs go to, and how many are sent or waiting."""
    return _logbook_out(conn, who, sync)


@api.put("/logbook/qrz")
def put_qrz_logbook(body: QrzLogbookIn, who: Signed, conn: Conn, sync: Sync) -> QrzLogbookOut:
    """Save the operator's QRZ logbook API key and check it if QRZ.com can be reached.

    QSOs entered from now on, for the logbook's callsign (the operator's own
    unless ``book_call`` names another, such as a club call), are sent to it.
    The key is saved even with no internet; QSOs wait until QRZ.com is reached.
    """
    book_call = (body.book_call or who.operator).upper()
    LogbookStore(conn).save(who, body.api_key, book_call)
    try:
        sync.client.status(body.api_key)
    except QrzKeyError as error:
        checked = f"Saved, but QRZ.com refused this API key: {error}"
    except (QrzUnreachableError, QrzReplyError):
        checked = "Saved. QRZ.com can't be reached right now to check it."
    else:
        checked = "Saved. QRZ.com accepted the API key."
    sync.wake()
    out = _logbook_out(conn, who, sync)
    return out.model_copy(update={"message": f"{checked} {out.message}"})


@api.delete("/logbook/qrz", status_code=status.HTTP_204_NO_CONTENT)
def delete_qrz_logbook(who: Signed, conn: Conn) -> None:
    """Stop sending the operator's QSOs to QRZ.com and forget the key."""
    LogbookStore(conn).remove(who)


_STATUS_FOR: Final[tuple[tuple[type[StoreError], int], ...]] = (
    (SignInError, status.HTTP_401_UNAUTHORIZED),
    (AgentKeyError, status.HTTP_401_UNAUTHORIZED),
    (NotSignedInError, status.HTTP_401_UNAUTHORIZED),
    (NotAuthorizedError, status.HTTP_403_FORBIDDEN),
    (QsoNotFoundError, status.HTTP_404_NOT_FOUND),
    (OperationNotFoundError, status.HTTP_404_NOT_FOUND),
    (QsoExistsError, status.HTTP_409_CONFLICT),
    (ArchivedError, status.HTTP_409_CONFLICT),
    (StaleVersionError, status.HTTP_409_CONFLICT),
    (LogbookTakenError, status.HTTP_409_CONFLICT),
)


def _store_error(_request: Request, error: Exception) -> JSONResponse:
    code = next(
        (code for kind, code in _STATUS_FOR if isinstance(error, kind)),
        status.HTTP_422_UNPROCESSABLE_CONTENT,
    )
    if isinstance(error, SignInError) and "too many" in str(error):
        code = status.HTTP_429_TOO_MANY_REQUESTS
    headers = {"WWW-Authenticate": "Bearer"} if code == status.HTTP_401_UNAUTHORIZED else {}
    return JSONResponse({"detail": str(error)}, status_code=code, headers=headers)


async def _security_headers(
    request: Request,
    call_next: Callable[[Request], Awaitable[Response]],
) -> Response:
    response = await call_next(request)
    if not request.url.path.startswith("/api/docs"):
        response.headers["Content-Security-Policy"] = _CSP
    response.headers["X-Content-Type-Options"] = "nosniff"
    response.headers["Referrer-Policy"] = "no-referrer"
    response.headers["Cache-Control"] = "no-store"
    return response


# -- The application ----------------------------------------------------------


def create_app(  # noqa: PLR0913 - one stand-in for each outside service
    database: str,
    *,
    qrz_fetch: Callable[[str], bytes] = http_fetch,
    qrz_post: Callable[[str, bytes, str], bytes] = http_post,
    qrz_fetch_large: Callable[[str, int], bytes] = http_fetch_large,
    lotw_transport: Transport = https,
    license_downloads: Downloads | None = None,
    profile_network: Callable[[], NetworkSeen] = look_at_network,
    fieldlab: FieldLabBus | None = None,
) -> FastAPI:
    """The Engine's API and web client, using the PostgreSQL database ``database``.

    ``qrz_fetch`` and ``qrz_post`` are how the Engine reaches QRZ.com's lookup
    and logbook services, ``qrz_fetch_large`` its biographies and photos,
    ``lotw_transport`` how it reaches LoTW, and ``license_downloads`` how it
    fetches the FCC and ISED license lists, and ``profile_network`` how it
    tells which network its internet comes through; tests give them stand-ins.
    ``fieldlab`` is FieldLab's MQTT bus, when the Engine runs there: its GPS
    fix is the station's home, and its gateway holds large downloads on a
    metered link.
    The QRZ logbook sender runs in the background once
    ``app.state.logbook.start()`` is called, the license lists are kept current
    once ``app.state.licenses.start()`` is, and kept QRZ.com profiles are
    re-checked once ``app.state.profiles.start()`` is.
    """
    app = FastAPI(
        title="LabLog Engine",
        version=__version__,
        summary="The log, operators, and sign-in. Every client uses these operations.",
        license_info={"name": "AGPL-3.0-only", "url": f"{SOURCE}/blob/main/LICENSE"},
        docs_url="/api/docs",
        redoc_url=None,
        openapi_url="/api/v1/openapi.json",
    )
    app.state.database = database
    app.state.qrz = Link(QrzClient(f"LabLog-{__version__}", qrz_fetch, qrz_fetch_large))
    app.state.profiles = ProfileRefresher(database, app.state.qrz, profile_network)
    app.state.lotw = LotwLink(LotwClient(lotw_transport))
    app.state.fieldlab = fieldlab
    app.state.licenses = Refresher(
        database,
        license_downloads or HttpDownloads(f"LabLog/{__version__} ({SOURCE})"),
        hold=fieldlab.download_hold if fieldlab is not None else lambda: None,
    )
    app.state.logbook = LogbookSync(database, QrzLogbookClient(f"LabLog/{__version__}", qrz_post))
    app.state.on_air = OnAir()
    app.state.rig = RigBoard()
    app.state.pairings = PairingDesk()
    app.state.demo = Demo(database, app.state.on_air, app.state.rig)
    app.include_router(api)
    app.add_exception_handler(StoreError, _store_error)
    app.middleware("http")(_security_headers)

    web = files("loglab_engine") / "web"

    @app.get("/", include_in_schema=False)
    def index() -> FileResponse:
        return FileResponse(str(web / "index.html"), media_type="text/html")

    app.mount("/static", StaticFiles(directory=str(web)), name="static")
    return app
