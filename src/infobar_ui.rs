//! The primary pane's info bar as a gpui element tree over the canvas: the
//! 48px row under the pane's title row (`tile_ui::primary_title_row`),
//! modelled on the webview toolbar (`webview_ui`) and in its pill language —
//! 32px-high full pills of the strip ink at .07, with `tile_ui::StripStyle`'s
//! inks so a light terminal palette stays legible. Every size here is the
//! figure at the default chrome text size and scales by
//! [`crate::workspace::chrome_ui_scale`].
//!
//! Left to right, on one clipped line (8px 10px padding, 8px gaps):
//!
//! - the **repo/branch pill** (only in a git checkout): a branch glyph,
//!   `repo / branch` (the short `HEAD` SHA when detached), then 11.5px counts —
//!   the committed branch diff as `+A −R`, the way the sidebar rows show it
//!   (`GitContext` carries no commits-ahead count), and `● N` in amber for `N`
//!   uncommitted files (the dot 3px clear of its count);
//! - the **cwd pill**, centred in the space that is left and capped at 420px:
//!   a folder glyph and the group's directory with `$HOME` as `~`, truncated
//!   from the left so the tail stays readable; a click copies the whole
//!   `~`-abbreviated directory to the clipboard and the pill reads "Copied"
//!   in the status green for half a second;
//! - the **PR pill** (only when the branch has a pull request): the state
//!   glyph in the sidebar's PR colours, `#<number>`, a dim state word and the
//!   checks rollup as `✓ p/t`; a click runs `Action::OpenPrInGithub`'s path;
//! - the **unread chip**: a round pill around an 8px dot — green with a soft
//!   ring while the pane's tab is unread, faint otherwise.
//!
//! A narrow bar sheds parts in order ([`visible_parts`]): the counts, then the
//! PR pill, then the unread chip.
//!
//! The bar's rect is the pure [`crate::workspace::primary_info_bar`], which
//! the canvas mouse path in `main.rs` reads too: a press anywhere on the bar
//! focuses the primary pane and never reaches the terminal. The data is the
//! `App::git_contexts` snapshot keyed by the group's cwd — exactly what the
//! sidebar row reads, refreshed by the same `spawn_git_context_refresh` walk
//! (which covers every group), so the bar never fetches on its own.

use std::time::{Duration, Instant};


use gpui::{
    AnyElement, App as GpuiApp, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use crate::App;
use crate::gh::{Check, CheckStatus, PrSummary, aggregate_check_status};
use crate::git_context::{GitContext, derive_rollup};
use crate::sidebar_card::{CardAvatar, avatar_for};
use crate::tile_ui::StripStyle;
use crate::ui::assets::{ICON_FOLDER, ICON_GIT_BRANCH};
use crate::ui::icon;
use crate::workspace;

/// The pills — the webview toolbar's: 32px high, fully rounded, the strip ink
/// at .07 — with 12px of side padding and 6px between a pill's children.
const PILL_H: f32 = 32.0;
const PILL_FILL: f32 = 0.07;
const PILL_PAD_X: f32 = 12.0;
const PILL_GAP: f32 = 6.0;
/// The bar's side padding and the gap between its pills.
const BAR_PAD_X: f32 = 10.0;
const BAR_GAP: f32 = 8.0;
/// A pill's leading glyph.
const GLYPH: f32 = 13.0;
/// The pills' type, and the smaller size of the counts.
const TEXT_SIZE: f32 = 12.5;
const COUNT_SIZE: f32 = 11.5;
/// The uncommitted dot, and the gap between it and its count.
const DIRTY_DOT: &str = "●";
const DIRTY_GAP: f32 = 3.0;
/// The cwd pill's cap.
const CWD_MAX_W: f32 = 420.0;
/// Advance of one character as a share of the type size — the chrome face is
/// monospace (0.6em), so a pill's width can be estimated without measuring.
const CHAR_EM: f32 = 0.6;
/// The fewest characters the cwd keeps, however narrow the bar.
const CWD_MIN_CHARS: usize = 4;
/// The unread dot, and the soft ring around it while unread.
const DOT: f32 = 8.0;
const DOT_RING: f32 = 3.0;
const DOT_RING_ALPHA: f32 = 0.18;
/// Bar widths (at the default chrome text size) under which parts drop out.
const HIDE_COUNTS_BELOW: f32 = 620.0;
const HIDE_PR_BELOW: f32 = 540.0;
const HIDE_UNREAD_BELOW: f32 = 400.0;
/// The least the cwd pill keeps before the optional parts give way to it.
const CWD_MIN_W: f32 = 120.0;
/// The most of the bar the repo pill may take; a longer branch is cut.
const REPO_MAX_SHARE: f32 = 0.45;
/// The uncommitted amber: the mock's on dark, a darker one that still reads
/// on a light pill.
/// How long the cwd pill reads "Copied" after a click.
const COPIED_FLASH: Duration = Duration::from_millis(500);

/// True on the frame the "Copied" flash passes its deadline: clears it
/// exactly once, then stays false until the next copy.
pub(crate) fn flash_due(until: &mut Option<Instant>, now: Instant) -> bool {
    if until.is_some_and(|deadline| now >= deadline) {
        *until = None;
        return true;
    }
    false
}

fn amber(dark: bool) -> Hsla {
    gpui::rgb(if dark { 0xe3b341 } else { 0x9a6700 }).into()
}

/// Which optional parts a bar `bar_w` logical px wide shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parts {
    /// The counts inside the repo pill.
    pub counts: bool,
    /// The PR pill.
    pub pr: bool,
    /// The unread chip.
    pub unread: bool,
}

/// The width collapse: below 620px the repo pill drops its counts, below 540
/// the PR pill goes too, below 400 the unread chip as well. The thresholds
/// are at the default chrome text size and scale by `ui` like the pills do.
pub(crate) fn visible_parts(bar_w: f32, ui: f32) -> Parts {
    Parts {
        counts: bar_w >= HIDE_COUNTS_BELOW * ui,
        pr: bar_w >= HIDE_PR_BELOW * ui,
        unread: bar_w >= HIDE_UNREAD_BELOW * ui,
    }
}

/// [`visible_parts`] narrowed by content: while the pills beside the cwd
/// would leave it less than [`CWD_MIN_W`], the counts, then the PR pill, then
/// the unread chip drop out. Widths are estimates in px at the default chrome
/// text size — `repo` without its counts, `counts` their extra, `pr` the PR
/// pill (0 when there is none).
fn fit_parts(mut parts: Parts, bar_w: f32, repo: f32, counts: f32, pr: f32) -> Parts {
    let room = |p: Parts| {
        let repo = (repo + if p.counts { counts } else { 0.0 }).min(bar_w * REPO_MAX_SHARE);
        let taken: f32 = [
            (repo > 0.0).then_some(repo),
            (p.pr && pr > 0.0).then_some(pr),
            p.unread.then_some(PILL_H),
        ]
        .into_iter()
        .flatten()
        .map(|w| w + BAR_GAP)
        .sum();
        bar_w - 2.0 * BAR_PAD_X - taken
    };
    if room(parts) < CWD_MIN_W {
        parts.counts = false;
    }
    if room(parts) < CWD_MIN_W {
        parts.pr = false;
    }
    if room(parts) < CWD_MIN_W {
        parts.unread = false;
    }
    parts
}

/// `text` cut to at most `max_chars` characters by dropping its head: the
/// tail — the part of a path that names where you are — stays, behind a `…`.
pub(crate) fn truncate_left(text: &str, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let tail: String = text.chars().skip(n - keep).collect();
    format!("…{tail}")
}

/// Estimated width (px at the default chrome text size) of a pill holding a
/// glyph and `chars` characters of pill type plus `count_chars` characters of
/// counts in `count_runs` separate runs.
fn pill_width(chars: usize, count_chars: usize, count_runs: usize) -> f32 {
    2.0 * PILL_PAD_X
        + GLYPH
        + PILL_GAP
        + chars as f32 * TEXT_SIZE * CHAR_EM
        + count_runs as f32 * PILL_GAP
        + count_chars as f32 * COUNT_SIZE * CHAR_EM
}

/// How many characters of the cwd fit its pill, given the bar's width and the
/// estimated widths of the other pills on it (all in px at the default chrome
/// text size): the pill takes what the others leave, capped at 420px.
fn cwd_char_budget(bar_w: f32, others: &[f32]) -> usize {
    let taken: f32 = others.iter().map(|w| w + BAR_GAP).sum();
    let pill = (bar_w - 2.0 * BAR_PAD_X - taken).min(CWD_MAX_W);
    let text = pill - 2.0 * PILL_PAD_X - GLYPH - PILL_GAP;
    ((text / (TEXT_SIZE * CHAR_EM)).floor().max(0.0) as usize).max(CWD_MIN_CHARS)
}

/// The dim state word beside a PR's number.
fn pr_state_word(pr: &PrSummary) -> &'static str {
    if pr.state.eq_ignore_ascii_case("open") {
        if pr.is_draft { "Draft" } else { "Open" }
    } else if pr.state.eq_ignore_ascii_case("merged") {
        "Merged"
    } else {
        "Closed"
    }
}

/// The checks rollup: `(passed, total, verdict)`, or `None` for a PR that
/// carries no checks. The verdict is [`aggregate_check_status`]'s — any
/// failure wins, then anything still running, otherwise success.
fn checks_summary(checks: &[Check]) -> Option<(usize, usize, CheckStatus)> {
    let verdict = aggregate_check_status(checks)?;
    // Skipped / neutral / cancelled checks are neither passes nor part of
    // the total, so an all-green PR with a skipped job still reads n/n.
    let counted = |s: &CheckStatus| {
        matches!(s, CheckStatus::Success | CheckStatus::Failure | CheckStatus::Pending)
    };
    let passed = checks.iter().filter(|c| c.status == CheckStatus::Success).count();
    let total = checks.iter().filter(|c| counted(&c.status)).count();
    Some((passed, total, verdict))
}

/// The repo pill's text: `(repo, branch-or-short-sha)`. Either half may be
/// missing; `None` when the snapshot names neither.
fn repo_label(ctx: &GitContext) -> Option<(Option<String>, Option<String>)> {
    let repo = ctx.repo.clone().filter(|r| !r.is_empty());
    let head = ctx
        .branch
        .clone()
        .filter(|b| !b.is_empty())
        .or_else(|| ctx.head_sha.clone().filter(|s| !s.is_empty()));
    (repo.is_some() || head.is_some()).then_some((repo, head))
}

/// The repo pill's counts: the committed branch diff as `(+A, −R)` when
/// there is one, and the number of uncommitted files when above zero.
fn repo_counts(ctx: &GitContext) -> (Option<(String, String)>, Option<String>) {
    let diff = ctx
        .branch_diff
        .filter(|d| d.insertions > 0 || d.deletions > 0)
        .map(|d| (format!("+{}", d.insertions), format!("−{}", d.deletions)));
    let dirty = ctx.dirty.filter(|d| d.files > 0).map(|d| d.files.to_string());
    (diff, dirty)
}

/// A full pill: [`PILL_H`] high, fully rounded, `fill` behind its children.
fn pill(ui: f32, fill: Hsla) -> gpui::Div {
    let h = PILL_H * ui;
    div()
        .flex_shrink_0()
        .h(px(h))
        .rounded(px(h / 2.0))
        .bg(fill)
        .px(px(PILL_PAD_X * ui))
        .flex()
        .items_center()
        .gap(px(PILL_GAP * ui))
        .overflow_hidden()
        .whitespace_nowrap()
}

impl App {
    /// The visible group's primary-pane info bar, or an empty element off the
    /// Sessions page, in the empty state, or when the group has no primary
    /// pane on screen.
    pub(crate) fn render_info_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.page != crate::Page::Sessions || self.is_empty_state() {
            return div().into_any_element();
        }
        // The flyover slides over the bottom of the tiles; stop this layer
        // above it exactly as the tab strips do.
        let ceiling = self.flyover_ceiling();
        if ceiling == Some(0.0) {
            return div().into_any_element();
        }
        let scale = self.scale();
        let inv = 1.0 / scale;
        let ui = workspace::chrome_ui_scale();
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        let Some((_, rect)) = tiles.iter().find(|(id, _)| ws.is_primary(*id)) else {
            return div().into_any_element();
        };
        let Some(tile) = ws.root.find_tile(ws.primary_tile) else {
            return div().into_any_element();
        };
        // A collapsed (or mid-animation) primary is a bare sideways strip
        // (`tile_ui`): no info bar.
        if tile.collapsed || tile.collapse_anim > 0.0 {
            return div().into_any_element();
        }
        let bar = workspace::primary_info_bar(rect, scale);
        if bar.w < 1.0 || bar.h < 1.0 {
            return div().into_any_element();
        }
        let bar_w = bar.w * inv;

        let th = crate::theme::current();
        let dark = crate::theme::dark_active();
        let strip = StripStyle::from_scheme(th);
        let fill = strip.ink.opacity(PILL_FILL);
        // The sidebar's diff inks double as the status green / red: they
        // already have light-palette variants.
        let green = crate::sidebar_ui::diff_added(dark);
        let red = crate::sidebar_ui::diff_removed(dark);
        let amber = amber(dark);
        let text_size = TEXT_SIZE * ui;
        let count_size = COUNT_SIZE * ui;
        let glyph = px(GLYPH * ui);

        // The same snapshot the sidebar row reads: keyed by the group's own
        // cwd, absent until the background walk has delivered one.
        let ctx = ws.cwd.as_deref().and_then(|cwd| self.git_contexts.get(cwd));
        let git = ctx.filter(|c| c.is_git);
        // Estimated widths of the pills beside the cwd, for its char budget.
        let mut others: Vec<f32> = Vec::new();
        // Which optional parts show: the width thresholds, then whatever the
        // actual content still crowds out.
        let est = |chars: usize, counts: &[usize]| {
            pill_width(chars, counts.iter().sum::<usize>() + counts.len().saturating_sub(1), counts.len())
        };
        let parts = {
            let label = git.and_then(|c| repo_label(c).map(|l| (c, l)));
            let (repo_w, counts_w) = label.map_or((0.0, 0.0), |(c, (repo, head))| {
                let chars = repo.map_or(0, |r| r.chars().count() + 1)
                    + head.map_or(0, |h| h.chars().count());
                let (diff, dirty) = repo_counts(c);
                let runs: Vec<usize> = diff
                    .iter()
                    .flat_map(|(a, r)| [a.chars().count(), r.chars().count()])
                    .chain(dirty.iter().map(|d| 1 + d.chars().count()))
                    .collect();
                let bare = est(chars, &[]);
                let dirty_gap = if dirty.is_some() { DIRTY_GAP } else { 0.0 };
                (bare, est(chars, &runs) - bare + dirty_gap)
            });
            let pr_w = git.and_then(|c| c.pr.as_ref()).map_or(0.0, |pr| {
                let chars = format!("#{}", pr.number).len() + 1 + pr_state_word(pr).len();
                let checks = checks_summary(&pr.checks)
                    .map(|(p, t, _)| format!("✓ {p}/{t}").chars().count());
                est(chars, checks.as_slice())
            });
            fit_parts(visible_parts(bar_w, ui), bar_w / ui, repo_w, counts_w, pr_w)
        };

        // ── Repo / branch ───────────────────────────────────────────────
        let repo_pill = git.and_then(|c| repo_label(c).map(|label| (c, label))).map(
            |(c, (repo, head))| {
                let (diff, dirty) = if parts.counts { repo_counts(c) } else { (None, None) };
                let chars = repo.as_deref().map_or(0, |r| r.chars().count())
                    + head.as_deref().map_or(0, |h| h.chars().count())
                    + usize::from(repo.is_some() && head.is_some());
                let count_chars = diff
                    .as_ref()
                    .map_or(0, |(a, r)| a.chars().count() + 1 + r.chars().count())
                    + dirty.as_deref().map_or(0, |d| 1 + d.chars().count());
                let runs = usize::from(diff.is_some()) + usize::from(dirty.is_some());
                let dirty_gap = if dirty.is_some() { DIRTY_GAP } else { 0.0 };
                others.push(
                    (pill_width(chars, count_chars, runs) + dirty_gap)
                        .min(bar_w / ui * REPO_MAX_SHARE),
                );

                let mut name =
                    div().flex().items_center().min_w(px(0.0)).text_size(px(text_size));
                if let Some(repo) = repo.clone() {
                    name = name.child(div().text_color(strip.ink).child(repo));
                }
                if repo.is_some() && head.is_some() {
                    name = name.child(div().text_color(strip.ink_dim).child("/"));
                }
                if let Some(head) = head {
                    // The branch is the half that gives when the pill is
                    // capped.
                    name = name.child(div().min_w(px(0.0)).truncate().text_color(strip.ink).child(head));
                }
                pill(ui, fill)
                    .flex_shrink(1.0)
                    .min_w(px(0.0))
                    .max_w(px(bar_w * REPO_MAX_SHARE))
                    .child(icon(ICON_GIT_BRANCH, glyph, strip.ink_dim))
                    .child(name)
                    .when_some(diff, |d, (added, removed)| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(PILL_GAP * ui))
                                .text_size(px(count_size))
                                .child(
                                    div()
                                        .text_color(crate::sidebar_ui::diff_added(dark))
                                        .child(added),
                                )
                                .child(
                                    div()
                                        .text_color(crate::sidebar_ui::diff_removed(dark))
                                        .child(removed),
                                ),
                        )
                    })
                    .when_some(dirty, |d, files| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(DIRTY_GAP * ui))
                                .text_size(px(count_size))
                                .text_color(amber)
                                .child(DIRTY_DOT)
                                .child(files),
                        )
                    })
            },
        );

        // ── Pull request ────────────────────────────────────────────────
        let entity = cx.entity().downgrade();
        let pr_pill = git
            .and_then(|c| c.pr.as_ref())
            .filter(|_| parts.pr)
            .map(|pr| {
                let kind = avatar_for(derive_rollup(Some(pr)), pr.is_draft);
                let tint = match kind {
                    CardAvatar::NoPr => crate::sidebar_ui::pr_none_ink(dark),
                    CardAvatar::Draft => crate::sidebar_ui::accent(),
                    CardAvatar::Open => crate::sidebar_ui::pr_open(dark),
                    CardAvatar::Merged => crate::sidebar_ui::pr_merged(dark),
                };
                let number = format!("#{}", pr.number);
                let word = pr_state_word(pr);
                let checks = checks_summary(&pr.checks).map(|(passed, total, verdict)| {
                    let ink = match verdict {
                        CheckStatus::Failure => red,
                        CheckStatus::Pending => strip.ink_dim,
                        _ => green,
                    };
                    (format!("✓ {passed}/{total}"), ink)
                });
                others.push(pill_width(
                    number.chars().count() + 1 + word.len(),
                    checks.as_ref().map_or(0, |(text, _)| text.chars().count()),
                    usize::from(checks.is_some()),
                ));
                let entity = entity.clone();
                pill(ui, fill)
                    .id("infobar-pr")
                    .cursor_pointer()
                    .hover(move |d| d.bg(strip.ink.opacity(2.0 * PILL_FILL)))
                    .text_size(px(text_size))
                    .child(icon(crate::sidebar_ui::avatar_icon(kind), glyph, tint))
                    .child(div().text_color(strip.ink).child(number))
                    .child(div().text_color(strip.ink_dim).child(word))
                    .when_some(checks, |d, (text, ink)| {
                        d.child(div().text_size(px(count_size)).text_color(ink).child(text))
                    })
                    // Focus the pane like any press on the bar, then take
                    // ⇧⌘G's path for this (the active) group.
                    .on_mouse_down(
                        MouseButton::Left,
                        move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                            app.stop_propagation();
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, cx| {
                                    this.note_pointer(ev);
                                    this.press_primary_header();
                                    this.open_pr_in_github();
                                    cx.notify();
                                });
                            }
                        },
                    )
                    .into_any_element()
            });

        // ── Unread ──────────────────────────────────────────────────────
        let unread_chip = parts.unread.then(|| {
            others.push(PILL_H);
            let unread = tile.active_tab().is_some_and(|tab| tab.unread);
            let side = PILL_H * ui;
            let ring = (DOT + 2.0 * DOT_RING) * ui;
            div()
                .flex_shrink_0()
                .w(px(side))
                .h(px(side))
                .rounded(px(side / 2.0))
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(ring))
                        .h(px(ring))
                        .rounded(px(ring / 2.0))
                        .when(unread, |d| d.bg(green.opacity(DOT_RING_ALPHA)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(DOT * ui))
                                .h(px(DOT * ui))
                                .rounded(px(DOT * ui / 2.0))
                                .bg(if unread { green } else { strip.ink_dim }),
                        ),
                )
        });

        // ── cwd ─────────────────────────────────────────────────────────
        // A group without a directory of its own runs where pwrde was
        // launched, as everywhere else in the app.
        let cwd = ws
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .map(|dir| crate::tilde(&dir))
            .unwrap_or_default();
        // A click copies the directory as shown, but whole — never the
        // truncated label.
        let full_cwd = cwd.clone();
        // Just copied: the pill says so, in the status green, until
        // `drain_events` ends the flash.
        let copied = self.cwd_copied_until.is_some();
        let cwd = if copied {
            "Copied".to_string()
        } else {
            truncate_left(&cwd, cwd_char_budget(bar_w / ui, &others))
        };
        let (cwd_fill, cwd_ink, cwd_icon_ink) = if copied {
            (green.opacity(2.0 * PILL_FILL), green, green)
        } else {
            (fill, strip.ink, strip.ink_dim)
        };
        let entity = cx.entity().downgrade();
        let cwd_pill = div().flex_1().min_w(px(0.0)).flex().justify_center().child(
            pill(ui, cwd_fill)
                .id("infobar-cwd")
                .cursor_pointer()
                .when(!copied, |d| d.hover(move |d| d.bg(strip.ink.opacity(2.0 * PILL_FILL))))
                .flex_shrink(1.0)
                .min_w(px(0.0))
                .max_w(px(CWD_MAX_W * ui))
                // Focus the pane like any press on the bar, then copy.
                .on_mouse_down(
                    MouseButton::Left,
                    move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                        app.stop_propagation();
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.note_pointer(ev);
                                this.press_primary_header();
                                this.copy_cwd(&full_cwd);
                                cx.notify();
                            });
                        }
                    },
                )
                .child(icon(ICON_FOLDER, glyph, cwd_icon_ink))
                .child(
                    // Right-aligned, so if the estimate above ever runs
                    // long it is still the head that is clipped.
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .flex()
                        .justify_end()
                        .text_size(px(text_size))
                        .text_color(cwd_ink)
                        .child(cwd),
                ),
        );

        let entity = cx.entity().downgrade();
        let bar_el = div()
            .absolute()
            .left(px(bar.x * inv))
            .top(px(bar.y * inv))
            .w(px(bar_w))
            .h(px(bar.h * inv))
            .overflow_hidden()
            .px(px(BAR_PAD_X * ui))
            .flex()
            .items_center()
            .gap(px(BAR_GAP * ui))
            .font_family(crate::renderer::FONT_FAMILY)
            // A press on the bar focuses the primary pane and stops there:
            // it must never reach the canvas as a terminal click.
            .on_mouse_down(
                MouseButton::Left,
                move |ev: &MouseDownEvent, _win: &mut Window, app: &mut GpuiApp| {
                    app.stop_propagation();
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.note_pointer(ev);
                            this.press_primary_header();
                            cx.notify();
                        });
                    }
                },
            )
            .children(repo_pill)
            .child(cwd_pill)
            .children(pr_pill)
            .children(unread_chip);

        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w_full()
            .overflow_hidden()
            .map(|d| match ceiling {
                Some(limit) => d.h(px(limit)),
                None => d.h_full(),
            })
            .child(bar_el)
            .into_any_element()
    }

    /// The cwd pill's click: put the directory on the clipboard and say so.
    fn copy_cwd(&mut self, cwd: &str) {
        if cwd.is_empty() {
            return;
        }
        // `$HOME` itself reads `~/`; paste it as `~`.
        let cwd = if cwd == "~/" { "~" } else { cwd };
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(cwd)) {
            // Confirmed in place: the pill reads "Copied" for a moment.
            Ok(()) => self.cwd_copied_until = Some(Instant::now() + COPIED_FLASH),
            Err(error) => self.toast_notification(format!("copy directory: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::DirtyStats;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    fn ctx() -> GitContext {
        GitContext {
            is_git: true,
            cwd: PathBuf::from("/src/pwrde"),
            repo: Some("pwrde".to_string()),
            branch: Some("main".to_string()),
            default_branch: Some("main".to_string()),
            head_sha: Some("da11342".to_string()),
            dirty: None,
            branch_diff: None,
            pr: None,
            pr_error: None,
            fetched_at: UNIX_EPOCH,
        }
    }

    fn pr(state: &str, is_draft: bool, checks: &[CheckStatus]) -> PrSummary {
        PrSummary {
            number: 132,
            title: String::new(),
            state: state.to_string(),
            is_draft,
            head: String::new(),
            author: String::new(),
            review_decision: None,
            mergeable: None,
            checks: checks
                .iter()
                .map(|status| Check { name: String::new(), status: *status, url: String::new() })
                .collect(),
            url: String::new(),
        }
    }

    /// The width collapse sheds the counts, then the PR pill, then the unread
    /// chip — at 620 / 540 / 400px, scaled by the chrome factor.
    #[test]
    fn narrow_bars_shed_counts_then_pr_then_unread() {
        let all = Parts { counts: true, pr: true, unread: true };
        assert_eq!(visible_parts(900.0, 1.0), all);
        assert_eq!(visible_parts(620.0, 1.0), all);
        assert_eq!(visible_parts(619.0, 1.0), Parts { counts: false, ..all });
        assert_eq!(visible_parts(540.0, 1.0), Parts { counts: false, ..all });
        assert_eq!(visible_parts(539.0, 1.0), Parts { counts: false, pr: false, unread: true });
        assert_eq!(visible_parts(400.0, 1.0), Parts { counts: false, pr: false, unread: true });
        assert_eq!(visible_parts(399.0, 1.0), Parts { counts: false, pr: false, unread: false });
        // A larger chrome text size widens the pills, so the thresholds move
        // with it.
        assert_eq!(visible_parts(700.0, 1.25), Parts { counts: false, ..all });
        assert_eq!(visible_parts(775.0, 1.25), all);
        assert_eq!(visible_parts(499.0, 1.25), Parts { counts: false, pr: false, unread: false });
    }

    #[test]
    fn copied_flash_clears_once_at_its_deadline() {
        let now = Instant::now();
        let mut until = Some(now + COPIED_FLASH);
        assert!(!flash_due(&mut until, now));
        assert!(until.is_some());
        assert!(flash_due(&mut until, now + COPIED_FLASH));
        assert!(until.is_none());
        assert!(!flash_due(&mut until, now + 2 * COPIED_FLASH));
    }

    #[test]
    fn truncate_left_keeps_the_tail() {
        assert_eq!(truncate_left("~/src/pwrde", 20), "~/src/pwrde");
        assert_eq!(truncate_left("~/src/pwrde", 11), "~/src/pwrde");
        assert_eq!(truncate_left("~/src/pwrde", 6), "…pwrde");
        assert_eq!(truncate_left("~/src/pwrde", 1), "…");
        // Counted in characters, not bytes.
        assert_eq!(truncate_left("~/src/日本語", 4), "…日本語");
    }

    /// The cwd takes what the other pills leave, capped by the 420px pill and
    /// floored so a crowded bar still names the directory.
    #[test]
    fn cwd_budget_follows_the_room_left() {
        // Alone on a wide bar: the 420px cap decides.
        let capped = cwd_char_budget(1200.0, &[]);
        assert_eq!(capped, ((420.0 - 24.0 - 13.0 - 6.0) / 7.5_f32).floor() as usize);
        // Neighbours eat into it.
        assert!(cwd_char_budget(500.0, &[200.0, 32.0]) < capped);
        // Never below the floor.
        assert_eq!(cwd_char_budget(100.0, &[200.0, 150.0]), CWD_MIN_CHARS);
    }

    #[test]
    fn pr_state_words() {
        assert_eq!(pr_state_word(&pr("OPEN", true, &[])), "Draft");
        assert_eq!(pr_state_word(&pr("OPEN", false, &[])), "Open");
        assert_eq!(pr_state_word(&pr("MERGED", false, &[])), "Merged");
        assert_eq!(pr_state_word(&pr("CLOSED", false, &[])), "Closed");
    }

    #[test]
    fn checks_summary_counts_passes_and_takes_the_worst_verdict() {
        use CheckStatus::{Failure, Pending, Success};
        assert_eq!(checks_summary(&[]), None);
        assert_eq!(
            checks_summary(&pr("OPEN", false, &[Success, Success]).checks),
            Some((2, 2, Success))
        );
        assert_eq!(
            checks_summary(&pr("OPEN", false, &[Success, Pending]).checks),
            Some((1, 2, Pending))
        );
        assert_eq!(
            checks_summary(&pr("OPEN", false, &[Success, Pending, Failure]).checks),
            Some((1, 3, Failure))
        );
        // A skipped job is neither a pass nor part of the total.
        assert_eq!(
            checks_summary(&pr("OPEN", false, &[Success, CheckStatus::Skipped]).checks),
            Some((1, 1, Success))
        );
    }

    /// Content can crowd the cwd out before the width thresholds do: the
    /// counts go first, then the PR pill, then the unread chip.
    #[test]
    fn crowded_bars_shed_parts_until_the_cwd_has_room() {
        let all = Parts { counts: true, pr: true, unread: true };
        // Plenty of room: nothing drops.
        assert_eq!(fit_parts(all, 900.0, 180.0, 90.0, 160.0), all);
        // 620px with a heavy repo pill and PR: the counts give way.
        assert_eq!(
            fit_parts(all, 620.0, 200.0, 120.0, 180.0),
            Parts { counts: false, pr: true, unread: true }
        );
        // Still short without them: the PR pill goes too.
        assert_eq!(
            fit_parts(all, 480.0, 200.0, 120.0, 180.0),
            Parts { counts: false, pr: false, unread: true }
        );
        // A part the thresholds already hid stays hidden.
        let none = Parts { counts: false, pr: false, unread: false };
        assert_eq!(fit_parts(none, 900.0, 180.0, 90.0, 160.0), none);
    }

    #[test]
    fn repo_label_falls_back_to_the_short_sha_when_detached() {
        assert_eq!(
            repo_label(&ctx()),
            Some((Some("pwrde".to_string()), Some("main".to_string())))
        );
        let detached = GitContext { branch: None, ..ctx() };
        assert_eq!(
            repo_label(&detached),
            Some((Some("pwrde".to_string()), Some("da11342".to_string())))
        );
        let bare = GitContext { repo: None, branch: None, head_sha: None, ..ctx() };
        assert_eq!(repo_label(&bare), None);
    }

    #[test]
    fn repo_counts_show_the_branch_diff_and_uncommitted_files() {
        assert_eq!(repo_counts(&ctx()), (None, None));
        let mut c = ctx();
        c.branch_diff = Some(DirtyStats { files: 3, insertions: 42, deletions: 7 });
        c.dirty = Some(DirtyStats { files: 2, insertions: 1, deletions: 0 });
        assert_eq!(
            repo_counts(&c),
            (Some(("+42".to_string(), "−7".to_string())), Some("2".to_string()))
        );
        // A clean tree and an empty branch show nothing.
        c.branch_diff = Some(DirtyStats::default());
        c.dirty = Some(DirtyStats::default());
        assert_eq!(repo_counts(&c), (None, None));
    }
}
