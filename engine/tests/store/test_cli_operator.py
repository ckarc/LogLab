# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The ``lablog-operator`` command line: setting up, signing in, and managing accounts."""

from __future__ import annotations

import stat
from typing import TYPE_CHECKING

from loglab_engine.cli.operator import EXIT_OK, EXIT_REFUSED, main

from .conftest import ADMIN_SECRET, VISITOR_SECRET, Terminal

if TYPE_CHECKING:
    from pathlib import Path


def test_setup_sign_in_whoami_and_sign_out(database: str, tmp_path: Path) -> None:
    """The first administrator gets every permission; the session file is private."""
    cli = Terminal(database, tmp_path / "state" / "session")
    code, out, _ = cli.operator("setup", "k9ksu")
    assert code == EXIT_OK
    assert "K9KSU is the first administrator, with view, log, radio, admin" in out
    code, _, err = cli.operator("setup", "w0xyz")
    assert code == EXIT_REFUSED
    assert "already exists" in err

    code, out, _ = cli.operator("sign-in", "k9ksu", "--station", "shack")
    assert code == EXIT_OK
    assert "Signed in as K9KSU at shack until " in out
    assert out.splitlines()[0].endswith(" UTC.")
    mode = stat.S_IMODE(cli.session_file.stat().st_mode)
    assert mode == 0o600
    assert cli.operator("whoami")[1] == "K9KSU at shack, with view, log, radio, admin\n"

    assert cli.operator("sign-out")[1] == "Signed out.\n"
    assert not cli.session_file.exists()
    assert cli.operator("sign-out")[1] == "Not signed in.\n"
    code, _, err = cli.operator("whoami")
    assert code == EXIT_REFUSED
    assert "not signed in" in err


def test_a_wrong_secret_is_refused(database: str, tmp_path: Path) -> None:
    """Nothing is saved when the secret is wrong."""
    cli = Terminal(database, tmp_path / "session")
    assert cli.operator("setup", "K9KSU")[0] == EXIT_OK
    code, _, err = cli.operator("sign-in", "K9KSU", "--station", "shack", secret="000000")  # noqa: S106 - a wrong PIN on purpose
    assert code == EXIT_REFUSED
    assert "wrong callsign or secret" in err
    assert not cli.session_file.exists()


def test_an_administrator_manages_accounts(cli: Terminal, database: str, tmp_path: Path) -> None:
    """Add, grant, revoke, disable, enable, and list, each recorded with who did it."""
    code, out, _ = cli.operator("add", "w0xyz", secret=VISITOR_SECRET)
    assert code == EXIT_OK
    assert "Added W0XYZ with view, log." in out
    assert "Recorded as made by K9KSU at shack." in out
    code, out, _ = cli.operator("add", "N0ABC", "--grant", "view,radio", secret=VISITOR_SECRET)
    assert "Added N0ABC with view, radio." in out
    assert cli.operator("grant", "W0XYZ", "radio")[1].startswith("Granted radio for W0XYZ.")
    assert cli.operator("revoke", "N0ABC", "view")[0] == EXIT_OK
    assert "every session they had is ended" in cli.operator("disable", "N0ABC")[1]

    listing = cli.operator("list")[1].splitlines()
    assert listing[0].split() == ["K9KSU", "enabled", "view,", "log,", "radio,", "admin"]
    assert listing[1].split() == ["N0ABC", "DISABLED", "radio"]
    assert listing[2].split() == ["W0XYZ", "enabled", "view,", "log,", "radio"]
    assert listing[3] == "3 operator(s)"

    other = Terminal(database, tmp_path / "n0abc.session")
    code, _, err = other.operator("sign-in", "N0ABC", "--station", "fd-1", secret=VISITOR_SECRET)
    assert code == EXIT_REFUSED
    assert "disabled" in err
    assert cli.operator("enable", "N0ABC")[0] == EXIT_OK
    code, _, _ = other.operator("sign-in", "N0ABC", "--station", "fd-1", secret=VISITOR_SECRET)
    assert code == EXIT_OK


def test_account_commands_explain_refusals(cli: Terminal, visitor_cli: Terminal) -> None:
    """Bad permissions, mismatched secrets, and non-administrators are refused."""
    for argv, reason in [
        (("grant", "W0XYZ", "transmit"), "'transmit' is not a permission"),
        (("grant", "N0BODY", "log"), "there is no operator N0BODY"),
        (("add", "W0XYZ"), "already has an account"),
        (("disable", "K9KSU"), "cannot disable their own account"),
    ]:
        code, _, err = cli.operator(*argv)
        assert code == EXIT_REFUSED, argv
        assert reason in err, (argv, err)

    def typed_twice_differently(prompt: str) -> str:
        return "111111" if prompt.startswith("New") else "222222"

    code = cli.run(main, ("set-pin",), typed_twice_differently)[0]
    assert code == EXIT_REFUSED

    code, _, err = visitor_cli.operator("add", "N0NEW", secret=VISITOR_SECRET)
    assert code == EXIT_REFUSED
    assert "admin permission" in err
    new_pin = "135791"
    assert visitor_cli.operator("set-pin", secret=new_pin)[0] == EXIT_OK
    assert visitor_cli.operator("sign-in", "W0XYZ", "--station", "fd-2", secret=new_pin)[0] == 0
    assert cli.operator("set-pin", "W0XYZ", secret=ADMIN_SECRET)[0] == EXIT_OK


def test_an_administrator_makes_and_ends_station_keys(
    cli: Terminal, visitor_cli: Terminal, tmp_path: Path
) -> None:
    """A key is saved where asked, privately; only an administrator can make one."""
    key_file = tmp_path / "agent" / "key"
    code, out, _ = cli.operator("agent-key", "fd-1", "--write", str(key_file))
    assert code == EXIT_OK
    assert f"saved it in {key_file}" in out
    assert stat.S_IMODE(key_file.stat().st_mode) == 0o600
    assert key_file.read_text(encoding="ascii").startswith("lls_")
    code, out, _ = cli.operator("agent-key", "fd-2")
    assert code == EXIT_OK
    assert "shown only this once:\nlls_" in out
    assert cli.operator("agent-keys")[1] == "fd-1\nfd-2\n2 station(s) with a working key\n"
    code, _, err = visitor_cli.operator("agent-key", "fd-2", secret=VISITOR_SECRET)
    assert code == EXIT_REFUSED
    assert "administrator" in err
    assert "Ended the key for fd-1" in cli.operator("agent-key", "fd-1", "--end")[1]
    assert cli.operator("agent-keys")[1] == "fd-2\n1 station(s) with a working key\n"
