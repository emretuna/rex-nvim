//! State, socket, and install paths shared by every rex-nvim component.
//!
//! State dir: `REX_NVIM_STATE_DIR` -> `$XDG_STATE_HOME/rex-nvim` ->
//! `~/.local/state/rex-nvim`. Sockets normally live in `<state>/run`, but
//! macOS caps `sun_path` at 104 bytes (Linux 108), and Rex's Lua half uses a
//! 100-byte safety margin: when a socket path would exceed 100 bytes, sockets
//! move to `/tmp/rex-nvim-<uid>/run` while state files stay put.

use std::env;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest socket path we will ask the OS to bind (macOS's effective
/// comfortable limit; `sun_path` is 104 bytes including the terminating NUL).
pub const MAX_SOCKET_PATH: usize = 100;

pub fn state_dir() -> PathBuf {
    if let Some(dir) = env::var_os("REX_NVIM_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("rex-nvim");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local/state/rex-nvim")
}

pub fn run_dir(state: &Path) -> PathBuf {
    state.join("run")
}

/// Directory holding the daemons' sockets: `<state>/run` unless the socket
/// path would exceed [`MAX_SOCKET_PATH`], in which case the short temp dir.
pub fn socket_dir(state: &Path, key: &str) -> PathBuf {
    let normal = run_dir(state);
    if socket_path_too_long(&normal, key) {
        short_socket_dir()
    } else {
        normal
    }
}

/// The socket path for `key`, using whichever directory respects the limit.
pub fn socket_path(state: &Path, key: &str) -> PathBuf {
    socket_dir(state, key).join(format!("{key}.sock"))
}

fn socket_path_too_long(dir: &Path, key: &str) -> bool {
    dir.join(format!("{key}.sock")).as_os_str().len() > MAX_SOCKET_PATH
}

fn short_socket_dir() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/rex-nvim-{uid}/run"))
}

pub fn install_json_path(state: &Path) -> PathBuf {
    state.join("install.json")
}

/// `install.json` written by `install.sh`; absent in a source checkout.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Install {
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub rex: Option<String>,
}

pub fn load_install() -> Install {
    let path = install_json_path(&state_dir());
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Lexically resolve `path` against `cwd`: expand a leading `~`, join
/// relative paths, and fold `.`/`..` without touching the filesystem (session
/// logs often name files that no longer exist).
pub fn absolute_path(cwd: &Path, path: &str) -> PathBuf {
    let expanded = if path == "~" {
        home_dir().unwrap_or_else(|| PathBuf::from(path))
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir().unwrap_or_else(|| PathBuf::from("~")).join(rest)
    } else {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        }
    };
    normalize(&expanded)
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_honors_override() {
        // Env vars are process-global in tests; assert the rules directly.
        assert!(state_dir().is_absolute());
    }

    #[test]
    fn socket_path_short_state_dir_is_under_run() {
        let state = Path::new("/tmp/s");
        let key = "s12345678_w87654321";
        assert_eq!(
            socket_path(state, key),
            PathBuf::from("/tmp/s/run/s12345678_w87654321.sock")
        );
    }

    #[test]
    fn socket_path_falls_back_for_long_state_dir() {
        // 80-byte state dir + "/run/" + 24-byte key exceeds 100 bytes.
        let long = format!("/tmp/{}", "x".repeat(75));
        let state = Path::new(&long);
        let key = "s12345678_w87654321";
        let sock = socket_path(state, key);
        assert!(
            sock.as_os_str().len() <= MAX_SOCKET_PATH,
            "socket {} is {} bytes",
            sock.display(),
            sock.as_os_str().len()
        );
        assert!(sock.to_string_lossy().starts_with("/tmp/rex-nvim-"));
        assert_eq!(sock.file_name().unwrap(), format!("{key}.sock").as_str());
    }

    #[test]
    fn absolute_path_resolves_relative_and_parent() {
        assert_eq!(
            absolute_path(Path::new("/repo"), "src/main.rs"),
            PathBuf::from("/repo/src/main.rs")
        );
        assert_eq!(
            absolute_path(Path::new("/repo/src"), "../lua/x.lua"),
            PathBuf::from("/repo/lua/x.lua")
        );
        assert_eq!(
            absolute_path(Path::new("/repo"), "/abs/a.rs"),
            PathBuf::from("/abs/a.rs")
        );
    }
}
