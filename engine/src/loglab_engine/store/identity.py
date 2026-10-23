# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operators, their permissions, and signing in (Principle XIII).

Operators are local accounts held by the Engine; signing in needs no internet
connection (Principle V). An operator signs in at a station with their callsign
and a secret (a PIN or passphrase) and gets a session token. Every change to
the log is made in a live session, as an :class:`~loglab_engine.store.Actor`
that :meth:`IdentityStore.authenticate` returns for the token.

Secrets are stored only as salted scrypt hashes, and tokens only as SHA-256
digests, so a copy of the database cannot be used to sign in. Repeated wrong
secrets for a callsign lock it for a while. Account changes are made by an
administrator and kept forever, with who made them and when (Principle VII).

The database enforces the same rules (see migration ``0003_identity``): no
code path can record a change for an operator who is not signed in, or change
an account without an administrator's live session.

Transmit authorization is per operator and per radio; it comes with the
Station Agent's radios, and nothing here can grant it.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
import uuid
from dataclasses import dataclass
from datetime import datetime, timedelta
from enum import StrEnum
from typing import TYPE_CHECKING, Final

from psycopg import errors

from loglab_engine.store.log import (
    Actor,
    NotAuthorizedError,
    StoreError,
    check_callsign,
    check_station,
)

if TYPE_CHECKING:
    from collections.abc import Iterable

    import psycopg
    from psycopg.rows import TupleRow

MIN_SECRET: Final = 6
MAX_SECRET: Final = 128
SESSION_LIFETIME: Final = timedelta(hours=24)
MAX_FAILURES: Final = 5
LOCKOUT: Final = timedelta(minutes=15)

# scrypt with 16 MiB of memory: slow to guess, quick enough to sign in.
_SCRYPT_N: Final = 2**14
_SCRYPT_R: Final = 8
_SCRYPT_P: Final = 1
_SALT_BYTES: Final = 16
_HASH_BYTES: Final = 32
_TOKEN_BYTES: Final = 32
_WRONG: Final = "wrong callsign or secret"


class IdentityError(StoreError):
    """An identity request cannot be carried out; nothing was changed."""


class SignInError(IdentityError):
    """Signing in failed: wrong callsign or secret, disabled, or locked."""


class NotSignedInError(IdentityError):
    """The token is unknown, or its session has ended or expired."""


class Permission(StrEnum):
    """What an operator may do. Each is granted on its own; none implies another."""

    VIEW = "view"
    LOG = "log"
    RADIO = "radio"
    ADMIN = "admin"


@dataclass(frozen=True, slots=True)
class Session:
    """A sign-in: the token the client keeps, and who and where it is for."""

    token: str
    actor: Actor
    expires_at: datetime


@dataclass(frozen=True, slots=True)
class OperatorAccount:
    """An operator's account as it stands now; a disabled one keeps its grants."""

    callsign: str
    enabled: bool
    has_secret: bool
    permissions: frozenset[Permission]


def check_secret(secret: str) -> None:
    """Refuse a secret that is too short, too long, or not text a person can type.

    Raises:
        IdentityError: The secret is not acceptable.
    """
    if not MIN_SECRET <= len(secret) <= MAX_SECRET:
        msg = f"a PIN or passphrase is {MIN_SECRET} to {MAX_SECRET} characters"
        raise IdentityError(msg)
    if not secret.isprintable() or secret.strip() != secret:
        msg = "a PIN or passphrase cannot have control characters or start or end with a space"
        raise IdentityError(msg)


def hash_secret(secret: str) -> str:
    """A salted scrypt hash of ``secret``, as stored in the database."""
    salt = secrets.token_bytes(_SALT_BYTES)
    digest = _scrypt(secret, salt, _SCRYPT_N, _SCRYPT_R, _SCRYPT_P)
    return "$".join(
        (
            "scrypt",
            str(_SCRYPT_N),
            str(_SCRYPT_R),
            str(_SCRYPT_P),
            base64.b64encode(salt).decode(),
            base64.b64encode(digest).decode(),
        ),
    )


def verify_secret(secret: str, stored: str) -> bool:
    """Whether ``secret`` matches a hash made by :func:`hash_secret`."""
    parts = stored.split("$")
    if len(parts) != 6 or parts[0] != "scrypt":  # noqa: PLR2004 - the stored format
        return False
    try:
        n, r, p = int(parts[1]), int(parts[2]), int(parts[3])
        salt = base64.b64decode(parts[4], validate=True)
        expected = base64.b64decode(parts[5], validate=True)
    except ValueError:
        return False
    return hmac.compare_digest(_scrypt(secret, salt, n, r, p), expected)


def _scrypt(secret: str, salt: bytes, n: int, r: int, p: int) -> bytes:
    return hashlib.scrypt(
        secret.encode(),
        salt=salt,
        n=n,
        r=r,
        p=p,
        maxmem=256 * n * r,
        dklen=_HASH_BYTES,
    )


# Checked against when the callsign is unknown, so a wrong callsign takes as
# long as a wrong secret and does not reveal which callsigns exist.
_DECOY: Final = hash_secret(secrets.token_urlsafe(_TOKEN_BYTES))


def _token_digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


def _callsign(text: str) -> str:
    callsign = text.strip().upper()
    check_callsign(callsign)
    return callsign


class IdentityStore:
    """Operator accounts and sign-in, on one autocommit UTC PostgreSQL connection."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation; each method is one transaction."""
        if not conn.autocommit:
            msg = "IdentityStore needs an autocommit connection; it manages its own transactions"
            raise IdentityError(msg)
        self._conn = conn

    # -- Signing in -----------------------------------------------------------

    def sign_in(
        self,
        callsign: str,
        secret: str,
        station: str,
        lifetime: timedelta = SESSION_LIFETIME,
    ) -> Session:
        """Sign an operator in at a station and return the session's token.

        Every attempt is recorded. After :data:`MAX_FAILURES` failures within
        :data:`LOCKOUT`, the callsign is locked until the failures age out.

        Raises:
            SignInError: Wrong callsign or secret, disabled account, or locked.
            IdentityError: The callsign or station name is not valid.
        """
        call = _callsign(callsign)
        check_station(station)
        conn = self._conn
        if self._recent_failures(call) >= MAX_FAILURES:
            self._record_attempt(call, station, succeeded=False)
            minutes = int(LOCKOUT.total_seconds() // 60)
            msg = f"too many wrong attempts for {call}; wait {minutes} minutes and try again"
            raise SignInError(msg)
        row = conn.execute(
            """
            SELECT o.id, s.secret_hash, log.operator_enabled(o.id)
            FROM log.operator o
            LEFT JOIN LATERAL (
                SELECT secret_hash FROM log.operator_secret
                WHERE operator_id = o.id ORDER BY id DESC LIMIT 1
            ) s ON true
            WHERE o.callsign = %s
            """,
            (call,),
        ).fetchone()
        known = row is not None and row[1] is not None
        matches = verify_secret(secret, row[1] if known and row is not None else _DECOY)
        if row is None or not known or not matches:
            self._record_attempt(call, station, succeeded=False)
            raise SignInError(_WRONG)
        operator_id, _, enabled = row
        if not enabled:
            self._record_attempt(call, station, succeeded=False)
            msg = f"{call}'s account is disabled; ask an administrator"
            raise SignInError(msg)
        token = secrets.token_urlsafe(_TOKEN_BYTES)
        session_id = uuid.uuid4()
        with conn.transaction():
            self._record_attempt(call, station, succeeded=True)
            conn.execute(
                "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING",
                (station,),
            )
            result = conn.execute(
                """
                INSERT INTO log.session
                    (id, token_sha256, operator_id, station_id, started_at, expires_at)
                SELECT %s, %s, %s, s.id, transaction_timestamp(), transaction_timestamp() + %s
                FROM log.station s WHERE s.name = %s
                RETURNING expires_at
                """,
                (session_id, _token_digest(token), operator_id, lifetime, station),
            ).fetchone()
        if result is None:  # pragma: no cover - the station row was just ensured
            msg = "could not start the session"
            raise IdentityError(msg)
        return Session(token, Actor(call, station, session_id), result[0])

    def _recent_failures(self, callsign: str) -> int:
        """Failed attempts within the lockout window since the last success."""
        row = self._conn.execute(
            """
            SELECT count(*) FROM log.sign_in_attempt a
            WHERE a.callsign = %s
              AND NOT a.succeeded
              AND a.attempted_at > statement_timestamp() - %s
              AND a.id > coalesce(
                  (SELECT max(id) FROM log.sign_in_attempt
                   WHERE callsign = %s AND succeeded), 0)
            """,
            (callsign, LOCKOUT, callsign),
        ).fetchone()
        return 0 if row is None else int(row[0])

    def _record_attempt(self, callsign: str, station: str, *, succeeded: bool) -> None:
        self._conn.execute(
            """
            INSERT INTO log.sign_in_attempt (callsign, station, succeeded, attempted_at)
            VALUES (%s, %s, %s, transaction_timestamp())
            """,
            (callsign, station, succeeded),
        )

    def authenticate(self, token: str) -> Actor:
        """The signed-in operator and station for a session token.

        Raises:
            NotSignedInError: The token is unknown, or its session ended or expired.
        """
        row = self._conn.execute(
            """
            SELECT s.id, o.callsign, st.name
            FROM log.session s
            JOIN log.operator o ON o.id = s.operator_id
            JOIN log.station st ON st.id = s.station_id
            WHERE s.token_sha256 = %s AND log.session_live(s.id)
            """,
            (_token_digest(token),),
        ).fetchone()
        if row is None:
            msg = "not signed in, or the session has ended; sign in again"
            raise NotSignedInError(msg)
        session_id, callsign, station = row
        return Actor(callsign, station, session_id)

    def at_station(self, station: str) -> Actor | None:
        """Who is signed in at ``station`` now: the latest live session there, if any.

        A program on the station's computer (WSJT-X, through the Station
        Agent) logs as this operator, as if they had typed the QSO there.
        """
        row = self._conn.execute(
            """
            SELECT s.id, o.callsign, st.name
            FROM log.session s
            JOIN log.operator o ON o.id = s.operator_id
            JOIN log.station st ON st.id = s.station_id
            WHERE st.name = %s AND log.session_live(s.id)
            ORDER BY s.started_at DESC
            LIMIT 1
            """,
            (station,),
        ).fetchone()
        if row is None:
            return None
        session_id, callsign, name = row
        return Actor(callsign, name, session_id)

    def sign_out(self, token: str) -> None:
        """End the token's session. Signing out of an ended session does nothing."""
        self._conn.execute(
            """
            INSERT INTO log.session_end (session_id, ended_at, reason)
            SELECT id, transaction_timestamp(), 'signed out'
            FROM log.session WHERE token_sha256 = %s
            ON CONFLICT DO NOTHING
            """,
            (_token_digest(token),),
        )

    def may(self, actor: Actor, permission: Permission) -> bool:
        """Whether the actor's session is live and its operator holds ``permission``."""
        row = self._conn.execute(
            """
            SELECT log.session_live(s.id) AND log.operator_may(s.operator_id, %s)
            FROM log.session s
            JOIN log.operator o ON o.id = s.operator_id
            WHERE s.id = %s AND o.callsign = %s
            """,
            (permission.value, actor.session_id, actor.operator),
        ).fetchone()
        return row is not None and bool(row[0])

    def require(self, actor: Actor, permission: Permission) -> None:
        """Refuse unless the actor is signed in and holds ``permission``.

        Raises:
            NotAuthorizedError: The actor's session is not live or lacks the permission.
        """
        if not self.may(actor, permission):
            msg = f"{actor.operator} does not have the {permission.value} permission"
            raise NotAuthorizedError(msg)

    # -- Accounts -------------------------------------------------------------

    def enroll_first_admin(self, callsign: str, secret: str) -> None:
        """Create the first administrator, with every permission this module can grant.

        Only possible while no operator holds admin; afterwards, an
        administrator adds operators with :meth:`add_operator`.

        Raises:
            IdentityError: An administrator already exists, or the input is invalid.
        """
        call = _callsign(callsign)
        check_secret(secret)
        conn = self._conn
        with conn.transaction():
            conn.execute("LOCK TABLE log.operator_permission IN EXCLUSIVE MODE")
            if self._any_admin():
                msg = "an administrator already exists; ask them to add you"
                raise IdentityError(msg)
            operator_id = self._ensure_operator(call)
            self._insert_secret(operator_id, hash_secret(secret), None)
            self._insert_permissions(operator_id, Permission, granted=True, by=None)

    def add_operator(
        self,
        admin: Actor,
        callsign: str,
        secret: str,
        permissions: Iterable[Permission],
    ) -> None:
        """Add an operator (or give a secret to one known only from the log).

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            IdentityError: The operator already has a secret, or the input is invalid.
        """
        call = _callsign(callsign)
        check_secret(secret)
        wanted = frozenset(permissions)
        conn = self._conn
        with conn.transaction():
            self.require(admin, Permission.ADMIN)
            operator_id = self._ensure_operator(call)
            existing = conn.execute(
                "SELECT 1 FROM log.operator_secret WHERE operator_id = %s LIMIT 1",
                (operator_id,),
            ).fetchone()
            if existing is not None:
                msg = f"{call} already has an account"
                raise IdentityError(msg)
            by = (self._operator_id(admin.operator), admin.session_id)
            self._insert_secret(operator_id, hash_secret(secret), by)
            self._insert_permissions(operator_id, wanted, granted=True, by=by)

    def set_secret(self, actor: Actor, callsign: str, secret: str) -> None:
        """Change an operator's secret: your own, or anyone's as an administrator.

        Raises:
            NotAuthorizedError: Not signed in, or changing someone else's without admin.
            IdentityError: No such operator, or the secret is invalid.
        """
        call = _callsign(callsign)
        check_secret(secret)
        with self._conn.transaction():
            if call != actor.operator:
                self.require(actor, Permission.ADMIN)
            operator_id = self._existing_operator(call)
            by = (self._operator_id(actor.operator), actor.session_id)
            self._insert_secret(operator_id, hash_secret(secret), by)

    def set_permissions(
        self,
        admin: Actor,
        callsign: str,
        permissions: Iterable[Permission],
        *,
        granted: bool,
    ) -> None:
        """Grant or revoke permissions.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            IdentityError: No such operator, or an administrator removing their own admin.
        """
        call = _callsign(callsign)
        with self._conn.transaction():
            self.require(admin, Permission.ADMIN)
            operator_id = self._existing_operator(call)
            by = (self._operator_id(admin.operator), admin.session_id)
            self._insert_permissions(operator_id, frozenset(permissions), granted=granted, by=by)

    def set_enabled(self, admin: Actor, callsign: str, *, enabled: bool) -> None:
        """Enable or disable an account. Disabling ends every session it has.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            IdentityError: No such operator, or an administrator disabling themselves.
        """
        call = _callsign(callsign)
        conn = self._conn
        with conn.transaction():
            self.require(admin, Permission.ADMIN)
            operator_id = self._existing_operator(call)
            by = self._operator_id(admin.operator)
            try:
                conn.execute(
                    """
                    INSERT INTO log.operator_status
                        (operator_id, enabled, changed_by, session_id, changed_at)
                    VALUES (%s, %s, %s, %s, transaction_timestamp())
                    """,
                    (operator_id, enabled, by, admin.session_id),
                )
            except errors.CheckViolation as error:
                raise IdentityError(error.diag.message_primary or str(error)) from error
            if not enabled:
                conn.execute(
                    """
                    INSERT INTO log.session_end (session_id, ended_at, reason)
                    SELECT id, transaction_timestamp(), 'revoked'
                    FROM log.session WHERE operator_id = %s
                    ON CONFLICT DO NOTHING
                    """,
                    (operator_id,),
                )

    def accounts(self) -> tuple[OperatorAccount, ...]:
        """Every operator, in callsign order."""
        rows = self._conn.execute(
            """
            SELECT o.callsign,
                   log.operator_enabled(o.id),
                   EXISTS (SELECT 1 FROM log.operator_secret s WHERE s.operator_id = o.id),
                   array(SELECT latest.permission FROM (
                             SELECT DISTINCT ON (g.permission) g.permission, g.granted
                             FROM log.operator_permission g
                             WHERE g.operator_id = o.id
                             ORDER BY g.permission, g.id DESC
                         ) latest WHERE latest.granted)
            FROM log.operator o
            ORDER BY o.callsign
            """,
        ).fetchall()
        return tuple(
            OperatorAccount(
                callsign,
                enabled,
                has_secret,
                frozenset(Permission(p) for p in permissions),
            )
            for callsign, enabled, has_secret, permissions in rows
        )

    # -- Internals ------------------------------------------------------------

    def _any_admin(self) -> bool:
        row = self._conn.execute("SELECT log.any_admin()").fetchone()
        return row is not None and bool(row[0])

    def _ensure_operator(self, callsign: str) -> int:
        self._conn.execute(
            "INSERT INTO log.operator (callsign) VALUES (%s) ON CONFLICT DO NOTHING",
            (callsign,),
        )
        return self._existing_operator(callsign)

    def _existing_operator(self, callsign: str) -> int:
        row = self._conn.execute(
            "SELECT id FROM log.operator WHERE callsign = %s",
            (callsign,),
        ).fetchone()
        if row is None:
            msg = f"there is no operator {callsign}"
            raise IdentityError(msg)
        operator_id: int = row[0]
        return operator_id

    def _operator_id(self, callsign: str) -> int:
        return self._existing_operator(callsign)

    def _insert_secret(
        self,
        operator_id: int,
        secret_hash: str,
        by: tuple[int, uuid.UUID] | None,
    ) -> None:
        self._conn.execute(
            """
            INSERT INTO log.operator_secret
                (operator_id, secret_hash, changed_by, session_id, changed_at)
            VALUES (%s, %s, %s, %s, transaction_timestamp())
            """,
            (operator_id, secret_hash, *(by or (None, None))),
        )

    def _insert_permissions(
        self,
        operator_id: int,
        permissions: Iterable[Permission],
        *,
        granted: bool,
        by: tuple[int, uuid.UUID] | None,
    ) -> None:
        changed_by, session_id = by or (None, None)
        try:
            with self._conn.cursor() as cur:
                cur.executemany(
                    """
                    INSERT INTO log.operator_permission
                        (operator_id, permission, granted, changed_by, session_id, changed_at)
                    VALUES (%s, %s, %s, %s, %s, transaction_timestamp())
                    """,
                    [
                        (operator_id, p.value, granted, changed_by, session_id)
                        # admin last: the first administrator's rows name nobody, which
                        # the database allows only until an administrator exists.
                        for p in sorted(permissions, key=lambda p: (p is Permission.ADMIN, p))
                    ],
                )
        except errors.CheckViolation as error:
            raise IdentityError(error.diag.message_primary or str(error)) from error
