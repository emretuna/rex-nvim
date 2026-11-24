//! The file picker: builds candidates from the agent session log, git state,
//! and a pane scrape, then runs a borderless crossterm list in its floating
//! layer. On Enter it hands the chosen file to the detached `rex_nvim_open`
//! finisher; every exit path closes the picker's own layer.

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use serde_json::{json, Value};

use crate::candidates::{build_candidates, BuildInput, Candidate};
use crate::config;
use crate::extract;
use crate::fff::{FffHit, FffIndex};
use crate::gitscan;
use crate::paths;
use crate::rex;
use crate::sessions;

/// The plugin's accent, matching the annotation rail.
const AMBER: Color = Color::Rgb {
    r: 0xd7,
    g: 0xa6,
    b: 0x5f,
};

pub struct PickArgs {
    pub session: String,
    pub window: String,
    pub block: String,
    pub kind: String,
    pub origin: String,
    pub cwd: String,
}

pub struct FilterMatch {
    pub index: usize,
    /// Higher is better; 0 for empty-query rows.
    pub score: i32,
    /// Byte spans into the candidate's display path.
    pub highlights: Vec<(usize, usize)>,
}

/// Fuzzy-match `needle` against `hay` as a case-insensitive subsequence.
/// Per matched char: +12 consecutive, +10 word boundary, +3 filename tail,
/// and a -len/40 noise penalty.
pub fn fuzzy_match(hay: &str, needle: &str) -> Option<(i32, Vec<(usize, usize)>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay_chars: Vec<(usize, char)> = hay.char_indices().collect();
    let needle_lower: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    let name_start = hay.rfind('/').map(|i| i + 1).unwrap_or(0);

    let mut ni = 0usize;
    let mut score = 0i32;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut prev_matched_hi: Option<usize> = None;

    for hi in 0..hay_chars.len() {
        if ni >= needle_lower.len() {
            break;
        }
        let (byte, ch) = hay_chars[hi];
        if ch.to_lowercase().next() != Some(needle_lower[ni]) {
            continue;
        }
        let end = hay_chars.get(hi + 1).map_or(hay.len(), |&(b, _)| b);

        let consecutive = prev_matched_hi == Some(hi.wrapping_sub(1)) && hi > 0;
        score += if consecutive { 12 } else { 1 };
        let boundary = byte == 0
            || matches!(
                hay[..byte].chars().next_back(),
                Some('/' | '_' | '-' | '.' | ' ')
            );
        if boundary {
            score += 10;
        }
        if byte >= name_start {
            score += 3;
        }

        match spans.last_mut() {
            Some(last) if last.1 == byte => last.1 = end,
            _ => spans.push((byte, end)),
        }
        prev_matched_hi = Some(hi);
        ni += 1;
    }

    if ni == needle_lower.len() {
        score -= (hay_chars.len() as i32) / 40;
        Some((score, spans))
    } else {
        None
    }
}

/// Display form of an absolute path: relative to `cwd` inside it, `~` form
/// inside home, else absolute.
pub fn display_path(path: &str, cwd: &str, home: Option<&str>) -> String {
    if let Some(rest) = strip_dir_prefix(path, cwd) {
        rest.to_owned()
    } else if let Some(rest) = home.and_then(|h| strip_dir_prefix(path, h)) {
        format!("~/{rest}")
    } else {
        path.to_owned()
    }
}

fn strip_dir_prefix<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(dir)?;
    rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\'))
}

pub fn ellipsize_prefix(prefix: &str, budget: usize) -> String {
    let chars: Vec<char> = prefix.chars().collect();
    if chars.len() <= budget || budget <= 1 {
        return prefix.to_owned();
    }
    let keep = budget.saturating_sub(1).max(1);
    let head = keep / 2;
    let tail = keep - head;
    if chars.len() > head + tail {
        let head_s: String = chars[..head].iter().collect();
        let tail_s: String = chars[chars.len() - tail..].iter().collect();
        format!("{head_s}…{tail_s}")
    } else {
        prefix.to_owned()
    }
}

pub fn fmt_age(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    if secs < 60 {
        "now".to_owned()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub fn default_view(cands: &[Candidate]) -> Vec<FilterMatch> {
    cands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.session)
        .map(|(index, _)| FilterMatch {
            index,
            score: 0,
            highlights: Vec::new(),
        })
        .collect()
}

pub fn session_tier_matches(
    cands: &[Candidate],
    displays: &[String],
    query: &str,
) -> Vec<FilterMatch> {
    let mut matches: Vec<FilterMatch> = displays
        .iter()
        .enumerate()
        .filter(|(index, _)| cands[*index].session)
        .filter_map(|(index, display)| {
            fuzzy_match(display, query).map(|(score, highlights)| FilterMatch {
                index,
                score,
                highlights,
            })
        })
        .collect();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches
}

/// Append the fff repo-wide tier after the session tier, deduped by index and
/// dropping highlight spans that do not land on char boundaries.
pub fn merge_fff_tier(
    cands: &[Candidate],
    displays: &[String],
    session: Vec<FilterMatch>,
    fff_hits: Vec<FffHit>,
) -> Vec<FilterMatch> {
    use std::collections::HashSet;

    let by_path: HashMap<&str, usize> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| (c.path.as_str(), i))
        .collect();
    let taken: HashSet<usize> = session.iter().map(|m| m.index).collect();

    let mut out = session;
    for hit in fff_hits {
        let Some(&index) = by_path.get(hit.path.as_str()) else {
            continue;
        };
        if taken.contains(&index) {
            continue;
        }
        let display = &displays[index];
        let spans_ok = hit.highlights.iter().all(|&(s, e)| {
            s < e
                && e <= display.len()
                && display.is_char_boundary(s)
                && display.is_char_boundary(e)
        });
        out.push(FilterMatch {
            index,
            score: 0,
            highlights: if spans_ok { hit.highlights } else { Vec::new() },
        });
    }
    out
}

pub fn visible_count(total_matches: usize, query: &str, max_files: u32) -> usize {
    if query.is_empty() {
        total_matches.min(max_files as usize)
    } else {
        total_matches
    }
}

pub fn scroll_window(cursor: usize, total: usize, viewport_rows: usize) -> (usize, usize) {
    if total <= viewport_rows {
        return (0, total);
    }
    let max_first = total - viewport_rows;
    let first = if cursor < viewport_rows {
        0
    } else {
        cursor + 1 - viewport_rows
    };
    let first = first.min(max_first);
    (first, viewport_rows)
}

/// Entry point for `rex-nvim pick`.
pub fn pick(args: PickArgs) -> Result<i32> {
    let layer = own_layer(&args.session, &args.block);
    let _guard = LayerGuard {
        session: args.session.clone(),
        layer,
    };

    let candidates = build(&args);
    if candidates.is_empty() {
        let _ = rex::notify(
            &args.session,
            &args.origin,
            "No file paths found in agent output",
        );
        return Ok(0);
    }

    let config = config::load();
    let fff = FffIndex::open(&args.cwd, config.picker.frecency);
    if fff.is_none() {
        eprintln!("rex-nvim: fff index failed to initialize; fuzzy fallback tier is empty");
    }

    let home = paths::home_dir().map(|h| h.to_string_lossy().into_owned());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let chosen = run_overlay(
        &candidates,
        &args.cwd,
        home.as_deref(),
        config.picker.max_files,
        now,
        fff.as_ref(),
    )?;

    if let Some(index) = chosen {
        let candidate = &candidates[index];
        hand_off(&args, candidate)?;
    }
    Ok(0)
}

/// Build the candidate list from every read-only source.
fn build(args: &PickArgs) -> Vec<Candidate> {
    let cwd = std::path::Path::new(&args.cwd);
    let mined = sessions::mine(&args.kind, &args.cwd);

    let empty_scrape: Vec<extract::ScrapedPath> = Vec::new();

    let toplevel = gitscan::toplevel(cwd);
    let git_dirty = toplevel
        .as_ref()
        .and_then(|top| gitscan::dirty_paths(top).ok())
        .unwrap_or_default();
    let git_untracked = toplevel
        .as_ref()
        .and_then(|top| gitscan::untracked_paths(top).ok())
        .unwrap_or_default();
    let git_cached = toplevel
        .as_ref()
        .and_then(|top| gitscan::cached_paths(top).ok())
        .unwrap_or_default();
    let git_committed = mined
        .first_op_unix
        .and_then(|since| {
            toplevel
                .as_ref()
                .and_then(|top| gitscan::committed_since(top, since).ok())
        })
        .unwrap_or_default();
    let diff_stats = toplevel
        .as_ref()
        .and_then(|top| gitscan::diff_numstat_by_path(top).ok())
        .unwrap_or_default();
    let repo_files = toplevel
        .as_ref()
        .and_then(|top| gitscan::list_repo_files(top).ok())
        .unwrap_or_default();

    // Scrape only when mining produced nothing: the last `scan_lines` lines
    // of the agent pane.
    let scraped = if mined.touches.is_empty() {
        match rex::capture(&args.session, &args.block) {
            Ok(text) => {
                let config = config::load();
                let tail = last_lines(&text, config.picker.scan_lines as usize);
                extract::extract(&tail, cwd, &extract::exists)
            }
            Err(_) => empty_scrape,
        }
    } else {
        empty_scrape
    };

    let exists = |path: &str| std::path::Path::new(path).is_file();
    let mtime = |path: &str| sessions::file_mtime_unix(path);
    let in_worktree = |path: &str| -> bool {
        toplevel
            .as_ref()
            .map(|top| std::path::Path::new(path).starts_with(top))
            .unwrap_or(false)
    };

    build_candidates(BuildInput {
        mined_touches: &mined.touches,
        git_dirty: &git_dirty,
        git_untracked: &git_untracked,
        git_cached: &git_cached,
        git_committed_in_session: &git_committed,
        in_git_worktree: &in_worktree,
        git_mtime_unix: &mtime,
        diff_stats: &diff_stats,
        scraped_mentioned: &scraped,
        repo_files: &repo_files,
        exists: &exists,
    })
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// The floating layer containing the picker's own block, if it runs in one.
///
/// Rex sets `REX_BLOCK` inside every pane, so the picker's block is that value,
/// not the `--block` argument: `--block` names the *agent* pane candidates come
/// from, which lives in a tiled layer and therefore never matches here.
fn own_layer(session: &str, block: &str) -> Option<String> {
    let block = own_block_id(block, std::env::var("REX_BLOCK").ok().as_deref());
    let view = rex::api_call(session, "session.view", &json!({})).ok()?;
    layer_containing(&view, &block)
}

/// The block whose layer we own: `REX_BLOCK` when Rex set it, else `--block`.
fn own_block_id(explicit: &str, rex_block: Option<&str>) -> String {
    match rex_block {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => explicit.to_owned(),
    }
}

/// The non-tiled layer whose `blocks` include `block` (hidden layers included).
fn layer_containing(view: &Value, block: &str) -> Option<String> {
    for window in view.get("windows")?.as_array()? {
        let Some(layers) = window.get("layers").and_then(Value::as_array) else {
            continue;
        };
        for layer in layers {
            if layer.get("kind").and_then(|k| k.as_str()) == Some("tiled") {
                continue;
            }
            let Some(blocks) = layer.get("blocks").and_then(Value::as_array) else {
                continue;
            };
            let contains = blocks
                .iter()
                .any(|b| b.get("block_id").and_then(|id| id.as_str()) == Some(block));
            if contains {
                return layer
                    .get("layer_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned);
            }
        }
    }
    None
}

struct LayerGuard {
    session: String,
    layer: Option<String>,
}

impl Drop for LayerGuard {
    fn drop(&mut self) {
        if let Some(layer) = self.layer.take() {
            let _ = rex::api_call(
                &self.session,
                "session.close_layer",
                &json!({ "layer_id": layer }),
            );
        }
    }
}

/// Write the open-args file and spawn the detached finisher that runs
/// `rex do rex_nvim_open`.
fn hand_off(args: &PickArgs, candidate: &Candidate) -> Result<()> {
    let state = paths::state_dir();
    std::fs::create_dir_all(&state).ok();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file = std::env::temp_dir().join(format!(
        "rex-nvim-open-{}-{}.json",
        std::process::id(),
        nanos
    ));

    let mut payload = json!({
        "path": candidate.path,
        "window_id": args.window,
        "origin_block_id": args.origin,
    });
    if let Some(line) = candidate.line {
        payload["line"] = json!(line);
    }
    std::fs::write(&file, serde_json::to_vec(&payload)?)
        .with_context(|| format!("writing open args {}", file.display()))?;

    let log = state.join("finisher.log");
    let script = format!(
        "{} do -s {} rex_nvim_open --args @{} >> {} 2>&1; rm -f {}",
        shell_quote(&rex::bin()),
        shell_quote(&args.session),
        shell_quote(&file.to_string_lossy()),
        shell_quote(&log.to_string_lossy()),
        shell_quote(&file.to_string_lossy()),
    );
    spawn_detached(&script).context("spawning open finisher")?;
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn spawn_detached(script: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn()?;
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, Hide).context("entering alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn run_overlay(
    cands: &[Candidate],
    cwd: &str,
    home: Option<&str>,
    max_files: u32,
    now: u64,
    fff: Option<&FffIndex>,
) -> Result<Option<usize>> {
    if cands.is_empty() {
        return Ok(None);
    }
    let displays: Vec<String> = cands
        .iter()
        .map(|c| display_path(&c.path, cwd, home))
        .collect();
    let session_total = cands.iter().filter(|c| c.session).count();

    let _guard = TerminalGuard::enter()?;
    let mut query = String::new();
    let mut cursor = 0usize;

    loop {
        let matches = match (fff, query.is_empty()) {
            (_, true) => default_view(cands),
            (Some(index), false) => {
                let session = session_tier_matches(cands, &displays, &query);
                merge_fff_tier(cands, &displays, session, index.search(&query))
            }
            (None, false) => session_tier_matches(cands, &displays, &query),
        };
        let shown = visible_count(matches.len(), &query, max_files);
        let visible = &matches[..shown];
        if cursor >= visible.len() {
            cursor = visible.len().saturating_sub(1);
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 20));
        let viewport = viewport_rows(rows);
        render(
            cands,
            &displays,
            visible,
            cursor,
            &query,
            session_total,
            now,
            cols,
            rows,
        )?;

        let Event::Key(key) = event::read().context("reading terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = visible.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Enter => return Ok(visible.get(cursor).map(|m| m.index)),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(last),
            KeyCode::PageUp => cursor = cursor.saturating_sub(viewport),
            KeyCode::PageDown => cursor = (cursor + viewport).min(last),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = last,
            KeyCode::Backspace => {
                query.pop();
                cursor = 0;
            }
            KeyCode::Char('c') if ctrl => return Ok(None),
            KeyCode::Char('p') if ctrl => cursor = cursor.saturating_sub(1),
            KeyCode::Char('n') if ctrl => cursor = (cursor + 1).min(last),
            KeyCode::Char('a') if ctrl => cursor = 0,
            KeyCode::Char('e') if ctrl => cursor = last,
            KeyCode::Char('u') if ctrl => {
                query.clear();
                cursor = 0;
            }
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(c) => {
                query.push(c);
                cursor = 0;
            }
            _ => {}
        }
    }
}

const PAD_X: u16 = 2;
const CHROME_ROWS: u16 = 5;
const LIST_TOP: u16 = 3;

fn viewport_rows(rows: u16) -> usize {
    rows.saturating_sub(CHROME_ROWS).max(1) as usize
}

struct Seg {
    text: String,
    color: Option<Color>,
    dim: bool,
}

#[allow(clippy::too_many_arguments)]
fn render(
    cands: &[Candidate],
    displays: &[String],
    visible: &[FilterMatch],
    cursor: usize,
    query: &str,
    session_total: usize,
    now: u64,
    cols: u16,
    rows: u16,
) -> Result<()> {
    let mut out = io::stdout();
    queue!(out, Clear(ClearType::All)).context("clearing screen")?;

    let cols = cols.max(8) as usize;
    let width = cols.saturating_sub(2 * PAD_X as usize);

    let count = if query.is_empty() {
        format!("{session_total} files")
    } else {
        format!("{} matches", visible.len())
    };
    let prompt = format!("› {query}");
    queue!(
        out,
        MoveTo(PAD_X, 0),
        SetForegroundColor(AMBER),
        Print("› "),
        ResetColor,
        Print(query)
    )
    .context("query line")?;
    let count_col = cols.saturating_sub(count.chars().count() + PAD_X as usize);
    if count_col > prompt.chars().count() + PAD_X as usize {
        queue!(
            out,
            MoveTo(count_col as u16, 0),
            SetAttribute(Attribute::Dim),
            Print(&count),
            SetAttribute(Attribute::Reset)
        )
        .context("count label")?;
    }

    let viewport = viewport_rows(rows);
    let (first, count_rows) = scroll_window(cursor, visible.len(), viewport);
    for screen in 0..viewport {
        let row = LIST_TOP - 1 + screen as u16;
        if screen < count_rows {
            let m = &visible[first + screen];
            queue!(out, MoveTo(PAD_X, row)).context("row move")?;
            draw_candidate(
                &mut out,
                cands,
                displays,
                m,
                first + screen == cursor,
                now,
                width,
            )?;
        }
    }

    let hint = "↑↓ move  ⏎ open  ^u clear  esc close";
    queue!(
        out,
        MoveTo(PAD_X, rows.saturating_sub(2)),
        SetAttribute(Attribute::Dim),
        Print(hint),
        SetAttribute(Attribute::Reset)
    )
    .context("hint line")?;

    out.flush().context("flushing overlay")?;
    Ok(())
}

fn draw_candidate(
    out: &mut impl Write,
    cands: &[Candidate],
    displays: &[String],
    m: &FilterMatch,
    selected: bool,
    now: u64,
    width: usize,
) -> Result<()> {
    let cand = &cands[m.index];
    let display = &displays[m.index];

    if selected {
        queue!(
            out,
            SetForegroundColor(AMBER),
            Print('▌'),
            ResetColor,
            Print(' ')
        )
        .context("selection bar")?;
    } else {
        queue!(out, Print("  ")).context("row indent")?;
    }

    let region = width.saturating_sub(2);

    let mut segs: Vec<Seg> = Vec::new();
    if let Some((added, removed)) = cand.diff_stat {
        segs.push(Seg {
            text: format!("+{added}"),
            color: Some(Color::Green),
            dim: false,
        });
        segs.push(Seg {
            text: " ".to_owned(),
            color: None,
            dim: false,
        });
        segs.push(Seg {
            text: format!("-{removed}"),
            color: Some(Color::Red),
            dim: false,
        });
    } else if cand.newly_created {
        segs.push(Seg {
            text: "new".to_owned(),
            color: Some(AMBER),
            dim: false,
        });
    }
    if let Some(ts) = cand.touched_unix {
        if !segs.is_empty() {
            segs.push(Seg {
                text: "  ".to_owned(),
                color: None,
                dim: false,
            });
        }
        segs.push(Seg {
            text: fmt_age(now, ts),
            color: None,
            dim: true,
        });
    }
    let cluster_w: usize = segs.iter().map(|s| s.text.chars().count()).sum();

    let path_budget = region.saturating_sub(cluster_w + 1).max(1);
    let path_w = draw_path(out, display, &m.highlights, selected, path_budget)?;

    let gap = region.saturating_sub(path_w + cluster_w);
    queue!(out, Print(" ".repeat(gap))).context("cluster gap")?;
    for seg in &segs {
        if seg.dim {
            queue!(out, SetAttribute(Attribute::Dim)).context("seg dim")?;
        }
        if let Some(color) = seg.color {
            queue!(out, SetForegroundColor(color)).context("seg color")?;
        }
        queue!(out, Print(&seg.text)).context("seg text")?;
        queue!(out, ResetColor, SetAttribute(Attribute::Reset)).context("seg reset")?;
    }
    Ok(())
}

fn draw_path(
    out: &mut impl Write,
    display: &str,
    highlights: &[(usize, usize)],
    selected: bool,
    budget: usize,
) -> Result<usize> {
    let name_start = display.rfind('/').map(|i| i + 1).unwrap_or(0);
    let width = display.chars().count();

    if width <= budget {
        draw_spans(out, display, 0, highlights, name_start, selected)?;
        return Ok(width);
    }
    let prefix = &display[..name_start];
    let name = &display[name_start..];
    let name_w = name.chars().count();
    let prefix_budget = budget.saturating_sub(name_w).max(1);
    let ellipsized = ellipsize_prefix(prefix, prefix_budget);
    queue!(
        out,
        SetAttribute(Attribute::Dim),
        Print(&ellipsized),
        SetAttribute(Attribute::Reset)
    )
    .context("dim ellipsized prefix")?;
    draw_spans(out, name, name_start, highlights, name_start, selected)?;
    Ok(ellipsized.chars().count() + name_w)
}

fn draw_spans(
    out: &mut impl Write,
    text: &str,
    base: usize,
    highlights: &[(usize, usize)],
    name_start: usize,
    selected: bool,
) -> Result<()> {
    for (local, ch) in text.char_indices() {
        let global = base + local;
        let matched = highlights.iter().any(|&(s, e)| global >= s && global < e);
        let in_name = global >= name_start;

        if matched {
            queue!(
                out,
                SetForegroundColor(AMBER),
                SetAttribute(Attribute::Bold)
            )
            .context("hl on")?;
        } else if !in_name {
            queue!(out, SetAttribute(Attribute::Dim)).context("dim on")?;
        } else if selected {
            queue!(out, SetAttribute(Attribute::Bold)).context("bold on")?;
        }
        queue!(out, Print(ch)).context("path char")?;
        if matched || !in_name || selected {
            queue!(out, ResetColor, SetAttribute(Attribute::Reset)).context("style reset")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(path: &str) -> Candidate {
        Candidate {
            path: path.into(),
            line: None,
            is_edit: false,
            newly_created: false,
            session: true,
            touched_unix: None,
            diff_stat: None,
        }
    }

    fn repo_cand(path: &str) -> Candidate {
        Candidate {
            session: false,
            ..cand(path)
        }
    }

    fn displays_of(cands: &[Candidate], cwd: &str) -> Vec<String> {
        cands
            .iter()
            .map(|c| display_path(&c.path, cwd, None))
            .collect()
    }

    #[test]
    fn fuzzy_matches_subsequence_and_rejects_out_of_order() {
        assert!(fuzzy_match("src/main.rs", "mnrs").is_some());
        assert!(fuzzy_match("src/main.rs", "rsmain").is_none());
        assert!(fuzzy_match("src/Main.rs", "MAIN").is_some());
        let (score, spans) = fuzzy_match("anything", "").unwrap();
        assert_eq!(score, 0);
        assert!(spans.is_empty());
    }

    #[test]
    fn fuzzy_prefers_filename_and_consecutive() {
        let a = fuzzy_match("src/main.rs", "main").unwrap().0;
        let b = fuzzy_match("main/util.rs", "main").unwrap().0;
        assert!(a > b, "filename ({a}) should outrank dir ({b})");
        let consecutive = fuzzy_match("abc.rs", "abc").unwrap().0;
        let scattered = fuzzy_match("a_b_c.rs", "abc").unwrap().0;
        assert!(consecutive > scattered);
    }

    #[test]
    fn fuzzy_spans_are_char_boundaries_under_case_folding() {
        let path = "/\u{212A}main.rs";
        let (_, spans) = fuzzy_match(path, "kmain").expect("kelvin should match");
        for (s, e) in spans {
            assert!(path.is_char_boundary(s));
            assert!(path.is_char_boundary(e));
        }
    }

    #[test]
    fn empty_query_shows_only_session_candidates() {
        let cands = vec![cand("/r/a.rs"), repo_cand("/r/b.rs")];
        let m = default_view(&cands);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].index, 0);
    }

    #[test]
    fn session_tier_sorted_by_score_then_recency() {
        let cands = vec![cand("/r/domain/other.rs"), cand("/r/src/main.rs")];
        let displays = displays_of(&cands, "/r");
        assert_eq!(session_tier_matches(&cands, &displays, "main")[0].index, 1);

        let cands = vec![cand("/r/a/x.rs"), cand("/r/b/x.rs")];
        let displays = displays_of(&cands, "/r");
        assert_eq!(session_tier_matches(&cands, &displays, "x.rs")[0].index, 0);
    }

    #[test]
    fn display_path_relative_tilde_and_absolute() {
        assert_eq!(
            display_path("/repo/src/main.rs", "/repo", None),
            "src/main.rs"
        );
        assert_eq!(
            display_path("/home/u/.config/foo.toml", "/repo", Some("/home/u")),
            "~/.config/foo.toml"
        );
        assert_eq!(
            display_path("/var/log/x.log", "/repo", Some("/home/u")),
            "/var/log/x.log"
        );
    }

    #[test]
    fn ellipsize_keeps_head_and_tail() {
        let out = ellipsize_prefix("a/b/c/d/e/f/", 6);
        assert!(out.chars().count() <= 6);
        assert!(out.contains('…'));
        assert_eq!(ellipsize_prefix("src/", 20), "src/");
    }

    #[test]
    fn ages_bucket() {
        assert_eq!(fmt_age(100, 100), "now");
        assert_eq!(fmt_age(130, 100), "now");
        assert_eq!(fmt_age(220, 100), "2m");
        assert_eq!(fmt_age(100 + 3 * 3600, 100), "3h");
        assert_eq!(fmt_age(100 + 2 * 86_400, 100), "2d");
    }

    #[test]
    fn visibility_and_scrolling() {
        assert_eq!(visible_count(50, "", 20), 20);
        assert_eq!(visible_count(50, "main", 20), 50);
        assert_eq!(visible_count(5, "", 20), 5);
        assert_eq!(scroll_window(0, 5, 10), (0, 5));
        assert_eq!(scroll_window(19, 20, 5), (15, 5));
        let (first, count) = scroll_window(10, 20, 5);
        assert!(first <= 10 && 10 < first + count);
        assert_eq!(count, 5);
    }

    #[test]
    fn fff_tier_appends_dedupes_and_validates_spans() {
        let cands = vec![cand("/r/touched.rs"), repo_cand("/r/repo_only.rs")];
        let displays = displays_of(&cands, "/r");
        let session = session_tier_matches(&cands, &displays, "rs");
        assert_eq!(session.len(), 1);
        let merged = merge_fff_tier(
            &cands,
            &displays,
            session,
            vec![
                FffHit {
                    path: "/r/repo_only.rs".into(),
                    highlights: vec![(0, 2)],
                },
                FffHit {
                    path: "/r/touched.rs".into(),
                    highlights: vec![(0, 7)],
                },
                FffHit {
                    path: "/r/not_in_pool.rs".into(),
                    highlights: vec![],
                },
            ],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].index, 0);
        assert_eq!(merged[1].index, 1);

        let merged = merge_fff_tier(
            &cands,
            &displays,
            Vec::new(),
            vec![FffHit {
                path: "/r/touched.rs".into(),
                highlights: vec![(0, 99)],
            }],
        );
        assert_eq!(merged.len(), 1);
        assert!(merged[0].highlights.is_empty());
    }

    #[test]
    fn last_lines_takes_tail() {
        assert_eq!(last_lines("a\nb\nc", 2), "b\nc");
        assert_eq!(last_lines("a", 10), "a");
    }

    #[test]
    fn own_block_prefers_rex_block_over_picker_argument() {
        assert_eq!(
            own_block_id("block:agent", Some("block:picker")),
            "block:picker"
        );
        assert_eq!(own_block_id("block:agent", None), "block:agent");
        assert_eq!(own_block_id("block:agent", Some("")), "block:agent");
    }

    #[test]
    fn own_layer_skips_tiled_and_finds_floating_block() {
        let view = json!({
            "active_window_id": "window:1",
            "windows": [{
                "window_id": "window:1",
                "layers": [
                    {
                        "layer_id": "layer:tiled",
                        "kind": "tiled",
                        "blocks": [{ "block_id": "block:agent" }],
                    },
                    {
                        "layer_id": "layer:float",
                        "kind": "floating",
                        "blocks": [{ "block_id": "block:picker" }],
                    },
                    { "layer_id": "layer:empty" },
                ],
            }],
        });
        assert_eq!(
            layer_containing(&view, "block:picker").as_deref(),
            Some("layer:float")
        );
        // The agent pane lives in the tiled layer, which is never "ours".
        assert_eq!(layer_containing(&view, "block:agent"), None);
        assert_eq!(layer_containing(&view, "block:absent"), None);
    }
}
