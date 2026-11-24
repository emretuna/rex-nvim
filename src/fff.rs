//! fff-search backend for the picker's fuzzy fallback tier.
//!
//! Frecency reuse is read-only: fff.nvim's DB (`stdpath('cache')/fff_nvim`)
//! is copied to a temp dir before opening, since `FrecencyTracker::open`
//! writes (garbage-collects) on open.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fff_search::file_picker::FilePicker;
use fff_search::frecency::FrecencyTracker;
use fff_search::{
    FFFMode, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser, SharedFilePicker,
    SharedFrecency,
};

/// One ranked hit: absolute path and byte-offset highlight spans into its
/// workspace-relative path.
pub struct FffHit {
    pub path: String,
    pub highlights: Vec<(usize, usize)>,
}

/// Hits pulled from fff per keystroke.
const PAGE_LIMIT: usize = 100;

/// Bound on the initial background scan.
const SCAN_WAIT: Duration = Duration::from_secs(3);

pub struct FffIndex {
    shared: SharedFilePicker,
    cwd: String,
    tmp: Option<PathBuf>,
}

impl FffIndex {
    /// Start indexing `cwd`. `use_frecency` gates the read-only fff.nvim DB
    /// reuse; `None` leaves the fuzzy tier empty.
    pub fn open(cwd: &str, use_frecency: bool) -> Option<Self> {
        let shared = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();

        let mut tmp = None;
        if use_frecency {
            if let Some((tracker, dir)) = open_frecency_copy() {
                if shared_frecency.init(tracker).is_ok() {
                    tmp = Some(dir);
                }
            }
        }

        FilePicker::new_with_shared_state(
            shared.clone(),
            shared_frecency,
            FilePickerOptions {
                base_path: cwd.into(),
                mode: FFFMode::Ai,
                watch: false,
                ..Default::default()
            },
        )
        .ok()?;

        Some(Self {
            shared,
            cwd: cwd.to_owned(),
            tmp,
        })
    }

    /// Ranked fff hits for `query`, in fff's score order; empty on failure.
    pub fn search(&self, query: &str) -> Vec<FffHit> {
        if !self.shared.wait_for_scan(SCAN_WAIT) {
            return Vec::new();
        }
        let Ok(guard) = self.shared.read() else {
            return Vec::new();
        };
        let Some(picker) = guard.as_ref() else {
            return Vec::new();
        };
        let parsed = QueryParser::default().parse(query);
        let results = picker.fuzzy_search(
            &parsed,
            None,
            FuzzySearchOptions {
                max_threads: 0,
                current_file: None,
                pagination: PaginationArgs {
                    offset: 0,
                    limit: PAGE_LIMIT,
                },
                ..Default::default()
            },
        );
        results
            .items
            .iter()
            .zip(results.match_byte_offsets.iter())
            .map(|(item, offsets)| {
                let rel = item.relative_path(picker);
                FffHit {
                    path: format!("{}/{rel}", self.cwd.trim_end_matches('/')),
                    highlights: offsets
                        .iter()
                        .map(|&(s, e)| (s as usize, e as usize))
                        .collect(),
                }
            })
            .collect()
    }
}

impl Drop for FffIndex {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_dir_all(tmp);
        }
    }
}

/// Copy fff.nvim's frecency DB to a temp dir and open the copy.
fn open_frecency_copy() -> Option<(FrecencyTracker, PathBuf)> {
    let src = nvim_cache_dir(|var| std::env::var_os(var))?.join("fff_nvim");
    if !src.is_dir() {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!("rex-nvim-fff-{}-{}", std::process::id(), nanos()));
    if copy_dir(&src, &tmp).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }
    match FrecencyTracker::open(&tmp) {
        Ok(tracker) => Some((tracker, tmp)),
        Err(_) => {
            let _ = std::fs::remove_dir_all(&tmp);
            None
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// nvim's `stdpath('cache')`: `$XDG_CACHE_HOME` else `~/.cache`, then
/// `$NVIM_APPNAME` else `nvim`.
fn nvim_cache_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let base = set("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::paths::home_dir().map(|home| home.join(".cache")))?;
    Some(base.join(set("NVIM_APPNAME").unwrap_or_else(|| "nvim".into())))
}

/// Shallow copy of the LMDB env dir (`data.mdb` + `lock.mdb`).
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_follows_nvim_rules() {
        let vars = |name: &str| -> Option<OsString> {
            match name {
                "XDG_CACHE_HOME" => Some(OsString::from("/tmp/xdg-cache")),
                "NVIM_APPNAME" => Some(OsString::from("rex-test")),
                _ => None,
            }
        };
        assert_eq!(
            nvim_cache_dir(vars),
            Some(PathBuf::from("/tmp/xdg-cache/rex-test"))
        );
    }
}
