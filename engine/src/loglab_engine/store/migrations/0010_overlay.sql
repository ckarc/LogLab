-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Overlays an administrator makes or copies in the editor.
--
-- * An overlay is declarative data (Principle III). The API parses each one
--   into the same strict model as the overlay files the Engine ships before it
--   arrives here (Principle XII); one row holds one whole definition, written
--   in one transaction (Principle XIV).
-- * Only a signed-in administrator saves or retires one (Principle XIII).
-- * Every save is a new version and nothing is erased: the latest version is
--   the overlay as it stands, and a retired overlay can be saved again.

CREATE TABLE workspace.overlay_version (
    overlay_id text NOT NULL CHECK (overlay_id ~ '^[a-z][a-z0-9-]{0,31}$'),
    version    integer NOT NULL CHECK (version >= 1),
    retired    boolean NOT NULL DEFAULT false,
    definition jsonb CHECK (
        definition IS NULL
        OR (jsonb_typeof(definition) = 'object' AND octet_length(definition::text) <= 65536)
    ),
    session_id uuid NOT NULL REFERENCES log.session,
    saved_at   timestamptz NOT NULL,
    PRIMARY KEY (overlay_id, version),
    CHECK (retired OR definition IS NOT NULL)
);
COMMENT ON TABLE workspace.overlay_version IS
    'Every version of every overlay made in the editor; the latest is current.';

CREATE FUNCTION workspace.overlay_version_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR NOT log.operator_may((SELECT operator_id FROM log.session WHERE id = NEW.session_id), 'admin') THEN
        RAISE EXCEPTION 'only an administrator, signed in, changes an overlay (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtext('workspace.overlay_version:' || NEW.overlay_id));
    IF NEW.version <> coalesce(
        (SELECT max(version) FROM workspace.overlay_version WHERE overlay_id = NEW.overlay_id), 0
    ) + 1 THEN
        RAISE EXCEPTION 'overlay % has changed since it was opened', NEW.overlay_id
            USING ERRCODE = 'serialization_failure';
    END IF;
    NEW.saved_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER overlay_version_check BEFORE INSERT ON workspace.overlay_version
    FOR EACH ROW EXECUTE FUNCTION workspace.overlay_version_check();
CREATE TRIGGER overlay_version_append_only BEFORE UPDATE OR DELETE ON workspace.overlay_version
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER overlay_version_no_truncate BEFORE TRUNCATE ON workspace.overlay_version
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
