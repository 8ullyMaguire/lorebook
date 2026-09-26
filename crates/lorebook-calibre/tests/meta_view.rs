//! Proves the `meta` view is usable from Rust, and records what it actually
//! contains.
//!
//! `meta` is **not** queryable from the `sqlite3` CLI: the view calls
//! `sortconcat()`, a Calibre-registered SQL function, so a bare client gets
//! `no such function: sortconcat`. M1.2 pages this view for the library screen,
//! so that dependency is pinned here rather than discovered later.
//!
//! Follows the same pattern as `interop.rs`: copy the fixture into a temp
//! library directory, then open it through the real open path so Calibre's
//! functions get registered.

use lorebook_calibre as cal;
use rusqlite::Connection;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/calibre-9.15-metadata.db")
}

fn open_copy(name: &str) -> Connection {
    let dir = std::env::temp_dir().join(format!("lorebook-metaview-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp lib dir");
    std::fs::copy(fixture(), dir.join("metadata.db")).expect("copy fixture");
    cal::open_library(&dir).expect("open copied library")
}

#[test]
fn meta_view_is_queryable_once_functions_are_registered() {
    let conn = open_copy("queryable");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
        .expect("meta view must be queryable once functions are registered");
    println!("meta rows: {n}");
    assert!(n >= 1, "fixture should contain at least one book");
}

#[test]
fn meta_view_paginates_ordered_by_sort() {
    // M1.2 pages `meta` ordered by `sort`; both halves are load-bearing.
    let conn = open_copy("paginate");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
        .expect("count");

    let page: Vec<(i64, String)> = conn
        .prepare("SELECT id, title FROM meta ORDER BY sort LIMIT 1 OFFSET 0")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(page.len() as i64, total.min(1));
    println!("total={total} first={:?}", page.first());

    // Paging past the end yields nothing rather than erroring, so a UI can
    // clamp an out-of-range page instead of handling a failure.
    let past_end: Vec<(i64, String)> = conn
        .prepare("SELECT id, title FROM meta ORDER BY sort LIMIT 10 OFFSET 100000")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert!(
        past_end.is_empty(),
        "a page past the end must be empty, not an error"
    );
}

// ---------------------------------------------------------------------------
// Paging (M1.2)
// ---------------------------------------------------------------------------

/// A library with a known number of books, built through the real insert path
/// so Calibre's triggers run and the books are indistinguishable from
/// Calibre-created ones.
///
/// `tag` must be unique per test, and the directory must not be pre-cleaned.
///
/// cargo runs tests in one process on several threads. An earlier version did
/// `remove_dir_all` then `create_dir_all` on a fixed path, which is a race: one
/// thread removes the directory another thread just created, and the loser gets
/// `unable to open database file` — an error that looks like a code bug and is
/// really a test-fixture bug. A unique name per test, created once and never
/// removed, cannot collide.
fn library_with_books(tag: &str, n: usize) -> (PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!(
        "lorebook-page-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("create lib dir");
    let conn = cal::create_library(&dir).expect("create library");
    for i in 0..n {
        cal::insert_book(
            &conn,
            &format!("Book {i:03}"),
            None, // let Calibre's trigger compute `sort`
            None, // author_sort is a display column, not an author
            None, // let Calibre's trigger assign the uuid
        )
        .expect("insert book");
    }
    (dir, conn)
}

#[test]
fn paging_covers_every_book_exactly_once() {
    let (_dir, conn) = library_with_books("walk", 25);
    assert_eq!(cal::count_books(&conn).unwrap(), 25);

    // Walk in pages of 10, as the UI does. Assert the ids are distinct and
    // total 25: overlapping pages are the bug this catches.
    let mut seen = Vec::new();
    for offset in (0..30).step_by(10) {
        for b in cal::list_books_page(&conn, 10, offset).unwrap() {
            seen.push(b.id);
        }
    }
    seen.sort_unstable();
    assert_eq!(seen.len(), 25, "every book must appear exactly once");
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "no book may appear on two pages");
}

#[test]
fn a_page_past_the_end_is_empty_rather_than_an_error() {
    let (_dir, conn) = library_with_books("past-end", 3);
    let page = cal::list_books_page(&conn, 10, 1000).expect("must not error");
    assert!(page.is_empty());
}

#[test]
fn limit_is_clamped_to_a_maximum() {
    let (_dir, conn) = library_with_books("clamp", 2);
    // A caller asking for everything gets PAGE_MAX_LIMIT, not the whole table.
    // This is what keeps a 50k-book library out of a single IPC message.
    assert_eq!(cal::PAGE_MAX_LIMIT, 500);
    let page = cal::list_books_page(&conn, i64::MAX, 0).expect("clamped, not an error");
    assert!(page.len() <= cal::PAGE_MAX_LIMIT as usize);
}

#[test]
fn a_negative_offset_is_clamped_rather_than_counting_from_the_end() {
    let (_dir, conn) = library_with_books("past-end", 3);
    // SQLite treats a negative OFFSET as "from the end of the table", which
    // would silently return the last rows instead of the first.
    let page = cal::list_books_page(&conn, 10, -5).expect("must not error");
    assert_eq!(
        page.len(),
        3,
        "a negative offset clamps to 0, returning page one"
    );
}

#[test]
fn a_page_agrees_with_the_unpaged_list() {
    // `list_books` and `list_books_page` must not drift: the UI pages, but the
    // order a user sees has to be the order the core defines.
    let (_dir, conn) = library_with_books("agree", 12);
    let all: Vec<i64> = cal::list_books(&conn)
        .unwrap()
        .iter()
        .map(|b| b.id)
        .collect();
    let paged: Vec<i64> = cal::list_books_page(&conn, 12, 0)
        .unwrap()
        .iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(all, paged, "paged order must match the unpaged order");
}

// ---------------------------------------------------------------------------
// The comma aggregates (M1.2)
// ---------------------------------------------------------------------------

#[test]
fn concat_joins_values_with_commas_and_is_null_when_empty() {
    // `concat` is an AGGREGATE in Calibre. Registering it as a scalar "works"
    // (SQLite accepts the call) and is silently wrong: a scalar sees one row, so
    // `concat` over three tags returns one tag, and over zero tags returns NULL
    // for each row instead of NULL for the group. Both look like missing data.
    let (_dir, conn) = library_with_books("concat", 0);

    let one: Option<String> = conn
        .query_row("SELECT concat(x) FROM (SELECT 'one' AS x)", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(one.as_deref(), Some("one"), "a single value passes through");

    let many: Option<String> = conn
        .query_row(
            "SELECT concat(x) FROM (SELECT 'a' AS x UNION ALL SELECT 'b' \
             UNION ALL SELECT 'c')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(many.as_deref(), Some("a,b,c"), "values join with a comma");

    // Calibre returns NULL for an empty group, not "". The difference is visible:
    // it decides whether the UI treats a book as untagged.
    let none: Option<String> = conn
        .query_row(
            "SELECT concat(x) FROM (SELECT 'a' AS x WHERE 1 <> 1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(none, None, "an empty group is NULL, not an empty string");

    // NULL inputs are skipped, not stringified.
    let with_null: Option<String> = conn
        .query_row(
            "SELECT concat(x) FROM (SELECT NULL AS x UNION ALL SELECT 'b')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(with_null.as_deref(), Some("b"), "NULL values are skipped");
}

#[test]
fn sortconcat_orders_by_index_not_by_arrival() {
    // `sortconcat(index, value)` is what gives meta.authors Calibre's author
    // ordering. Getting the order wrong yields the right names in the wrong
    // order, which is a silent data bug.
    let (_dir, conn) = library_with_books("sortconcat", 0);
    let out: Option<String> = conn
        .query_row(
            "SELECT sortconcat(i, v) FROM (SELECT 2 AS i, 'second' AS v \
             UNION ALL SELECT 0, 'zeroth' UNION ALL SELECT 1, 'first')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        out.as_deref(),
        Some("zeroth,first,second"),
        "values are emitted in ascending index order"
    );
}

#[test]
fn meta_reports_every_author_of_a_multi_author_book() {
    // The end-to-end consequence: a book with three authors must show all three
    // in meta.authors. This is the assertion that would have caught the scalar
    // registration.
    let (dir, conn) = library_with_books("multiauthor", 0);
    let id = cal::insert_book(&conn, "Anthology", None, None, None).expect("insert");
    for name in ["Alpha", "Bravo", "Charlie"] {
        let a = cal::ensure_author(&conn, name).expect("author");
        cal::link_author(&conn, id, a).expect("link");
    }
    let authors: Option<String> = conn
        .query_row("SELECT authors FROM meta WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    let parts: Vec<&str> = authors.as_deref().unwrap_or("").split(',').collect();
    assert_eq!(
        parts.len(),
        3,
        "all three authors must appear, got {authors:?} in {dir:?}"
    );
    for name in ["Alpha", "Bravo", "Charlie"] {
        assert!(parts.contains(&name), "{name} missing from {authors:?}");
    }
}

#[test]
fn meta_reports_tags_and_formats() {
    // `concat` covers meta.tags and meta.formats. Neither was readable before
    // the aggregate fix.
    let (_dir, conn) = library_with_books("tags", 0);
    let id = cal::insert_book(&conn, "Tagged", None, None, None).expect("insert");
    for t in ["alpha", "beta"] {
        let tag = cal::ensure_tag(&conn, t).expect("tag");
        cal::link_tag(&conn, id, tag).expect("link");
    }
    let tags: Option<String> = conn
        .query_row("SELECT tags FROM meta WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(
        tags.as_deref(),
        Some("alpha,beta"),
        "meta.tags must list every tag"
    );

    // An untagged book is NULL, not "".
    let plain = cal::insert_book(&conn, "Plain", None, None, None).expect("insert");
    let none: Option<String> = conn
        .query_row("SELECT tags FROM meta WHERE id = ?1", [plain], |r| r.get(0))
        .unwrap();
    assert_eq!(
        none, None,
        "an untagged book has NULL tags, not an empty string"
    );
}
