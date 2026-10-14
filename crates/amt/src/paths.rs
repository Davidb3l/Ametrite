//! Path normalization for Windows verbatim (`\\?\`) paths.
//!
//! `Path::canonicalize` on Windows returns the extended-length form
//! (`\\?\C:\Users\...`). That form is correct for the Win32 API but is stored
//! in the registry, printed to users, and written into batch files, where
//! `cmd.exe` rejects it as a current directory ("CMD does not support UNC
//! paths as current directories"). Everything user-visible or persisted should
//! use the plain form instead; this is the one place that converts.

use std::path::{Path, PathBuf};

/// The longest simplified path we hand out. Verbatim paths opt out of the
/// classic 260-character `MAX_PATH` limit, so a longer path must stay verbatim
/// to remain usable.
const MAX_SIMPLE_LEN: usize = 259;

/// Strip a Windows verbatim prefix from `path`, when the plain form means the
/// same thing:
///
/// * `\\?\C:\dir` becomes `C:\dir`
/// * `\\?\UNC\server\share\dir` becomes `\\server\share\dir`
///
/// Anything else (a plain path, a `\\?\Volume{...}` path, or one too long for
/// the plain form) is returned unchanged. Pure string logic, so it behaves the
/// same on every platform and is testable anywhere.
pub fn simplify_str(path: &str) -> String {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return path.to_string();
    };
    let simple = if let Some(unc) = strip_prefix_ci(rest, r"UNC\") {
        format!(r"\\{unc}")
    } else if is_drive_path(rest) {
        rest.to_string()
    } else {
        return path.to_string();
    };
    // A verbatim path may contain separators and names the plain form
    // reinterprets ("/", ".." are literal in verbatim form); keep those verbatim.
    if simple.len() > MAX_SIMPLE_LEN
        || rest.contains('/')
        || rest.split('\\').any(|c| c == "." || c == "..")
    {
        return path.to_string();
    }
    simple
}

/// `X:` followed by a separator or the end, e.g. `C:\dir` or `C:`.
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\')
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// [`simplify_str`] for a `Path`.
pub fn simplified(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(s) => PathBuf::from(simplify_str(s)),
        None => path.to_path_buf(),
    }
}

/// `Path::canonicalize`, minus the Windows verbatim prefix.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize().map(|p| simplified(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_and_unc_verbatim_paths_are_simplified() {
        assert_eq!(simplify_str(r"\\?\C:\Users\me\app"), r"C:\Users\me\app");
        assert_eq!(simplify_str(r"\\?\c:"), "c:");
        assert_eq!(
            simplify_str(r"\\?\UNC\server\share\dir"),
            r"\\server\share\dir"
        );
        assert_eq!(simplify_str(r"\\?\unc\server\share"), r"\\server\share");
    }

    #[test]
    fn other_paths_are_left_alone() {
        for p in [
            r"C:\Users\me",
            "/home/me/app",
            r"\\server\share",
            r"\\?\Volume{1234}\dir",
            r"\\?\C:\a\..\b",
            "",
        ] {
            assert_eq!(simplify_str(p), p, "{p}");
        }
        // Too long for the plain form: stays verbatim.
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(simplify_str(&long), long);
    }

    #[test]
    fn simplified_works_on_paths() {
        assert_eq!(
            simplified(Path::new(r"\\?\D:\x\y")),
            PathBuf::from(r"D:\x\y")
        );
    }
}
