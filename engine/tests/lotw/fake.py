# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Stand-ins for LoTW: a made-up callsign certificate, and a LoTW server that checks uploads.

The certificate is self-signed here, carrying the callsign and the extensions
TrustedQSL's published source defines; it is not issued by LoTW and could
never be accepted by it. The server checks every signature in an upload the
way LoTW's published interface describes, so a test that passes has sent
files LoTW could verify.
"""

from __future__ import annotations

import base64
import gzip
import re
import urllib.parse
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta

from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.hazmat.primitives.serialization import BestAvailableEncryption, pkcs12
from cryptography.x509.oid import NameOID

from loglab_engine.lotw import LotwUnreachableError
from loglab_engine.lotw.certificate import (
    OID_CALLSIGN,
    OID_DXCC,
    OID_QSO_NOT_AFTER,
    OID_QSO_NOT_BEFORE,
)

CERT_PASSWORD = "cert-pass"  # noqa: S105 - a made-up certificate's password
LOTW_PASSWORD = "lotw-pass"  # noqa: S105 - a stand-in LoTW account
_KEY = rsa.generate_private_key(public_exponent=65537, key_size=2048)


def make_p12(  # noqa: PLR0913 - one argument per thing a test varies
    *,
    call: str = "K9KSU",
    dxcc: str = "291",
    first: str = "1990-01-01",
    last: str | None = None,
    password: str = CERT_PASSWORD,
    key: rsa.RSAPrivateKey = _KEY,
) -> bytes:
    """A ``.p12`` file like the one TrustedQSL saves, for a made-up certificate."""
    subject = x509.Name(
        [
            x509.NameAttribute(NameOID.COMMON_NAME, "Test Operator"),
            x509.NameAttribute(OID_CALLSIGN, call),
        ]
    )
    now = datetime.now(UTC)
    builder = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(4242)
        .not_valid_before(now - timedelta(days=1))
        .not_valid_after(now + timedelta(days=365))
        .add_extension(x509.UnrecognizedExtension(OID_DXCC, dxcc.encode()), critical=False)
        .add_extension(
            x509.UnrecognizedExtension(OID_QSO_NOT_BEFORE, first.encode()), critical=False
        )
    )
    if last is not None:
        builder = builder.add_extension(
            x509.UnrecognizedExtension(OID_QSO_NOT_AFTER, last.encode()), critical=False
        )
    cert = builder.sign(key, hashes.SHA256())
    encryption = BestAvailableEncryption(password.encode())
    return pkcs12.serialize_key_and_certificates(b"k9ksu", key, cert, None, encryption)


_FIELD = re.compile(r"<([A-Za-z0-9_.]+):(\d+)(?::[A-Za-z0-9])?>", re.ASCII)


def read_gabbi(tq8: bytes) -> list[dict[str, str]]:
    """The records of a ``.tq8`` file, each as its fields (``Rec_Type`` included)."""
    text = gzip.decompress(tq8).decode()
    records: list[dict[str, str]] = []
    current: dict[str, str] = {}
    pos = 0
    while True:
        found = _FIELD.search(text, pos)
        eor = text.lower().find("<eor>", pos)
        if eor != -1 and (found is None or eor < found.start()):
            records.append(current)
            current = {}
            pos = eor + 5
            continue
        if found is None:
            break
        start, length = found.end(), int(found.group(2))  # ASCII, so characters are bytes
        current[found.group(1)] = text[start : start + length]
        pos = start + length
    if current:
        records.append(current)
    return records


def verify(tq8: bytes) -> list[dict[str, str]]:
    """The signed contacts of an upload, after checking each signature as LoTW would.

    Raises:
        AssertionError: A signature does not verify, or the file is malformed.
    """
    records = read_gabbi(tq8)
    assert "TQSL_IDENT" in records[0]
    cert_rec = next(r for r in records if r.get("Rec_Type") == "tCERT")
    pem = "-----BEGIN CERTIFICATE-----\n" + cert_rec["CERTIFICATE"] + "-----END CERTIFICATE-----\n"
    public = x509.load_pem_x509_certificate(pem.encode()).public_key()
    assert isinstance(public, rsa.RSAPublicKey)
    contacts = [r for r in records if r.get("Rec_Type") == "tCONTACT"]
    for contact in contacts:
        signature = base64.b64decode(contact["SIGN_LOTW_V2.0"])
        public.verify(
            signature,
            contact["SIGNDATA"].encode(),
            padding.PKCS1v15(),
            hashes.SHA1(),  # noqa: S303
        )
    return contacts


@dataclass
class FakeLotw:
    """LoTW's upload and report interfaces, with one account."""

    online: bool = True
    refuse: str | None = None
    uploads: list[list[dict[str, str]]] = field(default_factory=list)
    queries: list[dict[str, str]] = field(default_factory=list)
    report: bytes = b""

    def __call__(
        self, url: str, body: bytes | None, headers: dict[str, str], limit: int, timeout: float
    ) -> bytes:
        """Answer one request."""
        del limit, timeout
        if not self.online:
            raise LotwUnreachableError("no internet")
        if url == "https://lotw.arrl.org/lotw/upload":
            assert body is not None
            boundary = headers["Content-Type"].split("boundary=")[1]
            part = body.split(f"--{boundary}".encode())[1]
            assert b'name="upfile"' in part
            tq8 = part.split(b"\r\n\r\n", 1)[1].removesuffix(b"\r\n")
            self.uploads.append(verify(tq8))
            if self.refuse is not None:
                return f"<!-- .UPL. rejected --><!-- .UPLMESSAGE. {self.refuse} -->".encode()
            return b"<html><!-- .UPL.  accepted --><!-- .UPLMESSAGE. Log uploaded --></html>"
        split = urllib.parse.urlsplit(url)
        assert split.netloc == "lotw.arrl.org"
        assert split.path == "/lotwuser/lotwreport.adi"
        query = dict(urllib.parse.parse_qsl(split.query))
        self.queries.append(query)
        if query["login"] != "k9ksu" or query["password"] != LOTW_PASSWORD:
            return b"<html><body>Username/password incorrect</body></html>"
        return self.report


def adif(name: str, value: str) -> str:
    """One ADIF field."""
    return f"<{name}:{len(value)}>{value}"


def report(*records: dict[str, str], last_qsl: str = "2026-10-18 02:00:00") -> bytes:
    """A ``lotwreport.adi`` answer carrying ``records``."""
    head = (
        "ARRL Logbook of the World Status Report\nGenerated at 2026-10-18 03:00:00\n"
        + adif("PROGRAMID", "LoTW")
        + adif("APP_LoTW_LASTQSL", last_qsl)
        + adif("APP_LoTW_NUMREC", str(len(records)))
        + "\n<eoh>\n"
    )
    body = "".join("".join(adif(k, v) for k, v in r.items()) + "\n<eor>\n" for r in records)
    return (head + body).encode()
