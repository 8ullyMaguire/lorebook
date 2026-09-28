/// Filename parsing module.
///
/// This is a placeholder for the Calibre FileInfo regex set.
/// The actual implementation will parse filenames into structured
/// metadata according to Calibre's rules.
///
/// For now, expose a function that returns `String` placeholders.
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Full filename without extension.
    pub basename: String,
    /// Extension (e.g., "epub").
    pub extension: Option<String>,
}

/// Parse a filename into a `FileInfo`.
///
/// Currently a no-op that splits on the last '.'.
pub fn parse_file_info<P: AsRef<Path>>(path: P) -> FileInfo {
    let p = path.as_ref();
    let ext = p.extension().and_then(|s| s.to_str().map(|s| s.to_ascii_lowercase()));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    FileInfo { basename: stem, extension: ext }
}
