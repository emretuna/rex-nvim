//! The merge/dedup/order pipeline turning session mining, git worktree state,
//! and pane scraping into the picker's flat, recency-ordered candidate list.
//! Pure: every I/O-shaped input is precomputed or injected.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{extract, gitscan, sessions};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub path: String,
    pub line: Option<u32>,
    /// A currently relevant edit: session-edited and non-git/dirty/committed
    /// in session, or a git-only dirty file.
    pub is_edit: bool,
    pub newly_created: bool,
    /// Touched during the session or currently dirty in git. Repo-pool-only
    /// entries are `false` and hidden from the empty-query view.
    pub session: bool,
    pub touched_unix: Option<u64>,
    /// `+added/-removed` for edited, dirty, non-new files.
    pub diff_stat: Option<(u32, u32)>,
}

pub struct BuildInput<'a> {
    pub mined_touches: &'a [sessions::MinedTouch],
    pub git_dirty: &'a HashSet<String>,
    pub git_untracked: &'a HashSet<String>,
    pub git_cached: &'a HashSet<String>,
    pub git_committed_in_session: &'a HashSet<String>,
    pub in_git_worktree: &'a dyn Fn(&str) -> bool,
    pub git_mtime_unix: &'a dyn Fn(&str) -> Option<u64>,
    pub diff_stats: &'a std::collections::HashMap<String, (u32, u32)>,
    pub scraped_mentioned: &'a [extract::ScrapedPath],
    pub repo_files: &'a [String],
    pub exists: &'a dyn Fn(&str) -> bool,
}

/// Build the single flat, deduped, recency-ordered candidate list. No cap
/// here: the picker caps only its empty-query view.
pub fn build_candidates(input: BuildInput) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut mined_paths: HashSet<&str> = HashSet::new();

    for touch in input.mined_touches {
        mined_paths.insert(touch.path.as_str());
        let in_repo = (input.in_git_worktree)(&touch.path);
        let dirty = input.git_dirty.contains(&touch.path);
        let committed = input.git_committed_in_session.contains(&touch.path);
        let untracked = input.git_untracked.contains(&touch.path);
        let cached = input.git_cached.contains(&touch.path);
        let is_edit = touch.was_edited && gitscan::should_keep_edited(in_repo, dirty, committed);
        out.push(Candidate {
            path: touch.path.clone(),
            line: None,
            is_edit,
            newly_created: untracked || (touch.newly_created && !cached),
            session: true,
            touched_unix: touch.last_touch_unix,
            diff_stat: None,
        });
    }

    // Git-only dirty paths the miner never saw (e.g. a shell `sed -i`).
    for path in input.git_dirty {
        if mined_paths.contains(path.as_str()) {
            continue;
        }
        let in_repo = (input.in_git_worktree)(path);
        out.push(Candidate {
            path: path.clone(),
            line: None,
            is_edit: gitscan::should_keep_edited(in_repo, true, false),
            newly_created: input.git_untracked.contains(path),
            session: true,
            touched_unix: (input.git_mtime_unix)(path),
            diff_stat: None,
        });
    }

    // Scrape fallback: only when mining produced nothing at all. A mention of
    // an already-listed path lends its line number instead of duplicating it.
    if input.mined_touches.is_empty() {
        for scraped in input.scraped_mentioned {
            if let Some(listed) = out.iter_mut().find(|c| c.path == scraped.path) {
                listed.line = listed.line.or(scraped.line);
                continue;
            }
            out.push(Candidate {
                path: scraped.path.clone(),
                line: scraped.line,
                is_edit: false,
                newly_created: false,
                session: true,
                touched_unix: None,
                diff_stat: None,
            });
        }
    }

    // Repo-wide pool: searchable once the user types, hidden by default.
    let mut present: HashSet<String> = out.iter().map(|c| c.path.clone()).collect();
    for path in input.repo_files {
        if present.insert(path.clone()) {
            out.push(Candidate {
                path: path.clone(),
                line: None,
                is_edit: false,
                newly_created: false,
                session: false,
                touched_unix: None,
                diff_stat: None,
            });
        }
    }

    out.retain(|c| (input.exists)(&c.path));

    for candidate in &mut out {
        if candidate.is_edit
            && !candidate.newly_created
            && input.git_dirty.contains(&candidate.path)
        {
            if let Some(&(added, removed)) = input.diff_stats.get(&candidate.path) {
                if added > 0 || removed > 0 {
                    candidate.diff_stat = Some((added, removed));
                }
            }
        }
    }

    // Descending touched_unix; `None` sorts last and ties keep source order.
    out.sort_by_key(|candidate| std::cmp::Reverse(candidate.touched_unix));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::MinedTouch;
    use std::collections::HashMap;
    use std::sync::LazyLock;

    fn always_true(_: &str) -> bool {
        true
    }
    fn always_false(_: &str) -> bool {
        false
    }
    fn no_mtime(_: &str) -> Option<u64> {
        None
    }
    static EMPTY_STATS: LazyLock<HashMap<String, (u32, u32)>> = LazyLock::new(HashMap::new);
    fn empty_stats() -> &'static HashMap<String, (u32, u32)> {
        &EMPTY_STATS
    }
    fn no_set() -> &'static HashSet<String> {
        static EMPTY: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);
        &EMPTY
    }

    fn touch(path: &str, was_edited: bool, ts: Option<u64>) -> MinedTouch {
        MinedTouch {
            path: path.into(),
            was_edited,
            newly_created: false,
            last_touch_unix: ts,
        }
    }
    fn base_input<'a>(
        mined: &'a [MinedTouch],
        dirty: &'a HashSet<String>,
        committed: &'a HashSet<String>,
        in_worktree: &'a dyn Fn(&str) -> bool,
    ) -> BuildInput<'a> {
        static EMPTY: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);
        let empty = &*EMPTY;
        BuildInput {
            mined_touches: mined,
            git_dirty: dirty,
            git_untracked: empty,
            git_cached: empty,
            git_committed_in_session: committed,
            in_git_worktree: in_worktree,
            git_mtime_unix: &no_mtime,
            diff_stats: empty_stats(),
            scraped_mentioned: &[],
            repo_files: &[],
            exists: &always_true,
        }
    }

    #[test]
    fn dirty_edited_touch_is_marked_as_edit() {
        let touches = [touch("/repo/a.rs", true, Some(5))];
        let dirty = HashSet::from(["/repo/a.rs".to_owned()]);
        let out = build_candidates(base_input(&touches, &dirty, no_set(), &always_true));
        assert!(out[0].is_edit);
    }

    #[test]
    fn committed_in_session_touch_is_marked_as_edit() {
        let touches = [touch("/repo/a.rs", true, Some(5))];
        let committed = HashSet::from(["/repo/a.rs".to_owned()]);
        let out = build_candidates(base_input(&touches, no_set(), &committed, &always_true));
        assert!(out[0].is_edit);
    }

    #[test]
    fn reverted_edit_is_demoted_but_kept() {
        let touches = [touch("/repo/a.rs", true, Some(5))];
        let out = build_candidates(base_input(&touches, no_set(), no_set(), &always_true));
        assert_eq!(out.len(), 1);
        assert!(!out[0].is_edit);
        assert!(out[0].diff_stat.is_none());
    }

    #[test]
    fn non_git_edit_stays_edit() {
        let touches = [touch("/tmp/a.rs", true, Some(5))];
        let out = build_candidates(base_input(&touches, no_set(), no_set(), &always_false));
        assert!(out[0].is_edit);
    }

    #[test]
    fn sorts_desc_by_touch_with_none_last_and_keeps_ties() {
        let touches = [
            touch("/r/a.rs", false, Some(10)),
            touch("/r/b.rs", false, None),
            touch("/r/c.rs", false, Some(30)),
            touch("/r/d.rs", false, None),
        ];
        let out = build_candidates(base_input(&touches, no_set(), no_set(), &always_true));
        assert_eq!(
            out.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["/r/c.rs", "/r/a.rs", "/r/b.rs", "/r/d.rs"]
        );
    }

    #[test]
    fn scrape_only_when_mining_empty() {
        let touches = [touch("/r/a.rs", false, Some(1))];
        let scraped = vec![extract::ScrapedPath {
            path: "/r/b.rs".into(),
            line: Some(3),
        }];
        let mut input = base_input(&touches, no_set(), no_set(), &always_true);
        input.scraped_mentioned = &scraped;
        let out = build_candidates(input);
        assert_eq!(out.len(), 1, "scrape is ignored when mining produced data");

        let mut input = base_input(&[], no_set(), no_set(), &always_true);
        input.scraped_mentioned = &scraped;
        let out = build_candidates(input);
        assert_eq!(out[0].path, "/r/b.rs");
        assert_eq!(out[0].line, Some(3));
    }

    #[test]
    fn scrape_lends_line_to_listed_path() {
        // Mining produced nothing; the git-dirty entry is already listed, so
        // the scrape mention lends it a line number instead of duplicating it.
        let dirty = HashSet::from(["/r/b.rs".to_owned()]);
        let scraped = vec![extract::ScrapedPath {
            path: "/r/b.rs".into(),
            line: Some(9),
        }];
        let mut input = base_input(&[], &dirty, no_set(), &always_true);
        input.scraped_mentioned = &scraped;
        let out = build_candidates(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line, Some(9));
    }

    #[test]
    fn repo_pool_appended_hidden_and_deduped() {
        let touches = [touch("/r/a.rs", false, Some(1))];
        let repo = vec!["/r/a.rs".to_owned(), "/r/z.rs".to_owned()];
        let mut input = base_input(&touches, no_set(), no_set(), &always_true);
        input.repo_files = &repo;
        let out = build_candidates(input);
        assert_eq!(out.len(), 2);
        let z = out.iter().find(|c| c.path == "/r/z.rs").unwrap();
        assert!(!z.session);
    }

    #[test]
    fn diff_stat_only_for_dirty_edit_non_new() {
        let touches = [touch("/r/a.rs", true, Some(5))];
        let dirty = HashSet::from(["/r/a.rs".to_owned()]);
        let stats = HashMap::from([("/r/a.rs".to_owned(), (3, 2))]);
        let mut input = base_input(&touches, &dirty, no_set(), &always_true);
        input.diff_stats = &stats;
        let out = build_candidates(input);
        assert_eq!(out[0].diff_stat, Some((3, 2)));

        let mut touch_new = touch("/r/a.rs", true, Some(5));
        touch_new.newly_created = true;
        let touches = [touch_new];
        let mut input = base_input(&touches, &dirty, no_set(), &always_true);
        input.diff_stats = &stats;
        let out = build_candidates(input);
        assert!(out[0].newly_created);
        assert!(out[0].diff_stat.is_none());
    }

    #[test]
    fn untracked_session_write_is_new_and_untracked_git_dirty_is_new() {
        let touches = [touch("/r/new.rs", true, Some(5))];
        let untracked = HashSet::from(["/r/new.rs".to_owned(), "/r/other.rs".to_owned()]);
        let dirty = HashSet::from(["/r/other.rs".to_owned()]);
        let empty = HashSet::new();
        let mut input = base_input(&touches, &dirty, &empty, &always_true);
        input.git_untracked = &untracked;
        let out = build_candidates(input);
        let new = out.iter().find(|c| c.path == "/r/new.rs").unwrap();
        assert!(new.newly_created);
        let other = out.iter().find(|c| c.path == "/r/other.rs").unwrap();
        assert!(other.newly_created);
    }

    #[test]
    fn nonexistent_are_filtered() {
        let touches = [touch("/r/gone.rs", false, Some(1))];
        let mut input = base_input(&touches, no_set(), no_set(), &always_false);
        input.exists = &always_false;
        assert!(build_candidates(input).is_empty());
    }
}
