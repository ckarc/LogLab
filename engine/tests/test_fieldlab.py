# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""FieldLab's MQTT bus: the trailer's GPS fix and the gateway's word on metered links."""

from __future__ import annotations

import io
import json
import time
from datetime import date
from typing import TYPE_CHECKING

import pytest

from loglab_engine.cli import engine as engine_cli
from loglab_engine.fieldlab import FACTS_TOPIC, LOCATION_TOPIC, FieldLabBus, GpsFix, parse_fix
from loglab_engine.mqtt import MqttError
from loglab_engine.reference.refresh import Refresher

if TYPE_CHECKING:
    from collections.abc import Callable
    from datetime import datetime
    from pathlib import Path

NOW = 1_792_771_200.0
NAMES = {"wan1": "Shop", "wan2": "Starlink", "wan3": "Phone tether"}


def _fix(**doc: object) -> bytes:
    return json.dumps(doc).encode()


def _facts(*wans: str, ts: float = NOW - 10, error: str = "") -> bytes:
    return json.dumps(
        {"ts": ts, "uplink": {"wans": list(wans), "names": NAMES, "error": error}}
    ).encode()


def test_a_recent_fix() -> None:
    """The kiosk's location message, as the tracker sends it."""
    fix = parse_fix(_fix(lat=38.84031, lon=-97.61142, alt=350.0, ts=NOW - 20), NOW)
    assert fix == GpsFix(lat=38.84031, lon=-97.61142, alt_m=350.0, at=NOW - 20)
    assert parse_fix(_fix(lat=38.8, lon=-97.6, ts=NOW), NOW) == GpsFix(38.8, -97.6, None, NOW)


@pytest.mark.parametrize(
    "payload",
    [
        None,
        b"",
        b"null",
        b"{}",
        b"nope",
        b"[1, 2]",
        _fix(lat=0, lon=0, ts=NOW),  # a receiver with no fix yet
        _fix(lat=95, lon=10, ts=NOW),
        _fix(lat=38.8, lon=200, ts=NOW),
        _fix(lat="38.8", lon=-97.6, ts=NOW),
        _fix(lat=True, lon=-97.6, ts=NOW),
        _fix(lat=38.8, lon=-97.6),  # no time: can't tell how old
        _fix(lat=38.8, lon=-97.6, ts=NOW - 7 * 3600),  # too old
        _fix(lat=38.8, lon=-97.6, ts=NOW + 3600),  # from the future
        b'{"lat": NaN, "lon": -97.6, "ts": 1792771200}',
    ],
)
def test_unusable_fixes_are_ignored(payload: bytes | None) -> None:
    """Anything malformed, impossible or stale is no fix at all."""
    assert parse_fix(payload, NOW) is None


def test_the_bus_keeps_the_last_message_when_the_broker_is_away() -> None:
    """A read that fails keeps what was heard; the fix still ages out on its own."""
    clock = [NOW]
    messages: dict[str, bytes | None] = {
        LOCATION_TOPIC: _fix(lat=38.9, lon=-97.2, ts=NOW),
        FACTS_TOPIC: _facts("wan1"),
    }
    down = False

    def read(topic: str) -> bytes | None:
        if down:
            raise MqttError("the broker could not be reached")
        return messages[topic]

    bus = FieldLabBus(read, clock=lambda: clock[0])
    assert bus.fix() is None  # nothing heard yet
    bus.read_once()
    assert bus.fix() == GpsFix(38.9, -97.2, None, NOW)
    assert bus.facts() == _facts("wan1")
    down = True
    bus.read_once()
    assert bus.fix() == GpsFix(38.9, -97.2, None, NOW)
    clock[0] = NOW + 3601
    assert bus.fix() is None


def test_a_topic_with_nothing_retained_forgets_the_old_message() -> None:
    """The broker answering "nothing retained" means there is no fix now."""
    messages: dict[str, bytes | None] = {LOCATION_TOPIC: _fix(lat=38.9, lon=-97.2, ts=NOW)}
    bus = FieldLabBus(messages.get, clock=lambda: NOW)
    bus.read_once()
    assert bus.fix() is not None
    messages[LOCATION_TOPIC] = None
    bus.read_once()
    assert bus.fix() is None


@pytest.mark.parametrize(
    ("facts", "held"),
    [
        (_facts("wan1"), None),
        (_facts("wan1", "wan2"), None),  # the first is the one in use
        (_facts("wan2"), "Paused: the internet is Starlink, which has a data budget."),
        (_facts("wan2", "wan1"), "Paused: the internet is Starlink, which has a data budget."),
        (_facts("wan3"), "Paused: the internet is a metered link (Phone tether)."),
        (_facts(), "No internet right now."),
        (_facts("wan2", ts=NOW - 3600), None),  # too old to go by: as without FieldLab
        (_facts("wan2", error="login refused"), None),
        (None, None),
        (b"garbage", None),
    ],
)
def test_large_downloads_wait_off_the_shop_link(facts: bytes | None, held: str | None) -> None:
    """Only the gateway's word holds a download: Starlink and the phone tether cost data."""
    bus = FieldLabBus({FACTS_TOPIC: facts}.get, clock=lambda: NOW)
    bus.read_once()
    assert bus.download_hold() == held


class _NoDownloads:
    def published(self, url: str) -> datetime | None:
        raise AssertionError(url)

    def fetch(self, url: str, dest: Path, cap: int) -> datetime | None:
        del dest, cap
        raise AssertionError(url)


def test_the_license_lists_are_not_fetched_while_held() -> None:
    """On Starlink the 143 MB license lists wait; nothing is asked of the FCC or ISED."""
    held = "Paused: the internet is Starlink, which has a data budget."
    refresher = Refresher(
        "dbname=never-opened", _NoDownloads(), today=lambda: date(2026, 10, 23), hold=lambda: held
    )
    report = refresher.run_once()
    assert report.problems == [held]
    assert report.imported == []


def _now_facts(*wans: str) -> bytes:
    return json.dumps(
        {"ts": time.time() - 5, "uplink": {"wans": list(wans), "names": NAMES}}
    ).encode()


def _no_bus() -> bytes | None:
    raise MqttError("connection refused")


@pytest.mark.parametrize(
    ("read", "code", "said"),
    [
        (lambda: _now_facts("wan1", "wan2"), 0, "shop (Shop)\n"),
        (lambda: _now_facts("wan2"), 1, "starlink (Starlink)\n"),
        (lambda: _now_facts("wan3"), 1, "metered (Phone tether)\n"),
        (_now_facts, 1, "none\n"),
        (lambda: None, 1, "no fresh word from the gateway\n"),
        (_no_bus, 1, "FieldLab's bus: connection refused\n"),
    ],
)
def test_uplink_says_yes_only_on_the_shop_link(
    read: Callable[[], bytes | None], code: int, said: str
) -> None:
    """``lablog-engine uplink``: the kiosk's updater checks for updates only on the shop link."""
    out = io.StringIO()
    assert engine_cli._uplink(("10.73.10.10", 1883), out, read) == code
    assert out.getvalue() == said


def test_uplink_with_no_broker_says_no() -> None:
    """With no broker named, nothing says it is the shop link."""
    out = io.StringIO()
    assert engine_cli._uplink(None, out) == 1
