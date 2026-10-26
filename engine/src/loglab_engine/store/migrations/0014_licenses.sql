-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- The US and Canadian amateur license lists, kept by the Engine itself so a
-- call fills in with no internet at all (Principles V and VI).
--
-- * US calls come from the FCC's Universal Licensing System (public domain),
--   Canadian calls from ISED's amateur list (Open Government Licence - Canada).
-- * Only what fills in a QSO is kept: call, name, city, state or province,
--   license class, and for US licenses whether it is active or recently
--   expired, and when it expires. No street address, email or phone.
--   Cancelled licenses, and ones expired past the two-year grace period, are
--   not kept.
-- * A refresh writes into this table in one transaction (Principle XIV):
--   readers see the old list until the new one is complete.
-- * Every import is recorded with its source and the date the agency
--   published it (Principle VI).
-- * This is reference data, apart from the log (Principle VI): nothing here
--   can touch a QSO.

CREATE TABLE reference.license (
    source    text NOT NULL CHECK (source IN ('fcc', 'ised')),
    -- The FCC's unique system identifier for the license; for ISED, the call.
    record_id text NOT NULL CHECK (record_id ~ '^[A-Z0-9]{1,20}$'),
    call      text NOT NULL CHECK (call ~ '^[A-Z0-9]+$' AND length(call) <= 20),
    name      text CHECK (length(name) <= 128),
    city      text CHECK (length(city) <= 64),
    state     text CHECK (length(state) <= 32),
    class     text CHECK (length(class) <= 32),
    active    boolean NOT NULL,
    expires   date,
    PRIMARY KEY (source, record_id)
);
CREATE INDEX license_call ON reference.license (call);
COMMENT ON TABLE reference.license IS
    'US (FCC) and Canadian (ISED) amateur licenses, for offline lookup (Principles V and VI).';

CREATE TABLE reference.license_import (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      text NOT NULL CHECK (source IN ('fcc', 'ised')),
    -- 'full' replaces every license from the source; 'daily' applies one day's changes.
    kind        text NOT NULL CHECK (kind IN ('full', 'daily')),
    file_name   text NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
    -- When the agency published the file (its Last-Modified date), if known.
    published   timestamptz,
    records     integer NOT NULL CHECK (records >= 0),
    imported_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);
CREATE INDEX license_import_latest ON reference.license_import (source, kind, imported_at);
COMMENT ON TABLE reference.license_import IS
    'Each license list import: its source, file, and publication date (Principle VI).';
