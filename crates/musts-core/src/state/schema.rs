//! SQL schema and migrations per `docs/PLAN.md` §4.7.
//!
//! All tables are created by migration v1 so later phases can write to
//! them without a schema bump. Phase 1 only reads/writes the first three.

/// Current schema version. Bump and add a new migration when changing the
/// shape of any existing table.
pub const CURRENT_VERSION: i32 = 1;

/// Statements applied by `apply_migrations` when the existing
/// `schema_version` is below 1.
pub const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS manifest_index (
    manifest_path TEXT PRIMARY KEY,
    scope_path    TEXT NOT NULL,
    mtime_ns      INTEGER NOT NULL,
    size_bytes    INTEGER NOT NULL,
    content_hash  TEXT NOT NULL,
    last_seen_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS file_fingerprints (
    rel_path     TEXT PRIMARY KEY,
    mtime_ns     INTEGER NOT NULL,
    size_bytes   INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    last_seen_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS scope_snapshots (
    scope_path  TEXT PRIMARY KEY,
    scope_hash  TEXT NOT NULL,
    computed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    task_id             TEXT PRIMARY KEY,
    capability          TEXT NOT NULL,
    title               TEXT NOT NULL,
    satisfies_json      TEXT NOT NULL,
    scope_hashes        TEXT NOT NULL,
    task_snapshot_hash  TEXT NOT NULL,
    payload_json        TEXT NOT NULL,
    created_at          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS resolve_notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    capability  TEXT NOT NULL,
    note        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS evidence_records (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id         TEXT NOT NULL,
    submission_id   TEXT NOT NULL,
    check_id        TEXT NOT NULL,
    scope_hash      TEXT NOT NULL,
    accepted        INTEGER NOT NULL,
    summary         TEXT,
    submission_json TEXT NOT NULL,
    result_json     TEXT NOT NULL,
    submitted_at    INTEGER NOT NULL,
    UNIQUE(task_id, submission_id, check_id)
);

CREATE INDEX IF NOT EXISTS evidence_records_check_idx
    ON evidence_records(check_id, scope_hash, accepted);
"#;
