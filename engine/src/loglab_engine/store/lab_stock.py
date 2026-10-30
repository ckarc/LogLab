# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Lab Stock: the lab's everyday settings for each radio model, behind a gate.

A **proposal** is a whole set of settings for one maker and model, sent by a
Station Agent from the radio in front of it, or proposed again by an
administrator from an earlier proposal. It changes nothing by itself.

The **gate**: a signed-in administrator approves or refuses each proposal,
once (migration ``0022_lab_stock``, enforced by the database whatever code
asks). The model's **Lab Stock** is the proposal approved most recently. A
station key can propose but never decide. Every proposal and decision is
kept (Principle VII).

The Engine only keeps settings. Putting Lab Stock on a radio is the Agent's,
only when an operator there asks and confirms (Principle X).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import TYPE_CHECKING, Final

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError, StoreError, check_station

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor

_SERIAL: Final = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
_ID: Final = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
_BYTES: Final = re.compile(r"^[0-9A-F]{2}( [0-9A-F]{2}){0,63}$")
_PLAIN: Final = re.compile(r"^[^\x00-\x1f\x7f]*$")
# The most settings one proposal may hold; an IC-7300 has 260.
MAX_SETTINGS: Final = 2000
MAX_NAME: Final = 40
MAX_LABEL: Final = 200
MAX_NOTE: Final = 500


class LabStockNotFoundError(StoreError):
    """No Lab Stock proposal has that number."""


class DecidedError(StoreError):
    """The proposal was already approved or refused; a decision is made once."""


class LabStockMovedError(StoreError):
    """The model's Lab Stock changed since the one the administrator compared against."""


class Status(StrEnum):
    """Where a proposal stands."""

    PROPOSED = "proposed"
    APPROVED = "approved"
    REFUSED = "refused"


@dataclass(frozen=True, slots=True)
class StockSetting:
    """One setting of a Lab Stock, as the radio sent it."""

    id: str
    label: str
    bytes: str
    # The value in words, as the Agent showed it, like "USB"; for reading.
    shown: str | None


@dataclass(frozen=True, slots=True)
class Decision:
    """An administrator's approval or refusal of a proposal."""

    approved: bool
    by: str
    at: datetime


@dataclass(frozen=True, slots=True)
class Proposal:
    """A proposed Lab Stock for one model, and what became of it."""

    id: int
    maker: str
    model: str
    settings: tuple[StockSetting, ...]
    note: str | None
    # The station whose Agent sent it and the radio read, if an Agent did.
    station: str | None
    serial: str | None
    # The proposal it is a copy of, if an administrator proposed it again.
    again_of: int | None
    # Who was signed in: at the station, or the administrator.
    proposed_by: str | None
    proposed_at: datetime
    decision: Decision | None
    # Whether it is the model's Lab Stock now.
    current: bool

    @property
    def status(self) -> Status:
        """Proposed, approved or refused."""
        if self.decision is None:
            return Status.PROPOSED
        return Status.APPROVED if self.decision.approved else Status.REFUSED


def _check_model(maker: str, model: str) -> None:
    for field, value in (("maker", maker), ("model", model)):
        if not 1 <= len(value) <= MAX_NAME or not _PLAIN.match(value):
            msg = f"the radio's {field} must be 1-{MAX_NAME} plain characters"
            raise StoreError(msg)


def check_settings(
    maker: str, model: str, serial: str, note: str | None, settings: tuple[StockSetting, ...]
) -> None:
    """Refuse settings read from a radio, or a note, that can't be kept.

    Raises:
        StoreError: Something in them is not valid.
    """
    _check_model(maker, model)
    if not _SERIAL.match(serial):
        msg = "the radio's serial number must be 1-64 letters, digits, '.', '_' or '-'"
        raise StoreError(msg)
    if note is not None and (not 1 <= len(note) <= MAX_NOTE or not _PLAIN.match(note)):
        msg = f"a note must be 1-{MAX_NOTE} plain characters"
        raise StoreError(msg)
    if not 1 <= len(settings) <= MAX_SETTINGS:
        msg = f"a Lab Stock holds 1 to {MAX_SETTINGS} settings"
        raise StoreError(msg)
    seen: set[str] = set()
    for setting in settings:
        if not _ID.match(setting.id) or setting.id in seen:
            msg = f"setting id {setting.id!r} is not valid, or is there twice"
            raise StoreError(msg)
        seen.add(setting.id)
        for text in (setting.label, setting.shown):
            if text is not None and (not 1 <= len(text) <= MAX_LABEL or not _PLAIN.match(text)):
                msg = f"setting {setting.id!r}: its label and value need 1-{MAX_LABEL} characters"
                raise StoreError(msg)
        if not _BYTES.match(setting.bytes):
            msg = f"setting {setting.id!r}: bytes must be hex pairs like '00 67'"
            raise StoreError(msg)


def _json(settings: tuple[StockSetting, ...]) -> Jsonb:
    return Jsonb(
        [{"id": s.id, "label": s.label, "bytes": s.bytes, "shown": s.shown} for s in settings]
    )


class LabStockStore:
    """Proposing, deciding and finding Lab Stock, on one autocommit connection."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation; each method is one transaction."""
        self._conn = conn

    def propose(  # noqa: PLR0913 - each is a separate fact about the proposal
        self,
        station: str,
        maker: str,
        model: str,
        serial: str,
        settings: tuple[StockSetting, ...],
        *,
        note: str | None = None,
    ) -> Proposal:
        """Keep the settings ``station``'s Agent read from a radio, as a proposal.

        Raises:
            StoreError: Something in it is not valid.
        """
        check_station(station)
        check_settings(maker, model, serial, note, settings)
        conn = self._conn
        with conn.transaction():
            conn.execute(
                "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING", (station,)
            )
            row = conn.execute(
                """
                INSERT INTO log.lab_stock
                    (maker, model, settings, note, station_id, serial, session_id)
                SELECT %s, %s, %s, %s, st.id, %s,
                    (SELECT s.id FROM log.session s
                     WHERE s.station_id = st.id AND log.session_live(s.id)
                     ORDER BY s.started_at DESC LIMIT 1)
                FROM log.station st WHERE st.name = %s
                RETURNING id
                """,
                (maker, model, _json(settings), note, serial, station),
            ).fetchone()
            if row is None:  # pragma: no cover - the station was just made
                msg = f"station {station!r} is missing"
                raise StoreError(msg)
        return self.get(int(row[0]))

    def propose_again(self, admin: Actor, proposal_id: int) -> Proposal:
        """Propose an earlier proposal's settings again, as a new proposal.

        It still needs an approval, like any other.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            LabStockNotFoundError: No such proposal.
        """
        conn = self._conn
        try:
            with conn.transaction():
                row = conn.execute(
                    """
                    INSERT INTO log.lab_stock (maker, model, settings, note, again_of, session_id)
                    SELECT maker, model, settings, %s, id, %s FROM log.lab_stock WHERE id = %s
                    RETURNING id
                    """,
                    (f"Lab Stock #{proposal_id} again", admin.session_id, proposal_id),
                ).fetchone()
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error
        if row is None:
            msg = f"no Lab Stock proposal #{proposal_id}"
            raise LabStockNotFoundError(msg)
        return self.get(int(row[0]))

    def decide(
        self, admin: Actor, proposal_id: int, *, approve: bool, against: int | None = None
    ) -> Proposal:
        """Approve or refuse a proposal, once.

        Approving makes it the model's Lab Stock. ``against`` is the Lab Stock
        the administrator compared it with (None: there was none); if the
        model's Lab Stock has changed since, nothing is decided, so nobody
        approves a change they did not see.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            LabStockNotFoundError: No such proposal.
            DecidedError: It was already approved or refused.
            LabStockMovedError: The model's Lab Stock is not ``against`` any more.
        """
        conn = self._conn
        try:
            with conn.transaction():
                found = conn.execute(
                    "SELECT maker, model FROM log.lab_stock WHERE id = %s", (proposal_id,)
                ).fetchone()
                if found is None:
                    msg = f"no Lab Stock proposal #{proposal_id}"
                    raise LabStockNotFoundError(msg)
                maker, model = str(found[0]), str(found[1])
                # One decision at a time for a model, so two administrators
                # can't each approve against the same Lab Stock.
                conn.execute(
                    "SELECT pg_advisory_xact_lock(hashtext('lab_stock'), hashtext(%s))",
                    (f"{maker}\n{model}",),
                )
                decided = conn.execute(
                    "SELECT 1 FROM log.lab_stock_decision WHERE proposal_id = %s", (proposal_id,)
                ).fetchone()
                if decided is not None:
                    msg = f"Lab Stock proposal #{proposal_id} was already decided"
                    raise DecidedError(msg)
                if approve:
                    now = self._current_id(maker, model)
                    if now != against:
                        msg = (
                            f"the {maker} {model} Lab Stock changed since this was compared "
                            f"(it is now {'#' + str(now) if now else 'none'}); look again"
                        )
                        raise LabStockMovedError(msg)
                row = conn.execute(
                    """
                    INSERT INTO log.lab_stock_decision
                        (proposal_id, approved, decided_by, session_id)
                    SELECT %s, %s, s.operator_id, s.id FROM log.session s WHERE s.id = %s
                    RETURNING proposal_id
                    """,
                    (proposal_id, approve, admin.session_id),
                ).fetchone()
                if row is None:
                    msg = "only a signed-in administrator can approve or refuse Lab Stock"
                    raise NotAuthorizedError(msg)
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error
        return self.get(proposal_id)

    def current(self, maker: str, model: str) -> Proposal | None:
        """The model's Lab Stock: the proposal approved most recently, if any."""
        _check_model(maker, model)
        found = self._current_id(maker, model)
        return None if found is None else self.get(found)

    def get(self, proposal_id: int) -> Proposal:
        """One proposal.

        Raises:
            LabStockNotFoundError: No such proposal.
        """
        found = self._proposals("p.id = %s", (proposal_id,))
        if not found:
            msg = f"no Lab Stock proposal #{proposal_id}"
            raise LabStockNotFoundError(msg)
        return found[0]

    def all(self) -> tuple[Proposal, ...]:
        """Every proposal, newest first."""
        return self._proposals("TRUE", ())

    def _current_id(self, maker: str, model: str) -> int | None:
        row = self._conn.execute(
            """
            SELECT p.id FROM log.lab_stock p
            JOIN log.lab_stock_decision d ON d.proposal_id = p.id AND d.approved
            WHERE p.maker = %s AND p.model = %s
            ORDER BY d.decided_at DESC, p.id DESC LIMIT 1
            """,
            (maker, model),
        ).fetchone()
        return None if row is None else int(row[0])

    def _proposals(self, where: str, params: tuple[object, ...]) -> tuple[Proposal, ...]:
        rows = self._conn.execute(
            f"""
            SELECT p.id, p.maker, p.model, p.settings, p.note, st.name, p.serial, p.again_of,
                po.callsign, p.proposed_at, d.approved, dop.callsign, d.decided_at,
                p.id = (SELECT c.id FROM log.lab_stock c
                        JOIN log.lab_stock_decision cd ON cd.proposal_id = c.id AND cd.approved
                        WHERE c.maker = p.maker AND c.model = p.model
                        ORDER BY cd.decided_at DESC, c.id DESC LIMIT 1)
            FROM log.lab_stock p
            LEFT JOIN log.station st ON st.id = p.station_id
            LEFT JOIN log.session ps ON ps.id = p.session_id
            LEFT JOIN log.operator po ON po.id = ps.operator_id
            LEFT JOIN log.lab_stock_decision d ON d.proposal_id = p.id
            LEFT JOIN log.operator dop ON dop.id = d.decided_by
            WHERE {where}
            ORDER BY p.id DESC
            """,  # noqa: S608 - only this module's fixed conditions are formatted in
            params,
        ).fetchall()
        return tuple(
            Proposal(
                id=int(pid),
                maker=str(maker),
                model=str(model),
                settings=tuple(
                    StockSetting(
                        id=str(s["id"]),
                        label=str(s["label"]),
                        bytes=str(s["bytes"]),
                        shown=None if s.get("shown") is None else str(s["shown"]),
                    )
                    for s in settings
                ),
                note=None if note is None else str(note),
                station=None if station is None else str(station),
                serial=None if serial is None else str(serial),
                again_of=None if again_of is None else int(again_of),
                proposed_by=None if by is None else str(by),
                proposed_at=proposed_at,
                decision=None
                if approved is None
                else Decision(approved=bool(approved), by=str(decider), at=decided_at),
                current=bool(current),
            )
            for (
                pid,
                maker,
                model,
                settings,
                note,
                station,
                serial,
                again_of,
                by,
                proposed_at,
                approved,
                decider,
                decided_at,
                current,
            ) in rows
        )


__all__ = [
    "MAX_SETTINGS",
    "DecidedError",
    "Decision",
    "LabStockMovedError",
    "LabStockNotFoundError",
    "LabStockStore",
    "Proposal",
    "Status",
    "StockSetting",
    "check_settings",
]
