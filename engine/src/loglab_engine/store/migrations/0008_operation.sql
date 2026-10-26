-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Operations: one club's use of one overlay, such as "Field Day 2027 as W0CK".
--
-- * An operation names the overlay (a contest template, Principle III), the
--   station callsign every QSO is made as, an optional GOTA callsign (ARRL
--   Field Day Rule 4.1.1.1: the GOTA station uses its own call), and the
--   exchange the station sends. It adds no columns for any one contest.
-- * Every operator still signs in as themselves; the QSO's changeset records
--   who logged it (Principles VII and XIII). Only an administrator, signed in,
--   creates or changes an operation.
-- * It is not part of the log, so it lives in the workspace schema. Ending an
--   operation keeps it; nothing here is ever erased.

CREATE TABLE workspace.operation (
    id           uuid PRIMARY KEY,
    overlay      text NOT NULL CHECK (overlay ~ '^[a-z][a-z0-9-]{0,31}$'),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 60),
    station_call text NOT NULL
                 CHECK (station_call ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(station_call) <= 20),
    gota_call    text CHECK (gota_call ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(gota_call) <= 20),
    sent         jsonb NOT NULL DEFAULT '{}'::jsonb
                 CHECK (jsonb_typeof(sent) = 'object' AND octet_length(sent::text) <= 4096),
    ended        boolean NOT NULL DEFAULT false,
    session_id   uuid NOT NULL REFERENCES log.session,
    created_at   timestamptz NOT NULL,
    changed_at   timestamptz NOT NULL,
    CHECK (gota_call IS NULL OR gota_call <> station_call)
);
COMMENT ON TABLE workspace.operation IS
    'A club''s use of one overlay: its station call, GOTA call and sent exchange.';

CREATE FUNCTION workspace.operation_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR NOT log.operator_may((SELECT operator_id FROM log.session WHERE id = NEW.session_id), 'admin') THEN
        RAISE EXCEPTION 'only an administrator, signed in, sets up an operation (Principle XIII)'
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

CREATE TRIGGER operation_check BEFORE INSERT OR UPDATE ON workspace.operation
    FOR EACH ROW EXECUTE FUNCTION workspace.operation_check();
CREATE TRIGGER operation_no_delete BEFORE DELETE ON workspace.operation
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER operation_no_truncate BEFORE TRUNCATE ON workspace.operation
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
