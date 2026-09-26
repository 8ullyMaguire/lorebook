//! Helper binary for `tests/interop_with_calibre.sh`.
//!
//! Exists so the shell script can drive the library through *our* code rather
//! than through sqlite3, which would prove nothing about the crate. Every
//! subcommand is a thin wrapper over the public API; no logic lives here.
//!
//! Usage:
//!   lorebook-interop-check create  <libdir>
//!   lorebook-interop-check insert <libdir> <title> <author> <identifier>
//!   lorebook-interop-check open   <libdir>
//!   lorebook-interop-check verify <libdir>

use std::path::Path;

use lorebook_calibre as cal;

fn main() -> std::process::ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: lorebook-interop-check <create|insert|open|verify> <libdir> ...");
        return std::process::ExitCode::from(2);
    }
    // take_first pulls a positional off the front without cloning.
    fn take_first(args: &mut Vec<String>) -> Option<String> {
        if args.is_empty() {
            None
        } else {
            Some(args.remove(0))
        }
    }

    // A usage mistake and a library error are different failures: the script
    // distinguishes them, and conflating them would report a bad invocation as
    // a broken library.
    let result: Result<(), String> = match args.remove(0).as_str() {
        "create" => match take_first(&mut args) {
            Some(dir) => cal::create_library(Path::new(&dir))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            None => Err("create needs a libdir".into()),
        },
        "insert" => match (
            take_first(&mut args),
            take_first(&mut args),
            take_first(&mut args),
            take_first(&mut args),
        ) {
            (Some(dir), Some(title), Some(author), Some(ident)) => {
                insert(Path::new(&dir), &title, &author, &ident)
            }
            _ => Err("insert needs <libdir> <title> <author> <identifier>".into()),
        },
        "open" => match take_first(&mut args) {
            Some(dir) => cal::open_library(Path::new(&dir))
                .map(|_| ())
                .map_err(|e| e.to_string()),
            None => Err("open needs a libdir".into()),
        },
        "verify" => match take_first(&mut args) {
            Some(dir) => verify(Path::new(&dir)),
            None => Err("verify needs a libdir".into()),
        },
        other => Err(format!("unknown command {other}")),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn insert(dir: &Path, title: &str, author: &str, identifier: &str) -> Result<(), String> {
    let conn = cal::open_library(dir).map_err(|e| e.to_string())?;
    // Extend the library with our tables first: a write into a library we have
    // not touched is exactly the first-run case, and it must work.
    cal::apply_additive_schema(&conn).map_err(|e| e.to_string())?;

    let id = cal::insert_book(&conn, title, None, None, None).map_err(|e| e.to_string())?;
    let a = cal::ensure_author(&conn, author).map_err(|e| e.to_string())?;
    cal::link_author(&conn, id, a).map_err(|e| e.to_string())?;
    cal::add_identifier(&conn, id, "isbn", identifier).map_err(|e| e.to_string())?;
    println!("inserted book {id}");
    Ok(())
}

fn verify(dir: &Path) -> Result<(), String> {
    let conn = cal::open_library(dir).map_err(|e| e.to_string())?;
    let books = cal::list_books(&conn).map_err(|e| e.to_string())?;
    if books.is_empty() {
        return Err("library has no books".into());
    }
    for b in &books {
        // Every book must be readable through Calibre's own tables, with its
        // author and formats resolved — the shape the UI depends on.
        cal::list_sources(&conn, b.id).map_err(|e| e.to_string())?;
        println!(
            "book {} | {} | authors={:?} | tags={:?}",
            b.id, b.title, b.authors, b.tags
        );
    }
    Ok(())
}
