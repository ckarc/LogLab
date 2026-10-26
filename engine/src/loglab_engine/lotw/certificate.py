# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""An operator's LoTW callsign certificate, read from the ``.p12`` file TrustedQSL saves.

LoTW issues each callsign a certificate and private key; TrustedQSL can save
both, with a password, as a PKCS#12 (``.p12``) file. The certificate names the
callsign, its DXCC entity, and the range of QSO dates it may sign for, in
extensions TrustedQSL's published source defines (OIDs under
``1.3.6.1.4.1.12348.1``). The file is untrusted input (Principle XII): it is
size-capped, and every value used is checked.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import UTC, date, datetime
from typing import Final

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.hazmat.primitives.serialization import pkcs12

MAX_FILE: Final = 64 * 1024
_OID_BASE: Final = "1.3.6.1.4.1.12348.1."
OID_CALLSIGN: Final = x509.ObjectIdentifier(_OID_BASE + "1")
OID_QSO_NOT_BEFORE: Final = x509.ObjectIdentifier(_OID_BASE + "2")
OID_QSO_NOT_AFTER: Final = x509.ObjectIdentifier(_OID_BASE + "3")
OID_DXCC: Final = x509.ObjectIdentifier(_OID_BASE + "4")
_CALL: Final = re.compile(r"^[A-Z0-9]+(/[A-Z0-9]+)*$")
_PEM_BEGIN: Final = "-----BEGIN CERTIFICATE-----\n"
_PEM_END: Final = "-----END CERTIFICATE-----"


class CertificateError(ValueError):
    """The file is not a usable LoTW callsign certificate."""


@dataclass(frozen=True, slots=True)
class CallsignCertificate:
    """A LoTW callsign certificate with its private key, ready to sign QSOs."""

    call: str
    dxcc: int
    qso_first: date
    qso_last: date | None
    expires: datetime
    serial: int
    certificate: x509.Certificate
    key: rsa.RSAPrivateKey

    def pem_body(self) -> str:
        """The certificate in PEM, without its BEGIN and END lines, as LoTW wants it."""
        pem = self.certificate.public_bytes(serialization.Encoding.PEM).decode("ascii")
        return pem.removeprefix(_PEM_BEGIN).split(_PEM_END, 1)[0]

    def sign(self, data: bytes) -> bytes:
        """``data``'s RSA signature with SHA-1, the LoTW V2.0 signature."""
        return self.key.sign(data, padding.PKCS1v15(), hashes.SHA1())  # noqa: S303 - LoTW's spec

    def covers(self, day: date) -> bool:
        """Whether a QSO on ``day`` falls in the dates this certificate may sign."""
        return day >= self.qso_first and (self.qso_last is None or day <= self.qso_last)


def _extension_text(cert: x509.Certificate, oid: x509.ObjectIdentifier) -> str | None:
    try:
        extension = cert.extensions.get_extension_for_oid(oid)
    except x509.ExtensionNotFound:
        return None
    value = extension.value
    if not isinstance(value, x509.UnrecognizedExtension):
        return None
    try:
        return value.value.decode("ascii").strip()
    except UnicodeDecodeError:
        return None


def _date(text: str | None, what: str) -> date | None:
    if text is None or text == "":
        return None
    try:
        return date.fromisoformat(text[:10])
    except ValueError as error:
        raise CertificateError(f"the certificate's {what} date {text!r} is not a date") from error


def load_certificate(p12: bytes, password: str) -> CallsignCertificate:
    """Read a callsign certificate and its key from a ``.p12`` file.

    Raises:
        CertificateError: The file is too large, the password is wrong, or it
            holds no LoTW callsign certificate with an RSA key.
    """
    if len(p12) > MAX_FILE:
        raise CertificateError(f"the file is larger than {MAX_FILE // 1024} KB")
    try:
        key, cert, _chain = pkcs12.load_key_and_certificates(p12, password.encode() or None)
    except ValueError as error:
        raise CertificateError(
            "the file could not be opened: the password is wrong, or it is not a .p12 file"
        ) from error
    if cert is None or key is None:
        raise CertificateError("the file has no certificate with its private key")
    if not isinstance(key, rsa.RSAPrivateKey):
        raise CertificateError("the certificate's key is not an RSA key")
    names = cert.subject.get_attributes_for_oid(OID_CALLSIGN)
    raw_call = names[0].value if names else None
    call = raw_call.strip().upper() if isinstance(raw_call, str) else ""
    if not _CALL.match(call) or len(call) > 20:  # noqa: PLR2004 - the log's callsign limit
        raise CertificateError("this is not a LoTW callsign certificate (it names no callsign)")
    dxcc_text = _extension_text(cert, OID_DXCC)
    if dxcc_text is None or not dxcc_text.isdigit():
        raise CertificateError("the certificate names no DXCC entity")
    first = _date(_extension_text(cert, OID_QSO_NOT_BEFORE), "first QSO")
    if first is None:
        raise CertificateError("the certificate names no first QSO date")
    return CallsignCertificate(
        call=call,
        dxcc=int(dxcc_text),
        qso_first=first,
        qso_last=_date(_extension_text(cert, OID_QSO_NOT_AFTER), "last QSO"),
        expires=cert.not_valid_after_utc.astimezone(UTC),
        serial=cert.serial_number,
        certificate=cert,
        key=key,
    )
