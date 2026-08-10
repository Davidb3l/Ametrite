import { expect, test } from "bun:test";
import { ago, agoLabel } from "./time";

// A fixed "now" so these never depend on wall-clock timing.
const NOW = Date.parse("2026-08-10T12:00:00.000Z");
const secondsAgo = (n: number) => new Date(NOW - n * 1000).toISOString();

test("ago() is the compact form used in dense rows", () => {
  expect(ago(secondsAgo(0), NOW)).toBe("now");
  expect(ago(secondsAgo(59), NOW)).toBe("now");
  expect(ago(secondsAgo(60), NOW)).toBe("1m");
  expect(ago(secondsAgo(3599), NOW)).toBe("59m");
  expect(ago(secondsAgo(3600), NOW)).toBe("1h");
  expect(ago(secondsAgo(86399), NOW)).toBe("23h");
  expect(ago(secondsAgo(86400), NOW)).toBe("1d");
  expect(ago(secondsAgo(86400 * 9), NOW)).toBe("9d");
});

test("agoLabel() never renders 'now ago' (AMT-22)", () => {
  // The bug: four call sites appended " ago" unconditionally, so anything
  // under a minute rendered as "now  ago" with a double space.
  expect(agoLabel(secondsAgo(0), NOW)).toBe("now");
  expect(agoLabel(secondsAgo(59), NOW)).toBe("now");
  expect(agoLabel(secondsAgo(0), NOW)).not.toContain("ago");
  // Everything else reads as a sentence, with exactly one space.
  expect(agoLabel(secondsAgo(60), NOW)).toBe("1m ago");
  expect(agoLabel(secondsAgo(7200), NOW)).toBe("2h ago");
  expect(agoLabel(secondsAgo(86400 * 3), NOW)).toBe("3d ago");
  for (const s of [0, 30, 60, 3600, 86400]) {
    expect(agoLabel(secondsAgo(s), NOW)).not.toContain("  ");
  }
});

test("a future timestamp reads as 'now' rather than a negative age", () => {
  // Clock skew between the server and browser shouldn't render "-1m ago".
  expect(ago(secondsAgo(-30), NOW)).toBe("now");
  expect(agoLabel(secondsAgo(-30), NOW)).toBe("now");
});
