-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Online callbook lookups (QRZ.com): each operator's own login, and a cache of
-- what the callbook said.
--
-- * The lookup is an enhancement. With no login, or no internet, nothing here
--   is used and logging works exactly the same (Principle V).
-- * The cache is reference data, in the reference schema, apart from the log
--   (Principle VI). Every answer is kept, so the same call is never fetched
--   twice unless an operator asks for a fresh copy. Only the database's clock
--   stamps it (Principle VIII).
-- * A login belongs to one operator, who saves, changes, or removes it only
--   while signed in (Principle XIII). The Engine has to send the password to
--   the callbook, so it is kept as typed: anyone who can read this database
--   can read it. The API never returns it.

CREATE TABLE reference.callbook_entry (
    source     text NOT NULL CHECK (source IN ('qrz')),
    call       text NOT NULL CHECK (call ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(call) <= 20),
    found      boolean NOT NULL,
    entry      jsonb NOT NULL
               CHECK (jsonb_typeof(entry) = 'object' AND octet_length(entry::text) <= 8192),
    fetched_at timestamptz NOT NULL,
    PRIMARY KEY (source, call)
);
COMMENT ON TABLE reference.callbook_entry IS
    'What an online callbook said about a call, so it is fetched once (Principle VI).';

CREATE FUNCTION reference.callbook_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.fetched_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER callbook_stamp BEFORE INSERT OR UPDATE ON reference.callbook_entry
    FOR EACH ROW EXECUTE FUNCTION reference.callbook_stamp();

CREATE TABLE workspace.callbook_login (
    operator_id bigint NOT NULL REFERENCES log.operator,
    source      text NOT NULL CHECK (source IN ('qrz')),
    session_id  uuid NOT NULL REFERENCES log.session,
    username    text NOT NULL CHECK (length(username) BETWEEN 1 AND 64),
    password    text NOT NULL CHECK (length(password) BETWEEN 1 AND 128),
    -- The callbook's own session key, reused until it expires.
    service_key text CHECK (length(service_key) <= 128),
    saved_at    timestamptz NOT NULL,
    PRIMARY KEY (operator_id, source)
);

CREATE FUNCTION workspace.callbook_login_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    row_ workspace.callbook_login := CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
BEGIN
    IF NOT log.session_live(row_.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = row_.session_id) <> row_.operator_id THEN
        RAISE EXCEPTION 'a callbook login is changed only by its own operator, signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    NEW.saved_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER callbook_login_check BEFORE INSERT OR UPDATE OR DELETE ON workspace.callbook_login
    FOR EACH ROW EXECUTE FUNCTION workspace.callbook_login_check();
