-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- When the Engine may re-check, in the background, the QRZ.com profiles whose
-- biography or photo it keeps.
--
-- * One row for the whole station. 'auto' re-checks on any internet except
--   Starlink (a metered link in the field); 'always' re-checks on any
--   internet; 'off' never does. Lookups an operator makes are not affected.
-- * Only an operator who holds admin, signed in, changes it (Principle XIII).
-- * With no row, the Engine behaves as 'auto'. With no internet nothing is
--   tried at all (Principle V). Only the database's clock stamps it
--   (Principle VIII).

CREATE TABLE workspace.callbook_refresh (
    one         boolean PRIMARY KEY DEFAULT true CHECK (one),
    mode        text NOT NULL CHECK (mode IN ('auto', 'always', 'off')),
    operator_id bigint NOT NULL REFERENCES log.operator,
    session_id  uuid NOT NULL REFERENCES log.session,
    changed_at  timestamptz NOT NULL
);
COMMENT ON TABLE workspace.callbook_refresh IS
    'When kept QRZ.com profiles are re-checked in the background (admin setting).';

CREATE FUNCTION workspace.callbook_refresh_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.operator_id
        OR NOT log.operator_may(NEW.operator_id, 'admin') THEN
        RAISE EXCEPTION 'only an administrator, signed in, changes when profiles are re-checked (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.changed_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER callbook_refresh_check BEFORE INSERT OR UPDATE ON workspace.callbook_refresh
    FOR EACH ROW EXECUTE FUNCTION workspace.callbook_refresh_check();
