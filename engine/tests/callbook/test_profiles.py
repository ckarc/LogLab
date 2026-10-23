# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Telling Starlink from the shop's internet, from what address services say."""

from __future__ import annotations

import json

from loglab_engine.callbook.profiles import (
    WHAT_IS_MY_ADDRESS,
    WHERE_AM_I,
    Network,
    NetworkUnreachableError,
    look_at_network,
)


class Services:
    """The address services, answering as the test says; a missing answer is no internet."""

    def __init__(self, answers: dict[str, bytes], names: dict[str, str] | None = None) -> None:
        """Answer ``answers`` by address; ``names`` are registered names by public address."""
        self.answers = answers
        self.names = names or {}
        self.asked: list[str] = []

    def fetch(self, url: str) -> bytes:
        """Answer one request."""
        self.asked.append(url)
        if url not in self.answers:
            raise NetworkUnreachableError("no internet")
        return self.answers[url]

    def reverse(self, address: str) -> str | None:
        """The registered name of ``address``."""
        self.asked.append(f"name of {address}")
        return self.names.get(address)


def _where(ip: str, org: str, hostname: str | None = None) -> bytes:
    body: dict[str, str] = {"ip": ip, "org": org}
    if hostname is not None:
        body["hostname"] = hostname
    return json.dumps(body).encode()


def test_spacex_network_is_starlink() -> None:
    """SpaceX's AS14593 is Starlink."""
    services = Services({WHERE_AM_I: _where("98.97.16.1", "AS14593 Space Exploration")})
    seen = look_at_network(services.fetch, services.reverse)
    assert seen.network is Network.STARLINK
    assert (seen.address, seen.provider) == ("98.97.16.1", "AS14593 Space Exploration")


def test_a_starlinkisp_name_is_starlink_even_without_the_network_number() -> None:
    """When only the address is known, its registered name tells."""
    services = Services(
        {WHAT_IS_MY_ADDRESS: b"129.222.200.1\n"},
        {"129.222.200.1": "customer.chcgilx1.pop.starlinkisp.net."},
    )
    assert look_at_network(services.fetch, services.reverse).network is Network.STARLINK


def test_any_other_network_is_not_starlink() -> None:
    """The shop's line, by whatever company, is fine."""
    services = Services(
        {WHERE_AM_I: _where("203.0.113.7", "AS64500 Shop Fiber", "host.shopfiber.example")}
    )
    seen = look_at_network(services.fetch, services.reverse)
    assert seen.network is Network.OTHER
    assert "name of 203.0.113.7" not in services.asked  # the service already named it
    # A look-alike number is not SpaceX's.
    services = Services({WHERE_AM_I: _where("203.0.113.8", "AS145930 Someone Else")})
    assert look_at_network(services.fetch, services.reverse).network is Network.OTHER


def test_no_answer_at_all_is_no_internet_and_nonsense_is_unknown() -> None:
    """Nothing is re-checked on either."""
    services = Services({})
    assert look_at_network(services.fetch, services.reverse).network is Network.NONE
    services = Services({WHERE_AM_I: b"<html>captive portal</html>", WHAT_IS_MY_ADDRESS: b"hi"})
    assert look_at_network(services.fetch, services.reverse).network is Network.UNKNOWN
    services = Services({WHERE_AM_I: json.dumps({"ip": "not an address"}).encode()})
    assert look_at_network(services.fetch, services.reverse).network is Network.UNKNOWN
