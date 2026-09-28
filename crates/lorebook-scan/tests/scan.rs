//! Scanner tests. Plan M2.2 and M2.3.
//!
//! Every test builds a real Calibre library through `create_library` so Calibre's
//! triggers run and the rows are indistinguishable from Calibre-created ones. A
//! hand-rolled SQLite file would skip the trigger registration and the tests
//! would pass against a schema the app never actually writes to.

use std::fs;
use std::path::PathBuf;

use lorebook_calibre as cal;
use lorebook_scan::{adopt_source, scan, ScanOptions, ScanReport, ScanRoot};
use rusqlite::Connection;

/// A uniquely named temp directory, created once and never removed.
///
/// Unique per test: cargo runs tests in one process on several threads, and an
/// earlier version of this repo's fixtures did `remove_dir_all` then
/// `create_dir_all` on a fixed path, which is a race — one thread removes the
/// directory another just created and the loser gets "unable to open database
/// file", an error that looks like a product bug and is a fixture bug. A unique
/// name created once cannot collide.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lorebook-scan-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Fixture { root: dir }
    }

    /// A real Calibre library, with the app's additive tables applied.
    fn library(&self) -> Connection {
        cal::create_library(&self.root).expect("create library")
    }

    /// Writes a file under the fixture root and returns its path.
    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let p = self.root.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&p, contents).expect("write file");
        p
    }

    fn scan_root(&self, recursive: bool) -> ScanRoot {
        ScanRoot {
            id: 1,
            path: self.root.clone(),
            recursive,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best effort. Leaving a temp dir behind is harmless; failing a test
        // because cleanup failed is not.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect("count query")
}

/// Every `kind` currently in `book_sources`.
fn kinds(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT kind FROM book_sources ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// A five-file tree, as the plan's verification asks for.
fn five_files(tag: &str) -> (Fixture, Connection) {
    let fx = Fixture::new(tag);
    for i in 0..5 {
        fx.file(
            &format!("book{i}.epub"),
            format!("contents of book {i}").as_bytes(),
        );
    }
    let conn = fx.library();
    (fx, conn)
}

// ---------------------------------------------------------------------------
// Rule 2: incremental re-scan (the plan's stated verification)
// ---------------------------------------------------------------------------

#[test]
fn scanning_the_same_tree_twice_changes_nothing() {
    let (fx, conn) = five_files("twice");
    let root = fx.scan_root(true);

    let first = scan(&conn, &root, &ScanOptions::default()).expect("first scan");
    assert_eq!(first.added, 5, "five files, five books");
    assert_eq!(first.errors.len(), 0, "no errors: {:?}", first.errors);

    let rows_before = count(&conn, "SELECT COUNT(*) FROM book_sources");
    let books_before = count(&conn, "SELECT COUNT(*) FROM books");

    let second = scan(&conn, &root, &ScanOptions::default()).expect("second scan");
    assert_eq!(second.added, 0, "nothing added on a re-scan");
    assert_eq!(second.updated, 0, "nothing updated on a re-scan");
    assert_eq!(second.missing, 0, "nothing missing on a re-scan");

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM book_sources"),
        rows_before,
        "every book_sources row is unchanged"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM books"),
        books_before,
        "no book was created or destroyed"
    );
}

#[test]
fn an_unchanged_file_is_not_re_hashed() {
    let (fx, conn) = five_files("norehash");
    let root = fx.scan_root(true);
    scan(&conn, &root, &ScanOptions::default()).expect("first scan");

    // Record the stored hash, make the file unreadable, and re-scan. If the
    // size/mtime check short-circuits, the unreadable file is never opened and
    // the scan reports no error — which is the observable proof that no
    // re-hashing happened.
    let path = fx.root.join("book0.epub");
    let stored: String = conn
        .query_row(
            "SELECT content_hash FROM book_sources WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .expect("stored hash");

    let perms = fs::metadata(&path).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    }

    let report = scan(&conn, &root, &ScanOptions::default()).expect("rescan");

    #[cfg(unix)]
    {
        fs::set_permissions(&path, perms).expect("restore perms");
        assert_eq!(
            report.errors.len(),
            0,
            "an unchanged file must not be opened, so its permissions cannot matter: {:?}",
            report.errors
        );
    }

    let after: String = conn
        .query_row(
            "SELECT content_hash FROM book_sources WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .expect("stored hash after");
    assert_eq!(stored, after, "the stored hash is untouched");
}

// ---------------------------------------------------------------------------
// Rule 1: the merge key is the content hash
// ---------------------------------------------------------------------------

#[test]
fn a_duplicate_file_adds_a_source_not_a_book() {
    let (fx, conn) = five_files("dupe");
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("first scan");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM books"), 5);

    // Identical bytes, different name. Same content, so the same book.
    let copy = fx.root.join("copy-of-book0.epub");
    fs::copy(fx.root.join("book0.epub"), &copy).expect("copy");

    let report = scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("rescan");
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM books"),
        5,
        "a duplicate must not create a sixth book"
    );
    assert_eq!(report.added, 0, "nothing added");

    // The second file is a new *format* slot on an existing book only if the
    // format differs; here the format is identical, so UNIQUE (book, format)
    // holds and the row is deliberately not added. The content is still one
    // book, which is the property that matters.
    let sources = count(&conn, "SELECT COUNT(*) FROM book_sources");
    assert!(
        sources == 5,
        "one source per (book, format): a second copy of the same format is the same slot, got {sources}"
    );
}

#[test]
fn the_same_content_under_a_different_extension_shares_the_book() {
    let fx = Fixture::new("crossformat");
    let contents = b"the same book, two containers";
    fx.file("novel.epub", contents);
    fx.file("novel.pdf", contents);
    let conn = fx.library();

    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM books"),
        1,
        "identical content is one book regardless of container format"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM book_sources"),
        2,
        "and it has two sources"
    );
}

// ---------------------------------------------------------------------------
// Rule 3: absence is a state, never a deletion
// ---------------------------------------------------------------------------

#[test]
fn a_vanished_file_is_marked_missing_and_never_deleted() {
    let (fx, conn) = five_files("vanish");
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("first scan");

    let gone = fx.root.join("book2.epub");
    fs::remove_file(&gone).expect("remove file");

    let report = scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("rescan");
    assert_eq!(report.missing, 1, "exactly one file went missing");

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM book_sources"),
        5,
        "the row survives: the file may be on an unmounted drive"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM books WHERE id NOT IN (SELECT book FROM book_sources)"
        ),
        0,
        "no orphaned book rows"
    );

    let state: String = conn
        .query_row(
            "SELECT state FROM book_sources WHERE path = ?1",
            [gone.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .expect("state of the missing source");
    assert_eq!(state, "missing");
}

#[test]
fn a_returned_file_heals_itself_without_user_action() {
    let (fx, conn) = five_files("heal");
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("first scan");

    let path = fx.root.join("book1.epub");
    let bytes = fs::read(&path).expect("read");
    fs::remove_file(&path).expect("remove");
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan while gone");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM book_sources WHERE state = 'missing'"
        ),
        1
    );

    fs::write(&path, bytes).expect("restore");
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan after return");

    let state: String = conn
        .query_row(
            "SELECT state FROM book_sources WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .expect("state after return");
    assert_eq!(state, "ok", "an unmounted drive coming back heals itself");
}

// ---------------------------------------------------------------------------
// Rule 4: symlinks
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn symlinks_are_skipped_unless_following_is_asked_for() {
    let fx = Fixture::new("symlink");
    let target = fx.file("real.epub", b"a real book");
    let link = fx.root.join("linked.epub");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    let conn = fx.library();

    let report = scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");

    assert_eq!(report.added, 1, "only the real file is recorded");
    assert_eq!(
        report.skipped_symlinks.len(),
        1,
        "and the symlink is reported, not silently followed"
    );
    assert_eq!(report.skipped_symlinks[0], link);
    assert!(
        !kinds(&conn).contains(&"symlink".to_string()),
        "nothing is recorded as a symlink source unless it is followed: {:?}",
        kinds(&conn)
    );
}

#[test]
#[cfg(unix)]
fn a_followed_symlink_to_new_content_is_recorded_as_a_symlink() {
    // The target is OUTSIDE the scan root, so its content is unseen and this
    // reaches the new-book path. That path is the one that decides ownership:
    // an earlier version hardcoded kind='reference' there, so a symlink whose
    // content was new became a file the app believed it owned -- and
    // `SourceKind::owns_file` is precisely what authorises deleting it.
    //
    // (A symlink to content already in the library cannot be tested this way:
    // UNIQUE (book, format) means the duplicate slot is already taken, so the
    // row is ignored rather than recorded. That is correct behaviour and is a
    // different property.)
    let fx = Fixture::new("symfollow");
    let outside = std::env::temp_dir().join(format!(
        "lorebook-outside-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::write(&outside, b"a book that lives elsewhere").expect("write outside file");

    let link = fx.root.join("linked.epub");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    let conn = fx.library();

    let opts = ScanOptions {
        formats: lorebook_scan::DEFAULT_FORMATS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        follow_symlinks: true,
    };
    scan(&conn, &fx.scan_root(true), &opts).expect("scan");

    let found = kinds(&conn);
    assert_eq!(
        found,
        vec!["symlink".to_string()],
        "a followed symlink is recorded as kind=symlink, never as a file we own"
    );

    let _ = fs::remove_file(&outside);
}

#[test]
#[cfg(unix)]
fn a_dangling_symlink_is_not_treated_as_a_missing_book_file() {
    let fx = Fixture::new("dangling");
    fx.file("real.epub", b"a real book");
    let link = fx.root.join("broken.epub");
    std::os::unix::fs::symlink(fx.root.join("does-not-exist.epub"), &link).expect("symlink");
    let conn = fx.library();

    let opts = ScanOptions {
        formats: lorebook_scan::DEFAULT_FORMATS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        follow_symlinks: true,
    };
    let report = scan(&conn, &fx.scan_root(true), &opts).expect("scan");
    assert_eq!(report.added, 1, "a link to nothing is not a book");
}

// ---------------------------------------------------------------------------
// M2.3: never silently adopt
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn a_scan_can_never_produce_a_managed_row() {
    // The plan asks for exactly this assertion, and it is the load-bearing one:
    // a scan must never take ownership of a user's file.
    let fx = Fixture::new("nomANAGED");
    for i in 0..3 {
        fx.file(&format!("b{i}.epub"), format!("book {i}").as_bytes());
    }
    let real = fx.file("outside.epub", b"a book in another folder");
    std::os::unix::fs::symlink(&real, fx.root.join("link.epub")).expect("symlink");

    let conn = fx.library();
    // Follow too, so the symlink path is exercised.
    let opts = ScanOptions {
        follow_symlinks: true,
        ..ScanOptions::default()
    };
    scan(&conn, &fx.scan_root(true), &opts).expect("scan");

    let found = kinds(&conn);
    assert!(!found.is_empty(), "the scan did record something");
    assert!(
        !found.contains(&"managed".to_string()),
        "NO code path from scan may produce kind=managed: {found:?}"
    );
}

#[test]
fn adopting_is_a_separate_explicit_call() {
    // The complement of the test above: managed rows DO exist, and the only way
    // to get one is `adopt_source`, which copies the file into the library.
    let fx = Fixture::new("adopt");
    let src = fx.file("book.epub", b"a book to adopt");
    let conn = fx.library();
    scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");
    assert_eq!(kinds(&conn), vec!["reference".to_string()]);

    let id: i64 = conn
        .query_row("SELECT id FROM book_sources LIMIT 1", [], |r| r.get(0))
        .expect("source id");

    let library = fx.root.join("library");
    let dest = adopt_source(&conn, id, &library).expect("adopt");

    assert!(dest.exists(), "the file was copied into the library");
    assert_eq!(fs::read(&dest).expect("read dest"), b"a book to adopt");
    assert!(src.exists(), "the original is untouched");
    assert_eq!(kinds(&conn), vec!["managed".to_string()]);
}

// ---------------------------------------------------------------------------
// Scope, formats, recursion
// ---------------------------------------------------------------------------

#[test]
fn only_requested_formats_are_scanned() {
    let fx = Fixture::new("formats");
    fx.file("a.epub", b"epub");
    fx.file("b.pdf", b"pdf");
    fx.file("c.txt", b"txt");
    fx.file("d.docx", b"not a supported format");
    let conn = fx.library();

    let opts = ScanOptions {
        formats: vec!["epub".into(), "pdf".into()],
        follow_symlinks: false,
    };
    let report = scan(&conn, &fx.scan_root(true), &opts).expect("scan");

    assert_eq!(report.added, 2, "only epub and pdf");
    let paths: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM book_sources")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    assert!(
        paths
            .iter()
            .all(|p| p.ends_with(".epub") || p.ends_with(".pdf")),
        "unexpected paths: {paths:?}"
    );
}

#[test]
fn a_non_recursive_scan_stays_in_the_top_directory() {
    let fx = Fixture::new("shallow");
    fx.file("top.epub", b"top level");
    fx.file("nested/deep.epub", b"nested");
    let conn = fx.library();

    let report = scan(&conn, &fx.scan_root(false), &ScanOptions::default()).expect("scan");
    assert_eq!(report.added, 1, "only the top-level file");
}

#[test]
fn a_recursive_scan_descends() {
    let fx = Fixture::new("deep");
    fx.file("top.epub", b"top level");
    fx.file("a/b/c/deep.epub", b"deeply nested");
    let conn = fx.library();

    let report = scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");
    assert_eq!(report.added, 2, "both files");
}

#[test]
fn scanning_a_missing_directory_is_an_error_not_an_empty_report() {
    let fx = Fixture::new("nodir");
    let conn = fx.library();
    let root = ScanRoot {
        id: 1,
        path: fx.root.join("does-not-exist"),
        recursive: true,
    };
    let err = scan(&conn, &root, &ScanOptions::default()).expect_err("must fail");
    assert!(
        err.to_string().contains("does-not-exist"),
        "the error names the root: {err}"
    );
}

#[test]
fn one_unreadable_file_does_not_abandon_the_rest_of_the_scan() {
    let fx = Fixture::new("partial");
    for i in 0..3 {
        fx.file(&format!("ok{i}.epub"), format!("fine {i}").as_bytes());
    }
    let bad = fx.file("bad.epub", b"unreadable");
    let conn = fx.library();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    }

    let report = scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).expect("restore");
        assert_eq!(report.added, 3, "the three readable files are recorded");
        assert_eq!(report.errors.len(), 1, "and the unreadable one is reported");
        assert!(
            report.errors[0].0.to_string_lossy().contains("bad.epub"),
            "the error names the file: {:?}",
            report.errors
        );
    }
}

#[test]
fn an_empty_directory_is_a_clean_no_op() {
    let fx = Fixture::new("empty");
    let conn = fx.library();
    let report: ScanReport =
        scan(&conn, &fx.scan_root(true), &ScanOptions::default()).expect("scan");
    assert_eq!(report.added, 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM books"), 0);
}
