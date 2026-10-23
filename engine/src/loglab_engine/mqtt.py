# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading one retained message from an MQTT broker (MQTT 3.1.1, from its OASIS standard).

FieldLab's own gear reports on an MQTT broker on its LAN: which internet link
the gateway is using, the trailer's GPS fix, and more. The Engine only reads:
it connects, subscribes to one topic, takes the retained message the broker
hands every new subscriber, and disconnects. It never publishes.

Everything the broker sends is untrusted (Principle XII): every packet is
size-capped, and the payload is returned as bytes for the caller to check.
The broker is optional (Principle V): every failure is an :class:`MqttError`,
which callers treat as "not known this time".
"""

from __future__ import annotations

import socket
import time
from typing import Final

TIMEOUT_SECONDS: Final = 3.0
MAX_PACKET: Final = 256 * 1024
_KEEPALIVE: Final = 30
_CONNECT: Final = 0x10
_CONNACK: Final = 0x20
_PUBLISH: Final = 0x30
_SUBSCRIBE: Final = 0x82  # with the flags the standard requires
_SUBACK: Final = 0x90
_DISCONNECT: Final = b"\xe0\x00"
_RETAINED_WAIT_SECONDS: Final = 1.0
_PACKET_ID: Final = 1


class MqttError(Exception):
    """The broker could not be reached, refused, or sent something unusable."""


def _string(text: str) -> bytes:
    data = text.encode()
    return len(data).to_bytes(2, "big") + data


def _length(size: int) -> bytes:
    """The standard's variable-length "remaining length"."""
    out = bytearray()
    while True:
        size, digit = divmod(size, 128)
        out.append(digit | (0x80 if size else 0))
        if not size:
            return bytes(out)


def _packet(kind: int, body: bytes) -> bytes:
    return bytes([kind]) + _length(len(body)) + body


class _Reader:
    def __init__(self, sock: socket.socket, deadline: float) -> None:
        self._sock = sock
        self._deadline = deadline
        self._buffer = b""

    def shorten(self, seconds: float) -> None:
        """Wait at most ``seconds`` more for anything else."""
        self._deadline = min(self._deadline, time.monotonic() + seconds)

    def _take(self, size: int) -> bytes:
        while len(self._buffer) < size:
            left = self._deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError
            self._sock.settimeout(left)
            chunk = self._sock.recv(65536)
            if not chunk:
                raise MqttError("the broker closed the connection")
            self._buffer += chunk
        taken, self._buffer = self._buffer[:size], self._buffer[size:]
        return taken

    def packet(self) -> tuple[int, bytes]:
        kind = self._take(1)[0]
        size, shift = 0, 0
        for _ in range(4):
            digit = self._take(1)[0]
            size |= (digit & 0x7F) << shift
            shift += 7
            if not digit & 0x80:
                break
        else:
            raise MqttError("a packet's length is malformed")
        if size > MAX_PACKET:
            raise MqttError(f"a packet is larger than {MAX_PACKET} bytes")
        return kind, self._take(size)


def _publish(flags: int, body: bytes) -> tuple[str, bytes]:
    """A PUBLISH packet's topic and payload."""
    if len(body) < 2:  # noqa: PLR2004 - the topic's length
        raise MqttError("a message is malformed")
    size = int.from_bytes(body[:2], "big")
    topic = body[2 : 2 + size]
    rest = 2 + size + (2 if flags & 0x06 else 0)  # a packet id comes with QoS 1 and 2
    if len(topic) != size or len(body) < rest:
        raise MqttError("a message is malformed")
    return topic.decode(errors="replace"), body[rest:]


def read_retained(
    host: str, port: int, topic: str, *, client_id: str, timeout: float = TIMEOUT_SECONDS
) -> bytes | None:
    """The message retained on ``topic`` (no wildcards), or ``None`` if there is none.

    Raises:
        MqttError: The broker could not be reached in ``timeout`` seconds, refused
            the connection, or answered with something the standard does not allow.
    """
    connect = (
        _string("MQTT")
        + bytes([4, 0x02])  # protocol level 4 (3.1.1); a clean session
        + _KEEPALIVE.to_bytes(2, "big")
        + _string(client_id)
    )
    subscribe = _PACKET_ID.to_bytes(2, "big") + _string(topic) + b"\x00"  # QoS 0
    try:
        with socket.create_connection((host, port), timeout=timeout) as sock:
            reader = _Reader(sock, time.monotonic() + timeout)
            sock.sendall(_packet(_CONNECT, connect))
            kind, body = reader.packet()
            if kind != _CONNACK or len(body) != 2:  # noqa: PLR2004 - the standard's CONNACK
                raise MqttError("the broker did not acknowledge the connection")
            if body[1] != 0:
                raise MqttError(f"the broker refused the connection (code {body[1]})")
            sock.sendall(_packet(_SUBSCRIBE, subscribe))
            found = _retained(reader, topic)
            sock.sendall(_DISCONNECT)
            return found
    except TimeoutError as error:
        raise MqttError("the broker did not answer in time") from error
    except OSError as error:
        raise MqttError(f"the broker could not be reached ({error})") from error


def _retained(reader: _Reader, topic: str) -> bytes | None:
    """The retained message the broker sends a new subscriber, if any.

    The broker sends it with the subscription's acknowledgement; once that has
    come, a short wait with nothing more means nothing is retained.
    """
    while True:
        kind, body = reader.packet()
        if kind == _SUBACK:
            if body[2:3] == b"\x80":
                raise MqttError(f"the broker refused the subscription to {topic}")
            reader.shorten(_RETAINED_WAIT_SECONDS)
            try:
                return _first_retained(reader, topic)
            except TimeoutError:
                return None
        if kind & 0xF0 == _PUBLISH and (found := _match(kind, body, topic)) is not None:
            return found


def _first_retained(reader: _Reader, topic: str) -> bytes:
    while True:
        kind, body = reader.packet()
        if kind & 0xF0 == _PUBLISH and (found := _match(kind, body, topic)) is not None:
            return found


def _match(kind: int, body: bytes, topic: str) -> bytes | None:
    """A PUBLISH packet's payload, if it is the message retained on ``topic``."""
    name, payload = _publish(kind & 0x0F, body)
    return payload if name == topic and kind & 0x01 else None  # 0x01: the retain flag
