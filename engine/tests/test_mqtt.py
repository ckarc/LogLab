# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Reading one retained MQTT message, from a stand-in broker speaking MQTT 3.1.1."""

from __future__ import annotations

import contextlib
import socket
import threading
from typing import TYPE_CHECKING

import pytest

from loglab_engine.mqtt import MAX_PACKET, MqttError, read_retained

if TYPE_CHECKING:
    from collections.abc import Iterator

TOPIC = "fieldlab/facts/host"


def _length(size: int) -> bytes:
    out = bytearray()
    while True:
        size, digit = divmod(size, 128)
        out.append(digit | (0x80 if size else 0))
        if not size:
            return bytes(out)


def _publish(topic: str, payload: bytes, *, retain: bool = True, qos: int = 0) -> bytes:
    name = topic.encode()
    body = len(name).to_bytes(2, "big") + name + (b"\x00\x07" if qos else b"") + payload
    return bytes([0x30 | (qos << 1) | int(retain)]) + _length(len(body)) + body


class Broker:
    """One connection's worth of a broker: what it answers is set by the test."""

    def __init__(self) -> None:
        """Listen on a free local port."""
        self.server = socket.create_server(("127.0.0.1", 0))
        self.port: int = self.server.getsockname()[1]
        self.connack = b"\x20\x02\x00\x00"
        self.after_subscribe = b"\x90\x03\x00\x01\x00"
        self.received = b""
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        conn, _ = self.server.accept()
        with conn:
            conn.settimeout(5)
            self.received += conn.recv(4096)  # CONNECT
            conn.sendall(self.connack)
            if self.connack[3:4] != b"\x00":
                return
            self.received += conn.recv(4096)  # SUBSCRIBE
            conn.sendall(self.after_subscribe)
            with contextlib.suppress(OSError):  # the client may simply give up
                self.received += conn.recv(4096)  # DISCONNECT

    def wait(self) -> None:
        """Wait until the broker's side of the connection is over."""
        self._thread.join(timeout=5)

    def close(self) -> None:
        """Stop listening."""
        self.server.close()


@pytest.fixture
def broker() -> Iterator[Broker]:
    """A stand-in broker."""
    stand_in = Broker()
    yield stand_in
    stand_in.close()


def _read(broker: Broker, topic: str = TOPIC) -> bytes | None:
    return read_retained("127.0.0.1", broker.port, topic, client_id="lablog-test", timeout=3)


def test_the_retained_message_is_read_and_the_client_says_goodbye(broker: Broker) -> None:
    """Connect, subscribe once at QoS 0, take the retained message, disconnect."""
    broker.after_subscribe += _publish(TOPIC, b'{"ts": 1}')
    assert _read(broker) == b'{"ts": 1}'
    broker.wait()  # the DISCONNECT may still be on its way
    sent = broker.received
    assert sent.startswith(b"\x10")  # CONNECT
    assert b"\x00\x04MQTT\x04\x02" in sent  # MQTT 3.1.1, a clean session
    assert b"\x82" in sent  # SUBSCRIBE
    assert TOPIC.encode() + b"\x00" in sent  # at QoS 0
    assert sent.endswith(b"\xe0\x00")  # DISCONNECT


def test_a_qos_1_retained_message_and_others_before_it_are_handled(broker: Broker) -> None:
    """A live message, or one on another topic, is not the retained one."""
    broker.after_subscribe += (
        _publish("fieldlab/other", b"no")
        + _publish(TOPIC, b"live", retain=False)
        + _publish(TOPIC, b"kept", qos=1)
    )
    assert _read(broker) == b"kept"


def test_nothing_retained_is_none_after_a_short_wait(broker: Broker) -> None:
    """A subscription with nothing kept answers None, not an error."""
    assert _read(broker) is None


def test_a_refused_connection_is_an_error(broker: Broker) -> None:
    """The broker's no is reported, never guessed around."""
    broker.connack = b"\x20\x02\x00\x05"
    with pytest.raises(MqttError, match="refused the connection"):
        _read(broker)


def test_a_refused_subscription_is_an_error(broker: Broker) -> None:
    """A SUBACK failure code (0x80) is an error."""
    broker.after_subscribe = b"\x90\x03\x00\x01\x80"
    with pytest.raises(MqttError, match="refused the subscription"):
        _read(broker)


def test_an_oversized_or_malformed_answer_is_refused(broker: Broker) -> None:
    """Every packet is size-capped before it is read (Principle XII)."""
    broker.after_subscribe = b"\x30" + _length(MAX_PACKET + 1)
    with pytest.raises(MqttError, match="larger than"):
        _read(broker)


def test_no_broker_is_an_error_quickly() -> None:
    """With nothing listening, the caller hears so at once (Principle V)."""
    with socket.create_server(("127.0.0.1", 0)) as spare:
        port = spare.getsockname()[1]
    with pytest.raises(MqttError, match="could not be reached"):
        read_retained("127.0.0.1", port, TOPIC, client_id="lablog-test", timeout=1)
