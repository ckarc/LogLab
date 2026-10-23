-- SPDX-FileCopyrightText: 2026 LogLab contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- Logbook of the World batches: nothing goes to LoTW live.
--
-- * QSOs go to LoTW only in a batch the operator has looked over. Preparing a
--   batch fixes exactly which QSOs, at which version, it holds; nothing is
--   sent until the operator releases it (Principle XIII: a deliberate act).
-- * An operator has at most one batch waiting for review at a time.
-- * When a batch is released, only its QSOs still exactly as they were when
--   it was prepared are sent; one edited since stays behind for a later batch.
-- * LoTW numbers neither uploads nor the QSOs in them, so the batch number is
--   LabLog's own record of what went out together, when, and what LoTW said.
-- * Batches are kept: a batch is never deleted, and once sent or cancelled it
--   never changes. Only its own operator, signed in, prepares, releases or
--   cancels it.

CREATE TABLE workspace.lotw_batch (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operator_id  bigint NOT NULL REFERENCES log.operator,
    session_id   uuid NOT NULL REFERENCES log.session,
    station_call text NOT NULL CHECK (station_call ~ '^[A-Z0-9/]{3,20}$'),
    state        text NOT NULL DEFAULT 'ready' CHECK (state IN ('ready', 'sent', 'cancelled')),
    prepared_at  timestamptz NOT NULL DEFAULT transaction_timestamp(),
    finished_at  timestamptz,
    answer       text CHECK (length(answer) <= 1000),
    CHECK ((state = 'ready') = (finished_at IS NULL))
);
CREATE UNIQUE INDEX lotw_batch_one_ready ON workspace.lotw_batch (operator_id)
    WHERE state = 'ready';
COMMENT ON TABLE workspace.lotw_batch IS
    'Batches of QSOs an operator prepared, reviewed and sent to Logbook of the World.';

CREATE TABLE workspace.lotw_batch_qso (
    batch_id bigint NOT NULL REFERENCES workspace.lotw_batch,
    qso_id   uuid NOT NULL,
    version  integer NOT NULL,
    sent     boolean NOT NULL DEFAULT false,
    PRIMARY KEY (batch_id, qso_id),
    FOREIGN KEY (qso_id, version) REFERENCES log.qso_version
);
CREATE INDEX lotw_batch_qso_qso ON workspace.lotw_batch_qso (qso_id);
COMMENT ON TABLE workspace.lotw_batch_qso IS
    'The QSOs, at the version reviewed, in each LoTW batch, and whether each was sent.';

CREATE FUNCTION workspace.lotw_batch_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'LoTW batches are kept; cancel one instead'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NOT log.session_live(NEW.session_id)
        OR (SELECT operator_id FROM log.session WHERE id = NEW.session_id) <> NEW.operator_id THEN
        RAISE EXCEPTION 'a LoTW batch is handled only by its own operator, signed in (Principle XIII)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF TG_OP = 'UPDATE' AND (
        OLD.state <> 'ready'
        OR NEW.operator_id <> OLD.operator_id
        OR NEW.station_call <> OLD.station_call
        OR NEW.prepared_at <> OLD.prepared_at
    ) THEN
        RAISE EXCEPTION 'LoTW batch % is finished and can''t change', OLD.id
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    IF NEW.state <> 'ready' THEN
        NEW.finished_at := transaction_timestamp();
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER lotw_batch_check BEFORE INSERT OR UPDATE OR DELETE ON workspace.lotw_batch
    FOR EACH ROW EXECUTE FUNCTION workspace.lotw_batch_check();

-- A batch's QSOs are added while it is ready, marked sent as it is released,
-- and never changed or removed after that.
CREATE FUNCTION workspace.lotw_batch_qso_check() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'the QSOs in a LoTW batch are kept'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    IF (SELECT state FROM workspace.lotw_batch WHERE id = NEW.batch_id) <> 'ready'
        OR (TG_OP = 'UPDATE' AND (
            NEW.batch_id <> OLD.batch_id OR NEW.qso_id <> OLD.qso_id
            OR NEW.version <> OLD.version OR OLD.sent
        )) THEN
        RAISE EXCEPTION 'the QSOs in a LoTW batch change only while it waits for review'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER lotw_batch_qso_check BEFORE INSERT OR UPDATE OR DELETE ON workspace.lotw_batch_qso
    FOR EACH ROW EXECUTE FUNCTION workspace.lotw_batch_qso_check();
