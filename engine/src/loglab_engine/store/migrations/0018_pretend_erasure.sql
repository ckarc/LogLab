-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Erasing pretend QSOs (Constitution 2.1.0, Principle VII).
--
-- A pretend QSO (APP_LOGLAB_PRETEND = Y, which only a practice operation
-- takes) is not a contact, so an administrator may erase the pretend QSOs of
-- a practice operation entirely, versions and all, as when demo mode is
-- turned off. Everything else in the log stays append-only.
--
-- * log.erase_pretend(operation, session) is the only way: the session must
--   be live and its operator an administrator, the operation a practice one.
--   It erases only QSOs every version of which is pretend and logged in that
--   operation.
-- * The erasure itself is recorded in log.pretend_erasure (who, when, which
--   operation, how many), which is append-only.
-- * The changesets that made the QSOs stay: they hold no QSO data, only who
--   entered something, when, and why.

CREATE TABLE log.pretend_erasure (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operation_id uuid NOT NULL,
    operator_id  bigint NOT NULL REFERENCES log.operator,
    session_id   uuid NOT NULL REFERENCES log.session,
    qsos         integer NOT NULL CHECK (qsos >= 0),
    erased_at    timestamptz NOT NULL DEFAULT transaction_timestamp()
);

CREATE TRIGGER pretend_erasure_append_only BEFORE UPDATE OR DELETE ON log.pretend_erasure
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER pretend_erasure_no_truncate BEFORE TRUNCATE ON log.pretend_erasure
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- The log's tables refuse every change, except deleting a pretend QSO's rows
-- while log.erase_pretend is erasing them.
CREATE FUNCTION log.erasing_pretend() RETURNS text
LANGUAGE sql STABLE
AS $$
    SELECT nullif(current_setting('loglab.erasing_pretend', true), '')
$$;

CREATE FUNCTION log.qso_version_change_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE'
       AND log.erasing_pretend() IS NOT NULL
       AND workspace.is_pretend(OLD.fields)
       AND workspace.in_practice(OLD.fields)
       AND lower(btrim(log.field_value(OLD.fields, 'APP_LOGLAB_OPERATION')))
           = log.erasing_pretend() THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'log.% is append-only: % is not allowed (Principle VII)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE FUNCTION log.qso_delete_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    -- Only a QSO whose versions have all just been erased.
    IF log.erasing_pretend() IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM log.qso_version v WHERE v.qso_id = OLD.id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'log.% is append-only: % is not allowed (Principle VII)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'restrict_violation';
END;
$$;

DROP TRIGGER qso_version_append_only ON log.qso_version;
CREATE TRIGGER qso_version_append_only BEFORE UPDATE OR DELETE ON log.qso_version
    FOR EACH ROW EXECUTE FUNCTION log.qso_version_change_guard();
DROP TRIGGER qso_no_delete ON log.qso;
CREATE TRIGGER qso_no_delete BEFORE DELETE ON log.qso
    FOR EACH ROW EXECUTE FUNCTION log.qso_delete_guard();

-- Erase the pretend QSOs logged in practice operation ``op`` and record that
-- the session ``sid`` did; returns how many were erased.
CREATE FUNCTION log.erase_pretend(op uuid, sid uuid) RETURNS integer
LANGUAGE plpgsql
AS $$
DECLARE
    who bigint;
    doomed uuid[];
BEGIN
    SELECT s.operator_id INTO who FROM log.session s WHERE s.id = sid;
    IF who IS NULL OR NOT log.session_live(sid) OR NOT log.operator_may(who, 'admin') THEN
        RAISE EXCEPTION 'only a signed-in administrator erases pretend QSOs (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM workspace.operation o WHERE o.id = op AND o.practice) THEN
        RAISE EXCEPTION 'pretend QSOs are erased only from a practice operation (Principle VII)'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    -- QSOs every version of which is pretend and in this operation.
    SELECT coalesce(array_agg(q.id), '{}') INTO doomed
    FROM log.qso q
    WHERE EXISTS (SELECT 1 FROM log.qso_version v WHERE v.qso_id = q.id)
      AND NOT EXISTS (
          SELECT 1 FROM log.qso_version v
          WHERE v.qso_id = q.id
            AND NOT (workspace.is_pretend(v.fields)
                     AND lower(btrim(log.field_value(v.fields, 'APP_LOGLAB_OPERATION'))) = op::text)
      );
    PERFORM set_config('loglab.erasing_pretend', op::text, true);
    DELETE FROM workspace.qrz_upload WHERE qso_id = ANY (doomed);
    DELETE FROM log.qso_version WHERE qso_id = ANY (doomed);
    DELETE FROM log.qso WHERE id = ANY (doomed);
    PERFORM set_config('loglab.erasing_pretend', '', true);
    INSERT INTO log.pretend_erasure (operation_id, operator_id, session_id, qsos)
    VALUES (op, who, sid, cardinality(doomed));
    RETURN cardinality(doomed);
END;
$$;
