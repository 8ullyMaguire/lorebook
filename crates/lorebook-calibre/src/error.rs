//! Errors for the Calibre interop layer.

use std::path::PathBuf;

/// Anything that can go wrong opening or talking to a Calibre library.
#[derive(Debug, thiserror::Error)]
pub enum CalibreError {
    /// The directory has no `metadata.db`, so it is not a Calibre library.
    #[error("not a Calibre library (no metadata.db in {0})")]
    NotALibrary(PathBuf),

    /// `metadata.db` exists but is not a Calibre schema — e.g. a plain SQLite
    /// file that happens to be named that.
    #[error("metadata.db is not a Calibre schema (no meta.version row)")]
    NotACalibreSchema,

    /// The path is not readable or writable.
    #[error("io error: {0}")]
    Io(String),

    /// sqlite returned an error.
    #[error("sqlite error: {0}")]
    Sql(String),

    /// The caller passed something the schema cannot accept.
    #[error("invalid input: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, CalibreError>;

/// `rusqlite::Error` → [`CalibreError::Sql`].
///
/// Without this, every `?` on a rusqlite call in this crate needs a
/// `.map_err(|e| CalibreError::Sql(format!("...: {e}")))`, and the context has to
/// be retyped at every call site. The message is preserved; only the type is
/// narrowed.
impl From<rusqlite::Error> for CalibreError {
    fn from(e: rusqlite::Error) -> Self {
        CalibreError::Sql(e.to_string())
    }
}
