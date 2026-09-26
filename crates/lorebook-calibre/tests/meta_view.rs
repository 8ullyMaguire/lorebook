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
