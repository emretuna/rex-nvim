//! `rex-nvim daemons`: inventory every per-window nvim daemon (state file,
//! probe, process RAM/uptime, orphan status) and stop one, all, or orphans by
//! delegating to the `rex_nvim_stop` Rex action (which owns stop semantics).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::paths::{self, state_dir};
use crate::rex;

/// One round trip returns `<pid> <unsaved> <comments> <window_id>` without
/// loading `rex-nvim.comments` into `package.loaded`.
pub const PROBE_EXPR: &str = r#"join([getpid(), len(filter(getbufinfo({'bufmodified':1}),'v:val.listed')), luaeval("(function() local c = package.loaded['rex-nvim.comments'] if not c then return 0 end local ok, l = pcall(c.list) return ok and #l or 0 end)()"), $REX_WINDOW_ID], ' ')"#;

const PROBE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
pub struct StateFile {
    pub session_id: String,
    pub window_id: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub socket: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub nvim_bin: Option<String>,
}

#[derive(Debug, Clone)]
enum Probe {
    Answered {
        pid: u32,
        unsaved: u32,
        comments: u32,
        window_id: String,
    },
    Unresponsive,
    Gone,
    Error(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub key: String,
    pub session_id: String,
    pub window_id: String,
    pub session_label: Option<String>,
    pub window_label: Option<String>,
    pub pid: Option<u32>,
    pub rss_bytes: Option<u64>,
    pub uptime_secs: Option<u64>,
    pub state: String,
    pub unsaved_buffers: u32,
    pub pending_comments: u32,
    pub error: Option<String>,
}

impl Row {
    fn suffix(&self) -> String {
        let mut suffix = String::new();
        if self.unsaved_buffers > 0 {
            suffix.push_str(&format!(" · {} unsaved", self.unsaved_buffers));
        }
        if self.pending_comments > 0 {
            suffix.push_str(&format!(" · {} comment(s)", self.pending_comments));
        }
        suffix
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Alive,
    Orphaned,
    Unresponsive,
    Stale,
    Error,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Orphaned => "orphaned",
            Self::Unresponsive => "unresponsive",
            Self::Stale => "stale",
            Self::Error => "error",
        }
    }
}

struct ProcInfo {
    ppid: u32,
    rss_kb: u64,
    etime: String,
}

/// `daemons [--json] | daemons stop <key> | --all | --orphans [--force]`.
pub fn daemons_cmd(args: &[String]) -> Result<i32> {
    if args.is_empty() {
        let rows = collect();
        print_table(&rows);
        return Ok(0);
    }
    if args[0] == "--json" {
        if args.len() > 1 {
            bail!("usage: rex-nvim daemons [--json]");
        }
        let rows = collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    if args[0] == "stop" {
        return stop(&args[1..]);
    }
    bail!("usage: rex-nvim daemons [--json]\n       rex-nvim daemons stop <key> [--force]\n       rex-nvim daemons stop --all [--force]\n       rex-nvim daemons stop --orphans [--force]")
}

fn stop(args: &[String]) -> Result<i32> {
    let mut force = false;
    let mut all = false;
    let mut orphans = false;
    let mut key: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "--all" => all = true,
            "--orphans" => orphans = true,
            other if other.starts_with('-') => {
                bail!("unknown option {other}\nusage: rex-nvim daemons stop <key>|--all|--orphans [--force]")
            }
            other => {
                if key.replace(other.to_owned()).is_some() {
                    bail!("usage: rex-nvim daemons stop <key>|--all|--orphans [--force]");
                }
            }
        }
    }
    let targets = [all, orphans, key.is_some()]
        .iter()
        .filter(|selected| **selected)
        .count();
    if targets != 1 {
        bail!("usage: rex-nvim daemons stop <key>|--all|--orphans [--force]");
    }

    let rows = collect();
    if orphans && !server_reachable() {
        bail!("cannot resolve orphans: the Rex server is unreachable");
    }

    let selected: Vec<&Row> = if all {
        rows.iter().collect()
    } else if orphans {
        rows.iter().filter(|row| row.state == "orphaned").collect()
    } else {
        let key = key.as_deref().unwrap_or_default();
        match rows.iter().find(|row| row.key == key) {
            Some(row) => vec![row],
            None => bail!("no daemon named {key}"),
        }
    };

    let mut failed = false;
    // Orphans' windows are gone, so whatever they held went with the window:
    // force is always safe there and never needs the user's flag.
    let stop_force = delegated_force(orphans, force);
    for row in selected {
        if row.state == "error" {
            eprintln!(
                "not stopping {}: its probe failed, so nothing is known about it",
                row.key
            );
            failed = true;
            continue;
        }
        let args = json!({ "key": row.key, "force": stop_force }).to_string();
        let output = Command::new(rex::bin())
            .args([
                "do",
                "-s",
                &row.session_id,
                "rex_nvim_stop",
                "--args",
                &args,
            ])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                println!("stopped {}", row.key);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                };
                eprintln!("failed to stop {}: {detail}", row.key);
                failed = true;
            }
            Err(err) => {
                eprintln!("failed to stop {}: {err}", row.key);
                failed = true;
            }
        }
    }
    Ok(if failed { 1 } else { 0 })
}

/// `force` passed to the Lua stop action. `--orphans` always forces; an
/// explicit key or `--all` follows the user's `--force`.
fn delegated_force(orphans: bool, user_force: bool) -> bool {
    orphans || user_force
}

fn collect() -> Vec<Row> {
    let state = state_dir();
    let states = load_states(&state);
    if states.is_empty() {
        return Vec::new();
    }

    let deadline = Instant::now() + PROBE_DEADLINE;
    let handles: Vec<_> = states
        .into_iter()
        .map(|(key, state)| {
            thread::spawn(move || {
                let bin = state.nvim_bin.clone().unwrap_or_else(|| "nvim".to_owned());
                // Old or hand-made state files may omit the socket; derive it
                // with the same state/short-temp-directory rule the Lua half uses.
                let socket = if state.socket.is_empty() {
                    paths::socket_path(&state_dir(), &key)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    state.socket.clone()
                };
                let probe = probe(&socket, &bin, deadline);
                (key, state, probe)
            })
        })
        .collect();

    let sessions = session_labels();
    let server_reachable = sessions.is_some();
    let mut window_labels: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut windows_present: HashMap<String, HashSet<String>> = HashMap::new();

    let procs = process_table();
    let mut rows = Vec::new();
    for handle in handles {
        let Ok((key, state, probe)) = handle.join() else {
            continue;
        };
        let mut unsaved = 0;
        let mut comments = 0;
        let mut pid = state.pid;
        let mut error = None;
        let mut daemon_state;

        match probe {
            Probe::Answered {
                pid: answered_pid,
                unsaved: buffers,
                comments: pending,
                window_id,
            } => {
                pid = Some(answered_pid);
                unsaved = buffers;
                comments = pending;
                daemon_state = State::Alive;
                // The daemon reports the window it serves; trust the state
                // file's window for lookups but fall back to the answer.
                let _ = window_id;
            }
            Probe::Unresponsive => {
                daemon_state = State::Unresponsive;
            }
            Probe::Gone => {
                daemon_state = State::Stale;
            }
            Probe::Error(message) => {
                daemon_state = State::Error;
                error = Some(message);
            }
        }

        if server_reachable {
            let session_present = sessions
                .as_ref()
                .map(|labels| labels.contains_key(&state.session_id))
                .unwrap_or(true);
            if !session_present {
                if daemon_state == State::Alive {
                    daemon_state = State::Orphaned;
                }
            } else {
                let present = windows_present
                    .entry(state.session_id.clone())
                    .or_insert_with(|| {
                        session_windows(&state.session_id)
                            .unwrap_or_default()
                            .into_iter()
                            .collect()
                    });
                if !present.contains(&state.window_id) && daemon_state == State::Alive {
                    daemon_state = State::Orphaned;
                }
            }
        }

        let (rss_bytes, uptime_secs) = pid
            .and_then(|pid| {
                procs.as_ref().map(|table| {
                    (
                        descendants_rss(table, pid),
                        table.get(&pid).and_then(|p| etime_secs(&p.etime)),
                    )
                })
            })
            .unwrap_or((None, None));

        let window_label = window_labels
            .entry(state.session_id.clone())
            .or_insert_with(|| session_windows_labels(&state.session_id).unwrap_or_default())
            .get(&state.window_id)
            .cloned();

        rows.push(Row {
            key,
            session_id: state.session_id.clone(),
            window_id: state.window_id.clone(),
            session_label: sessions
                .as_ref()
                .and_then(|labels| labels.get(&state.session_id).cloned()),
            window_label,
            pid,
            rss_bytes,
            uptime_secs,
            state: daemon_state.as_str().to_owned(),
            unsaved_buffers: unsaved,
            pending_comments: comments,
            error,
        });
    }

    rows.sort_by(|a, b| {
        (a.session_id.as_str(), a.window_id.as_str())
            .cmp(&(b.session_id.as_str(), b.window_id.as_str()))
    });
    rows
}

fn load_states(state: &Path) -> Vec<(String, StateFile)> {
    let Ok(entries) = std::fs::read_dir(state) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(key) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut file) = serde_json::from_str::<StateFile>(&text) else {
            continue;
        };
        if file.key.is_empty() {
            file.key = key.clone();
        }
        out.push((key, file));
    }
    out
}

fn probe(socket: &str, nvim_bin: &str, deadline: Instant) -> Probe {
    let mut child = match Command::new(nvim_bin)
        .args(["--server", socket, "--remote-expr", PROBE_EXPR])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Probe::Error(format!("failed to run {nvim_bin}: {err}")),
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                return classify(status.success(), &stdout, &stderr);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Probe::Unresponsive;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Probe::Error(format!("waiting for {nvim_bin}: {err}")),
        }
    }
}

fn classify(success: bool, stdout: &[u8], stderr: &[u8]) -> Probe {
    let out = String::from_utf8_lossy(stdout);
    if success {
        let parts: Vec<&str> = out.split_whitespace().collect();
        if parts.len() < 3 {
            return Probe::Error(format!("unexpected probe reply: {}", out.trim()));
        }
        let (Ok(pid), Ok(unsaved), Ok(comments)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) else {
            return Probe::Error(format!("unexpected probe reply: {}", out.trim()));
        };
        return Probe::Answered {
            pid,
            unsaved,
            comments,
            window_id: parts.get(3).copied().unwrap_or_default().to_owned(),
        };
    }
    let err = String::from_utf8_lossy(stderr);
    let lower = err.to_lowercase();
    if lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("connection")
        || lower.contains("no such file")
        || lower.contains("no process")
        || lower.contains("socket")
    {
        Probe::Gone
    } else if err.trim().is_empty() {
        Probe::Error("nvim exited without an error message".to_owned())
    } else {
        Probe::Error(err.trim().to_owned())
    }
}

fn server_reachable() -> bool {
    session_labels().is_some()
}

fn session_labels() -> Option<HashMap<String, String>> {
    let value = rex::api_call_unattached("session.list", &json!({})).ok()?;
    let sessions = value.get("sessions")?.as_array()?;
    Some(
        sessions
            .iter()
            .filter_map(|session| {
                let id = session.get("session_id")?.as_str()?.to_owned();
                let label = session
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .to_owned();
                Some((id, label))
            })
            .collect(),
    )
}

fn session_windows(session_id: &str) -> Option<Vec<String>> {
    let value = rex::api_call(session_id, "session.view", &json!({})).ok()?;
    Some(
        value
            .get("windows")?
            .as_array()?
            .iter()
            .filter_map(|window| window.get("window_id")?.as_str().map(str::to_owned))
            .collect(),
    )
}

fn session_windows_labels(session_id: &str) -> Option<HashMap<String, String>> {
    let value = rex::api_call(session_id, "session.view", &json!({})).ok()?;
    Some(
        value
            .get("windows")?
            .as_array()?
            .iter()
            .filter_map(|window| {
                let id = window.get("window_id")?.as_str()?.to_owned();
                let label = window
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .to_owned();
                Some((id, label))
            })
            .collect(),
    )
}

fn process_table() -> Option<HashMap<u32, ProcInfo>> {
    let output = Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=,ppid=,rss=,etime=,args="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut table = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Ok(pid), Ok(ppid), Ok(rss)) = (
            fields.next().unwrap_or("").parse::<u32>(),
            fields.next().unwrap_or("").parse::<u32>(),
            fields.next().unwrap_or("").parse::<u64>(),
        ) else {
            continue;
        };
        let Some(etime) = fields.next() else {
            continue;
        };
        table.insert(
            pid,
            ProcInfo {
                ppid,
                rss_kb: rss,
                etime: etime.to_owned(),
            },
        );
    }
    Some(table)
}

fn descendants_rss(table: &HashMap<u32, ProcInfo>, root: u32) -> Option<u64> {
    if !table.contains_key(&root) {
        return None;
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, info) in table {
        children.entry(info.ppid).or_default().push(pid);
    }
    let mut total_kb = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(info) = table.get(&pid) {
            total_kb += info.rss_kb;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    if seen.is_empty() {
        None
    } else {
        Some(total_kb * 1024)
    }
}

fn etime_secs(etime: &str) -> Option<u64> {
    let (days, rest) = match etime.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(days * 86_400 + hours * 3600 + minutes * 60 + seconds)
}

fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * MB;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else {
        format!("{:.1} MB", value / MB)
    }
}

fn human_uptime(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn print_table(rows: &[Row]) {
    if rows.is_empty() {
        println!("no daemons");
        return;
    }
    let targets: Vec<(String, String, String, String)> = rows
        .iter()
        .map(|row| {
            let session = row
                .session_label
                .clone()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| row.session_id.clone());
            let window = row
                .window_label
                .clone()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| row.window_id.clone());
            (
                format!("{session}/{window}  {}", row.key),
                row.rss_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "—".to_owned()),
                row.uptime_secs
                    .map(human_uptime)
                    .unwrap_or_else(|| "—".to_owned()),
                format!("{}{}", row.state, row.suffix()),
            )
        })
        .collect();

    let header = ("SESSION/WINDOW", "RAM", "UP", "STATE");
    let w0 = targets
        .iter()
        .map(|t| t.0.chars().count())
        .chain(std::iter::once(header.0.len()))
        .max()
        .unwrap_or(0);
    let w1 = targets
        .iter()
        .map(|t| t.1.chars().count())
        .chain(std::iter::once(header.1.len()))
        .max()
        .unwrap_or(0);
    let w2 = targets
        .iter()
        .map(|t| t.2.chars().count())
        .chain(std::iter::once(header.2.len()))
        .max()
        .unwrap_or(0);

    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {}",
        header.0, header.1, header.2, header.3
    );
    for (session, ram, up, state) in &targets {
        println!("{session:<w0$}  {ram:<w1$}  {up:<w2$}  {state}");
    }

    let total: u64 = rows.iter().filter_map(|row| row.rss_bytes).sum();
    println!();
    println!("{} daemons · {} total", rows.len(), human_bytes(total));
    println!("stop one: rex-nvim daemons stop <key>   (--force discards unsaved work)");
    println!("stop all: rex-nvim daemons stop --all");
    println!("orphans: rex-nvim daemons stop --orphans   (safe: their windows are gone)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parses_all_shapes() {
        assert_eq!(etime_secs("05:07"), Some(307));
        assert_eq!(etime_secs("01:05:07"), Some(3907));
        assert_eq!(etime_secs("2-01:05:07"), Some(2 * 86_400 + 3907));
        assert_eq!(etime_secs("garbage"), None);
    }

    #[test]
    fn descendant_rss_sums_tree() {
        let mut table = HashMap::new();
        table.insert(
            1,
            ProcInfo {
                ppid: 0,
                rss_kb: 100,
                etime: "00:01".into(),
            },
        );
        table.insert(
            2,
            ProcInfo {
                ppid: 1,
                rss_kb: 50,
                etime: "00:01".into(),
            },
        );
        table.insert(
            3,
            ProcInfo {
                ppid: 2,
                rss_kb: 25,
                etime: "00:01".into(),
            },
        );
        table.insert(
            4,
            ProcInfo {
                ppid: 99,
                rss_kb: 999,
                etime: "00:01".into(),
            },
        );
        assert_eq!(descendants_rss(&table, 1), Some(175 * 1024));
    }

    #[test]
    fn orphans_delegate_force_and_explicit_targets_keep_the_flag() {
        // `--orphans` never takes unsaved work with it: its window is gone.
        assert!(delegated_force(true, false));
        assert!(delegated_force(true, true));
        // A key or --all follows the user's flag.
        assert!(!delegated_force(false, false));
        assert!(delegated_force(false, true));
    }

    #[test]
    fn human_units() {
        assert_eq!(human_bytes(512 * 1024), "0.5 MB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(human_uptime(59), "59s");
        assert_eq!(human_uptime(3900), "1h5m");
        assert_eq!(human_uptime(90_000), "1d1h");
    }

    #[test]
    fn classify_connection_refused_is_stale() {
        assert!(matches!(
            classify(
                false,
                b"",
                b"E247: no registered server named x Connection refused"
            ),
            Probe::Gone
        ));
        assert!(matches!(
            classify(false, b"", b"E5108: Error executing lua ..."),
            Probe::Error(_)
        ));
    }

    #[test]
    fn classify_answers_parse() {
        match classify(true, b"1234 2 1 window:abc\n", b"") {
            Probe::Answered {
                pid,
                unsaved,
                comments,
                window_id,
            } => {
                assert_eq!((pid, unsaved, comments), (1234, 2, 1));
                assert_eq!(window_id, "window:abc");
            }
            other => panic!("expected answer, got {other:?}"),
        }
    }
}
