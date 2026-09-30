//! Schema-completeness test: every table `books_delete_trg` touches must exist.
//!
//! The `annotations` table was missing from `calibre_schema.sql` while
//! `books_delete_trg` did `DELETE FROM annotations WHERE book=OLD.id`. SQLite
//! resolves a trigger body lazily, so the omission was invisible until
//! something actually deleted a book — which is exactly what a duplicate-merge
//! does. It then failed with "no such table: main.annotations", reported
//! against a table the caller never named, and a merge could never complete.
//!
//! This is a test about **absence**, which is the case where green means
//! nothing on its own: a check that matches nothing passes too. So the list of
//! tables the trigger names is compared against the list the schema creates,
//! and `the_delete_trigger_names_at_least_one_table` exists to prove the
//! extraction is not silently matching zero things — if the trigger body were
//! reformatted and the regex stopped matching, that test fails instead of this
//! one passing vacuously.

use lorebook_calibre::{create_library, functions};
use std::path::{Path, PathBuf};

const SCHEMA: &str = include_str!("../src/calibre_schema.sql");

/// A scratch directory, matching the convention in `interop.rs` — this crate
/// deliberately has no `tempfile` dev-dependency, and a test that needs a
/// throwaway library should not be the reason one is added.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lorebook-schema-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Tables `books_delete_trg` deletes from, in schema order.
fn trigger_tables() -> Vec<String> {
    let start = SCHEMA
        .find("CREATE TRIGGER books_delete_trg")
        .expect("books_delete_trg must exist: a library with no cascade leaves orphans");
    let end = SCHEMA[start..]
        .find("END;")
        .map(|i| start + i)
        .expect("the trigger body must be terminated");
    let body = &SCHEMA[start..end];

    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DELETE FROM ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            out.push(name);
        }
    }
    out
}

/// Tables `calibre_schema.sql` creates.
fn created_tables() -> Vec<String> {
    let mut out = Vec::new();
    for line in SCHEMA.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("CREATE TABLE ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            out.push(name);
        }
    }
    out
}

/// The guard on the guard. If this ever fails, the extraction above has stopped
/// working and `every_table_the_delete_trigger_touches_exists` is measuring an
/// empty list — which it would pass.
#[test]
fn the_delete_trigger_names_at_least_one_table() {
    let tables = trigger_tables();
    assert!(
        tables.len() >= 10,
        "only {} table(s) parsed out of books_delete_trg: {tables:?}. The extraction \
         has stopped matching the trigger body, so the completeness test below is \
         vacuous.",
        tables.len()
    );
}

#[test]
fn every_table_the_delete_trigger_touches_exists() {
    let created = created_tables();
    let named = trigger_tables();
    let missing: Vec<&String> = named.iter().filter(|t| !created.contains(t)).collect();

    assert!(
        missing.is_empty(),
        "books_delete_trg deletes from {missing:?} but calibre_schema.sql never creates \
         them. Every DELETE FROM books will fail with 'no such table', because SQLite \
         resolves a trigger body lazily and only when it fires."
    );
}

/// The behavioural half. Schema text can agree with itself and still not work:
/// this deletes a book for real, on a library built by the real function.
#[test]
fn deleting_a_book_works_on_a_library_we_create() {
    let dir = scratch("delete");
    let path = dir.join("library.db");
    let conn = create_library(&path).expect("create_library");
    functions::register(&conn).expect("register functions");

    conn.execute("INSERT INTO books (title) VALUES ('doomed')", [])
        .expect("insert book");
    let id: i64 = conn
        .query_row(
            "SELECT id FROM books WHERE title = 'doomed'",
            [],
            |r| r.get(0),
        )
        .expect("find it back");

    // The assertion is that this does not return Err. Before the `annotations`
    // table was added it failed here, and the error named a table that is
    // nowhere in this test.
    conn.execute("DELETE FROM books WHERE id = ?1", [id])
        .expect("deleting a book must not fail: the cascade trigger is the library's \
                 whole contract for cleaning up after a merge");

    let left: i64 = conn
        .query_row("SELECT count(*) FROM books", [], |r| r.get(0))
        .expect("count");
    assert_eq!(left, 0, "the book is actually gone, not just un-deleted");

    drop(conn);
    assert!(Path::new(&path).exists(), "the database file itself is still there");
}
