// Board label filter (AMT-44). Pure so the ordering and filtering rules are
// unit-tested; the board filters the issues it already fetched, so chip
// counts and column counts always agree.

/** Labels that lead the chip row when present: bugs should be one click away. */
export const PINNED_LABELS = ["bug"];

export type LabelCount = { label: string; count: number };

/**
 * Every label on `issues` with how many issues carry it: pinned labels first,
 * then by count (descending), then alphabetically. An `active` label is always
 * included (count 0 if no issue has it) so a filter can be cleared after the
 * view changes underneath it.
 */
export function labelCounts(issues: { labels: string[] }[], active?: string): LabelCount[] {
  const counts = new Map<string, number>();
  for (const i of issues) for (const l of new Set(i.labels)) counts.set(l, (counts.get(l) ?? 0) + 1);
  if (active && !counts.has(active)) counts.set(active, 0);
  const pin = (l: string) => {
    const p = PINNED_LABELS.indexOf(l);
    return p === -1 ? PINNED_LABELS.length : p;
  };
  return [...counts]
    .map(([label, count]) => ({ label, count }))
    .sort((a, b) => pin(a.label) - pin(b.label) || b.count - a.count || a.label.localeCompare(b.label));
}

/** The issues carrying `label`, or all of them when no label is selected. */
export function withLabel<T extends { labels: string[] }>(issues: T[], label?: string): T[] {
  return label ? issues.filter((i) => i.labels.includes(label)) : issues;
}
