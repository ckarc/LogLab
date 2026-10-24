# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Sending QSOs to Logbook of the World and bringing its confirmations back.

LoTW is an enhancement (Principle V). Logging never waits for it: a QSO is
logged when the Engine commits it, and simply waits to be sent. Nothing goes
to LoTW live: the operator first gathers waiting QSOs into a numbered batch
and looks it over, and only when they release it does the Engine sign those
QSOs with their own callsign certificate and send them (Principle XXI).
When LoTW takes the file, each QSO's ``LOTW_QSL_SENT`` becomes ``Y``. Fetching confirmations sets
``LOTW_QSL_RCVD`` to ``Y`` on each QSO LoTW confirmed. Both are changes to
the QSOs' own ADIF fields (Principle IV), made through the audited path by
the operator who asked (Principles VII and XXI), and a QSO an operator has
edited in the meantime is left alone.

When LoTW cannot be reached, nothing changes and the answer says so; the
Engine then does not try again for a minute.
"""

from __future__ import annotations

import re
import threading
import time
from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta
from enum import StrEnum
from typing import TYPE_CHECKING, Final

from loglab_engine.adif import Field, Record
from loglab_engine.lotw.certificate import CallsignCertificate, CertificateError, load_certificate
from loglab_engine.lotw.client import (
    LotwClient,
    LotwLoginError,
    LotwRejectedError,
    LotwReplyError,
    LotwUnreachableError,
)
from loglab_engine.lotw.gabbi import (
    Contact,
    LocationError,
    NotSignableError,
    StationLocation,
    build_tq8,
    contact_from_record,
)
from loglab_engine.lotw.tables import tables
from loglab_engine.store.log import LogStore
from loglab_engine.store.lotw import (
    BatchQso,
    LotwAccount,
    LotwAccountStore,
    LotwBatch,
    LotwBatchStore,
)

if TYPE_CHECKING:
    import uuid
    from collections.abc import Callable, Mapping

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor, QsoVersion

SERVICE: Final = "LOTW"
MAX_PER_UPLOAD: Final = 1000
MATCH_WINDOW: Final = timedelta(minutes=30)
RETRY_AFTER_SECONDS: Final = 60.0
FIRST_FETCH: Final = "1900-01-01"
_DAY: Final = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}")
_UNREACHABLE: Final = "LoTW can't be reached right now. Nothing changed; try again later."


class Outcome(StrEnum):
    """How a send or a fetch turned out."""

    DONE = "done"
    NOTHING = "nothing"
    NOT_SET_UP = "not_set_up"
    UNREACHABLE = "unreachable"
    LOGIN_FAILED = "login_failed"
    REJECTED = "rejected"
    CERTIFICATE = "certificate_problem"


@dataclass(frozen=True, slots=True)
class CertificateSummary:
    """What a saved certificate says, without its key."""

    call: str
    dxcc: int
    entity: str
    qso_first: date
    qso_last: date | None
    expires: datetime


@dataclass(frozen=True, slots=True)
class Setup:
    """The signed-in operator's LoTW set-up, as the API shows it (never a password)."""

    certificate: CertificateSummary | None
    problem: str | None
    username: str | None
    location: StationLocation | None
    waiting: int
    qsl_since: str | None
    batch: LotwBatch | None = None


@dataclass(frozen=True, slots=True)
class Skipped:
    """A waiting QSO that was not sent, and why."""

    qso_id: uuid.UUID
    call: str | None
    reason: str


@dataclass(frozen=True, slots=True)
class UploadResult:
    """What sending a batch did."""

    outcome: Outcome
    sent: int
    skipped: tuple[Skipped, ...]
    remaining: int
    message: str


@dataclass(frozen=True, slots=True)
class PrepareResult:
    """What preparing a batch did: the batch waiting for review, if any."""

    outcome: Outcome
    batch: LotwBatch | None
    skipped: tuple[Skipped, ...]
    remaining: int
    message: str


@dataclass(frozen=True, slots=True)
class DownloadResult:
    """What fetching confirmations did."""

    outcome: Outcome
    confirmed: int
    already: int
    unmatched: int
    message: str


class Link:
    """The Engine's one connection to LoTW, shared by every request.

    It remembers when LoTW last could not be reached, so requests in the next
    minute say so at once instead of each waiting for a timeout.
    """

    def __init__(self, client: LotwClient, clock: Callable[[], float] = time.monotonic) -> None:
        """Use ``client``; ``clock`` is a monotonic clock in seconds."""
        self.client = client
        self._clock = clock
        self._lock = threading.Lock()
        self._down_until = 0.0

    def down(self) -> bool:
        """Whether LoTW could not be reached within the last minute."""
        with self._lock:
            return self._clock() < self._down_until

    def mark(self, *, reachable: bool) -> None:
        """Record whether LoTW just answered."""
        with self._lock:
            self._down_until = 0.0 if reachable else self._clock() + RETRY_AFTER_SECONDS


def with_fields(record: Record, values: Mapping[str, str]) -> Record:
    """``record`` with each named field set to its value: replaced in place, or added at the end."""
    wanted = {name.upper(): value for name, value in values.items()}
    out: list[Field] = []
    for field in record.fields:
        if field.key in wanted:
            value = wanted.pop(field.key)
            out.append(Field(field.name, value, field.type_indicator))
        else:
            out.append(field)
    out += [Field(name, value) for name, value in wanted.items()]
    return Record(tuple(out))


def _location_dict(location: StationLocation) -> dict[str, object]:
    return {
        "grid": location.grid,
        "ituz": location.ituz,
        "cqz": location.cqz,
        "us_state": location.us_state,
        "us_county": location.us_county,
        "ca_province": location.ca_province,
    }


def make_location(cert: CallsignCertificate, values: Mapping[str, object]) -> StationLocation:
    """A station location for ``cert``'s call and entity from saved or typed values.

    Raises:
        LocationError: The values are incomplete or not on LoTW's lists.
    """

    def text(key: str) -> str | None:
        value = values.get(key)
        return value.strip() if isinstance(value, str) and value.strip() else None

    def number(key: str) -> int:
        value = values.get(key)
        if isinstance(value, bool) or not isinstance(value, int):
            raise LocationError(f"the {key.upper()} zone is missing")
        return value

    grid = text("grid") or ""
    return StationLocation(
        call=cert.call,
        dxcc=cert.dxcc,
        grid=grid[:2].upper() + grid[2:4] + grid[4:6].upper(),
        ituz=number("ituz"),
        cqz=number("cqz"),
        us_state=(text("us_state") or "").upper() or None,
        us_county=text("us_county"),
        ca_province=(text("ca_province") or "").upper() or None,
    )


def _summary(cert: CallsignCertificate) -> CertificateSummary:
    return CertificateSummary(
        cert.call,
        cert.dxcc,
        tables().dxcc.get(cert.dxcc, f"entity {cert.dxcc}"),
        cert.qso_first,
        cert.qso_last,
        cert.expires,
    )


def _where_made(record: Record, location: StationLocation) -> str | None:
    """Why the QSO's own MY_ fields say it was not made from ``location``, if they do."""
    grid = (record.get("MY_GRIDSQUARE") or "").strip().upper()
    if grid and grid[:4] != location.grid[:4].upper():
        return f"it was made from grid {grid}, and your LoTW location is {location.grid}"
    dxcc = (record.get("MY_DXCC") or "").strip()
    if dxcc and dxcc != str(location.dxcc):
        return f"it was made from DXCC entity {dxcc}, and your LoTW location is {location.dxcc}"
    state = (record.get("MY_STATE") or "").strip().upper()
    mine = location.us_state or location.ca_province
    if state and mine and state != mine:
        return f"it was made from {state}, and your LoTW location is in {mine}"
    county = (record.get("MY_CNTY") or "").strip().split(",")[-1].strip()
    if county and location.us_county and county.upper() != location.us_county.upper():
        return f"it was made in {county} County, and your LoTW location is {location.us_county}"
    return None


def _when(record: Record) -> datetime | None:
    day = (record.get("QSO_DATE") or "").strip()
    clock = (record.get("TIME_ON") or "").strip().ljust(6, "0")
    try:
        return datetime.strptime(day + clock[:6], "%Y%m%d%H%M%S").replace(tzinfo=UTC)
    except ValueError:
        return None


class Lotw:
    """LoTW for the operator a request is made by."""

    def __init__(self, conn: psycopg.Connection[TupleRow], link: Link, writer: str) -> None:
        """Use ``conn`` (autocommit), the Engine's ``link`` to LoTW; sign files as ``writer``."""
        self._accounts = LotwAccountStore(conn)
        self._batches = LotwBatchStore(conn)
        self._log = LogStore(conn)
        self._link = link
        self._writer = writer

    # -- Set-up -----------------------------------------------------------------

    def setup(self, actor: Actor) -> Setup:
        """What the operator has set up, and how many QSOs wait to be sent."""
        account = self._accounts.get(actor)
        if account is None:
            return Setup(None, None, None, None, 0, None)
        summary = location = None
        problem: str | None = None
        cert: CallsignCertificate | None = None
        try:
            cert = self._certificate(account)
        except CertificateError as error:
            problem = f"The saved certificate can't be used: {error}."
        if cert is not None:
            summary = _summary(cert)
            if account.location is not None:
                try:
                    location = make_location(cert, account.location)
                except LocationError as error:
                    problem = f"Your station location needs fixing: {error}."
        waiting = 0 if cert is None else self._log.qsl_waiting(SERVICE, cert.call)
        return Setup(
            summary,
            problem,
            account.username,
            location,
            waiting,
            account.qsl_since,
            self._batches.ready(actor),
        )

    def save_certificate(self, actor: Actor, p12: bytes, password: str) -> CertificateSummary:
        """Check and keep the operator's callsign certificate.

        Raises:
            CertificateError: It cannot be opened, or is not a LoTW certificate.
        """
        cert = load_certificate(p12, password)
        self._accounts.save_certificate(actor, p12, password)
        return _summary(cert)

    def save_login(self, actor: Actor, username: str, password: str) -> None:
        """Keep the operator's LoTW website login, for fetching confirmations."""
        self._accounts.save_login(actor, username, password)

    def save_location(self, actor: Actor, values: Mapping[str, object]) -> StationLocation:
        """Check and keep the station location LoTW records with each upload.

        Raises:
            LocationError: No certificate is saved yet, or the location is not
                one LoTW accepts for it.
        """
        account = self._accounts.get(actor)
        try:
            cert = None if account is None else self._certificate(account)
        except CertificateError as error:
            raise LocationError(f"the saved certificate can't be used: {error}") from error
        if cert is None:
            raise LocationError("save your callsign certificate first")
        location = make_location(cert, values)
        self._accounts.save_location(actor, _location_dict(location))
        return location

    def remove(self, actor: Actor) -> bool:
        """Remove the operator's whole LoTW set-up; ``False`` if there was none."""
        return self._accounts.remove(actor)

    @staticmethod
    def _certificate(account: LotwAccount) -> CallsignCertificate | None:
        if account.certificate is None:
            return None
        return load_certificate(account.certificate, account.certificate_password or "")

    # -- Sending ----------------------------------------------------------------

    def _ready_to_sign(
        self, actor: Actor
    ) -> tuple[CallsignCertificate, StationLocation] | UploadResult:
        """The operator's certificate and location, or why QSOs can't be signed."""
        account = self._accounts.get(actor)
        try:
            cert = None if account is None else self._certificate(account)
        except CertificateError as error:
            return UploadResult(Outcome.CERTIFICATE, 0, (), 0, f"Your certificate: {error}.")
        if account is None or cert is None or account.location is None:
            return UploadResult(
                Outcome.NOT_SET_UP,
                0,
                (),
                0,
                "Save your callsign certificate and station location to send QSOs to LoTW.",
            )
        try:
            location = make_location(cert, account.location)
        except LocationError as error:
            return UploadResult(Outcome.NOT_SET_UP, 0, (), 0, f"Your station location: {error}.")
        return cert, location

    def prepare(self, actor: Actor) -> PrepareResult:
        """Gather the operator's waiting QSOs into a batch for them to review.

        Nothing is sent: the batch waits until the operator releases it. An
        operator has one batch waiting at a time; while one waits, this
        answers with it.
        """
        ready_to_sign = self._ready_to_sign(actor)
        if isinstance(ready_to_sign, UploadResult):
            return PrepareResult(ready_to_sign.outcome, None, (), 0, ready_to_sign.message)
        cert, location = ready_to_sign
        waiting = self._batches.ready(actor)
        if waiting is not None:
            return PrepareResult(
                Outcome.DONE,
                waiting,
                (),
                0,
                f"Batch {waiting.id} is still waiting for your review: send it or cancel it first.",
            )
        queue = self._log.qsl_queue(SERVICE, cert.call, MAX_PER_UPLOAD + 1)
        remaining = max(len(queue) - MAX_PER_UPLOAD, 0)
        ready, skipped = self._sign_ready(cert, location, queue[:MAX_PER_UPLOAD])
        if not ready:
            message = (
                "No QSOs are waiting to go to LoTW."
                if not skipped
                else f"None of the {len(skipped)} waiting QSOs can be sent yet; see why below."
            )
            return PrepareResult(Outcome.NOTHING, None, tuple(skipped), remaining, message)
        batch_id = self._batches.create(actor, cert.call, [(v.qso_id, v.version) for v, _ in ready])
        batch = self._batches.get(actor, batch_id)
        count = len(ready)
        message = (
            f"Batch {batch_id} holds {count} QSO{'' if count == 1 else 's'}. Check them, then send"
            " the batch to LoTW. Nothing has been sent yet."
        )
        if remaining:
            message += f" {remaining} more are waiting for a later batch."
        return PrepareResult(Outcome.DONE, batch, tuple(skipped), remaining, message)

    def batch(self, actor: Actor, batch_id: int) -> tuple[LotwBatch, tuple[BatchQso, ...]] | None:
        """The operator's batch ``batch_id`` and its QSOs; ``None`` if they have no such batch."""
        batch = self._batches.get(actor, batch_id)
        return None if batch is None else (batch, self._batches.qsos(batch_id))

    def batches(self, actor: Actor, limit: int = 20) -> tuple[LotwBatch, ...]:
        """The operator's latest batches, newest first."""
        return self._batches.recent(actor, limit)

    def cancel(self, actor: Actor, batch_id: int) -> bool:
        """Cancel the operator's waiting batch, sending nothing; ``False`` if it isn't waiting."""
        return self._batches.cancel(actor, batch_id)

    def release(self, actor: Actor, batch_id: int) -> UploadResult:  # noqa: PLR0911 - one answer per outcome
        """Sign the QSOs in the operator's reviewed batch and send them to LoTW.

        Only QSOs still exactly as they were when the batch was prepared, and
        still waiting, are sent; any other stays behind for a later batch.
        """
        batch = self._batches.get(actor, batch_id)
        if batch is None or batch.state != "ready":
            return UploadResult(
                Outcome.NOTHING, 0, (), 0, f"Batch {batch_id} is not waiting to be sent."
            )
        ready_to_sign = self._ready_to_sign(actor)
        if isinstance(ready_to_sign, UploadResult):
            return ready_to_sign
        cert, location = ready_to_sign
        if cert.call != batch.station_call:
            self._batches.cancel(actor, batch_id, f"Cancelled: the certificate is now {cert.call}.")
            return UploadResult(
                Outcome.NOTHING,
                0,
                (),
                0,
                f"Batch {batch_id} was for {batch.station_call}, but your certificate is now for"
                f" {cert.call}. It was cancelled; prepare a new batch.",
            )
        queue: list[QsoVersion] = []
        changed: list[Skipped] = []
        for item in self._batches.qsos(batch_id):
            now = self._log.current(item.qso_id)
            sent_mark = (now.record.get(f"{SERVICE}_QSL_SENT") or "").strip().upper()
            if now.version != item.version or now.deleted or sent_mark in {"Y", "N", "I"}:
                changed.append(
                    Skipped(
                        item.qso_id,
                        item.call,
                        "Not sent: it changed after the batch was prepared; it goes in a later"
                        " batch.",
                    )
                )
                continue
            queue.append(now)
        ready, skipped = self._sign_ready(cert, location, tuple(queue))
        skipped = changed + skipped
        if not ready:
            self._batches.cancel(actor, batch_id, "Cancelled: none of its QSOs could be sent.")
            return UploadResult(
                Outcome.NOTHING,
                0,
                tuple(skipped),
                0,
                f"None of the QSOs in batch {batch_id} can be sent now, so it was cancelled;"
                " see why below.",
            )
        if self._link.down():
            return UploadResult(Outcome.UNREACHABLE, 0, tuple(skipped), 0, _UNREACHABLE)
        tq8 = build_tq8(cert, location, [c for _, c in ready], self._writer)
        try:
            reply = self._link.client.upload(tq8)
        except LotwRejectedError as error:
            self._link.mark(reachable=True)
            message = f"LoTW refused the upload: {error}"
            self._batches.note(actor, batch_id, message[:1000])
            return UploadResult(Outcome.REJECTED, 0, tuple(skipped), 0, message)
        except (LotwUnreachableError, LotwReplyError):
            self._link.mark(reachable=False)
            return UploadResult(Outcome.UNREACHABLE, 0, tuple(skipped), 0, _UNREACHABLE)
        self._link.mark(reachable=True)
        today = datetime.now(UTC).strftime("%Y%m%d")
        self._log.edit_many(
            [
                (
                    v.qso_id,
                    v.version,
                    with_fields(v.record, {"LOTW_QSL_SENT": "Y", "LOTW_QSLSDATE": today}),
                )
                for v, _ in ready
            ],
            actor=actor,
            note=f"Sent to LoTW in batch {batch_id}",
        )
        count = len(ready)
        answer = f"LoTW queued {count} QSO{'' if count == 1 else 's'}."
        if reply:
            answer += f" LoTW says: {reply.rstrip('.')}."
        self._batches.mark_sent(actor, batch_id, answer[:1000], [v.qso_id for v, _ in ready])
        message = (
            f"Batch {batch_id} sent. {answer} LoTW checks them over the next while; confirmations"
            " come back with Fetch confirmations."
        )
        return UploadResult(Outcome.DONE, count, tuple(skipped), 0, message)

    @staticmethod
    def _sign_ready(
        cert: CallsignCertificate, location: StationLocation, queue: tuple[QsoVersion, ...]
    ) -> tuple[list[tuple[QsoVersion, Contact]], list[Skipped]]:
        ready: list[tuple[QsoVersion, Contact]] = []
        skipped: list[Skipped] = []
        for version in queue:
            try:
                contact = contact_from_record(version.record)
            except NotSignableError as error:
                skipped.append(Skipped(version.qso_id, version.call, f"Not sent: {error}."))
                continue
            if not cert.covers(contact.day):
                skipped.append(
                    Skipped(
                        version.qso_id,
                        version.call,
                        f"Not sent: your certificate signs QSOs from {cert.qso_first}"
                        + ("" if cert.qso_last is None else f" to {cert.qso_last}")
                        + ".",
                    )
                )
                continue
            elsewhere = _where_made(version.record, location)
            if elsewhere is not None:
                skipped.append(Skipped(version.qso_id, version.call, f"Not sent: {elsewhere}."))
                continue
            ready.append((version, contact))
        return ready, skipped

    # -- Confirmations ----------------------------------------------------------

    def download(self, actor: Actor) -> DownloadResult:
        """Fetch new confirmations from LoTW and mark the QSOs they confirm."""
        account = self._accounts.get(actor)
        if account is None or account.username is None or account.password is None:
            return DownloadResult(
                Outcome.NOT_SET_UP, 0, 0, 0, "Save your LoTW website login to fetch confirmations."
            )
        try:
            cert = self._certificate(account)
        except CertificateError:
            cert = None
        if self._link.down():
            return DownloadResult(Outcome.UNREACHABLE, 0, 0, 0, _UNREACHABLE)
        own = None if cert is None else cert.call
        try:
            report = self._link.client.report(
                account.username, account.password, account.qsl_since or FIRST_FETCH, own
            )
        except LotwLoginError:
            self._link.mark(reachable=True)
            return DownloadResult(
                Outcome.LOGIN_FAILED, 0, 0, 0, "LoTW refused your website username and password."
            )
        except (LotwUnreachableError, LotwReplyError):
            self._link.mark(reachable=False)
            return DownloadResult(Outcome.UNREACHABLE, 0, 0, 0, _UNREACHABLE)
        self._link.mark(reachable=True)
        default_call = own or account.username.upper()
        confirmed, already, unmatched = self._mark_confirmed(actor, report.records, default_call)
        if report.last_qsl and _DAY.match(report.last_qsl):
            self._accounts.save_qsl_since(actor, report.last_qsl[:10])
        if confirmed == 0 and unmatched == 0:
            message = "No new confirmations from LoTW."
        else:
            message = f"{confirmed} QSO{'' if confirmed == 1 else 's'} newly confirmed by LoTW."
            if unmatched:
                message += (
                    f" {unmatched} confirmation{'' if unmatched == 1 else 's'} matched no QSO "
                    "in this log."
                )
        return DownloadResult(Outcome.DONE, confirmed, already, unmatched, message)

    def _mark_confirmed(
        self, actor: Actor, records: tuple[Record, ...], default_call: str
    ) -> tuple[int, int, int]:
        """Mark the QSOs ``records`` confirm; return how many newly, already, and unmatched."""
        t = tables()
        by_station: dict[str, list[Record]] = {}
        for record in records:
            if (record.get("QSL_RCVD") or "").strip().upper() != "Y":
                continue
            station = (
                record.get("STATION_CALLSIGN") or record.get("APP_LoTW_OWNCALL") or default_call
            )
            by_station.setdefault(station.strip().upper(), []).append(record)
        edits: list[tuple[uuid.UUID, int, Record]] = []
        used: set[uuid.UUID] = set()
        already = unmatched = 0
        for station, confirmations in by_station.items():
            calls = {(r.get("CALL") or "").strip().upper() for r in confirmations}
            qsos = self._log.station_qsos(station, calls)
            for confirmation in confirmations:
                match = _best_match(confirmation, qsos, used, t.group)
                if match is None:
                    unmatched += 1
                    continue
                used.add(match.qso_id)
                if (match.record.get("LOTW_QSL_RCVD") or "").strip().upper() == "Y":
                    already += 1
                    continue
                received = (confirmation.get("QSLRDATE") or "").strip()
                if not re.fullmatch(r"[0-9]{8}", received):
                    received = datetime.now(UTC).strftime("%Y%m%d")
                values = {"LOTW_QSL_RCVD": "Y", "LOTW_QSLRDATE": received}
                if (match.record.get("LOTW_QSL_SENT") or "").strip().upper() != "Y":
                    values["LOTW_QSL_SENT"] = "Y"
                edits.append((match.qso_id, match.version, with_fields(match.record, values)))
        changed = self._log.edit_many(edits, actor=actor, note="Confirmed on LoTW")
        return len(changed), already, unmatched


def _best_match(
    confirmation: Record,
    qsos: tuple[QsoVersion, ...],
    used: set[uuid.UUID],
    group: Callable[[str], str | None],
) -> QsoVersion | None:
    """The QSO a LoTW confirmation is for: same call, band and mode group, within 30 minutes.

    A QSO in exactly the confirmed mode wins over one in the same group, then
    the nearest in time.
    """
    when = _when(confirmation)
    call = (confirmation.get("CALL") or "").strip().upper()
    band = (confirmation.get("BAND") or "").strip().upper()
    mode = (confirmation.get("MODE") or "").strip().upper()
    wanted_group = (confirmation.get("APP_LoTW_MODEGROUP") or "").strip().upper() or group(mode)
    if when is None or not call or not band:
        return None
    best: tuple[int, timedelta, QsoVersion] | None = None
    for qso in qsos:
        if qso.qso_id in used or qso.call != call or (qso.band or "").upper() != band:
            continue
        start = qso.qso_start or _when(qso.record)
        if start is None or abs(start - when) > MATCH_WINDOW:
            continue
        ours = tables().lotw_mode(qso.record.get("MODE") or "", qso.record.get("SUBMODE"))
        if ours is None or (ours != mode and group(ours) != wanted_group):
            continue
        rank = (0 if ours == mode else 1, abs(start - when), qso)
        if best is None or rank[:2] < best[:2]:
            best = rank
    return None if best is None else best[2]
