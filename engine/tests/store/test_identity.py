# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operators and sign-in: identified, authorized, and enforced by the database (XXI, XVIII)."""

from __future__ import annotations

import time
import uuid
from datetime import timedelta
from typing import TYPE_CHECKING

import psycopg
import pytest

from loglab_engine.adif import Field, Record
from loglab_engine.store import (
    Actor,
    IdentityError,
    IdentityStore,
    LogStore,
    NotAuthorizedError,
    NotSignedInError,
    Permission,
    SignInError,
)
from loglab_engine.store.identity import MAX_FAILURES, hash_secret, verify_secret

from .conftest import ADMIN_SECRET, VISITOR_SECRET

if TYPE_CHECKING:
    from psycopg.rows import TupleRow

QSO = Record((Field("CALL", "Q1ABC"), Field("QSO_DATE", "20260627"), Field("TIME_ON", "1805")))


def _scalar(conn: psycopg.Connection[TupleRow], query: str) -> object:
    row = conn.execute(query.encode()).fetchone()
    assert row is not None
    value: object = row[0]
    return value


# -- Secrets ------------------------------------------------------------------


def test_secrets_are_stored_only_as_salted_hashes() -> None:
    """The same secret hashes differently each time, and only the right one verifies."""
    first, second = hash_secret("246810"), hash_secret("246810")
    assert first != second
    assert first.startswith("scrypt$")
    assert "246810" not in first
    assert verify_secret("246810", first)
    assert verify_secret("246810", second)
    assert not verify_secret("246811", first)
    assert not verify_secret("246810", "not-a-hash")
    assert not verify_secret("246810", "scrypt$x$8$1$AAAA$AAAA")


@pytest.mark.parametrize("secret", ["12345", "x" * 129, " 123456", "123456\n", "12\x0034567"])
def test_unusable_secrets_are_refused(identity: IdentityStore, secret: str) -> None:
    """Too short, too long, padded, or with control characters."""
    with pytest.raises(IdentityError, match="PIN or passphrase"):
        identity.enroll_first_admin("K9KSU", secret)


def test_nothing_in_the_database_can_be_used_to_sign_in(
    identity: IdentityStore,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Neither the secret nor the session token is stored as itself."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    session = identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    dump = str(conn.execute("SELECT * FROM log.operator_secret, log.session").fetchall())
    assert ADMIN_SECRET not in dump
    assert session.token not in dump


# -- Signing in ---------------------------------------------------------------


def test_sign_in_gives_a_session_for_that_operator_and_station(identity: IdentityStore) -> None:
    """The token authenticates as the operator, at the station they signed in at."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    session = identity.sign_in("k9ksu", ADMIN_SECRET, "shack")
    assert session.actor.operator == "K9KSU"
    assert session.actor.station == "shack"
    assert identity.authenticate(session.token) == session.actor
    assert session.expires_at.utcoffset() == timedelta(0)  # Principle VIII


def test_a_wrong_secret_and_an_unknown_callsign_look_the_same(
    identity: IdentityStore,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Failing to sign in does not reveal which callsigns exist; every try is recorded."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    with pytest.raises(SignInError) as wrong:
        identity.sign_in("K9KSU", "999999", "shack")
    with pytest.raises(SignInError) as unknown:
        identity.sign_in("N0BODY", ADMIN_SECRET, "shack")
    assert str(wrong.value) == str(unknown.value) == "wrong callsign or secret"
    assert _scalar(conn, "SELECT count(*) FROM log.sign_in_attempt WHERE NOT succeeded") == 2
    assert _scalar(conn, "SELECT count(*) FROM log.session") == 0


def test_repeated_wrong_secrets_lock_the_callsign(identity: IdentityStore) -> None:
    """After too many failures even the right secret is refused for a while."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    for _ in range(MAX_FAILURES):
        with pytest.raises(SignInError, match="wrong"):
            identity.sign_in("K9KSU", "000000", "shack")
    with pytest.raises(SignInError, match="too many wrong attempts"):
        identity.sign_in("K9KSU", ADMIN_SECRET, "shack")


def test_a_success_clears_earlier_failures(identity: IdentityStore) -> None:
    """Typos before a good sign-in do not count toward the next lock."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    for _ in range(MAX_FAILURES - 1):
        with pytest.raises(SignInError):
            identity.sign_in("K9KSU", "000000", "shack")
    identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    with pytest.raises(SignInError, match="wrong"):
        identity.sign_in("K9KSU", "000000", "shack")
    identity.sign_in("K9KSU", ADMIN_SECRET, "shack")


def test_sign_out_and_expiry_end_a_session(identity: IdentityStore, store: LogStore) -> None:
    """An ended or expired session authenticates as nobody and can change nothing."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    session = identity.sign_in("K9KSU", ADMIN_SECRET, "shack")
    identity.sign_out(session.token)
    identity.sign_out(session.token)  # twice is harmless
    with pytest.raises(NotSignedInError):
        identity.authenticate(session.token)
    with pytest.raises(NotAuthorizedError, match="signed-in operator"):
        store.create_qso(QSO, actor=session.actor)

    brief = identity.sign_in("K9KSU", ADMIN_SECRET, "shack", lifetime=timedelta(milliseconds=50))
    time.sleep(0.1)
    with pytest.raises(NotSignedInError):
        identity.authenticate(brief.token)
    with pytest.raises(NotAuthorizedError):
        store.create_qso(QSO, actor=brief.actor)


def test_a_made_up_actor_is_refused_by_the_database(
    shack: Actor,
    visitor: Actor,
    store: LogStore,
) -> None:
    """Naming an operator is not enough: the change must come from their own live session."""
    forgeries = [
        Actor("K9KSU", "shack", uuid.uuid4()),  # no such session
        Actor("K9KSU", "shack", visitor.session_id),  # someone else's session
        Actor("W0XYZ", "shack", visitor.session_id),  # the wrong station
    ]
    for forged in forgeries:
        with pytest.raises(NotAuthorizedError):
            store.create_qso(QSO, actor=forged)
    assert store.find() == ()
    store.create_qso(QSO, actor=shack)


# -- Permissions --------------------------------------------------------------


def test_the_first_administrator_can_be_enrolled_only_once(identity: IdentityStore) -> None:
    """After that, accounts come from an administrator."""
    identity.enroll_first_admin("K9KSU", ADMIN_SECRET)
    with pytest.raises(IdentityError, match="already exists"):
        identity.enroll_first_admin("W0XYZ", VISITOR_SECRET)
    (account,) = identity.accounts()
    assert account.permissions == {
        Permission.VIEW,
        Permission.LOG,
        Permission.RADIO,
        Permission.ADMIN,
    }


def test_logging_never_grants_radio_control_or_administration(
    identity: IdentityStore,
    visitor: Actor,
) -> None:
    """Each permission is granted on its own (Principle XXI)."""
    assert identity.may(visitor, Permission.LOG)
    assert identity.may(visitor, Permission.VIEW)
    assert not identity.may(visitor, Permission.RADIO)
    assert not identity.may(visitor, Permission.ADMIN)
    assert "transmit" not in {p.value for p in Permission}


def test_only_administrators_change_accounts(
    identity: IdentityStore,
    shack: Actor,
    visitor: Actor,
) -> None:
    """A logging operator cannot add operators or grant themselves anything."""
    with pytest.raises(NotAuthorizedError, match="admin"):
        identity.add_operator(visitor, "N0NEW", "135791", [Permission.LOG])
    with pytest.raises(NotAuthorizedError, match="admin"):
        identity.set_permissions(visitor, "W0XYZ", [Permission.ADMIN], granted=True)
    with pytest.raises(NotAuthorizedError, match="admin"):
        identity.set_enabled(visitor, "K9KSU", enabled=False)
    with pytest.raises(NotAuthorizedError, match="admin"):
        identity.set_secret(visitor, "K9KSU", "135791")
    identity.set_permissions(shack, "W0XYZ", [Permission.RADIO], granted=True)
    assert identity.may(visitor, Permission.RADIO)


def test_operators_change_their_own_secret(identity: IdentityStore, visitor: Actor) -> None:
    """No administrator is needed for your own PIN; the old one stops working."""
    identity.set_secret(visitor, "W0XYZ", "975310")
    with pytest.raises(SignInError):
        identity.sign_in("W0XYZ", VISITOR_SECRET, "fd-2")
    identity.sign_in("W0XYZ", "975310", "fd-2")


def test_revoking_log_stops_changes_at_once(
    identity: IdentityStore,
    shack: Actor,
    visitor: Actor,
    store: LogStore,
) -> None:
    """A session already open loses the permission the moment it is revoked."""
    store.create_qso(QSO, actor=visitor)
    identity.set_permissions(shack, "W0XYZ", [Permission.LOG], granted=False)
    with pytest.raises(NotAuthorizedError, match="permission to log"):
        store.create_qso(QSO, actor=visitor)


def test_disabling_an_account_ends_its_sessions(
    identity: IdentityStore,
    shack: Actor,
    visitor: Actor,
    store: LogStore,
) -> None:
    """A disabled operator cannot change the log or sign in until enabled again."""
    identity.set_enabled(shack, "W0XYZ", enabled=False)
    with pytest.raises(NotAuthorizedError):
        store.create_qso(QSO, actor=visitor)
    with pytest.raises(SignInError, match="disabled"):
        identity.sign_in("W0XYZ", VISITOR_SECRET, "fd-2")
    identity.set_enabled(shack, "W0XYZ", enabled=True)
    store.create_qso(QSO, actor=identity.sign_in("W0XYZ", VISITOR_SECRET, "fd-2").actor)


def test_the_last_administrator_cannot_lock_everyone_out(
    identity: IdentityStore,
    shack: Actor,
) -> None:
    """An administrator cannot disable themselves or drop their own admin."""
    with pytest.raises(IdentityError, match="their own account"):
        identity.set_enabled(shack, "K9KSU", enabled=False)
    with pytest.raises(IdentityError, match="their own admin"):
        identity.set_permissions(shack, "K9KSU", [Permission.ADMIN], granted=False)
    assert identity.may(shack, Permission.ADMIN)


# -- The database enforces it, not just this code (Principle XVIII) ----------


@pytest.mark.usefixtures("visitor")
def test_accounts_cannot_be_changed_with_sql_behind_an_administrators_back(
    conn: psycopg.Connection[TupleRow],
) -> None:
    """Direct SQL that grants, rewrites history, or names nobody is refused."""
    w0xyz = _scalar(conn, "SELECT id FROM log.operator WHERE callsign = 'W0XYZ'")
    visitor_session = _scalar(
        conn,
        "SELECT id FROM log.session WHERE operator_id = "
        "(SELECT id FROM log.operator WHERE callsign = 'W0XYZ')",
    )
    admin_session = _scalar(conn, "SELECT id FROM log.session WHERE operator_id = 1")
    grant = (
        "INSERT INTO log.operator_permission "
        "(operator_id, permission, granted, changed_by, session_id, changed_at) "
        "VALUES (%s, %s, true, %s, %s, now())"
    )
    attempts: list[tuple[str, tuple[object, ...], str]] = [
        (grant, (w0xyz, "admin", None, None), "must name the administrator"),
        (grant, (w0xyz, "admin", w0xyz, visitor_session), "only an administrator"),
        (grant, (w0xyz, "admin", 1, uuid.uuid4()), "signed in"),
        (grant, (w0xyz, "transmit", 1, admin_session), "permission_check"),
        ("UPDATE log.operator_permission SET granted = true", (), "append-only"),
        ("DELETE FROM log.operator_secret", (), "append-only"),
        ("UPDATE log.session SET expires_at = expires_at + interval '1 year'", (), "append-only"),
        ("TRUNCATE log.session_end", (), "append-only"),
        ("DELETE FROM log.sign_in_attempt", (), "append-only"),
        ("TRUNCATE log.operator_permission CASCADE", (), "append-only"),
    ]
    for statement, params, error in attempts:
        with pytest.raises(psycopg.Error, match=error):
            conn.execute(statement.encode(), params)
    row = conn.execute("SELECT log.operator_may(%s, 'admin')", (w0xyz,)).fetchone()
    assert row == (False,)


def test_a_new_change_must_name_a_session(
    shack: Actor,
    conn: psycopg.Connection[TupleRow],
) -> None:
    """A changeset without a session is refused, even with a real operator and station."""
    del shack
    with pytest.raises(psycopg.errors.InsufficientPrivilege, match="signed-in operator"):
        conn.execute(
            """
            INSERT INTO log.changeset (operator_id, station_id, kind)
            SELECT o.id, s.id, 'create' FROM log.operator o, log.station s LIMIT 1
            """,
        )
