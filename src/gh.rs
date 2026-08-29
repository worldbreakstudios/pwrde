//! GitHub pull-request data + actions, shelled out through a configurable CLI.
//!
//! h20 proved the model: talk to GitHub through `lfg` (a local-first `gh`
//! cache) rather than `gh` directly, so cached data paints instantly and a
//! background refresh streams fresh data over SSE. pwrde keeps that model but
//! makes the CLI swappable — the `git.cli` setting picks the binary (default
//! `lfg`, or plain `gh`, or any drop-in with the same `pr view/list/diff
//! --json` contract), and `git.async` toggles the `-A/--force-async` fast path
//! (only meaningful for `lfg`; plain `gh` blocks synchronously).
//!
//! Reads return normalized, gpui-free structs so the UI layer and the tests
//! don't care which CLI produced them. Writes (approve/comment/merge/ready) go
//! through the same CLI's `pr` subcommands.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::diff::DiffFile;

/// The CLI binary used for PR data. `git.cli`, defaulting to `lfg`; an empty
/// value also falls back to `lfg`.
pub fn cli() -> String {
    crate::settings::get_str("git.cli")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "lfg".into())
}

/// Whether the async/streaming fast path is enabled (`git.async`, default on).
pub fn async_enabled() -> bool {
    crate::settings::get_bool("git.async", true)
}

/// Whether to actually use `lfg`'s `-A` + SSE stream: only when async is on and
/// the configured CLI is `lfg` (plain `gh` has no `-A` and no daemon).
pub fn use_async() -> bool {
    async_enabled() && cli() == "lfg"
}

/// Build the configured CLI command in `dir`. On the async path `-A` is
/// prepended (before the subcommand, where lfg's global flags live) unless
/// `sync` forces a blocking cache-through read — the fallback used when the
/// async fast path returned only a cold placeholder.
fn cli_cmd_sync(dir: &Path, sync: bool) -> Command {
    let mut cmd = crate::git::augmented_command(&cli());
    cmd.current_dir(dir);
    if use_async() && !sync {
        cmd.arg("-A");
    }
    cmd
}

/// Run the CLI and return stdout as a string, or an error message. A non-zero
/// exit surfaces stderr so failures (auth, no remote) are legible.
fn run(mut cmd: Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{}: {e}", cli()))?;
    finish(out)
}

/// Run the CLI under `limit`, killing it when the deadline passes.
///
/// Used by the reads a background poller drives: a wedged daemon or a stalled
/// network call must never pin that thread forever. The three failure modes
/// stay distinguishable in the message — `is not installed` (ENOENT), `timed
/// out` (deadline), and the CLI's own stderr for everything else (auth, no
/// remote), which `is_auth_failure` can recognize.
fn run_within(mut cmd: Command, limit: web_time::Duration) -> Result<String, String> {
    let out = crate::git::output_within(&mut cmd, limit).map_err(|e| e.to_string())?;
    finish(out)
}

/// Turn a finished invocation into stdout, or stderr as the error message.
fn finish(out: std::process::Output) -> Result<String, String> {
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err.trim();
        return Err(if msg.is_empty() {
            format!("{} exited with {}", cli(), out.status)
        } else {
            msg.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a CLI failure message is an authentication problem rather than a
/// timeout, a missing binary, or a missing remote.
///
/// The distinction is what the UI needs: an auth failure is the one the user
/// can fix (`gh auth login`), so it deserves different wording than a poll
/// that simply timed out and will be retried.
pub fn is_auth_failure(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    ["auth login", "authentication", "not logged in", "bad credentials", "unauthorized"]
        .iter()
        .any(|needle| lower.contains(needle))
}

// ── Normalized model ──────────────────────────────────────────────────────

/// One row in the PR picker list.
#[derive(Clone, Debug, PartialEq)]
pub struct PrSummary {
    pub number: u32,
    pub title: String,
    pub state: String,
    pub is_draft: bool,
    pub head: String,
    pub author: String,
    /// APPROVED / CHANGES_REQUESTED / REVIEW_REQUIRED — `None` when GitHub
    /// reports no verdict yet (the CLI hands back `""` for un-reviewed PRs).
    pub review_decision: Option<String>,
    /// MERGEABLE / CONFLICTING / UNKNOWN, straight from the CLI.
    pub mergeable: Option<String>,
    /// The check rollup, so a list row can show CI without a detail fetch.
    pub checks: Vec<Check>,
}

/// A PR's detail, everything the panel header + conversation needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrDetail {
    pub number: u32,
    pub title: String,
    pub body: String,
    pub state: String,
    pub is_draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub review_decision: Option<String>,
    pub mergeable: Option<String>,
    pub url: String,
    /// RFC3339 creation time, for the body card's relative timestamp.
    pub created_at: String,
    pub labels: Vec<String>,
    pub comments: Vec<Comment>,
    pub checks: Vec<Check>,
    /// Threaded inline review comments (from `api graphql` review threads).
    pub threads: Vec<ReviewThread>,
    /// Non-comment activity (commits, force-pushes, labels, …) from `api graphql`.
    pub events: Vec<TimelineEvent>,
}

/// Which kind of conversation comment a [`Comment`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CommentKind {
    /// A plain top-level conversation comment.
    #[default]
    Issue,
    /// A review summary (carries a `review_state`).
    Review,
}

/// A conversation entry — a plain issue comment or a review (with its verdict).
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub kind: CommentKind,
    /// `None` for issue comments; the review state (APPROVED / CHANGES_REQUESTED
    /// / COMMENTED) for reviews.
    pub review_state: Option<String>,
    pub created_at: String,
}

/// Normalized CI check status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Success,
    Failure,
    Pending,
    Neutral,
    Skipped,
    Cancelled,
}

/// One CI check.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub url: String,
}

/// One comment inside a review thread.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewThreadComment {
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A threaded set of inline review comments on a file/line, with its resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewThread {
    /// GraphQL node id — the handle for resolve/unresolve.
    pub id: String,
    pub path: String,
    pub line: Option<u32>,
    /// The diff hunk the thread hangs off (as GitHub returns it).
    pub diff_hunk: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<ReviewThreadComment>,
}

/// The kind of a non-comment timeline event. Mirrors the GraphQL `__typename`s
/// h20's `TIMELINE_QUERY` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Committed,
    ForcePushed,
    Reviewed,
    ReviewRequested,
    ReviewRequestRemoved,
    Merged,
    HeadRefDeleted,
    Labeled,
    Unlabeled,
    Assigned,
    Unassigned,
    AutoMergeEnabled,
    AutoMergeDisabled,
    Renamed,
    BaseRefChanged,
    ConvertToDraft,
    ReadyForReview,
}

/// A non-comment activity item on the PR timeline. A flat struct with a `kind`
/// tag and per-variant optional fields, mirroring h20's `PrTimelineEvent`.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineEvent {
    pub kind: EventKind,
    pub actor: Option<String>,
    pub created_at: String,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
    pub before_sha: Option<String>,
    pub after_sha: Option<String>,
    pub reviewer: Option<String>,
    pub label: Option<String>,
    pub assignee: Option<String>,
    pub previous_title: Option<String>,
    pub current_title: Option<String>,
    pub review_state: Option<String>,
}

/// One item in the unified, chronologically-ordered conversation stream built
/// by [`build_timeline`].
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineEntry {
    Comment(Comment),
    Thread(ReviewThread),
    Event(TimelineEvent),
    /// A run of consecutive commits, collapsed together like GitHub.
    CommitGroup(Vec<TimelineEvent>),
}

/// Interleave comments, review threads, and events into one time-ordered stream,
/// filtering out `reviewed` events (they already appear as review comment cards)
/// and collapsing consecutive commits into a [`TimelineEntry::CommitGroup`].
/// Pure over its inputs so it can be unit-tested without a CLI.
pub fn build_timeline(
    comments: &[Comment],
    threads: &[ReviewThread],
    events: &[TimelineEvent],
) -> Vec<TimelineEntry> {
    fn flush(group: &mut Vec<TimelineEvent>, keyed: &mut Vec<(String, TimelineEntry)>) {
        if !group.is_empty() {
            let time = group[0].created_at.clone();
            keyed.push((time, TimelineEntry::CommitGroup(std::mem::take(group))));
        }
    }

    let mut keyed: Vec<(String, TimelineEntry)> = Vec::new();
    for c in comments {
        keyed.push((c.created_at.clone(), TimelineEntry::Comment(c.clone())));
    }
    for t in threads {
        let time = t.comments.first().map(|c| c.created_at.clone()).unwrap_or_default();
        keyed.push((time, TimelineEntry::Thread(t.clone())));
    }
    let mut group: Vec<TimelineEvent> = Vec::new();
    for e in events {
        if e.kind == EventKind::Reviewed {
            continue;
        }
        if e.kind == EventKind::Committed {
            group.push(e.clone());
        } else {
            flush(&mut group, &mut keyed);
            keyed.push((e.created_at.clone(), TimelineEntry::Event(e.clone())));
        }
    }
    flush(&mut group, &mut keyed);

    // RFC3339 timestamps sort chronologically as plain strings; sort is stable
    // so same-instant items keep comment-before-thread-before-event order.
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, e)| e).collect()
}

// ── Raw serde shapes (the CLI's `--json` output) ────────────────────────────

#[derive(Deserialize, Default)]
struct RawAuthor {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSummary {
    #[serde(default)]
    number: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    status_check_rollup: Vec<RawCheck>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLabel {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    submitted_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheck {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    #[serde(default)]
    number: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    #[serde(default)]
    changed_files: u32,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: Vec<RawComment>,
    #[serde(default)]
    reviews: Vec<RawReview>,
    #[serde(default)]
    status_check_rollup: Vec<RawCheck>,
}

fn normalize_check(raw: RawCheck) -> Check {
    let name = raw
        .name
        .filter(|s| !s.is_empty())
        .or(raw.context)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "check".into());
    let url = raw
        .details_url
        .filter(|s| !s.is_empty())
        .or(raw.target_url)
        .unwrap_or_default();
    // A CheckRun reports `conclusion` once COMPLETED (else it's still running);
    // a legacy StatusContext reports `state`.
    let status = if let Some(c) = raw.conclusion.as_deref().filter(|s| !s.is_empty()) {
        match c {
            "SUCCESS" => CheckStatus::Success,
            "SKIPPED" => CheckStatus::Skipped,
            "NEUTRAL" => CheckStatus::Neutral,
            "CANCELLED" => CheckStatus::Cancelled,
            _ => CheckStatus::Failure, // FAILURE, TIMED_OUT, ACTION_REQUIRED, STARTUP_FAILURE
        }
    } else if raw.status.as_deref().is_some_and(|s| s != "COMPLETED") {
        CheckStatus::Pending
    } else {
        match raw.state.as_deref().unwrap_or("") {
            "SUCCESS" => CheckStatus::Success,
            "PENDING" | "EXPECTED" => CheckStatus::Pending,
            "" => CheckStatus::Pending,
            _ => CheckStatus::Failure, // FAILURE, ERROR
        }
    };
    Check { name, status, url }
}

/// Collapse a PR's checks into the single status a card can show.
///
/// Precedence is deliberate: any failure wins, then anything still running,
/// otherwise success. `Neutral` / `Skipped` / `Cancelled` are not interesting
/// enough to outrank success. Returns `None` for an empty slice so a source
/// that simply carries no checks never wipes a status we already know.
pub fn aggregate_check_status(checks: &[Check]) -> Option<CheckStatus> {
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Failure) {
        return Some(CheckStatus::Failure);
    }
    if checks.iter().any(|c| c.status == CheckStatus::Pending) {
        return Some(CheckStatus::Pending);
    }
    Some(CheckStatus::Success)
}

// ── Reads ───────────────────────────────────────────────────────────────

const DETAIL_FIELDS: &str = "number,title,body,state,isDraft,author,createdAt,headRefName,\
baseRefName,additions,deletions,changedFiles,reviewDecision,mergeable,url,labels,comments,\
reviews,statusCheckRollup";

const LIST_FIELDS: &str =
    "number,title,state,isDraft,headRefName,author,reviewDecision,mergeable,statusCheckRollup";

fn parse_summaries(out: &str) -> Result<Vec<PrSummary>, String> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<RawSummary> =
        serde_json::from_str(trimmed).map_err(|e| format!("parse pr list: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| PrSummary {
            number: r.number,
            title: r.title,
            state: r.state.to_lowercase(),
            is_draft: r.is_draft,
            head: r.head_ref_name,
            author: r.author.login,
            // The CLI returns "" (not null) for a PR nobody has reviewed.
            review_decision: r.review_decision.filter(|s| !s.is_empty()),
            mergeable: r.mergeable.filter(|s| !s.is_empty()),
            checks: r.status_check_rollup.into_iter().map(normalize_check).collect(),
        })
        .collect())
}

/// Run a `pr list` invocation, falling back to a blocking cache-through read
/// when the async fast path returns an empty placeholder — so a genuinely
/// non-empty list can't be masked by a cold `-A` miss.
fn fetch_list(dir: &Path, args: &[&str]) -> Result<Vec<PrSummary>, String> {
    let mut cmd = cli_cmd_sync(dir, false);
    cmd.args(args);
    // A list read is network-backed and polled from a background thread, so it
    // runs under h20's 4s list tier instead of blocking that thread forever.
    let summaries = parse_summaries(&run_within(cmd, crate::git::TIMEOUT_LIST)?)?;
    if !summaries.is_empty() || !use_async() {
        return Ok(summaries);
    }
    // Empty on the async path may be a cold miss; block once on a real read.
    let mut cmd = cli_cmd_sync(dir, true);
    cmd.args(args);
    parse_summaries(&run_within(cmd, crate::git::TIMEOUT_LIST)?)
}

/// List all open PRs for the repo containing `dir` (repo resolved from the git
/// remote by the CLI). Newest first, capped so the list stays snappy. Used by
/// the holistic Pull Requests page.
pub fn pr_list(dir: &Path) -> Result<Vec<PrSummary>, String> {
    fetch_list(dir, &["pr", "list", "--state", "open", "--limit", "50", "--json", LIST_FIELDS])
}

/// List the PR(s) whose head is `branch` (any state), for the branch-scoped PR
/// tool. Usually one; capped small.
pub fn pr_list_for_branch(dir: &Path, branch: &str) -> Result<Vec<PrSummary>, String> {
    fetch_list(
        dir,
        &["pr", "list", "--head", branch, "--state", "all", "--limit", "10", "--json", LIST_FIELDS],
    )
}

/// Fetch a single PR's detail. On a cold `lfg -A` miss the CLI returns an empty
/// object placeholder (`{}`), which deserializes to a `PrDetail` with
/// `number == 0` — the caller treats that as "still loading" and waits for the
/// SSE refresh.
pub fn pr_detail(dir: &Path, number: u32) -> Result<PrDetail, String> {
    let n = number.to_string();
    let args = ["pr", "view", &n, "--json", DETAIL_FIELDS];
    let mut cmd = cli_cmd_sync(dir, false);
    cmd.args(args);
    let mut out = run(cmd)?;
    // A cold `-A` miss yields `{}`; block once on a real read so the panel
    // gets the actual PR rather than sticking on the placeholder.
    if use_async() && matches!(out.trim(), "" | "{}") {
        let mut cmd = cli_cmd_sync(dir, true);
        cmd.args(args);
        out = run(cmd)?;
    }
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(PrDetail::default());
    }
    let raw: RawDetail =
        serde_json::from_str(trimmed).map_err(|e| format!("parse pr view: {e}"))?;

    // Merge issue comments and reviews into one time-ordered conversation.
    let mut comments: Vec<Comment> = raw
        .comments
        .into_iter()
        .map(|c| Comment {
            author: c.author.login,
            body: c.body,
            kind: CommentKind::Issue,
            review_state: None,
            created_at: c.created_at,
        })
        .collect();
    comments.extend(raw.reviews.into_iter().filter(|r| !r.body.is_empty() || r.state != "COMMENTED").map(|r| {
        Comment {
            author: r.author.login,
            body: r.body,
            kind: CommentKind::Review,
            review_state: Some(r.state),
            created_at: r.submitted_at,
        }
    }));
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    // Threads + timeline come from `api graphql` passthrough, which needs the
    // owner/repo (resolved from the git remote). Best-effort: a failure here
    // leaves the timeline empty rather than dropping the whole detail.
    let (threads, events) = match origin_nwo(dir) {
        Some((owner, repo)) => (
            pr_review_threads(dir, &owner, &repo, number).unwrap_or_default(),
            pr_timeline(dir, &owner, &repo, number).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };

    Ok(PrDetail {
        number: raw.number,
        title: raw.title,
        body: raw.body,
        state: raw.state.to_lowercase(),
        is_draft: raw.is_draft,
        author: raw.author.login,
        head: raw.head_ref_name,
        base: raw.base_ref_name,
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        review_decision: raw.review_decision.filter(|s| !s.is_empty()),
        mergeable: raw.mergeable.filter(|s| !s.is_empty()),
        url: raw.url,
        created_at: raw.created_at,
        labels: raw.labels.into_iter().map(|l| l.name).filter(|n| !n.is_empty()).collect(),
        comments,
        checks: raw.status_check_rollup.into_iter().map(normalize_check).collect(),
        threads,
        events,
    })
}

// ── GraphQL passthrough (timeline + review threads) ─────────────────────────

const TIMELINE_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      timelineItems(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          __typename
          ... on PullRequestCommit {
            commit { oid abbreviatedOid message committedDate author { user { login } name } }
          }
          ... on HeadRefForcePushedEvent {
            createdAt actor { login }
            beforeCommit { abbreviatedOid oid }
            afterCommit { abbreviatedOid oid }
          }
          ... on PullRequestReview { createdAt author { login } state }
          ... on ReviewRequestedEvent {
            createdAt actor { login }
            requestedReviewer { ... on User { login } ... on Team { name } }
          }
          ... on ReviewRequestRemovedEvent {
            createdAt actor { login }
            requestedReviewer { ... on User { login } ... on Team { name } }
          }
          ... on MergedEvent { createdAt actor { login } }
          ... on HeadRefDeletedEvent { createdAt actor { login } }
          ... on LabeledEvent { createdAt actor { login } label { name } }
          ... on UnlabeledEvent { createdAt actor { login } label { name } }
          ... on AssignedEvent { createdAt actor { login } assignee { ... on User { login } } }
          ... on UnassignedEvent { createdAt actor { login } assignee { ... on User { login } } }
          ... on AutoMergeEnabledEvent { createdAt actor { login } }
          ... on AutoMergeDisabledEvent { createdAt actor { login } }
          ... on RenamedTitleEvent { createdAt actor { login } previousTitle currentTitle }
          ... on BaseRefChangedEvent { createdAt actor { login } }
          ... on ConvertToDraftEvent { createdAt actor { login } }
          ... on ReadyForReviewEvent { createdAt actor { login } }
        }
      }
    }
  }
}"#;

const REVIEW_THREADS_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line
          comments(first: 100) {
            nodes { id author { login } body createdAt diffHunk }
          }
        }
      }
    }
  }
}"#;

/// Resolve `owner/repo` from the git remote (`origin`) of `dir`, offline.
fn origin_nwo(dir: &Path) -> Option<(String, String)> {
    let out = crate::git::augmented_command("git")
        .current_dir(dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nwo(String::from_utf8_lossy(&out.stdout).trim())
}

/// Parse an owner/repo pair out of a GitHub remote URL (ssh or https).
fn parse_nwo(url: &str) -> Option<(String, String)> {
    let rest = url.rsplit_once("github.com")?.1;
    let rest = rest.trim_start_matches([':', '/']).trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Run one page of a paginated `api graphql` query and return the parsed JSON.
fn run_graphql(
    dir: &Path,
    sync: bool,
    query: &str,
    owner: &str,
    repo: &str,
    number: u32,
    cursor: Option<&str>,
) -> Result<serde_json::Value, String> {
    let mut cmd = cli_cmd_sync(dir, sync);
    cmd.arg("api").arg("graphql");
    cmd.arg("-f").arg(format!("query={query}"));
    // String GraphQL vars go through `-f` (raw), not `-F` (typed): `-F` would
    // coerce an all-digit or keyword repo/owner name (both legal on GitHub) into
    // a JSON number/bool and fail the `String!` type, and its leading-`@`
    // "read this file" magic is a hazard for any non-literal value. Only
    // `number` is a GraphQL `Int!`, so it alone needs `-F`.
    cmd.arg("-f").arg(format!("owner={owner}"));
    cmd.arg("-f").arg(format!("repo={repo}"));
    cmd.arg("-F").arg(format!("number={number}"));
    if let Some(c) = cursor {
        cmd.arg("-f").arg(format!("cursor={c}"));
    }
    let out = run(cmd)?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(trimmed).map_err(|e| format!("parse graphql: {e}"))
}

/// Walk every page of `field` (a connection under `repository.pullRequest`),
/// collecting the raw node values. Mirrors `pr_detail`'s cold-miss handling: an
/// empty first page on the async path is retried once synchronously.
fn graphql_pages(
    dir: &Path,
    query: &str,
    owner: &str,
    repo: &str,
    number: u32,
    field: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    let mut first = true;
    loop {
        let mut v = run_graphql(dir, false, query, owner, repo, number, cursor.as_deref())?;
        let mut conn = v["data"]["repository"]["pullRequest"][field].clone();
        let empty = conn["nodes"].as_array().is_none_or(|a| a.is_empty());
        if first && empty && use_async() {
            v = run_graphql(dir, true, query, owner, repo, number, cursor.as_deref())?;
            conn = v["data"]["repository"]["pullRequest"][field].clone();
        }
        first = false;
        let nodes = conn["nodes"].as_array().cloned().unwrap_or_default();
        let stop = nodes.is_empty();
        all.extend(nodes);
        let has_next = conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        let end = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
        match end {
            Some(c) if has_next && !stop => cursor = Some(c),
            _ => break,
        }
    }
    Ok(all)
}

/// Fetch the PR's timeline events via `api graphql`.
pub fn pr_timeline(dir: &Path, owner: &str, repo: &str, number: u32) -> Result<Vec<TimelineEvent>, String> {
    let nodes = graphql_pages(dir, TIMELINE_QUERY, owner, repo, number, "timelineItems")?;
    Ok(nodes.iter().filter_map(parse_timeline_node).collect())
}

/// Fetch the PR's review threads via `api graphql`.
pub fn pr_review_threads(dir: &Path, owner: &str, repo: &str, number: u32) -> Result<Vec<ReviewThread>, String> {
    let nodes = graphql_pages(dir, REVIEW_THREADS_QUERY, owner, repo, number, "reviewThreads")?;
    Ok(nodes.iter().filter_map(parse_thread_node).collect())
}

/// Read a non-empty string field off a JSON object.
fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

fn parse_timeline_node(node: &serde_json::Value) -> Option<TimelineEvent> {
    let kind = match node.get("__typename").and_then(|t| t.as_str())? {
        "PullRequestCommit" => EventKind::Committed,
        "HeadRefForcePushedEvent" => EventKind::ForcePushed,
        "PullRequestReview" => EventKind::Reviewed,
        "ReviewRequestedEvent" => EventKind::ReviewRequested,
        "ReviewRequestRemovedEvent" => EventKind::ReviewRequestRemoved,
        "MergedEvent" => EventKind::Merged,
        "HeadRefDeletedEvent" => EventKind::HeadRefDeleted,
        "LabeledEvent" => EventKind::Labeled,
        "UnlabeledEvent" => EventKind::Unlabeled,
        "AssignedEvent" => EventKind::Assigned,
        "UnassignedEvent" => EventKind::Unassigned,
        "AutoMergeEnabledEvent" => EventKind::AutoMergeEnabled,
        "AutoMergeDisabledEvent" => EventKind::AutoMergeDisabled,
        "RenamedTitleEvent" => EventKind::Renamed,
        "BaseRefChangedEvent" => EventKind::BaseRefChanged,
        "ConvertToDraftEvent" => EventKind::ConvertToDraft,
        "ReadyForReviewEvent" => EventKind::ReadyForReview,
        _ => return None,
    };
    let mut ev = TimelineEvent {
        kind,
        actor: None,
        created_at: String::new(),
        commit_sha: None,
        commit_message: None,
        before_sha: None,
        after_sha: None,
        reviewer: None,
        label: None,
        assignee: None,
        previous_title: None,
        current_title: None,
        review_state: None,
    };
    if kind == EventKind::Committed {
        let commit = node.get("commit")?;
        ev.created_at = str_field(commit, "committedDate").unwrap_or_default();
        ev.commit_sha = str_field(commit, "abbreviatedOid").or_else(|| str_field(commit, "oid"));
        ev.commit_message =
            str_field(commit, "message").map(|m| m.lines().next().unwrap_or("").to_string());
        ev.actor = commit.get("author").and_then(|a| {
            a.get("user").and_then(|u| str_field(u, "login")).or_else(|| str_field(a, "name"))
        });
    } else {
        ev.created_at = str_field(node, "createdAt").unwrap_or_default();
        ev.actor = node.get("actor").and_then(|a| str_field(a, "login"));
    }
    match kind {
        EventKind::ForcePushed => {
            ev.before_sha = node.get("beforeCommit").and_then(|c| str_field(c, "abbreviatedOid"));
            ev.after_sha = node.get("afterCommit").and_then(|c| str_field(c, "abbreviatedOid"));
        }
        EventKind::ReviewRequested | EventKind::ReviewRequestRemoved => {
            ev.reviewer = node
                .get("requestedReviewer")
                .and_then(|r| str_field(r, "login").or_else(|| str_field(r, "name")));
        }
        EventKind::Labeled | EventKind::Unlabeled => {
            ev.label = node.get("label").and_then(|l| str_field(l, "name"));
        }
        EventKind::Assigned | EventKind::Unassigned => {
            ev.assignee = node.get("assignee").and_then(|a| str_field(a, "login"));
        }
        EventKind::Renamed => {
            ev.previous_title = str_field(node, "previousTitle");
            ev.current_title = str_field(node, "currentTitle");
        }
        EventKind::Reviewed => ev.review_state = str_field(node, "state"),
        _ => {}
    }
    Some(ev)
}

fn parse_thread_node(node: &serde_json::Value) -> Option<ReviewThread> {
    let comment_nodes = node.get("comments")?.get("nodes")?.as_array()?;
    if comment_nodes.is_empty() {
        return None;
    }
    let diff_hunk = str_field(&comment_nodes[0], "diffHunk").unwrap_or_default();
    let comments = comment_nodes
        .iter()
        .map(|c| ReviewThreadComment {
            author: c.get("author").and_then(|a| str_field(a, "login")).unwrap_or_default(),
            body: str_field(c, "body").unwrap_or_default(),
            created_at: str_field(c, "createdAt").unwrap_or_default(),
        })
        .collect();
    Some(ReviewThread {
        id: str_field(node, "id").unwrap_or_default(),
        path: str_field(node, "path").unwrap_or_default(),
        line: node.get("line").and_then(|l| l.as_u64()).map(|n| n as u32),
        diff_hunk,
        is_resolved: node.get("isResolved").and_then(|b| b.as_bool()).unwrap_or(false),
        is_outdated: node.get("isOutdated").and_then(|b| b.as_bool()).unwrap_or(false),
        comments,
    })
}

/// The PR head commit SHA via the configured CLI, or None.
fn pr_head_sha(dir: &Path, number: u32) -> Option<String> {
    let n = number.to_string();
    let mut cmd = cli_cmd_sync(dir, false);
    cmd.args(["pr", "view", &n, "--json", "headRefOid"]);
    let out = run(cmd).ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    v.get("headRefOid")?.as_str().map(str::to_string)
}

/// Fill in each file's new-side content from the PR head commit, when that
/// commit happens to be present locally. Best effort: anything that fails
/// leaves `new_lines` as `None` and expansion stays disabled.
fn fill_new_lines(dir: &Path, number: u32, files: &mut [DiffFile]) {
    let Some(sha) = pr_head_sha(dir, number) else { return };
    for f in files.iter_mut().filter(|f| !f.binary) {
        f.new_lines = crate::git::file_lines_at(dir, Some(&sha), &f.path);
    }
}

/// Fetch and parse a PR's unified diff.
pub fn pr_diff(dir: &Path, number: u32) -> Result<Vec<DiffFile>, String> {
    let n = number.to_string();
    let args = ["pr", "diff", &n];
    let mut cmd = cli_cmd_sync(dir, false);
    cmd.args(args);
    let out = run(cmd)?;
    let mut files = crate::diff::parse(&out);
    // An empty diff on the async path is likely a cold miss (a PR always has a
    // diff); block once for the real one.
    if files.is_empty() && use_async() {
        let mut cmd = cli_cmd_sync(dir, true);
        cmd.args(args);
        let mut files = crate::diff::parse(&run(cmd)?);
        fill_new_lines(dir, number, &mut files);
        return Ok(files);
    }
    fill_new_lines(dir, number, &mut files);
    Ok(files)
}

// ── Writes ────────────────────────────────────────────────────────────────

/// A write action the panel can invoke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    RequestChanges(String),
    Comment(String),
    /// Mark a draft PR ready for review.
    Ready,
    /// Merge (squash) the PR.
    Merge,
    /// Close the PR without merging.
    Close,
    /// Resolve (`true`) or unresolve (`false`) a review thread by node id.
    ResolveThread { id: String, resolved: bool },
}

impl Action {
    /// Human label for the confirmation / result message.
    pub fn describe(&self) -> &'static str {
        match self {
            Action::Approve => "Approved",
            Action::RequestChanges(_) => "Requested changes",
            Action::Comment(_) => "Commented",
            Action::Ready => "Marked ready for review",
            Action::Merge => "Merged (squash)",
            Action::Close => "Closed",
            Action::ResolveThread { resolved: true, .. } => "Resolved thread",
            Action::ResolveThread { resolved: false, .. } => "Unresolved thread",
        }
    }
}

const RESOLVE_THREAD_MUTATION: &str =
    "mutation($threadId: ID!) { resolveReviewThread(input: { threadId: $threadId }) { thread { id } } }";
const UNRESOLVE_THREAD_MUTATION: &str =
    "mutation($threadId: ID!) { unresolveReviewThread(input: { threadId: $threadId }) { thread { id } } }";

/// Run a write action against a PR. Writes never use the async placeholder
/// path — they must block on the real mutation — so this shells the CLI
/// directly (no `-A`). Returns a short success message.
pub fn run_action(dir: &Path, number: u32, action: &Action) -> Result<String, String> {
    let n = number.to_string();
    let mut cmd = crate::git::augmented_command(&cli());
    cmd.current_dir(dir);
    match action {
        Action::Approve => {
            cmd.args(["pr", "review", &n, "--approve"]);
        }
        Action::RequestChanges(body) => {
            cmd.args(["pr", "review", &n, "--request-changes", "--body", body]);
        }
        Action::Comment(body) => {
            cmd.args(["pr", "comment", &n, "--body", body]);
        }
        Action::Ready => {
            cmd.args(["pr", "ready", &n]);
        }
        Action::Merge => {
            cmd.args(["pr", "merge", &n, "--squash"]);
        }
        Action::Close => {
            cmd.args(["pr", "close", &n]);
        }
        Action::ResolveThread { id, resolved } => {
            // threadId is a global node id and needs no owner/repo. Pass it as a
            // raw `-f` field (not `-F`): gh's `-F` treats a leading `@` as
            // "read this file", so a crafted id could otherwise leak a file.
            let mutation = if *resolved { RESOLVE_THREAD_MUTATION } else { UNRESOLVE_THREAD_MUTATION };
            cmd.arg("api").arg("graphql");
            cmd.arg("-f").arg(format!("threadId={id}"));
            cmd.arg("-f").arg(format!("query={mutation}"));
        }
    }
    run(cmd)?;
    // Freshness is handled by the caller re-fetching with `-A`, which kicks a
    // background refresh and streams the updated entity back over SSE.
    Ok(action.describe().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_check_run_conclusions() {
        let mk = |conclusion: &str| {
            normalize_check(RawCheck {
                name: Some("build".into()),
                context: None,
                status: Some("COMPLETED".into()),
                conclusion: Some(conclusion.into()),
                state: None,
                details_url: Some("http://x".into()),
                target_url: None,
            })
        };
        assert_eq!(mk("SUCCESS").status, CheckStatus::Success);
        assert_eq!(mk("FAILURE").status, CheckStatus::Failure);
        assert_eq!(mk("TIMED_OUT").status, CheckStatus::Failure);
        assert_eq!(mk("SKIPPED").status, CheckStatus::Skipped);
        assert_eq!(mk("CANCELLED").status, CheckStatus::Cancelled);
        assert_eq!(mk("SUCCESS").name, "build");
        assert_eq!(mk("SUCCESS").url, "http://x");
    }

    #[test]
    fn in_progress_check_run_is_pending() {
        let c = normalize_check(RawCheck {
            name: Some("test".into()),
            context: None,
            status: Some("IN_PROGRESS".into()),
            conclusion: None,
            state: None,
            details_url: None,
            target_url: None,
        });
        assert_eq!(c.status, CheckStatus::Pending);
    }

    #[test]
    fn aggregate_check_status_follows_failure_pending_success() {
        let chk = |status| Check {
            name: "c".into(),
            status,
            url: String::new(),
        };
        // No checks at all is "unknown", not "green".
        assert_eq!(aggregate_check_status(&[]), None);
        assert_eq!(
            aggregate_check_status(&[chk(CheckStatus::Success), chk(CheckStatus::Success)]),
            Some(CheckStatus::Success)
        );
        assert_eq!(
            aggregate_check_status(&[chk(CheckStatus::Success), chk(CheckStatus::Pending)]),
            Some(CheckStatus::Pending)
        );
        assert_eq!(
            aggregate_check_status(&[chk(CheckStatus::Success), chk(CheckStatus::Failure)]),
            Some(CheckStatus::Failure)
        );
        // Failure outranks anything still running.
        assert_eq!(
            aggregate_check_status(&[chk(CheckStatus::Pending), chk(CheckStatus::Failure)]),
            Some(CheckStatus::Failure)
        );
        // Neutral/skipped/cancelled never outrank success.
        assert_eq!(
            aggregate_check_status(&[
                chk(CheckStatus::Skipped),
                chk(CheckStatus::Neutral),
                chk(CheckStatus::Cancelled),
            ]),
            Some(CheckStatus::Success)
        );
    }

    #[test]
    fn aggregate_check_status_counts_unfinished_check_run_as_pending() {
        let running = normalize_check(RawCheck {
            name: Some("build".into()),
            context: None,
            status: Some("QUEUED".into()),
            conclusion: None,
            state: None,
            details_url: None,
            target_url: None,
        });
        let done = normalize_check(RawCheck {
            name: Some("lint".into()),
            context: None,
            status: Some("COMPLETED".into()),
            conclusion: Some("SUCCESS".into()),
            state: None,
            details_url: None,
            target_url: None,
        });
        assert_eq!(
            aggregate_check_status(&[done, running]),
            Some(CheckStatus::Pending)
        );
    }

    #[test]
    fn normalizes_legacy_status_context() {
        let c = normalize_check(RawCheck {
            name: None,
            context: Some("ci/circle".into()),
            status: None,
            conclusion: None,
            state: Some("SUCCESS".into()),
            details_url: None,
            target_url: Some("http://ci".into()),
        });
        assert_eq!(c.status, CheckStatus::Success);
        assert_eq!(c.name, "ci/circle");
        assert_eq!(c.url, "http://ci");
    }

    #[test]
    fn detail_placeholder_parses_to_empty() {
        // Simulates lfg's cold `-A` `{}` placeholder path.
        let raw: RawDetail = serde_json::from_str("{}").unwrap();
        assert_eq!(raw.number, 0);
    }

    #[test]
    fn summary_json_round_trips() {
        let json = r#"[{"number":42,"title":"Fix","state":"OPEN","isDraft":true,"headRefName":"feat","author":{"login":"me"}}]"#;
        let raw: Vec<RawSummary> = serde_json::from_str(json).unwrap();
        assert_eq!(raw[0].number, 42);
        assert!(raw[0].is_draft);
        assert_eq!(raw[0].head_ref_name, "feat");
        assert_eq!(raw[0].author.login, "me");
    }

    #[test]
    fn auth_failures_are_distinct_from_timeouts_and_missing_binaries() {
        assert!(is_auth_failure("gh: To get started with GitHub CLI, run: gh auth login"));
        assert!(is_auth_failure("HTTP 401: Bad credentials"));
        assert!(is_auth_failure("You are not logged in to any GitHub hosts"));
        assert!(!is_auth_failure("lfg timed out"));
        assert!(!is_auth_failure("lfg is not installed"));
        assert!(!is_auth_failure("no git remote found"));
    }

    /// A draft PR must reach the sidebar as a draft even when its checks are
    /// failing or it conflicts — the states that used to outrank draftness and
    /// paint the green open-PR avatar. Driven from verbatim `gh pr list` output
    /// rather than a hand-built struct, so a field rename in the JSON contract
    /// fails here too.
    #[test]
    fn live_json_carries_draftness_to_the_card_avatar() {
        use crate::git_context::derive_rollup;
        use crate::sidebar_card::{CardAvatar, avatar_for};

        // Verbatim `gh pr list --json ...` output for this very PR (a draft).
        let live = r#"[{"number":23,"title":"Sidebar: draw draft PRs as drafts, and honor the app text size","state":"OPEN","isDraft":true,"headRefName":"tw-sidebar-pr-state-a11y","reviewDecision":"","mergeable":"UNKNOWN","statusCheckRollup":[]}]"#;
        let prs = parse_summaries(live).unwrap();
        assert!(prs[0].is_draft, "isDraft must survive parsing");
        let r = derive_rollup(Some(&prs[0]));
        assert_eq!(avatar_for(r, prs[0].is_draft), CardAvatar::Draft);

        // The same PR once a check fails — the shape that used to draw green.
        let failing = live.replace(
            r#""statusCheckRollup":[]"#,
            r#""statusCheckRollup":[{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}]"#,
        );
        let prs = parse_summaries(&failing).unwrap();
        let r = derive_rollup(Some(&prs[0]));
        assert_eq!(r, crate::git_context::PrRollup::ChecksFailing);
        assert_eq!(avatar_for(r, prs[0].is_draft), CardAvatar::Draft);

        // And conflicting.
        let conflicting = live.replace(r#""mergeable":"UNKNOWN""#, r#""mergeable":"CONFLICTING""#);
        let prs = parse_summaries(&conflicting).unwrap();
        let r = derive_rollup(Some(&prs[0]));
        assert_eq!(avatar_for(r, prs[0].is_draft), CardAvatar::Draft);

        // A real non-draft open PR must be unaffected.
        let not_draft = live.replace(r#""isDraft":true"#, r#""isDraft":false"#);
        let prs = parse_summaries(&not_draft).unwrap();
        let r = derive_rollup(Some(&prs[0]));
        assert_eq!(avatar_for(r, prs[0].is_draft), CardAvatar::Open);
    }

    #[test]
    fn parse_summaries_maps_review_checks_and_mergeable() {
        let json = r#"[{"number":7,"title":"Ship","state":"OPEN","isDraft":false,
            "headRefName":"feat","author":{"login":"me"},"reviewDecision":"APPROVED",
            "mergeable":"MERGEABLE","statusCheckRollup":[
                {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"u"},
                {"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","detailsUrl":"v"}]}]"#;
        let prs = parse_summaries(json).unwrap();
        assert_eq!(prs[0].review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(prs[0].mergeable.as_deref(), Some("MERGEABLE"));
        assert_eq!(prs[0].checks.len(), 2);
        assert_eq!(prs[0].checks[0].name, "build");
        assert_eq!(prs[0].checks[0].status, CheckStatus::Success);
        assert_eq!(prs[0].checks[1].status, CheckStatus::Pending);
        assert_eq!(aggregate_check_status(&prs[0].checks), Some(CheckStatus::Pending));
    }

    #[test]
    fn parse_summaries_tolerates_missing_new_fields() {
        // Older payloads (and lfg's cold placeholder) omit the added fields.
        let json = r#"[{"number":42,"title":"Fix","state":"OPEN","isDraft":true,"headRefName":"feat","author":{"login":"me"}}]"#;
        let prs = parse_summaries(json).unwrap();
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].state, "open");
        assert_eq!(prs[0].review_decision, None);
        assert_eq!(prs[0].mergeable, None);
        assert!(prs[0].checks.is_empty());
    }

    #[test]
    fn parse_summaries_normalizes_empty_review_decision_to_none() {
        // The CLI returns "" (not null) for a PR nobody has reviewed yet.
        let json = r#"[{"number":9,"title":"WIP","state":"OPEN","isDraft":false,
            "headRefName":"wip","author":{"login":"me"},"reviewDecision":"","mergeable":""}]"#;
        let prs = parse_summaries(json).unwrap();
        assert_eq!(prs[0].review_decision, None);
        assert_eq!(prs[0].mergeable, None);
    }

    #[test]
    fn action_describe_is_stable() {
        assert_eq!(Action::Approve.describe(), "Approved");
        assert_eq!(Action::Merge.describe(), "Merged (squash)");
        assert_eq!(Action::Close.describe(), "Closed");
        assert_eq!(Action::ResolveThread { id: "x".into(), resolved: true }.describe(), "Resolved thread");
        assert_eq!(Action::ResolveThread { id: "x".into(), resolved: false }.describe(), "Unresolved thread");
    }

    #[test]
    fn parses_owner_repo_from_remote_urls() {
        let want = Some(("owner".to_string(), "repo".to_string()));
        assert_eq!(parse_nwo("git@github.com:owner/repo.git"), want);
        assert_eq!(parse_nwo("https://github.com/owner/repo.git"), want);
        assert_eq!(parse_nwo("https://github.com/owner/repo"), want);
        assert_eq!(parse_nwo("ssh://git@github.com/owner/repo.git"), want);
        assert_eq!(parse_nwo("https://gitlab.com/owner/repo.git"), None);
        assert_eq!(parse_nwo("not a url"), None);
    }

    fn ev(kind: EventKind, created_at: &str) -> TimelineEvent {
        TimelineEvent {
            kind,
            actor: None,
            created_at: created_at.into(),
            commit_sha: None,
            commit_message: None,
            before_sha: None,
            after_sha: None,
            reviewer: None,
            label: None,
            assignee: None,
            previous_title: None,
            current_title: None,
            review_state: None,
        }
    }

    #[test]
    fn build_timeline_groups_commits_filters_reviews_and_sorts() {
        let comments = vec![Comment {
            author: "a".into(),
            body: "hi".into(),
            kind: CommentKind::Issue,
            review_state: None,
            created_at: "2024-01-02T00:00:00Z".into(),
        }];
        let events = vec![
            ev(EventKind::Committed, "2024-01-01T00:00:00Z"),
            ev(EventKind::Committed, "2024-01-01T01:00:00Z"),
            ev(EventKind::Reviewed, "2024-01-01T02:00:00Z"),
            ev(EventKind::Labeled, "2024-01-03T00:00:00Z"),
        ];
        let tl = build_timeline(&comments, &[], &events);
        assert_eq!(tl.len(), 3, "reviewed filtered, two commits grouped");
        match &tl[0] {
            TimelineEntry::CommitGroup(g) => assert_eq!(g.len(), 2),
            other => panic!("expected commit group first, got {other:?}"),
        }
        assert!(matches!(tl[1], TimelineEntry::Comment(_)));
        match &tl[2] {
            TimelineEntry::Event(e) => assert_eq!(e.kind, EventKind::Labeled),
            other => panic!("expected labeled event last, got {other:?}"),
        }
    }

    #[test]
    fn build_timeline_splits_commit_run_on_non_commit() {
        let events = vec![
            ev(EventKind::Committed, "2024-01-01T00:00:00Z"),
            ev(EventKind::ForcePushed, "2024-01-01T00:30:00Z"),
            ev(EventKind::Committed, "2024-01-01T01:00:00Z"),
        ];
        let tl = build_timeline(&[], &[], &events);
        // group[commit] , event[force] , group[commit]
        assert_eq!(tl.len(), 3);
        assert!(matches!(tl[0], TimelineEntry::CommitGroup(_)));
        assert!(matches!(tl[1], TimelineEntry::Event(_)));
        assert!(matches!(tl[2], TimelineEntry::CommitGroup(_)));
    }

    #[test]
    fn parses_commit_timeline_node() {
        let node = serde_json::json!({
            "__typename": "PullRequestCommit",
            "commit": {
                "oid": "abcdef0",
                "abbreviatedOid": "abc123",
                "message": "Fix things\n\nlonger body",
                "committedDate": "2024-01-01T00:00:00Z",
                "author": { "user": { "login": "me" }, "name": "Me" }
            }
        });
        let e = parse_timeline_node(&node).unwrap();
        assert_eq!(e.kind, EventKind::Committed);
        assert_eq!(e.commit_sha.as_deref(), Some("abc123"));
        assert_eq!(e.commit_message.as_deref(), Some("Fix things"));
        assert_eq!(e.actor.as_deref(), Some("me"));
    }

    #[test]
    fn parses_force_push_and_unknown_timeline_nodes() {
        let fp = serde_json::json!({
            "__typename": "HeadRefForcePushedEvent",
            "createdAt": "2024-01-01T00:00:00Z",
            "actor": { "login": "x" },
            "beforeCommit": { "abbreviatedOid": "aaa" },
            "afterCommit": { "abbreviatedOid": "bbb" }
        });
        let e = parse_timeline_node(&fp).unwrap();
        assert_eq!(e.kind, EventKind::ForcePushed);
        assert_eq!(e.actor.as_deref(), Some("x"));
        assert_eq!(e.before_sha.as_deref(), Some("aaa"));
        assert_eq!(e.after_sha.as_deref(), Some("bbb"));
        assert!(parse_timeline_node(&serde_json::json!({ "__typename": "WeirdEvent" })).is_none());
    }

    #[test]
    fn parses_review_thread_node() {
        let node = serde_json::json!({
            "id": "THREAD1",
            "isResolved": true,
            "isOutdated": false,
            "path": "src/x.rs",
            "line": 42,
            "comments": { "nodes": [
                { "id": "C1", "author": { "login": "me" }, "body": "nit", "createdAt": "t", "diffHunk": "@@ -1 +1 @@" }
            ] }
        });
        let t = parse_thread_node(&node).unwrap();
        assert_eq!(t.id, "THREAD1");
        assert!(t.is_resolved);
        assert_eq!(t.path, "src/x.rs");
        assert_eq!(t.line, Some(42));
        assert_eq!(t.diff_hunk, "@@ -1 +1 @@");
        assert_eq!(t.comments.len(), 1);
        assert_eq!(t.comments[0].author, "me");
        // A thread with no comments is dropped.
        assert!(parse_thread_node(&serde_json::json!({ "id": "T", "comments": { "nodes": [] } })).is_none());
    }
}
