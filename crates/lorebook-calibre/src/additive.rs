//! The additive, namespaced tables (spec §4).
//!
//! Everything here is ours. Calibre does not know these tables and ignores
//! them, which is what lets a library this app has extended remain a valid
//! Calibre library — and a Calibre library that has never seen this app keep
//! working. Nothing in this module `ALTER`s a Calibre table.

use crate::error::Result;
use rusqlite::Connection;

/// The additive DDL. Idempotent: every statement is `IF NOT EXISTS`.
pub const DDL: &str = include_str!("additive.sql");

/// Create the additive tables if they are absent. Safe to run on every open.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)
        .map_err(|e| crate::error::CalibreError::Sql(format!("apply additive schema: {e}")))
}

/// Whether every additive table is present, i.e. whether [`apply`] has run.
pub fn is_applied(conn: &Connection) -> bool {
    const EXPECTED: &[&str] = &[
        "app_meta",
        "book_sources",
        "scan_roots",
        "scan_state",
        "curation_signals",
        "inbox_items",
        "split_provenance",
        "instance_connections",
        "canonical_entities",
        "work_canonical_refs",
        "signal_batches",
        "reading_state",
        "composite_cache",
        "column_templates",
        "plugins",
        "privacy_consent",
        "analytics_events",
        "actions",
    ];
    let Ok(found) = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN (         'app_meta','book_sources','scan_roots','scan_state','curation_signals',         'inbox_items','split_provenance','instance_connections','canonical_entities',         'work_canonical_refs','signal_batches','reading_state','composite_cache',         'column_templates','plugins','privacy_consent','analytics_events','actions')",
        [],
        |r| r.get::<_, i64>(0),
    ) else {
        return false;
    };
    found as usize == EXPECTED.len()
}
