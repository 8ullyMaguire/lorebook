//! The Lorebook desktop app: a Tauri shell over the `lorebook-calibre` core.
//!
//! # Why the state is behind a Mutex
//!
//! [`rusqlite::Connection`] is `!Sync` — a SQLite connection may not be used
//! from multiple threads without synchronisation. Tauri requires managed state
//! to be `Send + Sync`, so the open library lives behind a [`Mutex`]. There is
//! one library open at a time, which is what the mutex is sized for; a library
//! is not a shared cache and does not need concurrent readers.
//!
//! # Why commands are thin
//!
//! Every command here is a boundary shim: validate, call one `lorebook-calibre`
//! function, convert the error. No query, no business rule and no formatting
//! lives in this crate. If a rule needs testing, it belongs in
//! `lorebook-calibre`, where `cargo test` reaches it without a webview.

use std::path::PathBuf;
use std::sync::Mutex;

use lorebook_calibre::{Library, PAGE_MAX_LIMIT};
use serde::Serialize;
use tauri::{Manager, State};

/// Tauri managed state: the open library, if any.
pub struct AppState {
    library: Mutex<Option<Library>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            library: Mutex::new(None),
        }
    }
}

/// Errors crossing the IPC boundary.
///
/// `lorebook_calibre::CalibreError` is not `Serialize`, and the frontend cannot
/// read a Rust error type. This carries the message across as a string, which is
/// what the UI shows the user. The specific variants are preserved in the
/// message — `NotALibrary` in particular produces a message a user can act on.
pub type IpcResult<T> = Result<T, String>;

fn to_ipc_err(e: lorebook_calibre::CalibreError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// A book as the frontend receives it.
///
/// Deliberately **not** `lorebook_core::Book`. That type carries
/// `Option<SystemTime>`, which serialises as a `{secs_since_epoch, nanos}`
/// struct — something a JS `Date` cannot consume. The wire format is chosen for
/// the consumer, and the conversion is the one place the two shapes meet.
///
/// Timestamps are epoch milliseconds, which is what `Date.now()` speaks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookDto {
    pub id: i64,
    pub title: String,
    /// Calibre's sort key, the article-stripped form used for ordering.
    pub sort: Option<String>,
    pub author_sort: Option<String>,
    pub timestamp_ms: Option<i64>,
    pub pubdate_ms: Option<i64>,
    pub series_index: f64,
    pub has_cover: bool,
    pub authors: Vec<String>,
    pub tags: Vec<String>,
    pub series: Option<String>,
    /// Formats present, e.g. `["EPUB", "PDF"]`. Empty on a list page, which does
    /// not load per-format rows.
    pub formats: Vec<String>,
}

fn ms(t: Option<std::time::SystemTime>) -> Option<i64> {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

impl From<&lorebook_core::Book> for BookDto {
    fn from(b: &lorebook_core::Book) -> Self {
        Self {
            id: b.id,
            title: b.title.clone(),
            sort: b.sort.clone(),
            author_sort: b.author_sort.clone(),
            timestamp_ms: ms(b.timestamp),
            pubdate_ms: ms(b.pubdate),
            series_index: b.series_index,
            has_cover: b.has_cover,
            authors: b.authors.clone(),
            tags: b.tags.clone(),
            series: b.series.clone(),
            formats: b.sources.iter().map(|s| s.format.clone()).collect(),
        }
    }
}

/// One page of books plus the total, so the UI can show a page count without a
/// second round trip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookPage {
    pub books: Vec<BookDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

impl BookPage {
    /// Whether another page exists after this one.
    pub fn has_more(&self) -> bool {
        self.offset + (self.books.len() as i64) < self.total
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn with_library<T>(
    state: &State<AppState>,
    f: impl FnOnce(&Library) -> lorebook_calibre::Result<T>,
) -> IpcResult<T> {
    let guard = state
        .library
        .lock()
        .map_err(|_| "library lock poisoned — a previous command panicked".to_string())?;
    let lib = guard
        .as_ref()
        .ok_or_else(|| "no library is open".to_string())?;
    f(lib).map_err(to_ipc_err)
}

/// Open an existing Calibre library. Returns the book count.
#[tauri::command]
fn open_library(path: String, state: State<AppState>) -> IpcResult<i64> {
    let lib = Library::open(&PathBuf::from(path)).map_err(to_ipc_err)?;
    let count = lib.book_count().map_err(to_ipc_err)?;
    *state
        .library
        .lock()
        .map_err(|_| "library lock poisoned".to_string())? = Some(lib);
    Ok(count)
}

/// Create a library that Calibre itself can open. Returns the book count.
#[tauri::command]
fn create_library(path: String, state: State<AppState>) -> IpcResult<i64> {
    let lib = Library::create(&PathBuf::from(path)).map_err(to_ipc_err)?;
    let count = lib.book_count().map_err(to_ipc_err)?;
    *state
        .library
        .lock()
        .map_err(|_| "library lock poisoned".to_string())? = Some(lib);
    Ok(count)
}

/// Close the open library, if any. Idempotent.
#[tauri::command]
fn close_library(state: State<AppState>) -> IpcResult<()> {
    *state
        .library
        .lock()
        .map_err(|_| "library lock poisoned".to_string())? = None;
    Ok(())
}

/// Path of the open library, for the window title.
#[tauri::command]
fn library_path(state: State<AppState>) -> IpcResult<Option<String>> {
    let guard = state
        .library
        .lock()
        .map_err(|_| "library lock poisoned".to_string())?;
    Ok(guard.as_ref().map(|l| l.path().display().to_string()))
}

/// One page of books. Clamped by the core's `PAGE_MAX_LIMIT`.
#[tauri::command]
fn list_books(limit: i64, offset: i64, state: State<AppState>) -> IpcResult<BookPage> {
    with_library(&state, |lib| {
        let books = lorebook_calibre::list_books_page(lib.conn(), limit, offset)?;
        let total = lorebook_calibre::count_books(lib.conn())?;
        Ok(BookPage {
            books: books.iter().map(BookDto::from).collect(),
            total,
            offset: offset.max(0),
            limit: limit.clamp(1, PAGE_MAX_LIMIT),
        })
    })
}

/// One book in full, including its formats and file sources.
#[tauri::command]
fn get_book(id: i64, state: State<AppState>) -> IpcResult<Option<BookDto>> {
    with_library(&state, |lib| {
        Ok(lorebook_calibre::get_book(lib.conn(), id)?
            .as_ref()
            .map(BookDto::from))
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_library,
            create_library,
            close_library,
            library_path,
            list_books,
            get_book,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lorebook");
}
