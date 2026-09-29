//! Filename metadata extraction for scanned books.
//!
//! # What this is measured against
//!
//! The scanner adopts files from an existing library, and a large share of
//! those files were written by Calibre. This crate reads a filename back into
//! the fields Calibre would put in them, so an adopted book carries the same
//! author/series it had rather than a second, slightly different identity.
//!
//! Calibre's default save template is
//!
//! ```text
//! {author}/{author} - {title}{series: ' ('#' + series_index + ')'}...
//! ```
//!
//! reduced to the single-directory case that a `save_template` of
//! `author - title` produces, which is what this parses. The
//! `series:`, `index:`, `pubdate:` and `tags:` suffixes are all optional
//! segments of that template.
//!
//! # Why it is conservative
//!
//! Every field except `title` is optional, and a field that cannot be
//! attributed with confidence is left `None` rather than guessed. The reason
//! is asymmetry of consequence: a book that is missing an author still has the
//! right title and still appears under the right name, while a book with a
//! *wrong* author is merged into a different author's work and cannot be
//! un-merged without the user finding and undoing it by hand.
//!
//! This is why a bare `v3` is a volume marker but a bare `3` is not, why the
//! series separator is `#` and not whitespace, and why `(2011)` is a date but
//! `(Dune #1)` is a series.

use regex::Regex;
use std::sync::OnceLock;

/// Everything a single filename can be read as saying about a book.
///
/// Constructed field-by-field rather than by a struct literal at each return,
/// so adding a field cannot silently leave one code path unpopulated.
/// `Eq` is deliberately absent: `series_index` is an `f64`, which is
/// `PartialEq` but not `Eq`. A test comparing whole `ParsedName` values still
/// works — `assert_eq!` only needs `PartialEq` — and nothing here needs the
/// reflexive transitivity `Eq` would buy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedName {
    /// The book title. Never empty for a filename that had a readable stem.
    pub title: String,
    /// Author, from Calibre's `{author} - {title}` split.
    pub author: Option<String>,
    /// Series name, from the `(... #3.5)` group.
    pub series: Option<String>,
    /// Fractional series index. `f64` because Calibre's templates permit
    /// `Series #1.5`, and truncating it would put two books in one series slot.
    pub series_index: Option<f64>,
    /// Non-series volume marker, from a standalone `v3`.
    pub index: Option<u32>,
    /// Publication year, from a trailing `(2011)`.
    pub pubdate: Option<String>,
    /// Bracketed tags, comma separated, in the order they appeared.
    pub tags: Vec<String>,
}

/// A parenthesised group that is a series, i.e. carries a `#` index.
///
/// The `#` is required and that is the whole design. `(2011)` and
/// `(Foundation Series)` are syntactically the same shape, and without a
/// marker only a date is distinguishable — so anything without one is
/// deliberately read as neither, because reading `Foundation Series` as a
/// title suffix and reading it as a series are both guesses, and the second is
/// the one that silently reorganises a library.
fn series_group_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*(?P<series>.+?)\s*#\s*(?P<index>\d+(?:\.\d+)?)\s*$").unwrap()
    })
}

/// A parenthesised group that is a date: four digits, optionally with a
/// month or a full date, anchored to the whole group.
fn date_group_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(?P<date>\d{4}(?:[-/.]\d{1,2}){0,2})\s*$").unwrap())
}

/// A trailing `v3` volume marker, either case, digits only.
///
/// Anchored at the **end** only, not both ends. The marker sits at the end of
/// a title (`Dune v2`), so anchoring the start as well would mean it only ever
/// matched a title that is nothing but a marker — which is why the first
/// version of this never peeled anything.
///
/// The leading boundary is a space, not `\b`: `Dunev2` is one word and has no
/// marker, while `\b` would happily split it.
fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s[vV](?P<index>\d+)$").unwrap())
}

/// Reduce a path or bare filename to the stem the metadata lives in.
///
/// # The `rsplit` trap
///
/// This cannot be `rsplit('.').next()`. `rsplit` yields everything *after*
/// the last dot, so `The Dispossessed.epub` becomes `epub` and the extracted
/// title is the file extension — a parse that looks plausible and is wrong
/// about every field at once. `rsplit_once('.')` keeps everything before the
/// final dot, which is the stem.
///
/// Two cases are then excluded from the extension split:
/// - a leading dot is a hidden file, not an extension, so `.bashrc` is a
///   title and `bashrc` is not a filename;
/// - an extension with a space in it is not an extension, so
///   `Mr. Smith.epub` keeps its period.
fn stem_of(name: &str) -> &str {
    // `trim_end` only, and deliberately **not** `trim`. A stem may begin with
    // ` - ` for an authorless file, and that leading space is the first half
    // of the author separator the split below looks for. Trimming it here
    // makes `split_once(" - ")` miss, and the title keeps a leading dash.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim_end();

    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains(' ') => stem,
        _ => base,
    }
}

/// Remove trailing `[...]` tag groups, returning the remainder and the tags.
///
/// Scanning from the right means a `[` inside the title — `Dune [Special
/// Edition]` — is only treated as a tag group when it is actually a
/// well-formed group at the end. An unbalanced bracket is left alone rather
/// than swallowing the rest of the title.
fn take_tags(stem: &str) -> (&str, Vec<String>) {
    let mut rest = stem.trim_end();
    // One entry per `[...]` group, each holding that group's tags in written
    // order. Collected this way because the groups are found right-to-left,
    // so reversing the *groups* at the end is what restores file order.
    let mut groups: Vec<Vec<String>> = Vec::new();

    while let Some(inner) = rest.strip_suffix(']').and_then(|s| {
        s.rfind('[')
            .filter(|&open| {
                // A tag group does not contain an unbalanced closer, so
                // `Dune [a] b]` cannot be read as one group.
                !s[open..].contains(']')
            })
            .map(|open| &s[open + 1..])
    }) {
        rest = rest[..rest.rfind('[').expect("just found the open bracket")].trim_end();
        groups.push(
            inner
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }

    // Reverse the GROUPS, never the tags inside one. A flat
    // `tags.reverse()` looks equivalent and is not: it turns
    // `[Cyberpunk, Classic]` into `["Classic", "Cyberpunk"]`, because the two
    // tags of a single group are indistinguishable from two groups once they
    // are in one list. So the flattening has to happen after the group order
    // is restored.
    groups.reverse();
    (rest, groups.into_iter().flatten().collect())
}

/// Peel every trailing `(...)` group, classifying each as a series or a date.
///
/// A **loop**, because Calibre's template writes both: `{title} ({series} #{n})
/// ({pubdate})` is two groups, and a parser that takes one leaves the other
/// glued to the title. Taking exactly one is the shape that passes a test
/// written against a one-group filename and silently mis-parses every real
/// Calibre book.
///
/// Order is innermost-last: the group nearest the title is the series, the one
/// after it the date, which is the order the template emits them.
fn take_groups(stem: &str) -> (String, ParsedName) {
    let mut out = ParsedName::default();
    // `trim_end` only, for the same reason `parse` does: a leading space may
    // be the first half of the ` - ` author separator.
    let mut rest = stem.trim_end().to_string();

    // Bounded so a pathological `(((((...` cannot spin. Three is two real
    // groups plus one that will fail to classify and stop the loop anyway.
    for _ in 0..3 {
        let trimmed = rest.trim_end().to_string();
        let Some(open) = trimmed.strip_suffix(')').and_then(|s| s.rfind('(')) else {
            break;
        };
        let head = trimmed[..open].trim_end();
        // A group that is the whole remaining name is part of the name:
        // `(Untitled).epub` is a title, not a series called "Untitled".
        if head.is_empty() {
            break;
        }
        let inner = trimmed[open + 1..trimmed.len() - 1].to_string();

        if let Some(caps) = series_group_re().captures(&inner) {
            if out.series.is_none() {
                out.series = caps
                    .name("series")
                    .map(|m| m.as_str().trim().to_string())
                    .filter(|s| !s.is_empty());
                out.series_index = caps
                    .name("index")
                    .and_then(|m| m.as_str().parse::<f64>().ok());
                rest = head.to_string();
                continue;
            }
        } else if let Some(caps) = date_group_re().captures(&inner) {
            if out.pubdate.is_none() {
                out.pubdate = caps
                    .name("date")
                    .map(|m| m.as_str().trim().to_string())
                    .filter(|s| !s.is_empty());
                rest = head.to_string();
                continue;
            }
        }

        // A group that classifies as nothing is part of the name, so stop
        // peeling and leave it where it was written.
        break;
    }

    (rest, out)
}

/// Split the stem on Calibre's ` - ` author separator.
///
/// The separator carries its spaces, and only the **first** one counts.
/// Without the spaces a hyphenated surname (`Le Guin, Ursula K. - The
/// Dispossessed`) and an em-dash title (`Anne of Green Gables - A Novel`)
/// become ambiguous; with them the first split still leaves an author whose
/// surname is hyphenated intact.
fn split_author_title(stem: &str) -> (Option<String>, String) {
    match stem.split_once(" - ") {
        Some((author, title)) => {
            let (author, title) = (author.trim(), title.trim());
            if title.is_empty() {
                // Nothing after the separator: there is no title, so this was
                // not a separator at all and the dash belongs to the name.
                (None, stem.trim().to_string())
            } else {
                // The author may be empty — a file that *begins* ` - ` has no
                // author, and returning `None` rather than `Some("")` is what
                // keeps a nameless author out of the database. The separator is
                // still consumed, so the title does not keep a leading dash.
                (
                    Some(author.to_string()).filter(|a| !a.is_empty()),
                    title.to_string(),
                )
            }
        }
        None => (None, stem.trim().to_string()),
    }
}

/// Read a filename back into the fields Calibre writes into it.
///
/// Accepts a bare filename or a full path; any directory component is
/// discarded. `title` is always populated when the stem is non-empty, and
/// falls back to the trimmed stem when every optional field has been peeled
/// off it, so a call can never get back an empty title for a named file.
pub fn parse(name: &str) -> ParsedName {
    // Peeled outermost-first: a tag group or a parenthesised group is
    // metadata about the title, so it has to come off before the title is
    // read, and the tags before the groups so a tag group containing a
    // bracket cannot be mistaken for a series.
    let (after_tags, tags) = take_tags(stem_of(name));
    // `trim_end` only, and deliberately **not** `trim`: a stem may *begin*
    // with ` - ` (an authorless file), and that leading space is part of the
    // separator the author split looks for. Trimming both ends here turns
    // `" - The Dispossessed"` into `"- The Dispossessed"`, which no longer
    // matches `" - "`, and the title keeps a leading dash.
    let (rest, out) = take_groups(after_tags.trim_end());
    let mut out = out;
    out.tags = tags;

    // A leading `[author]` is the other convention in the wild, and it is
    // only an author when a title follows it: `[author] Title`. A file that
    // is *only* a bracket group — `[Untitled].epub` — keeps it as the title,
    // because an author with no title is not a book and guessing the other
    // way produces an entry with an author and a blank name.
    let (rest, bracket_author) = match rest.strip_prefix('[') {
        Some(tail) => match tail.find(']') {
            Some(close) => {
                let author = tail[..close].trim();
                let remainder = tail[close + 1..].trim();
                if !author.is_empty() && !remainder.is_empty() {
                    (remainder.to_string(), Some(author.to_string()))
                } else {
                    (rest, None)
                }
            }
            None => (rest, None),
        },
        None => (rest, None),
    };

    finish(out, &rest, bracket_author)
}

/// Split the author, drop a standalone volume marker, and set the title.
fn finish(mut out: ParsedName, rest: &str, bracket_author: Option<String>) -> ParsedName {
    let (author, title) = split_author_title(rest);
    // A ` - ` author wins over a leading `[author]`: it is the convention
    // Calibre itself writes, and a name like `[Some Author] Some - Title` is
    // more likely the bracket being part of the title than two authors.
    out.author = author.or(bracket_author);

    // Only a marker at the very end of the title. `Version 2` and `v3.2.1`
    // are not volume markers, and `Dune v2 Messiah` does not end in digits,
    // so none of them is peeled.
    let peeled = match version_re().captures(&title) {
        Some(c) => {
            out.index = c.name("index").and_then(|n| n.as_str().parse::<u32>().ok());
            // Cut at the whole-match start, so `Dune v2` becomes `Dune` and
            // the trailing separator with it. Trimming by "last
            // non-alphanumeric" instead would leave `Dune v` — the `v` is
            // alphanumeric, so it is preserved and the title keeps half a
            // marker. `Captures` has no `.start()` of its own; the whole match
            // is group 0.
            title[..c.get(0).expect("group 0 always matches").start()]
                .trim()
                .to_string()
        }
        None => title,
    };

    out.title = if peeled.is_empty() {
        // Nothing survived the peeling; the stem is a better answer than "".
        rest.trim().to_string()
    } else {
        peeled
    };
    out
}
