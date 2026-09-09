//! GitHub pull-request summaries, shelled out through a configurable CLI.
//!
//! h20 proved the model: talk to GitHub through `lfg` (a local-first `gh`
//! cache) rather than `gh` directly, so cached data paints instantly and a
//! background refresh streams fresh data over SSE (`lfg.rs` tails it). pwrde
//! keeps that model but makes the CLI swappable — the `git.cli` setting picks
//! the binary (default `lfg`, or plain `gh`, or any drop-in with the same
//! `pr list --json` contract), and `git.async` toggles the `-A/--force-async`
//! fast path (only meaningful for `lfg`; plain `gh` blocks synchronously).
//!
//! Reads return normalized, gpui-free structs so the sidebar card rollups
//! (`git_context.rs`), the ⇧⌘G open-in-browser action and the tests don't
//! care which CLI produced them.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;


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

/// Run the CLI under `limit`, killing it when the deadline passes.
///
/// Used by the reads a background poller drives: a wedged daemon or a stalled
/// network call must never pin that thread forever. The three failure modes
/// stay distinguishable in the message — `is not installed` (ENOENT), `timed
/// out` (deadline), and the CLI's own stderr for everything else (auth, no
/// remote), which `is_auth_failure` can recognize.
fn run_within(mut cmd: Command, limit: std::time::Duration) -> Result<String, String> {
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
    /// The PR's web page, what ⇧⌘G (`Action::OpenPrInGithub`) opens.
    pub url: String,
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
    #[serde(default)]
    url: String,
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

const LIST_FIELDS: &str =
    "number,title,state,isDraft,headRefName,author,reviewDecision,mergeable,statusCheckRollup,url";

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
            url: r.url,
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

/// List the PR(s) whose head is `branch` (any state), for the branch-scoped PR
/// tool. Usually one; capped small.
pub fn pr_list_for_branch(dir: &Path, branch: &str) -> Result<Vec<PrSummary>, String> {
    fetch_list(
        dir,
        &["pr", "list", "--head", branch, "--state", "all", "--limit", "10", "--json", LIST_FIELDS],
    )
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
            "url":"https://github.com/o/r/pull/7",
            "mergeable":"MERGEABLE","statusCheckRollup":[
                {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"u"},
                {"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","detailsUrl":"v"}]}]"#;
        let prs = parse_summaries(json).unwrap();
        assert_eq!(prs[0].review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(prs[0].mergeable.as_deref(), Some("MERGEABLE"));
        assert_eq!(prs[0].url, "https://github.com/o/r/pull/7");
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
        // No `url` → "" so ⇧⌘G degrades to a no-op instead of failing to parse.
        assert_eq!(prs[0].url, "");
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
}
