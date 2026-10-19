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

import base64
import binascii
import uuid
from collections import Counter
from collections.abc import Awaitable, Callable, Iterable, Iterator, Mapping
from datetime import UTC, date, datetime
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
from fastapi.responses import FileResponse, JSONResponse
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

from loglab_engine import __version__
from loglab_engine.adif import AdifModelError, Record
from loglab_engine.adif import Field as AdifField
from loglab_engine.callbook import (
    Callbook,
    Link,
    LoginCheck,
    LookupStatus,
    QrzClient,
    http_fetch,
)
from loglab_engine.callbook.logbook_sync import LogbookSync
from loglab_engine.callbook.qrz import QrzReplyError, QrzUnreachableError
from loglab_engine.callbook.qrz_logbook import QrzKeyError, QrzLogbookClient, http_post
from loglab_engine.contests import EVERYDAY, Earlier, Overlay, Worked, overlays, sent_problems
from loglab_engine.contests import check as contest_check
from loglab_engine.contests.stamp import Station, stamp, stamp_fields
from loglab_engine.lotw import (
    CertificateError,
    LocationError,
    Lotw,
    LotwClient,
    Place,
    Setup,
    Transport,
    https,
)
from loglab_engine.lotw import Link as LotwLink
from loglab_engine.lotw import tables as lotw_tables
from loglab_engine.reference import SECTIONS, tally_sections
from loglab_engine.reference import SOURCE as SECTIONS_SOURCE
from loglab_engine.store import (
    Actor,
    ArchivedError,
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

    from loglab_engine.callbook.service import LookupResult

SOURCE: Final = "https://github.com/ckarc/loglab"
MAX_FIELDS: Final = 500
MAX_VALUE: Final = 64 * 1024
MAX_NAME: Final = 64
MAX_NOTE: Final = 1000
MAX_LIST: Final = 500
MAX_P12_BASE64: Final = 96 * 1024
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


ModuleId = Literal["entry", "profile", "journal", "log", "detail", "map"]
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


class ModuleLayout(_Strict):
    """One module of the workspace: its width, the side it sits on, and whether it is shown.

    A third sits on the left, middle or right; a half on the left or right;
    a full-width box ignores it.
    """

    id: ModuleId
    width: Literal["third", "half", "full"] = "full"
    side: Literal["left", "middle", "right"] = "left"
    hidden: bool = False

    @model_validator(mode="after")
    def _middle_is_for_thirds(self) -> Self:
        if self.side == "middle" and self.width != "third":
            msg = "only a third can sit on the middle side"
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


class Layout(_Strict):
    """An operator's workspace: its modules in order, its tables, panels, and a QSO's fields.

    ``detail`` is ``None`` until the operator chooses, and the page's default is used.
    ``theme`` is light, dark, or ``auto`` to follow the computer's setting.
    ``operation`` is the operation the operator logs in, or ``None`` for
    everyday logging under their own call. ``by_overlay`` keeps how they
    arranged the page in each other overlay, to bring back when they return.
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
    """What the callbook says about a call, or why it could not say.

    ``status`` is ``found``, ``not_found``, ``not_set_up`` (no QRZ.com login),
    ``unreachable`` (no internet, or QRZ.com is down), or ``login_failed``.
    """

    status: str
    call: str
    name: str | None
    qth: str | None
    state: str | None
    country: str | None
    grid: str | None
    cached: bool
    fetched_at: datetime | None
    message: str


class QrzLoginIn(_Strict):
    """The operator's own QRZ.com username and password."""

    username: Annotated[str, StringConstraints(min_length=1, max_length=64, strip_whitespace=True)]
    password: Annotated[str, StringConstraints(min_length=1, max_length=128)]


class QrzLoginOut(BaseModel):
    """Whether the operator has a QRZ.com login saved. The password is never returned."""

    username: str | None
    status: str | None
    checked: bool
    message: str


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


class LotwSetupOut(BaseModel):
    """The operator's LoTW set-up, and how many of their QSOs wait to be sent.

    ``problem`` says what stops sending, if anything is set up wrongly.
    """

    certificate: LotwCertificateOut | None
    username: str | None
    location: LotwLocationModel | None
    waiting: int
    qsl_since: str | None
    problem: str | None
    message: str


class LotwSkippedOut(BaseModel):
    """A waiting QSO that was not sent, and why."""

    id: uuid.UUID
    call: str | None
    reason: str


class LotwUploadOut(BaseModel):
    """What sending did.

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
    """Which QSOs to list."""

    model_config = ConfigDict(extra="forbid", strict=False, frozen=True)

    call: Annotated[str, StringConstraints(max_length=20)] | None = None
    limit: Annotated[int, Field(ge=1, le=MAX_LIST)] = 50
    include_deleted: bool = False
    newest_first: bool = True
    sort: SortKey = "entered"
    operation: uuid.UUID | None = None


# -- Operations ---------------------------------------------------------------

api = APIRouter(prefix="/api/v1")


@api.get("/status")
def get_status() -> StatusOut:
    """Name, version, and source location. No sign-in needed."""
    return StatusOut(
        name="LogLab Engine",
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
def sign_out(who: Signed, conn: Conn, authorization: Annotated[str, Header()]) -> None:
    """End this session."""
    del who  # authenticated above; an ended session cannot sign out again
    IdentityStore(conn).sign_out(authorization.partition(" ")[2].strip())


@api.get("/layout")
def get_layout(who: Signed, conn: Conn) -> LayoutOut:
    """The signed-in operator's workspace layout; it follows them to any workstation."""
    saved = WorkspaceStore(conn).layout(who)
    if saved is None:
        return LayoutOut(layout=None)
    try:
        return LayoutOut(layout=Layout.model_validate(saved, strict=False))
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
        cached=result.cached,
        fetched_at=result.fetched_at,
        message=result.message,
    )


@api.get("/lookup")
def lookup(
    who: Signed, conn: Conn, books: Books, query: Annotated[LookupQuery, Query()]
) -> LookupOut:
    """Who and where a call is: kept answers first, then QRZ.com with the operator's login.

    Always answers 200; ``status`` says whether anything was found. A call is
    fetched from QRZ.com once and kept (Principle VI); ``refresh`` fetches it again.
    """
    _require(conn, who, Permission.VIEW)
    return _lookup_out(books.lookup(who, query.call.upper(), refresh=query.refresh))


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


def _lotw(request: Request, conn: Conn) -> Lotw:
    link: LotwLink = request.app.state.lotw
    return Lotw(conn, link, f"LogLab Engine {__version__}")


LotwService = Annotated[Lotw, Depends(_lotw)]


def _setup_out(setup: Setup) -> LotwSetupOut:
    cert = setup.certificate
    location = setup.location
    if cert is None:
        message = "LoTW is not set up."
    elif location is None:
        message = f"Certificate for {cert.call} saved. Add your station location to send QSOs."
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
    )


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


@api.post("/lotw/upload")
def lotw_upload(who: Signed, conn: Conn, lotw: LotwService) -> LotwUploadOut:
    """Sign the operator's waiting QSOs with their certificate and send them to LoTW.

    Always answers 200; ``status`` says what happened. QSOs LoTW accepted get
    ``LOTW_QSL_SENT`` ``Y``, as a new audited version (Principle VII).
    """
    _require(conn, who, Permission.LOG)
    result = lotw.upload(who)
    return LotwUploadOut(
        status=result.outcome.value,
        sent=result.sent,
        skipped=[LotwSkippedOut(id=s.qso_id, call=s.call, reason=s.reason) for s in result.skipped],
        remaining=result.remaining,
        message=result.message,
    )


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


def create_app(
    database: str,
    qrz_fetch: Callable[[str], bytes] = http_fetch,
    qrz_post: Callable[[str, bytes, str], bytes] = http_post,
    lotw_transport: Transport = https,
) -> FastAPI:
    """The Engine's API and web client, using the PostgreSQL database ``database``.

    ``qrz_fetch`` and ``qrz_post`` are how the Engine reaches QRZ.com's lookup
    and logbook services, and ``lotw_transport`` how it reaches LoTW; tests
    give them stand-ins. The QRZ logbook sender runs in the background once
    ``app.state.logbook.start()`` is called.
    """
    app = FastAPI(
        title="LogLab Engine",
        version=__version__,
        summary="The log, operators, and sign-in. Every client uses these operations.",
        license_info={"name": "AGPL-3.0-only", "url": f"{SOURCE}/blob/main/LICENSE"},
        docs_url="/api/docs",
        redoc_url=None,
        openapi_url="/api/v1/openapi.json",
    )
    app.state.database = database
    app.state.qrz = Link(QrzClient(f"LogLab-{__version__}", qrz_fetch))
    app.state.lotw = LotwLink(LotwClient(lotw_transport))
    app.state.logbook = LogbookSync(database, QrzLogbookClient(f"LogLab/{__version__}", qrz_post))
    app.include_router(api)
    app.add_exception_handler(StoreError, _store_error)
    app.middleware("http")(_security_headers)

    web = files("loglab_engine") / "web"

    @app.get("/", include_in_schema=False)
    def index() -> FileResponse:
        return FileResponse(str(web / "index.html"), media_type="text/html")

    app.mount("/static", StaticFiles(directory=str(web)), name="static")
    return app
