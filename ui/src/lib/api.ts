/**
 * The typed boundary to Rust.
 *
 * Every call into the backend goes through here, for one reason: the command
 * names and payload shapes are declared in exactly one place, so a rename in
 * `src-tauri/src/lib.rs` breaks the build here instead of failing at runtime
 * with "command not found" in front of a user.
 *
 * Tauri lowercases command names by default, so `list_books` is invoked as
 * `list_books`. Arguments are a single object, matching the `rename_all =
 * "camelCase"` on the Rust DTOs.
 */
import { invoke } from '@tauri-apps/api/core';

/** One book, as `BookDto` in `src-tauri/src/lib.rs`. */
export interface Book {
  id: number;
  title: string;
  sort: string | null;
  authorSort: string | null;
  timestampMs: number | null;
  pubdateMs: number | null;
  seriesIndex: number;
  hasCover: boolean;
  authors: string[];
  tags: string[];
  series: string | null;
  formats: string[];
}

/** One page of books, as `BookPage`. */
export interface BookPage {
  books: Book[];
  total: number;
  offset: number;
  limit: number;
}

export function openLibrary(path: string): Promise<number> {
  return invoke<number>('open_library', { path });
}

export function createLibrary(path: string): Promise<number> {
  return invoke<number>('create_library', { path });
}

export function closeLibrary(): Promise<void> {
  return invoke<void>('close_library');
}

export function libraryPath(): Promise<string | null> {
  return invoke<string | null>('library_path');
}

export function listBooks(limit: number, offset: number): Promise<BookPage> {
  return invoke<BookPage>('list_books', { limit, offset });
}

export function getBook(id: number): Promise<Book | null> {
  return invoke<Book | null>('get_book', { id });
}
