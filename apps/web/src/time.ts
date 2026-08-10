// Relative-time helpers. Kept in their own module (rather than inline in
// app.ts) so they can be unit-tested — app.ts touches `document` at import
// time and can't be loaded outside a browser.

/// Compact relative age for dense rows: "now", "5m", "3h", "2d".
export function ago(iso: string, now: number = Date.now()): string {
  const s = (now - Date.parse(iso)) / 1000;
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/// The same age as a sentence-ready label. "now" is already past tense, so it
/// stays bare — appending " ago" to it is what produced "now  ago" (AMT-22).
export function agoLabel(iso: string, now: number = Date.now()): string {
  const a = ago(iso, now);
  return a === "now" ? a : `${a} ago`;
}
