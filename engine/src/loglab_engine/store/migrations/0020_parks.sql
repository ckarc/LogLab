-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- The Parks on the Air park list, kept by the Engine itself so a park
-- reference fills in with no internet at all (Principles V and VI).
--
-- * The list comes from Parks on the Air (pota.app), which publishes every
--   park as one CSV file. Only what fills in a QSO or draws a park on a map
--   is kept: reference, name, whether it is active, its location (state or
--   province), its position and grid square.
-- * A refresh replaces the whole list in one transaction (Principle XIV):
--   readers see the old list until the new one is complete.
-- * Every import is recorded with its file and the date pota.app published
--   it (Principle VI).
-- * This is reference data, apart from the log (Principle VI): nothing here
--   can touch a QSO.

CREATE TABLE reference.park (
    -- Like US-1234 or VE-0042.
    reference text PRIMARY KEY CHECK (reference ~ '^[A-Z0-9]{1,4}-[0-9]{4,6}$'),
    name      text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    active    boolean NOT NULL,
    -- pota.app's location codes, like US-WI, comma-separated when a park spans several.
    locations text CHECK (length(locations) <= 200),
    latitude  double precision CHECK (latitude BETWEEN -90 AND 90),
    longitude double precision CHECK (longitude BETWEEN -180 AND 180),
    grid      text CHECK (grid ~ '^[A-R]{2}[0-9]{2}([A-X]{2}([0-9]{2})?)?$')
);
CREATE INDEX park_name ON reference.park (lower(name) text_pattern_ops);
COMMENT ON TABLE reference.park IS
    'Parks on the Air parks, for offline lookup (Principles V and VI).';

CREATE TABLE reference.park_import (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    file_name   text NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
    -- When pota.app published the file (its Last-Modified date), if known.
    published   timestamptz,
    records     integer NOT NULL CHECK (records >= 0),
    imported_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);
COMMENT ON TABLE reference.park_import IS
    'Each park list import: its file and publication date (Principle VI).';
