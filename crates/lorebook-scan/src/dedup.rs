use crate::SourceKind;
use crate::SourceState;

/// Represents the minimal information needed to decide if two scanned files
/// belong to the same work.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedEntry {
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    /// Series position (e.g., 1.5 sits between volumes 1 and 2).
    /// Parsed from filename by the same deterministic parser, so exact
    /// f64 equality is sound — values are bit-identical when strings match.
    /// None means "not in a series" and is incomparable with any Some(_).
    pub series_position: Option<f64>,
    /// Integral volume/chapter number (e.g., 1, 2, 3).
    /// Distinct from series_position: this is the whole-volume slot,
    /// series_position is the fractional position within a series.
    pub volume: Option<u32>,
    pub content_hash: [u8; 32], // from blake3 of file
    pub path: std::path::PathBuf,
    pub kind: SourceKind,
    pub state: SourceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupDecision {
    /// Same work, different version -> merge into one book with multiple sources.
    Merge,
    /// Same work, same version, different files -> propose via inbox (user decides).
    ProposeInbox,
    /// Different works -> keep separate.
    Separate,
}

/// Decide how to treat two scanned entries.
///
/// Returns `Merge` if they represent the same work but different versions
/// (different `series_position`). Returns `ProposeInbox` if they represent
/// the same work and same version (all work-defining fields equal, including
/// series_position) but are different files (different content hash or path).
/// Otherwise returns `Separate`.
///
/// `series_position` is parsed by the same deterministic parser, so exact f64
/// equality is sound — identical strings produce bit-identical values.
/// `None` means "not in a series" and is NOT equal to `Some(0.0)` or any other
/// `Some(_)`; two `None` values are considered same version (both unpositioned).
pub fn dedupe_decision(a: &ScannedEntry, b: &ScannedEntry) -> DedupDecision {
    // Work-defining fields: title, author, series.
    let same_work = a.title == b.title && a.author == b.author && a.series == b.series;
    if !same_work {
        return DedupDecision::Separate;
    }
    // Version-defining field: series_position.
    // Exact equality is correct here because values come from the same parser.
    let same_version = match (a.series_position, b.series_position) {
        (Some(a_pos), Some(b_pos)) => a_pos == b_pos, // exact equality
        (None, None) => true,                         // both unpositioned = same
        _ => false,                                   // None vs Some = different
    };
    if !same_version {
        return DedupDecision::Merge;
    }
    // Same work and same version.
    // If content hash differs or path differs, we have different files of same version.
    if a.content_hash != b.content_hash || a.path != b.path {
        DedupDecision::ProposeInbox
    } else {
        // Actually identical file (should not happen due to earlier dedup by hash).
        DedupDecision::Separate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_entry(
        title: &str,
        author: Option<&str>,
        series: Option<&str>,
        series_position: Option<f64>,
        volume: Option<u32>,
        hash: [u8; 32],
        path: &str,
    ) -> ScannedEntry {
        ScannedEntry {
            title: title.to_string(),
            author: author.map(|s| s.to_string()),
            series: series.map(|s| s.to_string()),
            series_position,
            volume,
            content_hash: hash,
            path: PathBuf::from(path),
            kind: SourceKind::Reference,
            state: SourceState::Ok,
        }
    }

    /// Table-driven test covering all DedupDecision branches and edge cases.
    #[test]
    fn dedupe_decision_table() {
        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];
        let hash3 = [3u8; 32];

        // Test case definition
        #[derive(Clone)]
        struct Case<'a> {
            desc: &'a str,
            // base entry params
            base_series_pos: Option<f64>,
            // other entry params
            other_title: &'a str,
            other_author: Option<&'a str>,
            other_series: Option<&'a str>,
            other_series_pos: Option<f64>,
            other_volume: Option<u32>,
            other_hash: [u8; 32],
            other_path: &'a str,
            expected: DedupDecision,
        }

        let cases = vec![
            // --- Merge: same work, different version (series_position differs) ---
            Case {
                desc: "different series_position -> Merge",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(2.0),
                other_volume: Some(2),
                other_hash: hash2,
                other_path: "/books/Title v2.epub",
                expected: DedupDecision::Merge,
            },
            Case {
                desc: "different series_position (1.5 vs 1.0) -> Merge",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.5),
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title 1.5.epub",
                expected: DedupDecision::Merge,
            },
            Case {
                desc: "different series_position (1.0 vs 1.0000001 diff 1e-7) -> Merge",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0000001),
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title 1.0000001.epub",
                expected: DedupDecision::Merge,
            },
            Case {
                desc: "None vs Some(1.0) -> Merge (unpositioned vs positioned)",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: None,
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title no-pos.epub",
                expected: DedupDecision::Merge,
            },
            Case {
                desc: "Some(1.0) vs None -> Merge",
                base_series_pos: None,
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title.epub",
                expected: DedupDecision::Merge,
            },
            // --- ProposeInbox: same work, same version, different file ---
            Case {
                desc: "same version, different hash -> ProposeInbox",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title - Author.epub",
                expected: DedupDecision::ProposeInbox,
            },
            Case {
                desc: "same version, same hash, different path -> ProposeInbox (problem #2 fix)",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash1,
                other_path: "/books/Title (copy).epub",
                expected: DedupDecision::ProposeInbox,
            },
            Case {
                desc: "same version, different hash, different path -> ProposeInbox",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title (another).epub",
                expected: DedupDecision::ProposeInbox,
            },
            Case {
                desc: "both None (unpositioned), different hash -> ProposeInbox",
                base_series_pos: None,
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: None,
                other_volume: Some(1),
                other_hash: hash2,
                other_path: "/books/Title.epub",
                expected: DedupDecision::ProposeInbox,
            },
            Case {
                desc: "both None, same hash, different path -> ProposeInbox",
                base_series_pos: None,
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: None,
                other_volume: Some(1),
                other_hash: hash1,
                other_path: "/books/Title (copy).epub",
                expected: DedupDecision::ProposeInbox,
            },
            // --- Separate: different work ---
            Case {
                desc: "different title -> Separate",
                base_series_pos: Some(1.0),
                other_title: "Other Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash3,
                other_path: "/books/Other.epub",
                expected: DedupDecision::Separate,
            },
            Case {
                desc: "different author -> Separate",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Other Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash3,
                other_path: "/books/Title2.epub",
                expected: DedupDecision::Separate,
            },
            Case {
                desc: "different series -> Separate",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Other Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash3,
                other_path: "/books/Title3.epub",
                expected: DedupDecision::Separate,
            },
            Case {
                desc: "different series (one None) -> Separate",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: None,
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash3,
                other_path: "/books/Title3.epub",
                expected: DedupDecision::Separate,
            },
            // --- Separate: actually identical file (should not happen in practice) ---
            Case {
                desc: "identical hash and path -> Separate",
                base_series_pos: Some(1.0),
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: Some(1.0),
                other_volume: Some(1),
                other_hash: hash1,
                other_path: "/books/Title - Author.epub",
                expected: DedupDecision::Separate,
            },
            Case {
                desc: "identical hash and path, both unpositioned -> Separate",
                base_series_pos: None,
                other_title: "Title",
                other_author: Some("Author"),
                other_series: Some("Series"),
                other_series_pos: None,
                other_volume: Some(1),
                other_hash: hash1,
                other_path: "/books/Title - Author.epub",
                expected: DedupDecision::Separate,
            },
        ];

        for case in cases {
            let base = make_entry(
                "Title",
                Some("Author"),
                Some("Series"),
                case.base_series_pos,
                Some(1),
                hash1,
                "/books/Title - Author.epub",
            );
            let other = make_entry(
                case.other_title,
                case.other_author,
                case.other_series,
                case.other_series_pos,
                case.other_volume,
                case.other_hash,
                case.other_path,
            );
            let got = dedupe_decision(&base, &other);
            assert_eq!(got, case.expected, "case: {}", case.desc);
            // Also test symmetric: dedupe_decision(a, b) == dedupe_decision(b, a)
            let got_rev = dedupe_decision(&other, &base);
            assert_eq!(got_rev, case.expected, "case (reversed): {}", case.desc);
        }
    }
}
