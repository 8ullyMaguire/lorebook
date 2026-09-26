/**
 * Which library is open, and the current page of books.
 *
 * A Svelte 5 rune module rather than a context: there is exactly one library
 * open at a time (the Rust side holds a single `Option<Library>`), so a
 * module-level rune is the accurate model. A context would imply a tree of
 * libraries that cannot exist.
 */
import * as api from './api';

/** Books per request. Matches the shell's default page size. */
export const PAGE_SIZE = 100;

class LibraryStore {
  path = $state<string | null>(null);
  total = $state(0);
  books = $state<api.Book[]>([]);
  selected = $state<api.Book | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);

  get isOpen() {
    return this.path !== null;
  }

  get pageCount() {
    return Math.max(1, Math.ceil(this.total / PAGE_SIZE));
  }

  private fail(e: unknown) {
    // Tauri rejects with a plain string, because the Rust side converts
    // CalibreError to a message for display. Anything else is a bug and is
    // worth seeing as such.
    this.error = typeof e === 'string' ? e : String(e);
  }

  async open(path: string) {
    this.loading = true;
    this.error = null;
    try {
      this.total = await api.openLibrary(path);
      this.path = path;
      await this.reload();
    } catch (e) {
      this.fail(e);
    } finally {
      this.loading = false;
    }
  }

  async create(path: string) {
    this.loading = true;
    this.error = null;
    try {
      this.total = await api.createLibrary(path);
      this.path = path;
      await this.reload();
    } catch (e) {
      this.fail(e);
    } finally {
      this.loading = false;
    }
  }

  async reload() {
    if (!this.isOpen) return;
    this.loading = true;
    try {
      const page = await api.listBooks(PAGE_SIZE, 0);
      this.books = page.books;
      this.total = page.total;
    } catch (e) {
      this.fail(e);
    } finally {
      this.loading = false;
    }
  }

  async close() {
    try {
      await api.closeLibrary();
    } catch (e) {
      this.fail(e);
    }
    this.path = null;
    this.books = [];
    this.total = 0;
    this.selected = null;
  }

  /** Load a book's full detail, including its formats and file sources. */
  async select(id: number) {
    try {
      this.selected = await api.getBook(id);
    } catch (e) {
      this.fail(e);
    }
  }

  clearError() {
    this.error = null;
  }
}

export const library = new LibraryStore();
