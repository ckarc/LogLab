-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Radio settings, kept by radio so a radio is recognized at every station
-- (Principle X).
--
-- * A radio is its maker, model and serial number (the one its USB serial
--   port reports), so two radios of the same model are kept apart.
-- * Its as-received record is what its menu and controls said the first time
--   a Station Agent met it. There is at most one per radio, and it is never
--   replaced.
-- * A setup is a reading the person at the radio chose to keep under a name,
--   like FT8 or VARA. Keeping one under a name it already has adds a new
--   version; the newest is the setup. Every version is kept.
-- * Each setting is kept as the bytes the radio sent, with its id and label
--   from the Agent's rig definition, so it can be compared and put back
--   whatever it means.
-- * Rows are only ever added, stamped by the database's clock
--   (Principle VIII), with the station that sent them and who was signed in
--   there, if anyone. Nothing here can touch the log, and nothing here is
--   sent to a radio by the Engine: putting a setup back is the Agent's, on
--   the operator's confirmed request (Principle X).

CREATE TABLE log.radio_setup (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    maker       text NOT NULL CHECK (length(maker) BETWEEN 1 AND 40),
    model       text NOT NULL CHECK (length(model) BETWEEN 1 AND 40),
    serial      text NOT NULL CHECK (serial ~ '^[A-Za-z0-9._-]{1,64}$'),
    -- NULL for the as-received record.
    name        text CHECK (length(name) BETWEEN 1 AND 40),
    settings    jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'array'),
    station_id  bigint NOT NULL REFERENCES log.station,
    session_id  uuid REFERENCES log.session,
    saved_at    timestamptz NOT NULL DEFAULT transaction_timestamp()
);
CREATE INDEX radio_setup_radio ON log.radio_setup (maker, model, serial, name, id DESC);
CREATE UNIQUE INDEX radio_setup_one_as_received ON log.radio_setup (maker, model, serial)
    WHERE name IS NULL;
COMMENT ON TABLE log.radio_setup IS
    'Each radio''s as-received settings and its named setups, every version (Principle X).';

CREATE TRIGGER radio_setup_append_only BEFORE UPDATE OR DELETE ON log.radio_setup
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER radio_setup_no_truncate BEFORE TRUNCATE ON log.radio_setup
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- Only the database's clock stamps a row.
CREATE FUNCTION log.radio_setup_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.saved_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER radio_setup_stamp BEFORE INSERT ON log.radio_setup
    FOR EACH ROW EXECUTE FUNCTION log.radio_setup_stamp();
