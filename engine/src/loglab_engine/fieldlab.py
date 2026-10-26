# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""What FieldLab's own gear says on its MQTT bus, kept current in the background.

FieldLab's kiosk (github.com/ckarc/fieldlab-kiosk) publishes retained
messages on the trailer's broker. The Engine reads two of them, every
:data:`READ_EVERY_SECONDS`, with :func:`loglab_engine.mqtt.read_retained`:

* ``fieldlab/location``: the trailer's GPS fix, ``{"lat", "lon", "alt", "ts"}``;
* ``fieldlab/facts/host``: the kiosk PC's facts, among them which internet
  link the gateway is using.

It never publishes. Everything on the bus is untrusted and only parsed and
compared (Principle XII). The bus is optional (Principle V): with no broker,
or with a message too old, the Engine does what it does without FieldLab.

A GPS fix is measured, so the Engine says it is from GPS wherever it shows it.
"""

from __future__ import annotations

import json
import logging
import math
import threading
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

from loglab_engine.callbook.profiles import Network, gateway_uplink
from loglab_engine.mqtt import MqttError, read_retained

if TYPE_CHECKING:
    from collections.abc import Callable

LOCATION_TOPIC: Final = "fieldlab/location"
FACTS_TOPIC: Final = "fieldlab/facts/host"
READ_EVERY_SECONDS: Final = 30.0
FIX_MAX_AGE_SECONDS: Final = 3600.0  # the kiosk's own pages drop a fix this old
_CLIENT_ID: Final = "lablog-engine-fieldlab"
_log = logging.getLogger(__name__)

# The gateway's links that cost money or are capped: Starlink and the phone tether.
_HOLD: Final = {
    Network.NONE: "No internet right now.",
    Network.STARLINK: "Paused: the internet is Starlink, which has a data budget.",
    Network.METERED: "Paused: the internet is a metered link",
}


@dataclass(frozen=True, slots=True)
class GpsFix:
    """Where the trailer's GPS put it, in degrees, and when (seconds since 1970, UTC)."""

    lat: float
    lon: float
    alt_m: float | None
    at: float


def _number(value: object) -> float | None:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    return float(value) if math.isfinite(value) else None


def parse_fix(payload: bytes | None, now: float) -> GpsFix | None:
    """A ``fieldlab/location`` message as a fix, if it is a usable, recent one."""
    try:
        doc = json.loads(payload) if payload else None
    except ValueError:
        return None
    if not isinstance(doc, dict):
        return None
    lat, lon, at = _number(doc.get("lat")), _number(doc.get("lon")), _number(doc.get("ts"))
    if lat is None or lon is None or at is None:
        return None
    if not (-90 <= lat <= 90 and -180 <= lon <= 180) or (lat == 0 and lon == 0):  # noqa: PLR2004
        return None
    if not -60 <= now - at <= FIX_MAX_AGE_SECONDS:  # noqa: PLR2004 - a minute of clock difference
        return None
    return GpsFix(lat=round(lat, 6), lon=round(lon, 6), alt_m=_number(doc.get("alt")), at=at)


class FieldLabBus:
    """The latest of FieldLab's messages the Engine uses, read in the background."""

    def __init__(
        self,
        read: Callable[[str], bytes | None],
        clock: Callable[[], float] = time.time,
    ) -> None:
        """Read each topic with ``read`` (it raises :class:`MqttError` when it can't)."""
        self._read = read
        self._clock = clock
        self._lock = threading.Lock()
        self._latest: dict[str, bytes | None] = {}
        self._thread: threading.Thread | None = None

    @classmethod
    def at(cls, host: str, port: int) -> FieldLabBus:
        """The bus served by the broker at ``host``:``port``."""

        def read(topic: str) -> bytes | None:
            return read_retained(host, port, topic, client_id=_CLIENT_ID)

        return cls(read)

    def start(self) -> None:
        """Read every :data:`READ_EVERY_SECONDS`, in the background, until the process ends."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, name="fieldlab", daemon=True)
            self._thread.start()

    def _loop(self) -> None:
        while True:
            self.read_once()
            time.sleep(READ_EVERY_SECONDS)

    def read_once(self) -> None:
        """Read every topic once. A topic that can't be read keeps its last message."""
        for topic in (LOCATION_TOPIC, FACTS_TOPIC):
            try:
                payload = self._read(topic)
            except MqttError as error:
                _log.info("FieldLab's bus: %s: %s", topic, error)
                continue
            with self._lock:
                self._latest[topic] = payload

    def _message(self, topic: str) -> bytes | None:
        with self._lock:
            return self._latest.get(topic)

    def fix(self) -> GpsFix | None:
        """The trailer's GPS fix, if a recent one has been heard."""
        return parse_fix(self._message(LOCATION_TOPIC), self._clock())

    def facts(self) -> bytes | None:
        """The kiosk PC's latest facts message; its own ``ts`` says how old it is."""
        return self._message(FACTS_TOPIC)

    def download_hold(self) -> str | None:
        """Why a large background download should wait, or ``None`` when it may go.

        Only the gateway's word counts: it holds on Starlink, a metered link
        or no internet. With no word from the bus, nothing is held, as without FieldLab.
        """
        seen = gateway_uplink(self.facts(), self._clock())
        if seen is None or seen.network not in _HOLD:
            return None
        if seen.network is Network.METERED:
            return f"{_HOLD[Network.METERED]} ({seen.provider or 'not the shop link'})."
        return _HOLD[seen.network]
