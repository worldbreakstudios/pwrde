//! Minimal git queries for the fork-source picker.
//!
//! The second step of the new-group picker (choosing where to fork a `drop`
//! worktree from) needs the same information drop's own interactive prompt
//! shows: the remote's default branch and the local + remote branch list. These
//! shell out to git and mirror the commands `drop` itself runs, so pwrde offers
//! exactly the choices drop would.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};


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
        paths.push(home.join(".local/bin"));
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
    // Pin the locale: git translates its porcelain-adjacent output, and we
    // parse some of it by keyword. Under a German locale `--shortstat` reads
    // "3 Dateien geändert, 42 Zeilen hinzugefügt(+)", which
    // [`parse_shortstat`] would score as all zeros — a dirty tree silently
    // reporting "no code changes" on its sidebar card.
    cmd.env("LC_ALL", "C");
    cmd
}

// ── Timed shell-outs ──────────────────────────────────────────────────────

/// Why a shell-out produced no usable output.
///
/// A wedged `git` (a hung credential helper, a network filesystem) must never
/// pin the thread that called it, so every invocation runs under a deadline.
/// Callers care about *which* failure they hit — a timeout is worth retrying,
/// a missing binary never is — so the cases stay distinct instead of
/// collapsing into one opaque string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The binary is not on the augmented PATH (ENOENT): not installed.
    NotInstalled(String),
    /// Still running when its deadline expired; the child was killed.
    TimedOut(String),
    /// The OS refused the spawn, or waiting on the child failed.
    Spawn(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::NotInstalled(program) => write!(f, "{program} is not installed"),
            CommandError::TimedOut(program) => write!(f, "{program} timed out"),
            CommandError::Spawn(msg) => write!(f, "{msg}"),
        }
    }
}

/// Deadline for the cheapest local reads (h20's 2s reflog tier).
pub const TIMEOUT_QUICK: Duration = Duration::from_secs(2);
/// Deadline for ref plumbing: `symbolic-ref`, `rev-parse`, `worktree list`.
pub const TIMEOUT_REF: Duration = Duration::from_secs(3);
/// Deadline for a network-backed list read (`gh pr list`).
pub const TIMEOUT_LIST: Duration = Duration::from_secs(4);
/// Deadline for history walks: `merge-base`, `diff`, `show`.
pub const TIMEOUT_HISTORY: Duration = Duration::from_secs(5);

/// Run `cmd` to completion under `limit`, killing it when the deadline passes.
///
/// std has no timed `wait` and pwrde takes no dependency for one, so this polls
/// `try_wait` while two reader threads drain the pipes — a chatty child whose
/// pipe filled would otherwise block forever on a write we never read.
pub fn output_within(cmd: &mut Command, limit: Duration) -> Result<Output, CommandError> {
    /// How often to check on the child; short enough to feel instant, long
    /// enough not to spin a core while we wait.
    const POLL: Duration = Duration::from_millis(5);

    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CommandError::NotInstalled(program.clone()),
        _ => CommandError::Spawn(format!("{program}: {e}")),
    })?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = out_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = err_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = out_reader.join().unwrap_or_default();
                let stderr = err_reader.join().unwrap_or_default();
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) => {}
            Err(e) => return Err(CommandError::Spawn(format!("{program}: {e}"))),
        }
        if Instant::now() >= deadline {
            // Kill *and* reap: an unreaped child would linger as a zombie.
            let _ = child.kill();
            let _ = child.wait();
            return Err(CommandError::TimedOut(program));
        }
        std::thread::sleep(POLL);
    }
}

/// The remote's default branch (`origin/main` vs `origin/master`), mirroring
/// drop's detection: `origin/HEAD`, then `origin/main`, then `origin/master`.
/// `None` when there is no remote (or git is unavailable).
pub fn default_remote_branch(repo: &Path) -> Option<String> {
    if let Ok(out) =
        output_within(git(repo).args(["symbolic-ref", "refs/remotes/origin/HEAD"]), TIMEOUT_REF)
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let name = text.trim().strip_prefix("refs/remotes/").unwrap_or("");
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    for candidate in ["origin/main", "origin/master"] {
        let found = output_within(
            git(repo).args(["rev-parse", "--verify", "--quiet", candidate]),
            TIMEOUT_REF,
        )
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
    let Ok(out) = output_within(git(repo).args(["worktree", "list", "--porcelain"]), TIMEOUT_REF)
    else {
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
    let out = output_within(git(dir).args(["rev-parse", "--git-common-dir"]), TIMEOUT_REF).ok()?;
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

/// The current branch name (`git rev-parse --abbrev-ref HEAD`), or `None` when
/// detached / not a repo. Used to scope the PR tool to the checked-out branch.
pub fn current_branch(dir: &Path) -> Option<String> {
    let out = output_within(git(dir).args(["rev-parse", "--abbrev-ref", "HEAD"]), TIMEOUT_QUICK)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty() && name != "HEAD").then_some(name)
}

/// Derive a repo name from a `--git-common-dir` path.
///
/// The common dir is `<repo>/.git` for a normal checkout and a bare `<repo>.git`
/// for a bare one, so strip the final `.git` component either way. Pure so it
/// can be exercised without a repo on disk.
fn repo_name_from_common_dir(common: &Path) -> Option<String> {
    let name = common.file_name()?.to_string_lossy().to_string();
    if name == ".git" {
        // `<repo>/.git` — the repo is the directory holding it.
        return common
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty());
    }
    // A bare repo: `<repo>.git`, or an already-named git dir.
    Some(name.strip_suffix(".git").unwrap_or(&name).to_string()).filter(|n| !n.is_empty())
}

/// The repository's display name for `dir`, or `None` outside a repo.
///
/// Resolved from `git rev-parse --git-common-dir` rather than the basename of
/// `--show-toplevel`: inside a linked worktree the toplevel is the *worktree's*
/// directory (`.worktrees/04dc3ffa/pwrde`), which names the branch's scratch
/// checkout instead of the repo. The common dir always points back at the
/// primary `.git`, which pwrde needs because it runs from worktrees itself.
pub fn repo_display_name(dir: &Path) -> Option<String> {
    let out = output_within(git(dir).args(["rev-parse", "--git-common-dir"]), TIMEOUT_REF).ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // git answers with a path relative to the cwd (often plain `.git`).
    let common = Path::new(&raw);
    let absolute = if common.is_absolute() { common.to_path_buf() } else { dir.join(common) };
    repo_name_from_common_dir(&absolute)
}

/// How much uncommitted work a worktree is carrying: the counts git's
/// `--shortstat` line reports for staged + unstaged changes vs `HEAD`.
/// Untracked files are *not* counted (git excludes them from that diff).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirtyStats {
    /// Files touched, `0` when the tree is clean.
    pub files: u32,
    /// Added lines across those files.
    pub insertions: u32,
    /// Removed lines across those files.
    pub deletions: u32,
}

/// Parse one `git diff --shortstat` line, e.g.
/// `" 3 files changed, 42 insertions(+), 7 deletions(-)"`.
///
/// git omits the insertion or deletion clause entirely when it is zero, and
/// prints nothing at all for a clean tree — both cases fall out as zeros. Split
/// on commas and read the leading number of each clause rather than pulling in
/// a regex engine for three patterns.
fn parse_shortstat(text: &str) -> DirtyStats {
    let mut stats = DirtyStats::default();
    for clause in text.trim().split(',') {
        let clause = clause.trim();
        let digits: String = clause.chars().take_while(char::is_ascii_digit).collect();
        let Ok(n) = digits.parse::<u32>() else { continue };
        // Singular and plural both match: "file"/"files", "insertion"/"insertions".
        if clause.contains("file") {
            stats.files = n;
        } else if clause.contains("insertion") {
            stats.insertions = n;
        } else if clause.contains("deletion") {
            stats.deletions = n;
        }
    }
    stats
}

/// Committed changes on this branch: `git diff <merge-base with base> HEAD
/// --shortstat`.
///
/// The counterpart to [`dirty_stats`], and the number a sidebar card actually
/// wants: `dirty_stats` compares the working tree against `HEAD`, so a branch
/// with everything committed reports zero — which read as "no code changes" on
/// a card whose pull request had thousands.
///
/// Two `--shortstat` calls rather than the pull request's own
/// `additions`/`deletions`: `PrSummary` doesn't carry them, this works with no
/// PR at all, and it costs one cheap git call instead of a heavier `pr list`
/// payload.
///
/// `None` when the base cannot be resolved (no remote, unborn HEAD) or git
/// fails — the card then falls back to what it can show.
pub fn branch_stats(dir: &Path) -> Option<DirtyStats> {
    let base = default_remote_branch(dir)?;
    let out = output_within(git(dir).args(["merge-base", "HEAD", &base]), TIMEOUT_HISTORY).ok()?;
    if !out.status.success() {
        return None;
    }
    let fork = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if fork.is_empty() {
        return None;
    }
    let out =
        output_within(git(dir).args(["diff", &fork, "HEAD", "--shortstat"]), TIMEOUT_HISTORY)
            .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_shortstat(&String::from_utf8_lossy(&out.stdout)))
}

/// Uncommitted change counts for `dir` (`git diff HEAD --shortstat`).
///
/// Far cheaper than parsing a full `git diff`, which is why the per-group
/// status card uses it. `None` when git is unavailable, `dir` is not a repo, or
/// there is no `HEAD` yet (a fresh repo with no commits).
pub fn dirty_stats(dir: &Path) -> Option<DirtyStats> {
    let out =
        output_within(git(dir).args(["diff", "HEAD", "--shortstat"]), TIMEOUT_HISTORY).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_shortstat(&String::from_utf8_lossy(&out.stdout)))
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
    fn output_within_kills_a_wedged_child() {
        // `sleep 30` will never finish inside the deadline: the helper must
        // give up (and reap the child) rather than block the caller.
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let err = output_within(&mut cmd, Duration::from_millis(150)).unwrap_err();
        assert!(matches!(err, CommandError::TimedOut(_)), "got {err:?}");
    }

    #[test]
    fn output_within_reports_a_missing_binary_distinctly() {
        let mut cmd = Command::new("pwrde-definitely-not-a-real-binary");
        let err = output_within(&mut cmd, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, CommandError::NotInstalled(_)), "got {err:?}");
    }

    #[test]
    fn repo_name_comes_from_the_common_dir_not_the_worktree() {
        // A linked worktree's common dir still points at the primary checkout.
        assert_eq!(
            repo_name_from_common_dir(Path::new("/Users/me/src/pwrde/.git")),
            Some("pwrde".to_string())
        );
        // Bare repos name themselves `<repo>.git`.
        assert_eq!(
            repo_name_from_common_dir(Path::new("/srv/git/pwrde.git")),
            Some("pwrde".to_string())
        );
        assert_eq!(repo_name_from_common_dir(Path::new("/")), None);
    }

    #[test]
    fn shortstat_parses_full_line() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 42 insertions(+), 7 deletions(-)\n"),
            DirtyStats { files: 3, insertions: 42, deletions: 7 }
        );
    }

    #[test]
    fn shortstat_without_insertions() {
        assert_eq!(
            parse_shortstat(" 2 files changed, 9 deletions(-)\n"),
            DirtyStats { files: 2, insertions: 0, deletions: 9 }
        );
    }

    #[test]
    fn shortstat_without_deletions() {
        assert_eq!(
            parse_shortstat(" 2 files changed, 9 insertions(+)\n"),
            DirtyStats { files: 2, insertions: 9, deletions: 0 }
        );
    }

    #[test]
    fn shortstat_empty_is_zeros() {
        assert_eq!(parse_shortstat(""), DirtyStats::default());
        assert_eq!(parse_shortstat("\n"), DirtyStats::default());
    }

    #[test]
    fn shortstat_singular_line() {
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+), 1 deletion(-)\n"),
            DirtyStats { files: 1, insertions: 1, deletions: 1 }
        );
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
