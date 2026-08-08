//! Minimal git queries for the fork-source picker.
//!
//! The second step of the new-group picker (choosing where to fork a `drop`
//! worktree from) needs the same information drop's own interactive prompt
//! shows: the remote's default branch and the local + remote branch list. These
//! shell out to git and mirror the commands `drop` itself runs, so pwrde offers
//! exactly the choices drop would.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One branch offered as a fork source.
pub struct Branch {
    /// Short name: `main`, or `origin/feature-x` for a remote.
    pub name: String,
    pub is_remote: bool,
    pub is_current: bool,
    pub is_default: bool,
}

/// One existing worktree of a repo, offered as an "attach here" target.
pub struct Worktree {
    /// Absolute path to the worktree.
    pub path: PathBuf,
    /// Short branch name checked out there, or `None` when detached.
    pub branch: Option<String>,
    /// The repo's primary working tree (the first porcelain entry).
    pub is_main: bool,
}

/// A [`Command`] with a PATH augmented to include the usual tool locations.
///
/// pwrde may be launched from Finder, where the process PATH lacks the shell's
/// additions — so `git`, `drop`, and the `bun` runtime `drop` needs would not
/// resolve. Prepending the common bin dirs (plus `~/.bun/bin`) keeps those
/// invocations working regardless of how pwrde was started.
pub fn augmented_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".bun/bin"));
    }
    for p in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        paths.push(PathBuf::from(p));
    }
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
    cmd
}

/// A git command rooted at `repo`, inheriting the augmented PATH.
fn git(repo: &Path) -> Command {
    let mut cmd = augmented_command("git");
    cmd.current_dir(repo);
    cmd
}

/// The remote's default branch (`origin/main` vs `origin/master`), mirroring
/// drop's detection: `origin/HEAD`, then `origin/main`, then `origin/master`.
/// `None` when there is no remote (or git is unavailable).
pub fn default_remote_branch(repo: &Path) -> Option<String> {
    if let Ok(out) = git(repo).args(["symbolic-ref", "refs/remotes/origin/HEAD"]).output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let name = text.trim().strip_prefix("refs/remotes/").unwrap_or("");
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    for candidate in ["origin/main", "origin/master"] {
        let found = git(repo)
            .args(["rev-parse", "--verify", "--quiet", candidate])
            .output()
            .is_ok_and(|o| o.status.success());
        if found {
            return Some(candidate.to_string());
        }
    }
    None
}

/// List local + remote branches, tagging each with remote/current/default.
/// Mirrors drop's `git branch -a --format=%(HEAD)\t%(refname:short)`, deduping
/// the local/remote pair and skipping the HEAD symbolic entries. Empty when git
/// is unavailable or the directory is not a repo.
pub fn list_branches(repo: &Path) -> Vec<Branch> {
    let def = default_remote_branch(repo);
    let Ok(out) =
        git(repo).args(["branch", "-a", "--format=%(HEAD)\t%(refname:short)"]).output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen = HashSet::new();
    let mut branches = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let head = parts.next().unwrap_or("");
        let Some(name) = parts.next() else { continue };
        if name.is_empty() || name.contains("->") || name == "HEAD" {
            continue;
        }
        let is_remote = name.starts_with("origin/") || name.starts_with("remotes/");
        let clean = name.strip_prefix("remotes/").unwrap_or(name).to_string();
        let key = format!("{}:{}", if is_remote { "r" } else { "l" }, clean);
        if !seen.insert(key) {
            continue;
        }
        let is_default = def.as_deref() == Some(clean.as_str());
        branches.push(Branch {
            is_current: head == "*",
            is_default,
            is_remote,
            name: clean,
        });
    }
    branches
}

/// List the repo's worktrees from `git worktree list --porcelain`. The first
/// entry is always the primary working tree (flagged `is_main`); the rest are
/// linked worktrees (e.g. the ones `drop` creates under `.worktrees/`). Empty
/// when git is unavailable or the directory is not a repo.
pub fn list_worktrees(repo: &Path) -> Vec<Worktree> {
    let Ok(out) = git(repo).args(["worktree", "list", "--porcelain"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut worktrees: Vec<Worktree> = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    // Blocks are separated by blank lines and always begin with `worktree`;
    // flush the block in progress whenever a new one starts (or at the end).
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(prev) = path.take() {
                worktrees.push(Worktree { path: prev, branch: branch.take(), is_main: worktrees.is_empty() });
            }
            path = Some(PathBuf::from(p));
            branch = None;
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        }
    }
    if let Some(prev) = path.take() {
        worktrees.push(Worktree { path: prev, branch, is_main: worktrees.is_empty() });
    }
    worktrees
}

/// Sanitize a worktree basename into a filesystem-safe slug.
///
/// Keeps ASCII alphanumerics plus `.`, `_`, `-`; replaces every other character
/// with `-`. Returns `None` when the result is empty.
pub fn sanitize_worktree_slug(name: &str) -> Option<String> {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Decide the per-worktree storage scope from absolute git paths.
///
/// Returns `None` for the primary checkout (`git_dir == git_common_dir`) or when
/// the toplevel basename sanitizes to empty. Otherwise `Some(slug)` of the
/// toplevel basename.
pub fn scope_from_git_dirs(
    toplevel: &str,
    git_dir: &str,
    git_common_dir: &str,
) -> Option<String> {
    if Path::new(git_dir) == Path::new(git_common_dir) {
        return None;
    }
    let base = Path::new(toplevel)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    sanitize_worktree_slug(base)
}

/// The main checkout root for any directory inside a git repo.
///
/// Uses `git rev-parse --git-common-dir` from `dir` to find the common git dir
/// (the `.git` of the primary checkout regardless of worktree), then returns
/// its parent — the main checkout root — canonicalized.
///
/// Returns `None` when `dir` is not inside a git repo or the command fails.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let out = git(dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let common_dir_str = text.trim();
    let common_dir = if Path::new(common_dir_str).is_absolute() {
        PathBuf::from(common_dir_str)
    } else {
        // Relative path — resolve against dir
        dir.join(common_dir_str)
    };
    // The common git dir's parent is the main checkout root
    let root = common_dir.parent()?.to_path_buf();
    root.canonicalize().ok()
}

/// Per-worktree storage scope for the process cwd, if any.
///
/// Runs `git rev-parse --path-format=absolute --show-toplevel --git-dir
/// --git-common-dir` once (memoized). Returns `None` when cwd is not in a git
/// repo, when it is the primary checkout, or when the slug would be empty.
/// Linked worktrees yield `Some(slug)` derived from the toplevel basename.
pub fn worktree_scope() -> Option<String> {
    static SCOPE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SCOPE
        .get_or_init(|| {
            let out = augmented_command("git")
                .args([
                    "rev-parse",
                    "--path-format=absolute",
                    "--show-toplevel",
                    "--git-dir",
                    "--git-common-dir",
                ])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
            let toplevel = lines.next()?;
            let git_dir = lines.next()?;
            let git_common_dir = lines.next()?;
            scope_from_git_dirs(toplevel, git_dir, git_common_dir)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(
            sanitize_worktree_slug("feature.branch_1-ok"),
            Some("feature.branch_1-ok".into())
        );
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(
            sanitize_worktree_slug("feat/my branch"),
            Some("feat-my-branch".into())
        );
        assert_eq!(sanitize_worktree_slug("a@b#c"), Some("a-b-c".into()));
    }

    #[test]
    fn sanitize_empty_is_none() {
        assert_eq!(sanitize_worktree_slug(""), None);
    }

    #[test]
    fn scope_primary_checkout_is_none() {
        assert_eq!(
            scope_from_git_dirs(
                "/Users/me/proj",
                "/Users/me/proj/.git",
                "/Users/me/proj/.git",
            ),
            None
        );
    }

    #[test]
    fn scope_linked_worktree_uses_basename_slug() {
        assert_eq!(
            scope_from_git_dirs(
                "/Users/me/proj/.worktrees/7cb7f5a9",
                "/Users/me/proj/.git/worktrees/7cb7f5a9",
                "/Users/me/proj/.git",
            ),
            Some("7cb7f5a9".into())
        );
    }

    #[test]
    fn scope_linked_sanitizes_basename() {
        assert_eq!(
            scope_from_git_dirs(
                "/repo/.worktrees/feat my-branch",
                "/repo/.git/worktrees/feat-my-branch",
                "/repo/.git",
            ),
            Some("feat-my-branch".into())
        );
    }
}
