import { expect, test } from "bun:test";
import { bindDraft, clearDraft, draftKey, type DraftStorage } from "./drafts";

function fakeStore(): DraftStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => (map.has(k) ? map.get(k)! : null),
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

function fakeField(value = "") {
  const listeners: (() => void)[] = [];
  return {
    value,
    addEventListener: (_: string, fn: () => void) => void listeners.push(fn),
    type(text: string) {
      this.value = text;
      listeners.forEach((fn) => fn());
    },
  };
}

test("a re-render repopulates the draft instead of losing it (AMT-28)", () => {
  const store = fakeStore();
  const key = draftKey("carbillpro", "CBP-31", "comment");

  // The human types; an agent's write forces a re-render (fresh empty field).
  const before = fakeField();
  bindDraft(before, key, store);
  before.type("half-written thought about the fix");
  const after = fakeField(); // render() rebuilt the DOM
  bindDraft(after, key, store);
  expect(after.value).toBe("half-written thought about the fix");
});

test("a draft never clobbers text already in the field", () => {
  const store = fakeStore();
  store.setItem("k", "old draft");
  const el = fakeField("newer text the render put there");
  bindDraft(el, "k", store);
  expect(el.value).toBe("newer text the render put there");
});

test("deliberately emptying the field withdraws the draft", () => {
  const store = fakeStore();
  const el = fakeField();
  bindDraft(el, "k", store);
  el.type("oops");
  el.type("");
  expect(store.getItem("k")).toBeNull();
  const rerendered = fakeField();
  bindDraft(rerendered, "k", store);
  expect(rerendered.value).toBe("");
});

test("posting clears the draft so it cannot ghost back", () => {
  const store = fakeStore();
  const key = draftKey("ametrite", "AMT-28", "comment");
  const el = fakeField();
  bindDraft(el, key, store);
  el.type("shipping it");
  clearDraft(key, store);
  const rerendered = fakeField();
  bindDraft(rerendered, key, store);
  expect(rerendered.value).toBe("");
});

test("draft keys are scoped by workspace and document", () => {
  expect(draftKey("carbillpro", "CBP-31", "comment")).toBe("amt-draft-carbillpro-CBP-31-comment");
  expect(draftKey("", "AMT-1", "comment")).toBe("amt-draft-default-AMT-1-comment");
  expect(draftKey("a", "X-1", "comment")).not.toBe(draftKey("b", "X-1", "comment"));
});
