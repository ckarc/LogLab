-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Practice operations: for trying LabLog without touching the real log's
-- outside world, such as working the Station Agent's pretend FT8 band.
--
-- * An operation is a practice one from the moment it is set up, or never:
--   the choice can't be changed afterwards, so real QSOs never become
--   practice ones or the other way round.
-- * A QSO made with a pretend station carries APP_LOGLAB_PRETEND = Y. It can
--   only be logged in a practice operation, so a made-up contact can never
--   land in a real one (Principle XX: an inference or a made-up signal is
--   never presented as a real contact).
-- * QSOs that are pretend, or logged in a practice operation, are never sent
--   to LoTW or QRZ.com.
-- * The database enforces all of this, so no code path can skip it.

ALTER TABLE workspace.operation
    ADD COLUMN practice boolean NOT NULL DEFAULT false;

CREATE FUNCTION workspace.operation_practice_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.practice IS DISTINCT FROM OLD.practice THEN
        RAISE EXCEPTION 'whether % is a practice operation was set when it was made, and can''t change', OLD.name
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER operation_practice_guard BEFORE UPDATE ON workspace.operation
    FOR EACH ROW EXECUTE FUNCTION workspace.operation_practice_guard();

-- Whether a QSO with these fields was made with a pretend station.
CREATE FUNCTION workspace.is_pretend(fields jsonb)
RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$
    SELECT coalesce(upper(btrim(log.field_value(fields, 'APP_LOGLAB_PRETEND'))), '') = 'Y'
$$;

-- Whether a QSO with these fields is logged in a practice operation.
CREATE FUNCTION workspace.in_practice(fields jsonb)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT EXISTS (
        SELECT 1 FROM workspace.operation
        WHERE practice
          AND id::text = lower(btrim(log.field_value(fields, 'APP_LOGLAB_OPERATION')))
    )
$$;

-- Whether a QSO is practice only: pretend, or in a practice operation. Such
-- QSOs are never sent to LoTW or QRZ.com.
CREATE FUNCTION workspace.is_practice(fields jsonb)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT workspace.is_pretend(fields) OR workspace.in_practice(fields)
$$;

CREATE FUNCTION workspace.qso_pretend_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF workspace.is_pretend(NEW.fields) AND NOT workspace.in_practice(NEW.fields) THEN
        RAISE EXCEPTION 'That contact was with a pretend station, so it can only be logged in a practice operation. Set one up in the Menu, under Operation.'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qso_version_pretend_guard BEFORE INSERT ON log.qso_version
    FOR EACH ROW EXECUTE FUNCTION workspace.qso_pretend_guard();
