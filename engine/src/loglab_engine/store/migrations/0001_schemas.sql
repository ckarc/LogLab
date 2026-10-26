-- SPDX-FileCopyrightText: 2026 LabLog contributors
-- SPDX-License-Identifier: AGPL-3.0-only

-- The log and the reference data live in separate schemas, so a reference
-- refresh can never touch, lock, or corrupt a QSO record (Principle VI).

CREATE SCHEMA log;
COMMENT ON SCHEMA log IS
    'The log: QSOs, every version of every QSO, and who changed what (Principle VII).';

CREATE SCHEMA reference;
COMMENT ON SCHEMA reference IS
    'Reference data (callbook, DXCC, sections). Kept apart from the log (Principle VI).';
