//! Content hashing and book identity. Spec §3.4.
//!
//! **A book's identity is its bytes, not its path.** The plan is explicit that
//! the merge key is the content hash: a file whose hash is already in the library
//! adds *no* book, it becomes another `book_sources` row pointing at the book
//! that already exists. Two files with the same content at different paths are
//! one book with two sources, and the alternative — keying on path — makes every
//! copy, move and re-download look like a different book.
//!
//! ## Why BLAKE3 and not SHA-256
//!
//! Faster, and **Calibre does not care which digest is used** because the hash
//! lives in *our* additive `book_sources` table, never in a Calibre-owned one.
//! That is the whole of the argument; there is no compatibility requirement
//! being satisfied here, so the faster primitive is simply the better one.
//!
//! ## Why the digest is versioned in the constant
//!
//! [`CONTENT_HASH_VERSION`] is part of every stored hash. The column is
//! `TEXT NOT NULL DEFAULT ''`, so a hash written by one build and read by
//! another has to be *comparable*, and a bare hex string is not: a future change
//! of algorithm would make every existing row silently mismatch every new file,
//! and a hash comparison is exactly the kind of thing that fails quietly — every
//! duplicate becomes a new book, and the user sees their library double. The
//! version prefix turns that into a detectable difference.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Identifies which digest produced a stored hash. Spec §3.4.
///
/// Part of the stored value (`"blake3:<hex>"`), so a hash written by a
/// different build is recognisable rather than merely wrong.
pub const CONTENT_HASH_VERSION: &str = "blake3";

/// Streaming read size.
///
/// 1 MiB: large enough that per-call overhead disappears next to the hashing,
/// small enough that peak memory is bounded regardless of book size. An ebook is
/// a few MB and a scanned PDF can be hundreds, and this function must not care
/// which it is holding.
const READ_BUFFER_BYTES: usize = 1024 * 1024;

/// Anything that can go wrong hashing a file.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// The file could not be opened or read.
    ///
    /// The path is kept: a scan hashes hundreds of files and the message alone
    /// ("io error: permission denied") does not say which one failed.
    #[error("cannot read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, HashError>;

/// Computes content hashes for files on disk.
pub struct ContentHasher;

impl ContentHasher {
    /// Hashes a file's contents, returning `"blake3:<hex>"`.
    ///
    /// Streamed, never `read_to_end`: a 900 MB scanned PDF must not require
    /// 900 MB of resident memory to identify. The buffer is a single
    /// allocation reused across reads, so peak memory is [`READ_BUFFER_BYTES`]
    /// for a file of any size.
    ///
    /// A missing file is an error, not an empty hash. An empty hash is what
    /// `book_sources.content_hash` defaults to, and a scan that cannot read a
    /// file must record that fact — writing the default would make an
    /// unreadable file indistinguishable from one that has never been hashed,
    /// and every later comparison against it would be meaningless.
    pub fn hash_file(path: &Path) -> Result<String> {
        let mut file = File::open(path).map_err(|source| HashError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; READ_BUFFER_BYTES];

        loop {
            let n = file.read(&mut buf).map_err(|source| HashError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if n == 0 {
                break;
            }
            // A short read is not an error: `read` is permitted to return less
            // than the buffer holds, and treating that as EOF would silently
            // hash a *prefix* of the file. That failure mode is invisible — it
            // produces a stable, plausible, wrong hash — so the loop continues
            // until a genuine zero-length read.
            hasher.update(&buf[..n]);
        }

        Ok(Self::format(hasher.finalize().as_bytes()))
    }

    /// Hashes bytes already in memory. Used by tests and by the import path
    /// that receives a downloaded file.
    pub fn hash_bytes(bytes: &[u8]) -> String {
        Self::format(blake3::hash(bytes).as_bytes())
    }

    /// Renders a raw digest as the stored value.
    fn format(digest: &[u8; 32]) -> String {
        // `write!` into a pre-sized String rather than `format!` per byte: this
        // runs once per scanned file, and the byte-at-a-time version is both
        // slower and easier to get subtly wrong.
        use std::fmt::Write as _;
        let mut out = String::with_capacity(CONTENT_HASH_VERSION.len() + 1 + 64);
        out.push_str(CONTENT_HASH_VERSION);
        out.push(':');
        for byte in digest {
            // `write!` to a String is infallible, so the result cannot be `Err`.
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Returns the digest portion of a stored hash, or `None` if the value was
    /// not produced by this algorithm.
    ///
    /// A value with an unknown version is `None` rather than an error: the
    /// caller's decision is "do I recognise this?", and a hash from a future
    /// build is unrecognised, not fatal. A *malformed* value is equally `None`.
    pub fn digest_of(stored: &str) -> Option<&str> {
        stored.strip_prefix(CONTENT_HASH_VERSION)?.strip_prefix(':')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes `contents` to a uniquely named file in a temp dir and returns the
    /// path plus a guard that deletes it.
    ///
    /// Unique per test *and* per call: the incremental-scan tests hash the same
    /// content at two different paths and would collide on a fixed name, and a
    /// shared name is also what makes a suite order-dependent — the second test
    /// to run finds the first test's file and asserts against stale content.
    fn temp_file(tag: &str, contents: &[u8]) -> (std::path::PathBuf, TempDir) {
        let dir = TempDir::new(tag);
        let path = dir.path().join("book.epub");
        let mut f = File::create(&path).expect("create temp file");
        f.write_all(contents).expect("write temp file");
        drop(f);
        (path, dir)
    }

    /// A self-deleting temp directory.
    ///
    /// Hand-rolled rather than pulled from a dev-dependency: `tempfile` is not
    /// in the workspace yet, and adding a crate for three test helpers is worse
    /// than twenty lines. Deletes on drop, so a panicking test still cleans up.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // The tag plus a counter keeps two concurrent tests from sharing a
            // directory; the process id keeps two *runs* from sharing one.
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("lorebook-test-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hashes_a_known_string_to_a_known_value() {
        // Pinned against the BLAKE3 reference value for the empty input, which
        // is the one digest that cannot drift with a library update. If this
        // ever fails, the *algorithm* changed, not the code.
        let got = ContentHasher::hash_bytes(b"");
        assert_eq!(
            got,
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn the_stored_value_is_versioned_lowercase_hex() {
        let got = ContentHasher::hash_bytes(b"abc");
        assert!(got.starts_with("blake3:"), "version prefix: {got}");
        let hex = got.strip_prefix("blake3:").unwrap();
        assert_eq!(hex.len(), 64, "BLAKE3 is 32 bytes = 64 hex chars");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex must be lowercase: {hex}"
        );
    }

    #[test]
    fn different_content_yields_a_different_hash() {
        assert_ne!(
            ContentHasher::hash_bytes(b"one"),
            ContentHasher::hash_bytes(b"two")
        );
    }

    #[test]
    fn hashing_a_file_matches_hashing_its_bytes() {
        // The streamed path and the in-memory path must agree, or a scan and an
        // import of the same file produce two different identities.
        let contents = b"the quick brown fox";
        let (path, _dir) = temp_file("agree", contents);
        let from_file = ContentHasher::hash_file(&path).expect("hash file");
        let from_bytes = ContentHasher::hash_bytes(contents);
        assert_eq!(from_file, from_bytes);
    }

    #[test]
    fn the_same_content_at_two_paths_yields_one_hash() {
        // This is spec §3.4's merge key, stated as a test: two files, one
        // identity. If this fails, every duplicate copy becomes a new book.
        let (a, _da) = temp_file("path-a", b"identical bytes");
        let (b, _db) = temp_file("path-b", b"identical bytes");
        assert_eq!(
            ContentHasher::hash_file(&a).expect("hash a"),
            ContentHasher::hash_file(&b).expect("hash b")
        );
    }

    #[test]
    fn a_file_larger_than_the_buffer_hashes_fully() {
        // Exercises the streaming loop past a buffer boundary, which is the
        // path that decides whether a short read is mistaken for EOF.
        let big = vec![0xABu8; READ_BUFFER_BYTES + 4096];
        let (path, _dir) = temp_file("big", &big);
        assert_eq!(
            ContentHasher::hash_file(&path).expect("hash big file"),
            ContentHasher::hash_bytes(&big)
        );
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_hash() {
        let dir = TempDir::new("missing");
        let err = ContentHasher::hash_file(&dir.path().join("nope.epub"))
            .expect_err("a missing file must not hash");
        // The path is in the message, because a scan hashes many files.
        assert!(
            err.to_string().contains("nope.epub"),
            "error names the file: {err}"
        );
    }

    #[test]
    fn digest_of_recognises_our_own_values_only() {
        let ours = ContentHasher::hash_bytes(b"x");
        assert_eq!(
            ContentHasher::digest_of(&ours),
            Some(ours.strip_prefix("blake3:").unwrap())
        );

        // The cases that must NOT be accepted. A hash from a future build is
        // unrecognised rather than fatal; the caller decides what to do, and
        // silently treating it as ours would compare digests from two algorithms.
        assert_eq!(ContentHasher::digest_of("sha256:abcd"), None);
        assert_eq!(ContentHasher::digest_of("blake3"), None, "no colon");
        assert_eq!(ContentHasher::digest_of(""), None, "the column default");
        assert_eq!(ContentHasher::digest_of("blake2:abcd"), None);
    }
}
