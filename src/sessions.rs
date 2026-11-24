//! Agent session-log mining for the picker's first tier.
//!
//! Rex has no agent-session registry, so the log is located from the agent
//! kind and its cwd: the newest `*.jsonl` under the standard per-agent layout.
//! Only pi/omp and claude have parsers; every other kind yields an empty
//! result and the picker falls back to scraping the pane.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::paths::{absolute_path, home_dir};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawOp {
    Write,
    Edit,
    Read,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    pub path: String,
    pub op: RawOp,
    pub unix_ts: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MinedTouch {
    pub path: String,
    /// True iff any Edit/Write event was seen for this path.
    pub was_edited: bool,
    /// True iff the first op seen for this path was a write.
    pub newly_created: bool,
    /// Latest timestamp among all of the path's events (read or edit).
    pub last_touch_unix: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Mined {
    pub touches: Vec<MinedTouch>,
    /// First mined tool-call timestamp; the picker's `git log --since` bound.
    pub first_op_unix: Option<u64>,
}

fn is_agy(agent_kind: &str) -> bool {
    matches!(agent_kind, "agy" | "antigravity" | "antigravity_cli")
}

/// Mine the agent's newest session log for `cwd`. Any miss, parse failure, or
/// unknown agent kind yields an empty result; never an error.
pub fn mine(agent_kind: &str, cwd: &str) -> Mined {
    let Some(text) = load_session_text(agent_kind, Path::new(cwd)) else {
        return Mined::default();
    };
    mine_text(agent_kind, &text, cwd)
}

/// The raw text of the newest session log for the agent in `cwd`.
pub fn load_session_text(agent_kind: &str, cwd: &Path) -> Option<String> {
    let home = home_dir()?;
    let cwd_text = cwd.to_string_lossy().into_owned();
    if agent_kind == "claude" {
        let projects = home.join(".claude/projects");
        let slug = cwd_text.replace('/', "-");
        let dir = projects.join(slug);
        let newest = newest_jsonl(&dir)?;
        std::fs::read_to_string(newest).ok()
    } else if agent_kind == "pi" || agent_kind == "omp" {
        let mut candidates = Vec::new();
        for base in [
            home.join(".omp/agent/sessions"),
            home.join(".pi/agent/sessions"),
        ] {
            // Real layouts use the cwd relative to home with `/` -> `-` and a
            // leading `-` (`~/Code/x` -> `-Code-x`); accept the absolute
            // spelling too, since layouts drift across versions.
            if let Ok(rel) = cwd.strip_prefix(&home) {
                candidates.push(base.join(format!("-{}", rel.to_string_lossy().replace('/', "-"))));
            }
            candidates.push(base.join(cwd_text.replace('/', "-")));
        }
        let dir = candidates
            .iter()
            .filter(|dir| dir.is_dir())
            .max_by_key(|dir| newest_jsonl(dir).and_then(mtime))?;
        let newest = newest_jsonl(dir)?;
        let text = std::fs::read_to_string(&newest).ok()?;
        // The newest log in a slug dir should be this cwd's; confirm via the
        // session header before trusting it (title records may precede it).
        if session_cwd_matches(&text, &cwd_text) {
            Some(text)
        } else {
            None
        }
    } else {
        None
    }
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

fn mtime(path: PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// The session header is a `{"type":"session","cwd":...}` record; it may be
/// preceded by title/service records, so scan the first few lines.
fn session_cwd_matches(text: &str, cwd: &str) -> bool {
    for line in text.lines().take(10) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            return value
                .get("cwd")
                .and_then(Value::as_str)
                .map(|logged| logged.trim_end_matches('/') == cwd.trim_end_matches('/'))
                .unwrap_or(false);
        }
    }
    false
}

fn pi_tool_map(name: &str) -> Option<RawOp> {
    match name {
        "write" => Some(RawOp::Write),
        "edit" => Some(RawOp::Edit),
        "read" => Some(RawOp::Read),
        _ => None,
    }
}

fn claude_tool_map(name: &str) -> Option<RawOp> {
    match name {
        "Write" => Some(RawOp::Write),
        "MultiEdit" | "NotebookEdit" | "Edit" => Some(RawOp::Edit),
        "Read" => Some(RawOp::Read),
        _ => None,
    }
}

/// Per-agent JSONL shape: everything that differs between agents' records,
/// expressed as data rather than parallel parser loops.
struct Dialect {
    record_type: Option<&'static str>,
    content_pointer: &'static str,
    item_type: Option<&'static str>,
    path_pointers: &'static [&'static str],
    strip_at_prefix: bool,
    tool_map: fn(&str) -> Option<RawOp>,
}

const PI_DIALECT: Dialect = Dialect {
    record_type: Some("message"),
    content_pointer: "/message/content",
    item_type: Some("toolCall"),
    path_pointers: &["/arguments/path", "/arguments/file_path"],
    strip_at_prefix: true,
    tool_map: pi_tool_map,
};

const CLAUDE_DIALECT: Dialect = Dialect {
    record_type: None,
    content_pointer: "/message/content",
    item_type: Some("tool_use"),
    path_pointers: &["/input/file_path", "/input/path"],
    strip_at_prefix: false,
    tool_map: claude_tool_map,
};

/// Parse a session JSONL's text into path events for `agent_kind`.
pub fn parse_session(text: &str, agent_kind: &str) -> Vec<RawEvent> {
    let dialect = if agent_kind == "claude" {
        &CLAUDE_DIALECT
    } else {
        &PI_DIALECT
    };
    let record_type = dialect.record_type;
    let content_pointer = dialect.content_pointer;
    let item_type = dialect.item_type;
    let path_pointers = dialect.path_pointers;
    let strip_at = dialect.strip_at_prefix;
    let tool_map = dialect.tool_map;

    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(want_type) = record_type {
            if value.get("type").and_then(Value::as_str) != Some(want_type) {
                continue;
            }
        }
        let unix_ts = value
            .get("timestamp")
            .or_else(|| value.get("created_at"))
            .and_then(Value::as_str)
            .and_then(parse_iso8601_unix);
        let Some(content) = value.pointer(content_pointer).and_then(Value::as_array) else {
            continue;
        };
        for item in content {
            if let Some(want_item) = item_type {
                if item.get("type").and_then(Value::as_str) != Some(want_item) {
                    continue;
                }
            }
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(op) = tool_map(name) else {
                continue;
            };
            let Some(path) = path_pointers
                .iter()
                .find_map(|p| item.pointer(p).and_then(Value::as_str))
            else {
                continue;
            };
            let path = if strip_at {
                path.strip_prefix('@').unwrap_or(path)
            } else {
                path
            };
            out.push(RawEvent {
                path: path.to_owned(),
                op,
                unix_ts,
            });
        }
    }
    out
}

/// Reduce raw events into per-path touches, resolving each logged spelling
/// against `cwd` exactly once.
pub fn mine_text(agent_kind: &str, text: &str, cwd: &str) -> Mined {
    if agent_kind == "codex" || is_agy(agent_kind) {
        return Mined::default();
    }
    let raw = parse_session(text, agent_kind);
    let first_op_unix = raw.iter().find_map(|event| event.unix_ts);

    struct Acc {
        first_op: RawOp,
        last_touch_unix: Option<u64>,
        ever_edited: bool,
    }
    let mut by_path: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut resolved: std::collections::HashMap<&str, String> = std::collections::HashMap::new();

    for event in &raw {
        let path = resolved
            .entry(event.path.as_str())
            .or_insert_with(|| {
                absolute_path(Path::new(cwd), &event.path)
                    .to_string_lossy()
                    .into_owned()
            })
            .clone();
        if !by_path.contains_key(&path) {
            order.push(path.clone());
        }
        let entry = by_path.entry(path).or_insert_with(|| Acc {
            first_op: event.op,
            last_touch_unix: None,
            ever_edited: false,
        });
        if matches!(event.op, RawOp::Edit | RawOp::Write) {
            entry.ever_edited = true;
        }
        if event.unix_ts.is_some() {
            entry.last_touch_unix = event.unix_ts;
        }
    }

    let touches = order
        .into_iter()
        .map(|path| {
            let acc = &by_path[&path];
            MinedTouch {
                path,
                was_edited: acc.ever_edited,
                newly_created: matches!(acc.first_op, RawOp::Write),
                last_touch_unix: acc.last_touch_unix,
            }
        })
        .collect();

    Mined {
        touches,
        first_op_unix,
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` into Unix seconds; `None` for anything
/// else (never panics).
pub fn parse_iso8601_unix(s: &str) -> Option<u64> {
    if s.len() < 20 || !s.ends_with('Z') || s.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + (hour * 3600 + minute * 60 + second) as i64) as u64)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn file_mtime_unix(path: &str) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PI_LOG: &str = concat!(
        r#"{"type":"session","version":3,"id":"x","timestamp":"2026-07-22T11:07:49.944Z","cwd":"/repo"}"#,
        "\n",
        r#"{"type":"message","timestamp":"2026-07-22T11:08:00.000Z","message":{"role":"assistant","content":[{"type":"toolCall","name":"read","arguments":{"path":"/repo/src/lib.rs"}}]}}"#,
        "\n",
        r#"{"type":"message","timestamp":"2026-07-22T11:09:00.000Z","message":{"role":"assistant","content":[{"type":"toolCall","name":"write","arguments":{"path":"@/repo/src/new_mod.rs"}}]}}"#,
        "\n",
        r#"{"type":"message","timestamp":"2026-07-22T11:10:00.000Z","message":{"role":"assistant","content":[{"type":"toolCall","name":"edit","arguments":{"file_path":"/repo/src/lib.rs"}}]}}"#,
        "\n",
        r#"{"type":"message","timestamp":"2026-07-22T11:11:00.000Z","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{"command":"ls"}}]}}"#,
        "\n",
    );

    const CLAUDE_LOG: &str = concat!(
        r#"{"type":"assistant","timestamp":"2026-07-19T16:51:55.425Z","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/repo/src/lib.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-19T16:52:00.000Z","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/repo/src/new_mod.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-19T16:53:00.000Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/repo/src/lib.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-19T16:54:00.000Z","message":{"content":[{"type":"tool_use","name":"MultiEdit","input":{"path":"/repo/src/lib.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-19T16:55:00.000Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        "\n",
    );

    #[test]
    fn pi_dialect_mines_read_write_edit_and_ignores_bash() {
        let events = parse_session(PI_LOG, "pi");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].path, "/repo/src/lib.rs");
        assert_eq!(events[0].op, RawOp::Read);
        assert_eq!(events[0].unix_ts, Some(1784718480));
        assert_eq!(events[1].path, "/repo/src/new_mod.rs");
        assert_eq!(events[1].op, RawOp::Write);
        assert_eq!(events[2].path, "/repo/src/lib.rs");
        assert_eq!(events[2].op, RawOp::Edit);
    }

    #[test]
    fn omp_uses_pi_dialect() {
        let mined = mine_text("omp", PI_LOG, "/repo");
        assert_eq!(mined.touches.len(), 2);
        assert_eq!(mined.first_op_unix, Some(1784718480));
        let new_mod = mined
            .touches
            .iter()
            .find(|t| t.path == "/repo/src/new_mod.rs")
            .unwrap();
        assert!(new_mod.newly_created && new_mod.was_edited);
        let lib = mined
            .touches
            .iter()
            .find(|t| t.path == "/repo/src/lib.rs")
            .unwrap();
        assert!(!lib.newly_created);
        assert!(lib.was_edited);
        assert_eq!(lib.last_touch_unix, Some(1784718600));
    }

    #[test]
    fn claude_dialect_mines_and_folds_multiedit() {
        let events = parse_session(CLAUDE_LOG, "claude");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].path, "/repo/src/lib.rs");
        assert_eq!(events[0].op, RawOp::Read);
        assert_eq!(events[1].op, RawOp::Write);
        assert_eq!(events[2].op, RawOp::Edit);
        assert_eq!(events[3].op, RawOp::Edit);
    }

    #[test]
    fn new_paths_resolve_against_cwd() {
        let mined = mine_text("pi", PI_LOG, "/repo");
        assert!(!mined.touches.is_empty());
        let text = r#"{"type":"message","timestamp":"2026-07-22T11:08:00Z","message":{"content":[{"type":"toolCall","name":"edit","arguments":{"path":"src/rel.rs"}}]}}"#;
        let mined = mine_text("pi", text, "/repo");
        assert_eq!(mined.touches[0].path, "/repo/src/rel.rs");
    }

    #[test]
    fn codex_and_unknown_yield_empty() {
        assert!(mine_text("codex", PI_LOG, "/repo").touches.is_empty());
        assert!(mine_text("agy", PI_LOG, "/repo").touches.is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let text = "not json\n{\"type\":\"message\",\"timestamp\":\"2026-07-22T11:08:00Z\",\"message\":{\"content\":[{\"type\":\"toolCall\",\"name\":\"read\",\"arguments\":{\"path\":\"/repo/a.rs\"}}]}}\n";
        assert_eq!(parse_session(text, "pi").len(), 1);
    }

    #[test]
    fn iso_timestamps() {
        assert_eq!(parse_iso8601_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601_unix("2026-07-22T11:07:49.944Z"),
            Some(1784718469)
        );
        assert_eq!(parse_iso8601_unix("2024-02-29T00:00:00Z"), Some(1709164800));
        assert_eq!(parse_iso8601_unix("nope"), None);
    }

    #[test]
    fn session_header_scans_past_title_records() {
        let text =
            "{\"type\":\"title\",\"title\":\"x\"}\n{\"type\":\"session\",\"cwd\":\"/repo\"}\n";
        assert!(session_cwd_matches(text, "/repo"));
        assert!(!session_cwd_matches(text, "/other"));
    }
}
