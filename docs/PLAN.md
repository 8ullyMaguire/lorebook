# Lorebook — Implementation Plan

**Status:** M1 in progress. This plan is the execution contract for the whole
project. An implementer (human or LLM) should be able to work through it without
consulting the spec for design decisions — the spec explains *why*, this
explains *how* and *what proves it worked*.

**How to use this document.** Work the milestones in order. Each task names exact
files, gives complete code, and ends with a verification command and its expected
output. Run the verification. If it does not print what is stated, stop and fix
before moving on — do not proceed on the assumption it will pass later.

**Spec precedence.** `docs/SPECIFICATION.md` is authoritative for *what* and
*why*. This plan is authoritative for *sequence and verification*. Where they
conflict, the spec wins, and the conflict is corrected in the same commit
(spec §2). Where this plan is silent, the spec decides.

**Core rule, inherited from the spec and not to be broken:** Calibre's own
tables are read and written **unmodified**. Everything Lorebook needs that
Calibre does not have lives in additive, namespaced tables
(`crates/lorebook-calibre/src/additive.sql`).

---

## Current state, verified

Verified by inspection on 2026-09-26. These are facts about the tree, not
intentions.

- Commit `8221370`, tag `v0.1.0-calibre-interop`. Working tree clean.
- **59 tests pass** (`cargo test`): 18 unit in `lorebook-calibre`, 27 integration
  in `tests/interop.rs`, 3 differential in `tests/matches_calibre`, 11 in
  `tests/meta_view.rs` (the `meta` view and paging).
- **11 interop checks pass** against a real Calibre 9.15 binary
  (`bash crates/lorebook-calibre/tests/interop_with_calibre.sh`).
- `cargo clippy --all-targets` is warning-free.
- Crates existing: `lorebook-core` (types only, no logic), `lorebook-calibre`
  (interop, complete for the M1 backend scope), `lorebook-interop-check` (test
  helper binary).
- **Not started:** the Tauri app, the SvelteKit UI, `book_sources` population,
  scanning, search, templates, plugins.

### What M1 already gives us, and what it does not

`lorebook-calibre` can open and create libraries, and read/write books, authors,
tags, series, identifiers and formats in Calibre's own tables. The additive
schema exists and applies idempotently.

**Not yet:** any UI, any Tauri wiring, and no `book_sources` rows are ever
written by the app itself (only the table definition ships). The scan pipeline
that populates it is M2.

### Verified Calibre facts this plan depends on

Established by reading Calibre 9.15 source, not assumed. Full detail and
re-verification instructions in `docs/CALIBRE-PROVENANCE.md`.

| Fact | Consequence for this plan |
|---|---|
| `books_insert_trg` calls `title_sort()`, `uuid4()` | We must register both before any write, or every insert fails. Done in `functions::register`. |
| `books_insert_trg` also overwrites `sort` | Never pass `sort` expecting it to stick. Computed columns are Calibre's. |
| `identifiers` is `UNIQUE(book, type)` | One identifier per scheme per book. A second one *replaces*. |
| `data` is one row per format, never a path | A path only exists in our `book_sources`. A Calibre-created book legitimately has none. |
| `meta` is a VIEW, not a table | Calibre's UI reads it. A library without it opens but displays nothing. |
| Schema version is `PRAGMA user_version = 27` | `create_library` must set it or Calibre treats the library as ancient and re-migrates it. |
| `books_pages_link` is maintained by a trigger | Do not insert into it manually. |

---

## M1 — Core (Tauri shell, library view)

**Goal:** a running desktop app that opens a Calibre library and lists its books.

**Progress.** M1.1 (Tauri scaffold) and M1.2 (`Library` newtype, pagination,
commands) are done and compiling. M1.3 (SvelteKit UI) and M1.4 (the completion
gate) remain.

One thing M1.2 corrected in the code, not just the plan: `sortconcat` and
`concat` are Calibre **aggregate** functions. They had been registered as
scalars, which SQLite accepts and which silently reports one author for a
three-author book. See `docs/CALIBRE-PROVENANCE.md`.

### M1.1 — Tauri scaffold

Create `src-tauri/` and `ui/`. Do **not** use `create-tauri-app` interactively;
write the files, so the result is reviewable and reproducible.

`src-tauri/Cargo.toml`:
```toml
[package]
name = "lorebook"
version = "0.1.0"
edition = "2021"
rust-version = "1.77"

[lib]
name = "lorebook_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-dialog = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
lorebook-core = { path = "../crates/lorebook-core" }
lorebook-calibre = { path = "../crates/lorebook-calibre" }

[features]
custom-protocol = ["tauri/custom-protocol"]
```

Add `src-tauri` to the workspace `members` in the root `Cargo.toml`.

`src-tauri/tauri.conf.json` — window 1200×800, title "Lorebook", devUrl
`http://localhost:5173`, `frontendDist: "../ui/dist"`.

`src-tauri/src/lib.rs` — the app entry, holding library state:
```rust
pub struct AppState {
    /// The open library, or None before one is chosen. Held behind a Mutex
    /// because Tauri commands are async and rusqlite Connection is !Sync.
    library: Mutex<Option<cal::Library>>,
}
```

**Verify:** `cd src-tauri && cargo check` → `Finished` with no errors.
**Note:** Tauri needs `libwebkit2gtk-4.1-dev`. If `pkg-config` cannot find it,
stop and report it — do not substitute a different webkit version.

### M1.2 — Library state and the open/create commands

The existing API is **free functions over `&Connection`**, returning
`Result<T, CalibreError>`:

| Function | Purpose |
|---|---|
| `open_library(&Path) -> Result<Connection>` | open an existing library |
| `create_library(&Path) -> Result<Connection>` | create one Calibre can open |
| `verify_schema(&Connection) -> Result<()>` | assert the schema is one we understand |
| `list_books(&Connection) -> Result<Vec<Book>>` | all books |
| `get_book(&Connection, i64) -> Result<Option<Book>>` | one book |
| `list_sources(&Connection, i64) -> Result<Vec<BookSource>>` | Lorebook's own file sources |
| `list_authors / list_tags / list_identifiers` | reference data |

`open_library` and `create_library` already call `functions::register`; anything
that opens a `Connection` another way must not assume that happened.

**M1.2 must add two things, and they are the real work of this step:**

**1. A `Library` newtype, because rusqlite's `Connection` is `!Sync` and Tauri
requires `Send + Sync` state.** Keep the free functions; the newtype only owns
the connection and the path:

```rust
pub struct Library { conn: Connection, path: PathBuf }

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { conn: open_library(path)?, path: path.to_path_buf() })
    }
    pub fn create(path: &Path) -> Result<Self> {
        Ok(Self { conn: create_library(path)?, path: path.to_path_buf() })
    }
    pub fn conn(&self) -> &Connection { &self.conn }
    pub fn path(&self) -> &Path { &self.path }
}
```

**2. Pagination.** `list_books` returns **every** book. A 50k-book library
serialised into a webview will hang the UI, so the Tauri command must not call
`list_books`. Add:

```rust
/// One page of books. Reads through Calibre's `meta` view — the same view
/// Calibre's own UI reads — so our listing and Calibre's cannot drift.
pub fn list_books_page(conn: &Connection, limit: i64, offset: i64)
    -> Result<Vec<Book>>;

/// Total row count, for the UI's page count.
pub fn count_books(conn: &Connection) -> Result<i64>;
```

`list_books_page` must `SELECT ... FROM meta` with `LIMIT ?1 OFFSET ?2` ordered
by `sort`, because `meta` is a view and hand-joining the same tables is how
listing and Calibre's listing come to disagree.

**Verified trap:** `meta` calls `sortconcat()`, so the view is *only* queryable
once the Calibre SQL functions are registered. A bare `sqlite3` CLI against a
user's library fails with `no such function: sortconcat`. Any tool that reads
`meta` — including a future content server — must register the functions too.
`tests/meta_view.rs` pins this, along with the paging behaviour M1.2 depends
on.

Three Tauri commands, each thin, over a `Mutex<Option<Library>>`:
- `open_library(path: String) -> Result<i64, String>` (returns the book count)
- `create_library(path: String) -> Result<i64, String>`
- `list_books_page(limit: i64, offset: i64) -> Result<Vec<Book>, String>`

**Verify:** `cargo test -p lorebook-calibre` still passes 59 tests.

### M1.3 — SvelteKit UI: library chooser and book list

`ui/` — Svelte 5 + Vite, static adapter (Tauri serves the built files).

Two views, no router needed yet:
- **Chooser**: "Open library…" and "Create library…" buttons using
  `tauri-plugin-dialog`'s folder picker.
- **Library**: virtualised list of books showing title, authors, formats, and
  the Calibre `#` id. Click a row → detail panel.

State lives in a single `library.svelte.ts` rune holding the open `Library`
handle. Do **not** mirror the whole book list into JS state — call the command
per page. A 50k-book library must not be serialised into the webview.

**Verify:** `cd ui && pnpm check && pnpm build` → both clean.
**Note:** this host uses pnpm with the hoisted linker; build with
`node node_modules/vite/bin/vite.js build`, not `pnpm build`, if the wrapper
misbehaves.

### M1.4 — M1 completion gate

M1 is done when all of these are true. Run them; do not assume.

```sh
# 1. Still green after all the new code
cd /home/alvaro/code-local/rust/lorebook
CARGO_TARGET_DIR=/home/alvaro/.cargo-target/lorebook cargo test
#   expect: 59 passed; 0 failed  (more is fine)

# 2. Calibre interop unbroken
CARGO_TARGET_DIR=/home/alvaro/.cargo-target/lorebook \
  bash crates/lorebook-calibre/tests/interop_with_calibre.sh
#   expect: interop: 11 passed, 0 failed

# 3. The app builds
CARGO_TARGET_DIR=/home/alvaro/.cargo-target/lorebook cargo build --release
cd ui && pnpm build
#   expect: both succeed

# 4. A real Calibre library opens in the app  ← the actual M1 deliverable
#   Launch the app, open a library, confirm the book list matches
#   `calibredb list --with-library=<that library>`.
```

**Do not tag M1 complete until check 4 has been done by hand.** A passing test
suite does not prove the app opens a library.

**Status of check 4, 2026-09-26 — partially done, and the remainder is blocked
on the environment, not on the code.**

What was verified: the release binary launches, `main` runs, and the process
stays alive with live `WebKitWebProcess` and `WebKitNetworkProcess` children.
`hyprctl` reports the window as `class: Lorebook`, `mapped: true`,
`visible: true`, `acceptsInput: true`. So the app starts, GTK initialises, and
the webview spawns.

What could not be verified: the window never paints. Screenshots of the
compositor show the desktop behind it, under both the X11 and the Wayland
backend, and `grim` captures of the reported window geometry contain other
applications' pixels. The window is created and mapped but its surface stays
blank.

That is a WebKitGTK-rendering-under-Hyprland symptom, not evidence about this
app: the frontend builds, `svelte-check` is clean, and the same library opens and
lists correctly through the core API. What is genuinely unproven is the last hop
— that the webview loads `index.html` and the IPC round trip works — because that
requires a painted window to observe.

**To close this: run the app on a session where WebKitGTK paints, and confirm a
library opens and lists.** Until then M1 is not tagged complete.

Commit: `feat(m1): Tauri shell, library open/create, book list view`
Tag: `v0.1.0-m1-core` (move the existing `v0.1.0-calibre-interop` — it marks the
backend half, which is a useful historical marker; do not delete it).

---

## M2 — Storage model (spec §3)

**Goal:** `book_sources` populated by scanning a directory, with managed /
reference / symlink kinds and incremental re-scan.

This is where the app stops being a Calibre viewer and starts being the tool the
spec describes. It is also where the "no silent adoption" rule (§3.3) lives, and
it is the first milestone where a bug can *destroy a user's files*. Read §3
before writing any of it.

### M2.1 — Content hashing and identity

The merge key is the content hash, not the path (spec §3.4). A book's identity
is its bytes.

Add to `lorebook-core`: `ContentHasher` — BLAKE3 (faster than SHA-256 and
Calibre does not care which digest is used, since the hash lives in *our* table).

```
crates/lorebook-core/src/hash.rs
  pub struct ContentHasher;
  pub fn hash_file(path: &Path) -> Result<String>;   // hex, streaming, 1MiB buffer
```

**Verify:** a unit test that hashes a known file and asserts the exact hex
value, plus a test that the same content at two paths yields one hash.

### M2.2 — The scanner

```
crates/lorebook-scan/src/lib.rs
  pub struct ScanRoot { pub id: i64, pub path: PathBuf, pub recursive: bool }
  pub struct ScanOptions { pub formats: Vec<String>, pub follow_symlinks: bool }
  pub struct ScanReport { pub added: usize, pub updated: usize,
                          pub missing: usize, pub errors: Vec<(PathBuf, String)> }
  pub fn scan(conn: &Connection, root: &ScanRoot, opts: &ScanOptions)
             -> Result<ScanReport>;
```

Rules, each with a test:
- A file whose content hash already exists adds **no** book — it becomes a
  `book_sources` row pointing at the existing book (spec §3.4).
- A file already recorded is not re-hashed if size and mtime match
  `book_sources` (this is what makes re-scan cheap).
- A `book_sources` row whose file has vanished is set to `state = 'missing'`,
  **never deleted** — the user's file might be on an unmounted drive.
- Symlinks are recorded as `kind = 'symlink'`, never followed silently; the
  report lists what was skipped.

**Verify:** a test fixture directory of 5 files scanned twice — the second scan
reports 0 added and 0 updated, and every `book_sources` row is unchanged.

### M2.3 — Never silently adopt (§3.3)

A referenced file is never copied or adopted without an explicit user action.
Enforce it in the type system, not in review: the scan API returns
`reference` rows only, and the only function that creates a `managed` row is
`adopt_source`, which requires an explicit call and is not reachable from scan.

**Verify:** a test asserting no code path from `scan` produces `kind='managed'`.

### M2.4 — M2 completion gate

M2 is done when all of these are true. Run them; do not assume.

```sh
cd /home/alvaro/code-local/rust/lorebook
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/lorebook

# 1. The whole workspace is green
cargo test --workspace
#   expect: 85 passed; 0 failed

# 2. Extending the library has not broken Calibre reading it.
#    This is the check that matters most: M2 writes to book_sources and to
#    books/data, and a mistake there shows up as Calibre refusing the file,
#    not as a failing Rust test.
bash crates/lorebook-calibre/tests/interop_with_calibre.sh
#   expect: interop: 11 passed, 0 failed

# 3. No lint debt
cargo clippy --workspace --all-targets
#   expect: no output

# 4. The app still builds
cargo build --release
#   expect: Finished `release` profile
```

**Status, 2026-09-28: all four pass.** 85 tests (60 at M1, plus 8 for the
hasher and 17 for the scanner), interop 11/11, clippy clean, release builds.

**Not required for M2, and not done:** the scan is not exposed over IPC to the
UI yet, so there is no way to point the app at a folder from the interface. That
is a UI milestone, not a storage one. A scanned book's title is its filename stem
until M3 parses real metadata — deliberately, because inventing metadata during
a scan is how a library ends up with plausible wrong titles.

**One bug M2's own tests found, recorded because it is the kind that ships.** The
new-book path originally hardcoded `kind='reference'`, so a symlink that was the
first sighting of its content was recorded as a file the app believed it owned —
and `SourceKind::owns_file` is exactly the predicate that authorises deleting it.
A scan could therefore mark a user's own file as ours to delete. Both paths now
derive the kind from the `SourceKind` enum rather than a string literal, and a
test links to content *outside* the scan root so it actually reaches that code.

Commit: `M2.1` then `M2.2 + M2.3`
Tag: `v0.2.0-m2-storage-model`

---

## M3 — Curation pipeline (spec §3.6)

**Goal:** identify, resolve, enrich, deduplicate, classify. Inbox view.

**The largest and most valuable milestone.** A library view built before this has
unstable identities and no metadata worth filtering by, and has to be rebuilt
rather than restyled. Do not reorder it earlier.

### M3.1 — Filename parsers

Port Calibre's `FileInfo` regex set rather than writing new patterns. The
patterns live in Calibre's source; extract them the same way
`tools/extract_schema.py` extracts the schema, so a Calibre upgrade surfaces a
diff instead of a silent behaviour change.

```
crates/lorebook-parse/src/lib.rs
  pub struct ParsedName { pub title: String, pub author: Option<String>,
                          pub series: Option<String>, pub series_index: Option<f64>,
                          pub index: Option<u32>, pub pubdate: Option<String>,
                          pub tags: Vec<String> }
  pub fn parse(name: &str) -> ParsedName;
```

**Verify:** a table-driven test over ≥20 real-world filenames
(`Le Guin, Ursula K. - The Dispossessed.epub`,
`[author] Title v03 (2020) [AO3].epub`, …) with the expected parse for each.

### M3.2 — Deduplicate with version awareness

The rule from §3.6: same work, different version → one book with many sources.
Same work, same version, different files → **propose**, never merge silently.

Confidence thresholds from the spec, and a test per threshold at the boundary
(0.499 / 0.500 / 0.501) asserting the bucket, because off-by-one here silently
merges or splits a user's library.

### M3.3 — Inbox — **DONE 2026-09-30**

Every uncertain decision lands in `inbox_items` with its evidence. The user
resolves; the app never guesses destructively.

Built: `crates/lorebook-scan/src/{inbox,resolve}.rs`, additive tables in
`lorebook-calibre/src/additive.sql`. Merge is a transaction, so a refusal
leaves the library untouched. Three answers per pair, and `defer` is a true
no-op.

**Verify, and what it actually found.** `cargo test --workspace` → **117
passed, 0 failed**; clippy clean; the 11-check real-Calibre interop suite
passes. The two tests that were red when this milestone was picked up were
red *for the wrong reason* — they blamed the merge path for refusing to merge
a book with no file behind it, and the refusal was correct. Their real defect
was an assumption about the Calibre fixture, and fixing it exposed a production
bug underneath: `calibre_schema.sql` omitted the `annotations` table that
Calibre's own `books_delete_trg` deletes from, so **every book deletion on a
library we create failed** — and a merge is a book deletion. This milestone
could never have completed a merge on a real library.

Two new test files, both mutation-proven (three mutations each, all killing the
suite): `crates/lorebook-calibre/tests/schema_completeness.rs`, and
`resolve.rs::a_merge_works_against_a_library_calibre_itself_wrote`. They prove
different things and only the pair is complete — the fixture-based one cannot
catch our schema omitting a table, because the fixture supplies it, and the
schema-based one names no fixture.

`PairAction::from_str` is now the `FromStr` trait returning
`Result<Self, UnknownPairAction>`, so an unknown stored action is reportable
rather than collapsing into "unresolved".

**Still open in M3:** M3.1 (filename parsers). The end-to-end check written
above — scan a real directory, watch a pair land in the inbox, accept it — runs
against `library_with_books`, i.e. a constructed library, not a scanned
directory on disk. It is the remaining manual gate.

---

## M4–M6 — Search, templates, columns

Ordering is forced (spec §10): **the template language (M5) before composite
columns (M6)**, because §5.1's default column set contains composite columns.
Build M6 first and the app opens with a broken library view.

- **M4** — port Calibre's search parser, compile to SQL, FTS5 index. Port, do
  not reinvent: Calibre's search language is a user-facing compatibility
  surface, and a subtly different grammar is worse than none.
- **M5** — template language with a **fixed** function registry. `#length` and
  `#read_dates` must work verbatim. An unknown function must produce a visible
  error in the cell, never a silent empty string — a silently empty column is
  indistinguishable from a book with no data.
- **M6** — the full §5.1 default column set, column editor, tag browser, column
  colouring, virtual libraries.

---

## M7–M14

Listed with their ordering constraints. Detail belongs in a per-milestone
section written *before* that milestone starts — not now, because writing it now
means writing it from imagination.

| M | Ships | Must come after |
|---|---|---|
| M7 | WASM plugin host, capability grants, event bus | M5 (plugins use the template language) |
| M8 | Built-in plugins: Count Pages, Metadata Backfill, Filename Parser, Action Chains, EpubMerge/Split | **M7** — they are plugins precisely to exercise the same API path a third party will. A plugin API only the app uses is an API nobody has tested. |
| M9 | Instance sync (`lore_metadata`), "instance suggests" per-field | M3 (no signals without the pipeline) |
| M10 | Action chains as an event pipeline, run log | M7, M8 |
| M11 | `reading_state` + `#read_progress` | M5 |
| M12 | Tiered analytics + separate exchange consent | M9 |
| M13 | OPDS + Calibre sync protocol, localhost-bound | M9, M11 |
| M14 | EPUB/AZW3/MOBI readers and writers | M2 |

**M7 is the gating decision for M8.** Do not build the built-in plugins first
"because they're easier" — the spec's reason for making them plugins is the
reason they must come second.

---

## Standing rules for every implementer

These are not style preferences. Violating them breaks something a user cares
about.

1. **Never modify an already-applied migration.** Write a new one. This applies
   to `additive.sql` too, now that it has shipped in a tagged commit.
2. **Never `ALTER` a Calibre-owned table.** If Lorebook needs a field Calibre
   does not have, it goes in an additive table.
3. **Register Calibre's SQL functions before any write.** Skipping this fails
   with `no such function: title_sort` at the *insert*, which reads like a
   schema bug and is not.
4. **Never let Calibre compute something twice.** `sort` and `uuid` are Calibre's
   via trigger. Writing them yourself is redundant at best and divergent at
   worst.
5. **Destructive operations require an explicit user action and a preview.**
   Scan, curate and classify never delete a file and never delete a book row.
   `state = 'missing'` is the mechanism for absence, not deletion.
6. **An unknown template function renders a visible error**, never an empty
   string.
7. **Verify with the command stated.** Each task gives an exact command and its
   expected output. If the output differs, stop.
8. **Commit at every task boundary**, explicit paths only in a repo that may
   have another agent in it. `git add -A` in such a repo absorbs someone else's
   work-in-progress into your commit.

## Adding a milestone's detail section

When a milestone is next to start, add its section to this file *before* writing
code, with the same shape as M1 and M2: goal, exact files, complete code, a
verification command with expected output per step, a completion gate, and the
commit/tag to use. The plan is a living document; a milestone whose plan is
written after the code is a milestone whose design was settled by accident.
