use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Default, PartialEq)]
pub struct Extracted {
    pub links: Vec<String>,
    pub tags: Vec<String>,
}

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\[\]]+)\]\]").unwrap())
}

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A tag starts a word: "#rust", "#api/auth". Not markdown headings ("# Title"
    // has a space) and not fragments inside words ("foo#bar").
    RE.get_or_init(|| Regex::new(r"(?m)(?:^|[\s(])#([A-Za-z][A-Za-z0-9_/-]*)").unwrap())
}

/// Remove fenced code blocks and inline code spans so links/tags inside
/// code are not indexed.
fn strip_code(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push('\n');
            continue;
        }
        // strip inline `code` spans
        let mut in_span = false;
        for ch in line.chars() {
            if ch == '`' {
                in_span = !in_span;
                out.push(' ');
            } else if in_span {
                out.push(' ');
            } else {
                out.push(ch);
            }
        }
        out.push('\n');
    }
    out
}

/// Extract wikilink targets and #tags from a markdown body.
/// `[[Target|alias]]` yields "Target"; `[[Note#heading]]` yields "Note".
pub fn extract(body: &str) -> Extracted {
    let clean = strip_code(body);
    let mut links: Vec<String> = Vec::new();
    for cap in link_re().captures_iter(&clean) {
        let inner = &cap[1];
        let target = inner.split('|').next().unwrap_or(inner);
        let target = target.split('#').next().unwrap_or(target).trim();
        if !target.is_empty() && !links.iter().any(|l| l.eq_ignore_ascii_case(target)) {
            links.push(target.to_string());
        }
    }
    let mut tags: Vec<String> = Vec::new();
    for cap in tag_re().captures_iter(&clean) {
        let tag = cap[1].to_string();
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    Extracted { links, tags }
}

/// A cross-workspace link target `alias:KEY` splits into `(alias, key)`.
/// Both halves must be slug-like (alphanumerics, `-`, `_`) with no spaces, so
/// a note title like "R1: multi-workspace" is not mistaken for a link. Mirrors
/// the web renderer's regex exactly.
pub fn cross_workspace(raw: &str) -> Option<(&str, &str)> {
    let (alias, key) = raw.split_once(':')?;
    // Must lead with an alphanumeric, then alphanumerics/`-`/`_` — exactly the
    // web renderer's `^[A-Za-z0-9][\w-]*$`, so doctor and the board agree.
    let slug = |s: &str| {
        let mut chars = s.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    (slug(alias) && slug(key)).then_some((alias, key))
}

/// Kebab-case slug for note/project ids.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_links_and_tags() {
        let e = extract("See [[AMT-1]] and [[Session Tokens|the notes]] on #auth/tokens.\n# Heading\n`[[not-a-link]]`\n```\n#not-a-tag [[nope]]\n```");
        assert_eq!(e.links, vec!["AMT-1", "Session Tokens"]);
        assert_eq!(e.tags, vec!["auth/tokens"]);
    }

    #[test]
    fn link_heading_suffix_stripped() {
        let e = extract("[[Note#section]]");
        assert_eq!(e.links, vec!["Note"]);
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Session Tokens & Auth!"), "session-tokens-auth");
        assert_eq!(slugify("  "), "untitled");
    }

    #[test]
    fn cross_workspace_targets() {
        assert_eq!(cross_workspace("web:AMT-3"), Some(("web", "AMT-3")));
        assert_eq!(cross_workspace("my-app:CLAP-12"), Some(("my-app", "CLAP-12")));
        // local links (no colon) are not cross-workspace
        assert_eq!(cross_workspace("AMT-1"), None);
        assert_eq!(cross_workspace("D-2"), None);
        // a note title with a colon+space is not an alias:KEY link
        assert_eq!(cross_workspace("R1: multi-workspace"), None);
        // empty halves don't count
        assert_eq!(cross_workspace(":AMT-1"), None);
        assert_eq!(cross_workspace("web:"), None);
        // must lead with an alphanumeric (mirrors the web regex) so doctor and
        // the board never disagree on what is a cross-workspace link
        assert_eq!(cross_workspace("_stg:BAR"), None);
        assert_eq!(cross_workspace("-web:AMT-1"), None);
    }
}
