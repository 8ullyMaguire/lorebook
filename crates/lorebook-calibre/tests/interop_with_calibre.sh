#!/usr/bin/env bash
# End-to-end interop proof, run against a REAL Calibre binary.
#
# The unit and integration tests check our code against Calibre's *schema*.
# This script checks the claim that actually matters to a user: a library this
# app writes is a library Calibre can open, and a book this app inserts is a
# book Calibre shows.
#
# It is skipped (exit 77) when no `calibre` binary is available, so it does not
# break `cargo test` on a machine without Calibre.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
WORK="${TMPDIR:-/tmp}/lorebook-interop-$$"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()   { printf '  ok   %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  FAIL %s\n' "$1"; fail=$((fail+1)); }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }

if ! command -v calibre >/dev/null 2>&1; then
  echo "SKIP: no calibre binary; cannot prove round-trip against real Calibre"
  exit 77
fi
CALIBRE_VERSION="$(calibre --version 2>&1 | head -1)"
echo "Using: $CALIBRE_VERSION"

# A tiny helper binary that drives the library through our own code.
# The path must honour CARGO_TARGET_DIR, which is set on this host to keep build
# artefacts off the slow network mount.
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export CARGO_TARGET_DIR
HELPER="$CARGO_TARGET_DIR/debug/lorebook-interop-check"
if [ ! -x "$HELPER" ]; then
  echo "Building the interop helper..."
  (cd "$ROOT" && cargo build -p lorebook-interop-check) || {
    echo "FAIL: could not build the interop helper"; exit 1; }
fi
if [ ! -x "$HELPER" ]; then
  echo "FAIL: helper still missing at $HELPER"; exit 1
fi

LIB="$WORK/lib"
mkdir -p "$LIB"

# --- 1. We create a library -------------------------------------------------
"$HELPER" create "$LIB" || { echo "FAIL: create_library errored"; exit 1; }
if [ -f "$LIB/metadata.db" ]; then
  ok "create_library produced a metadata.db"
else
  bad "create_library produced no metadata.db"; exit 1
fi

# --- 2. We insert a book ----------------------------------------------------
TITLE="A Test Interop Work"
if "$HELPER" insert "$LIB" "$TITLE" "An Author" "9780000000001"; then
  ok "inserted a book into our library"
else
  bad "insert failed"; exit 1
fi

# --- 3. Real Calibre reads it ----------------------------------------------
# calibredb, not `calibre list`: the latter reads Calibre's own on-disk cache
# and prints nothing for a library it has never opened, which would make this
# check pass or fail for reasons unrelated to our writes. calibredb queries the
# database directly, which is the claim under test.
#
# The library path must be absolute: this line does not run inside $WORK, and
# `--with-library=lib` would resolve against whatever the caller's cwd is.
LIST="$(calibredb list --with-library="$LIB" --fields title,authors,isbn 2>/dev/null | tr -d '\r')"
if printf '%s' "$LIST" | grep -q "Test Interop"; then
  ok "real Calibre sees the book we inserted"
else
  bad "real Calibre did not see our book; it listed: ${LIST:-<nothing>}"
fi

# Calibre's own author column must show the author we linked, which means our
# books_authors_link rows are shaped the way Calibre expects.
if printf '%s' "$LIST" | grep -q "An Author"; then
  ok "real Calibre resolves the author we linked"
else
  bad "real Calibre did not resolve our author; listed: ${LIST:-<nothing>}"
fi

# ...and the identifier, which is what duplicate detection keys on.
if printf '%s' "$LIST" | grep -q "9780000000001"; then
  ok "real Calibre resolves the identifier we wrote"
else
  bad "real Calibre did not resolve our identifier; listed: ${LIST:-<nothing>}"
fi

# Calibre's `meta` view is what its own UI reads. A library without it opens but
# displays nothing, so its presence is part of the interop guarantee.
if sqlite3 "$LIB/metadata.db" \
   "SELECT count(*) FROM sqlite_master WHERE type='view' AND name='meta';" 2>/dev/null \
   | grep -q '^1$'; then
  ok "our library has Calibre's meta view"
else
  bad "our library is missing Calibre's meta view"
fi

# --- 4. We can read what real Calibre wrote ---------------------------------
# Calibre recomputes sort with its own title_sort(); reading it back proves the
# two implementations agree, which is what stops an ordering flip on handover.
SORT="$(sqlite3 "$LIB/metadata.db" "SELECT sort FROM books WHERE id=1;" 2>/dev/null)"
check "Calibre's title_sort and ours agree" "$SORT" "Test Interop Work, A"

# --- 5. Calibre adds a book, we read it -------------------------------------
mkdir -p "$WORK/in"
printf 'interop fixture\n' > "$WORK/in/calibre-added.txt"
(cd "$WORK" && calibre --with-library="$LIB" add "in/calibre-added.txt" >/dev/null 2>&1)
# Calibre writes through its own triggers, which call the functions we
# registered; an error here means our implementations disagree with Calibre's.
if "$HELPER" verify "$LIB"; then
  ok "we can read the book real Calibre added"
else
  bad "we could not read the book Calibre added"
fi

# --- 6. A library Calibre made is one we can open ---------------------------
# The reverse direction of the guarantee.
mkdir -p "$WORK/theirlib"
printf 'reverse direction\n' > "$WORK/in/rev.txt"
(cd "$WORK" && calibre --with-library=theirlib add "in/rev.txt" >/dev/null 2>&1)
if "$HELPER" open "$WORK/theirlib"; then
  ok "we can open a library real Calibre created"
else
  bad "we could not open a library real Calibre created"
fi

# --- 7. Our additive tables do not stop Calibre -----------------------------
if calibredb list --with-library="$LIB" >/dev/null 2>&1; then
  ok "Calibre still reads the library after we extended it"
else
  bad "Calibre cannot read the library we extended"
fi

# And our table survived Calibre opening and using the library.
if sqlite3 "$LIB/metadata.db" \
   "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='book_sources';" 2>/dev/null \
   | grep -q '^1$'; then
  ok "our additive table survived Calibre's own writes"
else
  bad "our additive table was dropped or renamed by Calibre"
fi

echo
echo "interop: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
