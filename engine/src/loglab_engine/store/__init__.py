# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""QSO storage in PostgreSQL: schema migrations, operator sign-in, and the audited log."""

from loglab_engine.store.db import connect, default_database
from loglab_engine.store.identity import (
    IdentityError,
    IdentityStore,
    NotSignedInError,
    OperatorAccount,
    Permission,
    Session,
    SignInError,
)
from loglab_engine.store.log import (
    Action,
    Actor,
    DuplicateImportError,
    ImportBatch,
    ImportSummary,
    LogStore,
    NotAuthorizedError,
    QsoNotFoundError,
    QsoVersion,
    StaleVersionError,
    StoreError,
)
from loglab_engine.store.migrate import (
    AppliedMigration,
    Migration,
    MigrationError,
    applied_migrations,
    load_migrations,
    migrate,
    pending_migrations,
)

__all__ = [
    "Action",
    "Actor",
    "AppliedMigration",
    "DuplicateImportError",
    "IdentityError",
    "IdentityStore",
    "ImportBatch",
    "ImportSummary",
    "LogStore",
    "Migration",
    "MigrationError",
    "NotAuthorizedError",
    "NotSignedInError",
    "OperatorAccount",
    "Permission",
    "QsoNotFoundError",
    "QsoVersion",
    "Session",
    "SignInError",
    "StaleVersionError",
    "StoreError",
    "applied_migrations",
    "connect",
    "default_database",
    "load_migrations",
    "migrate",
    "pending_migrations",
]
