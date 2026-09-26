-- Lorebook additive tables (spec §4).
--
-- These are OURS. Calibre's own tables are read and written unmodified; these
-- are additional tables in the same database, which Calibre ignores because it
-- does not know them. Nothing here ALTERs a Calibre table, so a Calibre
-- install that has never seen this app still works, and a library this app has
-- extended is still a valid Calibre library.
--
-- Idempotent: every statement is IF NOT EXISTS, so this runs on every open.
--
-- Every table is STRICT, so SQLite rejects any column type outside
-- INT/INTEGER/REAL/TEXT/BLOB/ANY. That rules out TIMESTAMP and DATETIME, so
-- timestamps are TEXT — which is also what Calibre itself stores, so a
-- timestamp written by one is readable by the other.

PRAGMA foreign_keys = ON;

-- Schema/app bookkeeping. `key`/`value` rather than columns, so adding a
-- setting is not a migration.
CREATE TABLE IF NOT EXISTS app_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- Per-book-per-format file origin (spec §4 book_sources).
--
-- The content hash is the merge key, not the path: the same file reachable by
-- two scan roots is one book (§3.4). `kind` decides whether we may ever touch
-- the file; `state` is a visible state, not an error to hide.
CREATE TABLE IF NOT EXISTS book_sources (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    book          INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format        TEXT    NOT NULL COLLATE NOCASE,
    kind          TEXT    NOT NULL CHECK (kind IN ('managed','reference','symlink')),
    path          TEXT    NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    mtime_ns      INTEGER NOT NULL DEFAULT 0,
    content_hash  TEXT    NOT NULL DEFAULT '',
    state         TEXT    NOT NULL DEFAULT 'ok'
                          CHECK (state IN ('ok','missing','moved','conflict')),
    superseded_by INTEGER REFERENCES book_sources(id) ON DELETE SET NULL,
    confidence    REAL    NOT NULL DEFAULT 1.0
                          CHECK (confidence >= 0.0 AND confidence <= 1.0),
    UNIQUE (book, format)
) STRICT;

CREATE INDEX IF NOT EXISTS book_sources_hash_idx  ON book_sources (content_hash);
CREATE INDEX IF NOT EXISTS book_sources_state_idx ON book_sources (state);
CREATE INDEX IF NOT EXISTS book_sources_path_idx  ON book_sources (path);

-- Watched folders (spec §4 scan_roots).
CREATE TABLE IF NOT EXISTS scan_roots (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    path            TEXT    NOT NULL UNIQUE,
    recursive       INTEGER NOT NULL DEFAULT 1,
    include         TEXT    NOT NULL DEFAULT '*.epub;*.mobi;*.azw3;*.pdf;*.cbz;*.cbr',
    exclude         TEXT    NOT NULL DEFAULT '',
    follow_symlinks INTEGER NOT NULL DEFAULT 0,
    last_scan_at    TEXT
) STRICT;

-- Incremental scan bookkeeping: (path, size, mtime) is the cheap check, so a
-- rescan skips hashing a file that has not changed (§3.4).
CREATE TABLE IF NOT EXISTS scan_state (
    root        INTEGER NOT NULL REFERENCES scan_roots(id) ON DELETE CASCADE,
    path        TEXT    NOT NULL,
    size        INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (root, path)
) STRICT;

-- Per-file pipeline output with provenance (spec §4 curation_signals).
CREATE TABLE IF NOT EXISTS curation_signals (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    book       INTEGER REFERENCES books(id) ON DELETE CASCADE,
    stage      TEXT    NOT NULL,
    signal_type TEXT   NOT NULL,
    value      TEXT    NOT NULL,
    confidence REAL    NOT NULL DEFAULT 1.0,
    source     TEXT    NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE INDEX IF NOT EXISTS curation_signals_book_idx ON curation_signals (book, stage);

-- Items awaiting a user decision (spec §4 inbox_items).
CREATE TABLE IF NOT EXISTS inbox_items (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    book       INTEGER REFERENCES books(id) ON DELETE CASCADE,
    reason     TEXT    NOT NULL,
    suggestion TEXT    NOT NULL DEFAULT '',
    confidence REAL    NOT NULL DEFAULT 1.0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolved_at TEXT,
    resolution TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS inbox_items_unresolved_idx
    ON inbox_items (resolved_at) WHERE resolved_at IS NULL;

-- Anthology split records (spec §4 split_provenance).
CREATE TABLE IF NOT EXISTS split_provenance (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    original_book INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    split_book  INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    split_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (original_book, split_book)
) STRICT;

-- Configured instances for metadata exchange (spec §3.9).
CREATE TABLE IF NOT EXISTS instance_connections (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    url           TEXT    NOT NULL UNIQUE,
    token_ref     TEXT    NOT NULL DEFAULT '',
    enabled       INTEGER NOT NULL DEFAULT 0,
    auto_send     INTEGER NOT NULL DEFAULT 0,
    auto_receive  INTEGER NOT NULL DEFAULT 0,
    last_sync_at  TEXT
) STRICT;

-- Entities received from instances.
CREATE TABLE IF NOT EXISTS canonical_entities (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id   INTEGER NOT NULL REFERENCES instance_connections(id) ON DELETE CASCADE,
    entity_type   TEXT    NOT NULL,
    canonical_name TEXT   NOT NULL,
    aliases       TEXT    NOT NULL DEFAULT '[]',
    received_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    review_status TEXT    NOT NULL DEFAULT 'pending'
) STRICT;

-- book -> canonical entity links, per field, with an accept flag.
CREATE TABLE IF NOT EXISTS work_canonical_refs (
    book     INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE CASCADE,
    field    TEXT    NOT NULL,
    accepted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book, entity_id, field)
) STRICT;

-- Outbound exchange batches (spec §3.9).
CREATE TABLE IF NOT EXISTS signal_batches (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id INTEGER NOT NULL REFERENCES instance_connections(id) ON DELETE CASCADE,
    status     TEXT    NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    sent_at    TEXT,
    response   TEXT
) STRICT;

-- Reading progress, one row per book+format+device (spec §8).
CREATE TABLE IF NOT EXISTS reading_state (
    book     INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format   TEXT    NOT NULL COLLATE NOCASE,
    device   TEXT    NOT NULL,
    cfi      TEXT    NOT NULL DEFAULT '',
    pos_frac REAL    NOT NULL DEFAULT 0.0
                    CHECK (pos_frac >= 0.0 AND pos_frac <= 1.0),
    epoch    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (book, format, device)
) STRICT;

-- Composite-column cache. Without it, every sort on a composite column re-runs
-- the template program per row (spec §4).
CREATE TABLE IF NOT EXISTS composite_cache (
    book        INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    column_id   INTEGER NOT NULL,
    value       TEXT    NOT NULL DEFAULT '',
    computed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (book, column_id)
) STRICT;

-- Named template programs for composite columns (spec §5.2).
CREATE TABLE IF NOT EXISTS column_templates (
    name      TEXT PRIMARY KEY,
    program   TEXT NOT NULL,
    is_default INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Installed plugins and their state (spec §6).
CREATE TABLE IF NOT EXISTS plugins (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    version  TEXT NOT NULL,
    path     TEXT NOT NULL,
    enabled  INTEGER NOT NULL DEFAULT 0,
    settings TEXT NOT NULL DEFAULT '{}'
) STRICT;

-- The analytics decision, with a timestamp (spec §9).
CREATE TABLE IF NOT EXISTS privacy_consent (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    analytics_enabled INTEGER NOT NULL DEFAULT 0,
    decided_at       TEXT,
    decided_by       TEXT
) STRICT;

-- Local-first event log, drained only with consent (spec §9).
CREATE TABLE IF NOT EXISTS analytics_events (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    kind    TEXT    NOT NULL,
    book    INTEGER REFERENCES books(id) ON DELETE SET NULL,
    at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    payload TEXT    NOT NULL DEFAULT '{}'
) STRICT;

-- Automation chain log (spec §7).
CREATE TABLE IF NOT EXISTS actions (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    name   TEXT NOT NULL,
    book   INTEGER REFERENCES books(id) ON DELETE SET NULL,
    ran_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    result TEXT NOT NULL DEFAULT ''
) STRICT;

-- Record the schema version this app last wrote, so a future version can tell
-- whether an additive change has been applied.
INSERT OR IGNORE INTO app_meta (key, value) VALUES ('lorebook_schema_version', '1');
