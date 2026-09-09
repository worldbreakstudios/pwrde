//! The per-group git/PR aggregate, gathered off the paint path.
//!
//! h20 learned this the hard way: a sidebar card wants one flat answer — repo,
//! branch, dirty counts, and the branch's pull request rolled up to a single
//! badge — but every piece of it comes from a blocking shell-out. So the
//! gathering lives here, in one plain-data module a background thread can call,
//! and the UI only ever reads the last snapshot it produced.
//!
//! Everything is gpui-free: plain structs, `std`, and calls into [`crate::git`]
//! and [`crate::gh`]. That is what keeps the precedence rules testable without
//! a repo or a network.

// The consumers of this module (the sidebar and its refresh thread) land in a
// later pass; until then every item here is legitimately unused.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::gh::{self, CheckStatus, PrSummary};
use crate::git::{self, DirtyStats};

/// Everything the sidebar knows about one group's working directory.
///
/// A snapshot, not a live view: `fetched_at` records when the shell-outs ran so
/// the cache can decide whether it is worth running them again.
#[derive(Clone, Debug)]
pub struct GitContext {
    /// Whether `cwd` is inside a git repository at all.
    pub is_git: bool,
    /// The directory the snapshot describes.
    pub cwd: PathBuf,
    /// Repository display name, from the common dir (see
    /// [`crate::git::repo_display_name`] for why not the worktree basename).
    pub repo: Option<String>,
    /// Current branch, or `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The remote's default branch, e.g. `origin/main`.
    pub default_branch: Option<String>,
    /// Uncommitted work relative to HEAD.
    pub dirty: Option<DirtyStats>,
    /// This branch's *committed* work, versus its merge-base with the base
    /// branch. What a card means by "changes on this branch"; `dirty` is only
    /// what has not been committed yet.
    pub branch_diff: Option<DirtyStats>,
    /// The pull request for `branch`, picked by [`select_pr`].
    pub pr: Option<PrSummary>,
    /// Why the PR lookup failed, phrased for the UI. Set means "this snapshot
    /// learned nothing new about the PR", which [`merge`] treats specially.
    pub pr_error: Option<String>,
    /// When the snapshot was taken. Poll time — not a signal about the work,
    /// so do not stamp a card with it.
    pub fetched_at: SystemTime,
}

impl GitContext {
    /// An empty snapshot for a directory that is not (or not yet known to be) a
    /// repository.
    fn empty(dir: &Path) -> Self {
        Self {
            is_git: false,
            cwd: dir.to_path_buf(),
            repo: None,
            branch: None,
            default_branch: None,
            dirty: None,
            branch_diff: None,
            pr: None,
            pr_error: None,
            fetched_at: SystemTime::now(),
        }
    }
}

/// The single badge a card shows for its pull request.
///
/// Ported from h20 unchanged, variants and all: the ladder that produces it
/// ([`derive_rollup`]) is tuned so the most actionable state wins, and adding
/// or reordering variants quietly changes what a user sees first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrRollup {
    /// No pull request for this branch.
    None,
    /// Someone commented or requested changes.
    OpenCommented,
    /// Approved, mergeable, and checks are not still running.
    OpenReadyToMerge,
    /// Approved, but not yet clear to merge.
    OpenApproved,
    /// GitHub is waiting on a reviewer.
    OpenNeedsReview,
    /// Open with nothing else to say about it.
    OpenPending,
    /// Still a draft.
    Draft,
    /// Merged or closed.
    Finished,
    /// At least one check failed.
    ChecksFailing,
    /// Checks are still running.
    ChecksPending,
    /// The branch conflicts with its base.
    MergeConflicts,
}

/// Collapse a pull request into the badge a card shows.
///
/// The ladder is h20's, verbatim, and the order is the whole point: a finished
/// PR says nothing else, conflicts and failing checks outrank draft status
/// (they are what needs doing), and an explicit `REVIEW_REQUIRED` outranks
/// still-running checks because the reviewer is the blocker, not CI.
pub fn derive_rollup(pr: Option<&PrSummary>) -> PrRollup {
    let Some(pr) = pr else {
        return PrRollup::None;
    };
    if !pr.state.eq_ignore_ascii_case("open") {
        return PrRollup::Finished;
    }
    if pr.mergeable.as_deref() == Some("CONFLICTING") {
        return PrRollup::MergeConflicts;
    }
    let checks = gh::aggregate_check_status(&pr.checks);
    if checks == Some(CheckStatus::Failure) {
        return PrRollup::ChecksFailing;
    }
    if pr.is_draft {
        return PrRollup::Draft;
    }
    match pr.review_decision.as_deref() {
        Some("APPROVED") => {
            if pr.mergeable.as_deref() == Some("MERGEABLE") && checks != Some(CheckStatus::Pending)
            {
                PrRollup::OpenReadyToMerge
            } else {
                PrRollup::OpenApproved
            }
        }
        Some("CHANGES_REQUESTED") | Some("COMMENTED") => PrRollup::OpenCommented,
        Some("REVIEW_REQUIRED") => PrRollup::OpenNeedsReview,
        _ => {
            if checks == Some(CheckStatus::Pending) {
                PrRollup::ChecksPending
            } else {
                PrRollup::OpenPending
            }
        }
    }
}

/// Pick the one pull request a branch's card should show.
///
/// An open PR always wins, however old; otherwise the CLI's first result (it
/// lists newest first) stands in, so a merged branch still shows what happened
/// to it.
pub fn select_pr(prs: Vec<PrSummary>) -> Option<PrSummary> {
    let open = prs
        .iter()
        .position(|p| p.state.eq_ignore_ascii_case("open"));
    match open {
        Some(i) => prs.into_iter().nth(i),
        None => prs.into_iter().next(),
    }
}

/// Gather a directory's whole git/PR picture. **Blocking** — several shell-outs
/// deep, so this belongs on a background thread and must never be called while
/// painting.
///
/// A detached HEAD short-circuits the PR and default-branch work: there is no
/// branch to ask GitHub about, and both queries are the expensive ones.
pub fn fetch(dir: &Path) -> GitContext {
    let mut ctx = GitContext::empty(dir);
    let Some(repo) = git::repo_display_name(dir) else {
        return ctx;
    };
    ctx.is_git = true;
    ctx.repo = Some(repo);
    ctx.dirty = git::dirty_stats(dir);
    ctx.branch = git::current_branch(dir);

    if let Some(branch) = ctx.branch.clone() {
        ctx.default_branch = git::default_remote_branch(dir);
        ctx.branch_diff = git::branch_stats(dir);
        match gh::pr_list_for_branch(dir, &branch) {
            Ok(prs) => ctx.pr = select_pr(prs),
            Err(err) => ctx.pr_error = Some(describe_pr_error(&err)),
        }
    }
    ctx.fetched_at = SystemTime::now();
    ctx
}

/// Whether two snapshots describe the same checkout — the same directory on the
/// same branch — and so whether the old one may still speak for the new one.
///
/// The cache is keyed by directory alone, so a snapshot's *branch* is the only
/// thing that says a pane is still doing the same work: run `git switch main`
/// in a pane and the next poll is about a different branch entirely, however
/// familiar its path. A detached HEAD (`branch == None`) only matches another
/// detached HEAD.
fn same_checkout(prev: &GitContext, next: &GitContext) -> bool {
    prev.cwd == next.cwd && prev.branch == next.branch
}

/// Fold a fresh snapshot onto the previous one, keeping what the fresh one
/// failed to learn.
///
/// This is h20's `mergeGitContext`, and it exists for one reason: a poll that
/// timed out or came back empty must not make a card blink and resize. When the
/// new snapshot is about the same checkout as the old one, anything it failed
/// to learn — the PR (lost, or replaced by a `pr_error`), the dirty counts —
/// is taken from the old one; everything else is the new one's.
///
/// Falling back to the old PR also clears `pr_error`: the card is about to
/// render a PR, so a stale-fetch complaint alongside it would be noise. The
/// error survives when there is nothing to fall back to, which is exactly when
/// a caller needs it.
///
/// Nothing carries across a branch change. Anti-blink must not become
/// anti-truth: a group that was on `feature-a` with PR #7 and is now on `main`
/// has no PR, and a card reading `repo ⎇ main` beside `PR #7 · approved` is a
/// lie that would never expire, since every later poll would restore it again.
/// The same goes for the dirty counts, which describe the branch that was
/// checked out when they were measured.
pub fn merge(prev: &GitContext, next: GitContext) -> GitContext {
    let mut merged = next;
    if !same_checkout(prev, &merged) {
        return merged;
    }
    let recovered = (merged.pr_error.is_some() || merged.pr.is_none()) && prev.pr.is_some();
    if recovered {
        merged.pr = prev.pr.clone();
        merged.pr_error = None;
    }
    // A `TIMEOUT_QUICK` miss on `git diff --shortstat` leaves this `None`;
    // without the fallback the card silently drops its diffstat and reflows
    // around the gap.
    if merged.dirty.is_none() {
        merged.dirty = prev.dirty;
    }
    if merged.branch_diff.is_none() {
        merged.branch_diff = prev.branch_diff;
    }
    merged
}

/// How long a snapshot counts as fresh before a refresh is worth spending
/// shell-outs on.
const FRESH_WINDOW: Duration = Duration::from_secs(5);

/// How many directories the cache remembers before it starts evicting.
const CACHE_CAP: usize = 256;

/// Stale-while-revalidate storage for [`GitContext`], keyed by directory.
///
/// Reads are cheap and never block, so the paint path can ask for a group's
/// context and render whatever was last known; refreshing is somebody else's
/// job, on a background thread, via [`needs_refresh`](Self::needs_refresh) and
/// [`insert`](Self::insert).
///
/// The cap matters: freshness expiry alone never removes anything, so a session
/// that visits a long tail of worktrees would grow the map forever. Eviction is
/// approximate LRU — entries are stamped on insert and the oldest stamp goes.
#[derive(Debug, Default)]
pub struct GitContextCache {
    entries: HashMap<PathBuf, CacheEntry>,
    /// Monotonic stamp source; also the approximation in "approximate LRU",
    /// since only writes bump it.
    tick: u64,
}

/// One cached snapshot plus the stamp eviction orders by.
#[derive(Debug)]
struct CacheEntry {
    ctx: GitContext,
    used: u64,
    /// Set when the world changed underneath us, to make the entry refresh
    /// early without throwing its last-known-good values away.
    stale: bool,
}

impl GitContextCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last-known-good snapshot for a directory, or `None` if we have never
    /// fetched one. Cheap and non-blocking: safe to call while painting.
    pub fn get(&self, dir: &Path) -> Option<&GitContext> {
        self.entries.get(dir).map(|e| &e.ctx)
    }

    /// Whether a background refresh is worth running — true for a directory we
    /// have never seen, for any snapshot older than the fresh window, and for
    /// any entry [`mark_all_stale`](Self::mark_all_stale) has flagged.
    pub fn needs_refresh(&self, dir: &Path) -> bool {
        match self.entries.get(dir) {
            Some(entry) => {
                entry.stale
                    || entry
                        .ctx
                        .fetched_at
                        .elapsed()
                        .map(|age| age >= FRESH_WINDOW)
                        .unwrap_or(true)
            },
            None => true,
        }
    }

    /// Store a freshly fetched snapshot, folded onto any existing one by
    /// [`merge`] so a failed poll does not throw away a known PR. Evicts the
    /// least recently written entry when the cache is full.
    pub fn insert(&mut self, dir: &Path, ctx: GitContext) {
        let ctx = match self.entries.get(dir) {
            Some(entry) => merge(&entry.ctx, ctx),
            None => ctx,
        };
        self.tick += 1;
        let used = self.tick;
        if !self.entries.contains_key(dir) {
            self.evict_to(CACHE_CAP - 1);
        }
        self.entries.insert(
            dir.to_path_buf(),
            CacheEntry {
                ctx,
                used,
                stale: false,
            },
        );
    }

    /// Mark every snapshot as due for a refresh while KEEPING its values.
    ///
    /// For the cases where the world changed underneath us — a PR cache frame,
    /// a commit, a config reload. Clearing the cache instead would blink every
    /// card back to its bare title until some later trigger repopulated it, and
    /// would leave [`merge`] without a `prev` to fall back on when the next
    /// poll times out. A staleness bump gets the re-fetch without the blink.
    pub fn mark_all_stale(&mut self) {
        for entry in self.entries.values_mut() {
            entry.stale = true;
        }
    }

    /// [`mark_all_stale`] for one directory: the next poll re-fetches it,
    /// its values staying on screen meanwhile. No-op for an unknown directory.
    ///
    /// [`mark_all_stale`]: Self::mark_all_stale
    pub fn mark_stale(&mut self, dir: &Path) {
        if let Some(entry) = self.entries.get_mut(dir) {
            entry.stale = true;
        }
    }

    /// How many snapshots are held. Mostly here so the bound is testable.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop oldest-stamped entries until at most `target` remain.
    fn evict_to(&mut self, target: usize) {
        while self.entries.len() > target {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// Phrase a PR lookup failure for the card, keeping the three failure modes the
/// user reacts to differently apart: an auth problem they can fix, a missing
/// CLI they can install, and a timeout that simply retries.
fn describe_pr_error(err: &str) -> String {
    if gh::is_auth_failure(err) {
        format!("{} sign-in required", gh::cli())
    } else {
        err.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::Check;

    /// A plain open PR with nothing decided about it yet.
    fn pr(state: &str) -> PrSummary {
        PrSummary {
            number: 7,
            title: "wire up the sidebar".into(),
            state: state.into(),
            is_draft: false,
            head: "feature".into(),
            author: "twhitehurst".into(),
            review_decision: None,
            mergeable: None,
            checks: Vec::new(),
            url: String::new(),
        }
    }

    fn check(status: CheckStatus) -> Check {
        Check {
            name: "ci".into(),
            status,
            url: String::new(),
        }
    }

    fn ctx(dir: &str) -> GitContext {
        GitContext::empty(Path::new(dir))
    }

    #[test]
    fn rollup_without_a_pull_request_is_none() {
        assert_eq!(derive_rollup(None), PrRollup::None);
    }

    #[test]
    fn rollup_of_a_closed_or_merged_pr_is_finished() {
        assert_eq!(derive_rollup(Some(&pr("merged"))), PrRollup::Finished);
        assert_eq!(derive_rollup(Some(&pr("closed"))), PrRollup::Finished);

        // Finished outranks everything below it — a merged PR says nothing else.
        let mut p = pr("merged");
        p.mergeable = Some("CONFLICTING".into());
        p.checks = vec![check(CheckStatus::Failure)];
        assert_eq!(derive_rollup(Some(&p)), PrRollup::Finished);
    }

    #[test]
    fn conflicts_outrank_failing_checks() {
        let mut p = pr("open");
        p.mergeable = Some("CONFLICTING".into());
        p.checks = vec![check(CheckStatus::Failure)];
        assert_eq!(derive_rollup(Some(&p)), PrRollup::MergeConflicts);
    }

    #[test]
    fn a_single_failing_check_wins_over_the_review_verdict() {
        let mut p = pr("open");
        p.review_decision = Some("APPROVED".into());
        p.mergeable = Some("MERGEABLE".into());
        p.checks = vec![check(CheckStatus::Success), check(CheckStatus::Failure)];
        assert_eq!(derive_rollup(Some(&p)), PrRollup::ChecksFailing);
    }

    #[test]
    fn draft_loses_to_failing_checks_and_wins_over_everything_else() {
        // The ladder puts ChecksFailing above Draft: a broken draft still needs
        // fixing, and that is the actionable thing to show.
        let mut failing = pr("open");
        failing.is_draft = true;
        failing.checks = vec![check(CheckStatus::Failure)];
        assert_eq!(derive_rollup(Some(&failing)), PrRollup::ChecksFailing);

        let mut pending = pr("open");
        pending.is_draft = true;
        pending.review_decision = Some("APPROVED".into());
        pending.checks = vec![check(CheckStatus::Pending)];
        assert_eq!(derive_rollup(Some(&pending)), PrRollup::Draft);
    }

    #[test]
    fn approved_is_ready_to_merge_only_when_mergeable_and_checks_settled() {
        let mut ready = pr("open");
        ready.review_decision = Some("APPROVED".into());
        ready.mergeable = Some("MERGEABLE".into());
        ready.checks = vec![check(CheckStatus::Success)];
        assert_eq!(derive_rollup(Some(&ready)), PrRollup::OpenReadyToMerge);

        // Approved but checks still running: not clear to merge yet.
        let mut running = ready.clone();
        running.checks = vec![check(CheckStatus::Pending)];
        assert_eq!(derive_rollup(Some(&running)), PrRollup::OpenApproved);

        // Approved but GitHub has not called it mergeable.
        let mut unknown = ready.clone();
        unknown.mergeable = Some("UNKNOWN".into());
        assert_eq!(derive_rollup(Some(&unknown)), PrRollup::OpenApproved);
    }

    #[test]
    fn comments_and_change_requests_roll_up_the_same_way() {
        let mut commented = pr("open");
        commented.review_decision = Some("COMMENTED".into());
        assert_eq!(derive_rollup(Some(&commented)), PrRollup::OpenCommented);

        let mut changes = pr("open");
        changes.review_decision = Some("CHANGES_REQUESTED".into());
        assert_eq!(derive_rollup(Some(&changes)), PrRollup::OpenCommented);
    }

    #[test]
    fn review_required_outranks_pending_checks() {
        let mut p = pr("open");
        p.review_decision = Some("REVIEW_REQUIRED".into());
        p.checks = vec![check(CheckStatus::Pending)];
        // The reviewer is the blocker, not CI.
        assert_eq!(derive_rollup(Some(&p)), PrRollup::OpenNeedsReview);
    }

    #[test]
    fn no_verdict_falls_back_to_the_check_state() {
        let mut pending = pr("open");
        pending.checks = vec![check(CheckStatus::Success), check(CheckStatus::Pending)];
        assert_eq!(derive_rollup(Some(&pending)), PrRollup::ChecksPending);

        // No verdict and nothing running: just an open PR.
        assert_eq!(derive_rollup(Some(&pr("open"))), PrRollup::OpenPending);

        let mut green = pr("open");
        green.checks = vec![check(CheckStatus::Success)];
        assert_eq!(derive_rollup(Some(&green)), PrRollup::OpenPending);
    }

    #[test]
    fn select_pr_prefers_an_open_pr_over_a_newer_merged_one() {
        let mut merged = pr("merged");
        merged.number = 12;
        let mut open = pr("open");
        open.number = 3;
        let picked = select_pr(vec![merged, open]).expect("a pr");
        assert_eq!(picked.number, 3);
    }

    #[test]
    fn select_pr_falls_back_to_the_first_result() {
        let mut newest = pr("merged");
        newest.number = 12;
        let mut older = pr("closed");
        older.number = 4;
        let picked = select_pr(vec![newest, older]).expect("a pr");
        assert_eq!(picked.number, 12);
    }

    #[test]
    fn select_pr_of_nothing_is_none() {
        assert!(select_pr(Vec::new()).is_none());
    }

    #[test]
    fn merge_keeps_the_known_pr_when_the_poll_errored() {
        let mut prev = ctx("/repo");
        prev.pr = Some(pr("open"));
        let mut next = ctx("/repo");
        next.pr_error = Some("gh timed out".into());
        let merged = merge(&prev, next);
        assert_eq!(merged.pr.as_ref().map(|p| p.number), Some(7));
        // The card is about to render PR #7, so the fetch complaint is noise.
        assert_eq!(merged.pr_error, None);
    }

    #[test]
    fn merge_keeps_the_error_when_there_is_nothing_to_fall_back_to() {
        let prev = ctx("/repo");
        let mut next = ctx("/repo");
        next.pr_error = Some("gh timed out".into());
        let merged = merge(&prev, next);
        assert_eq!(merged.pr, None);
        assert_eq!(merged.pr_error.as_deref(), Some("gh timed out"));
    }

    #[test]
    fn merge_keeps_the_known_pr_when_the_poll_came_back_empty() {
        let mut prev = ctx("/repo");
        prev.branch = Some("feature".into());
        prev.pr = Some(pr("open"));
        let mut next = ctx("/repo");
        next.branch = Some("feature".into());
        let merged = merge(&prev, next);
        assert_eq!(merged.pr.as_ref().map(|p| p.number), Some(7));
        // Everything else still comes from the fresh snapshot.
        assert_eq!(merged.branch.as_deref(), Some("feature"));
    }

    #[test]
    fn merge_drops_the_pr_when_the_branch_changed() {
        // A group on `feature-a` with PR #7; the user runs `git switch main`
        // in that pane and the next poll finds no PR for `main`.
        let mut prev = ctx("/repo");
        prev.branch = Some("feature-a".into());
        prev.pr = Some(pr("open"));
        let mut next = ctx("/repo");
        next.branch = Some("main".into());

        let merged = merge(&prev, next);
        assert_eq!(
            merged.pr, None,
            "PR #7 belongs to feature-a; the card must not show it beside main"
        );
        assert_eq!(merged.branch.as_deref(), Some("main"));
    }

    #[test]
    fn merge_keeps_a_new_branchs_pr_error_instead_of_the_old_branchs_pr() {
        let mut prev = ctx("/repo");
        prev.branch = Some("feature-a".into());
        prev.pr = Some(pr("open"));
        let mut next = ctx("/repo");
        next.branch = Some("main".into());
        next.pr_error = Some("gh timed out".into());

        let merged = merge(&prev, next);
        assert_eq!(merged.pr, None);
        // Nothing to fall back to on this branch, so the complaint stands.
        assert_eq!(merged.pr_error.as_deref(), Some("gh timed out"));
    }

    #[test]
    fn merge_keeps_the_diffstat_the_poll_missed() {
        let stats = DirtyStats {
            files: 3,
            insertions: 42,
            deletions: 7,
        };
        let mut prev = ctx("/repo");
        prev.branch = Some("feature-a".into());
        prev.dirty = Some(stats);

        let mut next = ctx("/repo");
        next.branch = Some("feature-a".into());
        // `git diff --shortstat` missed its deadline.
        next.dirty = None;

        let merged = merge(&prev, next);
        assert_eq!(
            merged.dirty,
            Some(stats),
            "the card keeps its diffstat instead of reflowing around the gap"
        );
    }

    #[test]
    fn merge_prefers_what_the_new_snapshot_did_learn() {
        let mut prev = ctx("/repo");
        prev.branch = Some("feature-a".into());
        prev.dirty = Some(DirtyStats {
            files: 3,
            insertions: 42,
            deletions: 7,
        });

        let mut next = ctx("/repo");
        next.branch = Some("feature-a".into());
        // A commit landed: the tree is clean now, and zeroes are knowledge.
        next.dirty = Some(DirtyStats::default());

        let merged = merge(&prev, next);
        assert_eq!(merged.dirty, Some(DirtyStats::default()));
    }

    #[test]
    fn merge_carries_no_diffstat_across_a_branch_change() {
        let mut prev = ctx("/repo");
        prev.branch = Some("feature-a".into());
        prev.dirty = Some(DirtyStats {
            files: 1,
            insertions: 1,
            deletions: 0,
        });

        let mut next = ctx("/repo");
        next.branch = Some("main".into());

        let merged = merge(&prev, next);
        assert_eq!(
            merged.dirty, None,
            "feature-a's uncommitted work says nothing about main"
        );
    }

    #[test]
    fn merge_takes_a_genuinely_new_pr() {
        let mut prev = ctx("/repo");
        prev.pr = Some(pr("open"));
        let mut next = ctx("/repo");
        let mut fresh = pr("open");
        fresh.number = 99;
        next.pr = Some(fresh);
        let merged = merge(&prev, next);
        assert_eq!(merged.pr.as_ref().map(|p| p.number), Some(99));
    }

    #[test]
    fn needs_refresh_honors_the_fresh_window() {
        let mut cache = GitContextCache::new();
        let dir = Path::new("/repo");
        assert!(
            cache.needs_refresh(dir),
            "an unseen directory always refreshes"
        );

        cache.insert(dir, ctx("/repo"));
        assert!(
            !cache.needs_refresh(dir),
            "a just-fetched snapshot is fresh"
        );
        assert!(cache.get(dir).is_some());

        let mut stale = ctx("/repo");
        stale.fetched_at = SystemTime::now() - Duration::from_secs(6);
        cache.insert(dir, stale);
        assert!(
            cache.needs_refresh(dir),
            "past the window it is worth re-fetching"
        );
    }

    #[test]
    fn marking_stale_re_fetches_without_losing_what_we_knew() {
        let mut cache = GitContextCache::new();
        let mut a = ctx("/a");
        a.branch = Some("main".into());
        a.pr = Some(pr("open"));
        cache.insert(Path::new("/a"), a);
        cache.insert(Path::new("/b"), ctx("/b"));
        assert!(!cache.needs_refresh(Path::new("/a")), "just fetched");

        cache.mark_all_stale();
        assert_eq!(cache.len(), 2, "a staleness bump forgets nothing");
        assert!(cache.needs_refresh(Path::new("/a")));
        assert!(cache.needs_refresh(Path::new("/b")));
        let kept = cache.get(Path::new("/a")).expect("snapshot survives");
        assert_eq!(kept.branch.as_deref(), Some("main"));
        assert!(
            kept.pr.is_some(),
            "the card keeps rendering its PR while the refresh runs"
        );
    }

    #[test]
    fn a_completed_refresh_clears_the_staleness_bump() {
        let mut cache = GitContextCache::new();
        let dir = Path::new("/a");
        let mut first = ctx("/a");
        first.branch = Some("main".into());
        cache.insert(dir, first);
        cache.mark_all_stale();
        assert!(cache.needs_refresh(dir));

        let mut fresh = ctx("/a");
        fresh.branch = Some("main".into());
        cache.insert(dir, fresh);
        assert!(
            !cache.needs_refresh(dir),
            "the poll the bump asked for satisfies it"
        );
    }

    #[test]
    fn the_cache_stays_bounded_as_worktrees_come_and_go() {
        let mut cache = GitContextCache::new();
        for i in 0..(CACHE_CAP * 2) {
            let dir = PathBuf::from(format!("/tmp/worktree-{i}"));
            cache.insert(&dir, ctx(dir.to_str().unwrap()));
            assert!(cache.len() <= CACHE_CAP);
        }
        assert_eq!(cache.len(), CACHE_CAP);
        // Approximate LRU: the newest write survives, the very first is gone.
        assert!(cache.get(Path::new("/tmp/worktree-0")).is_none());
        let last = format!("/tmp/worktree-{}", CACHE_CAP * 2 - 1);
        assert!(cache.get(Path::new(&last)).is_some());
    }

    #[test]
    fn re_inserting_a_known_directory_does_not_evict() {
        let mut cache = GitContextCache::new();
        for i in 0..CACHE_CAP {
            let dir = PathBuf::from(format!("/tmp/w-{i}"));
            cache.insert(&dir, ctx(dir.to_str().unwrap()));
        }
        assert_eq!(cache.len(), CACHE_CAP);
        cache.insert(Path::new("/tmp/w-0"), ctx("/tmp/w-0"));
        assert_eq!(cache.len(), CACHE_CAP);
        assert!(cache.get(Path::new("/tmp/w-0")).is_some());
    }
}
