# Lorebook

A local-first ebook library manager. Reads and writes **Calibre libraries
directly** — the same `metadata.db` your existing library uses — so there is no
import step and no second copy of your books.

Rust core, Tauri desktop shell, SvelteKit UI.

| Document | What it is |
|---|---|
| `docs/SPECIFICATION.md` | The product spec - what it is and why |
| `docs/PLAN.md` | The implementation plan - sequence, exact files, per-step verification |
| `docs/CALIBRE-PROVENANCE.md` | How Calibre compatibility is verified, and how to re-verify after an upgrade |

Start with `docs/PLAN.md` to implement; it references the spec for design
reasoning rather than restating it.

## Status

**M2 — Storage model.** Built and verified. See below; M1 is still open on one
environment-blocked check.

**M1 — Calibre interop core and app shell.** Built and verified at the library
level. The app launches and its window maps, but the window does not paint in
this environment, so the final hop — webview loads the UI, IPC round trip — is
unverified. See `docs/PLAN.md` M1.4 for exactly what was and was not checked.

Done:
- Open an existing Calibre library; create a new one Calibre can open.
- Read and write Calibre's own tables (`books`, `authors`, `tags`, `series`,
  `data`, `identifiers`, link tables) **unmodified**.
- Register the SQL functions Calibre's triggers call, so Calibre's own triggers
  compute `sort`, `uuid` and page counts exactly as they would under Calibre.
- Reimplementation of `title_sort`, `author_sort`, `concat` and `sortconcat`
  transcribed from Calibre 9.15 source, byte-exact against Calibre's own Python
  across 103 cases.
- Additive, namespaced tables for Lorebook's own data (`book_sources`,
  `scan_roots`, `reading_state`, …), applied idempotently and invisible to
  Calibre.
- Tauri v2 shell and SvelteKit UI: open or create a library, page through its
  books, see one book's detail. Svelte 5, static adapter, IPC over `invoke`.

Verified:
- Our listing of a Calibre-created library is identical to `calibredb list` —
  same books, authors, tags, series, and the same article-stripped ordering.
- 85 cargo tests, `svelte-check` clean, clippy clean.
- 11 end-to-end checks against a real `calibre` binary, in both directions.

### M2 — Storage model

Done:
- **A book's identity is its bytes.** BLAKE3 content hashing, streamed so a
  900 MB scanned PDF does not need 900 MB of memory. The stored value is
  versioned (`blake3:<hex>`), so a hash from a different build is recognisable
  rather than silently mismatching.
- **Directory scanning** in a new `lorebook-scan` crate. Recursive or
  shallow, format-filtered, incremental: an unchanged file (same size and mtime)
  is not re-hashed, which is what makes a re-scan cheap.
- **Duplicate detection by content.** A file whose hash is already in the
  library adds no book — it becomes another source on the book that exists. The
  same bytes as `.epub` and `.pdf` is one book with two sources.
- **Absence is a state, never a deletion.** A file that has vanished is marked
  `state = 'missing'`; the row and the book survive, because the file may be on
  an unmounted drive. When the drive comes back the state clears itself.
- **Never silently adopt.** A scan can only ever write `reference` or `symlink`
  rows. The only function that can produce `managed` is `adopt_source`, which
  copies the file into the library and is not reachable from `scan`. Tested, not
  asserted: a `kind` is just a string in a database, so a scan of a tree
  including a symlink asserts that no row is managed.
- **Symlinks are opt-in.** Following is off by default, because a symlink into a
  home directory turns "scan my books folder" into "hash my entire filesystem".

Verified: 85 cargo tests (M2 adds 25), clippy clean across the workspace, and
the 11-check real-Calibre interop suite still passes — extending the library has
not broken Calibre reading it.

Not done in M2: the scan is not yet exposed over IPC to the UI, and the title of
a scanned book is its filename stem until the M3 curation pipeline parses real
metadata.

Not done: M1's window-level check — the app starts and the window maps, but
WebKitGTK does not paint under this Hyprland session, so the UI itself is
unverified. Then everything from M2 on: scanning, curation, search, templates,
plugins. `book_sources` and the additive schema are in place for them.

## The interop problem, in one paragraph

Calibre registers `title_sort()` and `uuid4()` as Python functions on its SQLite
connection, and its `books_insert_trg` trigger calls both. Those functions only
exist while Calibre has the database open, so any other SQLite client fails on
the first insert with `no such function: title_sort`. This crate implements
them in Rust and registers them, so Calibre's triggers keep working untouched.
That is the whole design: **we supply the functions Calibre's triggers call,
rather than replacing or disabling the triggers.**

## Layout

```
crates/lorebook-core       domain types (Book, BookSource, SourceKind, …)
crates/lorebook-calibre    Calibre metadata.db interop
  src/functions.rs         Calibre's SQL functions, transcribed from its source
  src/calibre_schema.sql   generated from a real library — do not hand-edit
  src/additive.sql         Lorebook's own tables
  tests/interop.rs         27 tests against a real Calibre database
  tests/matches_calibre.rs differential test vs Calibre's own Python
  tests/interop_with_calibre.sh   11 end-to-end checks against a real binary
crates/lorebook-interop-check  helper binary used by the shell test
tools/                     extract_schema.py, gen_calibre_expected.py
fixtures/                  a real Calibre 9.15 metadata.db, used by the tests
```

## Building and testing

```sh
cargo build
cargo test          # 48 tests, no external dependencies needed
```

The end-to-end interop test needs `calibre` on `PATH` and exits 77 (skip)
without it:

```sh
bash crates/lorebook-calibre/tests/interop_with_calibre.sh
```

Regenerating the Calibre-derived files after a Calibre upgrade:

```sh
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/kovidgoyal/calibre /tmp/calibre
cd /tmp/calibre && git sparse-checkout set src/calibre/ebooks src/calibre/utils
cd -
python3 tools/extract_schema.py                       # needs `calibre` binary
CALIBRE_SRC=/tmp/calibre python3 tools/gen_calibre_expected.py
cargo test -p lorebook-calibre
```

## Testing philosophy

Interop claims are only worth as much as the evidence behind them, so there are
three independent layers, and a change to any Calibre-facing behaviour has to
pass all three:

1. **Unit tests** pin the behaviour that matters and the cases that are easy to
   get wrong.
2. **A differential test against Calibre's own Python.** The first version of
   `title_sort` here was written from observed behaviour and used an article
   list that was wrong in 10 of 13 entries; Calibre's English list is `A`,
   `The`, `An`. Reading Calibre's source is what caught it, and the differential
   test is what keeps it caught.
3. **End-to-end against a real `calibre` binary**, in both directions: a library
   we create that Calibre reads, and a library Calibre creates that we read.
