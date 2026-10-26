-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Operator identity and sign-in (Principle XIII).
--
-- * Operators are local accounts held by the Engine. Signing in needs no
--   internet connection and no outside identity provider (Principle V).
-- * An operator's secret (a PIN or passphrase), permissions, and whether the
--   account is enabled are append-only histories: the latest row is the
--   current state, and every earlier one is kept, with who changed it and
--   when (Principle VII).
-- * Every change to the log is made in a sign-in session. The database checks
--   that the session is live and that its operator may log, so no code path
--   can record a change for an operator who is not signed in.
-- * Only the database's clock stamps these rows (Principle VIII).
--
-- Transmit authorization is granted per operator and per radio, so it arrives
-- with the Station Agent's radios; nothing here can grant it.

-- What an operator may do. Each is granted on its own; none implies another.
CREATE FUNCTION log.is_permission(permission text)
RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$
    SELECT permission IN ('view', 'log', 'radio', 'admin')
$$;

-- An operator's secret, stored only as a salted scrypt hash.
CREATE TABLE log.operator_secret (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id bigint NOT NULL REFERENCES log.operator,
    secret_hash text NOT NULL
                CHECK (secret_hash ~ '^scrypt\$[0-9]+\$[0-9]+\$[0-9]+\$[A-Za-z0-9+/=]+\$[A-Za-z0-9+/=]+$'),
    changed_by  bigint REFERENCES log.operator,
    session_id  uuid,
    changed_at  timestamptz NOT NULL
);
CREATE INDEX operator_secret_latest ON log.operator_secret (operator_id, id DESC);

CREATE TABLE log.operator_permission (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id bigint NOT NULL REFERENCES log.operator,
    permission  text NOT NULL CHECK (log.is_permission(permission)),
    granted     boolean NOT NULL,
    changed_by  bigint REFERENCES log.operator,
    session_id  uuid,
    changed_at  timestamptz NOT NULL
);
CREATE INDEX operator_permission_latest
    ON log.operator_permission (operator_id, permission, id DESC);

CREATE TABLE log.operator_status (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id bigint NOT NULL REFERENCES log.operator,
    enabled     boolean NOT NULL,
    changed_by  bigint NOT NULL REFERENCES log.operator,
    session_id  uuid,
    changed_at  timestamptz NOT NULL
);
CREATE INDEX operator_status_latest ON log.operator_status (operator_id, id DESC);

-- A sign-in at a station. The client holds a random token; only its SHA-256
-- is stored, so a copy of the database cannot be used to sign in.
CREATE TABLE log.session (
    id           uuid PRIMARY KEY,
    token_sha256 text NOT NULL UNIQUE CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    operator_id  bigint NOT NULL REFERENCES log.operator,
    station_id   bigint NOT NULL REFERENCES log.station,
    started_at   timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL,
    CHECK (expires_at > started_at)
);

CREATE TABLE log.session_end (
    session_id uuid PRIMARY KEY REFERENCES log.session,
    ended_at   timestamptz NOT NULL,
    reason     text NOT NULL CHECK (reason IN ('signed out', 'revoked'))
);

-- Every sign-in attempt, kept for the record and to slow down guessing.
CREATE TABLE log.sign_in_attempt (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    callsign     text NOT NULL CHECK (length(callsign) BETWEEN 1 AND 20),
    station      text NOT NULL CHECK (length(station) BETWEEN 1 AND 64),
    succeeded    boolean NOT NULL,
    attempted_at timestamptz NOT NULL
);
CREATE INDEX sign_in_attempt_recent ON log.sign_in_attempt (callsign, attempted_at);

-- Account changes name the session they were made in, like changes to the log.
ALTER TABLE log.operator_secret
    ADD FOREIGN KEY (session_id) REFERENCES log.session,
    ADD CHECK ((changed_by IS NULL) = (session_id IS NULL));
ALTER TABLE log.operator_permission
    ADD FOREIGN KEY (session_id) REFERENCES log.session,
    ADD CHECK ((changed_by IS NULL) = (session_id IS NULL));
ALTER TABLE log.operator_status
    ADD FOREIGN KEY (session_id) REFERENCES log.session,
    ADD CHECK (session_id IS NOT NULL);

-- Every change to the log now names the session it was made in. Changesets
-- recorded before sign-in existed keep a NULL here; new ones must have one.
ALTER TABLE log.changeset ADD COLUMN session_id uuid REFERENCES log.session;

-- ---------------------------------------------------------------------------
-- Current state
-- ---------------------------------------------------------------------------

CREATE FUNCTION log.operator_enabled(op_id bigint)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT coalesce(
        (SELECT enabled FROM log.operator_status
         WHERE operator_id = op_id ORDER BY id DESC LIMIT 1),
        true
    )
$$;

-- Whether an enabled operator holds a permission right now.
CREATE FUNCTION log.operator_may(op_id bigint, wanted text)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT log.operator_enabled(op_id) AND coalesce(
        (SELECT p.granted FROM log.operator_permission p
         WHERE p.operator_id = op_id AND p.permission = wanted
         ORDER BY p.id DESC LIMIT 1),
        false
    )
$$;

CREATE FUNCTION log.any_admin()
RETURNS boolean
LANGUAGE sql STABLE PARALLEL SAFE
AS $$
    SELECT EXISTS (SELECT 1 FROM log.operator o WHERE log.operator_may(o.id, 'admin'))
$$;

-- A session is live until it ends, expires, or its operator is disabled.
CREATE FUNCTION log.session_live(sid uuid)
RETURNS boolean
LANGUAGE sql STABLE STRICT PARALLEL SAFE
AS $$
    SELECT EXISTS (
        SELECT 1 FROM log.session s
        WHERE s.id = sid
          AND s.expires_at > statement_timestamp()
          AND log.operator_enabled(s.operator_id)
          AND NOT EXISTS (SELECT 1 FROM log.session_end e WHERE e.session_id = s.id)
    )
$$;

-- ---------------------------------------------------------------------------
-- Enforcement
-- ---------------------------------------------------------------------------

CREATE TRIGGER operator_secret_append_only BEFORE UPDATE OR DELETE ON log.operator_secret
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER operator_permission_append_only BEFORE UPDATE OR DELETE ON log.operator_permission
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER operator_status_append_only BEFORE UPDATE OR DELETE ON log.operator_status
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER session_append_only BEFORE UPDATE OR DELETE ON log.session
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER session_end_append_only BEFORE UPDATE OR DELETE ON log.session_end
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER sign_in_attempt_append_only BEFORE UPDATE OR DELETE ON log.sign_in_attempt
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();

CREATE TRIGGER operator_secret_no_truncate BEFORE TRUNCATE ON log.operator_secret
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER operator_permission_no_truncate BEFORE TRUNCATE ON log.operator_permission
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER operator_status_no_truncate BEFORE TRUNCATE ON log.operator_status
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER session_no_truncate BEFORE TRUNCATE ON log.session
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER session_end_no_truncate BEFORE TRUNCATE ON log.session_end
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER sign_in_attempt_no_truncate BEFORE TRUNCATE ON log.sign_in_attempt
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- Account changes are made in a live session by an operator who holds admin.
-- The one exception is the very first administrator, enrolled while no
-- administrator exists: that row names nobody, because nobody exists yet. Changing your own
-- secret needs no admin. Disabling or demoting yourself is refused, so the
-- last administrator cannot lock everyone out.
CREATE FUNCTION log.account_change_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    own_secret boolean := TG_TABLE_NAME = 'operator_secret'
                          AND NEW.changed_by IS NOT DISTINCT FROM NEW.operator_id;
BEGIN
    NEW.changed_at := transaction_timestamp();
    IF NEW.changed_by IS NULL THEN
        IF log.any_admin() THEN
            RAISE EXCEPTION 'an account change must name the administrator who made it'
                USING ERRCODE = 'insufficient_privilege';
        END IF;
    ELSIF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.changed_by THEN
        RAISE EXCEPTION 'an account change needs its maker to be signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    ELSIF NOT own_secret AND NOT log.operator_may(NEW.changed_by, 'admin') THEN
        RAISE EXCEPTION 'only an administrator can change operator accounts (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NEW.changed_by IS NOT DISTINCT FROM NEW.operator_id THEN
        -- Nested so each table's own columns are only read for that table.
        IF TG_TABLE_NAME = 'operator_status' THEN
            IF NOT NEW.enabled THEN
                RAISE EXCEPTION 'an administrator cannot disable their own account'
                    USING ERRCODE = 'check_violation';
            END IF;
        ELSIF TG_TABLE_NAME = 'operator_permission' THEN
            IF NEW.permission = 'admin' AND NOT NEW.granted THEN
                RAISE EXCEPTION 'an administrator cannot remove their own admin permission'
                    USING ERRCODE = 'check_violation';
            END IF;
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER operator_secret_check BEFORE INSERT ON log.operator_secret
    FOR EACH ROW EXECUTE FUNCTION log.account_change_check();
CREATE TRIGGER operator_permission_check BEFORE INSERT ON log.operator_permission
    FOR EACH ROW EXECUTE FUNCTION log.account_change_check();
CREATE TRIGGER operator_status_check BEFORE INSERT ON log.operator_status
    FOR EACH ROW EXECUTE FUNCTION log.account_change_check();

-- Sessions, their ends, and sign-in attempts are stamped by the database. A
-- session's expiry is given as an offset from the database's clock too.
CREATE FUNCTION log.session_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT log.operator_enabled(NEW.operator_id) THEN
        RAISE EXCEPTION 'a disabled operator cannot sign in' USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM log.operator_secret WHERE operator_id = NEW.operator_id) THEN
        RAISE EXCEPTION 'an operator without a secret cannot sign in'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    NEW.started_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER session_stamp BEFORE INSERT ON log.session
    FOR EACH ROW EXECUTE FUNCTION log.session_stamp();

CREATE FUNCTION log.session_end_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.ended_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER session_end_stamp BEFORE INSERT ON log.session_end
    FOR EACH ROW EXECUTE FUNCTION log.session_end_stamp();

CREATE FUNCTION log.sign_in_attempt_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.attempted_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER sign_in_attempt_stamp BEFORE INSERT ON log.sign_in_attempt
    FOR EACH ROW EXECUTE FUNCTION log.sign_in_attempt_stamp();

-- A change to the log is made in a live session, by that session's operator
-- at that session's station, and that operator may log right now.
CREATE FUNCTION log.changeset_session_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    s log.session;
BEGIN
    SELECT * INTO s FROM log.session WHERE id = NEW.session_id;
    IF NOT FOUND OR NOT log.session_live(NEW.session_id) THEN
        RAISE EXCEPTION 'a change to the log needs a signed-in operator (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF s.operator_id <> NEW.operator_id OR s.station_id <> NEW.station_id THEN
        RAISE EXCEPTION 'a changeset must name its session''s operator and station'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT log.operator_may(NEW.operator_id, 'log') THEN
        RAISE EXCEPTION 'this operator does not have permission to log (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER changeset_session_check BEFORE INSERT ON log.changeset
    FOR EACH ROW EXECUTE FUNCTION log.changeset_session_check();
