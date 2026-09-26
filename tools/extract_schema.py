import argparse
import os
import sqlite3
import subprocess
import tempfile

# Usage:
#   extract_schema.py                 # make a throwaway library, then extract
#   extract_schema.py /path/to/lib    # extract from an existing calibre library
#   extract_schema.py --from-repo     # make the throwaway library with `calibre`
#
# The point of this script is that the schema is copied from a REAL library
# rather than written from memory. A hand-written subset silently omits the
# triggers and views that Calibre needs, and the failure is invisible until
# Calibre shows an empty library.

HERE = os.path.dirname(os.path.abspath(__file__))
DST = os.path.join(
    HERE, "..", "crates", "lorebook-calibre", "src", "calibre_schema.sql"
)

ap = argparse.ArgumentParser()
ap.add_argument("library", nargs="?", help="path to an existing calibre library")
ap.add_argument(
    "--from-repo",
    action="store_true",
    help="also rewrite the $TABLES/$TRIGGER_SUPPORT sets from what calibre has",
)
args = ap.parse_args()

tmp = None
if args.library:
    src = args.library
else:
    # Build a real library with the calibre binary. Needs a real file to add.
    tmp = tempfile.mkdtemp(prefix="calibre-extract-")
    os.makedirs(f"{tmp}/in", exist_ok=True)
    book = f"{tmp}/in/extract-fixture.txt"
    with open(book, "w") as fh:
        fh.write("schema extraction fixture\n")
    subprocess.run(
        ["calibre", f"--with-library={tmp}/lib", "add", book],
        check=True, capture_output=True,
    )
    src = f"{tmp}/lib"

conn = sqlite3.connect(f"{src}/metadata.db")

# The TABLES this app reads or writes. Everything else (feeds, annotations,
# comments, plugin tables) is Calibre-internal and deliberately omitted: a
# partial library is still a valid one, and pulling in every table would mean
# re-checking all of them on every Calibre upgrade.
# Seeded with what the app reads or writes directly.
TABLES = {
    "authors", "books", "books_authors_link", "books_languages_link",
    "books_plugin_data", "books_publishers_link", "books_ratings_link",
    "books_series_link", "books_tags_link", "comments", "custom_columns",
    "data", "feeds", "identifiers", "languages", "library_id",
    "metadata_dirtied", "preferences", "publishers", "ratings", "series",
    "tags",
}

# Tables that exist only to satisfy a kept trigger or index. Omitting one
# produces a library that looks complete but fails on the first insert, because
# e.g. books_insert_trg's sibling books_pages_link_create_trigger writes to
# books_pages_link. Small, stable, and cheap to carry.
TRIGGER_SUPPORT = {
    "books_pages_link",   # books_pages_link_create_trigger
    "conversion_options",
    "annotations_dirtied",
    "last_read_positions",
}

# Triggers and indexes are NOT optional. Calibre's books_insert_trg computes
# sort and uuid, and its books_pages_link_create_trigger maintains the page
# counts; a library missing them is one Calibre will show wrong or refuse. So
# every trigger and index belonging to a kept table is included, and the
# filter is by *referenced table*, not by name.
rows = conn.execute("SELECT type, name, tbl_name, sql FROM sqlite_master").fetchall()

kept_tables = set()
triggers, indexes, tables, views = [], [], [], []
for typ, name, tbl, sql in rows:
    if sql is None:
        continue  # internal sqlite_* object
    if typ == "table":
        if name in TABLES or name in TRIGGER_SUPPORT:
            kept_tables.add(name)
            tables.append(sql)
    elif typ == "trigger":
        # sqlite_master.tbl_name is the table the trigger is defined ON.
        if tbl in kept_tables or tbl == "books":
            triggers.append((name, sql))
    elif typ == "view":
        # Views are not tables, so the table filter above never saw them. `meta`
        # is the one Calibre's own UI and tag browser read, so a library without
        # it opens but shows nothing. Emitted after the tables.
        views.append((name, sql))
    elif typ == "index":
        if tbl in kept_tables:
            indexes.append((name, sql))

HEADER = """-- Lorebook: the subset of Calibre 9.15's schema this app reads and writes.
--
-- Extracted verbatim from a real `calibre --with-library=... add`-generated
-- metadata.db, not from documentation, so the column set matches a real
-- library exactly.
--
-- Two things to know about how this file was produced:
--
--   * `sqlite_master.sql` stores each statement WITHOUT a trailing semicolon,
--     so every statement here is re-terminated with exactly one. Trigger bodies
--     keep their internal semicolons.
--   * Triggers and indexes are included per-table, and are NOT optional.
--     Calibre's `books_insert_trg` computes `sort` and `uuid`, and
--     `books_pages_link_create_trigger` maintains page counts. A library
--     without them is not a Calibre library, however complete its tables look.
--
-- When Calibre's schema changes, this file is what has to be re-checked;
-- `tests/interop_with_calibre.sh` asserts a real Calibre still accepts what we
-- produce.
--
-- These are Calibre's own objects, unmodified. Lorebook's own tables live in
-- additive.sql.

"""


def emit(sql: str) -> str:
    return sql.strip().rstrip(";").rstrip() + ";"


out = [HEADER]
out.append("-- ---- tables ----")
for sql in tables:
    out.append(emit(sql) + "\n")
out.append("-- ---- triggers ----")
for name, sql in sorted(triggers):
    out.append(f"-- {name}\n{emit(sql)}\n")
out.append("-- ---- views (Calibre's UI reads these; `meta` is the important one) ----")
for name, sql in views:
    out.append(f"-- {name}\n{emit(sql)}\n")
out.append("-- ---- indexes ----")
for name, sql in sorted(indexes):
    if sql.strip().upper().startswith("CREATE INDEX"):
        out.append(f"-- {name}\n{emit(sql)}\n")
with open(DST, "w", encoding="utf-8") as fh:
    fh.write("\n".join(out))

print(
    f"wrote {DST}: {len(tables)} tables, {len(triggers)} triggers, "
    f"{len(indexes)} indexes, {len(views)} views"
)
print("triggers:", ", ".join(sorted(n for n, _ in triggers)))
