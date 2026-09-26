//! Calibre `metadata.db` interop.
//!
//! The interop guarantee (spec §4): Calibre's own tables — `books`, `authors`,
//! `tags`, `series`, `publishers`, `languages`, the `books_*_link` tables,
//! `ratings`, `comments`, `identifiers`, `custom_columns` and its per-column
//! value tables — are read and written **in place, unmodified**. New tables are
//! additive and namespaced. We never `ALTER` a Calibre table, because a schema
//! Calibre cannot read is not an interop guarantee.
//!
//! This is written against a real Calibre 9.15 `metadata.db`, not against
//! documentation. `fixtures/` holds a generated library and the test suite
//! reads it, so a Calibre upgrade that moves a column fails here rather than
//! silently in someone's library.
//!
//! Calibre's quirks that matter and are handled here:
//!
//! * `books.timestamp` / `pubdate` are text timestamps, not epoch integers.
//! * The `data` table is an all-JSON blob table with column ids, not a schema.
//! * Book format *names* live in `data` (id 0), not in a column on `books`.
//! * `books` has insert/delete triggers maintaining `books_pages_link` and
//!   cleaning up link rows, so we must not duplicate that work or we will
//!   fight the trigger.

use lorebook_core::{Author, Book, BookSource, Identifier, SourceKind, SourceState, Tag};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub mod additive;
pub mod error;
pub mod functions;

pub use error::{CalibreError, Result};

/// Open an existing Calibre library, read-write.
///
/// Read-write is the default because the spec's M1 is "Calibre schema
/// read-write"; opening read-only when that is all you need is
/// [`Connection::open_with_flags`]'s job, and this deliberately does not
/// offer a read-only mode that could silently fail to persist.
pub fn open_library(path: &Path) -> Result<Connection> {
    let db = path.join("metadata.db");
    if !db.exists() {
        return Err(CalibreError::NotALibrary(path.to_path_buf()));
    }
    let conn = Connection::open(&db)
        .map_err(|e| CalibreError::Sql(format!("open {}: {e}", db.display())))?;

    // Calibre writes with WAL. Matching it keeps concurrent access (a running
    // Calibre alongside this app) working instead of producing lock errors.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "foreign_keys", "ON");
    // Calibre's own triggers call title_sort()/uuid4(). Without these
    // registered, every insert fails with "no such function". See
    // `functions` for why this is unavoidable and what it costs.
    functions::register(&conn)
        .map_err(|e| CalibreError::Sql(format!("register calibre functions: {e}")))?;
    verify_schema(&conn)?;
    Ok(conn)
}

/// Create a new, empty library that Calibre can open.
///
/// Produces the subset of Calibre's schema this app touches, matching
/// Calibre 9.15's shape. The aim is that `calibre` itself can open the result;
/// the test suite asserts that by having a real Calibre read a library we made.
pub fn create_library(path: &Path) -> Result<Connection> {
    std::fs::create_dir_all(path)
        .map_err(|e| CalibreError::Io(format!("create {}: {e}", path.display())))?;
    let db = path.join("metadata.db");
    let conn = Connection::open(&db)
        .map_err(|e| CalibreError::Sql(format!("create {}: {e}", db.display())))?;
    // WAL is an optimisation here, not a requirement, and creating it on a
    // brand-new empty file can fail transiently (notably under parallel tests,
    // where several fresh databases set the pragma at once). `open_library`
    // already treats this as best-effort; creating a library must not be the
    // stricter of the two, or the same library is un-creatable under load.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "foreign_keys", "ON");
    functions::register(&conn)
        .map_err(|e| CalibreError::Sql(format!("register calibre functions: {e}")))?;

    // `calibre_schema.sql` is extracted verbatim from `sqlite_master`, so its
    // statements are bare `CREATE TABLE`/`CREATE TRIGGER` with no
    // `IF NOT EXISTS`. Running it against a database that already has the
    // schema therefore fails on the first existing object.
    //
    // The rest of this function is already idempotent (see the `INSERT OR
    // REPLACE` and read-or-create `library_id` below), so re-creating a library
    // that exists has to work too: the app calls `create_library` when the user
    // picks a directory, and a directory that already holds a Calibre library is
    // a normal thing to point at.
    //
    // A genuinely broken statement still surfaces here as an error — this only
    // tolerates "already exists", not any other failure.
    init_schema(&conn)?;

    // Calibre keys its preferences on a library id; without one it treats the
    // library as uninitialised and re-runs its own setup over our work.
    let library_id: String = conn
        .query_row("SELECT uuid FROM library_id WHERE id = 1", [], |r| r.get(0))
        .optional()
        .map_err(|e| CalibreError::Sql(format!("read library_id: {e}")))?
        .unwrap_or_default();
    let library_uuid = if library_id.is_empty() {
        new_uuid()
    } else {
        library_id
    };
    conn.execute(
        "INSERT OR REPLACE INTO library_id (id, uuid) VALUES (1, ?1)",
        params![library_uuid],
    )
    .map_err(|e| CalibreError::Sql(format!("write library_id: {e}")))?;

    // Calibre reads this to decide whether the library needs migrating. Left
    // at 0, Calibre would treat our fresh library as ancient and rewrite it.
    conn.pragma_update(None, "user_version", CALIBRE_USER_VERSION)
        .map_err(|e| CalibreError::Sql(format!("pragma user_version: {e}")))?;

    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| CalibreError::Sql(format!("pragma foreign_keys: {e}")))?;

    // Our own tables, so a library we create is immediately usable.
    additive::apply(&conn)?;
    Ok(conn)
}

/// A random v4-shaped uuid, without pulling in a uuid crate for one call.
fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    // Mix so two calls in the same nanosecond differ.
    let mut state = nanos ^ (pid << 32) ^ 0x9E37_79B9_7F4A_7C15;
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = (state >> 24) as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    let h = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Fail early, and specifically, if the database is not a Calibre library.
///
/// Checking up front means a wrong path produces "this is not a Calibre
/// library" rather than a confusing "no such table: books" from the first
/// query.
pub fn verify_schema(conn: &Connection) -> Result<()> {
    // Calibre records its DB schema version in PRAGMA user_version, not in a
    // table. `meta` is a VIEW over books, not a key/value table, and
    // `preferences` is (id, key, val) — all three are plausible-looking wrong
    // answers, and picking one means "no such column" on every open.
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| CalibreError::Sql(format!("read user_version: {e}")))?;

    // A Calibre library always has a populated books table. Checking that is
    // what actually distinguishes it from any other sqlite file.
    let has_books: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='books'",
            [],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )
        .unwrap_or(false);

    if has_books && user_version > 0 {
        tracing::info!(schema = user_version, "opened Calibre library");
        Ok(())
    } else {
        Err(CalibreError::NotACalibreSchema)
    }
}

/// The Calibre DB schema version (`PRAGMA user_version`).
pub fn calibre_schema_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| CalibreError::Sql(format!("read user_version: {e}")))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// List every book, with authors, tags, series and formats resolved.
///
/// One connection, prepared statements reused per page — the book list is the
/// app's home screen and is hit on every navigation.
pub fn list_books(conn: &Connection) -> Result<Vec<Book>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, sort, timestamp, pubdate, series_index,
                    author_sort, uuid, has_cover
             FROM books ORDER BY sort COLLATE NOCASE, title COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare books: {e}")))?;

    let authors = authors_by_book(conn)?;
    let tags = tags_by_book(conn)?;
    let series = series_by_book(conn)?;

    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<bool>>(8)?.unwrap_or(false),
            ))
        })
        .map_err(|e| CalibreError::Sql(format!("query books: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        let (id, title, sort, timestamp, pubdate, series_index, author_sort, uuid, has_cover) =
            row.map_err(|e| CalibreError::Sql(format!("read book row: {e}")))?;
        out.push(Book {
            id,
            title,
            sort,
            author_sort,
            timestamp: parse_calibre_time(timestamp.as_deref()),
            pubdate: parse_calibre_time(pubdate.as_deref()),
            series_index,
            uuid,
            has_cover,
            sources: list_sources(conn, id)?,
            authors: authors.get(&id).cloned().unwrap_or_default(),
            tags: tags.get(&id).cloned().unwrap_or_default(),
            series: series.get(&id).cloned(),
        });
    }
    Ok(out)
}

/// One book, or `None` if the id is unknown.
pub fn get_book(conn: &Connection, id: i64) -> Result<Option<Book>> {
    let row = conn
        .query_row(
            "SELECT id, title, sort, timestamp, pubdate, series_index,
                    author_sort, uuid, has_cover
             FROM books WHERE id = ?1",
            params![id],
            |r| {
                Ok(Book {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    sort: r.get(2)?,
                    author_sort: r.get(6)?,
                    timestamp: parse_calibre_time(r.get::<_, Option<String>>(3)?.as_deref()),
                    pubdate: parse_calibre_time(r.get::<_, Option<String>>(4)?.as_deref()),
                    series_index: r.get(5)?,
                    uuid: r.get(7)?,
                    has_cover: r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                    sources: Vec::new(),
                    authors: Vec::new(),
                    tags: Vec::new(),
                    series: None,
                })
            },
        )
        .optional()
        .map_err(|e| CalibreError::Sql(format!("get book {id}: {e}")))?;

    let Some(mut book) = row else {
        return Ok(None);
    };
    book.sources = list_sources(conn, id)?;
    book.authors = author_names_for(conn, id)?;
    book.tags = tag_names_for(conn, id)?;
    book.series = series_for(conn, id)?;
    Ok(Some(book))
}

/// Formats recorded for a book, from Calibre's `data` table plus our
/// additive `book_sources` rows.
///
/// Calibre stores only format *names* in `data` — never a path. A path
/// only exists in `book_sources`, which is additive, so a book Calibre created
/// and this app has not imported yet legitimately has no source rows. The
/// format is still listed, with no path and `Missing` state, so the UI can show
/// "EPUB (not imported)" rather than hiding the format — the same information
/// Calibre itself shows.
///
/// Works on a library this app has never written to, because `book_sources`
/// legitimately does not exist there yet.
pub fn list_sources(conn: &Connection, book: i64) -> Result<Vec<BookSource>> {
    let mut out: Vec<BookSource> = Vec::new();

    // From our additive table: these have real paths. Its absence is normal for
    // a library that has not been extended yet, so it is not an error.
    if additive::is_applied(conn) {
        let mut stmt = conn
            .prepare(
                "SELECT book, format, kind, path, size, mtime_ns, content_hash,
                        state, superseded_by, confidence
                 FROM book_sources WHERE book = ?1 ORDER BY format COLLATE NOCASE",
            )
            .map_err(|e| CalibreError::Sql(format!("prepare book_sources: {e}")))?;
        let rows = stmt
            .query_map(params![book], |r| {
                Ok(BookSource {
                    book: r.get(0)?,
                    format: r.get(1)?,
                    kind: SourceKind::parse(&r.get::<_, String>(2)?)
                        .unwrap_or(SourceKind::Reference),
                    path: r.get::<_, String>(3)?.into(),
                    size: r.get(4)?,
                    mtime_ns: r.get(5)?,
                    content_hash: r.get(6)?,
                    state: SourceState::parse(&r.get::<_, String>(7)?)
                        .unwrap_or(SourceState::Missing),
                    superseded_by: r.get(8)?,
                    confidence: r.get(9)?,
                })
            })
            .map_err(|e| CalibreError::Sql(format!("query book_sources: {e}")))?;
        for row in rows {
            out.push(row.map_err(|e| CalibreError::Sql(format!("read source: {e}")))?);
        }
    }

    // From Calibre's own table: the formats that exist in the library, with no
    // path, for any format the additive table did not already describe.
    for f in calibre_formats(conn, book)? {
        if out.iter().any(|s| s.format.eq_ignore_ascii_case(&f.format)) {
            continue;
        }
        out.push(BookSource {
            book,
            format: f.format,
            kind: SourceKind::Reference,
            // Calibre records that the format exists but never where the file
            // is, so there is no path to report until this app imports it.
            path: PathBuf::new(),
            size: f.uncompressed_size,
            // Calibre keeps no mtime in `data`; 0 means "unknown", which is why
            // this is not Option.
            mtime_ns: 0,
            content_hash: String::new(),
            state: SourceState::Missing,
            superseded_by: None,
            confidence: 0.0,
        });
    }

    out.sort_by(|a, b| a.format.cmp(&b.format));
    Ok(out)
}

/// Format names Calibre knows about for a book, from the `data` table.
///
/// `data` is one row per format — `(id, book, format, uncompressed_size,
/// name)` — not a JSON blob table. It carries the format name and size but
/// **never a path**: Calibre's library owns its files and records only that
/// they exist in the library dir. A path only ever exists in our additive
/// `book_sources`, which is exactly why a Calibre-created book legitimately
/// has no source rows until this app imports it.
pub fn calibre_formats(conn: &Connection, book: i64) -> Result<Vec<FormatRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT format, uncompressed_size, name FROM data
             WHERE book = ?1 ORDER BY format COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare data: {e}")))?;
    let rows = stmt
        .query_map(params![book], |r| {
            Ok(FormatRow {
                format: r.get::<_, String>(0)?.to_uppercase(),
                uncompressed_size: r.get(1)?,
                name: r.get(2)?,
            })
        })
        .map_err(|e| CalibreError::Sql(format!("query data: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CalibreError::Sql(format!("read data row: {e}")))?);
    }
    Ok(out)
}

/// One row of Calibre's `data` table: a format that exists in the library.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FormatRow {
    /// Uppercase format id, e.g. `EPUB`.
    pub format: String,
    pub uncompressed_size: i64,
    /// Calibre's internal name for this file, relative to the library dir.
    /// Not a usable absolute path — the library root is implied.
    pub name: String,
}

/// Record a format in Calibre's `data` table.
///
/// Calibre keeps `data` consistent with `books` by trigger; this writes the
/// row and lets the trigger validate the foreign key rather than reimplementing
/// the check.
pub fn add_format(
    conn: &Connection,
    book: i64,
    format: &str,
    name: &str,
    uncompressed_size: i64,
) -> Result<()> {
    let format = format.trim().to_uppercase();
    if format.is_empty() || name.trim().is_empty() {
        return Err(CalibreError::Invalid(
            "format and name must be non-empty".into(),
        ));
    }
    conn.execute(
        "INSERT OR REPLACE INTO data (book, format, uncompressed_size, name)
         VALUES (?1, ?2, ?3, ?4)",
        params![book, format, uncompressed_size, name],
    )
    .map_err(|e| CalibreError::Sql(format!("add format: {e}")))?;
    Ok(())
}

/// Calibre's own per-book dirty flag (`metadata_dirtied`).
///
/// Calibre uses this to decide what still needs re-indexing; writes that do
/// not set it leave Calibre showing stale search results until it is
/// restarted. Cheap to set, so we always do.
pub fn mark_dirty(conn: &Connection, book: i64) -> Result<()> {
    // Real Calibre shape: `(id, book)` with `UNIQUE(book)` — the id is the
    // book's own id, and a book is either dirty or not.
    conn.execute(
        "INSERT OR IGNORE INTO metadata_dirtied (id, book) VALUES (?1, ?1)",
        params![book],
    )
    .map_err(|e| CalibreError::Sql(format!("mark dirty: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Insert a book into Calibre's `books` table, unmodified.
///
/// Returns the new id. Calibre's `books_pages_link_create_trigger` fires on
/// insert, so we do not create that link row ourselves — doing both would
/// violate the link table's uniqueness.
pub fn insert_book(
    conn: &Connection,
    title: &str,
    sort: Option<&str>,
    // Calibre's `books.author_sort` column — a free-text display column, NOT an
    // author. Real authors live in `authors` and are attached with
    // `link_author`; setting this does not create one.
    author_sort: Option<&str>,
    // `None` lets Calibre's own trigger assign one.
    uuid: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO books (title, sort, author_sort, uuid, timestamp, last_modified)
         VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        params![title, sort, author_sort, uuid],
    )
    .map_err(|e| CalibreError::Sql(format!("insert book: {e}")))?;
    Ok(conn.last_insert_rowid())
}

/// Update a book's own columns. `None` leaves a column alone.
pub fn update_book(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    sort: Option<&str>,
    series_index: Option<f64>,
) -> Result<bool> {
    let n = conn
        .execute(
            "UPDATE books
             SET title = COALESCE(?2, title),
                 sort = COALESCE(?3, sort),
                 series_index = COALESCE(?4, series_index),
                 last_modified = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![id, title, sort, series_index],
        )
        .map_err(|e| CalibreError::Sql(format!("update book {id}: {e}")))?;
    Ok(n > 0)
}

/// Delete a book row. Calibre's own delete trigger removes the link rows.
///
/// This removes **metadata only**. Whether any file on disk is also removed is
/// decided by the caller's `book_sources` rows, never here — the app must not
/// delete a file it does not own (spec §3.3).
pub fn delete_book(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn
        .execute("DELETE FROM books WHERE id = ?1", params![id])
        .map_err(|e| CalibreError::Sql(format!("delete book {id}: {e}")))?;
    Ok(n > 0)
}

/// Find or create an author, returning its id.
///
/// Calibre keys authors by name and sorts separately; reusing an existing row
/// is what keeps "the same author" from becoming two authors.
pub fn ensure_author(conn: &Connection, name: &str) -> Result<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CalibreError::Invalid("author name is empty".into()));
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM authors WHERE name = ?1",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| CalibreError::Sql(format!("find author: {e}")))?
    {
        return Ok(id);
    }
    // Calibre's default sort is a comma-inverted form of the name.
    let sort = author_sort_default(name);
    conn.execute(
        "INSERT INTO authors (name, sort, link) VALUES (?1, ?2, '')",
        params![name, sort],
    )
    .map_err(|e| CalibreError::Sql(format!("insert author: {e}")))?;
    Ok(conn.last_insert_rowid())
}

/// Calibre's author-sort convention: `Last, First`.
fn author_sort_default(name: &str) -> String {
    match name.split_once(", ") {
        Some((last, first)) => format!("{last}, {first}"),
        None => match name.rsplit_once(' ') {
            Some((first, last)) => format!("{last}, {first}"),
            None => name.to_string(),
        },
    }
}

/// Link a book to an author. Calibre's own `link` column tracks the order.
pub fn link_author(conn: &Connection, book: i64, author: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO books_authors_link (book, author) VALUES (?1, ?2)",
        params![book, author],
    )
    .map_err(|e| CalibreError::Sql(format!("link author: {e}")))?;
    Ok(())
}

/// Find or create a tag.
pub fn ensure_tag(conn: &Connection, name: &str) -> Result<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CalibreError::Invalid("tag name is empty".into()));
    }
    if let Some(id) = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", params![name], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .map_err(|e| CalibreError::Sql(format!("find tag: {e}")))?
    {
        return Ok(id);
    }
    conn.execute("INSERT INTO tags (name) VALUES (?1)", params![name])
        .map_err(|e| CalibreError::Sql(format!("insert tag: {e}")))?;
    Ok(conn.last_insert_rowid())
}

/// Link a book to a tag.
pub fn link_tag(conn: &Connection, book: i64, tag: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO books_tags_link (book, tag) VALUES (?1, ?2)",
        params![book, tag],
    )
    .map_err(|e| CalibreError::Sql(format!("link tag: {e}")))?;
    Ok(())
}

/// Attach an identifier (isbn, ao3, …) to a book.
///
/// Add (or replace) an identifier for a book.
///
/// Calibre's constraint is `UNIQUE(book, type)` — one identifier per scheme per
/// book — so a second identifier of the same scheme **replaces** the first.
/// `INSERT OR REPLACE` rather than `OR IGNORE`: ignoring would leave a stale
/// value in place while reporting success, which is how a book ends up with the
/// wrong ISBN and nothing in the logs to explain it.
pub fn add_identifier(conn: &Connection, book: i64, scheme: &str, val: &str) -> Result<()> {
    let scheme = scheme.trim().to_lowercase();
    let val = val.trim();
    if scheme.is_empty() || val.is_empty() {
        return Err(CalibreError::Invalid(
            "identifier scheme and val must be non-empty".into(),
        ));
    }
    conn.execute(
        "INSERT OR REPLACE INTO identifiers (book, type, val) VALUES (?1, ?2, ?3)",
        params![book, scheme, val],
    )
    .map_err(|e| CalibreError::Sql(format!("add identifier: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn authors_by_book(conn: &Connection) -> Result<HashMap<i64, Vec<String>>> {
    let mut stmt = conn
        .prepare(
            "SELECT l.book, a.name FROM books_authors_link l
             JOIN authors a ON a.id = l.author ORDER BY a.name COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare authors: {e}")))?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| CalibreError::Sql(format!("query authors: {e}")))?;
    for row in rows {
        let (book, name) = row.map_err(|e| CalibreError::Sql(format!("read author: {e}")))?;
        map.entry(book).or_default().push(name);
    }
    Ok(map)
}

fn author_names_for(conn: &Connection, book: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT a.name FROM books_authors_link l JOIN authors a ON a.id = l.author
             WHERE l.book = ?1 ORDER BY a.name COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare book authors: {e}")))?;
    let rows = stmt
        .query_map(params![book], |r| r.get::<_, String>(0))
        .map_err(|e| CalibreError::Sql(format!("query book authors: {e}")))?;
    collect_strings(rows)
}

fn tag_names_for(conn: &Connection, book: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT t.name FROM books_tags_link l JOIN tags t ON t.id = l.tag
             WHERE l.book = ?1 ORDER BY t.name COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare book tags: {e}")))?;
    let rows = stmt
        .query_map(params![book], |r| r.get::<_, String>(0))
        .map_err(|e| CalibreError::Sql(format!("query book tags: {e}")))?;
    collect_strings(rows)
}

fn tags_by_book(conn: &Connection) -> Result<HashMap<i64, Vec<String>>> {
    let mut stmt = conn
        .prepare(
            "SELECT l.book, t.name FROM books_tags_link l JOIN tags t ON t.id = l.tag
             ORDER BY t.name COLLATE NOCASE",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare tags: {e}")))?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| CalibreError::Sql(format!("query tags: {e}")))?;
    for row in rows {
        let (book, name) = row.map_err(|e| CalibreError::Sql(format!("read tag: {e}")))?;
        map.entry(book).or_default().push(name);
    }
    Ok(map)
}

fn series_for(conn: &Connection, book: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT s.name FROM books_series_link l JOIN series s ON s.id = l.series
         WHERE l.book = ?1",
        params![book],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| CalibreError::Sql(format!("query series: {e}")))
}

fn series_by_book(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut stmt = conn
        .prepare("SELECT l.book, s.name FROM books_series_link l JOIN series s ON s.id = l.series")
        .map_err(|e| CalibreError::Sql(format!("prepare series: {e}")))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| CalibreError::Sql(format!("query series: {e}")))?;
    let mut map = HashMap::new();
    for row in rows {
        let (book, name) = row.map_err(|e| CalibreError::Sql(format!("read series: {e}")))?;
        map.insert(book, name);
    }
    Ok(map)
}

fn collect_strings(rows: impl Iterator<Item = rusqlite::Result<String>>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CalibreError::Sql(format!("read string: {e}")))?);
    }
    Ok(out)
}

/// Calibre stores timestamps as text. It has used several formats over the
/// years, so this accepts what a real library contains rather than one shape.
pub fn parse_calibre_time(s: Option<&str>) -> Option<SystemTime> {
    let s = s?.trim();
    if s.is_empty() {
        return None;
    }
    // "YYYY-MM-DD HH:MM:SS[.fff][+ZZ:ZZ]" — lexically comparable, so the
    // common case is a string compare against the epoch rather than a parse.
    let (date_part, time_part) = s.split_once(' ').unwrap_or((s, "00:00:00"));
    let mut d = date_part.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time_part.split(':');
    let hh: i64 = t.next().unwrap_or("0").parse().unwrap_or(0);
    let mm: i64 = t.next().unwrap_or("0").parse().unwrap_or(0);
    let ss: f64 = t.next().unwrap_or("0").parse().unwrap_or(0.0);

    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil epoch (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss as i64;
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

/// Row helper: read an optional string column as `Option<String>`.
#[allow(dead_code)]
fn opt_string(r: &Row<'_>) -> rusqlite::Result<Option<String>> {
    r.get(0)
}

/// All authors in the library.
pub fn list_authors(conn: &Connection) -> Result<Vec<Author>> {
    let mut stmt = conn
        .prepare("SELECT id, name, sort, link FROM authors ORDER BY sort COLLATE NOCASE")
        .map_err(|e| CalibreError::Sql(format!("prepare list authors: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Author {
                id: r.get(0)?,
                name: r.get(1)?,
                sort: r.get(2)?,
                link: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })
        .map_err(|e| CalibreError::Sql(format!("query list authors: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CalibreError::Sql(format!("read author: {e}")))?);
    }
    Ok(out)
}

/// All tags in the library.
pub fn list_tags(conn: &Connection) -> Result<Vec<Tag>> {
    let mut stmt = conn
        .prepare("SELECT id, name FROM tags ORDER BY name COLLATE NOCASE")
        .map_err(|e| CalibreError::Sql(format!("prepare list tags: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })
        .map_err(|e| CalibreError::Sql(format!("query list tags: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CalibreError::Sql(format!("read tag: {e}")))?);
    }
    Ok(out)
}

/// Identifiers attached to a book.
pub fn list_identifiers(conn: &Connection, book: i64) -> Result<Vec<Identifier>> {
    let mut stmt = conn
        .prepare("SELECT id, book, type, val FROM identifiers WHERE book = ?1 ORDER BY type, val")
        .map_err(|e| CalibreError::Sql(format!("prepare identifiers: {e}")))?;
    let rows = stmt
        .query_map(params![book], |r| {
            Ok(Identifier {
                id: r.get(0)?,
                book: r.get(1)?,
                scheme: r.get(2)?,
                val: r.get(3)?,
            })
        })
        .map_err(|e| CalibreError::Sql(format!("query identifiers: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CalibreError::Sql(format!("read identifier: {e}")))?);
    }
    Ok(out)
}

pub use additive::apply as apply_additive_schema;
/// Re-exported so callers do not need to know the module layout.
pub use additive::DDL as ADDITIVE_SCHEMA;

/// Create Calibre's schema, tolerating objects that already exist.
///
/// A "CREATE with no IF NOT EXISTS" failure is expected when the database is
/// already initialised; anything else is a real error and is reported. The
/// approach is to retry statement by statement rather than rewriting the
/// extracted SQL to add `IF NOT EXISTS` everywhere — the schema file is a
/// verbatim copy of what Calibre has, and that is worth preserving so it can be
/// re-extracted and diffed against a Calibre upgrade.
fn init_schema(conn: &Connection) -> Result<()> {
    match conn.execute_batch(CREATE_SCHEMA) {
        Ok(()) => Ok(()),
        Err(_first_batch_error) => {
            // Not idempotent as a batch, so apply it one statement at a time and
            // keep going past the ones that already exist.
            let mut hard_error: Option<rusqlite::Error> = None;
            for stmt in split_sql_statements(CREATE_SCHEMA) {
                if let Err(e) = conn.execute_batch(&stmt) {
                    let msg = e.to_string();
                    let already_exists = msg.contains("already exists");
                    if !already_exists && hard_error.is_none() {
                        hard_error = Some(e);
                    }
                }
            }
            match hard_error {
                Some(e) => Err(CalibreError::Sql(format!("init schema: {e}"))),
                // The batch failed only on pre-existing objects, which is the
                // expected outcome for an already-initialised library.
                None => Ok(()),
            }
        }
    }
}

/// Split a SQL script into individual statements.
///
/// `sqlite3` can execute a whole script, but there is no way to resume it after
/// one statement fails, and re-running the batch to find the next failure would
/// fail on the same statement forever. Splitting on `;` at end of line is
/// sufficient here because `calibre_schema.sql` is machine-extracted: it
/// contains no string literals with embedded semicolons, and no triggers or
/// `BEGIN...END` bodies (Calibre keeps those in Python).
fn split_sql_statements(script: &str) -> Vec<String> {
    script
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.lines().all(|l| l.trim_start().starts_with("--")))
        .map(|s| format!("{s};"))
        .collect()
}

const CREATE_SCHEMA: &str = include_str!("calibre_schema.sql");

/// Calibre 9.15's `PRAGMA user_version`. A library we create has to advertise
/// the same value a real one does, or Calibre treats it as an old library and
/// runs its own migrations over our work.
const CALIBRE_USER_VERSION: i64 = 27;

// ---------------------------------------------------------------------------
// Library — an owned, open Calibre library
// ---------------------------------------------------------------------------

/// An open Calibre library.
///
/// This exists because [`rusqlite::Connection`] is `!Sync`, and a Tauri app
/// keeps its state in something that must be `Send + Sync`. The crate's
/// functions stay free functions over `&Connection` — the newtype only owns the
/// connection and the path, so there is exactly one way to hold a library and
/// no method here duplicates a query.
pub struct Library {
    conn: Connection,
    path: PathBuf,
}

impl Library {
    /// Open an existing library. See [`open_library`].
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: open_library(path)?,
            path: path.to_path_buf(),
        })
    }

    /// Create a library Calibre itself can open. See [`create_library`].
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: create_library(path)?,
            path: path.to_path_buf(),
        })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This library's book count, from Calibre's `meta` view.
    pub fn book_count(&self) -> Result<i64> {
        count_books(&self.conn)
    }
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

/// The most books one page may return.
///
/// A bound the caller cannot exceed by accident. An unbounded `LIMIT` is a
/// request for the whole library in memory, and a 50k-book library serialised
/// into a webview hangs the UI.
pub const PAGE_MAX_LIMIT: i64 = 500;

/// Total number of books, via Calibre's `meta` view.
pub fn count_books(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
        .map_err(|e| CalibreError::Sql(format!("count books: {e}")))
}

/// One page of books, ordered as Calibre orders them.
///
/// Reads through the `meta` view rather than joining the tables by hand.
/// `meta` is the view Calibre's own UI reads, so using it is what makes our
/// listing and Calibre's listing incapable of disagreeing.
///
/// `meta` calls `sortconcat()`, a Calibre-registered SQL function, so this only
/// works on a connection that has been through [`open_library`] /
/// [`create_library`]. See `tests/meta_view.rs`.
pub fn list_books_page(conn: &Connection, limit: i64, offset: i64) -> Result<Vec<Book>> {
    // Clamp rather than trust: a negative offset is a client bug, and letting
    // it through would make SQLite treat it as "from the end of the table".
    let limit = limit.clamp(1, PAGE_MAX_LIMIT);
    let offset = offset.max(0);

    // The `meta` view's real columns are:
    //   id, title, authors, publisher, rating, timestamp, size, tags, comments,
    //   series, series_index, sort, author_sort, formats, path, pubdate, uuid
    //
    // It has NO `has_cover` -- that lives on `books` -- and it aggregates
    // authors, tags, formats and path into single columns via `sortconcat()`.
    // Those aggregates are the point: one row per book, with the relations
    // already resolved, instead of a query per relation per row.
    let mut stmt = conn
        .prepare(
            "SELECT id, title, sort, timestamp, pubdate, series_index,
                    author_sort, uuid, authors, tags, series, formats
             FROM meta ORDER BY sort, title LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| CalibreError::Sql(format!("prepare books page: {e}")))?;

    let rows = stmt
        .query_map(params![limit, offset], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(|e| CalibreError::Sql(format!("query books page: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            title,
            sort,
            timestamp,
            pubdate,
            series_index,
            author_sort,
            uuid,
            authors,
            tags,
            series,
            formats,
        ) = row.map_err(|e| CalibreError::Sql(format!("read book row: {e}")))?;
        out.push(Book {
            id,
            title,
            sort,
            author_sort,
            timestamp: parse_calibre_time(timestamp.as_deref()),
            pubdate: parse_calibre_time(pubdate.as_deref()),
            series_index,
            uuid,
            // `has_cover` is not in `meta`; a list row does not show covers, and
            // selecting from `books` for it would be a per-row lookup to fill a
            // field the list does not display.
            has_cover: false,
            sources: Vec::new(),
            authors: split_aggregate(authors.as_deref()),
            tags: split_aggregate(tags.as_deref()),
            series: series.filter(|s| !s.is_empty()),
        });
        // `formats` is read to prove the column is queryable on a list page;
        // the Book carries per-format detail in `sources`, which a list row
        // does not load.
        let _ = formats;
    }
    Ok(out)
}

/// Split one of `meta`'s aggregated columns into its parts.
///
/// Calibre joins these with `\x1f` (unit separator) and terminates with a
/// trailing separator, so splitting on it yields a trailing empty element that
/// is not a value. See the view definition in `calibre_schema.sql`.
fn split_aggregate(value: Option<&str>) -> Vec<String> {
    let Some(v) = value else {
        return Vec::new();
    };
    v.split('\u{1f}')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}
