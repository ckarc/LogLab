# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Logbook of the World: an optional enhancement that sends QSOs and brings confirmations back."""

from loglab_engine.lotw.certificate import CallsignCertificate, CertificateError, load_certificate
from loglab_engine.lotw.client import (
    LotwClient,
    LotwError,
    LotwLoginError,
    LotwRejectedError,
    LotwReplyError,
    LotwUnreachableError,
    Report,
    Transport,
    https,
    read_report,
    read_upload_reply,
)
from loglab_engine.lotw.gabbi import (
    Contact,
    LocationError,
    NotSignableError,
    StationLocation,
    build_tq8,
    contact_from_record,
)
from loglab_engine.lotw.service import (
    CertificateSummary,
    DownloadResult,
    Link,
    Lotw,
    Outcome,
    PrepareResult,
    Setup,
    Skipped,
    UploadResult,
    make_location,
    with_fields,
)
from loglab_engine.lotw.tables import Place, Tables, tables

__all__ = [
    "CallsignCertificate",
    "CertificateError",
    "CertificateSummary",
    "Contact",
    "DownloadResult",
    "Link",
    "LocationError",
    "Lotw",
    "LotwClient",
    "LotwError",
    "LotwLoginError",
    "LotwRejectedError",
    "LotwReplyError",
    "LotwUnreachableError",
    "NotSignableError",
    "Outcome",
    "Place",
    "PrepareResult",
    "Report",
    "Setup",
    "Skipped",
    "StationLocation",
    "Tables",
    "Transport",
    "UploadResult",
    "build_tq8",
    "contact_from_record",
    "https",
    "load_certificate",
    "make_location",
    "read_report",
    "read_upload_reply",
    "tables",
    "with_fields",
]
