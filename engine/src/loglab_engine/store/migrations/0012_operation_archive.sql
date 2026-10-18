-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Archiving an operation: once its log has been submitted, it is locked so it
-- always matches what was sent in.
--
-- * Only an ended operation is archived, by whoever may change it (Principle
--   XIII), and archiving can't be undone. An archived operation's settings
--   never change again.
-- * No QSO logged in it (its APP_LOGLAB_OPERATION) can be added, edited,
--   deleted or restored, and no QSO can be moved into it. Its history stays
--   readable, as every QSO's does (Principle VII).
-- * The database enforces this, so no code path can skip it.

ALTER TABLE workspace.operation
    ADD COLUMN archived_at timestamptz,
    ADD CONSTRAINT operation_archived_ended CHECK (archived_at IS NULL OR ended);

-- Whether a QSO with these fields belongs to an archived operation.
CREATE FUNCTION workspace.in_archive(fields jsonb)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT EXISTS (
        SELECT 1 FROM workspace.operation
        WHERE archived_at IS NOT NULL
          AND id::text = lower(btrim(log.field_value(fields, 'APP_LOGLAB_OPERATION')))
    )
$$;

CREATE FUNCTION workspace.operation_archive_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.archived_at IS NOT NULL THEN
            RAISE EXCEPTION 'an operation is archived only after it has ended'
                USING ERRCODE = 'object_not_in_prerequisite_state';
        END IF;
    ELSIF OLD.archived_at IS NOT NULL THEN
        RAISE EXCEPTION 'the operation % is archived, so it can''t be changed', OLD.name
            USING ERRCODE = 'object_not_in_prerequisite_state';
    ELSIF NEW.archived_at IS NOT NULL THEN
        NEW.archived_at := transaction_timestamp();
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER operation_archive_guard BEFORE INSERT OR UPDATE ON workspace.operation
    FOR EACH ROW EXECUTE FUNCTION workspace.operation_archive_guard();

CREATE FUNCTION workspace.qso_archive_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    previous jsonb;
BEGIN
    IF NEW.version > 1 THEN
        SELECT fields INTO previous FROM log.qso_version
        WHERE qso_id = NEW.qso_id AND version = NEW.version - 1;
    END IF;
    IF workspace.in_archive(NEW.fields) OR coalesce(workspace.in_archive(previous), false) THEN
        RAISE EXCEPTION 'that QSO is in an archived operation, so it can''t be changed'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qso_version_archive_guard BEFORE INSERT ON log.qso_version
    FOR EACH ROW EXECUTE FUNCTION workspace.qso_archive_guard();
