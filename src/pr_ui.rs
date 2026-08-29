//! Pull Request tool: a gpui element tree over the canvas (same overlay
//! pattern as `cleanup_ui`), positioned over the right-edge tool panel.
//!
//! Two views live here: a picker listing the repo's open PRs, and a detail view
//! (header + checks + conversation + a syntax-highlighted files-changed diff)
//! with write actions (approve / request changes / comment / ready / merge).
//! Data is fetched off-thread through [`crate::gh`] and delivered back as
//! `TermEvent`s; with `lfg` + `git.async` on, cached data paints instantly and
//! the `lfg` SSE stream re-fetches when GitHub returns fresh data.
//!
//! The syntax-highlighted diff body ([`diff_files_view`]) is shared with the
//! local diff tool.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    div, px, AnyElement, App as GpuiApp, ClickEvent, Context, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, Window, prelude::FluentBuilder as _,
};

use crate::diff::{DiffFile, LineKind};
use crate::gh::{self, Check, CheckStatus, EventKind, PrDetail, PrSummary, TimelineEvent};
use crate::ui::theme::Theme;
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, CardContent, CardHeader,
    CardSize, CardTitle, Skeleton,
};
use crate::App;

/// Build the positioned tool-panel overlay shared by the PR and local-diff
/// tools. In embedded mode the card sits flush left of the ribbon; in floating
/// mode it insets by `TOOL_PANEL_FLOAT_INSET` on every free edge and gains a
/// drop shadow so it reads as floating over the tiles. The insets MUST match
/// `workspace::tool_panel(..., floating=true)` so the card's left edge lines up
/// with the resize hit-rect.
pub(crate) fn tool_panel_overlay(
    floating: bool,
    panel_w: f32,
    header: gpui::AnyElement,
    body: gpui::AnyElement,
) -> gpui::AnyElement {
    use crate::workspace::{AREA_PAD, RIBBON_W, TOOL_PANEL_FLOAT_INSET};
    // Occluding: the canvas no longer swallows clicks over the panel rect,
    // so the panel must own every click inside it — a floating panel sits
    // over live tiles.
    let mut root = div().absolute().occlude().w(px(panel_w));
    root = if floating {
        root.right(px(RIBBON_W + TOOL_PANEL_FLOAT_INSET))
            .top(px(AREA_PAD + TOOL_PANEL_FLOAT_INSET))
            .bottom(px(AREA_PAD + TOOL_PANEL_FLOAT_INSET))
    } else {
        root.right(px(RIBBON_W)).top(px(AREA_PAD)).bottom(px(AREA_PAD))
    };
    root.child(
        Card::new()
            .h_full()
            .when(floating, |c| c.floating())
            .child(CardHeader::new().child(header))
            .child(CardContent::new().flex_1().child(body)),
    )
    .into_any_element()
}

/// A lazily-loaded value fetched off the UI thread.
#[derive(Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// Which detail tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PrTab {
    #[default]
    Conversation,
    Files,
}

/// Which surface is currently presenting the PR views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrSurface {
    /// None visible.
    None,
    /// The Sessions right-edge tool (branch-scoped).
    Tool,
    /// The holistic Pull Requests page (all open PRs).
    Page,
}

/// Pull-request state, shared by the branch-scoped Sessions tool and the
/// holistic Pull Requests page. The two surfaces are never visible at once
/// (the page is a top-level `Page`, the tool only shows on `Sessions`), so they
/// share one detail/diff view; only the list differs.
pub struct PrState {
    /// PR(s) for the checked-out branch — the Sessions tool's list.
    pub branch_list: Load<Vec<PrSummary>>,
    /// All open PRs for the repo — the Pull Requests page's list.
    pub all_list: Load<Vec<PrSummary>>,
    /// Current branch name, for the tool's empty-state copy.
    pub branch: Option<String>,
    /// The PR open in the detail view, or `None` while a list shows.
    pub open: Option<u32>,
    pub detail: Load<PrDetail>,
    /// The open PR's conversation with markdown parsed + code pre-highlighted
    /// once per load, so the per-frame render never touches pulldown/syntect.
    /// Rebuilt whenever `detail` becomes `Ready`; `None` otherwise.
    pub conversation: Option<PreparedConversation>,
    /// Collapsible `<details>` whose expand state the user flipped from its
    /// `open` default, keyed by content hash. Survives conversation rebuilds.
    pub md_flipped: std::collections::HashSet<u64>,
    /// Precomputed, syntax-highlighted diff (built off-thread).
    pub diff: Load<Arc<DiffRender>>,
    pub tab: PrTab,
    /// Transient result banner from the last write action.
    pub action_msg: Option<String>,
    /// A write action is in flight (buttons disabled until it resolves).
    pub acting: bool,
    /// Whether the merge button is armed (showing the confirm/cancel step).
    /// Merge is irreversible + outward-facing, so it takes two clicks.
    pub merge_confirm: bool,
    /// Collapse/viewed state for the Files-changed diff cards.
    pub files_view: DiffViewState,
}

impl Default for PrState {
    fn default() -> Self {
        PrState {
            branch_list: Load::Idle,
            all_list: Load::Idle,
            branch: None,
            open: None,
            detail: Load::Idle,
            conversation: None,
            md_flipped: std::collections::HashSet::new(),
            diff: Load::Idle,
            tab: PrTab::default(),
            action_msg: None,
            acting: false,
            merge_confirm: false,
            files_view: DiffViewState::default(),
        }
    }
}

impl App {
    // ── Fetch orchestration ──────────────────────────────────────────────

    /// Which PR surface is on screen right now.
    pub fn pr_surface(&self) -> PrSurface {
        if self.page == crate::pages::Page::PullRequests {
            PrSurface::Page
        } else if self.visible_tool() == Some(crate::pages::Tool::Pr) {
            PrSurface::Tool
        } else {
            PrSurface::None
        }
    }

    /// (Re)load the PR(s) for the checked-out branch — the Sessions tool's list.
    pub fn spawn_pr_branch_list(&mut self) {
        let Some(dir) = self.active_repo_dir() else { return };
        let branch = crate::git::current_branch(&dir);
        self.pr.branch = branch.clone();
        self.pr.branch_list = Load::Loading;
        let tx = self.events_tx.clone();
        crate::bg::spawn(move || {
            let result = match branch {
                Some(b) => gh::pr_list_for_branch(&dir, &b),
                None => Ok(Vec::new()),
            };
            let _ = tx.send(crate::term::TermEvent::PrListLoaded { all: false, result });
        });
        self.request_redraw();
    }

    /// (Re)load all open PRs for the repo — the Pull Requests page's list.
    pub fn spawn_pr_all_list(&mut self) {
        let Some(dir) = self.active_repo_dir() else { return };
        self.pr.all_list = Load::Loading;
        let tx = self.events_tx.clone();
        crate::bg::spawn(move || {
            let _ = tx.send(crate::term::TermEvent::PrListLoaded { all: true, result: gh::pr_list(&dir) });
        });
        self.request_redraw();
    }

    /// Reset the detail view and (re)load the list for the current surface.
    /// Called when a surface is entered or the active group changes.
    pub fn reset_pr_surface(&mut self) {
        self.pr.open = None;
        self.pr.detail = Load::Idle;
        self.pr.diff = Load::Idle;
        self.pr.merge_confirm = false;
        self.pr.action_msg = None;
        match self.pr_surface() {
            PrSurface::Page => self.spawn_pr_all_list(),
            PrSurface::Tool => self.spawn_pr_branch_list(),
            PrSurface::None => {}
        }
    }

    /// Open a PR in the detail view and fetch its detail + diff.
    pub fn open_pr(&mut self, number: u32) {
        self.pr.open = Some(number);
        self.pr.tab = PrTab::Conversation;
        self.pr.action_msg = None;
        self.pr.merge_confirm = false;
        self.pr.files_view = DiffViewState::default();
        self.spawn_pr_detail(number);
        self.spawn_pr_diff(number);
    }

    /// Re-fetch the open PR's detail + diff without resetting the view: unlike
    /// [`open_pr`], a manual Refresh preserves the current tab and the files
    /// view's collapse/viewed/sidebar state (the diff is re-seeded only if it
    /// wasn't already), matching the silent background-refresh paths.
    pub fn refresh_open_pr(&mut self, number: u32) {
        self.pr.open = Some(number);
        self.pr.merge_confirm = false;
        self.spawn_pr_detail(number);
        self.spawn_pr_diff(number);
    }

    /// Return to the PR list from the detail view.
    pub fn close_pr_detail(&mut self) {
        self.pr.open = None;
        self.pr.detail = Load::Idle;
        self.pr.diff = Load::Idle;
        self.pr.merge_confirm = false;
        self.request_redraw();
    }

    fn spawn_pr_detail(&mut self, number: u32) {
        let Some(dir) = self.active_repo_dir() else { return };
        self.pr.detail = Load::Loading;
        self.pr.conversation = None;
        let tx = self.events_tx.clone();
        crate::bg::spawn(move || {
            let result = gh::pr_detail(&dir, number);
            let _ = tx.send(crate::term::TermEvent::PrDetailLoaded { number, result });
        });
        self.request_redraw();
    }

    fn spawn_pr_diff(&mut self, number: u32) {
        let Some(dir) = self.active_repo_dir() else { return };
        self.pr.diff = Load::Loading;
        let tx = self.events_tx.clone();
        let dark = crate::theme::dark_active();
        crate::bg::spawn(move || {
            // Fetch, parse, and syntax-highlight entirely off the UI thread so
            // the render path never touches syntect.
            let result = gh::pr_diff(&dir, number)
                .map(|files| Arc::new(build_diff_render(&files, dark)));
            let _ = tx.send(crate::term::TermEvent::PrDiffLoaded { number, result });
        });
        self.request_redraw();
    }

    /// Run a write action against the open PR, then refresh.
    pub fn spawn_pr_action(&mut self, action: gh::Action) {
        let (Some(dir), Some(number)) = (self.active_repo_dir(), self.pr.open) else { return };
        self.pr.acting = true;
        self.pr.action_msg = None;
        let tx = self.events_tx.clone();
        crate::bg::spawn(move || {
            let _ = tx.send(crate::term::TermEvent::PrActionDone(gh::run_action(
                &dir, number, &action,
            )));
        });
        self.request_redraw();
    }

    // ── TermEvent handlers (called from drain_events) ─────────────────────

    pub fn on_pr_list_loaded(&mut self, all: bool, result: Result<Vec<PrSummary>, String>) {
        let load = match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        };
        if all {
            self.pr.all_list = load;
            return;
        }
        // Branch-scoped: if there's exactly one PR for the checked-out branch
        // and nothing is open yet, jump straight into it — the common case.
        if self.pr.open.is_none()
            && let Load::Ready(v) = &load
            && v.len() == 1
        {
            let number = v[0].number;
            self.pr.branch_list = load;
            self.open_pr(number);
            return;
        }
        self.pr.branch_list = load;
    }

    pub fn on_pr_detail_loaded(&mut self, number: u32, result: Result<PrDetail, String>) {
        if self.pr.open != Some(number) {
            return; // navigated away; drop the stale result
        }
        self.pr.detail = match result {
            // A cold `lfg -A` miss yields number==0 (empty placeholder); keep
            // showing the loading state until the SSE refresh lands.
            Ok(d) if d.number == 0 => Load::Loading,
            Ok(d) => Load::Ready(d),
            Err(e) => Load::Failed(e),
        };
        // Prepare the conversation once, off the per-frame render path: parse
        // markdown + syntax-highlight code fences now so rendering stays cheap.
        self.pr.conversation = match &self.pr.detail {
            Load::Ready(d) => Some(prepare_conversation(d, crate::theme::dark_active())),
            _ => None,
        };
    }

    pub fn on_pr_diff_loaded(
        &mut self,
        number: u32,
        result: Result<Arc<DiffRender>, String>,
    ) {
        if self.pr.open != Some(number) {
            return;
        }
        self.pr.diff = match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        };
        if let Load::Ready(r) = &self.pr.diff {
            let r = r.clone();
            self.pr.files_view.seed(&r.files);
        }
    }

    pub fn on_pr_action_done(&mut self, result: Result<String, String>) {
        self.pr.acting = false;
        self.pr.action_msg = Some(match result {
            Ok(msg) => msg,
            Err(e) => format!("Action failed: {e}"),
        });
        // Refresh the PR so the new review/comment/state shows.
        if let Some(n) = self.pr.open {
            self.spawn_pr_detail(n);
            self.spawn_pr_diff(n);
        }
        self.refresh_pr_list();
    }

    /// Re-fetch the list backing the current surface (all vs branch).
    fn refresh_pr_list(&mut self) {
        match self.pr_surface() {
            PrSurface::Page => self.spawn_pr_all_list(),
            PrSurface::Tool => self.spawn_pr_branch_list(),
            PrSurface::None => {}
        }
    }

    /// An `lfg` cache-updated event arrived. Re-fetch the visible view if it's
    /// the PR that changed (or a PR-scoped event while a list is showing).
    /// Returns whether a redraw is warranted.
    pub fn on_pr_cache_updated(&mut self, kind: &str, number: Option<u32>) -> bool {
        if kind != "pr" || self.pr_surface() == PrSurface::None {
            return false;
        }
        match self.pr.open {
            Some(open) => {
                if number == Some(open) || number.is_none() {
                    self.spawn_pr_detail(open);
                    self.spawn_pr_diff(open);
                    return true;
                }
            }
            None => {
                self.refresh_pr_list();
                return true;
            }
        }
        false
    }

    // ── Render ────────────────────────────────────────────────────────────

    /// Build the Pull Request panel overlay. Call only when the PR tool is the
    /// visible tool.
    pub fn render_pr(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        let header_title = match self.pr.open {
            Some(n) => format!("PR #{n}"),
            None => "Pull Requests".into(),
        };

        // Header: back button (detail only) + title + refresh.
        let mut header_row = div().flex().flex_row().items_center().w_full().gap_2();
        if self.pr.open.is_some() {
            let back_entity = entity.clone();
            header_row = header_row.child(
                Button::new("pr-back")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child("← Back")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = back_entity.upgrade() {
                            e.update(app, |this, cx| {
                                this.close_pr_detail();
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        header_row = header_row.child(CardTitle::new().child(header_title));

        // Right-aligned actions: optional "Open in GitHub" (when a detail is
        // loaded), a Float/Dock toggle, then Refresh.
        let mut actions = div().flex().flex_row().items_center().gap_2();
        if self.pr.open.is_some() {
            if let Load::Ready(d) = &self.pr.detail {
                let url = d.url.clone();
                actions = actions.child(
                    Button::new("pr-open-gh")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .child("\u{2197} GitHub")
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, _app: &mut GpuiApp| {
                            crate::links::open(&url);
                        }),
                );
            }
        }
        let float_entity = entity.clone();
        actions = actions.child(
            Button::new("pr-float-toggle")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .child(if self.tool_panel_floating { "\u{25a3} Dock" } else { "\u{29c9} Float" })
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = float_entity.upgrade() {
                        e.update(app, |this, cx| {
                            this.toggle_tool_panel_floating();
                            cx.notify();
                        });
                    }
                }),
        );
        let refresh_entity = entity.clone();
        actions = actions.child(
            Button::new("pr-refresh")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child("Refresh")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = refresh_entity.upgrade() {
                        e.update(app, |this, cx| {
                            match this.pr.open {
                                Some(n) => this.refresh_open_pr(n),
                                None => this.spawn_pr_branch_list(),
                            }
                            cx.notify();
                        });
                    }
                }),
        );
        header_row = header_row.child(div().flex_1().flex().justify_end().child(actions));

        let body = match self.pr.open {
            Some(_) => self.render_pr_detail(&theme, entity.clone(), cx),
            None => {
                let empty = match &self.pr.branch {
                    Some(b) => format!("No pull request for branch \u{201c}{b}\u{201d}."),
                    None => "Not on a branch.".to_string(),
                };
                self.pr_list_body(&self.pr.branch_list, "pr-branch-row", &empty, &theme, entity.clone())
            }
        };

        crate::pr_ui::tool_panel_overlay(
            self.tool_panel_floating,
            self.tool_panel_w,
            header_row.into_any_element(),
            body.into_any_element(),
        )
    }

    /// Build the holistic Pull Requests page overlay (all open PRs for the
    /// repo). Full content-area card like Cleanup/Settings; clicking a row
    /// opens the shared detail view, with a Back button to the list.
    pub fn render_all_prs(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        // Match cleanup_ui's content-area insets so the page tracks resizes.
        let pad = crate::workspace::AREA_PAD;
        let sidebar = self.sidebar_w();
        let left = if sidebar == 0.0 { pad } else { sidebar };
        let right = self.right_w() + pad;

        let mut header_row = div().flex().flex_row().items_center().w_full().gap_2();
        if self.pr.open.is_some() {
            let back_entity = entity.clone();
            header_row = header_row.child(
                Button::new("all-pr-back")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child("\u{2190} All PRs")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = back_entity.upgrade() {
                            e.update(app, |this, cx| {
                                this.close_pr_detail();
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        header_row = header_row.child(CardTitle::new().child(match self.pr.open {
            Some(n) => format!("PR #{n}"),
            None => "Pull Requests".into(),
        }));
        // Right-aligned actions: optional "Open in GitHub" (when a detail is
        // loaded), then Refresh.
        let mut actions = div().flex().flex_row().items_center().gap_2();
        if self.pr.open.is_some() {
            if let Load::Ready(d) = &self.pr.detail {
                let url = d.url.clone();
                actions = actions.child(
                    Button::new("all-pr-open-gh")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .child("\u{2197} GitHub")
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, _app: &mut GpuiApp| {
                            crate::links::open(&url);
                        }),
                );
            }
        }
        let refresh_entity = entity.clone();
        actions = actions.child(
            Button::new("all-pr-refresh")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child("Refresh")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = refresh_entity.upgrade() {
                        e.update(app, |this, cx| {
                            match this.pr.open {
                                Some(n) => this.refresh_open_pr(n),
                                None => this.spawn_pr_all_list(),
                            }
                            cx.notify();
                        });
                    }
                }),
        );
        header_row = header_row.child(div().flex_1().flex().justify_end().child(actions));

        let body = match self.pr.open {
            Some(_) => self.render_pr_detail(&theme, entity.clone(), cx),
            None => self.pr_list_body(
                &self.pr.all_list,
                "all-pr-row",
                "No open pull requests.",
                &theme,
                entity.clone(),
            ),
        };

        div()
            .absolute()
            .left(px(left))
            .top(px(pad))
            .right(px(right))
            .bottom(px(pad))
            .child(
                Card::new()
                    .h_full()
                    .child(CardHeader::new().child(header_row))
                    .child(CardContent::new().flex_1().child(body)),
            )
            .into_any_element()
    }

    /// Render a PR list (branch-scoped or all), or its loading/empty/failed
    /// state. `id` namespaces the scroller + row ids so the two surfaces never
    /// collide. Shared by the Sessions tool and the Pull Requests page.
    fn pr_list_body(
        &self,
        list: &Load<Vec<PrSummary>>,
        id: &'static str,
        empty: &str,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
    ) -> AnyElement {
        let prs = match list {
            Load::Idle | Load::Loading => return skeleton_list(),
            Load::Failed(e) => return failed(e, theme),
            Load::Ready(prs) if prs.is_empty() => {
                return centered(empty, theme.muted_foreground);
            }
            Load::Ready(prs) => prs,
        };

        let mut list = div()
            .id(id)
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll();
        for (ix, pr) in prs.iter().enumerate() {
            let number = pr.number;
            let row_entity = entity.clone();
            let state_badge = state_badge(&pr.state, pr.is_draft, theme);
            list = list.child(
                div()
                    .id((id, ix))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.accent))
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = row_entity.upgrade() {
                            e.update(app, |this, cx| {
                                this.open_pr(number);
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("#{number}")),
                            )
                            .child(state_badge)
                            .child(div().flex_1().truncate().child(pr.title.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", pr.author, pr.head)),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_pr_detail(
        &self,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = match &self.pr.detail {
            Load::Idle | Load::Loading => return skeleton_list(),
            Load::Failed(e) => return failed(e, theme),
            Load::Ready(d) => d,
        };

        // Header block: title, state, author, branches, counts, review.
        let mut meta = div().flex().flex_row().items_center().gap_2().flex_wrap();
        meta = meta.child(state_badge(&detail.state, detail.is_draft, theme));
        meta = meta.child(
            div()
                .text_color(theme.muted_foreground)
                .child(format!("@{}", detail.author)),
        );
        // Branch chip: head → base, rendered as a subtle monospace pill.
        meta = meta.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(6.))
                .bg(theme.muted)
                .text_size(px(12.))
                .text_color(theme.foreground)
                .child(format!("{} → {}", detail.head, detail.base)),
        );
        meta = meta.child(
            div()
                .text_color(green(theme.dark))
                .child(format!("+{}", detail.additions)),
        );
        meta = meta.child(
            div()
                .text_color(red(theme.dark))
                .child(format!("−{}", detail.deletions)),
        );
        meta = meta.child(
            div()
                .text_color(theme.muted_foreground)
                .child(format!("{} files", detail.changed_files)),
        );
        if let Some(rd) = &detail.review_decision {
            meta = meta.child(review_badge(rd, theme));
        }
        for label in &detail.labels {
            meta = meta.child(Badge::new().variant(BadgeVariant::Outline).child(label.clone()));
        }

        let title = div()
            .text_size(px(20.))
            .line_height(px(28.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(detail.title.clone());

        // Action buttons.
        let actions = self.render_pr_actions(detail, theme, entity.clone(), cx);

        // Tabs.
        let tabs = self.render_pr_tabs(theme, entity.clone());

        let tab_body = match self.pr.tab {
            PrTab::Conversation => self.render_conversation(detail, theme, entity.clone()),
            PrTab::Files => self.render_files(theme, entity.clone()),
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .h_full()
            .min_h(px(0.))
            .child(title)
            .child(meta)
            .child(actions)
            .when_some(self.pr.action_msg.clone(), |el, msg| {
                el.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child(msg),
                )
            })
            .child(tabs)
            .child(div().flex_1().min_h(px(0.)).child(tab_body))
            .into_any_element()
    }

    fn render_pr_actions(
        &self,
        detail: &PrDetail,
        _theme: &Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let acting = self.pr.acting;
        let mk = |id: &'static str, label: &str, variant: ButtonVariant, action: gh::Action| {
            let e = entity.clone();
            Button::new(id)
                .variant(variant)
                .size(ButtonSize::Sm)
                .disabled(acting)
                .child(label.to_string())
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    let action = action.clone();
                    if let Some(e) = e.upgrade() {
                        e.update(app, move |this, cx| {
                            this.spawn_pr_action(action);
                            cx.notify();
                        });
                    }
                })
        };

        let mut row = div().flex().flex_row().flex_wrap().gap_2().items_center();
        row = row.child(mk("pr-approve", "Approve", ButtonVariant::Default, gh::Action::Approve));
        if detail.is_draft {
            row = row.child(mk("pr-ready", "Ready", ButtonVariant::Secondary, gh::Action::Ready));
        }
        if detail.state == "open" {
            // Merge is irreversible + outward-facing, so it's a two-click arm:
            // the first click asks for confirmation, the second merges.
            if self.pr.merge_confirm {
                let confirm_e = entity.clone();
                let cancel_e = entity.clone();
                row = row
                    .child(
                        Button::new("pr-merge-confirm")
                            .variant(ButtonVariant::Destructive)
                            .size(ButtonSize::Sm)
                            .disabled(acting)
                            .child("Confirm merge")
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                if let Some(e) = confirm_e.upgrade() {
                                    e.update(app, |this, cx| {
                                        this.pr.merge_confirm = false;
                                        this.spawn_pr_action(gh::Action::Merge);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("pr-merge-cancel")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .child("Cancel")
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                if let Some(e) = cancel_e.upgrade() {
                                    e.update(app, |this, cx| {
                                        this.pr.merge_confirm = false;
                                        cx.notify();
                                    });
                                }
                            }),
                    );
            } else {
                let arm_e = entity.clone();
                row = row.child(
                    Button::new("pr-merge")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .disabled(acting)
                        .child("Squash & merge")
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = arm_e.upgrade() {
                                e.update(app, |this, cx| {
                                    this.pr.merge_confirm = true;
                                    cx.notify();
                                });
                            }
                        }),
                );
            }
            let close_e = entity.clone();
            row = row.child(
                Button::new("pr-close")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .disabled(acting)
                    .child("Close")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = close_e.upgrade() {
                            e.update(app, |this, cx| {
                                this.spawn_pr_action(gh::Action::Close);
                                cx.notify();
                            });
                        }
                    }),
            );
        }

        // Comment / request-changes read the shared input box.
        let comment_input = div()
            .flex_1()
            .min_w(px(0.))
            .child(self.pr_comment_input.clone());
        let comment_entity = entity.clone();
        let comment_btn = Button::new("pr-comment")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .disabled(acting)
            .child("Comment")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = comment_entity.upgrade() {
                    e.update(app, |this, cx| {
                        let body = this.pr_comment_input.read(cx).text().trim().to_string();
                        if !body.is_empty() {
                            this.spawn_pr_action(gh::Action::Comment(body));
                            this.pr_comment_input.update(cx, |i, cx| i.set_text("", cx));
                        }
                        cx.notify();
                    });
                }
            });
        let rc_entity = entity.clone();
        let request_btn = Button::new("pr-request")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .disabled(acting)
            .child("Request changes")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = rc_entity.upgrade() {
                    e.update(app, |this, cx| {
                        let body = this.pr_comment_input.read(cx).text().trim().to_string();
                        if !body.is_empty() {
                            this.spawn_pr_action(gh::Action::RequestChanges(body));
                            this.pr_comment_input.update(cx, |i, cx| i.set_text("", cx));
                        }
                        cx.notify();
                    });
                }
            });

        let _ = cx;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(comment_input)
                    .child(comment_btn)
                    .child(request_btn),
            )
            .into_any_element()
    }

    fn render_pr_tabs(&self, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
        let tab = self.pr.tab;
        // GitHub-style underline tabs: an accent bottom border on the active
        // tab, transparent (but same-width) on the rest so the row never shifts.
        let accent = if theme.dark { gpui::rgb(0xf78166) } else { gpui::rgb(0xfd8c73) };
        let mk = |id: &'static str, label: &str, which: PrTab| {
            let active = tab == which;
            let e = entity.clone();
            div()
                .id(id)
                .cursor_pointer()
                .px_2()
                .pb_2()
                .border_b_2()
                .border_color(if active { accent.into() } else { gpui::transparent_black() })
                .text_size(px(14.))
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .when(active, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                .child(label.to_string())
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = e.upgrade() {
                        e.update(app, |this, cx| {
                            this.pr.tab = which;
                            cx.notify();
                        });
                    }
                })
        };
        div()
            .flex()
            .flex_row()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .child(mk("pr-tab-conv", "Conversation", PrTab::Conversation))
            .child(mk("pr-tab-files", "Files changed", PrTab::Files))
            .into_any_element()
    }

    fn render_conversation(
        &self,
        detail: &PrDetail,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
    ) -> AnyElement {
        let acting = self.pr.acting;
        let mut col = div()
            .id("pr-conversation")
            .flex()
            .flex_col()
            .gap_3()
            .h_full()
            .min_h(px(0.))
            .overflow_y_scroll();

        // Everything markdown-heavy is read from the precomputed conversation
        // (parsed + syntax-highlighted once on load), so this render never runs
        // pulldown/syntect — the same discipline the diff viewer uses.
        let conv = self.pr.conversation.as_ref();

        // Context for markdown rendering: live theme, the set of details the
        // user has toggled from their `open` default, and a factory that builds
        // a toggle handler flipping that set (kept out of markdown so it stays
        // App-agnostic).
        let toggle = {
            let entity = entity.clone();
            move |key: u64| -> crate::markdown::OnToggle {
                let entity = entity.clone();
                Box::new(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = entity.upgrade() {
                        e.update(app, |this, cx| {
                            if !this.pr.md_flipped.insert(key) {
                                this.pr.md_flipped.remove(&key);
                            }
                            cx.notify();
                        });
                    }
                })
            }
        };
        let seq = std::cell::Cell::new(0u64);
        let ctx =
            crate::markdown::MdCtx { theme, flipped: &self.pr.md_flipped, toggle: &toggle, seq: &seq };

        // Every item is `flex_none`: in this fixed-height scrolling column,
        // flexbox would otherwise shrink each child to fit the viewport, and an
        // rcn `Card` (which clips with `overflow_hidden`) would then cut off its
        // content instead of growing. Natural-height children + column scroll.
        let item = |el: AnyElement| div().flex_none().child(el).into_any_element();

        // PR body reads as the first card in the stream (like GitHub).
        let has_body = conv.is_some_and(|c| c.body.is_some());
        if let Some(body) = conv.and_then(|c| c.body.as_ref()) {
            col = col.child(item(comment_card(&detail.author, None, &detail.created_at, body, &ctx)));
        }

        // Unified, chronologically-ordered timeline: comments, review threads,
        // and events interleaved (reviewed events filtered, commits grouped).
        let entries: &[PreparedEntry] = conv.map(|c| c.entries.as_slice()).unwrap_or(&[]);
        let has_timeline = !entries.is_empty();
        for (ix, entry) in entries.iter().enumerate() {
            col = col.child(item(match entry {
                PreparedEntry::Comment { author, review_state, created_at, body } => {
                    comment_card(author, review_state.as_deref(), created_at, body, &ctx)
                }
                PreparedEntry::Thread(t) => review_thread_card(ix, t, &ctx, entity.clone(), acting),
                PreparedEntry::Event(e) => event_row(e, theme),
                PreparedEntry::CommitGroup(evs) => commit_group_entry(evs, theme),
            }));
        }

        // Checks + a GitHub-style merge-status line at the foot of the stream.
        if !detail.checks.is_empty() {
            col = col.child(item(checks_summary(&detail.checks, theme)));
        }
        if detail.state == "open" {
            col = col.child(item(merge_status_line(detail, theme)));
        }

        if !has_body && !has_timeline && detail.checks.is_empty() {
            col = col.child(centered("No conversation yet.", theme.muted_foreground));
        }

        col.into_any_element()
    }

    fn render_files(&self, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
        let threads: &[gh::ReviewThread] = if let Load::Ready(d) = &self.pr.detail {
            &d.threads
        } else {
            &[]
        };
        match &self.pr.diff {
            Load::Idle | Load::Loading => skeleton_list(),
            Load::Failed(e) => failed(e, theme),
            Load::Ready(r) if r.is_empty() => {
                centered("No file changes.", theme.muted_foreground)
            }
            Load::Ready(r) => self.render_diff_files(r, DiffSurface::Pr, threads, theme, entity),
        }
    }

    // ── Diff files-view state (collapse/viewed, shared by PR + local diff) ──

    fn files_view_mut(&mut self, s: DiffSurface) -> &mut DiffViewState {
        match s {
            DiffSurface::Pr => &mut self.pr.files_view,
            DiffSurface::LocalDiff => &mut self.local_diff.files_view,
        }
    }

    fn files_view(&self, s: DiffSurface) -> &DiffViewState {
        match s {
            DiffSurface::Pr => &self.pr.files_view,
            DiffSurface::LocalDiff => &self.local_diff.files_view,
        }
    }

    /// The currently-loaded diff render for a surface, if any.
    fn diff_render(&self, s: DiffSurface) -> Option<Arc<DiffRender>> {
        match s {
            DiffSurface::Pr => match &self.pr.diff {
                Load::Ready(r) => Some(r.clone()),
                _ => None,
            },
            DiffSurface::LocalDiff => match &self.local_diff.data {
                Load::Ready(d) => Some(d.render.clone()),
                _ => None,
            },
        }
    }

    /// Expand the next chunk of an elided context region. Highlights the new
    /// lines once here (not per frame) and caches them in the view state.
    fn diff_expand_gap(
        &mut self,
        surface: DiffSurface,
        file_ix: usize,
        old_start: u32,
        new_start: u32,
        count: u32,
    ) {
        const CHUNK: u32 = 20;
        // Clone the Arc so the immutable borrow ends before mutating state.
        let Some(render) = self.diff_render(surface) else {
            return;
        };
        let Some(file) = render.files.get(file_ix) else {
            return;
        };
        let Some(lines) = file.new_lines.clone() else {
            return;
        };
        let ext = file_ext(&file.path);
        let key = format!("{}:{}", file.path, new_start);
        let dark = crate::theme::dark_active();
        let already = self
            .files_view(surface)
            .expanded
            .get(&key)
            .map(|v| v.len() as u32)
            .unwrap_or(0);
        let remaining = count.saturating_sub(already);
        if remaining == 0 {
            return;
        }
        let take = remaining.min(CHUNK);
        let base = (new_start - 1 + already) as usize; // 0-indexed new-side start
        let mut hl = crate::highlight::LineHighlighter::new(&ext, dark);
        let mut new_rows = Vec::new();
        for k in 0..take {
            let Some(text) = lines.get(base + k as usize) else {
                break;
            };
            let runs = spans_to_runs(&hl.highlight(text));
            new_rows.push(DiffRow::Line {
                kind: LineKind::Context,
                old: Some(old_start + already + k),
                new: Some(new_start + already + k),
                text: text.clone().into(),
                runs,
            });
        }
        self.files_view_mut(surface)
            .expanded
            .entry(key)
            .or_default()
            .extend(new_rows);
        self.request_redraw();
    }

    fn diff_toggle_collapsed(&mut self, s: DiffSurface, path: &str) {
        let v = self.files_view_mut(s);
        if !v.collapsed.remove(path) {
            v.collapsed.insert(path.to_string());
        }
        self.request_redraw();
    }

    fn diff_toggle_viewed(&mut self, s: DiffSurface, path: &str) {
        let v = self.files_view_mut(s);
        if v.viewed.remove(path) {
            // unviewing: leave collapse as-is
        } else {
            v.viewed.insert(path.to_string());
            // marking viewed also collapses
            v.collapsed.insert(path.to_string());
        }
        self.request_redraw();
    }

    fn diff_expand_all(&mut self, s: DiffSurface) {
        self.files_view_mut(s).collapsed.clear();
        self.request_redraw();
    }

    fn diff_collapse_all(&mut self, s: DiffSurface, paths: Vec<String>) {
        let v = self.files_view_mut(s);
        v.collapsed.extend(paths);
        self.request_redraw();
    }

    fn diff_toggle_sidebar(&mut self, s: DiffSurface) {
        let v = self.files_view_mut(s);
        v.sidebar_open = !v.sidebar_open;
        self.request_redraw();
    }

    /// Select a file from the sidebar: expand its card and mark it active.
    fn diff_select_file(&mut self, s: DiffSurface, path: &str) {
        let v = self.files_view_mut(s);
        v.collapsed.remove(path);
        v.active_file = Some(path.to_string());
        self.request_redraw();
    }

    /// Build one file card's header row (chevron + status tag + path + thread
    /// badge + ± counts + Viewed checkbox).
    ///
    /// `id_prefix` disambiguates the element ids so the sticky-overlay clone
    /// doesn't collide with the in-card header. Id scheme: the header div uses
    /// `(id_prefix, ix)` and the Viewed checkbox uses `(id_prefix, ix +
    /// 1_000_000)`; with distinct prefixes for the real and sticky headers this
    /// keeps all four ids unique within a frame (no file list reaches 1M files).
    #[allow(clippy::too_many_arguments)]
    fn diff_file_header_row(
        &self,
        ix: usize,
        f: &FileDiff,
        collapsed: bool,
        viewed: bool,
        thread_count: usize,
        theme: &Theme,
        colors: &DiffColors,
        entity: &gpui::WeakEntity<App>,
        surface: DiffSurface,
        id_prefix: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        let chevron = if collapsed { "▸" } else { "▾" };
        let mut header = div()
            .id((id_prefix, ix))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .cursor_pointer();
        if viewed {
            // Subtle emerald tint + left accent when the file is marked viewed.
            header = header
                .bg(gpui::rgba(0x2ea04318))
                .border_l_2()
                .border_color(green(theme.dark));
        }
        header = header
            .child(div().flex_none().text_size(px(11.)).text_color(theme.muted_foreground).child(chevron))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme.muted_foreground)
                    .child(f.status_tag),
            );
        let mut path_el = div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(12.))
            .child(f.display_path.clone());
        if viewed {
            path_el = path_el.text_color(theme.muted_foreground).line_through();
        }
        header = header.child(path_el);
        if thread_count > 0 {
            header = header.child(Badge::new().child(format!("💬 {thread_count}")));
        }
        if f.binary {
            header = header
                .child(div().flex_none().text_size(px(11.)).text_color(theme.muted_foreground).child("binary"));
        } else {
            header = header
                .child(div().flex_none().text_size(px(11.)).text_color(colors.green).child(format!("+{}", f.add)))
                .child(div().flex_none().text_size(px(11.)).text_color(colors.red).child(format!("−{}", f.del)));
        }
        // ── Viewed checkbox (stop propagation so it doesn't toggle collapse) ──
        {
            let e = entity.clone();
            let path = f.path.clone();
            header = header.child(
                div()
                    .id((id_prefix, ix + 1_000_000))
                    .flex_none()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(if viewed { colors.green } else { theme.muted_foreground })
                    .child(if viewed { "☑ Viewed" } else { "☐ Viewed" })
                    .on_mouse_down(gpui::MouseButton::Left, move |_ev, _win, app: &mut GpuiApp| {
                        app.stop_propagation();
                    })
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        let path = path.clone();
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, _cx| {
                                this.diff_toggle_viewed(surface, &path);
                            });
                        }
                    }),
            );
        }
        // Toggle collapse when the header (not the checkbox) is clicked.
        let e = entity.clone();
        let path = f.path.clone();
        header = header.on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
            let path = path.clone();
            if let Some(e) = e.upgrade() {
                e.update(app, move |this, _cx| {
                    this.diff_toggle_collapsed(surface, &path);
                });
            }
        });
        header
    }

    /// Render the shared "Files changed" experience: a summary bar plus one
    /// collapsible bordered card per file, with a per-file "viewed" checkbox and
    /// inline review-thread comments anchored to diff lines. Replaces the old
    /// whole-diff `uniform_list` — collapsing is per file, so only expanded
    /// files build their bodies each frame.
    pub(crate) fn render_diff_files(
        &self,
        render: &DiffRender,
        surface: DiffSurface,
        threads: &[gh::ReviewThread],
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
    ) -> AnyElement {
        let view = match surface {
            DiffSurface::Pr => &self.pr.files_view,
            DiffSurface::LocalDiff => &self.local_diff.files_view,
        };
        let colors = DiffColors::new(theme);
        let acting = self.pr.acting;

        let n = render.files.len();
        let total_add: u32 = render.files.iter().map(|f| f.add).sum();
        let total_del: u32 = render.files.iter().map(|f| f.del).sum();
        let viewed_count = render.files.iter().filter(|f| view.viewed.contains(&f.path)).count();

        // "expandable" = non-binary files with at least one row.
        let expandable: Vec<&FileDiff> =
            render.files.iter().filter(|f| !f.binary && !f.rows.is_empty()).collect();
        let any_expandable_collapsed =
            expandable.iter().any(|f| view.collapsed.contains(&f.path));
        // Collapse-all is offered when everything is currently expanded.
        let show_collapse_all = !any_expandable_collapsed;
        let all_paths: Vec<String> = expandable.iter().map(|f| f.path.clone()).collect();

        // ── Summary bar (stays put; does not scroll) ──
        let mut summary = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .text_size(px(12.));
        {
            // Sidebar show/hide toggle (leads the bar like GitHub's file-tree button).
            let e = entity.clone();
            summary = summary.child(
                Button::new("diff-sidebar-toggle")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child(if view.sidebar_open { "◧ Files" } else { "▸ Files" })
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, _cx| this.diff_toggle_sidebar(surface));
                        }
                    }),
            );
        }
        summary = summary
            .child(
                div()
                    .text_color(theme.foreground)
                    .child(format!("{n} file{} changed", if n == 1 { "" } else { "s" })),
            )
            .child(div().text_color(colors.green).child(format!("+{total_add}")))
            .child(div().text_color(colors.red).child(format!("−{total_del}")));
        if !threads.is_empty() {
            summary = summary.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} threads", threads.len())),
            );
        }
        summary = summary
            .child(div().flex_1().min_w(px(0.)))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("{viewed_count}/{n} viewed")),
            );
        {
            let e = entity.clone();
            let paths = all_paths.clone();
            summary = summary.child(
                Button::new("diff-expand-all")
                    .variant(ButtonVariant::Ghost)
                    .child(if show_collapse_all { "Collapse all" } else { "Expand all" })
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        let paths = paths.clone();
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, _cx| {
                                if show_collapse_all {
                                    this.diff_collapse_all(surface, paths);
                                } else {
                                    this.diff_expand_all(surface);
                                }
                            });
                        }
                    }),
            );
        }

        // ── File cards ──
        let mut cards = div()
            .id("diff-files")
            .track_scroll(&view.scroll)
            .flex()
            .flex_col()
            .gap_2()
            .h_full()
            .min_h(px(0.))
            .overflow_y_scroll();
        for (ix, f) in render.files.iter().enumerate() {
            let collapsed = view.collapsed.contains(&f.path);
            let viewed = view.viewed.contains(&f.path);
            let file_threads: Vec<&gh::ReviewThread> =
                threads.iter().filter(|t| t.path == f.path).collect();
            let thread_count = file_threads.len();

            // ── Header (clickable → toggle collapse) ──
            let header = self.diff_file_header_row(
                ix,
                f,
                collapsed,
                viewed,
                thread_count,
                theme,
                &colors,
                &entity,
                surface,
                "diff-file",
            );

            let mut card = div()
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .overflow_hidden()
                .flex()
                .flex_col()
                .flex_none()
                .child(header);

            // ── Body (only when expanded) ──
            if !collapsed {
                let mut body = div().flex().flex_col();
                if f.binary {
                    body = body.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_size(px(11.))
                            .text_color(theme.muted_foreground)
                            .child("binary file"),
                    );
                } else {
                    let mut matched: Vec<usize> = Vec::new();
                    for row in &f.rows {
                        match row {
                            DiffRow::ExpandGap {
                                file_ix,
                                old_start,
                                new_start,
                                count,
                                section,
                            } => {
                                // Already-expanded context (highlighted once at
                                // click time) renders first; the control stays
                                // until the whole gap has been revealed.
                                let key = format!("{}:{}", f.path, new_start);
                                let shown = if let Some(rows) = view.expanded.get(&key) {
                                    for r in rows {
                                        body = body.child(render_diff_row(r, &colors));
                                    }
                                    rows.len() as u32
                                } else {
                                    0
                                };
                                if shown < *count {
                                    body = body.child(expand_gap_control(
                                        *file_ix,
                                        *old_start,
                                        *new_start,
                                        *count,
                                        shown,
                                        section,
                                        surface,
                                        &colors,
                                        entity.clone(),
                                    ));
                                }
                            }
                            _ => {
                                body = body.child(render_diff_row(row, &colors));
                                if let DiffRow::Line { new: Some(nn), .. } = row {
                                    for (ti, t) in file_threads.iter().enumerate() {
                                        if t.line == Some(*nn) {
                                            matched.push(ti);
                                            body = body.child(inline_thread_card(
                                                ix * 1000 + ti,
                                                t,
                                                theme,
                                                entity.clone(),
                                                acting,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Any thread whose line was None or didn't match a rendered
                    // line still gets shown so nothing is dropped.
                    for (ti, t) in file_threads.iter().enumerate() {
                        if !matched.contains(&ti) {
                            body = body.child(inline_thread_card(
                                ix * 1000 + ti,
                                t,
                                theme,
                                entity.clone(),
                                acting,
                            ));
                        }
                    }
                }
                card = card.child(body);
            }

            cards = cards.child(card);
        }

        // ── Sticky header for the topmost visible file ──
        // gpui at this rev has no `position: sticky`, so we clone the header of
        // the file that `ScrollHandle::top_item()` reports as topmost and float
        // it absolutely over the (relative) scroll area. On the very first frame
        // child bounds are empty: `top_item()` is 0 and `bounds_for_item` is
        // None, which simply pins file 0 with no shove.
        const STICKY_HEADER_H: f32 = 28.0; // approximate header row height (px)
        let sticky: Option<(gpui::Stateful<gpui::Div>, f32)> = if render.files.is_empty() {
            None
        } else {
            let top = view.scroll.top_item().min(render.files.len() - 1);
            let f = &render.files[top];
            let collapsed = view.collapsed.contains(&f.path);
            let viewed = view.viewed.contains(&f.path);
            let thread_count = threads.iter().filter(|t| t.path == f.path).count();
            // Shove: as the NEXT card's top approaches the viewport top, push the
            // pinned header up so it slides out exactly as the next file arrives.
            let vp_top = view.scroll.bounds().top();
            let shove = match view.scroll.bounds_for_item(top + 1) {
                Some(next) => (f32::from(next.top() - vp_top) - STICKY_HEADER_H).min(0.0),
                None => 0.0,
            };
            let hdr = self.diff_file_header_row(
                top,
                f,
                collapsed,
                viewed,
                thread_count,
                theme,
                &colors,
                &entity,
                surface,
                "diff-file-sticky",
            );
            Some((hdr, shove))
        };

        let cards_area = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(cards.h_full())
            .when_some(sticky, |el, (hdr, shove)| {
                el.child(
                    div()
                        .absolute()
                        .top(px(shove))
                        .left_0()
                        .right_0()
                        // Opaque card fill so scrolled lines don't bleed through.
                        .bg(theme.card)
                        .border_b_1()
                        .border_color(theme.border)
                        .shadow_sm()
                        .child(hdr),
                )
            });

        let main = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .min_h(px(0.))
            .child(summary)
            .child(cards_area);

        let sidebar = view
            .sidebar_open
            .then(|| diff_sidebar(render, threads, view, surface, theme, entity.clone()));

        div()
            .flex()
            .flex_row()
            .gap_2()
            .h_full()
            .min_h(px(0.))
            .when_some(sidebar, |el, sb| el.child(sb))
            .child(main)
            .into_any_element()
    }
}

/// Which diff surface a view-state action targets.
#[derive(Clone, Copy)]
pub enum DiffSurface {
    Pr,
    LocalDiff,
}

/// Per-surface collapse/viewed state for the files-changed diff cards.
pub struct DiffViewState {
    /// File paths whose cards are collapsed.
    pub collapsed: std::collections::HashSet<String>,
    /// File paths marked "viewed".
    pub viewed: std::collections::HashSet<String>,
    /// Whether the file-tree navigation sidebar is showing.
    pub sidebar_open: bool,
    /// The file last selected in the sidebar, highlighted there.
    pub active_file: Option<String>,
    /// Whether the initial collapse state has been seeded for the loaded diff.
    /// Seeding runs once per diff so a mid-review refresh (e.g. resolving a
    /// thread re-fetches the diff) never discards the user's expand/collapse.
    pub seeded: bool,
    /// Scroll handle for the files-changed cards container, so sidebar file
    /// links can scroll their card into view.
    pub scroll: gpui::ScrollHandle,
    /// Expanded elided-context regions: `"{path}:{new_start}"` → the already
    /// fetched + highlighted context rows to render before the (remaining)
    /// control.
    pub expanded: std::collections::HashMap<String, Vec<DiffRow>>,
}

impl Default for DiffViewState {
    fn default() -> Self {
        DiffViewState {
            collapsed: std::collections::HashSet::new(),
            viewed: std::collections::HashSet::new(),
            sidebar_open: true,
            active_file: None,
            seeded: false,
            scroll: gpui::ScrollHandle::new(),
            expanded: std::collections::HashMap::new(),
        }
    }
}

impl DiffViewState {
    /// Cumulative diff rows to leave expanded before defaulting the rest to
    /// collapsed, and the per-file cap above which a file starts collapsed. The
    /// files view is not virtualized (inline threads break uniform rows), so
    /// this bounds how many line divs an untouched diff paints per frame.
    const EXPAND_ROW_BUDGET: usize = 500;
    const PER_FILE_EXPAND_CAP: usize = 80;

    /// Seed the initial collapse state once: keep small leading files expanded
    /// up to a row budget, collapse the rest (and any oversized file).
    pub(crate) fn seed(&mut self, files: &[FileDiff]) {
        if self.seeded {
            return;
        }
        self.seeded = true;
        let mut budget = Self::EXPAND_ROW_BUDGET;
        for f in files {
            let rows = f.rows.len();
            let fits = !f.binary && rows <= Self::PER_FILE_EXPAND_CAP && rows <= budget;
            if fits {
                budget = budget.saturating_sub(rows);
            } else {
                self.collapsed.insert(f.path.clone());
            }
        }
    }
}

// ── Shared diff rendering (also used by the local diff tool) ───────────────
//
// Diffs are precomputed once (off the UI thread) into fixed-height rows with
// cached colour runs — no per-frame syntect, no wrapping-text layout — then
// grouped into collapsible per-file cards. Collapsing is per file (so inline
// review threads can interleave), and only expanded files build their bodies;
// `DiffViewState::seed` keeps the initially-expanded set within a row budget so
// an untouched diff never paints thousands of line divs on the mouse-move
// re-render path.

/// Cap on lines kept per file when precomputing; the overflow is summarized.
/// Only bounds one-time build cost + memory (rendering is gated by collapse).
const MAX_LINES_PER_FILE: usize = 1000;
/// Fixed row height (logical px) for a diff line.
const DIFF_ROW_H: f32 = 18.0;

/// One precomputed, ready-to-render diff row. Highlighting is resolved once at
/// build time into byte-range colour runs, so rendering never re-runs syntect.
#[derive(Clone, Debug)]
pub enum DiffRow {
    Hunk(SharedString),
    Line {
        kind: LineKind,
        old: Option<u32>,
        new: Option<u32>,
        text: SharedString,
        runs: Vec<(Range<usize>, gpui::Hsla)>,
    },
    /// A compacted region of unchanged lines that can be expanded on click.
    ExpandGap {
        file_ix: usize,              // index into DiffRender.files (keying + new_lines access)
        old_start: u32,              // first hidden old-side line number
        new_start: u32,              // first hidden new-side line number
        count: u32,                  // number of hidden lines
        section: gpui::SharedString, // parent scope of the code just below the gap
    },
    Truncated,
}

/// One file's precomputed diff: metadata for the card header plus its body rows
/// (Hunk/Line/Truncated — the file header is no longer a row, it lives here).
#[derive(Clone, Debug)]
pub struct FileDiff {
    pub path: String,           // full path, tree key (use file.path)
    pub display_path: String,   // rename-aware (file.display_path())
    pub status_tag: &'static str,
    pub add: u32,
    pub del: u32,
    pub binary: bool,
    pub rows: Vec<DiffRow>,     // Hunk | Line | Truncated (no File)
    /// New-side full file content, when the producer could fetch it. Enables
    /// expanding the elided context regions; `None` disables expansion.
    pub new_lines: Option<std::sync::Arc<Vec<String>>>,
}

/// A whole diff as per-file structures. Collapsing is per file, so the diff is
/// no longer flattened into one virtualized list.
#[derive(Clone, Debug)]
pub struct DiffRender {
    pub files: Vec<FileDiff>,
}

impl DiffRender {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A local diff plus the label it was diffed against.
#[derive(Clone, Debug)]
pub struct LocalDiffRender {
    pub base_ref: String,
    pub render: Arc<DiffRender>,
}

/// Flatten + syntax-highlight a set of diff files into render rows. Runs
/// syntect once per line — call this off the UI thread.
pub fn build_diff_render(files: &[DiffFile], dark: bool) -> DiffRender {
    let mut out = Vec::with_capacity(files.len());
    for (file_ix, file) in files.iter().enumerate() {
        let mut rows = Vec::new();
        if !file.binary {
            let mut rendered = 0usize;
            // Total new-side line count, when the producer supplied the file
            // content; `None` means expansion is unavailable → emit no gaps.
            let total = file.new_lines.as_ref().map(|l| l.len() as u32);
            let mut prev_last_old = 0u32;
            let mut prev_last_new = 0u32;
            let mut truncated = false;
            'file: for hunk in &file.hunks {
                // First/last line numbers actually present on this hunk.
                let (mut first_new, mut last_old, mut last_new) = (u32::MAX, 0u32, 0u32);
                for line in &hunk.lines {
                    if let Some(n) = line.old_no {
                        last_old = last_old.max(n);
                    }
                    if let Some(n) = line.new_no {
                        first_new = first_new.min(n);
                        last_new = last_new.max(n);
                    }
                }
                // Leading elided region between the previous hunk and this one.
                if total.is_some() && first_new != u32::MAX {
                    let count = first_new.saturating_sub(prev_last_new + 1);
                    if count > 0 {
                        rows.push(DiffRow::ExpandGap {
                            file_ix,
                            old_start: prev_last_old + 1,
                            new_start: prev_last_new + 1,
                            count,
                            section: SharedString::from(hunk.section().to_string()),
                        });
                    }
                }
                prev_last_old = prev_last_old.max(last_old);
                prev_last_new = prev_last_new.max(last_new);
                rows.push(DiffRow::Hunk(hunk.header.clone().into()));
                // Fresh highlighter per hunk bounds state drift across gaps.
                let mut hl = crate::highlight::LineHighlighter::new(&file.extension(), dark);
                for line in &hunk.lines {
                    if rendered >= MAX_LINES_PER_FILE {
                        rows.push(DiffRow::Truncated);
                        truncated = true;
                        break 'file;
                    }
                    let runs = spans_to_runs(&hl.highlight(&line.text));
                    rows.push(DiffRow::Line {
                        kind: line.kind,
                        old: line.old_no,
                        new: line.new_no,
                        text: line.text.clone().into(),
                        runs,
                    });
                    rendered += 1;
                }
            }
            // Trailing elided region after the last hunk.
            if let Some(total) = total {
                if !truncated && prev_last_new > 0 {
                    let count = total.saturating_sub(prev_last_new);
                    if count > 0 {
                        rows.push(DiffRow::ExpandGap {
                            file_ix,
                            old_start: prev_last_old + 1,
                            new_start: prev_last_new + 1,
                            count,
                            section: "".into(),
                        });
                    }
                }
            }
        }
        out.push(FileDiff {
            path: file.path.clone(),
            display_path: file.display_path(),
            status_tag: file.status.tag(),
            add: file.additions,
            del: file.deletions,
            binary: file.binary,
            rows,
            new_lines: file.new_lines.clone(),
        });
    }
    DiffRender { files: out }
}

/// Lowercased extension of a path for the syntax highlighter, mirroring
/// `DiffFile::extension` (falls back to the file name when there is no dot).
fn file_ext(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_lowercase(),
        _ => name.to_lowercase(),
    }
}

/// Convert consecutive highlight spans into byte-range → colour runs for
/// `StyledText::with_highlights`.
fn spans_to_runs(spans: &[crate::highlight::Span]) -> Vec<(Range<usize>, gpui::Hsla)> {
    let mut runs = Vec::with_capacity(spans.len());
    let mut ix = 0usize;
    for s in spans {
        let len = s.text.len();
        if len > 0 {
            runs.push((ix..ix + len, s.color.into()));
        }
        ix += len;
    }
    runs
}

/// Theme-derived colours, captured once and copied into the virtualized list
/// closure so it stays `'static`.
#[derive(Clone, Copy)]
struct DiffColors {
    add_bg: gpui::Hsla,
    remove_bg: gpui::Hsla,
    hunk_bg: gpui::Hsla,
    muted: gpui::Hsla,
    foreground: gpui::Hsla,
    green: gpui::Hsla,
    red: gpui::Hsla,
}

impl DiffColors {
    fn new(theme: &Theme) -> Self {
        DiffColors {
            add_bg: add_bg(theme.dark).into(),
            remove_bg: remove_bg(theme.dark).into(),
            hunk_bg: hunk_bg(theme.dark).into(),
            muted: theme.muted_foreground,
            foreground: theme.foreground,
            green: green(theme.dark),
            red: red(theme.dark),
        }
    }
}

fn diff_gutter(n: Option<u32>, colors: &DiffColors) -> gpui::Div {
    div()
        .w(px(34.))
        .flex_none()
        .text_size(px(11.))
        .text_color(colors.muted)
        .child(n.map(|v| v.to_string()).unwrap_or_default())
}

/// Clickable control for an elided context region: reveals the next chunk of
/// hidden lines (up to 20) and shows the parent scope of the code below.
#[allow(clippy::too_many_arguments)]
fn expand_gap_control(
    file_ix: usize,
    old_start: u32,
    new_start: u32,
    count: u32,
    shown: u32,
    section: &gpui::SharedString,
    surface: DiffSurface,
    colors: &DiffColors,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let remaining = count.saturating_sub(shown);
    let mut el = div()
        .id(("diff-expand", file_ix * 100_000 + new_start as usize))
        .h(px(DIFF_ROW_H))
        .w_full()
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .bg(colors.hunk_bg)
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(11.))
        .text_color(colors.muted)
        .child(
            div()
                .flex_none()
                .child(format!("⋯ Expand {} lines ⋯", remaining.min(20))),
        );
    if !section.is_empty() {
        el = el.child(
            div()
                .w_full()
                .truncate()
                .text_color(colors.foreground)
                .child(section.clone()),
        );
    }
    el.on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
        if let Some(e) = entity.upgrade() {
            e.update(app, move |this, _cx| {
                this.diff_expand_gap(surface, file_ix, old_start, new_start, count);
            });
        }
    })
    .into_any_element()
}

fn render_diff_row(row: &DiffRow, colors: &DiffColors) -> AnyElement {
    match row {
        DiffRow::Hunk(text) => div()
            .h(px(DIFF_ROW_H))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .bg(colors.hunk_bg)
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(11.))
            .text_color(colors.muted)
            .child(div().w_full().truncate().child(text.clone()))
            .into_any_element(),
        // Rendered by the file body loop (it owns the expansion state); a bare
        // row here would have no way to reach it.
        DiffRow::ExpandGap { .. } => div().into_any_element(),
        DiffRow::Truncated => div()
            .h(px(DIFF_ROW_H))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .text_size(px(11.))
            .text_color(colors.muted)
            .child("… diff truncated (open the file to see the rest)")
            .into_any_element(),
        DiffRow::Line { kind, old, new, text, runs } => {
            let (bg, sign, sign_color) = match kind {
                LineKind::Add => (Some(colors.add_bg), "+", colors.green),
                LineKind::Remove => (Some(colors.remove_bg), "−", colors.red),
                LineKind::Context => (None, " ", colors.muted),
            };
            let mut styled = StyledText::new(text.clone());
            if !runs.is_empty() {
                styled = styled.with_highlights(
                    runs.iter()
                        .map(|(r, c)| (r.clone(), HighlightStyle { color: Some(*c), ..Default::default() })),
                );
            }
            div()
                .h(px(DIFF_ROW_H))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                // Keep each code line to a single row: nowrap cascades into the
                // StyledText's default style so it doesn't wrap and overflow the
                // fixed-height row (which would stack lines on top of each other).
                .whitespace_nowrap()
                .font_family(crate::renderer::FONT_FAMILY)
                .text_size(px(12.))
                .text_color(colors.foreground)
                .when_some(bg, |el, c| el.bg(c))
                .child(diff_gutter(*old, colors))
                .child(diff_gutter(*new, colors))
                .child(div().w(px(10.)).flex_none().text_color(sign_color).child(sign))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(styled))
                .into_any_element()
        }
    }
}

fn checks_summary(checks: &[Check], theme: &Theme) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme.border);
    col = col.child(
        div()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_size(px(12.))
            .child("Checks"),
    );
    for (ix, c) in checks.iter().enumerate() {
        let (label, color) = check_style(c.status, theme.dark);
        col = col.child(
            div()
                .id(("pr-check", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Badge::new().color(color.into()).child(label))
                .child(div().flex_1().truncate().text_size(px(12.)).child(c.name.clone())),
        );
    }
    col.into_any_element()
}

// ── Precomputed conversation ────────────────────────────────────────────────

/// The open PR's conversation with all markdown parsed and code fences
/// syntax-highlighted once (see [`prepare_conversation`]). Building the gpui
/// element tree from this is cheap, so it's safe on the per-frame render path
/// even though the whole app re-renders on every mouse move.
pub struct PreparedConversation {
    body: Option<crate::markdown::Prepared>,
    entries: Vec<PreparedEntry>,
}

/// One prepared timeline item (mirrors [`gh::TimelineEntry`] with markdown
/// pre-rendered). Events carry no markdown, so they're kept as-is.
enum PreparedEntry {
    Comment {
        author: String,
        review_state: Option<String>,
        created_at: String,
        body: crate::markdown::Prepared,
    },
    Thread(PreparedThread),
    Event(TimelineEvent),
    CommitGroup(Vec<TimelineEvent>),
}

struct PreparedThread {
    id: String,
    path: String,
    line: Option<u32>,
    diff_hunk: String,
    is_resolved: bool,
    is_outdated: bool,
    comments: Vec<PreparedThreadComment>,
}

struct PreparedThreadComment {
    author: String,
    created_at: String,
    body: crate::markdown::Prepared,
}

/// Build the unified timeline and pre-parse + pre-highlight every markdown body
/// once. Called off the render path (on detail load), not per frame.
fn prepare_conversation(detail: &PrDetail, dark: bool) -> PreparedConversation {
    let body = (!detail.body.trim().is_empty())
        .then(|| crate::markdown::prepare(detail.body.trim(), dark));
    let entries = gh::build_timeline(&detail.comments, &detail.threads, &detail.events)
        .into_iter()
        .map(|e| match e {
            gh::TimelineEntry::Comment(c) => PreparedEntry::Comment {
                author: c.author,
                review_state: c.review_state,
                created_at: c.created_at,
                body: crate::markdown::prepare(c.body.trim(), dark),
            },
            gh::TimelineEntry::Thread(t) => PreparedEntry::Thread(PreparedThread {
                id: t.id,
                path: t.path,
                line: t.line,
                diff_hunk: t.diff_hunk,
                is_resolved: t.is_resolved,
                is_outdated: t.is_outdated,
                comments: t
                    .comments
                    .into_iter()
                    .map(|c| PreparedThreadComment {
                        author: c.author,
                        created_at: c.created_at,
                        body: crate::markdown::prepare(c.body.trim(), dark),
                    })
                    .collect(),
            }),
            gh::TimelineEntry::Event(ev) => PreparedEntry::Event(ev),
            gh::TimelineEntry::CommitGroup(evs) => PreparedEntry::CommitGroup(evs),
        })
        .collect();
    PreparedConversation { body, entries }
}

// ── Timeline rendering ──────────────────────────────────────────────────────

fn comment_card(
    author: &str,
    review_state: Option<&str>,
    created_at: &str,
    body: &crate::markdown::Prepared,
    ctx: &crate::markdown::MdCtx,
) -> AnyElement {
    let theme = ctx.theme;
    let mut head = div().flex().flex_row().items_center().gap_2().child(
        div()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_size(px(12.))
            .child(format!("@{author}")),
    );
    if let Some(state) = review_state {
        head = head.child(review_badge(state, theme));
    }
    head = head
        .child(div().flex_1().min_w(px(0.)))
        .child(
            div()
                .flex_none()
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .child(relative_time(created_at)),
        );
    Card::new()
        .size(CardSize::Sm)
        .child(CardHeader::new().size(CardSize::Sm).child(head))
        .child(CardContent::new().size(CardSize::Sm).child(crate::markdown::render_prepared(body, ctx)))
        .into_any_element()
}

/// A collapsed run of consecutive commits, or ≤2 rendered inline. Always
/// expanded here (a terminal panel has no room to hide a toggle affordance).
fn commit_group_entry(events: &[TimelineEvent], theme: &Theme) -> AnyElement {
    let mut col = div().flex().flex_col().gap_1().px_2().py_1();
    if events.len() > 2 {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child(format!("{} commits", events.len())),
        );
    }
    for e in events {
        let sha7: String = e.commit_sha.as_deref().unwrap_or("").chars().take(7).collect();
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .font_family(crate::renderer::FONT_FAMILY)
                        .text_size(px(10.))
                        .text_color(theme.primary)
                        .child(sha7),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child(e.commit_message.clone().unwrap_or_default()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme.muted_foreground)
                        .child(relative_time(&e.created_at)),
                ),
        );
    }
    col.into_any_element()
}

/// A single non-comment timeline event: a colored glyph, the verb phrase, and a
/// relative timestamp.
fn event_row(e: &TimelineEvent, theme: &Theme) -> AnyElement {
    let (glyph, color) = event_glyph(e.kind, theme);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .child(div().flex_none().text_color(color).child(glyph))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child(event_description(e)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .child(relative_time(&e.created_at)),
        )
        .into_any_element()
}

/// Glyph + color for a timeline event kind. Glyphs are common Unicode marks so
/// they render in any font (no nerd-font dependency).
fn event_glyph(kind: EventKind, theme: &Theme) -> (&'static str, gpui::Hsla) {
    let (merged, green_c) = pr_state_colors(theme.dark);
    let yellow = check_style(CheckStatus::Pending, theme.dark).1;
    let muted = theme.muted_foreground;
    match kind {
        EventKind::Committed => ("◦", muted),
        EventKind::ForcePushed => ("↺", yellow.into()),
        EventKind::Merged => ("◆", merged.into()),
        EventKind::ReadyForReview => ("●", green_c.into()),
        EventKind::ConvertToDraft => ("○", muted),
        _ => ("•", muted),
    }
}

/// The English verb phrase for a timeline event (ported from h20's
/// `timelineEventDescription`).
fn event_description(e: &TimelineEvent) -> String {
    let actor = e.actor.as_deref().map(|a| format!("@{a}")).unwrap_or_else(|| "Someone".into());
    let opt = |label: &str, v: &Option<String>| {
        v.as_deref().filter(|s| !s.is_empty()).map(|s| format!("{label}{s}")).unwrap_or_default()
    };
    match e.kind {
        EventKind::Committed => format!(
            "{actor} committed {}{}",
            e.commit_sha.as_deref().unwrap_or(""),
            e.commit_message.as_deref().filter(|s| !s.is_empty()).map(|m| format!(" — {m}")).unwrap_or_default()
        ),
        EventKind::ForcePushed => {
            let range = match (&e.before_sha, &e.after_sha) {
                (Some(b), Some(a)) => format!(" from {b} to {a}"),
                _ => String::new(),
            };
            format!("{actor} force-pushed the branch{range}")
        }
        EventKind::Reviewed => format!("{actor} reviewed"),
        EventKind::ReviewRequested => {
            format!("{actor} requested a review{}", opt(" from @", &e.reviewer))
        }
        EventKind::ReviewRequestRemoved => {
            format!("{actor} removed review request{}", opt(" from @", &e.reviewer))
        }
        EventKind::Merged => format!("{actor} merged this pull request"),
        EventKind::HeadRefDeleted => format!("{actor} deleted the head branch"),
        EventKind::Labeled => format!("{actor} added the {} label", e.label.as_deref().unwrap_or("")),
        EventKind::Unlabeled => format!("{actor} removed the {} label", e.label.as_deref().unwrap_or("")),
        EventKind::Assigned => format!("{actor} assigned @{}", e.assignee.as_deref().unwrap_or("")),
        EventKind::Unassigned => format!("{actor} unassigned @{}", e.assignee.as_deref().unwrap_or("")),
        EventKind::AutoMergeEnabled => format!("{actor} enabled auto-merge"),
        EventKind::AutoMergeDisabled => format!("{actor} disabled auto-merge"),
        EventKind::Renamed => format!(
            "{actor} changed the title{}{}",
            opt(" from \"", &e.previous_title.as_ref().map(|t| format!("{t}\""))),
            opt(" to \"", &e.current_title.as_ref().map(|t| format!("{t}\""))),
        ),
        EventKind::BaseRefChanged => format!("{actor} changed the base branch"),
        EventKind::ConvertToDraft => format!("{actor} converted this to a draft"),
        EventKind::ReadyForReview => format!("{actor} marked this as ready for review"),
    }
}

/// A review thread: file:line header with resolved/outdated pills and a
/// resolve/unresolve button, the diff hunk it hangs off, and its comments.
/// Rendered always-expanded (see [`commit_group_entry`] for the rationale).
/// A compact, self-contained review-thread card rendered inline under the diff
/// line it's anchored to. Unlike `review_thread_card` it's driven directly by a
/// `gh::ReviewThread` (no `PreparedThread`/markdown) and left-indented so it sits
/// beneath the diff gutter. Bodies are shown as plain text.
/// The file-tree navigation sidebar for the files-changed view: a GitHub-style
/// tree of the changed files with per-file +/- counts and thread-count badges.
/// Clicking a file expands its card and marks it active. Directories always
/// show expanded (no per-directory collapse yet).
fn diff_sidebar(
    render: &DiffRender,
    threads: &[gh::ReviewThread],
    view: &DiffViewState,
    surface: DiffSurface,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    use crate::file_tree::{build_file_tree, FileTreeNode};

    // Per-file counts, keyed by full path, for the tree rows.
    let mut meta: std::collections::HashMap<&str, (u32, u32, bool)> = std::collections::HashMap::new();
    for f in &render.files {
        meta.insert(f.path.as_str(), (f.add, f.del, f.binary));
    }
    let mut tcount: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for t in threads {
        *tcount.entry(t.path.as_str()).or_insert(0) += 1;
    }

    let paths: Vec<String> = render.files.iter().map(|f| f.path.clone()).collect();
    let tree = build_file_tree(&paths);

    // Flatten the tree into (depth, node) rows for a simple indented list.
    fn flatten<'a>(nodes: &'a [FileTreeNode], depth: usize, out: &mut Vec<(usize, &'a FileTreeNode)>) {
        for n in nodes {
            out.push((depth, n));
            if let FileTreeNode::Dir { children, .. } = n {
                flatten(children, depth + 1, out);
            }
        }
    }
    let mut rows = Vec::new();
    flatten(&tree, 0, &mut rows);

    let colors = DiffColors::new(theme);
    let mut list = div().id("diff-tree").flex().flex_col().h_full().min_h(px(0.)).overflow_y_scroll();
    for (ix, (depth, node)) in rows.into_iter().enumerate() {
        let indent = px(6.0 + depth as f32 * 12.0);
        match node {
            FileTreeNode::Dir { name, .. } => {
                list = list.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .pl(indent)
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child("▾")
                        .child(div().min_w(px(0.)).truncate().child(name.clone())),
                );
            }
            FileTreeNode::File { name, path } => {
                let (add, del, binary) = meta.get(path.as_str()).copied().unwrap_or((0, 0, false));
                let tc = tcount.get(path.as_str()).copied().unwrap_or(0);
                let active = view.active_file.as_deref() == Some(path.as_str());
                let viewed = view.viewed.contains(path);
                let e = entity.clone();
                let p = path.clone();
                // Index of this file's card in the cards container, used as the
                // scroll anchor when the link is clicked.
                let file_ix = render.files.iter().position(|f| f.path == *path);
                let mut row = div()
                    .id(("diff-tree-file", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pl(indent)
                    .pr_1()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .when(active, |d| d.bg(theme.accent))
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        let p = p.clone();
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, _cx| {
                                this.diff_select_file(surface, &p);
                                if let Some(ix) = file_ix {
                                    this.files_view_mut(surface).scroll.scroll_to_top_of_item(ix);
                                }
                            });
                        }
                    });
                let mut name_el = div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(crate::renderer::FONT_FAMILY)
                    .child(name.clone());
                if viewed {
                    name_el = name_el.text_color(theme.muted_foreground).line_through();
                }
                row = row.child(name_el);
                if tc > 0 {
                    row = row.child(
                        div().flex_none().text_size(px(10.)).text_color(theme.muted_foreground).child(format!("💬{tc}")),
                    );
                }
                if binary {
                    row = row.child(div().flex_none().text_size(px(10.)).text_color(theme.muted_foreground).child("bin"));
                } else {
                    row = row
                        .child(div().flex_none().text_size(px(10.)).text_color(colors.green).child(format!("+{add}")))
                        .child(div().flex_none().text_size(px(10.)).text_color(colors.red).child(format!("−{del}")));
                }
                list = list.child(row);
            }
        }
    }

    div()
        .flex_none()
        .w(px(190.))
        .h_full()
        .min_h(px(0.))
        .border_r_1()
        .border_color(theme.border)
        .pr_1()
        .child(list)
        .into_any_element()
}

fn inline_thread_card(
    ix: usize,
    thread: &gh::ReviewThread,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
    acting: bool,
) -> AnyElement {
    let mut head = div().flex().flex_row().items_center().gap_2().child(
        div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .child(format!(
                "{}:{}",
                thread.path,
                thread.line.map(|l| l.to_string()).unwrap_or_default()
            )),
    );
    if thread.is_resolved {
        head = head.child(Badge::new().color(green(theme.dark)).child("Resolved"));
    }
    if thread.is_outdated {
        let yellow = check_style(CheckStatus::Pending, theme.dark).1;
        head = head.child(Badge::new().color(yellow.into()).child("Outdated"));
    }
    let resolved_now = thread.is_resolved;
    let id = thread.id.clone();
    let e = entity.clone();
    head = head.child(
        Button::new(("diff-thread-resolve", ix))
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Sm)
            .disabled(acting || id.is_empty())
            .child(if resolved_now { "Unresolve" } else { "Resolve" })
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                let id = id.clone();
                if let Some(e) = e.upgrade() {
                    e.update(app, move |this, cx| {
                        this.spawn_pr_action(gh::Action::ResolveThread { id, resolved: !resolved_now });
                        cx.notify();
                    });
                }
            }),
    );

    let mut content = div().flex().flex_col().gap_2();
    for c in &thread.comments {
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_size(px(11.))
                                .child(format!("@{}", c.author)),
                        )
                        .child(div().flex_1().min_w(px(0.)))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme.muted_foreground)
                                .child(relative_time(&c.created_at)),
                        ),
                )
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(12.))
                        .child(c.body.clone()),
                ),
        );
    }

    div()
        .ml_4()
        .child(
            Card::new()
                .size(CardSize::Sm)
                .child(CardHeader::new().size(CardSize::Sm).child(head))
                .child(CardContent::new().size(CardSize::Sm).child(content)),
        )
        .into_any_element()
}

fn review_thread_card(
    ix: usize,
    thread: &PreparedThread,
    ctx: &crate::markdown::MdCtx,
    entity: gpui::WeakEntity<App>,
    acting: bool,
) -> AnyElement {
    let theme = ctx.theme;
    let mut head = div().flex().flex_row().items_center().gap_2().child(
        div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .font_family(crate::renderer::FONT_FAMILY)
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .child(match thread.line {
                Some(l) => format!("{}:{}", thread.path, l),
                None => thread.path.clone(),
            }),
    );
    if thread.is_resolved {
        head = head.child(Badge::new().color(green(theme.dark)).child("Resolved"));
    }
    if thread.is_outdated {
        let yellow = check_style(CheckStatus::Pending, theme.dark).1;
        head = head.child(Badge::new().color(yellow.into()).child("Outdated"));
    }
    let resolved_now = thread.is_resolved;
    let id = thread.id.clone();
    let e = entity.clone();
    head = head.child(
        Button::new(("pr-thread-resolve", ix))
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Sm)
            .disabled(acting || id.is_empty())
            .child(if resolved_now { "Unresolve" } else { "Resolve" })
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                let id = id.clone();
                if let Some(e) = e.upgrade() {
                    e.update(app, move |this, cx| {
                        this.spawn_pr_action(gh::Action::ResolveThread { id, resolved: !resolved_now });
                        cx.notify();
                    });
                }
            }),
    );

    let mut content = div().flex().flex_col().gap_2();
    if !thread.diff_hunk.is_empty() {
        content = content.child(diff_hunk_snippet(&thread.diff_hunk, theme));
    }
    for c in &thread.comments {
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_size(px(11.))
                                .child(format!("@{}", c.author)),
                        )
                        .child(div().flex_1().min_w(px(0.)))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme.muted_foreground)
                                .child(relative_time(&c.created_at)),
                        ),
                )
                .child(crate::markdown::render_prepared(&c.body, ctx)),
        );
    }

    Card::new()
        .size(CardSize::Sm)
        .child(CardHeader::new().size(CardSize::Sm).child(head))
        .child(CardContent::new().size(CardSize::Sm).child(content))
        .into_any_element()
}

/// Render the last few lines of a review thread's diff hunk, colored by the
/// leading +/-/@@ marker (mirrors h20's non-tokenized fallback).
fn diff_hunk_snippet(hunk: &str, theme: &Theme) -> AnyElement {
    let lines: Vec<&str> = hunk.split('\n').collect();
    let start = lines.len().saturating_sub(6);
    let add_c = green(theme.dark);
    let rem_c = red(theme.dark);
    let mut col = div()
        .flex()
        .flex_col()
        .px_2()
        .py_1()
        .bg(theme.muted)
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(10.));
    for line in &lines[start..] {
        let color = if line.starts_with('+') {
            add_c
        } else if line.starts_with('-') {
            rem_c
        } else if line.starts_with("@@") {
            theme.primary
        } else {
            theme.muted_foreground
        };
        let text = if line.is_empty() { " ".to_string() } else { (*line).to_string() };
        col = col.child(div().text_color(color).child(text));
    }
    col.into_any_element()
}

/// A GitHub-style merge-status line: red/yellow when merging is blocked, green
/// when approved and ready, muted otherwise.
fn merge_status_line(detail: &PrDetail, theme: &Theme) -> AnyElement {
    let (msg, color) = match merge_block(detail) {
        Some((m, true)) => (m.to_string(), red(theme.dark)),
        Some((m, false)) => (m.to_string(), check_style(CheckStatus::Pending, theme.dark).1.into()),
        None if detail.review_decision.as_deref() == Some("APPROVED") => {
            ("Approved and ready to merge".to_string(), green(theme.dark))
        }
        None => ("Review pending".to_string(), theme.muted_foreground),
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .child(div().text_size(px(12.)).text_color(color).child(msg))
        .into_any_element()
}

/// The merge block reason, if any: `(message, is_error)` — `is_error` picks red
/// over yellow. Ported from h20's `deriveMergeStatus`; with no per-check
/// "required" flag available it evaluates all checks (h20's own fallback when no
/// checks are marked required).
fn merge_block(detail: &PrDetail) -> Option<(&'static str, bool)> {
    if detail.mergeable.as_deref() == Some("CONFLICTING") {
        return Some(("This branch has conflicts that must be resolved", true));
    }
    if detail.checks.iter().any(|c| c.status == CheckStatus::Failure) {
        return Some(("Merging is blocked — required checks are failing", true));
    }
    if detail.checks.iter().any(|c| c.status == CheckStatus::Pending) {
        return Some(("Merging is blocked — required checks have not completed", false));
    }
    match detail.review_decision.as_deref() {
        Some("REVIEW_REQUIRED") => {
            Some(("Merging is blocked — at least 1 approving review is required", true))
        }
        Some("CHANGES_REQUESTED") => Some(("Changes have been requested", true)),
        _ => None,
    }
}

// ── Relative time (no chrono; GitHub timestamps are always RFC3339 Z) ────────

fn relative_time(iso: &str) -> String {
    let Some(then) = epoch_secs(iso) else {
        return String::new();
    };
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_ago(now - then)
}

/// Parse an RFC3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to epoch seconds.
fn epoch_secs(iso: &str) -> Option<i64> {
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    // Drop any fractional seconds or trailing offset/Z.
    let time = time.split(['.', '+', 'Z']).next().unwrap_or(time);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let s: i64 = t.next().unwrap_or("0").parse().unwrap_or(0);
    Some(days_from_civil(y, mo, day) * 86400 + h * 3600 + mi * 60 + s)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Bucket a second-delta into GitHub's `s/m/h/d/mo ago` phrasing.
fn format_ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = (seconds as f64 / 60.0).round() as i64;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = (minutes as f64 / 60.0).round() as i64;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = (hours as f64 / 24.0).round() as i64;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = (days as f64 / 30.0).round() as i64;
    format!("{months}mo ago")
}

// ── Small shared helpers ───────────────────────────────────────────────────

fn skeleton_list() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .p(px(8.))
        .children((0..6).map(|i| {
            Skeleton::new()
                .w(px(if i % 2 == 0 { 260. } else { 180. }))
                .h(px(16.))
                .into_any_element()
        }))
        .into_any_element()
}

fn failed(err: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .w_full()
        .h_full()
        .p(px(12.))
        .child(div().text_color(theme.destructive).child(err.to_string()))
        .into_any_element()
}

fn centered(msg: &str, color: gpui::Hsla) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .w_full()
        .h_full()
        .child(div().text_color(color).child(msg.to_string()))
        .into_any_element()
}

fn state_badge(state: &str, is_draft: bool, theme: &Theme) -> AnyElement {
    if is_draft {
        return Badge::new().variant(BadgeVariant::Outline).child("Draft").into_any_element();
    }
    let (merged, open) = pr_state_colors(theme.dark);
    match state {
        "merged" => Badge::new().color(merged.into()).child("Merged").into_any_element(),
        "open" => Badge::new().color(open.into()).child("Open").into_any_element(),
        "closed" => Badge::new().variant(BadgeVariant::Destructive).child("Closed").into_any_element(),
        other => Badge::new().variant(BadgeVariant::Secondary).child(other.to_string()).into_any_element(),
    }
}

fn review_badge(decision: &str, theme: &Theme) -> AnyElement {
    let (_, green) = pr_state_colors(theme.dark);
    match decision {
        "APPROVED" => Badge::new().color(green.into()).child("Approved").into_any_element(),
        "CHANGES_REQUESTED" | "CHANGES_REQUESTED " => {
            Badge::new().variant(BadgeVariant::Destructive).child("Changes requested").into_any_element()
        }
        "COMMENTED" => Badge::new().variant(BadgeVariant::Secondary).child("Commented").into_any_element(),
        "REVIEW_REQUIRED" => {
            Badge::new().variant(BadgeVariant::Outline).child("Review required").into_any_element()
        }
        other => Badge::new().variant(BadgeVariant::Outline).child(other.to_string()).into_any_element(),
    }
}

fn check_style(status: CheckStatus, dark: bool) -> (&'static str, gpui::Rgba) {
    let (ok, bad, pending) = if dark {
        (gpui::rgb(0x78be8c), gpui::rgb(0xe06c75), gpui::rgb(0xd6b25e))
    } else {
        (gpui::rgb(0x228b54), gpui::rgb(0xc0392b), gpui::rgb(0xb7791f))
    };
    match status {
        CheckStatus::Success => ("passed", ok),
        CheckStatus::Failure => ("failed", bad),
        CheckStatus::Pending => ("pending", pending),
        CheckStatus::Cancelled => ("cancelled", bad),
        CheckStatus::Skipped => ("skipped", pending),
        CheckStatus::Neutral => ("neutral", pending),
    }
}

fn pr_state_colors(dark: bool) -> (gpui::Rgba, gpui::Rgba) {
    if dark {
        (gpui::rgb(0xc882dc), gpui::rgb(0x78be8c))
    } else {
        (gpui::rgb(0x8e44ad), gpui::rgb(0x228b54))
    }
}

fn green(dark: bool) -> gpui::Hsla {
    if dark { gpui::rgb(0x78be8c).into() } else { gpui::rgb(0x228b54).into() }
}
fn red(dark: bool) -> gpui::Hsla {
    if dark { gpui::rgb(0xe06c75).into() } else { gpui::rgb(0xc0392b).into() }
}
fn add_bg(dark: bool) -> gpui::Rgba {
    if dark { gpui::rgba(0x2ea04322) } else { gpui::rgba(0x2ea04318) }
}
fn remove_bg(dark: bool) -> gpui::Rgba {
    if dark { gpui::rgba(0xf8514922) } else { gpui::rgba(0xf8514918) }
}
fn hunk_bg(dark: bool) -> gpui::Rgba {
    if dark { gpui::rgba(0x58a6ff18) } else { gpui::rgba(0x0969da12) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind) -> TimelineEvent {
        TimelineEvent {
            kind,
            actor: Some("me".into()),
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
        }
    }

    #[test]
    fn epoch_secs_parses_rfc3339() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2000-01-01T00:00:00Z"), Some(946_684_800));
        // Fractional seconds are dropped.
        assert_eq!(epoch_secs("1970-01-01T00:00:01.500Z"), Some(1));
        assert_eq!(epoch_secs("garbage"), None);
    }

    #[test]
    fn format_ago_buckets() {
        assert_eq!(format_ago(-5), "0s ago");
        assert_eq!(format_ago(30), "30s ago");
        assert_eq!(format_ago(90), "2m ago");
        assert_eq!(format_ago(45 * 60), "45m ago");
        assert_eq!(format_ago(2 * 3600), "2h ago");
        assert_eq!(format_ago(3 * 86400), "3d ago");
        assert_eq!(format_ago(90 * 86400), "3mo ago");
    }

    fn dl(kind: crate::diff::LineKind, old: Option<u32>, new: Option<u32>) -> crate::diff::DiffLine {
        crate::diff::DiffLine { kind, old_no: old, new_no: new, text: "x".to_string() }
    }

    #[test]
    fn build_diff_render_emits_expand_gaps() {
        use crate::diff::{DiffFile, DiffHunk, FileStatus, LineKind};
        // A 10-line file with one hunk covering new lines 4..=6, so lines 1..3
        // are hidden above and 7..10 hidden below.
        let hunk = DiffHunk {
            header: "@@ -4,3 +4,3 @@ fn foo() {".to_string(),
            lines: vec![
                dl(LineKind::Context, Some(4), Some(4)),
                dl(LineKind::Context, Some(5), Some(5)),
                dl(LineKind::Context, Some(6), Some(6)),
            ],
        };
        let file = DiffFile {
            path: "x.rs".into(),
            previous_path: None,
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            binary: false,
            hunks: vec![hunk],
            new_lines: Some(std::sync::Arc::new((1..=10).map(|i| format!("line {i}")).collect())),
        };
        let render = build_diff_render(std::slice::from_ref(&file), false);
        let rows = &render.files[0].rows;

        // Leading gap hides new lines 1..3 and carries the hunk's scope label.
        match &rows[0] {
            DiffRow::ExpandGap { new_start, old_start, count, section, .. } => {
                assert_eq!((*new_start, *old_start, *count), (1, 1, 3));
                assert_eq!(section.to_string(), "fn foo() {");
            }
            other => panic!("expected leading ExpandGap, got {other:?}"),
        }
        assert!(matches!(rows[1], DiffRow::Hunk(_)));
        // Trailing gap after the last hunk hides new lines 7..10.
        match rows.last().unwrap() {
            DiffRow::ExpandGap { new_start, count, .. } => {
                assert_eq!((*new_start, *count), (7, 4));
            }
            other => panic!("expected trailing ExpandGap, got {other:?}"),
        }

        // Without new-side content, expansion is disabled: no ExpandGap rows.
        let mut no_content = file.clone();
        no_content.new_lines = None;
        let render2 = build_diff_render(std::slice::from_ref(&no_content), false);
        assert!(!render2.files[0].rows.iter().any(|r| matches!(r, DiffRow::ExpandGap { .. })));
    }

    #[test]
    fn event_description_reads_like_github() {
        let mut e = ev(EventKind::ForcePushed);
        e.before_sha = Some("aaa".into());
        e.after_sha = Some("bbb".into());
        assert_eq!(event_description(&e), "@me force-pushed the branch from aaa to bbb");

        let mut label = ev(EventKind::Labeled);
        label.label = Some("bug".into());
        assert_eq!(event_description(&label), "@me added the bug label");

        let mut rename = ev(EventKind::Renamed);
        rename.previous_title = Some("Old".into());
        rename.current_title = Some("New".into());
        assert_eq!(event_description(&rename), r#"@me changed the title from "Old" to "New""#);

        let mut anon = ev(EventKind::Merged);
        anon.actor = None;
        assert_eq!(event_description(&anon), "Someone merged this pull request");
    }

    fn detail_with(mergeable: Option<&str>, decision: Option<&str>, checks: Vec<CheckStatus>) -> PrDetail {
        PrDetail {
            mergeable: mergeable.map(str::to_string),
            review_decision: decision.map(str::to_string),
            checks: checks
                .into_iter()
                .map(|status| Check { name: "c".into(), status, url: String::new() })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn merge_block_precedence() {
        assert_eq!(
            merge_block(&detail_with(Some("CONFLICTING"), Some("APPROVED"), vec![])),
            Some(("This branch has conflicts that must be resolved", true))
        );
        assert_eq!(
            merge_block(&detail_with(None, None, vec![CheckStatus::Failure])),
            Some(("Merging is blocked — required checks are failing", true))
        );
        assert_eq!(
            merge_block(&detail_with(None, None, vec![CheckStatus::Pending])),
            Some(("Merging is blocked — required checks have not completed", false))
        );
        assert_eq!(
            merge_block(&detail_with(None, Some("REVIEW_REQUIRED"), vec![CheckStatus::Success])),
            Some(("Merging is blocked — at least 1 approving review is required", true))
        );
        assert_eq!(merge_block(&detail_with(None, Some("APPROVED"), vec![CheckStatus::Success])), None);
    }
}
