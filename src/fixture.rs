//! Deterministic workspaces for the web build, screenshots, and tests: groups,
//! splits, and tabs whose sessions are fed recorded terminal output
//! (`src/fixtures/*.vt`, raw VT byte streams) instead of a shell. On wasm32
//! `Session::new` has no PTY, so this is the only way content reaches a grid
//! there; on native the same seeding works on top of real shells.

use std::path::PathBuf;

use crate::app::App;
use crate::cleanup::{PrInfo, WorktreeInfo};
use crate::gh::{PrDetail, PrSummary};
use crate::pr_ui::{LocalDiffRender, build_diff_render};
use crate::git::DirtyStats;
use crate::git_context::GitContext;
use crate::term::TermEvent;
use crate::workspace::Dir;

/// A recorded transcript: what a pane shows, plus the tab title the program
/// inside would have set.
pub struct Transcript {
    pub title: &'static str,
    pub bytes: &'static [u8],
}

/// An interactive shell: `ls`, `git status`, a test run.
pub const SHELL: Transcript =
    Transcript { title: "zsh", bytes: include_bytes!("fixtures/shell.vt") };
/// A Claude Code session mid-task.
pub const CLAUDE: Transcript =
    Transcript { title: "claude", bytes: include_bytes!("fixtures/claude.vt") };
/// A release build with one warning.
pub const BUILD: Transcript =
    Transcript { title: "cargo", bytes: include_bytes!("fixtures/build.vt") };
/// A unified diff (two files, one new) for the PR "Files changed" tab and the
/// local-diff tool.
pub const DIFF: &str = include_str!("fixtures/pr.diff");

/// What the sidebar card knows about a group's checkout — the snapshot the
/// git-context worker would have taken, minus the shell-outs.
pub struct Repo {
    pub branch: &'static str,
    /// (files, insertions, deletions) uncommitted; `(0, 0, 0)` is clean.
    pub dirty: (u32, u32, u32),
    /// The same, for the branch's committed work against the base branch.
    pub branch_diff: (u32, u32, u32),
    /// An open pull request for the branch: (number, title, draft).
    pub pr: Option<(u32, &'static str, bool)>,
}

/// One group of the demo layout: its sidebar name, where it lives (the
/// directory the card keys its git context on; it need not exist), the
/// checkout to describe, and the transcripts of its tiles in split order (the
/// first tile is the founding, primary one).
pub struct Group {
    pub name: &'static str,
    pub cwd: &'static str,
    pub repo: Repo,
    pub tiles: &'static [&'static [Transcript]],
}

/// The demo workspace: a pwrde group split side-by-side (shell | claude, with
/// a build tab behind the shell) on a branch with a draft PR, and a clean rcn
/// group holding one shell.
pub const DEMO: &[Group] = &[
    Group {
        name: "pwrde",
        cwd: "/Users/tyler/src/pwrde",
        repo: Repo {
            branch: "navis/web-build",
            dirty: (3, 41, 7),
            branch_diff: (36, 1057, 553),
            pr: Some((77, "Build pwrde for wasm32 so headless Chromium can screenshot the UI", true)),
        },
        tiles: &[&[SHELL, BUILD], &[CLAUDE]],
    },
    Group {
        name: "rcn",
        cwd: "/Users/tyler/src/rcn",
        repo: Repo { branch: "main", dirty: (0, 0, 0), branch_diff: (0, 0, 0), pr: None },
        tiles: &[&[SHELL]],
    },
];

fn stats((files, insertions, deletions): (u32, u32, u32)) -> DirtyStats {
    DirtyStats { files, insertions, deletions }
}

/// The git context a group's card renders, as the worker would have reported
/// it for `cwd`.
fn git_context(cwd: &std::path::Path, name: &str, repo: &Repo) -> GitContext {
    GitContext {
        is_git: true,
        cwd: cwd.to_path_buf(),
        repo: Some(name.to_string()),
        branch: Some(repo.branch.to_string()),
        default_branch: Some("origin/main".to_string()),
        dirty: Some(stats(repo.dirty)),
        branch_diff: Some(stats(repo.branch_diff)),
        pr: repo.pr.map(|(number, title, is_draft)| pr_summary(number, title, is_draft, repo.branch)),
        pr_error: None,
        fetched_at: web_time::SystemTime::now(),
    }
}

fn pr_summary(number: u32, title: &str, is_draft: bool, head: &str) -> PrSummary {
    PrSummary {
        number,
        title: title.to_string(),
        state: "OPEN".to_string(),
        is_draft,
        head: head.to_string(),
        author: "a1re1".to_string(),
        review_decision: None,
        mergeable: Some("MERGEABLE".to_string()),
        checks: Vec::new(),
    }
}

/// What the PR and Cleanup workers would have answered for the demo groups:
/// the repo's open PRs (the branch-scoped list and the page-wide one), the
/// detail of the branch's own PR, and a `drop` worktree scan. These go through
/// [`crate::bg::answer_requests_with`] on wasm32, where no worker can run.
fn worker_answers(groups: &[Group]) -> Vec<TermEvent> {
    let Some(first) = groups.first() else { return Vec::new() };
    let branch_prs: Vec<PrSummary> = first
        .repo
        .pr
        .map(|(n, t, d)| vec![pr_summary(n, t, d, first.repo.branch)])
        .unwrap_or_default();
    let mut all_prs = branch_prs.clone();
    all_prs.push(pr_summary(76, "Unify the pickers into one rcn command palette", false, "navis/palette-session-picker"));
    all_prs.push(pr_summary(74, "Sidebar status doc", false, "rcn/17-status-doc"));
    let mut events = vec![
        TermEvent::PrListLoaded { all: false, result: Ok(branch_prs.clone()) },
        TermEvent::PrListLoaded { all: true, result: Ok(all_prs) },
    ];
    if let Some(pr) = branch_prs.first() {
        events.push(TermEvent::PrDetailLoaded {
            number: pr.number,
            result: Ok(PrDetail {
                number: pr.number,
                title: pr.title.clone(),
                body: "## Goal\n\nMake pwrde's UI renderable in a browser so headless Chrome can \
                       screenshot and drive it.\n\n## Testing\n\n- `cargo test`: 404 passed\n- wasm32 \
                       check clean\n- captures verified for Sessions, Settings, dark mode"
                    .to_string(),
                state: "OPEN".to_string(),
                is_draft: pr.is_draft,
                author: pr.author.clone(),
                head: pr.head.clone(),
                base: "main".to_string(),
                additions: first.repo.branch_diff.1,
                deletions: first.repo.branch_diff.2,
                changed_files: first.repo.branch_diff.0,
                review_decision: None,
                mergeable: Some("MERGEABLE".to_string()),
                url: format!("https://github.com/worldbreakstudios/pwrde/pull/{}", pr.number),
                created_at: "2026-08-29T15:31:00Z".to_string(),
                ..Default::default()
            }),
        });
    }
    // The diff is highlighted for the polarity in effect when seeded, the
    // way the worker highlights for the polarity in effect when it runs.
    let render = std::sync::Arc::new(build_diff_render(
        &crate::diff::parse(DIFF),
        crate::theme::dark_active(),
    ));
    if let Some(pr) = branch_prs.first() {
        events.push(TermEvent::PrDiffLoaded { number: pr.number, result: Ok(render.clone()) });
    }
    events.push(TermEvent::LocalDiffLoaded(Ok(LocalDiffRender {
        base_ref: "origin/main".to_string(),
        render,
    })));
    let worktrees = groups
        .iter()
        .enumerate()
        .map(|(i, g)| WorktreeInfo {
            repo_root: format!("/Users/tyler/src/{}", g.name),
            path: g.cwd.to_string(),
            id: format!("{}-{}", g.name, i + 1),
            branch: Some(g.repo.branch.to_string()),
            head: format!("{:07x}", 0x1d4b156 + i as u32 * 0x1111),
            dirty_count: g.repo.dirty.0,
            merged: g.repo.branch == "main",
            ahead: if g.repo.branch == "main" { 0 } else { 2 },
            behind: 0,
            last_activity_ms: 1_787_000_000_000.0 - i as f64 * 86_400_000.0,
            is_current: i == 0,
            pr: g.repo.pr.map(|(number, title, draft)| PrInfo {
                number,
                state: if draft { "draft" } else { "open" }.to_string(),
                title: title.to_string(),
            }),
            dirty_files: Vec::new(),
        })
        .collect();
    events.push(TermEvent::CleanupScanned(worktrees));
    events
}

/// Replace the empty state with `groups`, feeding every tab its transcript.
/// The first group ends up active. Call once the window exists, so the grids
/// get their real size from `sync_layout` before the bytes land.
pub fn seed(app: &mut App, groups: &[Group]) {
    for group in groups {
        let cwd = PathBuf::from(group.cwd);
        app.add_group(group.name.to_string(), Some(cwd.clone()));
        // The card reads its branch/diffstat/PR from the git-context cache,
        // keyed by the group's cwd; this is the snapshot the worker would
        // have produced (and on wasm never will).
        app.git_contexts.insert(&cwd, git_context(&cwd, group.name, &group.repo));
        // The checkout probe and the git commands the New-session flow runs
        // (default branch, branch list, worktrees) answer from here on wasm32.
        #[cfg(target_family = "wasm")]
        crate::git::canned::register(cwd.clone(), git_answers(&cwd, group));
        // `add_group` queues the primary command (`claude` by default) for the
        // founding pane's first wakeup. A transcript already shows a program
        // running, so nothing should be typed over it.
        app.pending_primary_cmd.clear();
        // `add_group` founds the tile with one tab; every further tile is a
        // side-by-side split of the focused one, every further transcript in
        // a tile is another tab. Splits and tabs both spawn sessions.
        for (ti, tile) in group.tiles.iter().enumerate() {
            if ti > 0 {
                app.split(Dir::Row);
            }
            for _ in 1..tile.len() {
                app.new_tab();
            }
        }
        app.sync_layout();
        let ws = &mut app.workspaces[app.active];
        for (tile, transcripts) in ws.root.tiles_mut().into_iter().zip(group.tiles.iter()) {
            for (tab, transcript) in tile.tabs.iter_mut().zip(transcripts.iter()) {
                tab.session.feed(transcript.bytes);
                // Programs name their tab via OSC 0/2; the transcripts carry
                // that where the program would have sent it, and this is the
                // process-derived fallback the `ps` sweep would have found.
                tab.session.set_proc_title(Some(transcript.title.to_string()));
            }
            // Show the first tab, as a user would after opening the pane.
            tile.active = 0;
        }
    }
    app.switch_workspace(0);
    crate::bg::answer_requests_with(app.events_tx.clone(), worker_answers(groups));
    #[cfg(target_family = "wasm")]
    seed_storage(groups);
    app.request_redraw();
}

/// stdout of the git commands `build_fork_choices` runs in `cwd`.
#[cfg(target_family = "wasm")]
fn git_answers(cwd: &std::path::Path, group: &Group) -> Vec<(String, String)> {
    use crate::git::canned::key;
    let branch = group.repo.branch;
    let mut branches = String::from("*\t");
    branches.push_str(branch);
    branches.push('\n');
    if branch != "main" {
        branches.push_str(" \tmain\n");
    }
    branches.push_str(" \tremotes/origin/HEAD -> origin/main\n \tremotes/origin/main\n");
    if branch != "main" {
        branches.push_str(&format!(" \tremotes/origin/{branch}\n"));
    }
    let mut worktrees = format!("worktree {}\nHEAD 1d4b1560000\nbranch refs/heads/{branch}\n\n", cwd.display());
    if group.name == "pwrde" {
        worktrees.push_str(&format!(
            "worktree {}/.worktrees/60f3fbe1\nHEAD c556a0e0000\nbranch refs/heads/navis/palette-session-picker\n\n",
            cwd.display()
        ));
    }
    vec![
        (key(cwd, "git", &["symbolic-ref", "refs/remotes/origin/HEAD"]), "refs/remotes/origin/main\n".to_string()),
        (key(cwd, "git", &["branch", "-a", "--format=%(HEAD)\t%(refname:short)"]), branches),
        (key(cwd, "git", &["worktree", "list", "--porcelain"]), worktrees),
    ]
}

/// What the user's files would hold: the picker's pins and recents (the New
/// session flow), and a notes vault with a few docs. Only on wasm32, where
/// the storage seam is the page's `localStorage` — natively this would write
/// into the real home directory.
#[cfg(target_family = "wasm")]
fn seed_storage(groups: &[Group]) {
    let dirs: Vec<PathBuf> = groups.iter().map(|g| PathBuf::from(g.cwd)).collect();
    let store = crate::picker::PickerStore {
        pinned: dirs.iter().take(1).cloned().collect(),
        recents: dirs.iter().rev().cloned().collect(),
    };
    store.save();

    let vault = PathBuf::from("/Users/tyler/notes");
    for (rel, text) in NOTES {
        let _ = crate::notes::write_doc(&vault.join(rel), text);
    }
    if !crate::notes::vaults().contains(&vault) {
        crate::notes::add_vault(vault);
    }
}

/// The demo vault's docs, vault-relative.
#[cfg(target_family = "wasm")]
const NOTES: &[(&str, &str)] = &[
    ("web build.md", "# Web build\n\nThe browser runs the same `App`; only the platform seams differ.\n\n- PTY → `EchoShell`\n- threads → `bg::spawn`\n- files → `storage`\n"),
    ("parity/checklist.md", "# Parity checklist\n\n- [x] sidebar cards\n- [x] PR page\n- [x] cleanup scan\n- [ ] drag & drop capture\n"),
    ("ideas.md", "# Ideas\n\n> Screenshots instead of screen recordings.\n\nRecord a `.vt` per scenario and diff the captures in CI.\n"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Session;

    fn visible_text(session: &Session) -> String {
        let term = session.term.lock().unwrap();
        let screen = term.screen();
        screen
            .lines_in_phys_range(0..screen.physical_rows)
            .iter()
            .map(|l| l.as_str().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn transcripts_land_on_the_grid() {
        for t in [SHELL, CLAUDE, BUILD] {
            let session = Session::placeholder();
            session.feed(t.bytes);
            assert!(!visible_text(&session).trim().is_empty(), "{} rendered nothing", t.title);
        }
        let session = Session::placeholder();
        session.feed(SHELL.bytes);
        let text = visible_text(&session);
        assert!(text.contains("git status --short"), "{text}");
        assert!(text.contains("402 passed"), "{text}");
    }

    #[test]
    fn demo_layout_is_well_formed() {
        assert_eq!(DEMO[0].tiles.len(), 2);
        assert!(DEMO.iter().all(|g| g.tiles.iter().all(|t| !t.is_empty())));
    }
}
