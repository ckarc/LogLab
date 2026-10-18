-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Each operator's workspace layout: which modules the web page shows, in what
-- order, and how wide.
--
-- * A layout belongs to the operator, not to a workstation: signing in
--   anywhere brings it back. It is kept by the Engine, so it needs no
--   internet connection (Principle V).
-- * It is not part of the log, so it has its own schema; nothing here can
--   touch a QSO (Principle VI keeps reference data apart in the same way).
-- * Saving a layout appends a row; the latest row is the current layout.
--   Only the operator, signed in, can save their own (Principle XIII), and
--   only the database's clock stamps it (Principle VIII).

CREATE SCHEMA workspace;
COMMENT ON SCHEMA workspace IS
    'Operator preferences, such as the web page layout. Kept apart from the log.';

CREATE TABLE workspace.layout (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id bigint NOT NULL REFERENCES log.operator,
    session_id  uuid NOT NULL REFERENCES log.session,
    layout      jsonb NOT NULL
                CHECK (jsonb_typeof(layout) = 'object' AND octet_length(layout::text) <= 16384),
    saved_at    timestamptz NOT NULL
);
CREATE INDEX layout_latest ON workspace.layout (operator_id, id DESC);

CREATE FUNCTION workspace.layout_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.saved_at := transaction_timestamp();
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.operator_id THEN
        RAISE EXCEPTION 'a layout is saved only by its own operator, signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER layout_check BEFORE INSERT ON workspace.layout
    FOR EACH ROW EXECUTE FUNCTION workspace.layout_check();
CREATE TRIGGER layout_append_only BEFORE UPDATE OR DELETE ON workspace.layout
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER layout_no_truncate BEFORE TRUNCATE ON workspace.layout
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
