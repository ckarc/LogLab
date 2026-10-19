# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""``loglab-operator``: operator accounts and signing in, from the command line.

Set up the first administrator, sign in at a station and out again, and, as an
administrator, add operators, grant and revoke permissions, disable accounts,
and make the station keys Station Agents sign in with (Principle XIII).
Everything works with no internet connection (Principle V).

Signing in keeps the session's token in a file only you can read
(``$LOGLAB_SESSION_FILE``, else ``~/.local/state/loglab/session``), and
``loglab-log`` uses it for every change it makes. Secrets are typed at a prompt
and never shown or stored.

Exit codes: 0 success, 2 refused or failed.
"""

from __future__ import annotations

import argparse
import getpass
import os
import sys
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING, Final, TextIO

import psycopg

from loglab_engine import __version__
from loglab_engine.store import (
    Actor,
    IdentityStore,
    MigrationError,
    Permission,
    StoreError,
    connect,
    default_database,
    load_migrations,
    pending_migrations,
)
from loglab_engine.store.agent import AgentKeyStore

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from psycopg.rows import TupleRow

EXIT_OK: Final = 0
EXIT_REFUSED: Final = 2
SESSION_FILE_ENV: Final = "LOGLAB_SESSION_FILE"
DEFAULT_GRANTS: Final = (Permission.VIEW, Permission.LOG)


class RefusedError(Exception):
    """A command cannot go ahead; the message says why."""


# -- The session file ---------------------------------------------------------


def session_file() -> Path:
    """Where the signed-in session's token is kept."""
    configured = os.environ.get(SESSION_FILE_ENV)
    if configured:
        return Path(configured)
    return Path.home() / ".local" / "state" / "loglab" / "session"


def save_token(token: str) -> Path:
    """Keep ``token`` in the session file, readable only by you, written whole (Principle XIV)."""
    path = session_file().resolve()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    temp = Path(temp_name)
    try:
        with os.fdopen(fd, "w", encoding="ascii") as file:  # mkstemp makes it 0600
            file.write(token + "\n")
            file.flush()
            os.fsync(file.fileno())
        temp.replace(path)
    except BaseException:
        temp.unlink(missing_ok=True)
        raise
    return path


def saved_token() -> str | None:
    """The token in the session file, or None when there is none."""
    try:
        token = session_file().read_text(encoding="ascii").strip()
    except FileNotFoundError:
        return None
    except (OSError, UnicodeDecodeError) as error:
        msg = f"cannot read the session file {session_file()}: {error}"
        raise RefusedError(msg) from error
    return token or None


def signed_in(identity: IdentityStore) -> Actor:
    """The operator signed in through the session file.

    Raises:
        RefusedError: Nobody is signed in here.
        NotSignedInError: The session has ended or expired.
    """
    token = saved_token()
    if token is None:
        msg = "not signed in; run 'loglab-operator sign-in CALLSIGN --station NAME' first"
        raise RefusedError(msg)
    return identity.authenticate(token)


def check_schema(conn: psycopg.Connection[TupleRow]) -> None:
    """Refuse to work on a database whose schema is behind this code."""
    pending = pending_migrations(conn, load_migrations())
    if pending:
        msg = f"the database needs {len(pending)} migration(s); run 'loglab-log migrate' first"
        raise RefusedError(msg)


# -- Commands -----------------------------------------------------------------


def _new_secret(ask: Callable[[str], str], whose: str) -> str:
    secret = ask(f"New PIN or passphrase for {whose}: ")
    if ask("Type it again: ") != secret:
        msg = "the two entries differ; nothing was changed"
        raise RefusedError(msg)
    return secret


def _permissions(names: Sequence[str]) -> list[Permission]:
    found: list[Permission] = []
    for name in names:
        for part in name.split(","):
            try:
                found.append(Permission(part.strip().lower()))
            except ValueError as error:
                known = ", ".join(p.value for p in Permission)
                msg = f"{part!r} is not a permission; use {known}"
                raise RefusedError(msg) from error
    return found


def _setup(
    identity: IdentityStore,
    args: argparse.Namespace,
    out: TextIO,
    ask: Callable[[str], str],
) -> int:
    callsign = args.callsign.upper()
    identity.enroll_first_admin(callsign, _new_secret(ask, callsign))
    permissions = ", ".join(p.value for p in Permission)
    out.write(f"{callsign} is the first administrator, with {permissions}.\n")
    out.write(f"Sign in with: loglab-operator sign-in {callsign} --station NAME\n")
    return EXIT_OK


def _sign_in(
    identity: IdentityStore,
    args: argparse.Namespace,
    out: TextIO,
    ask: Callable[[str], str],
) -> int:
    callsign = args.callsign.upper()
    session = identity.sign_in(callsign, ask(f"PIN or passphrase for {callsign}: "), args.station)
    path = save_token(session.token)
    expires = session.expires_at.strftime("%Y-%m-%d %H:%M UTC")
    out.write(f"Signed in as {callsign} at {args.station} until {expires}.\n")
    out.write(f"The session is kept in {path}, readable only by you.\n")
    return EXIT_OK


def _sign_out(identity: IdentityStore, out: TextIO) -> int:
    token = saved_token()
    if token is None:
        out.write("Not signed in.\n")
        return EXIT_OK
    identity.sign_out(token)
    session_file().unlink(missing_ok=True)
    out.write("Signed out.\n")
    return EXIT_OK


def _whoami(identity: IdentityStore, out: TextIO) -> int:
    actor = signed_in(identity)
    held = [p.value for p in Permission if identity.may(actor, p)]
    out.write(f"{actor.operator} at {actor.station}, with {', '.join(held) or 'no permissions'}\n")
    return EXIT_OK


def _list(identity: IdentityStore, out: TextIO) -> int:
    signed_in(identity)
    accounts = identity.accounts()
    for account in accounts:
        state = "enabled " if account.enabled else "DISABLED"
        if not account.has_secret:
            state = "no PIN  "
        held = ", ".join(p.value for p in Permission if p in account.permissions)
        out.write(f"{account.callsign:<12} {state}  {held or '-'}\n")
    out.write(f"{len(accounts)} operator(s)\n")
    return EXIT_OK


def _account_command(
    identity: IdentityStore,
    args: argparse.Namespace,
    out: TextIO,
    ask: Callable[[str], str],
) -> int:
    actor = signed_in(identity)
    callsign = (args.callsign or actor.operator).upper()
    if args.command == "add":
        grants = _permissions(args.grant) if args.grant else list(DEFAULT_GRANTS)
        identity.add_operator(actor, callsign, _new_secret(ask, callsign), grants)
        out.write(f"Added {callsign} with {', '.join(p.value for p in grants)}.\n")
    elif args.command in {"grant", "revoke"}:
        permissions = _permissions(args.permission)
        granted = args.command == "grant"
        identity.set_permissions(actor, callsign, permissions, granted=granted)
        verb = "Granted" if granted else "Revoked"
        out.write(f"{verb} {', '.join(p.value for p in permissions)} for {callsign}.\n")
    elif args.command in {"enable", "disable"}:
        enabled = args.command == "enable"
        identity.set_enabled(actor, callsign, enabled=enabled)
        out.write(
            f"Enabled {callsign}.\n"
            if enabled
            else f"Disabled {callsign}; every session they had is ended.\n",
        )
    else:
        identity.set_secret(actor, callsign, _new_secret(ask, callsign))
        out.write(f"Changed the PIN or passphrase for {callsign}.\n")
    out.write(f"Recorded as made by {actor.operator} at {actor.station}.\n")
    return EXIT_OK


def _write_key(path: Path, key: str) -> Path:
    """Keep a station key in ``path``, readable only by you, written whole (Principle XIV)."""
    path = path.expanduser().resolve()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    temp = Path(temp_name)
    try:
        with os.fdopen(fd, "w", encoding="ascii") as file:  # mkstemp makes it 0600
            file.write(key + "\n")
            file.flush()
            os.fsync(file.fileno())
        temp.replace(path)
    except BaseException:
        temp.unlink(missing_ok=True)
        raise
    return path


def _agent_key(
    identity: IdentityStore,
    keys: AgentKeyStore,
    args: argparse.Namespace,
    out: TextIO,
) -> int:
    actor = signed_in(identity)
    if args.command == "agent-keys":
        stations = keys.stations()
        for station in stations:
            out.write(f"{station}\n")
        out.write(f"{len(stations)} station(s) with a working key\n")
        return EXIT_OK
    if args.end:
        ended = keys.end(actor, args.station)
        out.write(
            f"Ended the key for {args.station}; its Agent is refused from now on.\n"
            if ended
            else f"{args.station} had no working key.\n",
        )
        return EXIT_OK
    key = keys.make(actor, args.station)
    if args.write:
        path = _write_key(Path(args.write), key)
        out.write(
            f"Made a new key for {args.station} and saved it in {path}, readable only by you.\n"
        )
    else:
        out.write(f"Made a new key for {args.station}. It is shown only this once:\n{key}\n")
    out.write("Any earlier key for this station no longer works.\n")
    out.write(f"Recorded as made by {actor.operator} at {actor.station}.\n")
    return EXIT_OK


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="loglab-operator",
        description="LogLab operator accounts and sign-in (Principle XIII).",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument(
        "--database",
        default=None,
        help="libpq connection string or URL (default: $LOGLAB_DATABASE, else dbname=loglab)",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    setup = commands.add_parser("setup", help="create the first administrator")
    setup.add_argument("callsign")

    sign_in = commands.add_parser("sign-in", help="sign in at a station")
    sign_in.add_argument("callsign")
    sign_in.add_argument("--station", required=True, help="station name, e.g. shack")
    commands.add_parser("sign-out", help="end this session")
    commands.add_parser("whoami", help="who is signed in here, and what they may do")
    commands.add_parser("list", help="every operator and their permissions")

    add = commands.add_parser("add", help="add an operator (administrators)")
    add.add_argument("callsign")
    add.add_argument(
        "--grant",
        action="append",
        metavar="PERMISSIONS",
        help="permissions, e.g. view,log (default: view,log)",
    )
    for name, text in (("grant", "grant permissions"), ("revoke", "revoke permissions")):
        command = commands.add_parser(name, help=f"{text} (administrators)")
        command.add_argument("callsign")
        command.add_argument("permission", nargs="+", help="view, log, radio, admin")
    for name, text in (("enable", "enable an account"), ("disable", "disable an account")):
        command = commands.add_parser(name, help=f"{text} (administrators)")
        command.add_argument("callsign")
    agent_key = commands.add_parser(
        "agent-key", help="make a station key for a Station Agent (administrators)"
    )
    agent_key.add_argument("station", help="the station the Agent's radio is at, e.g. shack")
    agent_key.add_argument(
        "--write", metavar="FILE", help="save the key in FILE instead of showing it"
    )
    agent_key.add_argument("--end", action="store_true", help="end the station's key instead")
    commands.add_parser("agent-keys", help="the stations whose Agents have a working key")
    set_pin = commands.add_parser("set-pin", help="change a PIN or passphrase (yours by default)")
    set_pin.add_argument("callsign", nargs="?")
    return parser


def main(
    argv: Sequence[str] | None = None,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
    ask: Callable[[str], str] = getpass.getpass,
) -> int:
    """Run the command line and return its exit code."""
    args = _parser().parse_args(argv)
    try:
        with connect(args.database or default_database()) as conn:
            check_schema(conn)
            identity = IdentityStore(conn)
            simple = {"sign-out": _sign_out, "whoami": _whoami, "list": _list}
            if args.command in simple:
                return simple[args.command](identity, out)
            if args.command in {"agent-key", "agent-keys"}:
                return _agent_key(identity, AgentKeyStore(conn), args, out)
            command = {"setup": _setup, "sign-in": _sign_in}.get(args.command, _account_command)
            return command(identity, args, out, ask)
    except (RefusedError, StoreError, MigrationError) as error:
        err.write(f"refused: {error}\n")
    except psycopg.Error as error:
        err.write(f"database error: {str(error).strip()}\n")
    except (EOFError, KeyboardInterrupt):
        err.write("\nstopped; nothing was changed\n")
    return EXIT_REFUSED


def run() -> None:
    """Console-script entry point."""
    sys.exit(main())
