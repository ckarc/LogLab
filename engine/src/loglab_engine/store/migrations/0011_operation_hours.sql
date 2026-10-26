-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- An operation's hours: when the contest starts and ends, in UTC
-- (Principle VIII). A QSO made outside them is warned about, never refused,
-- and once the end has passed the operation takes no QSO made after it.
-- Either may be left open.

ALTER TABLE workspace.operation
    ADD COLUMN starts_at timestamptz,
    ADD COLUMN ends_at   timestamptz,
    ADD CONSTRAINT operation_hours CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at > starts_at);
