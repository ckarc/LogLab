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
from collections.abc import Awaitable, Callable, Iterator
from datetime import date, datetime
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
    BaseModel,
    ConfigDict,
    Field,
    StringConstraints,
    ValidationError,
    model_validator,
)

from loglab_engine import __version__
from loglab_engine.adif import AdifModelError, Record
from loglab_engine.adif import Field as AdifField
from loglab_engine.callbook import Callbook, Link, LookupStatus, QrzClient, http_fetch
from loglab_engine.callbook.logbook_sync import LogbookSync
from loglab_engine.callbook.qrz import QrzReplyError, QrzUnreachableError
from loglab_engine.callbook.qrz_logbook import QrzKeyError, QrzLogbookClient, http_post
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
    IdentityStore,
    LogStore,
    NotAuthorizedError,
    NotSignedInError,
    Permission,
    QsoExistsError,
    QsoNotFoundError,
    QsoVersion,
    SignInError,
    StaleVersionError,
    StoreError,
    WorkspaceStore,
    connect,
)
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


class QsoCreate(_Strict):
    """A new QSO: its fields, in order, and optionally the ID its client gave it.

    A client that keeps a journal gives each QSO its ID when it is typed, so
    sending the same QSO again never creates a second one (Principle XV).
    """

    # JSON has no UUID type: the ID arrives as text and must parse as one.
    id: Annotated[uuid.UUID, Field(strict=False)] | None = None
    fields: Annotated[list[AdifFieldModel], Field(min_length=1, max_length=MAX_FIELDS)]
    note: Note | None = None


class QsoEdit(_Strict):
    """New fields for a QSO, and the version they were based on."""

    base_version: Annotated[int, Field(ge=1)]
    fields: Annotated[list[AdifFieldModel], Field(min_length=1, max_length=MAX_FIELDS)]
    note: Note | None = None


class QsoChange(_Strict):
    """Why a QSO is being deleted or restored."""

    note: Note | None = None


ModuleId = Literal["entry", "journal", "log", "detail", "map"]
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
TableId = Literal["log"]


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


class Layout(_Strict):
    """An operator's workspace: the modules in the order they appear, and their tables."""

    version: Literal[1] = 1
    modules: Annotated[list[ModuleLayout], Field(min_length=1, max_length=32)]
    tables: dict[TableId, TableLayout] = Field(default_factory=dict)

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
    WorkspaceStore(conn).save_layout(who, body.model_dump())
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
        call=query.call or None,
        limit=query.limit,
        include_deleted=query.include_deleted,
        newest_first=query.newest_first,
        sort=query.sort,
    )
    return [_qso_out(v) for v in found]


@api.get("/sections")
def list_sections(who: Signed, conn: Conn) -> SectionsOut:
    """The ARRL and RAC sections, each with how many current QSOs are in it.

    This is what the Field Day section map colours in. Deleted QSOs do not count.
    """
    _require(conn, who, Permission.VIEW)
    tally = tally_sections(LogStore(conn).field_counts("ARRL_SECT"))
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
    username = books.login_name(who)
    message = "QRZ.com is not set up." if username is None else f"Using QRZ.com as {username}."
    return QrzLoginOut(username=username, status=None, message=message)


@api.put("/callbook/qrz")
def put_qrz_login(body: QrzLoginIn, who: Signed, books: Books) -> QrzLoginOut:
    """Save the signed-in operator's QRZ.com login, and try it if QRZ.com can be reached."""
    result = books.save_login(who, body.username, body.password)
    return QrzLoginOut(
        username=body.username,
        status=None if result is None else result.value,
        message=_LOGIN_MESSAGE[result],
    )


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
    (QsoExistsError, status.HTTP_409_CONFLICT),
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
