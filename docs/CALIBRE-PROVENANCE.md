# Calibre interop: provenance and how to re-verify

Everything in `lorebook-calibre` that claims to match Calibre is checked against
a real Calibre, not against assumptions. This file records where the knowledge
came from and how to redo it when Calibre changes.

## The one fact that makes this hard

Calibre registers Python functions with SQLite at runtime. Its own triggers call
them:

```sql
CREATE TRIGGER books_insert_trg AFTER INSERT ON books
    BEGIN
        UPDATE books SET sort=title_sort(NEW.title), uuid=uuid4() WHERE id=NEW.id;
```

`title_sort` and `uuid4` only exist while Calibre itself has the database open.
A plain SQLite client — which this app is — therefore **cannot insert a book into
a real Calibre library at all**: the trigger fires and fails with
`no such function: title_sort`.

There is no SQL workaround. The options are to drop the triggers (which breaks
the interop guarantee — Calibre's own UI would then show wrong data), or to
supply the functions ourselves. This crate does the latter:
`functions::register` installs `title_sort`, `uuid4`, `author_sort`,
`sortconcat` and `books_list_filter` before any write.

## Where the implementations come from

`title_sort` and `author_sort` are transcribed from Calibre source, not
reconstructed from observed behaviour. The first version of `title_sort` here
*was* reconstructed, and used a 13-article list that was wrong in 10 places;
Calibre's English list is exactly `A`, `The`, `An`.

| What | Where in Calibre |
|---|---|
| `title_sort` | `src/calibre/ebooks/metadata/__init__.py` |
| `sortconcat` (aggregate, 2 args) | `src/calibre/db/backend.py` → `SortedConcatenate` |
| `concat` (aggregate, 1 arg) | same file → `Concatenate` |
| `author_to_author_sort` | same file |
| `get_title_sort_pat` | same file |
| `quote_pairs` | same file |
| English articles | `resources/default_tweaks.py` → `per_language_title_sort_articles['eng']` |
| author copywords/prefixes/suffixes | same file → `author_name_*` |
| `author_sort_copy_method` | same file → `'comma'` |
| schema (tables/triggers/views) | extracted from a generated `metadata.db` |

Version checked: **Calibre 9.15** (`numeric_version = (9, 15, 100)`), commit
`cd29107` of <https://github.com/kovidgoyal/calibre>.

Note that the local install is a compiled bundle at `/opt/calibre`; its shipped
`resources/default_tweaks.py` is the authoritative copy of the tweak values and
is what the expected-output generator reads.

## Three layers of verification

**1. Unit tests** (`crates/lorebook-calibre/src/functions.rs`) — pin the
behaviour that matters, including the cases that are easy to get wrong:
corporate copywords, honorifics with and without a trailing dot, unmatched
opening quotes, non-English articles.

**2. Differential test** (`tests/matches_calibre.rs`) — runs Calibre's *own*
Python over a corpus of 54 titles and 49 author names, and asserts our Rust
matches every one. The expected values live in `tests/calibre_expected.json`, so
this runs without Python or Calibre installed.

To regenerate against a new Calibre:

```sh
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/kovidgoyal/calibre /tmp/calibre
cd /tmp/calibre && git sparse-checkout set src/calibre/ebooks src/calibre/utils
python3 tools/gen_calibre_expected.py     # writes tests/calibre_expected.json
cargo test -p lorebook-calibre --test matches_calibre
```

The generator `exec`s the real function bodies out of Calibre's source with the
tweak values parsed from `default_tweaks.py`, so the oracle is Calibre's code
rather than a reimplementation of it.

**3. End-to-end** (`tests/interop_with_calibre.sh`) — drives a real `calibre`
binary and checks both directions:

- we create a library → `calibredb` reads the book, author and identifier back;
- Calibre's own triggers computed our `sort` and `uuid`;
- Calibre opens the library after we extend it, and our tables survive;
- we open and read a library Calibre created.

It exits 77 (skip) when no `calibre` binary is present, so it does not break
`cargo test` elsewhere.

### Use `calibredb`, not `calibre list`

`calibre list` reads Calibre's own on-disk cache and prints nothing for a
library it has never opened — it will report an empty library for writes that
worked perfectly. `calibredb list` queries the database directly, which is the
claim actually under test. The shell test uses `calibredb` for this reason.

## Schema extraction

`src/calibre_schema.sql` is generated from a real library, not written by hand.
Two things about that are easy to get wrong and were both wrong here at some
point:

- `sqlite_master.sql` stores each statement **without** a trailing semicolon, so
  every statement is re-terminated. Pasting a statement with no terminator makes
  the *next* `CREATE` a syntax error, which surfaces as a confusing
  `near "CREATE"` far from the real problem.
- **Triggers, indexes and views are not optional.** `books_insert_trg` computes
  `sort` and `uuid`; `books_pages_link_create_trigger` maintains page counts; and
  the `meta` view is what Calibre's own UI reads. A library missing any of them
  opens without error and then displays nothing — or fails on the first insert.
  A first extraction filtered by table name and silently dropped all 35 triggers
  and all 11 views.

Regenerate it after any Calibre upgrade with `tools/extract_schema.py`.

## `concat` and `sortconcat` are aggregates, not scalars

The `meta` view reads:

```sql
(SELECT sortconcat(bal.id, name) FROM books_authors_link ...) authors
(SELECT concat(name)         FROM tags ...)               tags
(SELECT concat(format)      FROM data ...)               formats
```

Both are **aggregate** functions in Calibre, registered with
`createaggregatefunction`. An earlier version of this crate registered
`sortconcat` as a *scalar* and did not register `concat` at all.

SQLite accepts both registrations without complaint, which is what makes this a
silent bug rather than a loud one. A scalar function sees one row at a time, so
a book with three authors reports one name, and a book with no tags reports NULL
per row rather than NULL for the group. The list screen then shows most books
with no author — indistinguishable from a data problem, and unaffected by any
test that does not specifically count authors.

Both are now implemented as `Aggregate` in `src/functions.rs`, transcribed from
Calibre's `Concatenate` / `SortedConcatenate`:

- comma separator, matching `Concatenate(sep=',')`, the default Calibre
  constructs them with;
- NULL inputs skipped, not stringified (`if value is not None`);
- **NULL, not `""`, for an empty group** — the difference decides whether the UI
  treats a book as untagged;
- `sortconcat` stores values by index and emits them in ascending index order,
  which is what gives `meta.authors` Calibre's author ordering rather than
  link-table order.

`tests/meta_view.rs` asserts the behaviour directly (comma joining, index
ordering, NULL-when-empty, NULL-skipping) and end-to-end through `meta`: a
three-author book must report three names. That last assertion is the one that
would have caught it.

**Do not "simplify" these into scalar functions.** rusqlite's `Aggregate` trait
is the only way to get a `finalize` step, and without `finalize` there is no
joined string to return.

## What is deliberately not implemented

- **Per-language article lists.** Calibre's `title_sort` takes a `lang`
  parameter and consults `per_language_title_sort_articles` for it. The SQL
  trigger is registered under the default English configuration, so this crate
  implements the English list only. Sorting by a German article would need the
  `lang` argument plumbed through.
- **`title_series_sorting = 'strictly_alphabetic'`.** A user tweak that disables
  article handling entirely. Not taken.
- **`remove_bracketed_text`.** Calibre strips `(...)` from author names before
  sorting. The differential test's author corpus contains no bracketed names, so
  this is a known gap rather than a verified behaviour.
- **Calibre's FTS tables** (`annotations*`, `annotations_fts*`). Not read or
  written by this app; a library without them is still valid.
