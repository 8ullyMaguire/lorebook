//! Calibre's SQL helper functions, reimplemented in Rust.
//!
//! # Why this module exists
//!
//! Calibre registers Python functions with SQLite at runtime: `title_sort`,
//! `uuid4`, `author_sort`, `books_list_filter`, `sortconcat`, and others. Its
//! `books_insert_trg` and `books_update_trg` triggers **call** `title_sort()` and
//! `uuid4()`:
//!
//! ```sql
//! CREATE TRIGGER books_insert_trg AFTER INSERT ON books
//!     UPDATE books SET sort=title_sort(NEW.title), uuid=uuid4() WHERE id=NEW.id;
//! ```
//!
//! Those functions only exist while Calibre itself has the database open. A
//! plain SQLite client — which is what this app is — therefore **cannot insert
//! a book into a real Calibre library at all**: the trigger fires and fails
//! with `no such function: title_sort`. There is no way around this from SQL.
//!
//! The fix is to register the same functions, with the same behaviour, from
//! Rust. Then Calibre's own triggers work unmodified, which is exactly what the
//! interop guarantee (spec §4) requires: we do not replace or disable
//! Calibre's triggers, we supply the functions they call.
//!
//! # Provenance
//!
//! `title_sort` and `author_sort` are transcribed from Calibre 9.15 source at
//! <https://github.com/kovidgoyal/calibre> — `src/calibre/ebooks/metadata/__init__.py`,
//! functions `title_sort` and `author_to_author_sort` — together with the tweak
//! values they read from `resources/default_tweaks.py`. The articles, prefixes,
//! suffixes, copywords and quote pairs below are Calibre's, not guesses.
//!
//! Behaviour is additionally pinned by the tests in this module and by
//! `tests/interop_with_calibre.sh`, which checks our output against a real
//! Calibre binary, in both directions: a library Calibre created and one we
//! created.
//!
//! When Calibre changes these, this file is what has to be re-checked. See
//! `docs/CALIBRE-PROVENANCE.md`.

use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;

/// Register every Calibre SQL function on a connection.
///
/// Call this **before** any write. Reads work without it; writes do not.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    // Every one of these is pure and cheap, so no INNOCUOUS/DIRECTONLY flags:
    // SQLITE_DIRECTONLY would refuse to run them *inside a trigger*, and
    // Calibre's books_insert_trg is exactly a trigger calling title_sort().
    // Safety is not in question — they touch no state and cannot corrupt the
    // database; the flag would only break the interop we are here for.
    let f = FunctionFlags::SQLITE_UTF8;
    conn.create_scalar_function("title_sort", 1, f, |ctx| {
        Ok(title_sort(ctx.get::<String>(0)?.as_str()))
    })?;
    conn.create_scalar_function("uuid4", 0, f, |_ctx| Ok(crate::new_uuid()))?;
    conn.create_scalar_function("author_sort", 1, f, |ctx| {
        Ok(author_sort(ctx.get::<String>(0)?.as_str()))
    })?;
    // Calibre calls sortconcat(id, name); the id only matters for its NULL-ness
    // and this app never uses Calibre's own tag-browser ordering.
    conn.create_scalar_function("sortconcat", 2, f, |ctx| {
        let _id = ctx.get::<Option<i64>>(0)?;
        Ok(ctx.get::<Option<String>>(1)?.unwrap_or_default())
    })?;
    // Calibre's meta view calls this per row and drops books where it is 0.
    conn.create_scalar_function("books_list_filter", 1, f, |ctx| {
        Ok(ctx.get::<Option<i64>>(0)?.map(|_| 1i64).unwrap_or(0))
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// title_sort
// ---------------------------------------------------------------------------

/// Calibre's English article list, verbatim from
/// `per_language_title_sort_articles['eng']` in `default_tweaks.py`:
/// `A`, `The`, `An` — and nothing else.
///
/// These are regex fragments ending in `\s+`. This is **not** a short list that
/// could reasonably be extended: Calibre matches an ordered alternation, and
/// German/French/Spanish articles (`Die`, `Les`, `Los`) belong to *other*
/// languages, which Calibre selects with its `lang` parameter. Adding them here
/// would silently change English sorting.
const ARTICLES: &[&str] = &[r"A\s+", r"The\s+", r"An\s+"];

/// Opening quote characters and the characters that may close them.
///
/// Verbatim from Calibre's `quote_pairs`. Calibre strips a matched pair before
/// looking for an article, so `"A Tale of Two Cities"` sorts as
/// `Tale of Two Cities, A` rather than leaving quotes around the moved article.
const QUOTE_PAIRS: &[(char, &[char])] = &[
    ('"', &['"']),
    ('\'', &['\'']),
    ('\u{201c}', &['\u{201d}', '\u{201c}']), // " "
    ('\u{201d}', &['\u{201d}', '\u{201c}']), // " "
    ('\u{201e}', &['\u{201d}', '\u{201c}']), // „ "
    ('\u{201a}', &['\u{2019}', '\u{2018}']), // ‚ '
    ('\u{2019}', &['\u{2019}', '\u{2018}']), // ' '
    ('\u{2018}', &['\u{2019}', '\u{2018}']), // ' '
    ('\u{2039}', &['\u{203a}']),             // ‹ ›
    ('\u{203a}', &['\u{2039}']),             // › ‹
    ('\u{300a}', &['\u{300b}']),             // 《 》
    ('\u{3008}', &['\u{3009}']),             // 〈 〉
    ('\u{00bb}', &['\u{00ab}', '\u{00bb}']), // » « »
    ('\u{00ab}', &['\u{00ab}', '\u{00bb}']), // « » »
    ('\u{300c}', &['\u{300d}']),             // 「 」
    ('\u{300e}', &['\u{300f}']),             // 『 』
];

fn closing_quotes(open: char) -> Option<&'static [char]> {
    QUOTE_PAIRS
        .iter()
        .find(|(o, _)| *o == open)
        .map(|(_, c)| *c)
}

/// Calibre's `title_sort`: move a leading article to the end, comma-separated.
///
/// Transcribed from `calibre.ebooks.metadata.title_sort`. Calibre's algorithm:
///
/// 1. strip the title;
/// 2. if the first character opens a quote and the last closes it, drop both;
/// 3. match `^(A|The|An)\s+` case-insensitively; if it matches, move the matched
///    prefix to the end after a comma;
/// 4. if the result now *begins* with a quote, strip that pair too;
/// 5. strip again.
///
/// The article keeps **the caller's own casing**, because Calibre slices it out
/// of the input (`title[len(prep):] + ', ' + prep`) rather than rebuilding it
/// from the pattern. That is why `the Hobbit` becomes `Hobbit, the` and
/// `The Hobbit` becomes `Hobbit, The`.
///
/// Calibre also honours a `strictly_alphabetic` mode and a per-language article
/// list. Neither is taken here: the SQL trigger is registered under Calibre's
/// default English configuration.
pub fn title_sort(title: &str) -> String {
    let t = title.trim();

    // Step 2: a matched quote pair around the whole title comes off first.
    let t = strip_quote_pair(t);

    // Step 3: ^(A|The|An)\s+, case-insensitive.
    if let Some(article) = match_leading_article(&t) {
        let rest = t[article.len()..].trim_start();
        if !rest.is_empty() {
            // Steps 4 and 5.
            return strip_quote_pair(&format!("{rest}, {article}"))
                .trim()
                .to_string();
        }
    }

    // Step 5: nothing matched, so the title is already its own sort key.
    t.trim().to_string()
}

/// Drop a leading opening quote, and the matching closing quote if present.
///
/// Calibre strips the opening character as soon as it is a quote character —
/// it does **not** require a matching closer:
///
/// ```python
/// if title and title[0] in quote_pairs:
///     q = title[0]
///     title = title[1:]
///     if title and title[-1] in quote_pairs[q]:
///         title = title[:-1]
/// ```
///
/// So `"A Tale` becomes `A Tale` and then sorts as `Tale, A`. The earlier
/// version of this function only stripped matched pairs, which disagreed with
/// Calibre on exactly that input.
fn strip_quote_pair(s: &str) -> String {
    let Some(first) = s.chars().next() else {
        return String::new();
    };
    let Some(closers) = closing_quotes(first) else {
        return s.to_string();
    };
    let body = &s[first.len_utf8()..];
    match body.chars().last() {
        Some(last) if closers.contains(&last) => body[..body.len() - last.len_utf8()].to_string(),
        // Unmatched opener: Calibre still removes it.
        _ => body.to_string(),
    }
}

/// Match `^(A|The|An)\s+` case-insensitively, returning the matched article
/// **with the caller's own casing**.
///
/// Mirrors Python's ordered-alternation semantics: alternatives are tried in the
/// order Calibre lists them. Walking longest-first is *not* equivalent, so the
/// list order is preserved exactly.
fn match_leading_article(title: &str) -> Option<&str> {
    for pattern in ARTICLES {
        // Strip the trailing `\s+` to get the bare article word.
        let word = pattern.trim_end_matches(r"\s+");
        let Some(head) = title.get(..word.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(word) {
            continue;
        }
        // `\s+` requires at least one whitespace after the article. This is what
        // keeps "Android" from sorting as "droid, An".
        if title[word.len()..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            return Some(head);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// author_sort
// ---------------------------------------------------------------------------

/// `author_name_copywords` — a name containing any of these is a body, not a
/// person, and is never inverted.
const AUTHOR_COPYWORDS: &[&str] = &[
    "agency",
    "corporation",
    "company",
    "co.",
    "council",
    "committee",
    "inc.",
    "institute",
    "national",
    "society",
    "club",
    "team",
    "software",
    "games",
    "entertainment",
    "media",
    "studios",
];

/// `author_name_prefixes` — honorifics skipped from the front.
const AUTHOR_PREFIXES: &[&str] = &["mr", "mrs", "ms", "dr", "prof"];

/// `author_name_suffixes` — generational/academic suffixes moved to the end.
const AUTHOR_SUFFIXES: &[&str] = &[
    "jr", "sr", "inc", "ph.d", "phd", "md", "m.d", "i", "ii", "iii", "iv", "junior", "senior",
];

/// `author_surname_prefixes`. Only consulted when `author_use_surname_prefixes`
/// is true, which Calibre's default tweaks set to **False**; exposed so the
/// table is not silently lost if that default ever changes.
pub const AUTHOR_SURNAME_PREFIXES: &[&str] = &["da", "de", "di", "la", "le", "van", "von"];

/// Calibre's `author_sort(name)`: `Last, First`.
///
/// Transcribed from `calibre.ebooks.metadata.author_to_author_sort` with
/// Calibre's default tweaks (`author_sort_copy_method = 'comma'`,
/// `author_use_surname_prefixes = False`).
///
/// The rules that are easy to get wrong, and which this therefore implements
/// exactly:
///
/// * a name that **already contains a comma** is returned unchanged, because
///   `method == 'comma'` short-circuits — re-inverting `Le Guin, Ursula K.`
///   would give the nonsense `Ursula K., Le Guin`;
/// * a name containing a **copyword** (`Company`, `Games`, `National`, …) is
///   returned unchanged, because it is an organisation; inverting `Acme Games`
///   to `Games, Acme` is exactly the bug this rule prevents;
/// * a single-token name is returned unchanged, and gets **no** comma;
/// * leading honorifics are dropped from the sort position, generational
///   suffixes are appended after it.
pub fn author_sort(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    // method == 'comma': an existing comma means the caller already chose an
    // order, and Calibre leaves it alone.
    if name.contains(',') {
        return name.to_string();
    }

    let tokens: Vec<&str> = name.split_whitespace().collect();
    if tokens.len() < 2 {
        return name.to_string();
    }

    // A copyword anywhere in the name means this is an organisation.
    if tokens
        .iter()
        .any(|t| AUTHOR_COPYWORDS.contains(&t.to_lowercase().as_str()))
    {
        return name.to_string();
    }

    // Skip honorifics from the front.
    let mut first = 0;
    while first < tokens.len() && is_author_prefix(tokens[first]) {
        first += 1;
    }
    if first == tokens.len() {
        // Every token was an honorific; Calibre returns the input unchanged.
        return name.to_string();
    }

    // Skip generational/academic suffixes from the back.
    let mut last = tokens.len() - 1;
    while last > first && is_author_suffix(tokens[last]) {
        last -= 1;
    }

    let suffix = tokens[last + 1..].join(" ");

    // Reorder: surname, then the remaining forenames, then the suffix.
    let mut atokens: Vec<&str> = Vec::with_capacity(tokens.len() + 1);
    atokens.push(tokens[last]);
    atokens.extend_from_slice(&tokens[first..last]);

    let num_toks = atokens.len();
    if !suffix.is_empty() {
        atokens.push(suffix.as_str());
    }

    let mut out = atokens.join(" ");
    // Calibre attaches the comma to the first token only when there is more than
    // one, so a single-token name never gains a trailing comma.
    if num_toks > 1 {
        out.insert(atokens[0].len(), ',');
    }
    out
}

/// Does this token start with an honorific, with or without a trailing dot?
///
/// Calibre builds `prefixes |= {y + '.' for y in prefixes}` before comparing, so
/// both `Dr` and `Dr.` match.
fn is_author_prefix(token: &str) -> bool {
    let t = token.to_lowercase();
    // Calibre compares the whole token against the bare prefix and against the
    // prefix with a trailing dot added, so "Dr" and "Dr." both match "dr".
    AUTHOR_PREFIXES
        .iter()
        .any(|p| t == *p || t == format!("{p}."))
}

/// Does this token end with a generational or academic suffix, with or without
/// a trailing dot?
fn is_author_suffix(token: &str) -> bool {
    let t = token.to_lowercase();
    // Same rule as prefixes: the token matches a suffix bare or with one dot,
    // so "Jr." matches "jr". Suffixes already containing a dot ("ph.d", "m.d")
    // are in the list in that form, and adding another dot never matches, which
    // is the same as Calibre's behaviour.
    AUTHOR_SUFFIXES
        .iter()
        .any(|s| t == *s || t == format!("{s}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- title_sort, per Calibre 9.15 title_sort ------------------------------

    #[test]
    fn article_is_moved_to_the_end() {
        assert_eq!(title_sort("A Tale of Two Cities"), "Tale of Two Cities, A");
    }

    #[test]
    fn article_case_is_preserved() {
        // Calibre slices the article out of the input, so its casing survives.
        assert_eq!(title_sort("The Hobbit"), "Hobbit, The");
        assert_eq!(title_sort("the Hobbit"), "Hobbit, the");
        assert_eq!(title_sort("THE HOBBIT"), "HOBBIT, THE");
    }

    #[test]
    fn titles_without_an_article_are_untouched() {
        for t in ["dune", "Dune", "1984", "Neuromancer", "Blade Runner"] {
            assert_eq!(title_sort(t), t);
        }
    }

    #[test]
    fn a_title_that_is_only_an_article_is_untouched() {
        assert_eq!(title_sort("The"), "The");
        assert_eq!(title_sort("A"), "A");
    }

    #[test]
    fn a_word_starting_with_an_article_is_not_an_article() {
        // `\s+` is required after the article, so none of these are articles.
        assert_eq!(title_sort("Android"), "Android");
        assert_eq!(title_sort("Theory of Everything"), "Theory of Everything");
        assert_eq!(title_sort("Annabelle"), "Annabelle");
    }

    #[test]
    fn non_english_articles_are_left_alone() {
        // Calibre's English list is only A/The/An. "Die" and "Les" are handled
        // by its per-language list, which this function does not take, so under
        // the default English configuration they are not articles.
        assert_eq!(title_sort("Die Verwandlung"), "Die Verwandlung");
        assert_eq!(title_sort("Les Misérables"), "Les Misérables");
    }

    #[test]
    fn an_unmatched_opening_quote_is_still_removed() {
        // Calibre strips the opener without requiring a closer.
        assert_eq!(title_sort("\"A Tale"), "Tale, A");
    }

    #[test]
    fn quoted_titles_lose_their_quotes() {
        assert_eq!(
            title_sort("\"A Tale of Two Cities\""),
            "Tale of Two Cities, A"
        );
        assert_eq!(title_sort("\u{201c}The Hobbit\u{201d}"), "Hobbit, The");
        // An unmatched opener is still removed — see the test below for why.
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(title_sort("  Dune  "), "Dune");
        assert_eq!(title_sort("  The Hobbit "), "Hobbit, The");
    }

    // -- author_sort, per Calibre 9.15 author_to_author_sort -------------------

    #[test]
    fn inverts_a_personal_name() {
        assert_eq!(author_sort("Ursula K. Le Guin"), "Guin, Ursula K. Le");
        assert_eq!(author_sort("Jane Doe"), "Doe, Jane");
    }

    #[test]
    fn a_single_token_name_is_unchanged() {
        assert_eq!(author_sort("Homer"), "Homer");
        assert_eq!(author_sort("Plato"), "Plato");
    }

    #[test]
    fn an_existing_comma_is_left_alone() {
        // Re-inverting would give "Ursula K., Le Guin", which is nonsense.
        assert_eq!(author_sort("Le Guin, Ursula K."), "Le Guin, Ursula K.");
        assert_eq!(author_sort("Guin, Ursula K. Le"), "Guin, Ursula K. Le");
    }

    #[test]
    fn copywords_block_inversion() {
        // Organisations, not people. This is the rule that stops "Acme Games"
        // becoming "Games, Acme". The match is on a whole token, so
        // "Something Soft" is inverted while "National Geographic" is not.
        assert_eq!(author_sort("Acme Games"), "Acme Games");
        assert_eq!(author_sort("National Geographic"), "National Geographic");
        assert_eq!(author_sort("Team Rocket"), "Team Rocket");
        assert_eq!(author_sort("The Media Company"), "The Media Company");
        assert_eq!(author_sort("Something Soft"), "Soft, Something");
        assert_eq!(author_sort("BBC"), "BBC");
    }

    #[test]
    fn honorifics_are_dropped_with_or_without_a_dot() {
        // Calibre matches the token against the bare prefix and against the
        // prefix with a dot appended, so "Dr" and "Dr." are both stripped.
        assert_eq!(author_sort("Dr Jane Doe"), "Doe, Jane");
        assert_eq!(author_sort("Dr. Jane Doe"), "Doe, Jane");
        assert_eq!(author_sort("Mr. John Smith"), "Smith, John");
        assert_eq!(author_sort("Prof. John Smith"), "Smith, John");
    }

    #[test]
    fn suffixes_move_to_the_end_with_or_without_a_dot() {
        // The suffix is re-emitted as the caller wrote it, dot and all, so
        // "John Doe Jr." sorts as "Doe, John Jr." and not "Doe, John Jr".
        assert_eq!(author_sort("John Doe Jr"), "Doe, John Jr");
        assert_eq!(author_sort("John Doe Jr."), "Doe, John Jr.");
        assert_eq!(
            author_sort("Martin Luther King Jr."),
            "King, Martin Luther Jr."
        );
    }

    #[test]
    fn an_all_honorific_name_is_unchanged() {
        assert_eq!(author_sort("Dr Prof"), "Dr Prof");
    }

    // -- registration ---------------------------------------------------------

    #[test]
    fn uuid4_has_v4_layout_and_varies() {
        // Exercised through the registered SQL function, since that is the path
        // Calibre's trigger actually takes.
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let a: String = conn
            .query_row("SELECT uuid4()", [], |r| r.get(0))
            .expect("uuid4() must be callable from SQL");
        let b: String = conn.query_row("SELECT uuid4()", [], |r| r.get(0)).unwrap();
        assert_ne!(a, b, "each call must differ");
        assert_eq!(a.len(), 36, "canonical uuid string length");
        assert_eq!(&a[14..15], "4", "version nibble must be 4");
        assert!(
            matches!(&a[19..20], "8" | "9" | "a" | "b"),
            "variant nibble must be 8/9/a/b, got {}",
            &a[19..20]
        );
    }

    #[test]
    fn title_sort_is_callable_from_sql() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let got: String = conn
            .query_row("SELECT title_sort('The Hobbit')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, "Hobbit, The");
    }
}
