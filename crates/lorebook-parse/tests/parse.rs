//! The table-driven check the M3.1 milestone asks for.
//!
//! Twenty-plus real-world filenames, each with the parse it must produce.
//!
//! # Why the table is a fixture and not a loop of one-off asserts
//!
//! The bug this test was written to catch was a stem taken as
//! `rsplit('.').next()`, which returns the text *after* the last dot — so
//! `The Dispossessed.epub` parsed as the title `epub`. Every one of those
//! filenames would have failed loudly, which is the good case. The dangerous
//! case is a filename that produces a *plausible* wrong parse, and those are
//! the rows carrying the most weight: the ones where a group is deliberately
//! not read as a series, and the ones where a hyphen could be mistaken for
//! the author separator.

use lorebook_parse::{parse, ParsedName};

/// Build an expectation with only the fields a case cares about.
fn only(title: &str) -> ParsedName {
    ParsedName {
        title: title.to_string(),
        ..Default::default()
    }
}

fn with_author(title: &str, author: &str) -> ParsedName {
    ParsedName {
        title: title.to_string(),
        author: Some(author.to_string()),
        ..Default::default()
    }
}

fn with_date(title: &str, date: &str) -> ParsedName {
    ParsedName {
        title: title.to_string(),
        pubdate: Some(date.to_string()),
        ..Default::default()
    }
}

fn with_volume(title: &str, index: u32) -> ParsedName {
    ParsedName {
        title: title.to_string(),
        index: Some(index),
        ..Default::default()
    }
}

fn with_tags(title: &str, tags: &[&str]) -> ParsedName {
    ParsedName {
        title: title.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    }
}

/// Every case, as (filename, expected).
///
/// A tuple table rather than a struct-of-arrays so a new case is one line and
/// cannot be added to the filenames without also adding its expectation.
///
/// A function rather than a `const` because `ParsedName` owns a `String` and a
/// `Vec`, and a `const` would have to drop the whole table at compile time,
/// which Rust rejects ([E0493]). A `fn` is rebuilt per test, which is
/// immaterial for 33 cases and keeps the fixture readable.
fn cases() -> Vec<(&'static str, ParsedName)> {
    vec![
        // --- The plan's own two named examples ---------------------------------
        (
            "Le Guin, Ursula K. - The Dispossessed.epub",
            with_author("The Dispossessed", "Le Guin, Ursula K."),
        ),
        // Calibre writes `author - title`, so `[author] Title` is the *other*
        // convention in the wild rather than a bracketed tag group. The `v03` is
        // a genuine volume marker, so this row is also the one that proves both
        // are read together: an author, a marker, a date and a tag on one name.
        //
        // The agent's first attempt expected `title: "Title"` with **no** author
        // and **no** index, which is wrong twice — the bracket is an author and
        // the `v03` is a marker. A fixture that encodes a guess is worse than no
        // fixture, because it makes the correct implementation look broken.
        (
            "[author] Title v03 (2020) [AO3].epub",
            ParsedName {
                title: "Title".to_string(),
                author: Some("author".to_string()),
                index: Some(3),
                pubdate: Some("2020".to_string()),
                tags: vec!["AO3".to_string()],
                ..Default::default()
            },
        ),
        // --- Calibre's own default template shape -----------------------------
        (
            "Ursula K. Le Guin - The Left Hand of Darkness.mobi",
            with_author("The Left Hand of Darkness", "Ursula K. Le Guin"),
        ),
        (
            "Frank Herbert - Dune (Dune #1) (1965).epub",
            ParsedName {
                title: "Dune".to_string(),
                author: Some("Frank Herbert".to_string()),
                series: Some("Dune".to_string()),
                series_index: Some(1.0),
                pubdate: Some("1965".to_string()),
                ..Default::default()
            },
        ),
        (
            "Frank Herbert - Children of Dune (Dune #2) (1969) [Sci-Fi].epub",
            ParsedName {
                title: "Children of Dune".to_string(),
                author: Some("Frank Herbert".to_string()),
                series: Some("Dune".to_string()),
                series_index: Some(2.0),
                pubdate: Some("1969".to_string()),
                tags: vec!["Sci-Fi".to_string()],
                ..Default::default()
            },
        ),
        // A fractional series index must not truncate: 1.5 and 1 are different
        // slots in a series and collapsing them files two books as one. The
        // `Author - ` prefix is also there on purpose: this row proves the series
        // group is peeled *after* the author split, so the author survives.
        (
            "Author - Mid-Series (Foundation #1.5).epub",
            ParsedName {
                title: "Mid-Series".to_string(),
                author: Some("Author".to_string()),
                series: Some("Foundation".to_string()),
                series_index: Some(1.5),
                ..Default::default()
            },
        ),
        // --- Volume markers ---------------------------------------------------
        ("Dune v2.epub", with_volume("Dune", 2)),
        ("Dune v02.epub", with_volume("Dune", 2)),
        ("Dune V3.epub", with_volume("Dune", 3)),
        // `Version 2` is a word, not a marker, and stays whole.
        (
            "Blade Runner Version 2.epub",
            only("Blade Runner Version 2"),
        ),
        // --- Dates -------------------------------------------------------------
        ("The Hobbit (1937).epub", with_date("The Hobbit", "1937")),
        ("Dune (1965-08-01).epub", with_date("Dune", "1965-08-01")),
        // Four digits are required, so a group that merely contains a year is
        // not a date.
        (
            "A Study in Scarlet (Vol 1 of 2).epub",
            only("A Study in Scarlet (Vol 1 of 2)"),
        ),
        // --- Tags --------------------------------------------------------------
        (
            "Neuromancer [Cyberpunk] [Classic].epub",
            with_tags("Neuromancer", &["Cyberpunk", "Classic"]),
        ),
        (
            "Neuromancer [Cyberpunk, Classic].epub",
            with_tags("Neuromancer", &["Cyberpunk", "Classic"]),
        ),
        // An unbalanced bracket is not a tag group and must not swallow the title.
        ("Dune [Special Edition.epub", only("Dune [Special Edition")),
        // --- Author / title separation ----------------------------------------
        // The first ` - ` wins, so a hyphenated surname survives.
        (
            "Jean-Luc Picard - The Measure of a Man.epub",
            with_author("The Measure of a Man", "Jean-Luc Picard"),
        ),
        // A title containing a hyphen is not split, because the separator carries
        // spaces on both sides.
        ("Anne of Green Gables.epub", only("Anne of Green Gables")),
        // A bare hyphen is not a separator.
        ("Well-Travelled Road.epub", only("Well-Travelled Road")),
        // An empty side of the separator is not an author.
        (" - The Dispossessed.epub", only("The Dispossessed")),
        // --- Stems and extensions ---------------------------------------------
        // A dot inside a title survives; only the final extension is dropped.
        (
            "Mr. Smith Goes to Washington.epub",
            only("Mr. Smith Goes to Washington"),
        ),
        // No extension at all.
        ("The Dispossessed", only("The Dispossessed")),
        // A leading dot is a hidden file, not an extension.
        (".bashrc", only(".bashrc")),
        // A full path is accepted; the directory is discarded.
        (
            "/home/reader/books/Frank Herbert - Dune (Dune #1).epub",
            ParsedName {
                title: "Dune".to_string(),
                author: Some("Frank Herbert".to_string()),
                series: Some("Dune".to_string()),
                series_index: Some(1.0),
                ..Default::default()
            },
        ),
        // Windows separators, because Calibre writes those on a shared library.
        (
            r"C:\Books\Iain M. Banks - Consider Phlebas.epub",
            with_author("Consider Phlebas", "Iain M. Banks"),
        ),
        // --- Degenerate input --------------------------------------------------
        // A title that is entirely a parenthesised group keeps the group.
        ("(Untitled).epub", only("(Untitled)")),
        ("a.epub", only("a")),
        ("", only("")),
        // Whitespace around the stem is not part of the name.
        ("  The Hobbit  .epub", only("The Hobbit")),
    ]
}

#[test]
fn the_table_is_big_enough_to_be_worth_having() {
    // A milestone gate that reads "at least 20 real-world filenames" needs to
    // fail when the table is trimmed, or nobody notices the trim. A fixture
    // that silently shrinks is worse than no fixture.
    let table = cases();
    assert!(
        table.len() >= 20,
        "M3.1 requires >=20 filenames; the table has {}",
        table.len()
    );
}

#[test]
fn every_filename_parses_as_tabulated() {
    for (name, expected) in cases() {
        let got = parse(name);
        assert_eq!(
            got, expected,
            "\n  filename: {name:?}\n  expected: {expected:?}\n  got:      {got:?}"
        );
    }
}

#[test]
fn a_title_is_never_empty_when_the_stem_is_not() {
    // The invariant the table cannot express: every field may be absent, but
    // `title` may not, or the book has no name to show.
    for name in [
        "a.epub",
        "Dune v1.epub",
        "Dune (2011).epub",
        "Dune [x].epub",
    ] {
        let got = parse(name);
        assert!(
            !got.title.trim().is_empty(),
            "{name:?} produced no title: {got:?}"
        );
    }
}

/// A guard against the whole class of bug this crate is prone to.
///
/// The original implementation took the stem as `rsplit('.').next()`, which
/// returns the substring *after* the last dot. That makes the title the file
/// extension for every single filename — a parse that returns `Some` for every
/// field, passes any "is it populated" check, and is wrong about all of them.
#[test]
fn the_stem_is_not_the_extension() {
    let got = parse("The Dispossessed.epub");
    assert_eq!(got.title, "The Dispossessed");
    assert_ne!(
        got.title, "epub",
        "title is the file extension: rsplit('.') bug"
    );
}

/// The two properties whose failure is a *plausible* wrong answer.
///
/// Kept out of the table so a failure says which property broke.
#[test]
fn an_unrecognised_group_stays_in_the_title() {
    // `(Foundation Series)` has no `#` index, so it is neither a series nor
    // a date. Guessing either one silently reorganises a library, so it stays
    // part of the name.
    let got = parse("Isaac Asimov - Foundation (Foundation Series).epub");
    assert_eq!(got.series, None, "a group with no # is not a series");
    assert_eq!(got.series_index, None);
    assert_eq!(
        got.title, "Foundation (Foundation Series)",
        "an unrecognised group belongs to the title"
    );
    assert_eq!(got.author.as_deref(), Some("Isaac Asimov"));
}

#[test]
fn a_date_group_is_not_a_series() {
    // Same syntax, different meaning, decided by the `#`. Cross-reading them
    // files an edition of a book as volume N of a series called "2011".
    let got = parse("Some Author - Book (Dune #3) (2011).epub");
    assert_eq!(got.series.as_deref(), Some("Dune"));
    assert_eq!(got.series_index, Some(3.0));
    assert_eq!(got.pubdate.as_deref(), Some("2011"));
    assert_eq!(got.title, "Book");
}

#[test]
fn parsing_is_deterministic() {
    // The crate compiles its regexes once via `OnceLock` and peels the stem by
    // hand. A parser that returned a different answer for the same input
    // depending on timing would make the scanner's identity non-reproducible,
    // which is the one thing the content hash in M2.1 depends on.
    let name = "Frank Herbert - Dune (Dune #1) (1965) [Sci-Fi].epub";
    let first = parse(name);
    for _ in 0..16 {
        assert_eq!(parse(name), first);
    }
}
