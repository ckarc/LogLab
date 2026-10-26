-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- An operator may set up an operation under their own callsign, such as Field
-- Day from home as 1D. An operation under any other call (a club call) still
-- needs an administrator (Principle XIII). An operator who may log can change
-- or end their own-call operation, and nobody else's.

CREATE OR REPLACE FUNCTION workspace.operation_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    op_id bigint := (SELECT operator_id FROM log.session WHERE id = NEW.session_id);
    own   text := (SELECT callsign FROM log.operator WHERE id = op_id);
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR NOT (
            log.operator_may(op_id, 'admin')
            OR (log.operator_may(op_id, 'log')
                AND NEW.station_call = own
                AND NEW.gota_call IS NULL
                AND (TG_OP = 'INSERT' OR OLD.station_call = own))
        ) THEN
        RAISE EXCEPTION 'an operation under a call other than your own is set up by an administrator (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.changed_at := transaction_timestamp();
    IF TG_OP = 'INSERT' THEN
        NEW.created_at := transaction_timestamp();
    ELSE
        NEW.created_at := OLD.created_at;
        NEW.id := OLD.id;
    END IF;
    RETURN NEW;
END;
$$;
