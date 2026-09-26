<!--
  The app's single route, in two states: a chooser until a library is open,
  then the library view.

  One route rather than two because the choice is a mode, not a place: opening a
  library replaces the chooser, and closing it returns there. A router would add
  a navigation model for a two-state switch.
-->
<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { library, PAGE_SIZE } from '$lib/library.svelte';
  import type { Book } from '$lib/api';

  let page = $state(0);
  let detail = $state<Book | null>(null);

  /** Pick a directory and hand it to the right action. */
  async function pick(mode: 'open' | 'create') {
    const picked = await open({
      directory: true,
      multiple: false,
      title: mode === 'open' ? 'Open Calibre library' : 'Create library'
    });
    // Tauri returns null on cancel. Cancelling is not an error and must not put
    // a message on screen.
    if (typeof picked !== 'string') return;
    if (mode === 'open') await library.open(picked);
    else await library.create(picked);
    page = 0;
    detail = null;
  }

  async function select(id: number) {
    await library.select(id);
    detail = library.selected;
  }

  async function close() {
    await library.close();
    detail = null;
    page = 0;
  }

  const pageBooks = $derived(
    library.isOpen ? library.books.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE) : []
  );

  function fmt(ms: number | null) {
    if (ms === null) return '\u2014';
    return new Date(ms).toISOString().slice(0, 10);
  }
</script>

{#if !library.isOpen}
  <main class="chooser">
    <h1>Lorebook</h1>
    <p class="sub">A local ebook library that reads and writes Calibre libraries directly.</p>
    <div class="actions">
      <button onclick={() => pick('open')} disabled={library.loading}>Open library\u2026</button>
      <button onclick={() => pick('create')} disabled={library.loading} class="secondary">
        Create library\u2026
      </button>
    </div>
    {#if library.error}<p class="error" role="alert">{library.error}</p>{/if}
  </main>
{:else}
  <main class="library">
    <header>
      <span class="path" title={library.path ?? ''}>{library.path}</span>
      <span class="count">{library.total} books</span>
      <button onclick={close} class="link">close</button>
    </header>

    {#if library.error}
      <p class="error" role="alert">
        {library.error}
        <button onclick={() => library.clearError()} class="link">dismiss</button>
      </p>
    {/if}

    <div class="body">
      <section class="list">
        <table>
          <thead>
            <tr><th>Title</th><th>Authors</th><th>Formats</th></tr>
          </thead>
          <tbody>
            {#each pageBooks as book (book.id)}
              <tr class:selected={detail?.id === book.id} onclick={() => select(book.id)}>
                <td>{book.title}</td>
                <td>{book.authors.join(', ') || '\u2014'}</td>
                <td class="formats">{book.formats.join(' ') || '\u2014'}</td>
              </tr>
            {:else}
              <tr><td colspan="3" class="empty">No books in this library yet.</td></tr>
            {/each}
          </tbody>
        </table>

        {#if library.pageCount > 1}
          <nav>
            <button onclick={() => (page = Math.max(0, page - 1))} disabled={page === 0}>
              \u2039 Prev
            </button>
            <span>Page {page + 1} of {library.pageCount}</span>
            <button
              onclick={() => (page = Math.min(library.pageCount - 1, page + 1))}
              disabled={page + 1 >= library.pageCount}
            >
              Next \u203a
            </button>
          </nav>
        {/if}
      </section>

      <aside class="detail">
        {#if detail}
          <h2>{detail.title}</h2>
          <dl>
            <dt>Authors</dt><dd>{detail.authors.join(', ') || '\u2014'}</dd>
            <dt>Series</dt>
            <dd>{detail.series ? `${detail.series} #${detail.seriesIndex}` : '\u2014'}</dd>
            <dt>Formats</dt><dd>{detail.formats.join(', ') || '\u2014'}</dd>
            <dt>Tags</dt><dd>{detail.tags.join(', ') || '\u2014'}</dd>
            <dt>Added</dt><dd>{fmt(detail.timestampMs)}</dd>
            <dt>Published</dt><dd>{fmt(detail.pubdateMs)}</dd>
            <dt>Calibre id</dt><dd>{detail.id}</dd>
          </dl>
        {:else}
          <p class="empty">Select a book to see its details.</p>
        {/if}
      </aside>
    </div>
  </main>
{/if}

<style>
  :global(body) { margin: 0; background: #0d0d0f; color: #e8e8ea; }
  button {
    padding: 0.5rem 1rem; font: inherit; font-size: 0.95rem;
    border: 1px solid #3a3a3f; border-radius: 6px;
    background: #1c1c20; color: #e8e8ea; cursor: pointer;
  }
  button:hover:not(:disabled) { background: #26262c; }
  button:disabled { opacity: 0.45; cursor: default; }
  .link { background: none; border: none; color: #6ea8fe; cursor: pointer; padding: 0; }
  .error {
    background: #2a1414; color: #ff8080; margin: 0;
    padding: 0.5rem 1rem; display: flex; gap: 0.75rem; align-items: baseline;
  }

  .chooser {
    display: flex; flex-direction: column; align-items: center;
    justify-content: center; height: 100vh; gap: 0.75rem; font-family: system-ui, sans-serif;
  }
  .chooser h1 { margin: 0; font-size: 2rem; }
  .sub { margin: 0 0 1rem; color: #8b8b93; }
  .actions { display: flex; gap: 0.75rem; }
  .secondary { background: transparent; }

  .library { font-family: system-ui, sans-serif; height: 100vh; display: flex; flex-direction: column; }
  header {
    display: flex; gap: 1rem; align-items: center;
    padding: 0.5rem 1rem; border-bottom: 1px solid #26262c;
  }
  .path { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #8b8b93; }
  .count { color: #8b8b93; font-variant-numeric: tabular-nums; }
  .body { display: flex; flex: 1; min-height: 0; }
  .list { flex: 1; overflow: auto; }
  table { width: 100%; border-collapse: collapse; }
  th {
    text-align: left; padding: 0.4rem 0.75rem;
    border-bottom: 1px solid #26262c; color: #8b8b93; font-weight: 500;
    position: sticky; top: 0; background: #0d0d0f;
  }
  td { padding: 0.4rem 0.75rem; border-bottom: 1px solid #1a1a1f; }
  tbody tr { cursor: pointer; }
  tbody tr:hover { background: #16161a; }
  tbody tr.selected { background: #23232a; }
  .formats { font-family: ui-monospace, monospace; color: #8b8b93; }
  .empty { color: #63636b; padding: 1rem; }
  nav { display: flex; gap: 1rem; align-items: center; justify-content: center; padding: 0.75rem; }
  nav button:disabled { color: #45454c; cursor: default; }
  .detail { width: 22rem; border-left: 1px solid #26262c; padding: 1rem; overflow: auto; }
  .detail h2 { margin-top: 0; font-size: 1.1rem; }
  dl { display: grid; grid-template-columns: 6rem 1fr; gap: 0.35rem 0.75rem; }
  dt { color: #8b8b93; }
  dd { margin: 0; overflow-wrap: anywhere; }
</style>
