-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- QSO storage: every QSO is an append-only series of complete ADIF snapshots.
--
-- * A QSO gets its unique ID when it is first entered (Principle XV).
-- * Every change is a new version tied to a changeset that records which
--   operator, at which station, and when; the previous version is kept
--   verbatim, so "what it was before" is always answerable (Principle VII).
-- * Deletion and recovery are versions too. Nothing here is ever erased.
-- * Every ADIF field is kept as written: name case, order, duplicates, data,
--   and type indicator (Principle IV).
-- * Every time is a timestamptz; connections use UTC (Principle VIII).
--
-- The rules are enforced here, by the database, so no code path can skip them.

-- Operators and stations are placeholders until the identity work
-- (Principle XIII). The audit columns that point at them are required now.
CREATE TABLE log.operator (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    callsign   text NOT NULL UNIQUE
               CHECK (callsign ~ '^[A-Z0-9]+(/[A-Z0-9]+)*$' AND length(callsign) <= 20),
    created_at timestamptz NOT NULL DEFAULT statement_timestamp()
);
COMMENT ON TABLE log.operator IS 'Placeholder operator identities, until operator sign-in.';

CREATE TABLE log.station (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name       text NOT NULL UNIQUE CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    created_at timestamptz NOT NULL DEFAULT statement_timestamp()
);
COMMENT ON TABLE log.station IS 'Placeholder station identities, until station registration.';

-- One audited action. Everything it changes commits in the same transaction
-- (Principle XIV); its time and transaction are set here, never by a caller.
CREATE TABLE log.changeset (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id bigint NOT NULL REFERENCES log.operator,
    station_id  bigint NOT NULL REFERENCES log.station,
    changed_at  timestamptz NOT NULL,
    txid        xid8 NOT NULL,
    kind        text NOT NULL CHECK (kind IN ('import', 'create', 'edit', 'delete', 'restore')),
    note        text CHECK (note IS NULL OR length(note) <= 1000)
);

-- A list of ADIF fields: a non-empty (or, for a header, possibly empty) JSON
-- array of [name, data, type indicator or null].
CREATE FUNCTION log.is_field_list(fields jsonb, allow_empty boolean)
RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
AS $$
DECLARE
    item jsonb;
BEGIN
    IF jsonb_typeof(fields) <> 'array' THEN
        RETURN false;
    END IF;
    IF jsonb_array_length(fields) = 0 THEN
        RETURN allow_empty;
    END IF;
    FOR item IN SELECT value FROM jsonb_array_elements(fields) LOOP
        IF jsonb_typeof(item) <> 'array' THEN
            RETURN false;
        END IF;
        IF jsonb_array_length(item) <> 3
            OR jsonb_typeof(item -> 0) <> 'string'
            OR (item ->> 0) = ''
            OR jsonb_typeof(item -> 1) <> 'string'
            OR jsonb_typeof(item -> 2) NOT IN ('string', 'null') THEN
            RETURN false;
        END IF;
    END LOOP;
    RETURN true;
END;
$$;

-- The data of the first field called "name", ignoring case as ADIF does.
CREATE FUNCTION log.field_value(fields jsonb, name text)
RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$
    SELECT item ->> 1
    FROM jsonb_array_elements(fields) WITH ORDINALITY AS f(item, position)
    WHERE upper(item ->> 0) = upper(name)
    ORDER BY position
    LIMIT 1
$$;

-- QSO_DATE (YYYYMMDD) and TIME_ON (HHMM or HHMMSS) as a UTC instant, or NULL
-- when either is missing or not a real date and time. ADIF dates and times are
-- UTC by definition (Principle VIII).
CREATE FUNCTION log.derived_qso_start(fields jsonb)
RETURNS timestamptz
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
AS $$
DECLARE
    d text := btrim(log.field_value(fields, 'QSO_DATE'));
    t text := btrim(log.field_value(fields, 'TIME_ON'));
BEGIN
    IF d IS NULL OR t IS NULL OR d !~ '^[0-9]{8}$' OR t !~ '^([0-9]{4}|[0-9]{6})$' THEN
        RETURN NULL;
    END IF;
    RETURN make_timestamp(
        substr(d, 1, 4)::integer, substr(d, 5, 2)::integer, substr(d, 7, 2)::integer,
        substr(t, 1, 2)::integer, substr(t, 3, 2)::integer,
        CASE WHEN length(t) = 6 THEN substr(t, 5, 2)::integer ELSE 0 END
    ) AT TIME ZONE 'UTC';
EXCEPTION
    WHEN data_exception THEN
        RETURN NULL;
END;
$$;

-- Each import keeps its file's identity and header, so the import can be
-- exported back exactly as it arrived (Principle IV).
CREATE TABLE log.import_batch (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    changeset_id bigint NOT NULL UNIQUE REFERENCES log.changeset,
    file_name    text NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
    file_sha256  text NOT NULL UNIQUE CHECK (file_sha256 ~ '^[0-9a-f]{64}$'),
    header       jsonb CHECK (
        header IS NULL OR (
            jsonb_typeof(header) = 'object'
            AND jsonb_typeof(header -> 'preamble') = 'string'
            AND (header ->> 'preamble') <> ''
            AND log.is_field_list(header -> 'fields', true)
            AND header - 'preamble' - 'fields' = '{}'::jsonb
        )
    ),
    record_count integer NOT NULL CHECK (record_count >= 0)
);

-- A QSO's identity. Its state is whatever its latest version says;
-- current_version and deleted are kept in step by the version trigger only.
CREATE TABLE log.qso (
    id              uuid PRIMARY KEY,
    entry_seq       bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    import_batch_id bigint REFERENCES log.import_batch,
    import_position integer CHECK (import_position >= 1),
    current_version integer NOT NULL DEFAULT 0,
    deleted         boolean NOT NULL DEFAULT false,
    CHECK ((import_batch_id IS NULL) = (import_position IS NULL)),
    UNIQUE (import_batch_id, import_position)
);

-- Every version of every QSO: a complete snapshot of its ADIF fields. The
-- columns after "fields" are derived from the snapshot by the database and
-- cannot be written directly. They exist for dupe checking.
CREATE TABLE log.qso_version (
    qso_id       uuid NOT NULL REFERENCES log.qso,
    version      integer NOT NULL CHECK (version >= 1),
    changeset_id bigint NOT NULL REFERENCES log.changeset,
    action       text NOT NULL CHECK (action IN ('create', 'edit', 'delete', 'restore')),
    deleted      boolean NOT NULL,
    fields       jsonb NOT NULL CHECK (log.is_field_list(fields, false)),
    call         text GENERATED ALWAYS AS
                     (nullif(upper(btrim(log.field_value(fields, 'CALL'))), '')) STORED,
    qso_start    timestamptz GENERATED ALWAYS AS (log.derived_qso_start(fields)) STORED,
    band         text GENERATED ALWAYS AS
                     (nullif(lower(btrim(log.field_value(fields, 'BAND'))), '')) STORED,
    mode         text GENERATED ALWAYS AS
                     (nullif(upper(btrim(log.field_value(fields, 'MODE'))), '')) STORED,
    PRIMARY KEY (qso_id, version)
);
CREATE INDEX qso_version_call ON log.qso_version (call);
CREATE INDEX qso_version_qso_start ON log.qso_version (qso_start);
CREATE INDEX qso_version_band_mode ON log.qso_version (band, mode);
CREATE INDEX qso_version_changeset ON log.qso_version (changeset_id);

-- ---------------------------------------------------------------------------
-- Enforcement
-- ---------------------------------------------------------------------------

-- Nothing in the log is ever erased or rewritten.
CREATE FUNCTION log.refuse_change() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'log.% is append-only: % is not allowed (Principle VII)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER operator_append_only BEFORE UPDATE OR DELETE ON log.operator
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER station_append_only BEFORE UPDATE OR DELETE ON log.station
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER changeset_append_only BEFORE UPDATE OR DELETE ON log.changeset
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER import_batch_append_only BEFORE UPDATE OR DELETE ON log.import_batch
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER qso_version_append_only BEFORE UPDATE OR DELETE ON log.qso_version
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER qso_no_delete BEFORE DELETE ON log.qso
    FOR EACH ROW EXECUTE FUNCTION log.refuse_change();

CREATE TRIGGER operator_no_truncate BEFORE TRUNCATE ON log.operator
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER station_no_truncate BEFORE TRUNCATE ON log.station
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER changeset_no_truncate BEFORE TRUNCATE ON log.changeset
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER import_batch_no_truncate BEFORE TRUNCATE ON log.import_batch
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER qso_no_truncate BEFORE TRUNCATE ON log.qso
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();
CREATE TRIGGER qso_version_no_truncate BEFORE TRUNCATE ON log.qso_version
    FOR EACH STATEMENT EXECUTE FUNCTION log.refuse_change();

-- A changeset's time and transaction are the database's, not the caller's.
CREATE FUNCTION log.changeset_stamp() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.changed_at := transaction_timestamp();
    NEW.txid := pg_current_xact_id();
    RETURN NEW;
END;
$$;

CREATE TRIGGER changeset_stamp BEFORE INSERT ON log.changeset
    FOR EACH ROW EXECUTE FUNCTION log.changeset_stamp();

-- An import batch belongs to an import changeset made in the same transaction.
CREATE FUNCTION log.import_batch_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    cs log.changeset;
BEGIN
    SELECT * INTO cs FROM log.changeset WHERE id = NEW.changeset_id;
    IF cs.kind IS DISTINCT FROM 'import' OR cs.txid IS DISTINCT FROM pg_current_xact_id() THEN
        RAISE EXCEPTION 'an import batch needs an import changeset from its own transaction'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER import_batch_check BEFORE INSERT ON log.import_batch
    FOR EACH ROW EXECUTE FUNCTION log.import_batch_check();

-- A new QSO starts with no versions; its first version follows in the same
-- transaction (checked at commit, below).
CREATE FUNCTION log.qso_insert_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.current_version <> 0 OR NEW.deleted THEN
        RAISE EXCEPTION 'a new QSO starts at version 0, not deleted; its versions set its state'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qso_insert_check BEFORE INSERT ON log.qso
    FOR EACH ROW EXECUTE FUNCTION log.qso_insert_check();

-- log.qso changes only from the version trigger, and only its state columns.
CREATE FUNCTION log.qso_update_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF pg_trigger_depth() < 2
        OR NEW.id <> OLD.id
        OR NEW.entry_seq <> OLD.entry_seq
        OR NEW.import_batch_id IS DISTINCT FROM OLD.import_batch_id
        OR NEW.import_position IS DISTINCT FROM OLD.import_position
        OR NEW.current_version <> OLD.current_version + 1 THEN
        RAISE EXCEPTION 'a QSO changes only by adding a version (Principle VII)'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qso_update_guard BEFORE UPDATE ON log.qso
    FOR EACH ROW EXECUTE FUNCTION log.qso_update_guard();

-- Every QSO has at least one version once its transaction commits.
CREATE FUNCTION log.qso_has_version() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (SELECT current_version FROM log.qso WHERE id = NEW.id) = 0 THEN
        RAISE EXCEPTION 'QSO % was entered without a version', NEW.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER qso_has_version AFTER INSERT ON log.qso
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION log.qso_has_version();

-- The audited path: each new version must follow the one before it, belong to
-- a changeset of the matching kind made in this transaction, and make sense
-- for the QSO's state. Then it becomes the QSO's current state.
CREATE FUNCTION log.qso_version_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    q log.qso;
    cs log.changeset;
    previous jsonb;
BEGIN
    SELECT * INTO q FROM log.qso WHERE id = NEW.qso_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'QSO % does not exist', NEW.qso_id USING ERRCODE = 'foreign_key_violation';
    END IF;
    IF NEW.version <> q.current_version + 1 THEN
        RAISE EXCEPTION 'QSO % is at version %; the next version is %, not %',
            NEW.qso_id, q.current_version, q.current_version + 1, NEW.version
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT * INTO cs FROM log.changeset WHERE id = NEW.changeset_id;
    IF cs.txid IS DISTINCT FROM pg_current_xact_id() THEN
        RAISE EXCEPTION 'a QSO version needs a changeset from its own transaction'
            USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.version > 1 THEN
        SELECT fields INTO previous FROM log.qso_version
        WHERE qso_id = NEW.qso_id AND version = q.current_version;
    END IF;

    IF NEW.action = 'create' THEN
        IF NEW.version <> 1 OR NEW.deleted OR cs.kind NOT IN ('import', 'create') THEN
            RAISE EXCEPTION 'a create is version 1 of a QSO, not deleted, in a create or import'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.action = 'edit' THEN
        IF q.deleted OR NEW.deleted OR cs.kind <> 'edit' THEN
            RAISE EXCEPTION 'only a QSO that is not deleted can be edited, in an edit'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.fields = previous THEN
            RAISE EXCEPTION 'an edit must change something' USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.action = 'delete' THEN
        IF q.deleted OR NOT NEW.deleted OR cs.kind <> 'delete' OR NEW.fields <> previous THEN
            RAISE EXCEPTION 'a delete marks a QSO that is not deleted, keeping its fields'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.action = 'restore' THEN
        IF NOT q.deleted OR NEW.deleted OR cs.kind <> 'restore' OR NEW.fields <> previous THEN
            RAISE EXCEPTION 'a restore unmarks a deleted QSO, keeping its fields'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;

    UPDATE log.qso SET current_version = NEW.version, deleted = NEW.deleted
    WHERE id = NEW.qso_id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER qso_version_check BEFORE INSERT ON log.qso_version
    FOR EACH ROW EXECUTE FUNCTION log.qso_version_check();
