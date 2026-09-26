# Tauri Ebook Library — Spec

**Status:** proposed, not adopted. No code, no repo, no schema written. This is
the spec to review before anything is built.
**Owner:** Alvaro
**Date:** 2026-09-25
**Reference implementation:** Calibre, read from a shallow clone at
`~/code-local/research/calibre` (HEAD `00200f5`, 2026-09-25) and the locally
installed Calibre 9.15.
**Related:** [[lorehaven]] — a different application (fanfiction archive, HTTP
service). It shares vocabulary, not code. Nothing here touches it.

> **Placement note.** This was first drafted into `lorehaven/docs/plans/`, which
> was wrong: it is a separate application with its own lifecycle and no
> dependency on that codebase. It belongs here, and when it is adopted it should
> become its own repository rather than a directory inside either existing
> project.

---

## 0. TL;DR

A local-first desktop ebook library, built in Tauri, that reads and writes a
Calibre `metadata.db` so an existing library is usable immediately, imports
books by reference as well as by copy, ships the Calibre features that earn
their place (custom columns, the template language, the search language, the
plugin system), and is honest about analytics: opt-out, and anything that
depends on analytics is inert when you opt out.

The three decisions that shape everything else:

1. **Calibre's `metadata.db` is the interop target, not an internal format.** We
   read and write Calibre's schema directly. That is a hard compatibility
   guarantee, and it is the reason the app is useful on day one against a
   library that already exists.
2. **Files can be referenced, not only copied.** Calibre always copies
   (hardlink is opt-in). We make in-place the default for scanned folders, which
   is the single largest practical difference from Calibre and the reason this
   is not a clone.
3. **Analytics are opt-out and the dependency is real.** Opting out does not
   merely stop collection; it disables every feature whose value comes from the
   data. A recommendation list that cannot recommend is not shown, not faked
   with local heuristics.
4. **The curation pipeline is the product, not a feature.** This app does not
   assume you know what your files are. A pile of inconsistently-named downloads
   with absent or stale metadata is the input, and the pipeline's job is to
   work out what each one is: identify → resolve → enrich → deduplicate →
   classify → inbox-or-library (§3.6). Without this the app is a better
   Calibre, which is not the point. The inbox (§3.7) holds only what the
   pipeline genuinely cannot resolve, and its size is a design metric, not a
   backlog.
5. **Metadata backfill is in scope; content download is not.** Given a file the
   user already owns, the app fetches catalog metadata from the source site to
   answer "what is this?" (§3.6 stage 3). It never fetches chapters and never
   replaces a downloader (§5.6).
6. **Metadata flows to and from community instances, opt-out.** The app can
   exchange work metadata with configured compatible instances: it sends
   extracted signals (never file contents, never reading history) and receives
   community-curated canonical metadata back (§3.9). Opting out leaves a fully
   functional application; it only loses the community taxonomy.

---

## 1. What Calibre actually is

Read from source rather than from the manual, because several widely-assumed
facts turned out to be false. These findings drive the design.

**The database is plain SQLite and is small.** `resources/metadata_sqlite.sql`
is 663 lines, ~35 tables. The complexity lives in `src/calibre/db/` at 17,106
lines: `cache.py` (4,389) and `backend.py` (3,077) are an aggressive in-memory
cache over a write-through SQLite layer; `legacy.py` (1,291) is a shim over the
pre-9.0 API.

**Tags, authors, series, publishers, languages are many-to-many link tables** —
`books_authors_link`, `books_tags_link`, `books_series_link`,
`books_publishers_link`, `books_languages_link` — over single-row `authors`,
`tags`, `series` tables. The implicit many-to-many table is `books.id` +
format + a sort index. Worth copying: it is what makes `tag:foo and series:bar`
a join rather than a string match.

**Custom columns are schema-per-column, not EAV.** `custom_columns` is a
catalogue:

```sql
CREATE TABLE custom_columns (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    label         TEXT NOT NULL,
    name          TEXT NOT NULL,
    datatype      TEXT NOT NULL,
    mark_for_delete BOOL DEFAULT 0 NOT NULL,
    editable      BOOL DEFAULT 1 NOT NULL,
    display       TEXT DEFAULT '{}' NOT NULL,
    is_multiple   BOOL DEFAULT 0 NOT NULL,
    normalized    BOOL NOT NULL,
    UNIQUE(label)
);
```

Each value then lives in a per-column table created by migration. This is the most
important decision to copy: a custom column is a real typed column, so it
sorts, indexes, and type-checks at the storage layer. An EAV table would give
none of those and would make composite columns unindexable.

**Datatypes are a closed set** matching the UI: text, long text, integers,
floats, rating, date, bool, enumeration (fixed permitted values with per-value
colours), and composite (a template program). `is_multiple` covers the
comma-separated, tag-like columns.

**Full-text search is FTS5 with a custom C++ tokenizer.**
`src/calibre/db/sqlite_extension.cpp` (569 lines) registers a script-aware
tokenizer that treats code blocks separately from prose and switches between
stemmed and non-stemmed configurations. `annotations_fts` is an external-content
FTS5 table. Stock SQLite FTS5 is sufficient here; the custom tokenizer is an
optimisation, not a correctness requirement.

**The template language is the largest reusable subsystem**:
`src/calibre/utils/formatter.py` (2,128) + `formatter_functions.py` (4,522) =
6,690 lines. It powers composite columns, title pages, and templates, and its
`program:` form is a Python-ish DSL with a fixed function set
(`first_matching_cmp`, `format_number`, `list_union`, `strcat`, `human_readable`,
`raw_field`, `field`, …). Direct port candidate.

**The search language is separate and small**:
`src/calibre/utils/search_query_parser.py`, 483 lines, `and`/`or`/`not` over
field-qualified terms.

**The plugin system is zip-loaded, not entry-point based.**
`src/calibre/customize/zipplugin.py` implements a `Loader` and a `Finder`
(`CalibrePluginLoader`, `CalibrePluginFinder`) that import plugin classes
directly out of a zip. The taxonomy in `customize/__init__.py`:

| Class | Purpose |
|---|---|
| `Plugin` | base: `initialize`, `config_widget`, `save_settings`, `cli_main` |
| `FileTypePlugin` | `run`, `postimport`, `postconvert`, `postadd`, `postdelete` |
| `MetadataReaderPlugin` | `get_metadata(stream, type)` |
| `MetadataWriterPlugin` | `set_metadata(stream, mi, type)` |
| `CatalogPlugin` | `search_sort_db`, `get_output_fields`, `run` |
| `PreferencesPlugin` | `create_widget(parent)` |
| `StoreBase` | book stores, wraps an `actual_plugin` |
| `EditBookToolPlugin` | per-book tools |
| `LibraryClosedPlugin` | runs when the library shuts down |
| `AIProviderPlugin` | LLM providers |
| `ContentServerPlugin` | content-server extensions |
| `InterfaceActionBase` | legacy action wrapper |

Two corrections to widely-held beliefs. **`PlatformPlugin` and `MenuItem` no
longer exist** — `grep` finds neither in current Calibre; the menu-item plugin
model is legacy and a new app should not copy it. And **the plugins in real use
are all third-party** — Action Chains, Count Pages, EpubMerge, EpubSplit,
FanFicFare, Find Duplicates, Generated Cover live in
`~/.config/calibre/plugins/`, not in `calibre/customize/`. Only the class
taxonomy and the zip loader are Calibre's.

**Calibre has no scan-a-folder-without-copying mode.** Grepping `scan_folder`,
`watch_folder`, `folder_scan` across `src/calibre/` returns one string inside an
MTP error message and nothing else. Books always live in the library directory,
and `copy_format_to` / `copy_cover_to` take an opt-in `use_hardlink`. So §3 is
not porting Calibre's folder handling — it is deliberately better than it.

**`book_storage` is not a file-reference table.** It reads like one (`book,
format, user_type, user, timestamp, data`) but `book_storage.py` documents it as
per-book key/value storage for `localStorage` inside book-viewer scripts,
merged newest-timestamp-wins. Do not mistake it for the linked-file mechanism.

**`last_read_positions` already exists and is the right shape**: `book, format,
user, device, cfi, epoch, pos_frac`, unique on `(user, device, book, format)`.
See §8.

---

## 2. Architecture

**Tauri v2, Rust core, no server.** Tauri rather than Electron: the core is a
database engine and an archive/format reader, and there is no case for a JS
runtime in the hot path. Rust rather than a Python sidecar: the Calibre formats
layer, the template language and FTS work are all better in Rust than in
embedded CPython, and the plugin sandbox is better with a real process boundary
than with `exec`.

| Layer | Choice | Note |
|---|---|---|
| Shell | Tauri v2 | webkit2gtk-4.1 and gtk3 both already present locally |
| Core | Rust workspace | database, formats, search, templates, plugins |
| DB | SQLite via `rusqlite` (bundled) | FTS5 compiled in, WAL mode |
| Frontend | Svelte 5 | |
| Indexing | `tantivy`, deferred | only if stock FTS5 is measurably too slow |
| Plugin host | separate process, JSON-RPC over stdio | see §6 |

**Not ported:** the Qt GUI, device drivers (Kobo/MTP/Kindle), the news/feeds
subsystem, book lending, store plugins, the `legacy.py` shim, and `cache.py` in
its current form. On the cache specifically: the 4,389-line aggressive cache is a
Qt-GUI-latency optimisation. A Tauri frontend talks to Rust over IPC, so a
read-through query path plus a write-through invalidation channel is sufficient
and far smaller. Porting it would be porting the problem it solves.

**Precedence.** Where this spec and Calibre's docs disagree, this spec wins and
the disagreement is corrected in the same commit. A genuinely ambiguous decision
becomes an ADR, not a comment.

---

## 3. The import model — reference and copy, mixed

This is the part that is not a port, and the reason the app is worth building
rather than configuring.

### 3.1 Calibre's model, precisely

Calibre's library owns its files. Adding a book copies (or hardlinks, if you ask)
into `<library>/<author>/<title>/<format>`, and the DB records format names, not
paths. There is no column anywhere that stores "this file lives over there".
That is why a folder of 4,000 fanfic EPUBs cannot be "added" to Calibre without
Calibre taking ownership of 4,000 files.

### 3.2 The two source kinds

Every book format is backed by a **source** row:

| kind | meaning | file behaviour | default |
|---|---|---|---|
| `managed` | we own the file, inside the library dir | moved/copied in on import; deleted with the book | yes, for explicit "add book" |
| `reference` | the file stays where it is | never moved, never copied, never deleted | yes, for scanned folders |
| `symlink` | we manage a symlink at a path we choose | target untouched; link created/removed | opt-in |

A book may hold several formats with different kinds. Scanned folders produce
`reference` sources; explicit adds produce `managed`. Mixed libraries are
normal, not a special case.

### 3.3 Referenced files are never silently adopted

The dangerous transition is `reference` → `managed`, which copies the file and
can surprise someone who has a read-only archive. Rules:

- Import **never** changes an existing source's kind. Converting is an explicit
  "Take ownership" action, per book or per folder, with the byte count and free
  space shown first.
- A referenced file that disappears becomes `missing`, never a deletion. The
  book row and all its metadata survive; only the format is unavailable.
- A referenced file whose path moves (library reorganised) is re-matched by
  content hash, then by size+mtime, then left `missing`. No fuzzy guessing, and
  no deletion on a failed match.
- Deleting a `reference` book removes the row only. The file is never touched.
  The UI says so explicitly in the confirmation.

### 3.4 Scanning

A watched root is a `scan_root` row: path, recursive, include globs, exclude
globs, follow-symlinks, and a `last_scan_at`. Scanning is incremental —
`(path, size, mtime_ns)` is the cheap check, content hash only when size or
mtime changed — and identity for merge purposes is the **content hash**, not the
path, so the same file reachable by two roots is one book.

Conflicts resolve in this order, and the order is a decision, not an
implementation detail:

1. Same content hash → merge formats into one book.
2. Same hash, different metadata → union, with the newer `last_modified` winning
   per field. Never last-writer-wins on the whole record.
3. Different hash, same site identifier → **version conflict, not a
   duplicate.** This is the normal lifecycle of serialised fiction, not an
   anomaly: the newer version becomes the active format by `#updated` then
   mtime, the older is archived with `superseded_by` in `book_sources`, and the
   book shows a "version updated" badge. Only genuinely ambiguous timestamps
   (within 24h) go to the inbox. A duplicate flag here would fire hundreds of
   times on a real library and would never be cleared.
4. Different hash, no shared identifier → two books.

Auto-merge is on by default for cases 1–3 and is undoable for 30 days; case 4
and ambiguous case 3 stop and ask. Full precedence, including the
cross-post-by-similarity case, is §3.6 stage 4.

### 3.5 What "no duplication" means

The guarantee is: **the app never writes a second copy of a file it did not have
to.** Reference sources are read in place. Managed sources are written once into
the library. Format conversion (an EPUB→MOBI rebuild, say) is the only
operation that produces a new file, and it says so.

### 3.6 The curation pipeline

Every file that enters the library — by scan or explicit add — passes through
this pipeline before it appears in the main view.

```
file → identify → resolve → enrich → deduplicate → classify → [inbox | library]
```

**Stage 1 — Identify.** Extract every available signal, in priority order:

1. **Embedded metadata.** The `dc:identifier` and `dc:subject` fields in an
   EPUB's OPF, including the `calibre:` namespace. FanFicFare-produced EPUBs and
   Calibre exports carry these. The gold standard.
2. **URLs** in the metadata, title page, or first chapter. An
   `archiveofourown.org/works/12345` URL is a definitive identifier even when
   the metadata block is empty.
3. **Filename patterns**, configurable, with shipped defaults for
   `[Site] ID - Title by Author`, `Title - Author - Fandom`, and
   `Author - Title (Fandom)`. A name matching `download (N).ext` is recognised
   as unidentifiable rather than mis-parsed. Users add patterns via the
   Filename Parser plugin (§6).
4. **Content fingerprints** — first-paragraph hash, chapter-heading structure,
   word count. Fuzzy signals for stage 2 and 4 only, never for identity on
   their own.

Each signal carries a confidence value. These are placeholders to be tuned
against a real library, not calibrated constants: embedded identifier high,
URL high, filename-pattern match low, nothing zero. Stage 6's threshold is the
operator's to set.

**Stage 2 — Resolve identity.** Match signals against the local library, then
against configured instances (§3.9) if any. Precedence: local exact → instance
exact → local fuzzy → instance fuzzy → new work. Fuzzy matching normalises case
and punctuation and consults a local pseudonym-to-name table.

**Stage 3 — Enrich.** If stage 1 found a site identifier but the file's own
metadata is sparse, fetch catalog metadata for it.

- **AO3:** the work's JSON endpoint — title, author, fandom, relationships,
  characters, tags, word and chapter counts, completion status, updated date.
  One request per work, rate-limited, cached in `#fff_saved_metadata`.
- **FFN:** the story page, parsed. Same fields, less reliable.
- **Other sites:** plugin-extensible (`MetadataReaderPlugin` + `net.fetch`).
- **A configured instance** (§3.9), if it holds curated metadata for this work,
  is presented as the primary suggestion with its provenance visible and the
  direct site value as the alternative. The user accepts; nothing is applied
  silently. This mirrors the existing suggestion-queue pattern rather than
  introducing a second review path.

This is a **metadata lookup for a file the user already owns** — "what is
this?" — not a content download. It fetches no chapters. That boundary is why
it can be in scope where content downloading is not (§5.6).

**Stage 4 — Deduplicate.** Extends §3.4 with version awareness:

- Same identifier, same hash → merge sources, one book.
- Same identifier, different hash → version conflict; newest wins, older
  archived with `superseded_by`. Silent, per §3.4 rule 3.
- Different identifier, content similarity above threshold (fuzzy match on
  first-chapter text) → probable cross-post. **This** one goes to the inbox,
  because merging two genuinely distinct works is destructive in a way the
  user cannot undo from their side.
- Different hash, no shared identifier → two books.

**Stage 5 — Classify.** Populate the §5.1 columns from enriched metadata:
`#fandom`, `#relationships`, `#characters`, `#archive_warnings`,
`#content_rating`, `#genre` from site tags or instance canonical entities;
`#writing_status` from the site's completion flag; `#word_count` and
`#chapter_count` from **the file's actual content where possible** (site
metadata goes stale) with the site value as fallback; `#updated` and `#created`
from the site; `#story_url`, `#story_id`, `#author_url`, `#author_id` from the
identifier.

When an instance supplies canonical entities they replace raw tag strings, and
the tag browser shows canonical names alongside any aliases the local files
used. That is the payoff of instance participation: the local taxonomy becomes
as good as the community's without the user curating it.

**Stage 6 — Inbox or library.** Overall confidence at or above the threshold
(default 0.7, configurable) goes to the library; below it, to the inbox.

### 3.7 The inbox

A first-class view, not a dialog. It holds:

- Files the pipeline could not identify.
- Probable cross-posts it is not confident enough to merge.
- Version conflicts where "newest" is ambiguous.
- Proposed anthology splits with a preview (§3.8).
- Files with format problems: corrupt, empty, DRM-locked.

Each item shows the pipeline's best guess with one-click accept or correct, plus
bulk actions (accept all above a confidence, reject all corrupt). The count is
visible in the main navigation permanently — an inbox you cannot see is an
inbox you will not clear.

**Design metric:** 4,000 uncurated files should produce 50–200 inbox items, not
4,000. If the inbox exceeds 10% of a scan, the thresholds are wrong and should
be retuned against the data — not the user's patience.

### 3.8 Anthology splitting

Some EPUBs are collections — "Author's Complete Works", "Fandom Anthology
2023". The pipeline detects candidates (multiple title pages, multiple
`dc:identifier` blocks, chapter headings that read as individual fic titles with
their own author notes) and proposes a split. Proposals go to the inbox with a
preview; confirming produces N books linked by a `split_from` provenance record
in `split_provenance`. Rejecting leaves the anthology intact.

The false-positive rate is unknown, and because the split is inbox-gated the
cost of being wrong is one rejection. That is why this ships before the
heuristic is trusted.

### 3.9 Metadata exchange protocol

Optional, opt-out, and separate from analytics (§9). The app can send extracted
metadata signals to configured compatible instances and receive community-curated
canonical metadata back.

**Shared crate `lore_metadata`.** A dependency-light Rust crate — serde only, no
IO, no database types — defining `WorkSignal`, `CanonicalWork`, `EntityRef`,
`SignalBatch` / `CanonicalBatch` and `ExchangeVersion`. It is the contract both
sides compile against, and it can be written and published at any point in the
build order because nothing else depends on it. Version incompatibility is
handled by negotiation, not by hoping.

**Configuration:**

```toml
[instances]
participate = true              # the global opt-out switch

[[instances.configured]]
url = "https://instance.example"
enabled = true                  # per-instance opt-out
token = "..."                   # scoped token
auto_send = true
auto_receive = true
```

**Sent** (`WorkSignal`): site identifiers, title, author names, fandom, tag,
character and relationship strings, word and chapter counts, completion status,
content hash, language, source URL, extraction timestamp.

**Never sent:** file contents or excerpts; reading history, progress, position
or status; ratings, notes, or any user judgment; file paths or filenames;
device identifiers beyond the token.

**Received** (`CanonicalWork`): canonical entity references with aliases,
corrected metadata, quality signals, `curated_at`, and a review-status field
naming whether the value passed human quorum or is an unverified candidate.
Canonical metadata never reveals who submitted a signal or how many users hold
a work.

**Per-column send privacy.** Any custom column can be marked `send = false`.
Default-private: `#notes`, `#read_progress`, `reading_status`, `#times_read`,
`#last_read`, `#read_dates`, `#rating` — these are user judgments about a
reading experience, not facts about a work.

**Conflict rule.** When local and instance metadata disagree, the instance value
is offered as a per-field suggestion with its provenance, never applied
automatically. Bulk accept is available.

**On opt-out:** pending batches are discarded unsent, no further requests are
made, and **previously received canonical metadata is retained** — deleting it
would degrade a library the user already curated with it. Every other feature
is unaffected; the only losses are the "instance suggests" prompts and the
curated-taxonomy badge.

**The server side is specified in Lorehaven's `docs/spec.md`**, not here, and is
filed for triage in `lorehaven/docs/plans/metadata-exchange-triage.md`. From
this application's perspective an instance is a metadata oracle: its internal
curation is opaque and irrelevant.

---

## 4. Schema

Calibre's own tables are read and written in place, unmodified: `books`,
`authors`, `tags`, `series`, `publishers`, `languages`, the five `books_*_link`
tables, `ratings`, `comments`, `identifiers`, `custom_columns` and its
per-column value tables. That is the interop guarantee.

New tables are additive and namespaced, so a Calibre install that never sees
this app still works and vice versa. Where Calibre has a table we need to extend
rather than replace, we add a sibling table keyed by `books.id` — we do not
`ALTER` Calibre's tables, because a schema Calibre cannot read is not an
interop guarantee.

| Table | Purpose | Key columns |
|---|---|---|
| `app_meta` | schema version, library id, created_at | `key`, `value` |
| `book_sources` | per-book-per-format file origin | `book`, `format`, `kind`, `path`, `size`, `mtime_ns`, `content_hash`, `state`, `superseded_by`, `confidence` |
| `scan_roots` | watched folders | `id`, `path`, `recursive`, `include`, `exclude`, `follow_symlinks`, `last_scan_at` |
| `scan_state` | incremental bookkeeping | `root`, `path`, `size`, `mtime_ns`, `last_seen_at` |
| `curation_signals` | per-file pipeline output, with provenance | `book`, `stage`, `signal_type`, `value`, `confidence`, `source` |
| `inbox_items` | awaiting user decision | `id`, `book`, `reason`, `suggestion`, `confidence`, `created_at`, `resolved_at`, `resolution` |
| `split_provenance` | anthology split records | `original_book`, `split_book`, `split_at` |
| `instance_connections` | configured instances | `id`, `url`, `token_ref`, `enabled`, `auto_send`, `auto_receive`, `last_sync_at` |
| `canonical_entities` | entities received from instances | `id`, `instance_id`, `entity_type`, `canonical_name`, `aliases`, `received_at`, `review_status` |
| `work_canonical_refs` | book → canonical entity links | `book`, `entity_id`, `field`, `accepted` |
| `signal_batches` | outbound batches | `id`, `instance_id`, `status`, `created_at`, `sent_at`, `response` |
| `reading_state` | progress, one row per book+device | `book`, `format`, `device`, `cfi`, `pos_frac`, `epoch` |
| `column_templates` | named template programs for composite columns | `name`, `program`, `is_default` |
| `plugins` | installed plugins and state | `id`, `name`, `version`, `path`, `enabled`, `settings` |
| `privacy_consent` | the analytics decision, with a timestamp | `id`, `analytics_enabled`, `decided_at`, `decided_by` |
| `analytics_events` | local-first event log, drained only with consent | `id`, `kind`, `book`, `at`, `payload` |
| `actions` | the automation chain log (§7) | `id`, `name`, `book`, `ran_at`, `result` |

`book_sources.state` is an enum: `ok`, `missing`, `moved`, `conflict`. Only `ok`
is a usable format; the others are visible states, not errors to be hidden.

**Identifier strategy.** `identifiers` is Calibre's scheme
(`book`, `type`, `val`, unique on type+val), so `isbn`, `goodreads`, `calibre`,
`amazon`, `fanficsafe` and — for this library's purpose — `ao3`, `ffnet`, `fic`
all fit without schema change. That is why §3.4 can key duplicate detection on
it.

**Composite columns need a cache.** A composite column's value is a template
program over other columns, so it cannot be stored. It gets
`book_sources`-style caching in `composite_cache (book, column_id, value,
computed_at)`, invalidated by a dependency list recorded per template. Without
the cache every sort on a composite column re-runs the program per row.

## 5. Calibre feature parity — what ships, what does not

Defaults are chosen from your existing Calibre configuration, which is the
reference for what a real fanfiction library actually needs. Every one of these
is configurable, and the default is only a default.

### 5.1 Custom columns — the default set

Adopting your Calibre set as the app's defaults, because it is a well-considered
one and re-deriving it would be worse. Composite columns are the reason the
template language is a §4 prerequisite rather than a nice-to-have.

| lookup | datatype | notes |
|---|---|---|
| `#rating` | rating, half stars | your half-star setting |
| `#notes` | long text, markdown, no heading | |
| `#fandom` | text, multiple, tag browser | |
| `#relationships` | text, multiple, tag browser | your "pairings" TODO noted; named `relationships` for Calibre parity |
| `#writing_status` | enum: Completed, Work In Progress (WIP), Hiatus, Abandoned | |
| `#updated` | date `yyyy-MM-dd` | last author update |
| `#created` | date `yyyy-MM-dd` | |
| `#last_read` | date `yyyy-MM-dd` | |
| `#times_read` | integer, default 0 | |
| `#read_dates` | text, multiple, hierarchical in tag browser | `yyyy.MM (MMMM)` values |
| `reading_status` | text, default `Unread`, with your five colouring rules | see §5.5 |
| `#archive_warnings` | text, multiple, tag browser | |
| `#content_rating` | text, tag browser | AO3 vocabulary: General Audiences, Teen, Mature, Explicit, Not Rated |
| `#genre` | text, multiple, tag browser | |
| `#characters` | text, multiple, tag browser | |
| `#extra_tags` | text, multiple, tag browser | kept separate from `tags` so updates cannot clobber manual tags |
| `#story_id`, `#author_id` | text, no heading | site-local ids |
| `#story_url`, `#author_url` | text, no heading, link | |
| `#description` | long text, plain | |
| `#chapter_count`, `#word_count` | integer | `#word_count` format `{0:,}` |
| `#length` | composite | your `first_matching_cmp` program, verbatim |
| `#page_count` | integer, `{0:,}` | Count Pages plugin target |
| `#flesh_reading_ease`, `#flesh_kincaid_grade`, `#gunning_fog_idx` | float, `{:.1f}` | Count Pages plugin targets |
| `#fff_saved_metadata` | long text, plain | full scraped metadata, so columns can be rebuilt without re-downloading |
| `#fff_last_checked` | date `yyyy-MM-dd` | |
| `#fff_version` | text, multiple | |
| `#fff_update_overwrite_error` | long text, plain | |
| `#read_progress` | text | "last chapter read" note — see §8 |

`#fff_*` columns are the FanFicFare integration. This app does not ship a
scraper, but it does ship **metadata backfill** (§3.6 stage 3, §5.6): given a
site identifier found in a file the user already has, it looks up the catalog
entry. Downloading new works and chapters stays with FanFicFare or Fiction-DL.
`#fff_saved_metadata` is what makes that boundary survivable — the full metadata
snapshot is stored, so any column can be rebuilt without re-fetching anything
from anywhere.

**Column colouring** is a first-class feature, not a plugin: per-column
conditional rules, exactly as Calibre's `Column Coloring` does it. Your
`reading_status` rules become the shipped defaults (§5.5).

### 5.2 The template language — port, do not reimplement

Port `formatter.py` + `formatter_functions.py` (6,690 lines of Python) to Rust
as a real expression language with a fixed function registry, not as an `eval`.
Three reasons for the fixed registry: plugins can call it, a composite column
can be cached against a known dependency set (§4), and an unconstrained
interpreter in a plugin process is a sandbox hole.

The `program:` syntax is preserved so your existing column definitions work
unmodified — `#length` and `#read_dates` are the two that matter, and they are
literal Calibre programs. Supported on day one: `field`, `raw_field`,
`first_matching_cmp`, `format_number`, `list_union`, `strcat`, `human_readable`,
`substr`, plus arithmetic. The rest of the 200-plus function set lands
incrementally, and an unknown function is a *visible error on that cell*, never
a silent empty string — a template language that quietly returns nothing is
worse than one that says it does not understand.

### 5.3 The search language — port

Port `search_query_parser.py` (483 lines) nearly verbatim: field-qualified terms
(`tags:foo`, `series:bar`, `#word_count:>10000`), `and`/`or`/`not`, parentheses,
and bare terms matching across title/author/series. Compiled to SQL, with FTS5
`MATCH` used only for the free-text part and structured predicates compiled
separately. Recency boosting and the sort keys Calibre supports (`#word_count`,
`#last_read`, `timestamp`) are part of the query, not a UI concern.

### 5.4 Full-text search

FTS5 over title, authors, series, tags, comments, `#description` and full book
text for indexed formats. Indexing is incremental and off the UI thread, with a
per-book status. Calibre's custom C++ tokenizer is explicitly **not** ported in
phase one — stock `unicode61 remove_diacritics 2` is what Calibre's own
`annotations_fts` uses, and the advanced tokenizer is a performance
optimisation. The `tantivy` escape hatch stays open if FTS5 is measurably too
slow on a large library.

### 5.5 Column colouring, tag browser, virtual libraries

- **Column colouring** rules, per column, with a condition builder rather than
  a raw expression string. Your five `reading_status` rules are the shipped
  defaults: `#abb2bf` Unread, `#98c379` Read, `#61afef` Want To Read, `#e5c07b`
  Currently Reading, `#e06c75` Did Not Finish.
- **Tag browser** over the many-to-many tables, with the hierarchical display
  `#read_dates` needs (`>` nesting) and Calibre's `count`/`tag` combine modes.
- **Virtual libraries** as saved searches — "Reading Now", "Needs Update",
  "On Hiatus", "DNF" — with the count shown before you enter one.

### 5.6 Not shipping, and why

| Calibre feature | Call | Reason |
|---|---|---|
| News/feeds | no | unrelated to a personal library |
| Device drivers (Kobo/MTP/Kindle) | no | hardware surface, not library management |
| Book lending | no | needs a server, and this is local-first |
| Store plugins | no | commerce |
| **Metadata backfill** | **yes** | Given a site identifier extracted from a file the user already owns, fetch catalog metadata (title, author, tags, status, dates, counts). A metadata lookup, not a content download: no chapters, no new works. The `#fff_*` columns store the result so it is reconstructable offline. Rate limiting and caching mandatory. Implementation is the §3.6 stage 3 plugin, so it is gated by the `net.fetch` capability. |
| **Metadata exchange** | **yes, opt-out** | Send extracted signals to configured instances, receive community-curated canonical metadata (§3.9). |
| Content downloading | no | FanFicFare's job, and it already does it well. The app knows a fic has been updated (§7) and does not fetch the update. Keeping this line is what makes the backfill row above defensible. |
| Content server | later, opt-in | §11 |
| Format conversion (EPUB↔MOBI) | later | needs per-format writers; readers first |
| Book editor | no | a different application entirely |
| `cache.py` architecture | no | see §2 |

---

## 6. The plugin system

Port Calibre's *taxonomy and lifecycle*, replace its *packaging and language*.
Calibre's zip-loaded Python plugin model cannot be ported: it is Python, in
process, with no sandbox — `Plugin.load_resources` and the `Loader` give a plugin
the full interpreter. Fidelity there would mean shipping an embedded Python
runtime with no isolation, which is not a sandbox at all.

**Packaging:** a plugin is a single signed-or-not zip containing a `plugin.json`
manifest and a WASM component. Rust plugins compile to `wasm32-wasip1`; the
manifest names which capabilities it wants.

**Host:** one plugin process, restarted when a plugin crashes. JSON-RPC over
stdio. The host process holds no library state — every call is a request to the
core, so a plugin cannot corrupt the database except through the same validated
API the UI uses.

**Capability model, deny by default.** A manifest requests capabilities and the
user grants them per plugin:

| Capability | Grants |
|---|---|
| `library.read` | query books and metadata |
| `library.write` | mutate metadata (not files) |
| `files.read` | read book file bytes |
| `files.write` | create or replace book files |
| `net.fetch` | make outbound requests (the FanFicFare-shaped case) |
| `storage.app` | own key/value app storage, namespaced per plugin |
| `events.subscribe` | receive library events |

A plugin requesting `files.write` or `net.fetch` shows a plain-language warning
at install. The two Calibre plugins you actually lean on — Action Chains and
Count Pages — need `library.write` only.

**API surface mirrors the Calibre taxonomy**, so the concepts transfer even
though the implementation does not:

| Calibre class | This app |
|---|---|
| `Plugin` | base: `initialize`, `config_schema`, `on_event` |
| `FileTypePlugin` | `probe(path) -> BookFormat?`, `read_metadata`, `write_metadata` |
| `MetadataReaderPlugin` | `read_metadata(bytes, format) -> Metadata` |
| `MetadataWriterPlugin` | `write_metadata(bytes, metadata, format)` |
| `CatalogPlugin` | `export(selection, opts) -> bytes` |
| `PreferencesPlugin` | `config_schema` → rendered settings panel |
| `StoreBase` | not ported (see §5.6) |
| `EditBookToolPlugin` | a registered action available in the book context menu |
| `LibraryClosedPlugin` | `on_shutdown` |
| `ContentServerPlugin` | an HTTP route registration, when §11 lands |

**Event bus** — the part that makes automation possible, and Calibre's
best idea. Library events (`book_added`, `book_updated`, `metadata_changed`,
`scan_completed`, `metadata_fetched`, `format_converted`) are delivered to
subscribed plugins. This is how an external scraper says "I just fetched
metadata for these books" and an action chain reacts without either knowing
about the other.

**Built-in plugins ship in the box** for the things you would otherwise
install separately, and each exists to exercise the same API path a third-party
plugin will:

- **Metadata Backfill** — §3.6 stage 3. A plugin so it is gated by the
  `net.fetch` capability and so site-specific parsers are added without touching
  the core.
- **Filename Parser** — configurable regex extraction from filenames, so users
  can add patterns for their own download sources.
- **Count Pages** — word count, page count, Flesch reading ease, Flesch-Kincaid
  grade, Gunning Fog; writes the five custom columns of §5.1.
- **Action Chains** — §7.
- **EpubMerge/EpubSplit** — the plugin provides the mechanism; the pipeline
  drives the split (§3.8).
- **Find Duplicates** — *not* a plugin. Deduplication is a continuous pipeline
  stage (§3.6 stage 4), not an on-demand action, and shipping it as a button
  would misrepresent what it does.

Generated Cover is a nice-to-have later.

---

## 7. Automation — Action Chains

Port the concept: an ordered list of actions run over a selection or triggered by
an event, with conditions, and a Python module editor for custom steps. In this
app the custom-step escape hatch is **a plugin capability, not an interpreter**
— a chain step that needs arbitrary code is a plugin step.

Built-in step types, which cover everything your current chains do:

- `set_column` (with a value, a template result, or "prompt")
- `increment_column`
- `set_reading_status`
- `add_tags` / `remove_tags`
- `set_rating` (including `prompt`)
- `append_read_date` — your step 5, the `#last_read` → `#read_dates` copy
- `run_plugin`
- `notify`

A chain fires from a menu action, a keyboard shortcut, or an event subscription
(§6), and every run is logged to `actions` (§4) with its per-step result, so a
chain that half-failed is diagnosable rather than mysterious.

The four-step "mark as read" chain you described is a shipped template:

1. set `reading_status` = `Read`
2. set `#last_read` = today
3. increment `#times_read`
4. prompt for `#rating`
5. append today to `#read_dates`

**The stale-mark problem, handled.** You noted that the only way to know a
"read" book has since been updated is comparing `#updated` against `#last_read`.
The app surfaces that comparison directly: a book with a non-terminal
`#writing_status` and `#updated > #last_read` is flagged **"updated since you
finished it"** in the library view, and one click clears `reading_status` back
to `Currently Reading`. That is a query, not a stored flag, so it can never go
stale.

---

## 8. Reading progress, and the Moon+ sync

You read EPUBs with Moon+ Reader Pro and keep a per-fic note of the last chapter
read, and you asked how to track fanfiction reading progress. The answer is that
Calibre already has the right table and the wrong ergonomics, and this app
supplies both the table and a better interface.

**The table.** Calibre's `last_read_positions` is
`(book, format, user, device, cfi, epoch, pos_frac)`, unique on
`(user, device, book, format)`. This app adopts it, adding `reading_state` as
its own table only so that Calibre keeps reading its own — the same additive
rule as §4. `cfi` is an EPUB canonical fragment identifier, which is the only
locator that survives a reflow; `pos_frac` is the coarse fallback for formats
without one.

**Three kinds of progress, because they answer different questions:**

1. **Position** — a CFI plus a fraction. Synced with a reader (§8.2). Answers
   "where was I".
2. **Chapter** — the last chapter actually read, stored in `#read_progress`
   (§5.1). For serialised fanfic this is the number that matters: a position in
   an EPUB that was later re-downloaded and grew by four chapters is wrong in a
   way a chapter number is not. `#read_progress` is a **separate column on
   purpose** — it is user-owned and is never touched by a metadata update.
3. **Status** — `reading_status`, the five-valued column from your Calibre setup
   (`Unread`, `Want To Read`, `Currently Reading`, `Read`, `Did Not Finish`),
   with your colour rules (§5.5).

The relation between them is a query, never a stored flag: "chapters available
minus `#read_progress`" is how the app knows you are behind, and a book is
"finished" when status is `Read` *and* `#read_progress` equals the current
chapter count. This is the answer to the tracking question — **track the chapter
number, derive everything else.**

**Derived views that fall out of this for free**, and which your current setup
has to fake with tags:

- *Behind on an update*: `#writing_status` non-terminal, `#updated > #last_read`
  (§7).
- *Stalled*: non-terminal and `#updated` older than N days — the search your
  Action Chains question was about, as a built-in virtual library rather than a
  chain you have to write.
- *Oneshot*: `#writing_status` = Completed and `#chapter_count` = 1, so you can
  find the fic that are a single sitting.

### 8.2 Syncing with Moon+ Reader Pro

This is a two-way problem and the honest answer is that the app can be the
authority without owning the reader.

- **App → reader:** the content server (§11) exposes an OPDS-style feed plus a
  `/read-position/{book}` endpoint. A reader that can be taught a URL syncs
  position; Moon+ supports OPDS, and its Calibre-sync integration is the
  reference for what a compatible client expects.
- **Reader → app:** Moon+ writes a Calibre-compatible sync if configured against
  a Calibre content server. Because this app speaks the same protocol on the
  same port shape, the existing integration is the integration — this is a
  direct payoff of choosing Calibre's schema as the interop target.
- **Manual, always available:** set `#read_progress` by hand. For fanfic this is
  often the only option, because chapter numbers are site metadata rather than
  book structure, and no reader reports them.

The chapter number cannot come from the reader, so it is a first-class column
with a picker, not an inferred value. That is a deliberate limitation, stated
rather than hidden.

---

## 9. Analytics — opt-out, and the dependency is real

Your requirement: share analytics, opt out, and after opting out anything
depending on analytics must not work. The second half is the part that is
usually fudged, so it is specified as an invariant rather than a feature.

**Model.** Three tiers, and the tier decides what exists:

| Tier | Behaviour |
|---|---|
| `local` (default) | Everything is computed on-device. No analytics egress at all. Metadata exchange (§3.9) is a separate concern with its own opt-out — see below. |
| `shared` | Usage aggregates are sent — counts, feature usage, crash reports. No identifiers, no reading history. |
| `personalised` | Reading history drives cross-device recommendations. Requires `shared` plus an explicit per-purpose toggle. |

**Metadata exchange is not analytics.** §3.9 sends work-metadata signals
(title, author, tags, identifiers) to instances you configured. It sends no
usage data, no reading history, and nothing about your behaviour. It is
controlled by `[instances] participate`, not by the tier above. You can be
`local` for analytics and still exchange metadata, or disable either
independently, and the settings screen shows **two separate sections** rather
than one combined switch — collapsing them would make opting out of one look
like opting out of both, which is exactly the confusion the §9 invariant exists
to prevent.

**What is collected when `shared` is on:** app version, OS, feature-usage
counts, anonymised error reports, and library *size* bands. Never: titles,
authors, tags, notes, file paths, file contents, identifiers, or which books a
person opened. A `shared` install cannot report "you read 40 works by this
author", because that data is never collected in the first place.

**The opt-out invariant.** Consent lives in `privacy_consent` with a timestamp.
On opt-out, in this order:

1. Telemetry stops. The transport is torn down, not merely muted — a muted
   transport can be re-enabled by a bug.
2. `analytics_events` is truncated. The local log does not sit in the database
   waiting for consent to change.
3. **Every analytics-dependent feature is disabled, visibly.** Not hidden, not
   degraded to a local heuristic: disabled, with the reason shown in place.

That third step is the one worth being pedantic about, so the affected features
are named explicitly:

| Feature | Without analytics |
|---|---|
| "Recommended for you" | **not shown.** No local stand-in. |
| "Because you read X" | **not shown.** |
| "Readers also enjoyed" | **not shown.** |
| Trending / popular | local-only computation over the local library, clearly labelled as such, or not shown |
| Reading-insight stats (sessions, time, streaks) | **fully available** — local, never was analytics |
| Search, templates, columns, tags, chains, plugins, sync | **fully available** |

The distinction that matters: *reading insights* are computed from your own
library and are not analytics, so they survive. *Recommendations* are the
analytics-dependent feature, and without consent the app refuses to invent them
from your own data. A local heuristic is not a recommendation; presenting one
under a "recommended" label would be a lie about where it came from.

**Enforcement is structural, not a UI convention.** Analytics-touching code
lives behind one module (`telemetry::sink`) that is constructed only when consent
holds; the recommendation resolver is not registered at all when it does not. A
test asserts that the recommendation route 404s with consent off, and that
`analytics_events` is empty after a hard opt-out with no network available. This
is the same reasoning as the capability model in §6: a rule that is only
enforced in the interface is not enforced.

---

## 10. Milestones

Each is independently shippable, in dependency order, and the order is not
preference. The three orderings that matter:

- **Database before anything that reads it.** The library view, search and
  columns are all queries; building a UI against a provisional schema means
  rewriting the UI.
- **Template language before composite columns.** §5.1's default column set
  contains two composite columns. Without the language, the defaults do not
  exist and the app opens with a broken library view.
- **Plugin host before built-in plugins.** Count Pages, Metadata Backfill and
  Action Chains are plugins, not builtins, precisely so they exercise the same
  API path a third-party plugin will. A plugin API that only the app itself uses
  is an API nobody has tested.
- **Curation before the reading UI.** The pipeline (§3.6) decides what a file
  *is*. A library view built before it has stable identities, stable
  deduplication, and real metadata to sort and filter by is a view that has to
  be rebuilt, not restyled.
- **Curation before instance sync.** The exchange sends signals; a local app
  with no pipeline has no signals to send, and nothing to do with what comes
  back.

| # | Milestone | Ships | Est. |
|---|---|---|---|
| M1 | **Core** | Tauri shell, SQLite open/create, Calibre schema read-write, book list + detail, no search | 3–4 d |
| M2 | **Storage model** | `book_sources` (with `superseded_by`, `confidence`), managed/reference/symlink, scan roots, incremental scan, missing-file handling (§3) | 4–5 d |
| M3 | **Curation pipeline** | Identify, resolve, enrich, deduplicate with version awareness, classify. Inbox view. Filename parsers. Metadata backfill for AO3/FFN. Anthology split proposals. **The largest single milestone, and the reason the app is worth using.** | 8–10 d |
| M4 | **Search + language** | search parser port, SQL compilation, FTS5 index, search UI, saved searches (§5.3, §5.4) | 3–4 d |
| M5 | **Template language** | parser + fixed function registry, `#length` and `#read_dates` working verbatim, composite cache, visible errors on unknown functions (§5.2) | 5–7 d |
| M6 | **Custom columns** | the full §5.1 default set, column editor, tag browser, column colouring, virtual libraries (§5.1, §5.5) | 5–6 d |
| M7 | **Plugin host** | WASM host, capability grants, event bus, plugin manager UI (§6) | 5–7 d |
| M8 | **Built-in plugins** | Count Pages, Metadata Backfill, Filename Parser, Action Chains, EpubMerge/Split (§6) | 4–5 d |
| M9 | **Instance sync** | `lore_metadata` crate, connection UI, signal send/receive, "instance suggests" per-field flow with bulk accept, opt-out (§3.9) | 5–7 d |
| M10 | **Automation** | action chains as an event-capable pipeline, the mark-as-read template, run log (§7) | 3–4 d |
| M11 | **Reading progress** | `reading_state` + `#read_progress`, derived views, stale-mark flag (§8) | 2–3 d |
| M12 | **Analytics + consent** | tiered model, `telemetry::sink`, structural opt-out, and the *separate* exchange consent surface (§9) | 2–3 d |
| M13 | **Content server** | OPDS + Calibre sync protocol, opt-in, localhost-bound (§11) | 4–5 d |
| M14 | **Format conversion** | EPUB/AZW3/MOBI readers and writers (§5.6) | 7–10 d |

Total 61–83 days. Two useful stopping points: **M1–M3 (15–19 d)** is the minimum
viable replacement — open a library, point it at the drives, get curated output
with an inbox. **M1–M6 (28–36 d)** is a library with search, the full column
set and a working template language.

**Explicitly deferred past M14:** book editor, device drivers, store plugins,
book lending, a custom FTS tokenizer, `tantivy`, Generated Cover, re-downloading
updated chapters, and a native mobile client.

---

## 11. Content server

Late, opt-in, and the reason for it is §8.2: OPDS plus Calibre's
`/last_read_positions` sync protocol is what makes Moon+ work, and it is the
only feature that puts a network listener on a local-first app.

Default: bound to `127.0.0.1`, off unless enabled, with a token. LAN binding
requires a separate explicit confirmation and is the only path that gets a
warning about unencrypted traffic. The server is read-only except for
read-position writes.

---

## 12. Open questions for you

1. **Does it ship as a new repository?** My recommendation is yes —
   `~/code-local/rust/<name>` with its own `docs/plans/`, and this file copied
   in as its first commit. Keeping it inside either existing project means it
   inherits that project's release cadence and review process, which it does not
   belong to.
2. **How much of the template language, really?** M4's 5–7 days assumes the
   subset in §5.2. Porting all 200+ functions is closer to 15 days, and most are
   title-page-only. Your two composite columns need about ten.
3. **WASM or a scripting language for plugins?** WASM gives a real sandbox and
   costs plugin authors a Rust toolchain. A sandboxed Lua or Starlark keeps the
   authoring experience and gives up some isolation. I chose WASM; a Starlark
   tier for simple plugins is a reasonable hybrid if the toolchain proves
   hostile.
4. **Should the content server be in v1 at all?** It is what makes the Moon+ sync
   work. If that sync is the point, M11 moves much earlier and M12 goes last.
5. **What is the app called?** Not decided here, deliberately.
6. **Does the analytics endpoint exist to be real?** §9 specifies sending
   aggregates somewhere. If there is no server to send them to, `shared` tier
   should be dropped rather than shipped pointing at nothing.
7. **How messy are the drives, really?** This is now the highest-value unknown
   in the document. The pipeline design depends on the ratio of
   "FanFicFare EPUB with full embedded metadata" to "`download (3).epub`". If
   80% are the former, M3 is mostly a dedup-and-version pass and the Filename
   Parser is a footnote. If 80% are the latter, the parser and the inbox
   ergonomics are the critical path and M3's 8–10 days is optimistic. **A count
   of the actual files should precede M3**, not follow it.
8. **Which sites are the sources?** AO3 and FFN are assumed above. SpaceBattles
   and SufficientVelocity need forum-thread parsing rather than a JSON
   endpoint; Wattpad's API is hostile. The per-site work is plugin-sized (§6),
   but the *first* two sites are core and choosing wrong makes M3's estimate
   wrong.
9. **What is the default instance, if any?** §3.9 ships `participate = true`
   with an empty instance list, which is opt-in in practice. If a
   default-instance URL should be pre-filled, that instance must exist, must
   accept the traffic, and must be one the operator can identify. Shipping a
   URL that resolves nowhere is worse than shipping none.
10. **Should conflicting instances both be able to suggest?** §3.9 says the
    instance value is a per-field suggestion. If two instances disagree, the
    simpler rule is: whichever answered, first accepted wins, provenance
    recorded, later ones shown as alternatives. Worth confirming before M9
    rather than after.

---

## 13. What I did not do

- **No code, no repo, no scaffold.** Calibre was cloned to
  `~/code-local/research/calibre` for reading only.
- **No changes to Lorehaven's spec.** The server-side half of the metadata
  exchange is filed for triage in
  `lorehaven/docs/plans/metadata-exchange-triage.md` and has **not** been
  written into `docs/spec.md`. That triage records which of the proposal's
  section citations were wrong and which items were rejected.
- **No content downloading.** Metadata backfill looks up catalog entries for
  files already on disk. Fetching chapters and new works stays with
  FanFicFare or Fiction-DL (§5.6).
- **No claims about Calibre I did not verify in the source.** Every structural
  claim in §1 carries a file path, and three things I expected to be true were
  not: `PlatformPlugin` and `MenuItem` are gone, there is no scan-folders mode,
  and `book_storage` is not a file-reference table.

## 14. Provenance

§0 decisions 4–6 and §3.6–§3.9 were added 2026-09-25 after triaging an
external AI proposal. The proposal's central claim was correct and load-bearing:
the spec as originally written was a better Calibre, and the user does not know
what their files are. Its section citations for the Lorehaven side were
substantially wrong and its credit-for-submission and `signal_count`-as-demand
ideas were rejected on conflict with the existing spec — both decisions are
recorded with their reasoning in the triage document rather than silently
dropped.

## 15. Correction rule

A statement in this spec found to be wrong is fixed in place, in the same commit
as the code that proved it wrong. A genuinely ambiguous decision becomes an ADR
in the new repository, not a comment. A spec nobody corrects is worse than no
spec, because the next reader trusts it.

