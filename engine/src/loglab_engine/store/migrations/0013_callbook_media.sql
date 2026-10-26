-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- A callbook's biography and photo of a call, kept beside its entry.
--
-- * Reference data, apart from the log (Principle VI): fetched once, then
--   shown with no internet (Principle V) until an operator asks for a fresh copy.
-- * A biography is kept as plain text only, never the page it came from
--   (Principle XII). A photo is kept only as a JPEG, PNG, GIF or WebP picture,
--   with the address it came from, so a new photo is fetched when that changes.
-- * Only the database's clock stamps it (Principle VIII).

CREATE TABLE reference.callbook_media (
    source       text NOT NULL CHECK (source IN ('qrz')),
    call         text NOT NULL CHECK (call ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(call) <= 20),
    kind         text NOT NULL CHECK (kind IN ('bio', 'photo')),
    origin       text CHECK (length(origin) <= 300),
    content_type text NOT NULL CHECK (
        (kind = 'bio' AND content_type = 'text/plain')
        OR (kind = 'photo' AND content_type IN ('image/jpeg', 'image/png', 'image/gif', 'image/webp'))
    ),
    content      bytea NOT NULL CHECK (octet_length(content) <= 4194304),
    fetched_at   timestamptz NOT NULL,
    PRIMARY KEY (source, call, kind)
);
COMMENT ON TABLE reference.callbook_media IS
    'A callbook''s biography (plain text) and photo of a call, fetched once (Principle VI).';

CREATE TRIGGER callbook_media_stamp BEFORE INSERT OR UPDATE ON reference.callbook_media
    FOR EACH ROW EXECUTE FUNCTION reference.callbook_stamp();
