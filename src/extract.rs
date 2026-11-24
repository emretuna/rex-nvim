//! Pure path extraction from agent-pane output.
//!
//! Reads a raw capture (newest lines last), strips TUI chrome, and returns
//! file paths mentioned in it, newest-first. No I/O: existence is checked
//! through an injected closure.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::absolute_path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrapedPath {
    pub path: String,
    pub line: Option<u32>,
}

/// Extract file-path candidates from `text`.
pub fn extract(text: &str, cwd: &Path, exists: &dyn Fn(&Path) -> bool) -> Vec<ScrapedPath> {
    const CHROME: [char; 13] = [
        '│', '┃', '╭', '╮', '╰', '╯', '─', '━', '═', '├', '┤', '●', '∴',
    ];

    let mut all: Vec<ScrapedPath> = Vec::new();
    for line in text.lines() {
        let cleaned = line.replace(CHROME, " ");
        for raw in cleaned.split_whitespace() {
            let tok = trim_edges(raw);
            if tok.is_empty() {
                continue;
            }
            if let Some((path, lineno)) = parse_token(tok) {
                let resolved = absolute_path(cwd, path);
                if exists(&resolved) {
                    all.push(ScrapedPath {
                        path: resolved.to_string_lossy().into_owned(),
                        line: lineno,
                    });
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cand in all.into_iter().rev() {
        if seen.insert(cand.path.clone()) {
            out.push(cand);
        }
    }
    out
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | '\\' | '~' | '@' | ':')
}

fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c: char| !is_path_char(c))
}

fn strip_trailing_number(s: &str) -> Option<(&str, u32)> {
    let colon = s.rfind(':')?;
    let tail = &s[colon + 1..];
    if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((&s[..colon], tail.parse().ok()?))
}

/// Decide whether `tok` is path-shaped and split off a trailing `:line[:col]`.
pub fn parse_token(tok: &str) -> Option<(&str, Option<u32>)> {
    if tok.contains("://") {
        return None;
    }
    let (mut path, mut line) = (tok, None);
    if let Some((rest, first)) = strip_trailing_number(path) {
        if let Some((rest2, real_line)) = strip_trailing_number(rest) {
            path = rest2;
            line = Some(real_line);
        } else {
            path = rest;
            line = Some(first);
        }
    }

    if !path.contains('/') && !path.contains('\\') {
        return None;
    }
    let is_abs = Path::new(path).is_absolute() || path.starts_with('~');
    let is_dotslash = path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with(r".\")
        || path.starts_with(r"..\");
    let last = path.rsplit(['/', '\\']).next().unwrap_or("");
    let has_ext = last.contains('.');
    if !(is_abs || is_dotslash || has_ext) {
        return None;
    }
    Some((path, line))
}

/// Existence check over the real filesystem, used by the picker.
pub fn exists(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always(_: &Path) -> bool {
        true
    }

    #[test]
    fn extracts_absolute_with_line() {
        let c = extract(
            "error at /tmp/a/b.rs:42:7 in build",
            Path::new("/x"),
            &always,
        );
        assert_eq!(
            c,
            vec![ScrapedPath {
                path: "/tmp/a/b.rs".to_owned(),
                line: Some(42)
            }]
        );
    }

    #[test]
    fn resolves_relative_against_cwd() {
        let c = extract("modified src/main.rs", Path::new("/repo"), &always);
        assert_eq!(c[0].path, "/repo/src/main.rs");
    }

    #[test]
    fn strips_box_chrome() {
        let c = extract("│ ● /tmp/x.py │", Path::new("/"), &always);
        assert_eq!(c[0].path, "/tmp/x.py");
    }

    #[test]
    fn dedupes_keeping_newest_and_orders_newest_first() {
        let text = "/tmp/old.rs\n/tmp/a.rs\n/tmp/old.rs:9";
        let c = extract(text, Path::new("/"), &always);
        assert_eq!(
            c[0],
            ScrapedPath {
                path: "/tmp/old.rs".to_owned(),
                line: Some(9)
            }
        );
        assert_eq!(c[1].path, "/tmp/a.rs");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn drops_nonexistent() {
        assert!(extract("/tmp/ghost.rs", Path::new("/"), &|_| false).is_empty());
    }

    #[test]
    fn ignores_scheme_urls() {
        assert_eq!(parse_token("http://foo/bar.rs"), None);
        assert_eq!(parse_token("https://example.com/a/b.py:12"), None);
        assert_eq!(
            parse_token("src/main.rs:42:7"),
            Some(("src/main.rs", Some(42)))
        );
        assert_eq!(
            parse_token("src/main.rs:42"),
            Some(("src/main.rs", Some(42)))
        );
        assert_eq!(parse_token("src/main.rs"), Some(("src/main.rs", None)));
    }

    #[test]
    fn prose_without_extension_or_slash_is_rejected() {
        assert_eq!(parse_token("and/or"), None);
    }
    #[test]
    fn exists_rejects_directories() {
        let dir = std::env::temp_dir().join(format!("rex-nvim-exists-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(exists(&file));
        assert!(!exists(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
