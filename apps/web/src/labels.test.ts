import { expect, test } from "bun:test";
import { labelCounts, withLabel } from "./labels";

const issues = [
  { id: "A-1", labels: ["web", "feature"] },
  { id: "A-2", labels: ["web", "bug"] },
  { id: "A-3", labels: ["web"] },
  { id: "A-4", labels: ["cli", "feature"] },
  { id: "A-5", labels: [] },
];

test("bug leads the chip row, then by count, then alphabetically", () => {
  expect(labelCounts(issues)).toEqual([
    { label: "bug", count: 1 },
    { label: "web", count: 3 },
    { label: "feature", count: 2 },
    { label: "cli", count: 1 },
  ]);
});

test("no bug label means no bug chip", () => {
  expect(labelCounts([{ labels: ["web"] }]).map((c) => c.label)).toEqual(["web"]);
});

test("the active label stays clearable when no issue in view carries it", () => {
  expect(labelCounts([{ labels: ["web"] }], "bug")).toEqual([
    { label: "bug", count: 0 },
    { label: "web", count: 1 },
  ]);
});

test("a label repeated on one issue counts once", () => {
  expect(labelCounts([{ labels: ["bug", "bug"] }])).toEqual([{ label: "bug", count: 1 }]);
});

test("filtering keeps only issues with the label, or everything when none is set", () => {
  expect(withLabel(issues, "bug").map((i) => i.id)).toEqual(["A-2"]);
  expect(withLabel(issues, "web").map((i) => i.id)).toEqual(["A-1", "A-2", "A-3"]);
  expect(withLabel(issues).length).toBe(5);
  expect(withLabel(issues, "nope")).toEqual([]);
});
