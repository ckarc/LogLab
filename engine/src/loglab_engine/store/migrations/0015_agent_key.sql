-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Station keys: how a Station Agent proves which station it is (Principle XIII).
--
-- * An administrator makes a key for a station, in a live session. The Agent
--   at that station keeps the key and sends it with every report of its
--   radio's state. Only the key's SHA-256 is stored, so a copy of the
--   database cannot be used to pose as a station.
-- * A station has at most one working key: making a new one ends the old one,
--   and an administrator can end a station's key outright.
-- * Keys and their ends are kept forever, with who made them and when
--   (Principle VII). Only the database's clock stamps them (Principle VIII).
-- * A station key lets an Agent report its radio. It grants no operator
--   permission at all: it cannot log, and it can never transmit.

CREATE TABLE log.agent_key (
    id         uuid PRIMARY KEY,
    station_id bigint NOT NULL REFERENCES log.station,
    key_sha256 text NOT NULL UNIQUE CHECK (key_sha256 ~ '^[0-9a-f]{64}$'),
    made_by    bigint NOT NULL REFERENCES log.operator,
    session_id uuid NOT NULL REFERENCES log.session,
    made_at    timestamptz NOT NULL
);
CREATE INDEX agent_key_station ON log.agent_key (station_id, made_at DESC);

CREATE TABLE log.agent_key_end (
    key_id     uuid PRIMARY KEY REFERENCES log.agent_key,
    ended_by   bigint NOT NULL REFERENCES log.operator,
    session_id uuid NOT NULL REFERENCES log.session,
    ended_at   timestamptz NOT NULL
);

CREATE TRIGGER agent_key_append_only BEFORE UPDATE OR DELETE ON log.agent_key
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER agent_key_end_append_only BEFORE UPDATE OR DELETE ON log.agent_key_end
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER agent_key_no_truncate BEFORE TRUNCATE ON log.agent_key
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER agent_key_end_no_truncate BEFORE TRUNCATE ON log.agent_key_end
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- Keys are made and ended by an administrator in their own live session.
CREATE FUNCTION log.agent_key_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    maker bigint;
BEGIN
    -- Nested so each table's own columns are only read for that table.
    IF TG_TABLE_NAME = 'agent_key' THEN
        maker := NEW.made_by;
    ELSE
        maker := NEW.ended_by;
    END IF;
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> maker THEN
        RAISE EXCEPTION 'a station key change needs its maker to be signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT log.operator_may(maker, 'admin') THEN
        RAISE EXCEPTION 'only an administrator can make or end station keys (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF TG_TABLE_NAME = 'agent_key' THEN
        NEW.made_at := transaction_timestamp();
    ELSE
        NEW.ended_at := transaction_timestamp();
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER agent_key_check BEFORE INSERT ON log.agent_key
    FOR EACH ROW EXECUTE FUNCTION log.agent_key_check();
CREATE TRIGGER agent_key_end_check BEFORE INSERT ON log.agent_key_end
    FOR EACH ROW EXECUTE FUNCTION log.agent_key_check();
