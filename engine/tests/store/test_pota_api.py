# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""A POTA activation through the API: several operators under one club call (II, XVII)."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

import pytest
from fastapi.testclient import TestClient

from loglab_engine.api import create_app

from .conftest import conninfo
from .test_stats_api import k9ksu, w0xyz

__all__ = ["k9ksu", "w0xyz"]  # fixtures used here


@pytest.fixture
def client(database: str) -> TestClient:
    """The Engine's API on this test's database."""
    return TestClient(create_app(conninfo(database)))


@pytest.fixture
def activation(client: TestClient, k9ksu: dict[str, str]) -> dict[str, Any]:
    """The club's activation of Devil's Lake, as W0CK."""
    body = {
        "overlay": "pota-activator",
        "name": "Devil's Lake",
        "station_call": "W0CK",
        "gota_call": None,
        "sent": {"park": "US-1562"},
    }
    response = client.post("/api/v1/operations", headers=k9ksu, json=body)
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()
    return out


def _fields(call: str, band: str = "20m", **more: str) -> list[dict[str, str]]:
    now = datetime.now(UTC)
    fields = {
        "CALL": call,
        "QSO_DATE": now.strftime("%Y%m%d"),
        "TIME_ON": now.strftime("%H%M%S"),
        "BAND": band,
        "MODE": "SSB",
        "RST_SENT": "59",
        "RST_RCVD": "57",
        **more,
    }
    return [{"name": k, "value": v} for k, v in fields.items()]


def _log(
    client: TestClient, headers: dict[str, str], op: dict[str, Any], call: str, **more: str
) -> dict[str, Any]:
    body = {"fields": _fields(call, **more), "operation": {"id": op["id"]}}
    response = client.post("/api/v1/qsos", headers=headers, json=body)
    assert response.status_code == 201, response.text
    out: dict[str, Any] = response.json()["qso"]
    return out


def _check(
    client: TestClient, headers: dict[str, str], op: dict[str, Any], call: str, **more: str
) -> dict[str, Any]:
    body = {"fields": _fields(call, **more), "operation": {"id": op["id"]}}
    response = client.post("/api/v1/overlays/pota-activator/check", headers=headers, json=body)
    assert response.status_code == 200, response.text
    out: dict[str, Any] = response.json()
    return out


def test_an_operation_needs_its_park(client: TestClient, k9ksu: dict[str, str]) -> None:
    """No park, no activation."""
    body = {"overlay": "pota-activator", "name": "Somewhere", "station_call": "W0CK", "sent": {}}
    response = client.post("/api/v1/operations", headers=k9ksu, json=body)
    assert response.status_code == 422


def test_qsos_carry_the_park_and_dupes_are_per_band_and_mode(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], activation: dict[str, Any]
) -> None:
    """One operator's contact is a dupe for the next operator too: one club call."""
    qso = _log(client, k9ksu, activation, "W1AW")
    fields = {f["name"]: f["value"] for f in qso["fields"]}
    assert (fields["MY_SIG"], fields["MY_SIG_INFO"], fields["MY_POTA_REF"]) == (
        "POTA",
        "US-1562",
        "US-1562",
    )
    assert (fields["STATION_CALLSIGN"], fields["OPERATOR"]) == ("W0CK", "K9KSU")
    assert "CONTEST_ID" not in fields
    again = _check(client, w0xyz, activation, "W1AW")
    assert again["status"] == "dupe"
    assert _check(client, w0xyz, activation, "W1AW", band="40m")["status"] == "new"
    assert _check(client, w0xyz, activation, "W1AW", SIG_INFO="US-0001")["status"] == "new"


def test_the_stats_count_toward_ten_for_the_club_call(
    client: TestClient, k9ksu: dict[str, str], w0xyz: dict[str, str], activation: dict[str, Any]
) -> None:
    """Both operators' QSOs count toward W0CK's activation; dupes don't."""
    for i in range(5):
        _log(client, k9ksu, activation, f"K{i}AA")
    for i in range(4):
        _log(client, w0xyz, activation, f"N{i}BB")
    _log(client, w0xyz, activation, "K0AA")  # a dupe
    qso = _log(client, w0xyz, activation, "KD9P", SIG_INFO="us-0001")
    fields = {f["name"]: f["value"] for f in qso["fields"]}
    assert (fields["SIG"], fields["POTA_REF"]) == ("POTA", "US-0001")
    stats = client.get(f"/api/v1/operations/{activation['id']}/stats", headers=k9ksu).json()
    assert stats["goal"] == 10
    assert stats["dupes"] == 1
    assert stats["p2p"] == 1
    (today,) = stats["activations"]
    assert (today["station_call"], today["park"], today["qsos"], today["done"], today["p2p"]) == (
        "W0CK",
        "US-1562",
        10,
        True,
        1,
    )
    assert stats["show"][0] == "activation"
    assert sorted(o["operator"] for o in stats["operators"]) == ["K9KSU", "W0XYZ"]
