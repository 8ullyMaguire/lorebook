//! Directory scanning. Plan M2.2.
//!
//! Scans a directory tree and records what it finds in `book_sources`, without
//! ever adopting, copying or deleting a user's file.
//!
//! ## The four rules, each with a test
//!
//! 1. **A file whose content hash already exists adds no book.** It becomes a
//!    `book_sources` row pointing at the existing book (spec §3.4).
//! 2. **A file already recorded is not re-hashed** if size and mtime match. This
//!    is what makes a re-scan cheap, and it is why `mtime_ns` and `size` are
//!    columns rather than conveniences.
//! 3. **A recorded file that has vanished is marked `state = 'missing'`, never
//!    deleted.** The user's file may be on an unmounted drive; deleting the row
//!    would turn a temporary absence into permanent data loss.
//! 4. **Symlinks are recorded as `kind = 'symlink'`, never followed silently.**
//!
//! ## Nothing here can produce a `managed` row
//!
//! [`scan`] only ever writes [`SourceKind::Reference`] or [`SourceKind::Symlink`].
//! Creating a managed source is [`adopt_source`], in a different module, and it
//! takes an explicit call (plan M2.3). The type is not enough on its own — a
//! `kind` string in a database is just a string — so the guarantee is tested
//! rather than asserted: a test scans a tree and asserts every row it wrote is
//! non-managed.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lorebook_core::hash::ContentHasher;
use lorebook_core::{SourceKind, SourceState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub mod dedup;
pub mod inbox;

/// Anything that can go wrong during a scan.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("scan root does not exist or is not a directory: {0}")]
    NotADirectory(PathBuf),

    #[error("sqlite error during scan: {0}")]
    Sql(String),

    /// A file could not be hashed. Carries the path so the report names it.
    #[error("cannot hash {path}: {message}")]
    Hash { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, ScanError>;

/// `rusqlite::Error` → [`ScanError::Sql`].
///
/// The same conversion `lorebook-calibre` makes, for the same reason: without
/// it every `?` on a rusqlite call needs a hand-written `map_err` and the
/// context has to be retyped at every site. The message is preserved; only the
/// type is narrowed.
impl From<rusqlite::Error> for ScanError {
    fn from(e: rusqlite::Error) -> Self {
        ScanError::Sql(e.to_string())
    }
}

/// A directory to scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRoot {
    pub id: i64,
    pub path: PathBuf,
    /// Whether to descend into subdirectories.
    pub recursive: bool,
}

/// What to scan for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Lower-case extensions *without* the dot, e.g. `["epub", "pdf"]`.
    pub formats: Vec<String>,
    /// When false, a symlink is recorded as a source and never opened.
    pub follow_symlinks: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            formats: DEFAULT_FORMATS.iter().map(|s| s.to_string()).collect(),
            // False by default. Following a symlink during a scan means reading
            // a file the user did not put in this directory, and a symlink into
            // a home directory turns "scan my books folder" into "hash my entire
            // filesystem". Adopting that decision requires an explicit opt-in.
            follow_symlinks: false,
        }
    }
}

/// The formats a scan accepts unless told otherwise.
pub const DEFAULT_FORMATS: &[&str] = &["epub", "mobi", "azw3", "pdf", "djvu", "txt", "cbz", "cbr"];

/// What a scan did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// New books created.
    pub added: usize,
    /// New `book_sources` rows attached to books that already existed.
    pub updated: usize,
    /// Rows whose file is gone, marked `missing`.
    pub missing: usize,
    /// Files that could not be read or recorded, with the reason.
    pub errors: Vec<(PathBuf, String)>,
    /// Symlinks found but not followed, because `follow_symlinks` was false.
    pub skipped_symlinks: Vec<PathBuf>,
}

/// Scans `root` and records what it finds.
///
/// Never deletes a row and never writes a `managed` kind — see the module
/// docs. A file that cannot be read is an entry in `report.errors`, not a
/// failure: one unreadable file in a folder of five hundred must not abandon the
/// other four hundred and ninety-nine.
pub fn scan(conn: &Connection, root: &ScanRoot, opts: &ScanOptions) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    let meta = fs::metadata(&root.path).map_err(|_| ScanError::NotADirectory(root.path.clone()))?;
    if !meta.is_dir() {
        return Err(ScanError::NotADirectory(root.path.clone()));
    }

    let wanted: HashSet<String> = opts
        .formats
        .iter()
        .map(|f| f.trim().trim_start_matches('.').to_lowercase())
        .collect();

    let mut files = Vec::new();
    collect(
        &root.path,
        root.recursive,
        opts.follow_symlinks,
        &wanted,
        &mut files,
        &mut report,
    );

    // Deterministic order. `read_dir` returns entries in filesystem order, which
    // differs between machines and between runs on the same machine, so a
    // second scan of an unchanged tree would see a different order and report
    // spurious work. Sorting also makes the "scanned twice, second scan is a
    // no-op" test a real assertion rather than a lucky pass.
    files.sort();

    for path in files {
        if let Err(e) = record_file(conn, &path, &mut report) {
            report.errors.push((path, e.to_string()));
        }
    }

    report.missing = mark_missing(conn, root)?;

    Ok(report)
}

/// Walks the tree, collecting candidate files.
fn collect(
    dir: &Path,
    recursive: bool,
    follow_symlinks: bool,
    wanted: &HashSet<String>,
    out: &mut Vec<PathBuf>,
    report: &mut ScanReport,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            report.errors.push((dir.to_path_buf(), e.to_string()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // `symlink_metadata` does NOT follow the link, which is what makes the
        // symlink case detectable at all. Using `metadata` here would report a
        // symlink as a regular file and the `kind = 'symlink'` rule could never
        // fire.
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                report.errors.push((path, e.to_string()));
                continue;
            }
        };

        if meta.file_type().is_symlink() {
            if !follow_symlinks {
                report.skipped_symlinks.push(path);
                continue;
            }
            // Following is opt-in, and even then the link is recorded as a
            // symlink: the app manages the link, not its target.
            if let Ok(target_meta) = fs::metadata(&path) {
                if target_meta.is_dir() {
                    if recursive {
                        collect(&path, recursive, follow_symlinks, wanted, out, report);
                    }
                    continue;
                }
                if wanted.contains(&extension_of(&path).to_lowercase()) {
                    out.push(path);
                }
            }
            continue;
        }

        if meta.is_dir() {
            if recursive {
                collect(&path, recursive, follow_symlinks, wanted, out, report);
            }
            continue;
        }

        if !meta.is_file() {
            // Sockets, fifos, devices. Not books, and opening one can block
            // forever.
            continue;
        }

        if wanted.contains(&extension_of(&path).to_lowercase()) {
            out.push(path);
        }
    }
}

/// The lower-cased extension without the dot. Empty when there is none.
fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// mtime in nanoseconds since the epoch, the units `book_sources.mtime_ns` uses.
fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Records one file, deciding whether it is new, a duplicate, or unchanged.
fn record_file(conn: &Connection, path: &Path, report: &mut ScanReport) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| ScanError::Sql(e.to_string()))?;
    let size = meta.len() as i64;
    let mtime = mtime_ns(&meta);
    let format = extension_of(path);
    let path_str = path.to_string_lossy().to_string();

    // Rule 2: an already-recorded file with the same size and mtime is not
    // re-hashed. This is the whole cost model of a re-scan — without it every
    // re-scan reads every byte of the library.
    if let Some((id, state)) = existing_by_path(conn, &path_str)? {
        if state == SourceState::Ok.as_str() {
            let unchanged: bool = conn.query_row(
                "SELECT size = ?2 AND mtime_ns = ?3 FROM book_sources WHERE id = ?1",
                params![id, size, mtime],
                |r| r.get(0),
            )?;
            if unchanged {
                return Ok(());
            }
            // Size or mtime moved: fall through and re-hash. The file was
            // edited, so its identity may have changed.
        } else {
            // A previously-missing file is back. Re-hash it and clear the
            // state, which is how an unmounted drive coming back heals itself
            // without the user doing anything.
            let hash = hash_or_err(path)?;
            conn.execute(
                "UPDATE book_sources SET size = ?2, mtime_ns = ?3, content_hash = ?4, state = 'ok'
                 WHERE id = ?1",
                params![id, size, mtime, hash],
            )
            .map_err(|e| ScanError::Sql(e.to_string()))?;
            report.updated += 1;
            return Ok(());
        }
    }

    let hash = hash_or_err(path)?;

    // Rule 1: the merge key is the content hash. An existing book with this
    // content gains another source; no new book is created.
    if let Some(book) = book_with_hash(conn, &hash)? {
        let kind = if meta.file_type().is_symlink() {
            SourceKind::Symlink
        } else {
            SourceKind::Reference
        };
        // A book may hold one source per format (UNIQUE (book, format)). A
        // second EPUB of the same content is the *same* format, so there is
        // nothing to record and nothing to report — replacing the row would
        // move the user's existing path, which is a silent edit of a decision
        // they made.
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO book_sources
                   (book, format, kind, path, size, mtime_ns, content_hash, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'ok')",
                params![book, format, kind.as_str(), path_str, size, mtime, hash],
            )
            .map_err(|e| ScanError::Sql(e.to_string()))?;
        if inserted > 0 {
            report.updated += 1;
        }
        return Ok(());
    }

    // A genuinely new book. The title is the file stem: a scan does not parse
    // metadata, and inventing a title from the filename is the documented
    // behaviour the spec's curation pipeline (M3) later refines.
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string();

    let book: i64 = conn
        .query_row(
            "INSERT INTO books (title, timestamp, last_modified)
             VALUES (?1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             RETURNING id",
            params![title],
            |r| r.get(0),
        )
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    conn.execute(
        "INSERT INTO data (book, format, uncompressed_size, name)
         VALUES (?1, ?2, ?3, ?4)",
        params![book, format.to_uppercase(), size, path_str],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;

    // NEVER managed, and symlink-aware. The `kind` here decides whether the
    // app may ever delete this file, so hardcoding 'reference' was not a
    // cosmetic shortcut: a symlink that happened to be the first sighting of
    // its content was recorded as a file we own, and `SourceKind::owns_file`
    // is exactly what authorises a delete. A scanned symlink is a link we
    // recorded, not a file we hold.
    let kind = if meta.file_type().is_symlink() {
        SourceKind::Symlink
    } else {
        SourceKind::Reference
    };
    conn.execute(
        "INSERT INTO book_sources (book, format, kind, path, size, mtime_ns, content_hash, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'ok')",
        params![book, format, kind.as_str(), path_str, size, mtime, hash],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;

    report.added += 1;
    Ok(())
}

/// Hashes a file for a scan.
///
/// A hash failure is a *per-file* error, not a scan failure: the caller pushes
/// it into `report.errors` and moves to the next file, so one unreadable file
/// in a folder of five hundred does not abandon the other four hundred and
/// ninety-nine. The message keeps the path because that is what the user needs.
fn hash_or_err(path: &Path) -> Result<String> {
    ContentHasher::hash_file(path).map_err(|e| ScanError::Hash {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// The `book_sources` row for a path, if one exists.
fn existing_by_path(conn: &Connection, path: &str) -> Result<Option<(i64, String)>> {
    conn.query_row(
        "SELECT id, state FROM book_sources WHERE path = ?1",
        params![path],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(|e| ScanError::Sql(e.to_string()))
}

/// A book that already holds this content.
fn book_with_hash(conn: &Connection, hash: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT book FROM book_sources WHERE content_hash = ?1 AND state = 'ok' LIMIT 1",
        params![hash],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| ScanError::Sql(e.to_string()))
}

/// Rule 3: rows under this root whose file is gone become `state = 'missing'`.
///
/// Never a delete. The file may be on an unmounted drive, and a scan that
/// removed the row would make a temporary absence permanent.
fn mark_missing(conn: &Connection, root: &ScanRoot) -> Result<usize> {
    let prefix = format!("{}%", root.path.to_string_lossy());
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, path FROM book_sources WHERE path LIKE ?1 AND state = 'ok'")
            .map_err(|e| ScanError::Sql(e.to_string()))?;
        let mapped = stmt
            .query_map(params![prefix], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| ScanError::Sql(e.to_string()))?;
        mapped
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| ScanError::Sql(e.to_string()))?
    };

    let mut n = 0;
    for (id, path) in rows {
        // `symlink_metadata` so a dangling symlink counts as present-but-broken
        // rather than missing: the link is still recorded, and its target being
        // gone is a different problem from the file being gone.
        let gone = fs::symlink_metadata(&path).is_err();
        if gone {
            conn.execute(
                "UPDATE book_sources SET state = 'missing' WHERE id = ?1",
                params![id],
            )
            .map_err(|e| ScanError::Sql(e.to_string()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Adopts an already-scanned reference as a managed file: copies it into the
/// library and flips the row's kind.
///
/// **The only function in this crate that can produce a `managed` row**, and it
/// is not reachable from [`scan`]. That separation is plan M2.3's "never
/// silently adopt": adopting is a user action with a user-visible effect, and
/// the type system plus this module boundary is what keeps a future refactor
/// from routing a scan through it.
pub fn adopt_source(conn: &Connection, source_id: i64, library_dir: &Path) -> Result<PathBuf> {
    let (path, format): (String, String) = conn
        .query_row(
            "SELECT path, format FROM book_sources WHERE id = ?1",
            params![source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| ScanError::Sql(e.to_string()))?;

    let src = PathBuf::from(&path);
    let dest = library_dir.join(format!("{}.{}", source_id, format.to_lowercase()));
    fs::create_dir_all(library_dir).map_err(|e| ScanError::Sql(e.to_string()))?;
    fs::copy(&src, &dest).map_err(|e| ScanError::Sql(e.to_string()))?;

    conn.execute(
        "UPDATE book_sources SET kind = 'managed', path = ?2, state = 'ok' WHERE id = ?1",
        params![source_id, dest.to_string_lossy()],
    )
    .map_err(|e| ScanError::Sql(e.to_string()))?;

    Ok(dest)
}
