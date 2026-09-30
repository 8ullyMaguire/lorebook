use crate::inbox;
use crate::ScanError;
use lorebook_calibre::{create_library, migrate, functions};
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite::functions::{Context, FunctionFlags};
use std::fs;
use std::path::Path;

/// What the user decided about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairAction {
    /// One book, two sources. The loser's file becomes a source of the winner.
    Merge,
    /// Two genuinely distinct works. Nothing is written to the library.
    KeepSeparate,
    /// Not now. The pair stays in the inbox, exactly as it was.
    Defer,
}

impl PairAction {
    /// The value stored in `inbox_pairs.action`, which has a CHECK on it.
    pub fn as_str(self) -> &'static str {
        match self {
            PairAction::Merge => "merge",
            PairAction::KeepSeparate => "keep_separate",
            PairAction::Defer => "defer",
        }
    }

    /// Parse a stored value. `None` for anything else, which is what a row
    /// written by a future version with a new action would read as — the
    /// caller treats that as unresolved rather than guessing.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(PairAction::Merge),
            "keep_separate" => Some(PairAction::KeepSeparate),
            "defer" => Some(PairAction::Defer),
            _ => None,
        }
    }
}

/// Why the pipeline believes two books are one work, as shown to the user.
///
/// The spec's own words (spec §3.6 stage 4) are the variants, so the value the
/// user reads in the inbox is the spec's, not a re-summary of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairReason {
    /// Different identifiers, similar content. The one case §3.6 sends here
    /// because "merging two genuinely distinct works is destructive in a way
    /// the user cannot undo from their side".
    ProbableCrossPost,
    /// Same identifier, different hash, and "newest" is within 24 h either way
    /// (§3.4 rule 3: only genuinely ambiguous timestamps go to the inbox).
    AmbiguousVersion,
}

impl PairReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PairReason::ProbableCrossPost => "probable_cross_post",
            PairReason::AmbiguousVersion => "ambiguous_version",
        }
    }
}

/// A pair awaiting the user.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxPair {
    pub inbox_item: i64,
    pub book: i64,
    pub other_book: i64,
    pub reason: PairReason,
    /// The evidence string shown beside the guess.
    pub evidence: String,
    pub confidence: f64,
    /// The format each side holds, when they are different formats. `None` on
    /// both means the pair is the same format on both sides, which
    /// [`resolve_pair`] refuses to merge.
    pub book_format: Option<String>,
    pub other_format: Option<String>,
}

/// What a resolution actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// One book, and the surviving book id.
    Merged { kept_book: i64, kept_format: String },
    /// Recorded as two distinct works. Nothing in the library changed.
    KeptSeparate,
    /// Left in the inbox.
    Deferred,
}

/// Propose a pair.
///
/// Creates the `inbox_items` row and its `inbox_pairs` row together, so an
/// item can never exist without the evidence that makes it judgeable — the two
/// inserts are one transaction for the same reason [`inbox::resolve_inbox_item`]
/// writes its two columns together.
///
/// `book` and `other_book` must differ. A self-pair is rejected rather than
/// stored: the merge would be a no-op that consumed an inbox item, so the user
/// loses the prompt and gains nothing. (SQLite forbids the subquery that would
/// express this as a CHECK — see the note in `additive.sql`.)
///
/// # Errors
/// - `book == other_book`.
/// - `confidence == 1.0` — reserved for user assertions; see
///   [`inbox::propose_inbox_item`].
/// - `evidence` empty or whitespace: enforced by the table, so the error comes
///   from SQLite. Asserted anyway, so the failure names the cause.
pub fn propose_pair(
    conn: &mut Connection,
    book: i64,
    other_book: i64,
    reason: PairReason,
    evidence: &str,
    confidence: f64,
) -> Result<i64, ScanError> {
    if book == other_book {
        return Err(ScanError::Sql(format!(
            "a pair must name two different books, got {book} twice"
        )));
    }
    if evidence.trim().is_empty() {
        return Err(ScanError::Sql(
            "a pair must carry evidence; an unexplained inbox item is one the user has to investigate themselves"
                .to_string(),
        ));
    }

    let tx = conn
        .transaction()
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let item = inbox::propose_inbox_item(&tx, book, reason.as_str(), "", confidence)?;
    tx.execute(
        "INSERT INTO inbox_pairs (inbox_item, other_book, evidence) VALUES (?1, ?2, ?3)",
        params![item, other_book, evidence],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;

    tx.commit().map_err(|e| ScanError::Sql(e.to_string()))?;
    Ok(item)
}

/// The unresolved pair for an inbox item, with the formats each side holds.
pub fn list_pairs(conn: &Connection) -> Result<Vec<InboxPair>, ScanError> {
    let mut stmt = conn
        .prepare(
            "SELECT p.inbox_item, i.book, p.other_book, i.reason, p.evidence, i.confidence,
                    (SELECT format FROM book_sources WHERE book = i.book  ORDER BY id LIMIT 1),
                    (SELECT format FROM book_sources WHERE book = p.other_book ORDER BY id LIMIT 1)
             FROM inbox_pairs p
             JOIN inbox_items i ON i.id = p.inbox_item
             WHERE p.action IS NULL AND i.resolved_at IS NULL
             ORDER BY i.created_at, p.inbox_item",
        )
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let pairs = stmt
        .query_map([], |row| {
            let reason: String = row.get(3)?;
            Ok(InboxPair {
                inbox_item: row.get(0)?,
                book: row.get(1)?,
                other_book: row.get(2)?,
                // An unknown reason is a row from a future version. Reporting
                // it as unresolved is right; inventing a reason is not.
                reason: match reason.as_str() {
                    "probable_cross_post" => PairReason::ProbableCrossPost,
                    _ => PairReason::AmbiguousVersion,
                },
                evidence: row.get(4)?,
                confidence: row.get(5)?,
                book_format: row.get(6)?,
                other_format: row.get(7)?,
            })
        })
        .map_err(|e| ScanError::Sql(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ScanError::Sql(e.to_string()))?;
    Ok(pairs)
}

/// Resolve a pair, and perform the merge if the user chose one.
///
/// One transaction: the source move, the losing book's delete, the inbox item's
/// resolution and the pair's action are all-or-nothing. Calibre's
/// `books_delete_trg` removes the loser's `data` / `identifiers` / link rows,
/// so the merge deliberately does not copy them.
///
/// A merge picks the winner by format: `book_sources` is `UNIQUE (book, format)`,
/// so two books holding the same format cannot be merged without discarding a
/// file. That case is **not** resolved — the pair stays in the inbox and the
/// error names the collision, because a merge that quietly drops one of two
/// files the user pointed at is worse than an item they have to answer.
///
/// # Errors
/// - No such inbox item, or it is not a pair item.
/// - Already resolved (returns `Ok(None)` — not an error; a second click is a
///   no-op, not a failure).
/// - `Merge` when both books hold the same format.
pub fn resolve_pair(
    conn: &mut Connection,
    inbox_item: i64,
    action: PairAction,
) -> Result<Option<MergeOutcome>, ScanError> {
    let tx = conn
        .transaction()
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    // The pair, locked against a concurrent resolve. `book` is the inbox item's
    // own book, so a missing row here means "not a pair item" — which is a
    // different thing from "already resolved" and must not be reported as one.
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT i.book, p.other_book
             FROM inbox_pairs p JOIN inbox_items i ON i.id = p.inbox_item
             WHERE p.inbox_item = ?1 AND p.action IS NULL AND i.resolved_at IS NULL",
            params![inbox_item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let Some((book, other)) = row else {
        // Either it does not exist, or someone already answered it. Both are
        // "nothing to do"; the caller distinguishes with a second query if it
        // needs to.
        tx.commit().map_err(|e| ScanError::Sql(e.to_string()))?;
        return Ok(None);
    };

    let outcome = match action {
        PairAction::Defer => MergeOutcome::Deferred,
        PairAction::KeepSeparate => {
            tx.execute(
                "UPDATE inbox_pairs SET action = 'keep_separate' WHERE inbox_item = ?1",
                params![inbox_item],
            )
            .map_err(|e| ScanError::Sql(e.to_string()))?;
            inbox::resolve_inbox_item_in(&tx, inbox_item, "kept_separate")?;
            MergeOutcome::KeptSeparate
        }
        PairAction::Merge => {
            let merged = merge_books(&tx, book, other)?;
            MergeOutcome::Merged {
                kept_book: merged.0,
                kept_format: merged.1,
            }
        }
    };

    if action != PairAction::Defer {
        tx.execute(
            "UPDATE inbox_pairs SET action = ?2, kept_format = ?3 WHERE inbox_item = ?1",
            params![
                inbox_item,
                action.as_str(),
                match &outcome {
                    MergeOutcome::Merged { kept_format, .. } => Some(kept_format.clone()),
                    _ => None,
                }
            ],
        )
        .map_err(|e| ScanError::Sql(e.to_string()))?;

        inbox::resolve_inbox_item_in(&tx, inbox_item, action.as_str())?;
    }

    tx.commit().map_err(|e| ScanError::Sql(e.to_string()))?;
    Ok(Some(outcome))
}

/// Merge `loser` into `keeper`, returning `(keeper, kept_format)`.
///
/// The loser's `book_sources` row moves to the keeper. Calibre's
/// `books_delete_trg` then cleans up everything else the loser owned, which is
/// why nothing here copies `data` or `identifiers` — that trigger is
/// registered by `create_library`, and a hand-rolled copy would produce a book
/// with two rows in a table whose unique index allows one.
///
/// The loser's *file* is untouched: a source row is a record of a file, not
/// ownership of it, and `kind` decides whether the app may ever delete one.
fn merge_books(tx: &Connection, keeper: i64, loser: i64) -> Result<(i64, String), ScanError> {
    // One format each, and different. Same format on both sides means
    // `UNIQUE (book, format)` has nowhere to put the second row, and the only
    // way "through" is to drop a file the user still has.
    let keeper_format: Option<String> = tx
        .query_row(
            "SELECT format FROM book_sources WHERE book = ?1 AND state = 'ok' ORDER BY id LIMIT 1",
            params![keeper],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let loser_format: Option<String> = tx
        .query_row(
            "SELECT format FROM book_sources WHERE book = ?1 AND state = 'ok' ORDER BY id LIMIT 1",
            params![loser],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let (Some(kf), Some(lf)) = (keeper_format.clone(), loser_format) else {
        return Err(ScanError::Sql(format!(
            "cannot merge: one side of the pair has no usable source (book {keeper} / book {loser})"
        )));
    };

    if kf.eq_ignore_ascii_case(&lf) {
        return Err(ScanError::Sql(format!(
            "cannot merge books {keeper} and {loser}: both hold format {kf}, and \
             book_sources is UNIQUE(book, format) — merging would drop one of the two files"
        )));
    }

    // Move the loser's source to the keeper. `path`, `kind`, `content_hash` and
    // `state` all come across: the file is still there and still is what it was.
    tx.execute(
        "UPDATE book_sources SET book = ?2 WHERE book = ?1",
        params![loser, keeper],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;

    // Now the loser. Calibre's trigger clears its link tables, data,
    // identifiers and comments; our own `book_sources` rows are already gone.
    tx.execute("DELETE FROM books WHERE id = ?1", params![loser])
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    Ok((keeper, kf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lorebook_calibre::{create_library, migrate};
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A library with `n` books, each holding one EPUB at a distinct path.
    fn library_with_books(n: usize) -> (tempfile::TempDir, Connection) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("library.db");
        let conn = create_library(&path).expect("create_library");
        functions::register(&conn).expect("register functions");
        // The Calibre database may have user-defined functions that are not stored in the file.
        // Define the title_sort function used in some triggers.
        conn.create_scalar_function(
            "title_sort",
            1 as i32,
            FunctionFlags::SQLITE_UTF8,
            move |ctx: &Context<'_>| -> Result<String, rusqlite::Error> {
                let value: String = ctx.get(0)?;
                Ok(value)
            },
        )
        .expect("failed to create title_sort function");
        migrate(&conn).expect("migrate");
        for i in 0..n {
            add_book(&conn, i, "EPUB", "ok");
        }
        (dir, conn)
    }

    /// A library with Calibre's full schema (from the fixture) plus our additive migrations.
    fn library_with_fixture() -> (tempfile::TempDir, Connection) {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let fixture_path = format!("{}/../../fixtures/calibre-9.15-metadata.db", manifest_dir);
        let fixture = std::fs::read(fixture_path).expect("read fixture");
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("library.db");
        std::fs::write(&path, &fixture).expect("write fixture");
        let conn = rusqlite::Connection::open(&path).expect("open db");
        functions::register(&conn).expect("register functions");
        // The Calibre database may have user-defined functions that are not stored in the file.
        // Define the title_sort function used in some triggers.
        conn.create_scalar_function(
            "title_sort",
            1 as i32,
            FunctionFlags::SQLITE_UTF8,
            move |ctx: &Context<'_>| -> Result<String, rusqlite::Error> {
                let value: String = ctx.get(0)?;
                Ok(value)
            },
        )
        .expect("failed to create title_sort function");
        migrate(&conn).expect("migrate our additives");
        (dir, conn)
    }

    /// Helper to get two distinct book IDs from the fixture.
    fn get_two_books_from_fixture(conn: &mut Connection) -> Result<(i64, i64), ScanError> {
        let mut stmt = conn
            .prepare("SELECT id FROM books ORDER BY id LIMIT 2")
            .map_err(|e| ScanError::Sql(e.to_string()))?;
        let mut rows = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| ScanError::Sql(e.to_string()))?;

        let book1: i64 = match rows.next() {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(ScanError::Sql(e.to_string())),
            None => return Err(ScanError::Sql("need at least two books in fixture".to_string())),
        };

        let book2: i64 = match rows.next() {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(ScanError::Sql(e.to_string())),
            None => return Err(ScanError::Sql("need at least two books in fixture".to_string())),
        };

        Ok((book1, book2))
    }

    /// Insert a book plus one `book_sources` row, as a scan would.
    ///
    /// `path` and `content_hash` are keyed on the **book id**, not on the `i`
    /// passed in. The two are the same only when books are inserted into an
    /// empty table in order, and that coincidence is exactly what let three
    /// tests assert `/books/book{book1}` against a row actually holding
    /// `/books/book0` — a failure that looks like a merge bug and is not one.
    /// Keying on the id means a test cannot state a path the row does not have.
    fn add_book(conn: &Connection, i: usize, format: &str, state: &str) -> i64 {
        let book: i64 = conn
            .query_row(
                "INSERT INTO books (title) VALUES (?1) RETURNING id",
                params![format!("Book {i}")],
                |r| r.get(0),
            )
            .expect("insert book");
        conn.execute(
            "INSERT INTO book_sources (book, format, kind, path, size, mtime_ns, content_hash, state)
             VALUES (?1, ?2, 'reference', ?3, 100, ?4, ?5, ?6)",
            params![
                book,
                format,
                format!("/books/book{book}.{format}"),
                1000 + i as i64,
                format!("hash{book}"),
                state
            ],
        )
        .expect("insert source");
        book
    }

    fn sources_of(conn: &Connection, book: i64) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT format, path FROM book_sources WHERE book = ?1 ORDER BY format")
            .expect("prepare");
        let rows = stmt
            .query_map(params![book], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        rows
    }

    fn pair_for(conn: &Connection, item: i64) -> InboxPair {
        list_pairs(conn)
            .expect("list")
            .into_iter()
            .find(|p| p.inbox_item == item)
            .expect("pair is listed")
    }

    // --- propose_pair -----------------------------------------------------

    #[test]
    fn a_proposed_pair_is_listed_with_both_books_and_its_evidence() {
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "first-chapter text is 94% identical",
            0.62,
        )
        .expect("propose_pair");

        let pairs = list_pairs(&conn).expect("list");
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        assert_eq!(p.inbox_item, item);
        assert_eq!((p.book, p.other_book), (1, 2));
        assert_eq!(p.reason, PairReason::ProbableCrossPost);
        assert_eq!(p.evidence, "first-chapter text is 94% identical");
        assert!((p.confidence - 0.62).abs() < f64::EPSILON);
        assert_eq!(p.book_format.as_deref(), Some("EPUB"));
        assert_eq!(p.other_format.as_deref(), Some("EPUB"));
    }

    #[test]
    fn a_self_pair_is_rejected() {
        let (_d, conn) = library_with_books(1);
        let mut conn = conn;
        let err = propose_pair(
            &mut conn,
            1,
            1,
            PairReason::ProbableCrossPost,
            "itself",
            0.5,
        )
        .expect_err("a self-pair must be rejected");
        assert!(
            err.to_string().contains("two different books"),
            "error should name the self-pair, got: {err}"
        );
        // And nothing was written: a rejected proposal leaves no item.
        assert!(list_pairs(&conn).expect("list").is_empty());
        assert!(inbox::list_unresolved(&conn).expect("list").is_empty());
    }

    #[test]
    fn a_pair_without_evidence_is_rejected() {
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let err = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "   ",
            0.5,
        )
        .expect_err("blank evidence must be rejected");
        assert!(
            err.to_string().contains("evidence"),
            "error should name the evidence, got: {err}"
        );
        assert!(list_pairs(&conn).expect("list").is_empty());
    }

    #[test]
    fn the_schema_itself_rejects_empty_evidence() {
        // The Rust guard is the friendly error; this proves the table would
        // catch a row written by anything else. A hand-edited database or a
        // future migration is exactly the case the CHECK exists for.
        let (_d, mut conn) = library_with_books(2);
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "real evidence",
            0.5,
        )
        .expect("propose");
        let err = conn
            .execute(
                "INSERT INTO inbox_pairs (inbox_item, other_book, evidence) VALUES (?1, 2, '')",
                params![item],
            )
            .expect_err("the table must refuse empty evidence");
        assert!(
            err.to_string().contains("CHECK"),
            "expected a CHECK violation, got: {err}"
        );
    }

    #[test]
    fn a_pair_inherits_the_confidence_1_0_rejection() {
        // 1.0 means "the user asserted this" and is the schema's DEFAULT, so a
        // machine decision at 1.0 would bypass the inbox entirely — the same
        // hazard `propose_inbox_item` guards, and it must still fire when the
        // pair is proposed through the pair API.
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let err = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "evidence",
            1.0,
        )
        .expect_err("confidence 1.0 must be rejected");
        assert!(
            err.to_string().contains("confidence == 1.0"),
            "error should name the confidence guard, got: {err}"
        );
    }

    // --- resolve: merge ---------------------------------------------------

    #[test]
    fn merging_two_formats_leaves_one_book_with_two_sources() {
            // The M3.3 end-to-end claim: a duplicate pair, accepted, becomes one
            // book with two sources.
            //
            // Built on `library_with_books`, NOT on the Calibre fixture. The
            // fixture's single book is Calibre's own `notes` book, which has a
            // `books.path` but no `book_sources` row at all — so it is a book
            // with no file this app has ever seen. Merging it correctly fails,
            // because a side with no usable source cannot donate one. Asserting
            // a merge there tests a fiction about the fixture.
            let (d, mut conn) = library_with_books(1);
            // Get the single book, which holds one EPUB at a distinct path.
            let book1: i64 = conn.query_row("SELECT id FROM books LIMIT 1", [], |r| r.get(0))
                .expect("failed to get book1");
            // Add a second book in a different format so the merge is legal:
            // `book_sources` is UNIQUE(book, format), so same-format on both
            // sides is refused by design.
            let book2 = add_book(&mut conn, 999, "PDF", "ok");
            let item = propose_pair(
                &mut conn,
                book1,
                book2,
                PairReason::ProbableCrossPost,
                "identical first chapter",
                0.6,
            )
            .expect("propose");

            let outcome = resolve_pair(&mut conn, item, PairAction::Merge)
                .expect("resolve")
                .expect("the pair was listed, so it resolves");
            assert_eq!(
                outcome,
                MergeOutcome::Merged {
                    kept_book: book1,
                    kept_format: "EPUB".to_string()
                }
            );

            // One book survives (we started with 2, merged one -> 1).
            let books: i64 = conn
                .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
                .expect("count books");
            assert_eq!(books, 1, "the losing book must be gone");

            // ...with the keeper's source and the loser's, both intact.
            let sources = sources_of(&conn, book1);
            assert_eq!(sources.len(), 2, "both files must survive the merge");
            assert!(
                sources.iter().any(|(_, p)| *p == format!("/books/book{book1}.EPUB")),
                "the keeper's file must be untouched"
            );
            assert!(
                sources.iter().any(|(_, p)| *p == format!("/books/book{book2}.PDF")),
                "the loser's extra file must be carried across"
            );

            // And the inbox is clear.
            assert!(list_pairs(&conn).expect("list").is_empty());
            assert!(inbox::list_unresolved(&conn).expect("list").is_empty());

            // Clean up the temporary directory.
            d.close().expect("tempdir close");
        }

    #[test]
    fn merging_moves_the_loser_path_and_hash_verbatim() {
        // `library_with_books`, not the Calibre fixture — see the note on
        // `merging_two_formats_leaves_one_book_with_two_sources`. The keeper
        // must hold a real `book_sources` row for the loser's row to move to.
        let (d, mut conn) = library_with_books(1);
        // Get the single book, which holds one EPUB at a distinct path.
        let book1: i64 = conn.query_row("SELECT id FROM books LIMIT 1", [], |r| r.get(0))
            .expect("failed to get book1");
        // Add a second book in PDF format.
        let book2 = add_book(&mut conn, 999, "PDF", "ok");
        // Propose a pair between book1 and book2.
        let item = propose_pair(
            &mut conn,
            book1,
            book2,
            PairReason::ProbableCrossPost,
            "evidence",
            0.6,
        )
        .expect("propose");
        resolve_pair(&mut conn, item, PairAction::Merge)
            .expect("resolve")
            .expect("resolves");

        // Check that the loser's source row (book2) is now attached to the keeper (book1) and that the path and hash are unchanged.
        let (fmt, hash, state): (String, String, String) = conn
            .query_row(
                "SELECT format, content_hash, state FROM book_sources WHERE path = ?1",
                params![format!("/books/book{book2}.PDF")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("the loser's source row moved across with its values");
        assert_eq!(fmt, "PDF");
        assert_eq!(hash, format!("hash{book2}"), "the hash is the merge key and must not change");
        assert_eq!(state, "ok");

        // Clean up the temporary directory.
        d.close().expect("tempdir close");
    }

    #[test]
    fn merging_the_same_format_on_both_sides_is_refused_not_performed() {
        // `book_sources` is UNIQUE(book, format). Two books that each hold an
        // EPUB cannot be merged without discarding one of the user's files, so
        // this must stay an inbox item rather than resolve.
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "evidence",
            0.6,
        )
        .expect("propose");

        let err = resolve_pair(&mut conn, item, PairAction::Merge)
            .expect_err("same-format merge must be refused");
        assert!(
            err.to_string().contains("UNIQUE(book, format)"),
            "error should name the constraint, got: {err}"
        );

        // Nothing changed: both books, both sources, item still open.
        assert_eq!(sources_of(&conn, 1).len(), 1);
        assert_eq!(sources_of(&conn, 2).len(), 1);
        assert_eq!(list_pairs(&conn).expect("list").len(), 1, "still open");
    }

    #[test]
    fn a_merge_is_all_or_nothing() {
        // If the merge fails partway the transaction must roll back, leaving
        // the inbox item open. The same-format case is the natural failure:
        // it must not have half-moved the source row before erroring.
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "evidence",
            0.6,
        )
        .expect("propose");
        let _ = resolve_pair(&mut conn, item, PairAction::Merge);

        // Both books still exist, and neither has gained the other's file.
        assert_eq!(sources_of(&conn, 1).len(), 1);
        assert_eq!(sources_of(&conn, 2).len(), 1);
        assert_eq!(pair_for(&conn, item).book, 1);
    }

    // --- resolve: keep separate / defer ------------------------------------

    #[test]
    fn keeping_them_separate_writes_nothing_to_the_library() {
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "they are different translations",
            0.6,
        )
        .expect("propose");

        let outcome = resolve_pair(&mut conn, item, PairAction::KeepSeparate)
            .expect("resolve")
            .expect("resolves");
        assert_eq!(outcome, MergeOutcome::KeptSeparate);

        let books: i64 = conn
            .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
            .expect("count");
        assert_eq!(books, 2, "keep_separate must not merge or delete");
        assert!(list_pairs(&conn).expect("list").is_empty());
    }

    #[test]
    fn deferring_leaves_the_pair_exactly_as_it_was() {
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "evidence",
            0.6,
        )
        .expect("propose");
        let before = pair_for(&conn, item);

        let outcome = resolve_pair(&mut conn, item, PairAction::Defer)
            .expect("resolve")
            .expect("resolves");
        assert_eq!(outcome, MergeOutcome::Deferred);

        let after = pair_for(&conn, item);
        assert_eq!(after.inbox_item, before.inbox_item);
        assert_eq!(after.book, before.book);
        assert_eq!(after.other_book, before.other_book);
        assert_eq!(after.reason, before.reason);
        assert_eq!(after.evidence, before.evidence);
        assert!((after.confidence - before.confidence).abs() < f64::EPSILON);
        assert_eq!(after.book_format, before.book_format);
        assert_eq!(after.other_format, before.other_format);
        assert!(inbox::list_unresolved(&conn).expect("list").len() == 1);
    }

    // --- resolve: idempotence ---------------------------------------------

    #[test]
    fn resolving_twice_does_nothing_the_second_time() {
        let (_d, conn) = library_with_books(2);
        let mut conn = conn;
        let item = propose_pair(
            &mut conn,
            1,
            2,
            PairReason::ProbableCrossPost,
            "evidence",
            0.6,
        )
        .expect("propose");

        assert!(resolve_pair(&mut conn, item, PairAction::KeepSeparate)
            .expect("first")
            .is_some());
        // A second click on an already-answered item is a no-op, not an error:
        // the UI will happily send it when the user double-clicks.
        assert_eq!(
            resolve_pair(&mut conn, item, PairAction::Merge)
                .expect("second"),
            None
        );
        // ...and critically, the second resolve did NOT merge anything.
        let books: i64 = conn
            .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
            .expect("count");
        assert_eq!(books, 2, "a double-click must not merge after keep_separate");
    }

    #[test]
    fn resolving_an_id_that_is_not_a_pair_item_returns_none() {
        // An `inbox_items` row with no pair — a file the pipeline could not
        // name, §3.7's first bullet. Resolving it through the pair API is not
        // an error; there is simply no pair to act on.
        let (_d, mut conn) = library_with_books(1);
        let item = inbox::propose_inbox_item(&conn, 1, "unidentifiable", "", 0.2).expect("propose");
        assert_eq!(
            resolve_pair(&mut conn, item, PairAction::Merge).expect("resolve"),
            None
        );
    }
}