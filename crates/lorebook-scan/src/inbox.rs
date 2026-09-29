use crate::ScanError;
use rusqlite::{params, Connection};

/// Propose an item to the inbox for user resolution.
///
/// Returns the inserted row's `id`.
///
/// # Errors
/// - `confidence == 1.0` is rejected as a caller mistake: the schema's DEFAULT
///   1.0 means "user asserted this", and spec §3.6 says confidence >= 0.7
///   auto-accepts to library. Writing 1.0 from a machine decision silently
///   bypasses the inbox — this function makes that a tested error, not a comment.
/// - Propagates SQLite errors.
pub fn propose_inbox_item(
    conn: &Connection,
    book: i64,
    reason: &str,
    suggestion: &str,
    confidence: f64,
) -> Result<i64, ScanError> {
    if (confidence - 1.0).abs() < f64::EPSILON {
        return Err(ScanError::Sql(
            "confidence == 1.0 is reserved for user assertions; machine decisions must use < 1.0"
                .to_string(),
        ));
    }
    conn.execute(
        "INSERT INTO inbox_items (book, reason, suggestion, confidence)
         VALUES (?1, ?2, ?3, ?4)",
        params![book, reason, suggestion, confidence],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;
    Ok(conn.last_insert_rowid())
}

/// Resolve an inbox item in a single transaction.
///
/// Writes `resolved_at` and `resolution` together or neither. A half-resolved
/// item (one column set, the other NULL) would appear unanswered to the user
/// while already being "done" — this function prevents that.
///
/// Returns `true` if a row was updated, `false` if `id` did not exist.
pub fn resolve_inbox_item(
    conn: &mut Connection,
    id: i64,
    resolution: &str,
) -> Result<bool, ScanError> {
    let tx = conn
        .transaction()
        .map_err(|e| ScanError::Sql(e.to_string()))?;
    let rows = tx.execute(
        "UPDATE inbox_items
         SET resolved_at = CURRENT_TIMESTAMP, resolution = ?2
         WHERE id = ?1 AND resolved_at IS NULL",
        params![id, resolution],
    )?;
    tx.commit()?;
    Ok(rows > 0)
}

/// An unresolved inbox item.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxItem {
    pub id: i64,
    pub book: i64,
    pub reason: String,
    pub suggestion: String,
    pub confidence: f64,
    pub created_at: String,
}

/// List all unresolved inbox items.
///
/// Uses the partial index `inbox_items_unresolved_idx ON inbox_items
/// (resolved_at) WHERE resolved_at IS NULL` defined in additive.sql.
/// Resolved items are excluded by the `WHERE resolved_at IS NULL` predicate.
pub fn list_unresolved(conn: &Connection) -> Result<Vec<InboxItem>, ScanError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, book, reason, suggestion, confidence, created_at
             FROM inbox_items
             WHERE resolved_at IS NULL
             ORDER BY created_at",
        )
        .map_err(|e| ScanError::Sql(e.to_string()))?;
    let items = stmt
        .query_map([], |row| {
            Ok(InboxItem {
                id: row.get(0)?,
                book: row.get(1)?,
                reason: row.get(2)?,
                suggestion: row.get(3)?,
                confidence: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| ScanError::Sql(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ScanError::Sql(e.to_string()))?;
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lorebook_calibre::{create_library, migrate};
    use tempfile::tempdir;

    fn open_test_db() -> Connection {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.db");
        let conn = create_library(&path).expect("create_library");
        migrate(&conn).expect("migrate");
        conn
    }

    #[test]
    fn propose_rejects_confidence_1_0() {
        let conn = open_test_db();
        // Insert a book first (FK requirement)
        conn.execute("INSERT INTO books (title) VALUES ('Test Book')", [])
            .expect("insert book");
        let book_id = conn.last_insert_rowid();

        // confidence == 1.0 must be rejected
        let err = propose_inbox_item(&conn, book_id, "duplicate", "merge", 1.0).unwrap_err();
        assert!(
            err.to_string().contains("confidence == 1.0"),
            "error should mention confidence 1.0 rejection, got: {err}"
        );

        // confidence < 1.0 works
        let id = propose_inbox_item(&conn, book_id, "duplicate", "merge", 0.9).expect("propose");
        assert!(id > 0);
    }

    #[test]
    fn propose_then_list_then_resolve_then_list_empty() {
        let mut conn = open_test_db();
        conn.execute("INSERT INTO books (title) VALUES ('Test Book')", [])
            .expect("insert book");
        let book_id = conn.last_insert_rowid();

        // propose
        let id = propose_inbox_item(&conn, book_id, "duplicate", "merge", 0.6).expect("propose");

        // list unresolved - should contain our item
        let items = list_unresolved(&conn).expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id);
        assert_eq!(items[0].book, book_id);
        assert_eq!(items[0].reason, "duplicate");
        assert_eq!(items[0].suggestion, "merge");
        assert!((items[0].confidence - 0.6).abs() < f64::EPSILON);

        // resolve
        let updated = resolve_inbox_item(&mut conn, id, "accepted").expect("resolve");
        assert!(updated, "resolve should report row updated");

        // list again - should be empty (uses partial index on resolved_at IS NULL)
        let items = list_unresolved(&conn).expect("list after resolve");
        assert_eq!(
            items.len(),
            0,
            "resolved item must not appear in unresolved list"
        );
    }

    #[test]
    fn resolve_nonexistent_returns_false() {
        let mut conn = open_test_db();
        let updated = resolve_inbox_item(&mut conn, 99999, "rejected").expect("resolve");
        assert!(!updated, "resolve on nonexistent id returns false");
    }

    #[test]
    fn resolve_idempotent_second_call_returns_false() {
        let mut conn = open_test_db();
        conn.execute("INSERT INTO books (title) VALUES ('Test Book')", [])
            .expect("insert book");
        let book_id = conn.last_insert_rowid();
        let id = propose_inbox_item(&conn, book_id, "dup", "merge", 0.5).expect("propose");
        resolve_inbox_item(&mut conn, id, "accepted").expect("first resolve");
        let updated = resolve_inbox_item(&mut conn, id, "accepted").expect("second resolve");
        assert!(
            !updated,
            "second resolve on already-resolved id returns false"
        );
    }

    /// Mutation test: if the confidence guard is weakened to accept 1.0,
    /// this test should FAIL (i.e., the suite goes red).
    ///
    /// Change `if (confidence - 1.0).abs() < f64::EPSILON` to
    /// `if confidence > 1.0` in propose_inbox_item, and this test will pass
    /// incorrectly — proving the guard is necessary and tested.
    #[test]
    fn mutation_confidence_1_0_rejection_is_tested() {
        let conn = open_test_db();
        conn.execute("INSERT INTO books (title) VALUES ('Test Book')", [])
            .expect("insert book");
        let book_id = conn.last_insert_rowid();

        // The function MUST reject 1.0; if a mutation makes it accept 1.0,
        // this assert will fail to catch the error (the call will succeed
        // when it should return Err), so the test will incorrectly pass.
        // Running with the mutation proves the test goes red.
        let result = propose_inbox_item(&conn, book_id, "dup", "merge", 1.0);
        assert!(result.is_err(), "confidence 1.0 must be rejected");
    }
}
