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
