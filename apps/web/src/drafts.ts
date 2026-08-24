// Draft persistence for fields that live inside #main (AMT-28). The board
// re-renders the whole main pane on every live SSE update, so any uncommitted
// input there — a half-typed comment — would be destroyed the moment an agent
// claims or comments the issue being viewed. Mirroring every keystroke into
// storage (and repopulating on render) makes re-renders, reloads, and browser
// crashes lossless; a draft is cleared only by a successful submit. The
// dialog-based forms (new issue, decision) hang off document.body and survive
// re-renders untouched, so they don't need this.

export interface DraftStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export const draftKey = (ws: string, doc: string, field: string): string =>
  `amt-draft-${ws || "default"}-${doc}-${field}`;

/// Repopulate `el` from a saved draft (saved text wins over the empty value a
/// fresh render produces — never over text already present), then mirror every
/// keystroke back to storage.
export function bindDraft(
  el: { value: string; addEventListener(type: string, fn: () => void): void },
  key: string,
  store: DraftStorage,
): void {
  const saved = store.getItem(key);
  if (saved !== null && !el.value) el.value = saved;
  el.addEventListener("input", () => {
    if (el.value) store.setItem(key, el.value);
    // Manually emptied = the human withdrew the draft; forget it, or the next
    // render would resurrect text they deliberately deleted.
    else store.removeItem(key);
  });
}

export const clearDraft = (key: string, store: DraftStorage): void => store.removeItem(key);
