//! Read-only git helpers for the picker's candidate pipeline. Every function
//! only shells out to `git status`/`git log`/`git rev-parse`/`git ls-files`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

fn absolute_git_path(toplevel: &Path, path: &str) -> String {
    toplevel.join(path).to_string_lossy().into_owned()
}

/// `git -C <cwd> rev-parse --show-toplevel`; `None` outside a repo.
pub fn toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Absolute paths with uncommitted changes.
pub fn dirty_paths(toplevel: &Path) -> Result<HashSet<String>> {
    // `--no-optional-locks`: a status that refreshes `.git/index` wakes folder
    // watches for no benefit.
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(toplevel)
        .arg("status")
        .arg("--porcelain")
        .output()
        .context("failed to run git status --porcelain")?;
    if !output.status.success() {
        anyhow::bail!("git status --porcelain failed");
    }
    Ok(parse_status_porcelain(
        &String::from_utf8_lossy(&output.stdout),
        toplevel,
    ))
}

/// Absolute paths git reports as untracked (`??`).
pub fn untracked_paths(toplevel: &Path) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(toplevel)
        .arg("status")
        .arg("--porcelain")
        .output()
        .context("failed to run git status --porcelain")?;
    if !output.status.success() {
        anyhow::bail!("git status --porcelain failed");
    }
    Ok(parse_untracked(
        &String::from_utf8_lossy(&output.stdout),
        toplevel,
    ))
}

pub fn parse_untracked(output: &str, toplevel: &Path) -> HashSet<String> {
    output
        .lines()
        .filter(|line| line.starts_with("?? "))
        .map(|line| absolute_git_path(toplevel, &line[3..]))
        .collect()
}

/// Absolute paths of every tracked file (`git ls-files --cached`).
pub fn cached_paths(toplevel: &Path) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(toplevel)
        .arg("ls-files")
        .arg("--cached")
        .arg("-z")
        .output()
        .context("failed to run git ls-files --cached")?;
    if !output.status.success() {
        anyhow::bail!("git ls-files --cached failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| absolute_git_path(toplevel, entry))
        .collect())
}

/// Every tracked-or-untracked (not ignored) worktree file, absolute.
pub fn list_repo_files(toplevel: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(toplevel)
        .arg("ls-files")
        .arg("--cached")
        .arg("--others")
        .arg("--exclude-standard")
        .arg("-z")
        .output()
        .context("failed to run git ls-files")?;
    if !output.status.success() {
        anyhow::bail!("git ls-files failed");
    }
    Ok(parse_ls_files_z(
        &String::from_utf8_lossy(&output.stdout),
        toplevel,
    ))
}

pub fn parse_ls_files_z(output: &str, toplevel: &Path) -> Vec<String> {
    output
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| absolute_git_path(toplevel, entry))
        .collect()
}

/// Parse `git status --porcelain` into absolute paths; renames keep NEW.
pub fn parse_status_porcelain(output: &str, toplevel: &Path) -> HashSet<String> {
    output
        .lines()
        .filter(|line| line.len() > 3)
        .map(|line| {
            let rest = &line[3..];
            let path = rest.rsplit(" -> ").next().unwrap_or(rest);
            absolute_git_path(toplevel, path)
        })
        .collect()
}

/// Absolute paths touched by any commit since `since_unix`.
pub fn committed_since(toplevel: &Path, since_unix: u64) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(toplevel)
        .arg("log")
        .arg("--name-only")
        .arg("--pretty=format:commit")
        .arg(format!("--since=@{since_unix}"))
        .output()
        .context("failed to run git log --name-only")?;
    if !output.status.success() {
        anyhow::bail!("git log --name-only failed");
    }
    Ok(parse_log_name_only(
        &String::from_utf8_lossy(&output.stdout),
        toplevel,
    ))
}

pub fn parse_log_name_only(output: &str, toplevel: &Path) -> HashSet<String> {
    output
        .lines()
        .filter(|line| {
            !line.is_empty()
                && *line != "commit"
                && !line.starts_with("commit ")
                && !line.starts_with("Author:")
                && !line.starts_with("Date:")
                && !line.starts_with("    ")
        })
        .map(|line| absolute_git_path(toplevel, line))
        .collect()
}

/// Net-change demotion: a session-edited path stays an edit when it is not in
/// a git worktree, is dirty, or was committed during the session.
pub fn should_keep_edited(in_git_worktree: bool, dirty: bool, committed_in_session: bool) -> bool {
    if !in_git_worktree {
        return true;
    }
    dirty || committed_in_session
}

/// One `git diff HEAD --numstat` for the whole worktree, keyed by absolute
/// path. A repo with no `HEAD` yields an empty map.
pub fn diff_numstat_by_path(toplevel: &Path) -> Result<HashMap<String, (u32, u32)>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(toplevel)
        .arg("diff")
        .arg("HEAD")
        .arg("--numstat")
        .output()
        .context("failed to run git diff HEAD --numstat")?;
    if !output.status.success() {
        return Ok(HashMap::new());
    }
    Ok(parse_diff_numstat_map(
        &String::from_utf8_lossy(&output.stdout),
        toplevel,
    ))
}

pub fn parse_diff_numstat_map(output: &str, toplevel: &Path) -> HashMap<String, (u32, u32)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added_str = parts.next()?;
            let removed_str = parts.next()?;
            let path = parts.next()?;
            let added: u32 = added_str.parse().unwrap_or(0);
            let removed: u32 = removed_str.parse().unwrap_or(0);
            Some((absolute_git_path(toplevel, path), (added, removed)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modified_added_untracked_and_renamed() {
        let output =
            " M src/main.rs\nA  src/new.rs\n?? untracked.txt\nR  old.rs -> src/renamed.rs\n";
        assert_eq!(
            parse_status_porcelain(output, Path::new("/repo")),
            HashSet::from([
                "/repo/src/main.rs".to_owned(),
                "/repo/src/new.rs".to_owned(),
                "/repo/untracked.txt".to_owned(),
                "/repo/src/renamed.rs".to_owned(),
            ])
        );
    }

    #[test]
    fn parses_untracked_only() {
        let output = " M src/a.rs\n?? new.txt\n?? dir/b.rs\n";
        assert_eq!(
            parse_untracked(output, Path::new("/repo")),
            HashSet::from(["/repo/new.txt".to_owned(), "/repo/dir/b.rs".to_owned()])
        );
    }

    #[test]
    fn parses_ls_files_z_in_order() {
        let output = "src/main.rs\0lua/foo.lua\0a file with spaces.txt\0";
        assert_eq!(
            parse_ls_files_z(output, Path::new("/repo")),
            vec![
                "/repo/src/main.rs",
                "/repo/lua/foo.lua",
                "/repo/a file with spaces.txt",
            ]
        );
    }

    #[test]
    fn parses_name_only_log() {
        let output = "commit abc\nAuthor: a\nDate:   d\n\n    fix\nsrc/a.rs\nsrc/b.rs\n\ncommit def\nsrc/c.rs\n";
        assert_eq!(
            parse_log_name_only(output, Path::new("/repo")),
            HashSet::from([
                "/repo/src/a.rs".to_owned(),
                "/repo/src/b.rs".to_owned(),
                "/repo/src/c.rs".to_owned(),
            ])
        );
    }

    #[test]
    fn demotion_rules() {
        assert!(should_keep_edited(true, true, false));
        assert!(should_keep_edited(true, false, true));
        assert!(!should_keep_edited(true, false, false));
        assert!(should_keep_edited(false, false, false));
    }

    #[test]
    fn numstat_handles_binary_and_zero_counts() {
        let output = "3\t1\tsrc/a.rs\n0\t5\tsrc/b.rs\n-\t-\tassets/image.png\n";
        let map = parse_diff_numstat_map(output, Path::new("/repo"));
        assert_eq!(map["/repo/src/a.rs"], (3, 1));
        assert_eq!(map["/repo/src/b.rs"], (0, 5));
        assert_eq!(map["/repo/assets/image.png"], (0, 0));
        assert!(parse_diff_numstat_map("", Path::new("/repo")).is_empty());
    }
}
