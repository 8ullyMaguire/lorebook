//! Integration tests against a **real Calibre library**.
//!
//! `fixtures/calibre-9.15-metadata.db` was produced by running
//! `calibre --with-library=… add …` with Calibre 9.15, not hand-written. Every
//! assertion here therefore checks the actual schema a user has, and a Calibre
//! upgrade that moves a column fails these tests instead of failing silently
//! in someone's library.
//!
//! Each test works on a copy, so a failing test cannot damage the fixture.
//!
//! The interop guarantee under test (spec §4): Calibre's own tables are read
//! and written unmodified, and new tables are additive.

use lorebook_calibre as cal;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/calibre-9.15-metadata.db")
}

/// A writable copy of the real Calibre library.
fn copy_fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lorebook-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp lib dir");
    let dst = dir.join("metadata.db");
    std::fs::copy(fixture(), &dst).expect("copy fixture metadata.db");
    dst
}

fn open_copy(name: &str) -> (PathBuf, Connection) {
    let db = copy_fixture(name);
    let dir = db.parent().expect("fixture is inside a dir").to_path_buf();
    // Through the real open path, not a raw Connection::open: registration of
    // Calibre's SQL functions happens there, and it is what makes writes work
    // against a library Calibre created. A test that bypassed it would pass
    // while the app failed.
    let conn = cal::open_library(&dir).expect("open copied library");
    (db, conn)
}

// ---------------------------------------------------------------------------
// The interop guarantee
// ---------------------------------------------------------------------------

#[test]
fn opens_a_real_calibre_library() {
    let (_db, conn) = open_copy("open");
    cal::verify_schema(&conn).expect("real Calibre library must verify");
}

#[test]
fn records_calibre_schema_version() {
    let (_db, conn) = open_copy("version");
    // Calibre 9.15 writes 27. Asserting a concrete value is the point: a
    // silent change here means the fixture is no longer representative.
    assert_eq!(cal::calibre_schema_version(&conn).unwrap(), 27);
}

#[test]
fn reads_books_calibre_created() {
    let (_db, conn) = open_copy("read-books");
    let books = cal::list_books(&conn).expect("list books");
    assert!(!books.is_empty(), "fixture must contain Calibre's own book");
    let b = &books[0];
    assert_eq!(b.id, 1);
    assert_eq!(b.title, "notes");
    // Calibre computed the sort on insert via its own title_sort(); if we read
    // the wrong column, sort would be NULL rather than a derived value.
    assert_eq!(b.sort.as_deref(), Some("notes"));
    // No series was set, so Calibre stored the default 1.0 rather than NULL.
    assert_eq!(b.series_index, 1.0);
}

#[test]
fn reads_formats_from_the_data_table() {
    let (_db, conn) = open_copy("read-formats");
    // `data` is one row per format, not a JSON blob. A book with no formats
    // yields an empty list rather than an error.
    let formats = cal::calibre_formats(&conn, 1).expect("read formats");
    // The fixture book was added from a .txt, so it has one format.
    assert_eq!(formats.len(), 1, "expected exactly one format row");
    assert_eq!(formats[0].format, "TXT");
    assert!(formats[0].uncompressed_size > 0);
}

#[test]
fn a_calibre_book_has_no_source_rows_until_we_import_it() {
    // This is the property the whole import model rests on (spec §3.1-3.2):
    // Calibre records that a format exists but never where the file is, so a
    // book Calibre created legitimately has no `book_sources` row.
    let (_db, conn) = open_copy("no-sources");
    cal::apply_additive_schema(&conn).expect("apply additive");

    // No `book_sources` row: we have not imported the file, so we do not know
    // where it is. The row count from our own table is the real assertion.
    let ours: i64 = conn
        .query_row(
            "SELECT count(*) FROM book_sources WHERE book = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ours, 0, "importing has not happened, so we have no paths");

    // ...but the format is still visible, from Calibre's own table, marked as
    // not-yet-imported rather than hidden.
    let sources = cal::list_sources(&conn, 1).expect("list sources");
    assert_eq!(sources.len(), 1, "the TXT Calibre stored must be listed");
    assert_eq!(sources[0].format, "TXT");
    assert!(
        sources[0].path.as_os_str().is_empty(),
        "we cannot know the path"
    );
    assert_eq!(sources[0].state, lorebook_core::SourceState::Missing);
}

#[test]
fn rejects_a_directory_that_is_not_a_library() {
    let dir = std::env::temp_dir().join(format!("lorebook-notalib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    match cal::open_library(&dir) {
        Err(cal::CalibreError::NotALibrary(_)) => {}
        other => panic!("expected NotALibrary, got {other:?}"),
    }
}

#[test]
fn rejects_a_sqlite_file_that_is_not_calibre() {
    let dir = std::env::temp_dir().join(format!("lorebook-notcalibre-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let conn = Connection::open(dir.join("metadata.db")).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (x INTEGER)")
        .unwrap();
    drop(conn);

    match cal::open_library(&dir) {
        Err(cal::CalibreError::NotACalibreSchema) => {}
        other => panic!("expected NotACalibreSchema, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Writing into Calibre's own tables
// ---------------------------------------------------------------------------

#[test]
fn writes_a_book_calibre_can_read_back() {
    let (_db, conn) = open_copy("write-book");
    let id = cal::insert_book(
        &conn,
        "A Test Work",
        Some("A Test Work, A"),
        Some("Work, A"),
        None,
    )
    .expect("insert book");
    assert!(id > 0);

    let book = cal::get_book(&conn, id)
        .expect("get book")
        .expect("book exists");
    assert_eq!(book.title, "A Test Work");
    // `author_sort` is a display column, not an author: it stores no author row,
    // which is why the resolved author list is empty until link_author is used.
    assert_eq!(book.author_sort.as_deref(), Some("Work, A"));
    assert!(book.authors.is_empty(), "no author row was created");

    // Calibre's own books_insert_trg recomputed `sort` from the title via
    // title_sort(), overriding what we passed. That is the interop guarantee
    // working: Calibre stays the single authority on its own derived columns,
    // so a library this app writes and one Calibre writes sort the same way.
    // We pass sort only for a library with no such trigger.
    assert_eq!(book.sort.as_deref(), Some("Test Work, A"));
    // ...and that trigger assigned a uuid too.
    assert!(
        book.uuid.is_some_and(|u| !u.is_empty()),
        "Calibre's trigger must set uuid"
    );
}

#[test]
fn insert_fires_calibres_own_pages_link_trigger() {
    // Calibre has an AFTER INSERT trigger creating books_pages_link. We must not
    // create that row ourselves or we violate the link table's uniqueness — so
    // this asserts the trigger did it, and that our insert did not double it.
    let (_db, conn) = open_copy("pages-trigger");
    let id = cal::insert_book(&conn, "Trigger Test", None, None, None).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM books_pages_link WHERE book = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 1,
        "Calibre's own trigger must create exactly one page link"
    );
}

#[test]
fn update_leaves_unset_columns_alone() {
    let (_db, conn) = open_copy("update");
    let id = cal::insert_book(
        &conn,
        "Original Title",
        Some("orig"),
        Some("Author Col"),
        None,
    )
    .unwrap();
    let before = cal::get_book(&conn, id).unwrap().unwrap();

    // Only the title is set; everything else must survive untouched.
    let changed = cal::update_book(&conn, id, Some("New Title"), None, None).unwrap();
    assert!(changed);

    let book = cal::get_book(&conn, id).unwrap().unwrap();
    assert_eq!(book.title, "New Title");
    assert_eq!(
        book.author_sort, before.author_sort,
        "author_sort the update did not mention must survive"
    );
    // Sort is recomputed from the new title by Calibre's own trigger, so it
    // tracks the title rather than the value we passed at insert time.
    assert_eq!(book.sort.as_deref(), Some("New Title"));
}

#[test]
fn update_of_a_missing_book_reports_no_rows() {
    let (_db, conn) = open_copy("update-missing");
    assert!(!cal::update_book(&conn, 999_999, Some("x"), None, None).unwrap());
}

#[test]
fn deleting_a_book_cascades_via_calibres_own_triggers() {
    let (_db, conn) = open_copy("delete");
    let id = cal::insert_book(&conn, "Doomed", None, None, None).unwrap();
    let author = cal::ensure_author(&conn, "Some Author").unwrap();
    cal::link_author(&conn, id, author).unwrap();

    assert!(cal::delete_book(&conn, id).unwrap());
    // Calibre's books_delete_trg cleans up the link rows. If we also cleaned up
    // manually we would be doing work the trigger already did.
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM books_authors_link WHERE book = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "Calibre's delete trigger must remove the link row");
}

// ---------------------------------------------------------------------------
// Authors, tags, identifiers
// ---------------------------------------------------------------------------

#[test]
fn ensure_author_is_idempotent() {
    // Reusing the row is what keeps "the same author" from becoming two
    // authors when a book is added twice.
    let (_db, conn) = open_copy("author-idem");
    let a = cal::ensure_author(&conn, "Ursula K. Le Guin").unwrap();
    let b = cal::ensure_author(&conn, "Ursula K. Le Guin").unwrap();
    assert_eq!(a, b);
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM authors WHERE name = ?1",
            ["Ursula K. Le Guin"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn author_sort_follows_calibres_comma_convention() {
    let (_db, conn) = open_copy("author-sort");
    let id = cal::ensure_author(&conn, "Ursula K. Le Guin").unwrap();
    let sort: String = conn
        .query_row("SELECT sort FROM authors WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(sort, "Guin, Ursula K. Le");
}

#[test]
fn empty_author_name_is_rejected() {
    let (_db, conn) = open_copy("author-empty");
    assert!(matches!(
        cal::ensure_author(&conn, "   "),
        Err(cal::CalibreError::Invalid(_))
    ));
}

#[test]
fn authors_and_tags_resolve_through_the_link_tables() {
    let (_db, conn) = open_copy("links");
    let book = cal::insert_book(&conn, "Tagged Work", None, None, None).unwrap();
    for name in ["Fiction", "Fanfic"] {
        let t = cal::ensure_tag(&conn, name).unwrap();
        cal::link_tag(&conn, book, t).unwrap();
    }
    let a = cal::ensure_author(&conn, "Jane Doe").unwrap();
    cal::link_author(&conn, book, a).unwrap();

    let b = cal::get_book(&conn, book).unwrap().unwrap();
    assert_eq!(
        b.tags,
        vec!["Fanfic", "Fiction"],
        "tags sorted case-insensitively"
    );
    assert_eq!(b.authors, vec!["Jane Doe"]);
}

#[test]
fn identifiers_are_one_per_scheme_per_book() {
    // Calibre's own constraint is UNIQUE(book, type) — one identifier of each
    // scheme per book. Not unique on the value: two different books can carry
    // the same ISBN, which is exactly what duplicate detection looks for.
    let (_db, conn) = open_copy("identifiers");
    let b1 = cal::insert_book(&conn, "Work One", None, None, None).unwrap();
    let b2 = cal::insert_book(&conn, "Work Two", None, None, None).unwrap();
    cal::add_identifier(&conn, b1, "ao3", "12345").unwrap();
    // The same value on a different book is allowed and must be stored.
    cal::add_identifier(&conn, b2, "ao3", "12345").unwrap();
    cal::add_identifier(&conn, b2, "isbn", "9780000000001").unwrap();

    assert_eq!(cal::list_identifiers(&conn, b1).unwrap().len(), 1);
    assert_eq!(cal::list_identifiers(&conn, b2).unwrap().len(), 2);

    // Adding a second ao3 identifier to one book replaces the first rather than
    // violating Calibre's constraint.
    cal::add_identifier(&conn, b1, "ao3", "99999").unwrap();
    let ids = cal::list_identifiers(&conn, b1).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].val, "99999");

    // A third scheme is still fine.
    cal::add_identifier(&conn, b1, "goodreads", "42").unwrap();
    assert_eq!(cal::list_identifiers(&conn, b1).unwrap().len(), 2);
}

#[test]
fn identifier_scheme_is_normalised_to_lowercase() {
    let (_db, conn) = open_copy("ident-case");
    let b = cal::insert_book(&conn, "Case Test", None, None, None).unwrap();
    cal::add_identifier(&conn, b, "AO3", "999").unwrap();
    let ids = cal::list_identifiers(&conn, b).unwrap();
    assert_eq!(ids[0].scheme, "ao3");
}

#[test]
fn empty_identifier_is_rejected() {
    let (_db, conn) = open_copy("ident-empty");
    let b = cal::insert_book(&conn, "X", None, None, None).unwrap();
    assert!(cal::add_identifier(&conn, b, "", "1").is_err());
    assert!(cal::add_identifier(&conn, b, "isbn", "  ").is_err());
}

#[test]
fn add_format_records_a_row_in_calibres_data_table() {
    let (_db, conn) = open_copy("add-format");
    let b = cal::insert_book(&conn, "Format Test", None, None, None).unwrap();
    cal::add_format(
        &conn,
        b,
        "epub",
        "Author Name/Format Test (1)/Format Test - Format Test.epub",
        1234,
    )
    .unwrap();
    let formats = cal::calibre_formats(&conn, b).unwrap();
    assert_eq!(formats.len(), 1);
    // Lowercase input is uppercased, matching Calibre's convention.
    assert_eq!(formats[0].format, "EPUB");
}

#[test]
fn marking_dirty_is_idempotent() {
    let (_db, conn) = open_copy("dirty");
    let b = cal::insert_book(&conn, "Dirty Test", None, None, None).unwrap();
    cal::mark_dirty(&conn, b).unwrap();
    cal::mark_dirty(&conn, b).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM metadata_dirtied WHERE book = ?1",
            [b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------------------
// The additive tables
// ---------------------------------------------------------------------------

#[test]
fn reads_work_on_a_library_never_written_to() {
    // The most common first-run case: a stock Calibre library, never touched by
    // this app. It has no `book_sources` table, and reading it must still work
    // rather than erroring — otherwise the app cannot show a user their library
    // until it has written to it.
    let (_db, conn) = open_copy("untouched");
    assert!(!cal::additive::is_applied(&conn));
    let books = cal::list_books(&conn).expect("list books on an untouched library");
    assert_eq!(books.len(), 1);
    let sources = cal::list_sources(&conn, 1).expect("list sources on an untouched library");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].state, lorebook_core::SourceState::Missing);
}

#[test]
fn additive_schema_applies_cleanly_and_is_idempotent() {
    let (_db, conn) = open_copy("additive");
    assert!(
        !cal::additive::is_applied(&conn),
        "fixture has no additive tables"
    );
    cal::apply_additive_schema(&conn).unwrap();
    assert!(cal::additive::is_applied(&conn));
    // Running again on an already-extended library must be a no-op, because
    // this happens on every open.
    cal::apply_additive_schema(&conn).unwrap();
    assert!(cal::additive::is_applied(&conn));
}

#[test]
fn additive_tables_do_not_disturb_calibres_own_tables() {
    // The interop guarantee, stated as a test: after extending a real Calibre
    // library, Calibre's own data is still exactly what it was.
    let (_db, conn) = open_copy("additive-nondisturb");
    let before: i64 = conn
        .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
        .unwrap();
    let before_schema = cal::calibre_schema_version(&conn).unwrap();

    cal::apply_additive_schema(&conn).unwrap();

    let after: i64 = conn
        .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "adding our tables must not touch books");
    assert_eq!(before_schema, cal::calibre_schema_version(&conn).unwrap());

    // And Calibre's own tables are all still present and queryable.
    for t in [
        "books",
        "authors",
        "tags",
        "series",
        "data",
        "identifiers",
        "custom_columns",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "Calibre's {t} must survive");
    }
}

#[test]
fn book_sources_enforce_kind_and_state() {
    // The CHECK constraints are what make an invalid state unrepresentable,
    // rather than something every writer has to remember to validate.
    let (_db, conn) = open_copy("source-checks");
    cal::apply_additive_schema(&conn).unwrap();
    let book = cal::insert_book(&conn, "Source Test", None, None, None).unwrap();

    let ok = conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, content_hash, state)
         VALUES (?1, 'EPUB', 'reference', '/read/only/a.epub', 'h1', 'ok')",
        [book],
    );
    assert!(ok.is_ok());

    let bad_kind = conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, content_hash, state)
         VALUES (?1, 'PDF', 'stolen', '/x.pdf', 'h2', 'ok')",
        [book],
    );
    assert!(bad_kind.is_err(), "an unknown kind must be rejected");

    let bad_state = conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, content_hash, state)
         VALUES (?1, 'MOBI', 'managed', '/x.mobi', 'h3', 'vaporised')",
        [book],
    );
    assert!(bad_state.is_err(), "an unknown state must be rejected");

    let bad_conf = conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, content_hash, state, confidence)
         VALUES (?1, 'AZW3', 'managed', '/x.azw3', 'h4', 'ok', 1.5)",
        [book],
    );
    assert!(
        bad_conf.is_err(),
        "confidence outside 0..1 must be rejected"
    );
}

#[test]
fn a_book_source_cascades_when_the_book_is_deleted() {
    let (_db, conn) = open_copy("source-cascade");
    cal::apply_additive_schema(&conn).unwrap();
    let book = cal::insert_book(&conn, "Cascade Test", None, None, None).unwrap();
    conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, content_hash, state)
         VALUES (?1, 'EPUB', 'reference', '/a.epub', 'h', 'ok')",
        [book],
    )
    .unwrap();

    cal::delete_book(&conn, book).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM book_sources WHERE book = ?1",
            [book],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "our source row must not outlive its book");
}

// ---------------------------------------------------------------------------
// Round trip: our writes survive being read by plain Calibre-shaped queries
// ---------------------------------------------------------------------------

#[test]
fn a_written_book_is_visible_to_calibres_own_meta_view() {
    // `meta` is a VIEW Calibre's own UI reads. If our inserted rows are not
    // visible through it, Calibre would show an empty library even though the
    // tables have the data — the exact failure the interop guarantee exists to
    // prevent.
    let (_db, conn) = open_copy("meta-view");
    let book = cal::insert_book(&conn, "Visible Work", Some("Visible Work"), None, None).unwrap();
    let a = cal::ensure_author(&conn, "Meta Author").unwrap();
    cal::link_author(&conn, book, a).unwrap();

    let title: Option<String> = conn
        .query_row("SELECT title FROM meta WHERE id = ?1", [book], |r| r.get(0))
        .expect("meta view must be queryable");
    assert_eq!(title.as_deref(), Some("Visible Work"));
}
