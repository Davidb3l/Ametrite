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
    .sort(
      (a, b) =>
        pin(a.label) - pin(b.label) ||
        b.count - a.count ||
        a.label.localeCompare(b.label, undefined, { numeric: true }),
    );
}

/** How many chips the board shows before a "+N more" toggle. */
export const LABEL_CHIPS = 12;

/**
 * The chips to show: the first `limit` (unless expanded), plus the active
 * label wherever it ranks, so a filter is always visible and clearable.
 * `hidden` is how many labels the "+N more" toggle would reveal.
 */
export function visibleLabels(
  counts: LabelCount[],
  active?: string,
  expanded = false,
  limit = LABEL_CHIPS,
): { shown: LabelCount[]; hidden: number } {
  if (expanded) return { shown: counts, hidden: 0 };
  const shown = counts.filter((c, n) => n < limit || c.label === active);
  return { shown, hidden: counts.length - shown.length };
}

/** The issues carrying `label`, or all of them when no label is selected. */
export function withLabel<T extends { labels: string[] }>(issues: T[], label?: string): T[] {
  return label ? issues.filter((i) => i.labels.includes(label)) : issues;
}
