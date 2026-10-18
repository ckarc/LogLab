-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Sending QSOs to QRZ.com logbooks with their API keys.
--
-- * It is an enhancement. The log never waits on it, and with no internet the
--   QSOs simply wait here and go out when QRZ.com can be reached (Principle V).
-- * A key is saved by an operator, signed in, for one logbook callsign; only
--   that operator can change or remove it (Principle XIII). Each callsign has
--   one key, so no QSO is ever sent twice to the same logbook.
-- * Only QSOs entered after the key is saved are sent, so saving a key never
--   floods a QRZ logbook with a log imported earlier.
-- * What was sent is kept apart from the log: nothing here can touch a QSO.

CREATE TABLE workspace.qrz_logbook (
    operator_id    bigint PRIMARY KEY REFERENCES log.operator,
    session_id     uuid NOT NULL REFERENCES log.session,
    api_key        text NOT NULL CHECK (api_key ~ '^[A-Za-z0-9-]{8,64}$'),
    book_call      text NOT NULL UNIQUE
                   CHECK (book_call ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(book_call) <= 20),
    from_entry_seq bigint NOT NULL DEFAULT 0,
    saved_at       timestamptz NOT NULL
);
COMMENT ON TABLE workspace.qrz_logbook IS
    'QRZ.com logbook API keys, one per logbook callsign; QSOs entered later go to that logbook.';

CREATE FUNCTION workspace.qrz_logbook_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    row_ workspace.qrz_logbook := CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
BEGIN
    IF NOT log.session_live(row_.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = row_.session_id) <> row_.operator_id THEN
        RAISE EXCEPTION 'a QRZ logbook key is changed only by its own operator, signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    NEW.saved_at := transaction_timestamp();
    IF TG_OP = 'INSERT' OR NEW.book_call <> OLD.book_call THEN
        NEW.from_entry_seq := coalesce((SELECT max(entry_seq) FROM log.qso), 0);
    ELSE
        NEW.from_entry_seq := OLD.from_entry_seq;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qrz_logbook_check BEFORE INSERT OR UPDATE OR DELETE ON workspace.qrz_logbook
    FOR EACH ROW EXECUTE FUNCTION workspace.qrz_logbook_check();

-- What QRZ.com last did with each QSO sent to it: the version it reflects,
-- and the QSO's id in the QRZ logbook while it is there.
CREATE TABLE workspace.qrz_upload (
    qso_id    uuid PRIMARY KEY REFERENCES log.qso,
    book_call text NOT NULL,
    version   integer NOT NULL CHECK (version >= 1),
    logid     bigint CHECK (logid >= 0),
    outcome   text NOT NULL CHECK (outcome IN ('sent', 'deleted', 'refused')),
    message   text CHECK (length(message) <= 300),
    done_at   timestamptz NOT NULL
);
CREATE INDEX qrz_upload_book ON workspace.qrz_upload (book_call, outcome);

CREATE FUNCTION workspace.qrz_upload_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.done_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER qrz_upload_stamp BEFORE INSERT OR UPDATE ON workspace.qrz_upload
    FOR EACH ROW EXECUTE FUNCTION workspace.qrz_upload_stamp();
