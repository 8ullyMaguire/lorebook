//! Domain types for Lorebook.
//!
//! These are the app's own shapes. They deliberately do **not** mirror
//! Calibre's schema one-to-one: Calibre's `books`/`data` tables are encoded
//! around a GUI's needs, and the interop layer in `lorebook-calibre` is the
//! only place that knows about them.
//!
//! Spec: `docs/SPECIFICATION.md` §3 (import model), §4 (schema).

pub mod hash;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

/// How this app treats a book's file. Spec §3.2.
///
/// A book may hold several formats with different kinds; mixed libraries are
/// normal, not a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// We own the file, inside the library dir. Moved/copied in on import and
    /// deleted with the book. The default for an explicit "add book".
    Managed,
    /// The file stays where it is. Never moved, copied, or deleted. The
    /// default for a scanned folder.
    Reference,
    /// We manage a symlink at a path we choose; the target is untouched.
    Symlink,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Managed => "managed",
            SourceKind::Reference => "reference",
            SourceKind::Symlink => "symlink",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "managed" => Some(SourceKind::Managed),
            "reference" => Some(SourceKind::Reference),
            "symlink" => Some(SourceKind::Symlink),
            _ => None,
        }
    }

    /// Whether deleting a book may touch the file on disk.
    ///
    /// This is the property the spec calls load-bearing: deleting a
    /// `reference` book removes the row only, and the file is never touched
    /// (§3.3). Expressed as a method so a caller cannot forget the check.
    pub fn owns_file(self) -> bool {
        matches!(self, SourceKind::Managed)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Availability of a source. Spec §4: only `ok` is a usable format; the
/// others are *visible states, not errors to be hidden*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceState {
    Ok,
    /// The file is gone. The book row and its metadata survive.
    Missing,
    /// The path changed; the file was re-matched by hash.
    Moved,
    /// Two sources claim the same content.
    Conflict,
}

impl SourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceState::Ok => "ok",
            SourceState::Missing => "missing",
            SourceState::Moved => "moved",
            SourceState::Conflict => "conflict",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(SourceState::Ok),
            "missing" => Some(SourceState::Missing),
            "moved" => Some(SourceState::Moved),
            "conflict" => Some(SourceState::Conflict),
            _ => None,
        }
    }

    /// Whether a format in this state can actually be opened.
    pub fn is_usable(self) -> bool {
        matches!(self, SourceState::Ok)
    }
}

impl fmt::Display for SourceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One file backing one format of one book. Spec §4 `book_sources`.
///
/// A book's *content identity* is its content hash, not its path, so the same
/// file reachable by two scan roots is one book (§3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookSource {
    pub book: i64,
    /// Uppercase format id, Calibre's convention: `EPUB`, `MOBI`, `PDF`, `AZW3`.
    pub format: String,
    pub kind: SourceKind,
    pub path: PathBuf,
    pub size: i64,
    pub mtime_ns: i64,
    /// Strong content hash; the merge key.
    pub content_hash: String,
    pub state: SourceState,
    /// Set when this source was replaced by another (e.g. moved and re-added).
    pub superseded_by: Option<i64>,
    /// How sure we are that this source is the right file, 0.0..=1.0.
    pub confidence: f64,
}

impl BookSource {
    /// Whether this source's file may be deleted with the book.
    ///
    /// The single place that answers that question, so no caller has to
    /// remember the rule from §3.3.
    pub fn may_delete_file(&self) -> bool {
        self.kind.owns_file() && self.state.is_usable()
    }
}

/// A book as the app models it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub sort: Option<String>,
    pub author_sort: Option<String>,
    pub timestamp: Option<SystemTime>,
    pub pubdate: Option<SystemTime>,
    pub series_index: f64,
    pub uuid: Option<String>,
    pub has_cover: bool,
    /// Every format of this book, each with its own kind and state.
    pub sources: Vec<BookSource>,
    /// Author names, resolved through `books_authors_link`.
    pub authors: Vec<String>,
    /// Tag names, resolved through `books_tags_link`.
    pub tags: Vec<String>,
    /// Series name, if any.
    pub series: Option<String>,
}

impl Book {
    /// Formats that can actually be opened right now.
    pub fn usable_formats(&self) -> impl Iterator<Item = &BookSource> {
        self.sources.iter().filter(|s| s.state.is_usable())
    }

    /// Whether the book has at least one openable format. A book with every
    /// format `missing` still exists and still shows its metadata; it just
    /// cannot be read (§3.3).
    pub fn is_readable(&self) -> bool {
        self.usable_formats().next().is_some()
    }

    /// Series display, e.g. `Some Series #3`.
    pub fn series_display(&self) -> Option<String> {
        self.series
            .as_ref()
            .map(|s| format!("{s} #{}", fmt_index(self.series_index)))
    }
}

/// Format a series index for display: `1` not `1.0`, `3.5` as `3.5`.
fn fmt_index(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// An author, as Calibre stores one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub id: i64,
    pub name: String,
    pub sort: Option<String>,
    pub link: String,
}

/// A tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
}

/// A library-level identifier, e.g. an isbn or an AO3 work id.
///
/// Calibre's `identifiers` table is unique on (type, val) and needs no schema
/// change to carry `ao3`, `ffnet` or `fic`, which is what makes it usable for
/// this library's duplicate detection (spec §4, Identifier strategy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identifier {
    pub id: i64,
    pub book: i64,
    /// Lowercase scheme name, e.g. `isbn`, `ao3`, `goodreads`.
    pub scheme: String,
    pub val: String,
}
