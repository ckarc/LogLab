-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Lab Stock: the lab's everyday settings for each radio model, which any of
-- the lab's radios of that model can be put back to between users
-- (Principle X: the Agent puts it on only when an operator asks and
-- confirms).
--
-- * A proposal is a whole set of settings for one maker and model. A Station
--   Agent sends one from the radio in front of it, or an administrator
--   proposes an earlier Lab Stock again. A proposal changes nothing by
--   itself.
-- * The gate: an administrator, signed in, approves or refuses each
--   proposal, once. The model's Lab Stock is the proposal approved most
--   recently. A station key can propose but never decide, and nothing is
--   ever changed in place: every proposal and every decision is kept, with
--   who and when (Principle VII), stamped by the database's clock
--   (Principle VIII).
-- * Each setting is the bytes the radio sent, in hex, with the id and label
--   from the Agent's rig definition and the value in words, as the Agent
--   showed it, for the administrator to read. The Engine never sends a
--   setting to a radio.

CREATE TABLE log.lab_stock (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    maker       text NOT NULL CHECK (length(maker) BETWEEN 1 AND 40),
    model       text NOT NULL CHECK (length(model) BETWEEN 1 AND 40),
    settings    jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'array'),
    note        text CHECK (length(note) BETWEEN 1 AND 500),
    -- Sent by a station's Agent, read from this radio ...
    station_id  bigint REFERENCES log.station,
    serial      text CHECK (serial ~ '^[A-Za-z0-9._-]{1,64}$'),
    -- ... or proposed again by an administrator from an earlier one.
    again_of    bigint REFERENCES log.lab_stock,
    -- Who was signed in: at the station, or the administrator.
    session_id  uuid REFERENCES log.session,
    proposed_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CHECK ((station_id IS NOT NULL AND serial IS NOT NULL AND again_of IS NULL)
        OR (station_id IS NULL AND serial IS NULL AND again_of IS NOT NULL
            AND session_id IS NOT NULL))
);
CREATE INDEX lab_stock_model ON log.lab_stock (maker, model, id DESC);
COMMENT ON TABLE log.lab_stock IS
    'Proposed Lab Stock settings for each radio model; each takes effect only once approved.';

CREATE TABLE log.lab_stock_decision (
    proposal_id bigint PRIMARY KEY REFERENCES log.lab_stock,
    approved    boolean NOT NULL,
    decided_by  bigint NOT NULL REFERENCES log.operator,
    session_id  uuid NOT NULL REFERENCES log.session,
    decided_at  timestamptz NOT NULL DEFAULT transaction_timestamp()
);
CREATE INDEX lab_stock_decision_order ON log.lab_stock_decision (decided_at DESC, proposal_id DESC);
COMMENT ON TABLE log.lab_stock_decision IS
    'An administrator''s approval or refusal of a Lab Stock proposal, made once.';

CREATE TRIGGER lab_stock_append_only BEFORE UPDATE OR DELETE ON log.lab_stock
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER lab_stock_decision_append_only BEFORE UPDATE OR DELETE ON log.lab_stock_decision
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER lab_stock_no_truncate BEFORE TRUNCATE ON log.lab_stock
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER lab_stock_decision_no_truncate BEFORE TRUNCATE ON log.lab_stock_decision
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- Only the database's clock stamps a proposal, and one proposed again by an
-- administrator needs that administrator signed in.
CREATE FUNCTION log.lab_stock_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.again_of IS NOT NULL AND (
        NOT log.session_live(NEW.session_id)
        OR NOT log.operator_may(
            (SELECT operator_id FROM log.session WHERE id = NEW.session_id), 'admin')) THEN
        RAISE EXCEPTION 'only a signed-in administrator can propose a Lab Stock again'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.proposed_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER lab_stock_check BEFORE INSERT ON log.lab_stock
    FOR EACH ROW EXECUTE FUNCTION log.lab_stock_check();

-- The gate itself: a decision is made by an administrator in their own live
-- session, whatever code makes it.
CREATE FUNCTION log.lab_stock_decision_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.decided_by THEN
        RAISE EXCEPTION 'a Lab Stock decision needs its administrator to be signed in'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT log.operator_may(NEW.decided_by, 'admin') THEN
        RAISE EXCEPTION 'only an administrator can approve or refuse Lab Stock'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.decided_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER lab_stock_decision_check BEFORE INSERT ON log.lab_stock_decision
    FOR EACH ROW EXECUTE FUNCTION log.lab_stock_decision_check();
