"""Differential test: our Rust title_sort/author_sort vs Calibre's real Python.

Calibre's package __init__ pulls in the whole calibre runtime (polyglot, Qt,
...), so importing `calibre.ebooks.metadata` normally is not possible outside a
calibre install. Instead, extract the two functions plus the module-level names
they close over, and exec just those. That way the oracle is Calibre's own
source text, unmodified, rather than a reimplementation of it.

The functions under test are copied out of the file by name; the tweak tables
are read from the installed calibre's shipped default_tweaks.py.
"""
import ast
import json
import os
import re
import sys

# Where to read Calibre's behaviour from. Override with CALIBRE_SRC when your
# clone or install lives somewhere else.
CALIBRE_SRC = os.environ.get("CALIBRE_SRC", "/home/alvaro/code-local/thirdparty/calibre")
CAL = os.path.join(
    CALIBRE_SRC, "src/calibre/ebooks/metadata/__init__.py"
)
# The shipped tweak values. An installed calibre keeps them in its resources dir.
TWEAKS = os.environ.get(
    "CALIBRE_TWEAKS", "/opt/calibre/resources/default_tweaks.py"
)
for _p in (CAL, TWEAKS):
    if not os.path.exists(_p):
        sys.exit(
            f"not found: {_p}\n"
            "Set CALIBRE_SRC (a calibre git clone) and CALIBRE_TWEAKS "
            "(its resources/default_tweaks.py)."
        )

src = open(CAL, encoding="utf-8").read()
tree = ast.parse(src)

wanted_funcs = {"title_sort", "author_to_author_sort", "get_title_sort_pat"}
chunks = []
for node in tree.body:
    if isinstance(node, ast.FunctionDef) and node.name in wanted_funcs:
        chunks.append(ast.get_source_segment(src, node))
# quote_pairs is a module-level dict the two functions read.
for node in tree.body:
    if isinstance(node, ast.Assign) and any(
        isinstance(t, ast.Name) and t.id == "quote_pairs" for t in node.targets
    ):
        chunks.append(ast.get_source_segment(src, node))
# _title_pats is the memo dict.
for node in tree.body:
    if isinstance(node, ast.Assign) and any(
        isinstance(t, ast.Name) and t.id == "_title_pats" for t in node.targets
    ):
        chunks.append(ast.get_source_segment(src, node))

missing = wanted_funcs - {
    n.name for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in wanted_funcs
}
if missing:
    sys.exit(f"could not find {missing} in Calibre source")

# The tweak tables title_sort/author_to_author_sort read. Parsed out of the
# shipped default_tweaks.py with a literal eval, so they are Calibre's values.
tw = open(TWEAKS, encoding="utf-8", errors="replace").read()


# Parsed from the real AST: a regex cannot find the end of a multi-line tuple,
# and an earlier version of this script silently produced EMPTY tweak tables
# because of exactly that, which in turn made the oracle wrong.
_tw_tree = ast.parse(tw)


def tweak(name, default=None):
    for node in _tw_tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id == name for t in node.targets
        ):
            try:
                return ast.literal_eval(node.value)
            except (ValueError, SyntaxError):
                # Some tweaks are computed, not literals; fall back loudly.
                print(f"  WARNING: {name} is not a literal; using {default!r}")
                return default
    print(f"  WARNING: {name} not found in default_tweaks.py; using {default!r}")
    return default


ns = {
    "tweaks": {
        "title_series_sorting": tweak("title_series_sorting", "library_order"),
        "per_language_title_sort_articles": tweak("per_language_title_sort_articles", {}),
        "default_language_for_title_sort": tweak("default_language_for_title_sort"),
        "author_sort_copy_method": tweak("author_sort_copy_method", "comma"),
        "author_name_copywords": tweak("author_name_copywords", ()),
        "author_name_prefixes": tweak("author_name_prefixes", ()),
        "author_name_suffixes": tweak("author_name_suffixes", ()),
        "author_use_surname_prefixes": tweak("author_use_surname_prefixes", False),
        "author_surname_prefixes": tweak("author_surname_prefixes", ()),
    },
    "remove_bracketed_text": lambda s: s,
    "force_unicode": lambda s: s,
    # get_title_sort_pat imports these lazily, only on the non-default path.
    # Stub the module so the default English path runs unmodified.
    "_stub": None,
}
ns["_"] = lambda s: s

# Provide a fake `calibre.utils.localization` so the lazy import inside
# get_title_sort_pat resolves. canonicalize_lang is only reached when a
# per-language article list is consulted; get_lang likewise.
import types  # noqa: E402

_loc = types.ModuleType("calibre.utils.localization")
_loc.canonicalize_lang = lambda x: x
_loc.get_lang = lambda: "eng"
_pkg = types.ModuleType("calibre")
_pkg_utils = types.ModuleType("calibre.utils")
_pkg_utils.localization = _loc
_pkg.utils = _pkg_utils
sys.modules.setdefault("calibre", _pkg)
sys.modules.setdefault("calibre.utils", _pkg_utils)
sys.modules.setdefault("calibre.utils.localization", _loc)

exec("import re\n" + "\n\n".join(chunks), ns)  # noqa: S102 - exec'ing Calibre's own source

title_sort = ns["title_sort"]
author_to_author_sort = ns["author_to_author_sort"]

TITLES = [
    "A Tale of Two Cities", "The Hobbit", "the Hobbit", "THE HOBBIT",
    "An Apple", "an Apple", "A", "The", "An", "Android", "Annabelle",
    "Theory of Everything", "dune", "Dune", "DUNE", "1984", "Neuromancer",
    "Blade Runner", "Die Verwandlung", "Les Misérables", "La Casa", "El Dorado",
    "  Dune  ", "  The Hobbit ", '"A Tale of Two Cities"', "“The Hobbit”",
    "'The Hobbit'", "『A Tale』", '"A Tale', "A Tale of Two Cities, A",
    "A Stitch in Time", "An Embarrassment of Riches", "The Order of the Phoenix",
    "Abandon", "Absolute", "Astonishment", "The The", "A An The", "", "   ",
    "The Rise of the Robots", "A.I. Superpowers", "Übermensch", "Ærø",
    "L'Étranger", "O'Brien at the Walls", "The 39 Steps", "T", "Th", "Thee",
    "A.B.C.", "An-Apple", "The\u00a0Hobbit", "An\u00a0Apple",
]

AUTHORS = [
    "Ursula K. Le Guin", "Jane Doe", "Homer", "Plato", "Le Guin, Ursula K.",
    "Guin, Ursula K. Le", "Acme Games", "National Geographic", "Something Soft",
    "BBC", "Dr Jane Doe", "Dr. Jane Doe", "Prof. John Smith", "Mr John Smith",
    "Mrs Jane Doe", "Ms Jane Doe", "John Doe Jr", "John Doe Jr.",
    "Martin Luther King Jr.", "Dr Prof", "", "   ", "A", "Corporation",
    "Acme Inc.", "Team Rocket", "Studio Ghibli", "The Media Company",
    "Ludwig van Beethoven", "Vincent van Gogh", "Martin Luther King",
    "Isaac Newton", "Ursula Le Guin", "Jean de La Fontaine", "Olaudah Equiano",
    "Mary Shelley", "Dr", "Dr.", "King, Jr", "Smith, John", "John Smith III",
    "William Shakespeare", "Fyodor Dostoevsky", "Harper Lee", "Toni Morrison",
    "Andy Weir", "Dr Jane A Doe", "Rev John Doe", "Sir John Doe",
]

out = {
    "titles": {t: title_sort(t) for t in TITLES},
    "authors": {a: author_to_author_sort(a) for a in AUTHORS},
}
dest = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "crates", "lorebook-calibre", "tests", "calibre_expected.json",
)
with open(dest, "w", encoding="utf-8") as fh:
    json.dump(out, fh, ensure_ascii=False, indent=1, sort_keys=True)
print("wrote", dest)
print("titles:", len(out["titles"]), "authors:", len(out["authors"]))
for k in ["A Tale of Two Cities", "The Hobbit", "the Hobbit", "Die Verwandlung",
          '"A Tale of Two Cities"', "The\u00a0Hobbit"]:
    print(f"  title_sort({k!r}) = {out['titles'][k]!r}")
for k in ["Ursula K. Le Guin", "Acme Games", "Le Guin, Ursula K.", "Homer",
          "Dr Jane Doe", "John Doe Jr", "Ludwig van Beethoven"]:
    print(f"  author_sort({k!r}) = {out['authors'][k]!r}")
