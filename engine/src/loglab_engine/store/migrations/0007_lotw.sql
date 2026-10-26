-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Logbook of the World: each operator's LoTW set-up.
--
-- * LoTW is an enhancement. With nothing set up, or no internet, nothing here
--   is used and logging works exactly the same (Principle V).
-- * A set-up belongs to one operator, who saves, changes, or removes it only
--   while signed in (Principle XIII). It holds:
--   - the callsign certificate file (.p12) TrustedQSL saves, and its password,
--     so the Engine can sign uploads. Anyone who can read this database can
--     sign QSOs with it. The API never returns either.
--   - the LoTW website login, to fetch confirmations. Kept as typed; the API
--     never returns the password.
--   - the station location LoTW records with each upload.
--   - the date of the last confirmation fetched, so each fetch asks only for
--     what is new (Principle V: fetch each thing once).
-- * What was sent and what was confirmed is not kept here: it is written to
--   each QSO's own ADIF fields (LOTW_QSL_SENT, LOTW_QSL_RCVD and their dates)
--   through the audited path (Principles IV and VII).

CREATE TABLE workspace.lotw_account (
    operator_id          bigint PRIMARY KEY REFERENCES log.operator,
    session_id           uuid NOT NULL REFERENCES log.session,
    certificate          bytea CHECK (octet_length(certificate) BETWEEN 1 AND 65536),
    certificate_password text CHECK (length(certificate_password) <= 128),
    username             text CHECK (length(username) BETWEEN 1 AND 64),
    password             text CHECK (length(password) BETWEEN 1 AND 128),
    location             jsonb
                         CHECK (jsonb_typeof(location) = 'object'
                                AND octet_length(location::text) <= 1024),
    qsl_since            text CHECK (qsl_since ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'),
    saved_at             timestamptz NOT NULL,
    CHECK ((certificate IS NULL) = (certificate_password IS NULL)),
    CHECK ((username IS NULL) = (password IS NULL))
);
COMMENT ON TABLE workspace.lotw_account IS
    'Each operator''s Logbook of the World certificate, login and station location.';

CREATE FUNCTION workspace.lotw_account_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    row_ workspace.lotw_account := CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
BEGIN
    IF NOT log.session_live(row_.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = row_.session_id) <> row_.operator_id THEN
        RAISE EXCEPTION 'a LoTW set-up is changed only by its own operator, signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    NEW.saved_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER lotw_account_check BEFORE INSERT OR UPDATE OR DELETE ON workspace.lotw_account
    FOR EACH ROW EXECUTE FUNCTION workspace.lotw_account_check();
