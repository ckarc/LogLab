-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Lab profiles: for each radio model, the lab's default setup for each
-- program (VARA, FT8, JS8, fldigi, SSTV). An enrolled radio gets a
-- program's profile as that program starts (Principle X, second exception).
-- The Agent has one built in for each; a profile saved here takes its place
-- from then on.
--
-- * Only a signed-in administrator saves one, in LabLog, from the values a
--   station's radio has now: the Agent there reads them when the
--   administrator asks, and the administrator chooses which settings the
--   profile holds. Saving is the administrator's decision; there is no
--   separate approval.
-- * The model's profile for a program is the one saved most recently.
--   Nothing is changed in place: every version is kept, with who and when
--   (Principle VII), stamped by the database's clock (Principle VIII).
--   Going back to an earlier one saves it again, as a new version.
-- * Each setting is the bytes the radio sent, in hex, with the id and label
--   from the Agent's rig definition and the value in words as the Agent
--   showed it. The Engine never sends a setting to a radio.

CREATE TABLE log.lab_profile (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    maker       text NOT NULL CHECK (length(maker) BETWEEN 1 AND 40),
    model       text NOT NULL CHECK (length(model) BETWEEN 1 AND 40),
    program     text NOT NULL CHECK (program ~ '^[A-Za-z0-9]{1,16}$'),
    settings    jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'array'),
    note        text CHECK (length(note) BETWEEN 1 AND 500),
    -- Read from this radio at this station ...
    station_id  bigint REFERENCES log.station,
    serial      text CHECK (serial ~ '^[A-Za-z0-9._-]{1,64}$'),
    -- ... or an earlier version saved again.
    again_of    bigint REFERENCES log.lab_profile,
    -- The version it replaced, if any: none means the Agent's built-in one.
    replaces    bigint REFERENCES log.lab_profile,
    saved_by    bigint NOT NULL REFERENCES log.operator,
    session_id  uuid NOT NULL REFERENCES log.session,
    saved_at    timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CHECK ((station_id IS NOT NULL AND serial IS NOT NULL AND again_of IS NULL)
        OR (station_id IS NULL AND serial IS NULL AND again_of IS NOT NULL))
);
CREATE INDEX lab_profile_model ON log.lab_profile (maker, model, program, id DESC);
COMMENT ON TABLE log.lab_profile IS
    'Each model''s default setup for each program, every version; the newest is used.';

CREATE TRIGGER lab_profile_append_only BEFORE UPDATE OR DELETE ON log.lab_profile
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER lab_profile_no_truncate BEFORE TRUNCATE ON log.lab_profile
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- Only an administrator, signed in, in their own session, saves a profile,
-- whatever code asks; only the database's clock stamps it.
CREATE FUNCTION log.lab_profile_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.saved_by THEN
        RAISE EXCEPTION 'a lab profile needs its administrator to be signed in'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT log.operator_may(NEW.saved_by, 'admin') THEN
        RAISE EXCEPTION 'only an administrator can change a lab profile'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.saved_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER lab_profile_check BEFORE INSERT ON log.lab_profile
    FOR EACH ROW EXECUTE FUNCTION log.lab_profile_check();
