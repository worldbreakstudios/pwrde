//! Pull Request tool: a gpui element tree over the canvas (same overlay
//! pattern as `cleanup_ui`), positioned over the right-edge tool panel (the
//! branch-scoped tool) or filling the content area (the Pull Requests page).
//!
//! The review is one continuous scroll — description → conversation → one
//! collapsible card per changed file, unified or split, with review threads
//! and comment composers anchored to diff lines — beside a "contact card"
//! sidebar on the wide page surface (identity, Readiness: checks / review /
//! draft-or-conflicts, the file tree with viewed progress, and one contextual
//! primary action). The narrow tool surface folds the identity into a compact
//! header and puts readiness + the primary action at the top of the stream.
//!
//! Inline comments can be posted one at a time or batched into a pending
//! review that is submitted (approve / request changes / comment) in a single
//! call; thread replies and resolve/unresolve are inline too. Composers are
//! `gpui_component` multi-line editors created lazily when opened.
//!
//! Data is fetched off-thread through [`crate::gh`] and delivered back as
//! `TermEvent`s; with `lfg` + `git.async` on, cached data paints instantly and
//! the `lfg` SSE stream re-fetches when GitHub returns fresh data.
//!
//! The file cards ([`App::render_diff_files`]) are shared with the local diff
//! tool.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    div, px, AnyElement, App as GpuiApp, AppContext as _, ClickEvent, Context, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, Window, prelude::FluentBuilder as _,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};

use crate::diff::{DiffFile, LineKind};
use crate::gh::{self, CheckStatus, EventKind, PrDetail, PrSummary, TimelineEvent};
use crate::ui::theme::{alpha, Theme};
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

/// How diff bodies lay out: one column with both line numbers, or old/new
/// side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DiffView {
    #[default]
    Unified,
    Split,
}

/// Where an open composer is anchored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerAt {
    /// A new inline comment on the new side of `path:line`.
    Line { path: String, line: u32 },
    /// A reply to a review thread (`comment_id` = the thread's first comment).
    Reply { thread_id: String, comment_id: u64 },
    /// A top-level conversation comment.
    TopLevel,
    /// The review-submission body (approve / request changes / comment).
    Review,
}

/// Which face of a composer is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ComposerTab {
    #[default]
    Write,
    Preview,
}

/// One open markdown composer: a multi-line editor entity plus the Write /
/// Preview face. Only one is open at a time.
pub struct Composer {
    pub at: ComposerAt,
    pub editor: gpui::Entity<TextareaState>,
    pub tab: ComposerTab,
    /// Markdown prepared once when the Preview tab was chosen (syntect runs
    /// there, never per frame).
    pub preview: Option<crate::markdown::Prepared>,
    /// Keeps the ⌘↩ submit subscription alive for the editor's lifetime.
    _sub: gpui::Subscription,
}

/// Pull-request state, shared by the branch-scoped Sessions tool and the
/// holistic Pull Requests page. The two surfaces are never visible at once
/// (the page is a top-level `Page`, the tool only shows on `Sessions`), so they
/// share one review view; only the list differs.
pub struct PrState {
    /// PR(s) for the checked-out branch — the Sessions tool's list.
    pub branch_list: Load<Vec<PrSummary>>,
    /// All open PRs for the repo — the Pull Requests page's list.
    pub all_list: Load<Vec<PrSummary>>,
    /// Current branch name, for the tool's empty-state copy.
    pub branch: Option<String>,
    /// The PR open in the review view, or `None` while a list shows.
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
    /// Transient result banner from the last write action.
    pub action_msg: Option<String>,
    /// A write action is in flight (buttons disabled until it resolves).
    pub acting: bool,
    /// Whether the merge button is armed (showing the confirm/cancel step).
    /// Merge is irreversible + outward-facing, so it takes two clicks.
    pub merge_confirm: bool,
    /// Collapse/viewed state for the file cards.
    pub files_view: DiffViewState,
    /// Unified or split diff bodies.
    pub diff_view: DiffView,
    /// Whether inline review threads render under their lines.
    pub show_comments: bool,
    /// Readiness → Checks row expanded to the per-check list.
    pub checks_open: bool,
    /// The description card grown past its collapsed height.
    pub desc_expanded: bool,
    /// The one open composer, if any.
    pub composer: Option<Composer>,
    /// Inline comments queued into a pending review ("Start review").
    pub pending: Vec<gh::PendingComment>,
    /// The verdict picked for the pending review's submission.
    pub review_event: gh::ReviewEvent,
    /// The in-flight action came from a composer: close it on success.
    composer_submitting: bool,
    /// The in-flight action submits the pending review: clear it on success.
    pending_submitting: bool,
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
            action_msg: None,
            acting: false,
            merge_confirm: false,
            files_view: DiffViewState::default(),
            diff_view: DiffView::Unified,
            show_comments: true,
            checks_open: false,
            desc_expanded: false,
            composer: None,
            pending: Vec::new(),
            review_event: gh::ReviewEvent::Comment,
            composer_submitting: false,
            pending_submitting: false,
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
        std::thread::spawn(move || {
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
        std::thread::spawn(move || {
            let _ = tx.send(crate::term::TermEvent::PrListLoaded { all: true, result: gh::pr_list(&dir) });
        });
        self.request_redraw();
    }

    /// Reset the review view and (re)load the list for the current surface.
    /// Called when a surface is entered or the active group changes.
    pub fn reset_pr_surface(&mut self) {
        self.pr.open = None;
        self.pr.detail = Load::Idle;
        self.pr.diff = Load::Idle;
        self.pr.merge_confirm = false;
        self.pr.action_msg = None;
        self.pr.composer = None;
        self.pr.pending.clear();
        match self.pr_surface() {
            PrSurface::Page => self.spawn_pr_all_list(),
            PrSurface::Tool => self.spawn_pr_branch_list(),
            PrSurface::None => {}
        }
    }

    /// Open a PR in the review view and fetch its detail + diff.
    pub fn open_pr(&mut self, number: u32) {
        self.pr.open = Some(number);
        self.pr.action_msg = None;
        self.pr.merge_confirm = false;
        self.pr.files_view = DiffViewState::default();
        self.pr.checks_open = false;
        self.pr.desc_expanded = false;
        self.pr.composer = None;
        self.pr.pending.clear();
        self.pr.review_event = gh::ReviewEvent::Comment;
        self.spawn_pr_detail(number);
        self.spawn_pr_diff(number);
    }

    /// Re-fetch the open PR's detail + diff without resetting the view: unlike
    /// [`open_pr`], a manual Refresh preserves the files view's
    /// collapse/viewed state, any open composer and the pending review (the
    /// diff is re-seeded only if it wasn't already), matching the silent
    /// background-refresh paths.
    pub fn refresh_open_pr(&mut self, number: u32) {
        self.pr.open = Some(number);
        self.pr.merge_confirm = false;
        self.spawn_pr_detail(number);
        self.spawn_pr_diff(number);
    }

    /// Return to the PR list from the review view.
    pub fn close_pr_detail(&mut self) {
        self.pr.open = None;
        self.pr.detail = Load::Idle;
        self.pr.diff = Load::Idle;
        self.pr.merge_confirm = false;
        self.pr.composer = None;
        self.pr.pending.clear();
        self.request_redraw();
    }

    fn spawn_pr_detail(&mut self, number: u32) {
        let Some(dir) = self.active_repo_dir() else { return };
        self.pr.detail = Load::Loading;
        self.pr.conversation = None;
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
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
        std::thread::spawn(move || {
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
        let (Some(dir), Some(number)) = (self.active_repo_dir(), self.pr.open) else {
            // No `PrActionDone` will ever arrive, so don't leave the composer
            // / pending-review submit flags armed for an unrelated action.
            self.pr.composer_submitting = false;
            self.pr.pending_submitting = false;
            self.pr.action_msg = Some("No repository or open PR to act on.".into());
            self.request_redraw();
            return;
        };
        self.pr.acting = true;
        self.pr.action_msg = None;
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::term::TermEvent::PrActionDone(gh::run_action(
                &dir, number, &action,
            )));
        });
        self.request_redraw();
    }

    // ── Composers + pending review ────────────────────────────────────────

    /// Open (or re-anchor) the composer at `at`, focused, with an empty body.
    /// Needs the window to build the editor entity, so it runs from click
    /// handlers rather than plain state code.
    pub fn pr_open_composer(&mut self, at: ComposerAt, window: &mut Window, cx: &mut Context<Self>) {
        if self.pr.composer.as_ref().is_some_and(|c| c.at == at) {
            if let Some(c) = &self.pr.composer {
                c.editor.update(cx, |s, cx| s.focus(window, cx));
            }
            return;
        }
        let placeholder = match &at {
            ComposerAt::Line { .. } => "Leave a comment on this line…",
            ComposerAt::Reply { .. } => "Reply…",
            ComposerAt::TopLevel => "Leave a comment…",
            ComposerAt::Review => "Leave a review summary (optional)…",
        };
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(3, 14).placeholder(placeholder)
        });
        // ⌘↩ submits the composer's primary action (⇧↩ / ↩ insert newlines).
        let sub = cx.subscribe(&editor, |this: &mut App, _editor, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = ev {
                this.pr_submit_composer(cx);
            }
        });
        editor.update(cx, |s, cx| s.focus(window, cx));
        self.pr.composer = Some(Composer { at, editor, tab: ComposerTab::Write, preview: None, _sub: sub });
        self.request_redraw();
    }

    /// Discard the open composer (its text is dropped).
    pub fn pr_close_composer(&mut self) {
        self.pr.composer = None;
        self.pr.composer_submitting = false;
        self.request_redraw();
    }

    /// Whether a PR composer currently owns keyboard focus.
    pub fn pr_editor_focused(&self, window: &Window, cx: &GpuiApp) -> bool {
        use gpui::Focusable as _;
        self.pr
            .composer
            .as_ref()
            .is_some_and(|c| c.editor.read(cx).focus_handle(cx).is_focused(window))
    }

    fn pr_set_composer_tab(&mut self, tab: ComposerTab, cx: &mut Context<Self>) {
        let dark = crate::theme::dark_active();
        if let Some(c) = &mut self.pr.composer {
            c.tab = tab;
            c.preview = (tab == ComposerTab::Preview)
                .then(|| crate::markdown::prepare(c.editor.read(cx).value().trim(), dark));
        }
        self.request_redraw();
    }

    /// The composer's trimmed body, if one is open and non-empty.
    fn pr_composer_body(&self, cx: &GpuiApp) -> Option<(ComposerAt, String)> {
        let c = self.pr.composer.as_ref()?;
        let body = c.editor.read(cx).value().trim().to_string();
        (!body.is_empty()).then(|| (c.at.clone(), body))
    }

    /// The open PR's head commit, required to anchor inline comments.
    fn pr_head_sha(&self) -> Option<String> {
        match &self.pr.detail {
            Load::Ready(d) if !d.head_sha.is_empty() => Some(d.head_sha.clone()),
            _ => None,
        }
    }

    /// ⌘↩ / the composer's primary button: add a line comment to the pending
    /// review (starting one), reply, comment, or submit the pending review.
    pub fn pr_submit_composer(&mut self, cx: &mut Context<Self>) {
        if self.pr.acting {
            return;
        }
        match self.pr.composer.as_ref().map(|c| c.at.clone()) {
            // Matches the composer's visually primary button: a line comment
            // always goes into the (possibly new) pending review; posting a
            // lone comment is the explicit secondary button.
            Some(ComposerAt::Line { .. }) => self.pr_add_to_review(cx),
            Some(ComposerAt::Reply { .. }) => self.pr_post_reply(cx),
            Some(ComposerAt::TopLevel) => self.pr_post_comment(cx),
            Some(ComposerAt::Review) => self.pr_submit_review(cx),
            None => {}
        }
    }

    /// "Add single comment": post the line composer's body immediately.
    pub fn pr_post_line_comment(&mut self, cx: &mut Context<Self>) {
        if self.pr.acting {
            return;
        }
        let Some((ComposerAt::Line { path, line }, body)) = self.pr_composer_body(cx) else { return };
        let Some(commit_id) = self.pr_head_sha() else {
            self.pr.action_msg = Some("Can't comment: PR head commit unknown yet.".into());
            return;
        };
        self.pr.composer_submitting = true;
        self.spawn_pr_action(gh::Action::LineComment { path, line, body, commit_id });
    }

    /// "Start review" / "Add to review": queue the line composer's body into
    /// the pending review and close the composer.
    pub fn pr_add_to_review(&mut self, cx: &mut Context<Self>) {
        let Some((ComposerAt::Line { path, line }, body)) = self.pr_composer_body(cx) else { return };
        self.pr.pending.push(gh::PendingComment { path, line, body });
        self.pr_close_composer();
    }

    /// Drop a queued pending comment by index.
    pub fn pr_remove_pending(&mut self, ix: usize) {
        if ix < self.pr.pending.len() {
            self.pr.pending.remove(ix);
        }
        if self.pr.pending.is_empty() && self.pr.composer.as_ref().is_some_and(|c| c.at == ComposerAt::Review) {
            self.pr.composer = None;
        }
        self.request_redraw();
    }

    pub fn pr_post_reply(&mut self, cx: &mut Context<Self>) {
        if self.pr.acting {
            return;
        }
        let Some((ComposerAt::Reply { comment_id, .. }, body)) = self.pr_composer_body(cx) else { return };
        self.pr.composer_submitting = true;
        self.spawn_pr_action(gh::Action::ReplyThread { comment_id, body });
    }

    pub fn pr_post_comment(&mut self, cx: &mut Context<Self>) {
        if self.pr.acting {
            return;
        }
        let Some((ComposerAt::TopLevel, body)) = self.pr_composer_body(cx) else { return };
        self.pr.composer_submitting = true;
        self.spawn_pr_action(gh::Action::Comment(body));
    }

    /// Submit the pending review (queued inline comments + the review
    /// composer's body, which may be empty) with the chosen verdict.
    pub fn pr_submit_review(&mut self, cx: &mut Context<Self>) {
        if self.pr.acting {
            return;
        }
        let Some(commit_id) = self.pr_head_sha() else {
            self.pr.action_msg = Some("Can't submit: PR head commit unknown yet.".into());
            return;
        };
        let body = self
            .pr
            .composer
            .as_ref()
            .filter(|c| c.at == ComposerAt::Review)
            .map(|c| c.editor.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let comments = self.pr.pending.clone();
        if let Err(why) = review_submittable(self.pr.review_event, !body.is_empty(), !comments.is_empty()) {
            self.pr.action_msg = Some(why.into());
            return;
        }
        self.pr.composer_submitting = true;
        self.pr.pending_submitting = true;
        self.spawn_pr_action(gh::Action::SubmitReview {
            event: self.pr.review_event,
            body,
            comments,
            commit_id,
        });
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
        let ok = result.is_ok();
        self.pr.action_msg = Some(match result {
            Ok(msg) => msg,
            Err(e) => format!("Action failed: {e}"),
        });
        if ok {
            if self.pr.composer_submitting {
                self.pr.composer = None;
            }
            if self.pr.pending_submitting {
                self.pr.pending.clear();
                self.pr.review_event = gh::ReviewEvent::Comment;
            }
        }
        self.pr.composer_submitting = false;
        self.pr.pending_submitting = false;
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

    // ── Surfaces ──────────────────────────────────────────────────────────

    /// Build the Pull Request tool overlay (the narrow, branch-scoped
    /// surface). Call only when the PR tool is the visible tool.
    pub fn render_pr(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        let mut header_row = div().flex().flex_row().items_center().w_full().gap_2().min_w(px(0.));
        if self.pr.open.is_some() {
            header_row = header_row.child(back_button("pr-back", "\u{2039}", entity.clone()));
            header_row = header_row.child(self.compact_identity(&theme));
        } else {
            header_row = header_row.child(CardTitle::new().child("Pull Requests"));
        }

        // Right-aligned actions: GitHub (when a detail is loaded), a
        // Float/Dock toggle, then Refresh.
        let mut actions = div().flex().flex_row().items_center().gap_1().flex_none();
        if let (Some(_), Load::Ready(d)) = (self.pr.open, &self.pr.detail) {
            actions = actions.child(github_button("pr-open-gh", &d.url));
        }
        let float_entity = entity.clone();
        actions = actions.child(
            Button::new("pr-float-toggle")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .child(if self.tool_panel_floating { "\u{25a3}" } else { "\u{29c9}" })
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = float_entity.upgrade() {
                        e.update(app, |this, cx| {
                            this.toggle_tool_panel_floating();
                            cx.notify();
                        });
                    }
                }),
        );
        actions = actions.child(refresh_button("pr-refresh", false, entity.clone()));
        header_row = header_row.child(div().flex_1().min_w(px(0.))).child(actions);

        let body = match self.pr.open {
            Some(_) => self.render_pr_review(false, &theme, entity.clone(), cx),
            None => {
                let empty = match &self.pr.branch {
                    Some(b) => format!("No pull request for branch \u{201c}{b}\u{201d}."),
                    None => "Not on a branch.".to_string(),
                };
                self.pr_list_body(&self.pr.branch_list, "pr-branch-row", &empty, &theme, entity.clone())
            }
        };

        tool_panel_overlay(
            self.tool_panel_floating,
            self.tool_panel_w,
            header_row.into_any_element(),
            body.into_any_element(),
        )
    }

    /// Build the holistic Pull Requests page overlay (all open PRs for the
    /// repo). The list is a full content-area card like Cleanup/Settings;
    /// opening a PR swaps in the two-pane review: the stream card beside the
    /// PR's contact-card sidebar.
    pub fn render_all_prs(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        // Match cleanup_ui's content-area insets so the page tracks resizes.
        let pad = crate::workspace::AREA_PAD;
        let sidebar = self.sidebar_w();
        let left = if sidebar == 0.0 { pad } else { sidebar };
        let right = self.right_w() + pad;
        let root = div().absolute().left(px(left)).top(px(pad)).right(px(right)).bottom(px(pad));

        if self.pr.open.is_none() {
            let header_row = div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap_2()
                .child(CardTitle::new().child("Pull Requests"))
                .child(div().flex_1())
                .child(refresh_button("all-pr-refresh", true, entity.clone()));
            let body = self.pr_list_body(
                &self.pr.all_list,
                "all-pr-row",
                "No open pull requests.",
                &theme,
                entity.clone(),
            );
            return root
                .child(
                    Card::new()
                        .h_full()
                        .child(CardHeader::new().child(header_row))
                        .child(CardContent::new().flex_1().child(body)),
                )
                .into_any_element();
        }

        // Two panes: the stream card and the contact card. The stream card's
        // header carries the back button; identity lives in the sidebar.
        let header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap_2()
            .min_w(px(0.))
            .child(back_button("all-pr-back", "\u{2039} All PRs", entity.clone()))
            .child(self.stream_header(&theme, entity.clone()));
        let stream = Card::new()
            .h_full()
            .child(CardHeader::new().child(header_row))
            .child(CardContent::new().flex_1().child(self.render_pr_review(true, &theme, entity.clone(), cx)));

        root.flex()
            .flex_row()
            .gap_2()
            .child(div().flex_1().min_w(px(0.)).h_full().child(stream))
            .child(self.render_contact_card(&theme, entity.clone(), cx))
            .into_any_element()
    }

    /// The narrow surface's identity: "#77 · title" plus the three readiness
    /// dots (checks / review / draft-or-conflicts).
    fn compact_identity(&self, theme: &Theme) -> AnyElement {
        let Load::Ready(d) = &self.pr.detail else {
            return div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(12.))
                .child(format!("PR #{}", self.pr.open.unwrap_or(0)))
                .into_any_element();
        };
        let r = readiness(d, theme);
        let dot = |c: gpui::Hsla| div().flex_none().size(px(7.)).rounded_full().bg(c);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_1()
            .min_w(px(0.))
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(12.))
                    .child(format!("{} · #{}", d.title, d.number)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .flex_none()
                    .child(dot(r.checks.color))
                    .child(dot(r.review.color))
                    .child(dot(r.state.color)),
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
            let checks = gh::aggregate_check_status(&pr.checks)
                .map(|s| check_style(s, theme.dark).1)
                .map(|c| div().flex_none().size(px(7.)).rounded_full().bg(c));
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
                            .child(div().text_color(theme.muted_foreground).child(format!("#{number}")))
                            .child(state_badge)
                            .child(div().flex_1().min_w(px(0.)).truncate().child(pr.title.clone()))
                            .when_some(checks, |el, c| el.child(c)),
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

    // ── The review stream ─────────────────────────────────────────────────

    /// The stream header strip: "files changed · N" pill, ± totals, the
    /// Unified/Split toggle, the comments toggle and viewed progress.
    fn stream_header(&self, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
        let render = match &self.pr.diff {
            Load::Ready(r) => Some(r.clone()),
            _ => None,
        };
        let n = render.as_ref().map(|r| r.files.len()).unwrap_or(0);
        let (add, del) = render
            .as_ref()
            .map(|r| (r.files.iter().map(|f| f.add).sum::<u32>(), r.files.iter().map(|f| f.del).sum::<u32>()))
            .unwrap_or((0, 0));
        let viewed = render
            .as_ref()
            .map(|r| r.files.iter().filter(|f| self.pr.files_view.viewed.contains(&f.path)).count())
            .unwrap_or(0);
        let colors = DiffColors::new(theme);

        let mut row = div().flex().flex_row().items_center().gap_2().flex_1().min_w(px(0.));
        row = row
            .child(pill(format!("files changed · {n}"), theme))
            .child(mono(format!("+{add}"), colors.green))
            .child(mono(format!("−{del}"), colors.red))
            .child(div().flex_1().min_w(px(0.)));
        row = row.child(diff_view_toggle(self.pr.diff_view, DiffSurface::Pr, theme, entity.clone()));
        {
            let e = entity.clone();
            let on = self.pr.show_comments;
            row = row.child(
                div()
                    .id("pr-comments-toggle")
                    .flex_none()
                    .w(px(26.))
                    .h(px(22.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(if on { alpha(theme.primary, 0.14) } else { theme.muted })
                    .text_color(if on { theme.primary } else { theme.muted_foreground })
                    .text_size(px(12.))
                    .child("💬")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = e.upgrade() {
                            e.update(app, |this, cx| {
                                this.pr.show_comments = !this.pr.show_comments;
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        row = row.child(viewed_progress(viewed, n, theme));
        row.into_any_element()
    }

    /// The continuous review: description → conversation → file cards, with
    /// a sticky header for the topmost file. `wide` is the page surface (the
    /// contact card carries readiness + actions); the narrow tool surface
    /// puts them at the top of the stream instead.
    fn render_pr_review(
        &self,
        wide: bool,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = match &self.pr.detail {
            Load::Idle | Load::Loading => return skeleton_list(),
            Load::Failed(e) => return failed(e, theme),
            Load::Ready(d) => d,
        };
        let acting = self.pr.acting;
        let view = &self.pr.files_view;
        let colors = DiffColors::new(theme);

        // Every stream item is `flex_none`: in this fixed-height scrolling
        // column, flexbox would otherwise shrink each child to fit the
        // viewport and an rcn `Card` (which clips) would cut its content off.
        let item = |el: AnyElement| div().flex_none().child(el).into_any_element();
        let mut items: Vec<AnyElement> = Vec::new();

        if !wide {
            items.push(item(self.stream_header(theme, entity.clone())));
            items.push(item(self.readiness_block(detail, theme, entity.clone())));
            items.push(item(self.primary_actions(detail, theme, entity.clone(), cx)));
        }
        if let Some(msg) = &self.pr.action_msg {
            items.push(item(
                div().px_1().text_size(px(11.)).text_color(theme.muted_foreground).child(msg.clone()).into_any_element(),
            ));
        }

        // Markdown rendering context: live theme, the set of details the user
        // has toggled from their `open` default, and a factory that builds a
        // toggle handler flipping that set.
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
        let ctx = crate::markdown::MdCtx { theme, flipped: &self.pr.md_flipped, toggle: &toggle, seq: &seq };
        let conv = self.pr.conversation.as_ref();

        // Description card (collapsed to a fixed height until expanded).
        if let Some(body) = conv.and_then(|c| c.body.as_ref()) {
            items.push(item(self.description_card(detail, body, &ctx, entity.clone())));
        }
        // Unified, chronologically-ordered timeline: comments, review threads,
        // and events interleaved (reviewed events filtered, commits grouped).
        let entries: &[PreparedEntry] = conv.map(|c| c.entries.as_slice()).unwrap_or(&[]);
        for (ix, entry) in entries.iter().enumerate() {
            items.push(item(match entry {
                PreparedEntry::Comment { author, review_state, created_at, body } => {
                    comment_card(author, review_state.as_deref(), created_at, body, &ctx)
                }
                PreparedEntry::Thread(t) => review_thread_card(ix, t, &ctx, entity.clone(), acting),
                PreparedEntry::Event(e) => event_row(e, theme),
                PreparedEntry::CommitGroup(evs) => commit_group_entry(evs, theme),
            }));
        }
        // Top-level comment composer (or the pill that opens it).
        items.push(item(self.top_level_composer(theme, entity.clone(), cx)));

        // Files divider + cards. `pr_file_offset` is the single source of
        // truth for where file cards start (the contact card's tree links
        // scroll by it); the assert keeps it honest against the item order.
        let file_offset = self.pr_file_offset(wide);
        debug_assert_eq!(file_offset, items.len() + 1, "pr_file_offset drifted from the stream item order");
        let threads: &[gh::ReviewThread] = &detail.threads;
        let mut sticky: Option<(gpui::Stateful<gpui::Div>, f32)> = None;
        match &self.pr.diff {
            Load::Idle | Load::Loading => {
                items.push(item(section_divider("Files changed", theme)));
                items.push(item(skeleton_list()));
            }
            Load::Failed(e) => {
                items.push(item(section_divider("Files changed", theme)));
                items.push(item(failed(e, theme)));
            }
            Load::Ready(r) if r.is_empty() => {
                items.push(item(section_divider("Files changed", theme)));
                items.push(item(centered("No file changes.", theme.muted_foreground)));
            }
            Load::Ready(r) => {
                items.push(item(section_divider(&format!("Files changed · {}", r.files.len()), theme)));
                for (ix, f) in r.files.iter().enumerate() {
                    items.push(self.file_card(ix, f, DiffSurface::Pr, threads, theme, &colors, entity.clone(), cx));
                }
                let viewed = r.files.iter().filter(|f| view.viewed.contains(&f.path)).count();
                items.push(item(
                    div()
                        .flex()
                        .justify_center()
                        .py_3()
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(format!("That's all {} files · {viewed} viewed", r.files.len()))
                        .into_any_element(),
                ));
                sticky = self.sticky_file_header(r, file_offset, DiffSurface::Pr, threads, theme, &colors, &entity);
            }
        }

        let stream = div()
            .id("pr-stream")
            .track_scroll(&view.scroll)
            .flex()
            .flex_col()
            .gap_2()
            .h_full()
            .min_h(px(0.))
            .overflow_y_scroll()
            .children(items);

        div()
            .relative()
            .h_full()
            .min_h(px(0.))
            .child(stream)
            .when_some(sticky, |el, (hdr, shove)| el.child(sticky_overlay(hdr, shove, theme)))
            .into_any_element()
    }

    /// The PR description as the first card in the stream, clipped to a
    /// fixed height with a "Show full description" toggle until expanded.
    fn description_card(
        &self,
        detail: &PrDetail,
        body: &crate::markdown::Prepared,
        ctx: &crate::markdown::MdCtx,
        entity: gpui::WeakEntity<App>,
    ) -> AnyElement {
        let theme = ctx.theme;
        const COLLAPSED_H: f32 = 260.0;
        let expanded = self.pr.desc_expanded;
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(avatar(&detail.author, theme))
            .child(div().font_weight(gpui::FontWeight::BOLD).text_size(px(12.)).child(format!("@{}", detail.author)))
            .child(div().text_size(px(10.5)).text_color(theme.muted_foreground).child(relative_time(&detail.created_at)))
            .child(div().flex_1());
        let mut content = div().flex().flex_col().gap_1();
        content = content.child(
            div()
                .when(!expanded, |d| d.max_h(px(COLLAPSED_H)).overflow_hidden())
                .child(crate::markdown::render_prepared(body, ctx)),
        );
        let e = entity.clone();
        content = content.child(
            div()
                .id("pr-desc-toggle")
                .mt_1()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.primary))
                .child(if expanded { "▾ Collapse description" } else { "▸ Show full description" })
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = e.upgrade() {
                        e.update(app, |this, cx| {
                            this.pr.desc_expanded = !this.pr.desc_expanded;
                            cx.notify();
                        });
                    }
                }),
        );
        Card::new()
            .size(CardSize::Sm)
            .child(CardHeader::new().size(CardSize::Sm).child(head))
            .child(CardContent::new().size(CardSize::Sm).child(content))
            .into_any_element()
    }

    /// The top-level comment composer, or the "Leave a comment…" pill that
    /// opens it.
    fn top_level_composer(&self, theme: &Theme, entity: gpui::WeakEntity<App>, cx: &mut Context<Self>) -> AnyElement {
        if let Some(c) = self.pr.composer.as_ref().filter(|c| c.at == ComposerAt::TopLevel) {
            let submit = composer_button("pr-comment-submit", "Comment", ButtonVariant::Default, self.pr.acting, entity.clone(), |this, cx| {
                this.pr_post_comment(cx)
            });
            return self.composer_card("pr-composer-top", c, vec![submit], theme, entity, cx);
        }
        let e = entity.clone();
        div()
            .id("pr-comment-open")
            .px_3()
            .py(px(6.))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.card)
            .text_size(px(11.5))
            .text_color(theme.muted_foreground)
            .cursor_text()
            .child("Leave a comment…")
            .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = e.upgrade() {
                    e.update(app, |this, cx| this.pr_open_composer(ComposerAt::TopLevel, window, cx));
                }
            })
            .into_any_element()
    }

    /// A composer card: Write | Preview tabs, the multi-line editor (or the
    /// rendered preview), and a footer with Cancel plus the given actions.
    fn composer_card(
        &self,
        id: &'static str,
        c: &Composer,
        actions: Vec<AnyElement>,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let acting = self.pr.acting;
        let tab = |label: &'static str, which: ComposerTab, e: gpui::WeakEntity<App>| {
            let active = c.tab == which;
            div()
                .id((id, which as usize))
                .px_3()
                .py_1()
                .rounded_t(px(8.))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .cursor_pointer()
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .when(active, |d| d.bg(theme.card).border_1().border_b_0().border_color(theme.border))
                .child(label)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = e.upgrade() {
                        e.update(app, |this, cx| this.pr_set_composer_tab(which, cx));
                    }
                })
        };
        let head = div()
            .flex()
            .flex_row()
            .items_end()
            .gap_1()
            .px_2()
            .pt_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(tab("Write", ComposerTab::Write, entity.clone()))
            .child(tab("Preview", ComposerTab::Preview, entity.clone()))
            .child(div().flex_1())
            .child(
                div()
                    .pb_1()
                    .text_size(px(10.))
                    .text_color(theme.muted_foreground)
                    .child("Markdown · ⌘↩ to submit"),
            );

        let body: AnyElement = match c.tab {
            ComposerTab::Write => div()
                .px_2()
                .py_1()
                .font_family(crate::renderer::FONT_FAMILY)
                .text_size(px(12.))
                .child(Textarea::new(&c.editor).appearance(false).bordered(false))
                .into_any_element(),
            ComposerTab::Preview => {
                let toggle = |_key: u64| -> crate::markdown::OnToggle {
                    Box::new(|_ev: &ClickEvent, _win: &mut Window, _app: &mut GpuiApp| {})
                };
                let seq = std::cell::Cell::new(0u64);
                let flipped = std::collections::HashSet::new();
                let ctx = crate::markdown::MdCtx { theme, flipped: &flipped, toggle: &toggle, seq: &seq };
                div()
                    .px_3()
                    .py_2()
                    .min_h(px(52.))
                    .text_size(px(12.5))
                    .child(match &c.preview {
                        Some(p) => crate::markdown::render_prepared(p, &ctx),
                        None => div().text_color(theme.muted_foreground).child("Nothing to preview").into_any_element(),
                    })
                    .into_any_element()
            }
        };

        let cancel_e = entity.clone();
        let mut foot = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(
                Button::new((id, 2usize))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child("Cancel")
                    .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = cancel_e.upgrade() {
                            e.update(app, |this, cx| {
                                this.pr_close_composer();
                                window.focus(&this.focus_handle, cx);
                            });
                        }
                    }),
            )
            .child(div().flex_1());
        for a in actions {
            foot = foot.child(a);
        }

        div()
            .id(id)
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(alpha(theme.primary, 0.4))
            .bg(theme.card)
            .overflow_hidden()
            .when(acting, |d| d.opacity(0.6))
            .child(head)
            .child(body)
            .child(foot)
            .into_any_element()
    }

    // ── Contact card (wide surface) ───────────────────────────────────────

    /// The PR's contact card: identity, Readiness, the file tree, and the
    /// primary action pinned to the bottom.
    fn render_contact_card(&self, theme: &Theme, entity: gpui::WeakEntity<App>, cx: &mut Context<Self>) -> AnyElement {
        let Load::Ready(detail) = &self.pr.detail else {
            return div()
                .flex_none()
                .w(px(CONTACT_CARD_W))
                .h_full()
                .child(Card::new().h_full().child(CardContent::new().flex_1().child(skeleton_list())))
                .into_any_element();
        };
        let colors = DiffColors::new(theme);

        // Identity.
        let mut identity = div().flex().flex_col().items_center().gap_2().px_1().py_2().text_center();
        identity = identity.child(
            div()
                .size(px(56.))
                .rounded_full()
                .bg(theme.primary)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(24.))
                .text_color(theme.primary_foreground)
                .child("⎇"),
        );
        identity = identity
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.5))
                    .line_height(px(18.))
                    .child(detail.title.clone()),
            )
            .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(format!(
                "{} #{} · @{}",
                state_label(&detail.state, detail.is_draft),
                detail.number,
                detail.author
            )))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .max_w_full()
                    .child(branch_chip(&detail.head, theme))
                    .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child("→"))
                    .child(branch_chip(&detail.base, theme)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(mono(format!("+{}", detail.additions), colors.green))
                    .child(mono(format!("−{}", detail.deletions), colors.red))
                    .child(mono(format!("{} files", detail.changed_files), theme.muted_foreground)),
            );
        // Round action buttons: GitHub · Refresh · Session (when a group is on
        // this PR's branch).
        let mut round = div().flex().flex_row().gap_2().mt_1();
        {
            let url = detail.url.clone();
            round = round.child(round_action("pr-card-gh", "↗", "GitHub", theme, move |_win, _app| {
                let _ = std::process::Command::new("open").arg(&url).spawn();
            }));
        }
        {
            let e = entity.clone();
            let n = detail.number;
            round = round.child(round_action("pr-card-refresh", "⟳", "Refresh", theme, move |_win, app| {
                if let Some(e) = e.upgrade() {
                    e.update(app, |this, cx| {
                        this.refresh_open_pr(n);
                        cx.notify();
                    });
                }
            }));
        }
        if let Some(wi) = self.group_for_branch(&detail.head) {
            let e = entity.clone();
            round = round.child(round_action("pr-card-session", "⌖", "Session", theme, move |_win, app| {
                if let Some(e) = e.upgrade() {
                    e.update(app, |this, cx| {
                        this.jump_to_group(wi);
                        cx.notify();
                    });
                }
            }));
        }
        identity = identity.child(round);
        if !detail.labels.is_empty() {
            let mut labels = div().flex().flex_row().flex_wrap().gap_1().justify_center();
            for l in &detail.labels {
                labels = labels.child(Badge::new().variant(BadgeVariant::Outline).child(l.clone()));
            }
            identity = identity.child(labels);
        }

        // Files tree.
        let files: AnyElement = match &self.pr.diff {
            Load::Ready(r) if !r.is_empty() => {
                let viewed = r.files.iter().filter(|f| self.pr.files_view.viewed.contains(&f.path)).count();
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .pt_3()
                            .pb_1()
                            .px(px(2.))
                            .child(section_label("Files", theme))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{viewed}/{} viewed", r.files.len())),
                            ),
                    )
                    .child(file_tree(
                        r,
                        &detail.threads,
                        &self.pr.files_view,
                        DiffSurface::Pr,
                        self.pr_file_offset(true),
                        theme,
                        entity.clone(),
                    ))
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };

        let scroll = div()
            .id("pr-contact-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px_2()
            .child(identity)
            .child(section_label("Readiness", theme))
            .child(self.readiness_block(detail, theme, entity.clone()))
            .child(files);

        div()
            .flex_none()
            .w(px(CONTACT_CARD_W))
            .h_full()
            .child(
                Card::new()
                    .h_full()
                    .child(CardContent::new().flex_1().child(
                        div()
                            .flex()
                            .flex_col()
                            .h_full()
                            .min_h(px(0.))
                            .child(scroll)
                            .child(
                                div()
                                    .flex_none()
                                    .pt_2()
                                    .child(self.primary_actions(detail, theme, entity.clone(), cx)),
                            ),
                    )),
            )
            .into_any_element()
    }

    /// Index of the first file card among the review stream's children, for
    /// sidebar links that scroll a card into view. Mirrors the item order in
    /// [`render_pr_review`] (which asserts against it in debug builds).
    fn pr_file_offset(&self, wide: bool) -> usize {
        let mut n = 0usize;
        if !wide {
            n += 3; // stream header, readiness block, primary actions
        }
        if self.pr.action_msg.is_some() {
            n += 1;
        }
        let conv = self.pr.conversation.as_ref();
        if conv.is_some_and(|c| c.body.is_some()) {
            n += 1;
        }
        n += conv.map(|c| c.entries.len()).unwrap_or(0);
        n += 1; // top-level composer
        n += 1; // files divider
        n
    }

    /// The Readiness list: Checks (expandable), Review, Draft / conflicts.
    fn readiness_block(&self, detail: &PrDetail, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
        let r = readiness(detail, theme);
        let row = |glyph: &'static str, color: gpui::Hsla, title: String, sub: String, chevron: Option<&'static str>| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px(px(11.))
                .py(px(8.))
                .child(
                    div()
                        .flex_none()
                        .size(px(24.))
                        .rounded_full()
                        .bg(alpha(color, 0.14))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().text_size(px(12.)).font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(10.5)).text_color(theme.muted_foreground).truncate().child(sub)),
                )
                .when_some(chevron, |d, c| d.child(div().flex_none().text_size(px(9.)).text_color(theme.muted_foreground).child(c)))
        };
        let hairline = || div().h(px(1.)).mx(px(11.)).bg(theme.border);

        let checks_open = self.pr.checks_open;
        let e = entity.clone();
        let mut checks_row = div()
            .id("pr-readiness-checks")
            .cursor_pointer()
            .hover(|s| s.bg(alpha(theme.primary, 0.05)))
            .child(row(
                r.checks.glyph,
                r.checks.color,
                r.checks.title.clone(),
                r.checks.sub.clone(),
                Some(if checks_open { "▾" } else { "▸" }),
            ))
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = e.upgrade() {
                    e.update(app, |this, cx| {
                        this.pr.checks_open = !this.pr.checks_open;
                        cx.notify();
                    });
                }
            });
        if checks_open && !detail.checks.is_empty() {
            let mut list = div().flex().flex_col().gap_1().pl(px(46.)).pr(px(11.)).pb_2();
            for (ix, c) in detail.checks.iter().enumerate() {
                let (label, color) = check_style(c.status, theme.dark);
                let glyph = match c.status {
                    CheckStatus::Success => "✓",
                    CheckStatus::Failure | CheckStatus::Cancelled => "✕",
                    _ => "●",
                };
                let mut line = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .font_family(crate::renderer::FONT_FAMILY)
                    .text_size(px(10.5))
                    .child(div().flex_none().text_color(color).child(glyph))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_color(theme.muted_foreground).child(format!("{} · {label}", c.name)));
                if !c.url.is_empty() {
                    let url = c.url.clone();
                    line = line.child(
                        div()
                            .id(("pr-check-log", ix))
                            .flex_none()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.primary)
                            .cursor_pointer()
                            .child("log")
                            .on_mouse_down(gpui::MouseButton::Left, |_ev, _win, app: &mut GpuiApp| app.stop_propagation())
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, _app: &mut GpuiApp| {
                                let _ = std::process::Command::new("open").arg(&url).spawn();
                            }),
                    );
                }
                list = list.child(line);
            }
            checks_row = checks_row.child(list);
        }

        div()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.card)
            .overflow_hidden()
            .child(checks_row)
            .child(hairline())
            .child(row(r.review.glyph, r.review.color, r.review.title.clone(), r.review.sub.clone(), None))
            .child(hairline())
            .child(row(r.state.glyph, r.state.color, r.state.title.clone(), r.state.sub.clone(), None))
            .into_any_element()
    }

    /// The contextual primary action (and the pending-review panel when a
    /// review is in progress): Mark ready → Approve / Review… → Squash &
    /// merge, with Close tucked underneath for open PRs.
    fn primary_actions(
        &self,
        detail: &PrDetail,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let acting = self.pr.acting;
        let mut col = div().flex().flex_col().gap_2().px_1().pb_1();

        // Pending review panel: queued comments + verdict + submit.
        let review_open = self.pr.composer.as_ref().is_some_and(|c| c.at == ComposerAt::Review);
        if !self.pr.pending.is_empty() || review_open {
            col = col.child(self.pending_review_panel(theme, entity.clone(), cx));
            return col.into_any_element();
        }

        let mk = |id: &'static str, label: &str, variant: ButtonVariant, action: gh::Action, e: gpui::WeakEntity<App>| {
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

        if detail.state != "open" {
            return col
                .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(format!(
                    "This pull request is {}.",
                    detail.state
                )))
                .into_any_element();
        }

        let approved = detail.review_decision.as_deref() == Some("APPROVED");
        let mut primary = div().flex().flex_col().gap_1();
        if detail.is_draft {
            primary = primary.child(mk("pr-ready", "Mark ready for review", ButtonVariant::Default, gh::Action::Ready, entity.clone()));
        } else if approved && merge_block(detail).is_none() {
            if self.pr.merge_confirm {
                let confirm_e = entity.clone();
                let cancel_e = entity.clone();
                primary = primary
                    .child(
                        Button::new("pr-merge-confirm")
                            .variant(ButtonVariant::Destructive)
                            .size(ButtonSize::Sm)
                            .disabled(acting)
                            .child("Confirm squash & merge")
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
                primary = primary.child(
                    Button::new("pr-merge")
                        .variant(ButtonVariant::Default)
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
        } else {
            primary = primary.child(mk("pr-approve", "Approve", ButtonVariant::Default, gh::Action::Approve, entity.clone()));
        }
        col = col.child(primary);

        // Secondary row: Review… (opens the verdict composer) and Close.
        let review_e = entity.clone();
        let mut secondary = div().flex().flex_row().gap_1().child(
            Button::new("pr-review-open")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .disabled(acting)
                .child("Review…")
                .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = review_e.upgrade() {
                        e.update(app, |this, cx| this.pr_open_composer(ComposerAt::Review, window, cx));
                    }
                }),
        );
        secondary = secondary.child(div().flex_1());
        secondary = secondary.child(mk("pr-close", "Close", ButtonVariant::Ghost, gh::Action::Close, entity.clone()));
        col = col.child(secondary);
        col.into_any_element()
    }

    /// The pending review panel: queued inline comments (each removable), the
    /// verdict picker, the summary composer and Submit / Discard.
    fn pending_review_panel(&self, theme: &Theme, entity: gpui::WeakEntity<App>, cx: &mut Context<Self>) -> AnyElement {
        let acting = self.pr.acting;
        let mut col = div().flex().flex_col().gap_2();
        col = col.child(section_label(&format!("Pending review · {}", self.pr.pending.len()), theme));
        if !self.pr.pending.is_empty() {
            let mut list = div().flex().flex_col().gap_1();
            for (ix, p) in self.pr.pending.iter().enumerate() {
                let e = entity.clone();
                list = list.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded(px(8.))
                        .bg(theme.muted)
                        .text_size(px(11.))
                        .child(
                            div()
                                .flex_none()
                                .font_family(crate::renderer::FONT_FAMILY)
                                .text_size(px(10.))
                                .text_color(theme.primary)
                                .child(format!("{}:{}", p.path.rsplit('/').next().unwrap_or(&p.path), p.line)),
                        )
                        .child(div().flex_1().min_w(px(0.)).truncate().text_color(theme.muted_foreground).child(p.body.clone()))
                        .child(
                            div()
                                .id(("pr-pending-rm", ix))
                                .flex_none()
                                .cursor_pointer()
                                .text_color(theme.muted_foreground)
                                .hover(|s| s.text_color(theme.destructive))
                                .child("✕")
                                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                    if let Some(e) = e.upgrade() {
                                        e.update(app, |this, cx| {
                                            this.pr_remove_pending(ix);
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                );
            }
            col = col.child(list);
        }

        // Verdict segmented control.
        let current = self.pr.review_event;
        let seg = |label: &'static str, which: gh::ReviewEvent, e: gpui::WeakEntity<App>| {
            let active = current == which;
            div()
                .id(("pr-verdict", which as usize))
                .flex_1()
                .px_2()
                .py(px(3.))
                .rounded(px(5.))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .when(active, |d| d.bg(theme.card).shadow_sm())
                .cursor_pointer()
                .flex()
                .justify_center()
                .child(label)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = e.upgrade() {
                        e.update(app, |this, cx| {
                            this.pr.review_event = which;
                            cx.notify();
                        });
                    }
                })
        };
        col = col.child(
            div()
                .flex()
                .flex_row()
                .p(px(2.))
                .rounded(px(7.))
                .bg(theme.muted)
                .child(seg("Comment", gh::ReviewEvent::Comment, entity.clone()))
                .child(seg("Approve", gh::ReviewEvent::Approve, entity.clone()))
                .child(seg("Request changes", gh::ReviewEvent::RequestChanges, entity.clone())),
        );

        // Summary composer (opened by Review… or on demand).
        if let Some(c) = self.pr.composer.as_ref().filter(|c| c.at == ComposerAt::Review) {
            let submit = composer_button("pr-review-submit", "Submit review", ButtonVariant::Default, self.pr.acting, entity.clone(), |this, cx| {
                this.pr_submit_review(cx)
            });
            col = col.child(self.composer_card("pr-composer-review", c, vec![submit], theme, entity.clone(), cx));
        } else {
            let open_e = entity.clone();
            let submit_e = entity.clone();
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(
                        Button::new("pr-review-summary")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .disabled(acting)
                            .child("Add summary…")
                            .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                                if let Some(e) = open_e.upgrade() {
                                    e.update(app, |this, cx| this.pr_open_composer(ComposerAt::Review, window, cx));
                                }
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("pr-review-submit-quick")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Sm)
                            .disabled(acting)
                            .child("Submit review")
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                if let Some(e) = submit_e.upgrade() {
                                    e.update(app, |this, cx| {
                                        this.pr_submit_review(cx);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            );
        }
        let discard_e = entity.clone();
        col = col.child(
            Button::new("pr-review-discard")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Sm)
                .disabled(acting)
                .child("Discard review")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = discard_e.upgrade() {
                        e.update(app, |this, cx| {
                            this.pr.pending.clear();
                            this.pr.review_event = gh::ReviewEvent::Comment;
                            if this.pr.composer.as_ref().is_some_and(|c| c.at == ComposerAt::Review) {
                                this.pr.composer = None;
                            }
                            cx.notify();
                        });
                    }
                }),
        );
        col.into_any_element()
    }

    /// The group whose cwd is checked out on `branch`, if any.
    /// Reads the cached git context (never shells out: this runs per frame).
    fn group_for_branch(&self, branch: &str) -> Option<usize> {
        self.workspaces.iter().position(|ws| {
            ws.cwd
                .as_deref()
                .and_then(|cwd| self.git_contexts.get(cwd))
                .and_then(|c| c.branch.as_deref())
                == Some(branch)
        })
    }

    /// Jump to a Sessions group from the Pull Requests page.
    fn jump_to_group(&mut self, wi: usize) {
        self.set_page(crate::pages::Page::Sessions);
        self.switch_workspace(wi);
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

    fn diff_view_for(&self, s: DiffSurface) -> DiffView {
        match s {
            DiffSurface::Pr => self.pr.diff_view,
            DiffSurface::LocalDiff => self.local_diff.diff_view,
        }
    }

    fn set_diff_view(&mut self, s: DiffSurface, v: DiffView) {
        match s {
            DiffSurface::Pr => self.pr.diff_view = v,
            DiffSurface::LocalDiff => self.local_diff.diff_view = v,
        }
        self.request_redraw();
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

    fn diff_toggle_sidebar(&mut self, s: DiffSurface) {
        let v = self.files_view_mut(s);
        v.sidebar_open = !v.sidebar_open;
        self.request_redraw();
    }

    /// Select a file from the tree: expand its card and mark it active.
    fn diff_select_file(&mut self, s: DiffSurface, path: &str) {
        let v = self.files_view_mut(s);
        v.collapsed.remove(path);
        v.active_file = Some(path.to_string());
        self.request_redraw();
    }

    // ── File cards (shared by the PR stream and the local diff tool) ──────

    /// One file card's header row: chevron + status circle + path + ± counts
    /// + thread badge + the Viewed checkbox.
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
        let active = self.files_view(surface).active_file.as_deref() == Some(f.path.as_str());
        let chevron = if collapsed { "▸" } else { "▾" };
        let mut header = div()
            .id((id_prefix, ix))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py(px(7.))
            .bg(theme.muted)
            .when(!collapsed, |d| d.border_b_1().border_color(theme.border))
            .cursor_pointer();
        header = header
            .child(div().flex_none().w(px(10.)).text_size(px(9.)).text_color(theme.muted_foreground).child(chevron))
            .child(status_circle(f.status_tag, px(22.), theme, colors));
        let mut path_el = div()
            .min_w(px(0.))
            .truncate()
            .font_family(crate::renderer::FONT_FAMILY)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_size(px(12.))
            .text_color(if active {
                theme.primary
            } else if viewed {
                theme.muted_foreground
            } else {
                theme.foreground
            })
            .child(f.display_path.clone());
        if viewed {
            path_el = path_el.line_through();
        }
        header = header.child(path_el);
        if f.binary {
            header = header.child(div().flex_none().text_size(px(10.)).text_color(theme.muted_foreground).child("binary"));
        } else {
            header = header
                .child(mono(format!("+{}", f.add), colors.green))
                .child(mono(format!("−{}", f.del), colors.red));
        }
        if thread_count > 0 {
            header = header.child(comment_badge(thread_count, theme));
        }
        header = header.child(div().flex_1().min_w(px(0.)));
        // ── Viewed checkbox (stop propagation so it doesn't toggle collapse) ──
        {
            let e = entity.clone();
            let path = f.path.clone();
            header = header.child(
                div()
                    .id((id_prefix, ix + 1_000_000))
                    .flex_none()
                    .size(px(16.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if viewed { theme.primary } else { theme.muted_foreground })
                    .bg(if viewed { theme.primary } else { theme.card })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme.primary_foreground)
                    .child(if viewed { "✓" } else { "" })
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

    /// One collapsible file card: header + (when expanded) the unified or
    /// split diff body with inline threads and the line composer.
    #[allow(clippy::too_many_arguments)]
    fn file_card(
        &self,
        ix: usize,
        f: &FileDiff,
        surface: DiffSurface,
        threads: &[gh::ReviewThread],
        theme: &Theme,
        colors: &DiffColors,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.files_view(surface);
        let collapsed = view.collapsed.contains(&f.path);
        let viewed = view.viewed.contains(&f.path);
        let file_threads: Vec<&gh::ReviewThread> = threads.iter().filter(|t| t.path == f.path).collect();
        let header = self.diff_file_header_row(
            ix,
            f,
            collapsed,
            viewed,
            file_threads.len(),
            theme,
            colors,
            &entity,
            surface,
            "diff-file",
        );
        let mut card = div()
            .border_1()
            .border_color(theme.border)
            .rounded(px(12.))
            .bg(theme.card)
            .overflow_hidden()
            .flex()
            .flex_col()
            .flex_none()
            .child(header);
        if !collapsed {
            card = card.child(self.file_body(ix, f, surface, &file_threads, theme, colors, entity, cx));
        }
        card.into_any_element()
    }

    /// The expanded body of a file card.
    #[allow(clippy::too_many_arguments)]
    fn file_body(
        &self,
        ix: usize,
        f: &FileDiff,
        surface: DiffSurface,
        file_threads: &[&gh::ReviewThread],
        theme: &Theme,
        colors: &DiffColors,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.files_view(surface);
        let mut body = div().flex().flex_col();
        if f.binary {
            return body
                .child(div().px_2().py_1().text_size(px(11.)).text_color(theme.muted_foreground).child("binary file"))
                .into_any_element();
        }
        let split = self.diff_view_for(surface) == DiffView::Split;
        let can_comment = matches!(surface, DiffSurface::Pr);
        let show_threads = matches!(surface, DiffSurface::Pr) && self.pr.show_comments;
        let acting = self.pr.acting;
        let composer_line = match surface {
            DiffSurface::Pr => match self.pr.composer.as_ref().map(|c| &c.at) {
                Some(ComposerAt::Line { path, line }) if *path == f.path => Some(*line),
                _ => None,
            },
            DiffSurface::LocalDiff => None,
        };
        let mut matched: Vec<usize> = Vec::new();

        // Anything anchored to new-side line `nn`: threads (when shown) and
        // the open composer.
        let mut after_line = |body: gpui::Div, nn: u32| -> gpui::Div {
            let mut body = body;
            if show_threads {
                for (ti, t) in file_threads.iter().enumerate() {
                    if t.line == Some(nn) {
                        matched.push(ti);
                        body = body.child(self.inline_thread_card(ix * 1000 + ti, t, theme, entity.clone(), acting, cx));
                    }
                }
            }
            if composer_line == Some(nn) {
                body = body.child(self.line_composer(theme, entity.clone(), cx));
            }
            body
        };

        let rows: Vec<AnyElement> = Vec::new();
        let _ = rows;
        let split_rows = split.then(|| split_rows(&f.rows));
        match split_rows {
            Some(pairs) => {
                for sr in pairs {
                    match sr {
                        SplitRow::Other(ri) => {
                            body = self.other_row(body, &f.rows[ri], f, ix, surface, view, colors, &entity);
                        }
                        SplitRow::Pair { left, right } => {
                            let l = left.map(|i| &f.rows[i]);
                            let r = right.map(|i| &f.rows[i]);
                            let anchor = r.and_then(|row| match row {
                                DiffRow::Line { new: Some(n), .. } => Some(*n),
                                _ => None,
                            });
                            body = body.child(render_split_pair(l, r, ix, colors, can_comment.then_some(&f.path), entity.clone()));
                            if let Some(nn) = anchor {
                                body = after_line(body, nn);
                            }
                        }
                    }
                }
            }
            None => {
                for row in &f.rows {
                    match row {
                        DiffRow::ExpandGap { .. } => {
                            body = self.other_row(body, row, f, ix, surface, view, colors, &entity);
                        }
                        DiffRow::Line { new, .. } => {
                            let anchor = *new;
                            body = body.child(render_diff_row(row, ix, colors, can_comment.then_some(&f.path), entity.clone()));
                            if let Some(nn) = anchor {
                                body = after_line(body, nn);
                            }
                        }
                        _ => {
                            body = body.child(render_diff_row(row, ix, colors, None, entity.clone()));
                        }
                    }
                }
            }
        }
        // Any thread whose line was None or didn't match a rendered line
        // still gets shown so nothing is dropped.
        if show_threads {
            for (ti, t) in file_threads.iter().enumerate() {
                if !matched.contains(&ti) {
                    body = body.child(self.inline_thread_card(ix * 1000 + ti, t, theme, entity.clone(), acting, cx));
                }
            }
        }
        body.into_any_element()
    }

    /// Render a non-line row (hunk header / expand gap / truncation marker),
    /// with already-expanded context rows ahead of a gap control.
    #[allow(clippy::too_many_arguments)]
    fn other_row(
        &self,
        mut body: gpui::Div,
        row: &DiffRow,
        f: &FileDiff,
        ix: usize,
        surface: DiffSurface,
        view: &DiffViewState,
        colors: &DiffColors,
        entity: &gpui::WeakEntity<App>,
    ) -> gpui::Div {
        match row {
            DiffRow::ExpandGap { file_ix, old_start, new_start, count, section } => {
                // Already-expanded context (highlighted once at click time)
                // renders first; the control stays until the whole gap has
                // been revealed.
                let key = format!("{}:{}", f.path, new_start);
                let shown = if let Some(rows) = view.expanded.get(&key) {
                    for r in rows {
                        body = body.child(render_diff_row(r, ix, colors, None, entity.clone()));
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
                        colors,
                        entity.clone(),
                    ));
                }
            }
            _ => {
                body = body.child(render_diff_row(row, ix, colors, None, entity.clone()));
            }
        }
        body
    }

    /// The line composer with its "Add single comment" / "Start review" (or
    /// "Add to review") actions.
    fn line_composer(&self, theme: &Theme, entity: gpui::WeakEntity<App>, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.pr.composer.as_ref() else { return div().into_any_element() };
        let reviewing = !self.pr.pending.is_empty();
        let mut actions = Vec::new();
        if !reviewing {
            actions.push(composer_button("pr-line-single", "Add single comment", ButtonVariant::Secondary, self.pr.acting, entity.clone(), |this, cx| {
                this.pr_post_line_comment(cx)
            }));
        }
        actions.push(composer_button(
            "pr-line-review",
            if reviewing { "Add to review" } else { "Start review" },
            ButtonVariant::Default,
            self.pr.acting,
            entity.clone(),
            |this, cx| this.pr_add_to_review(cx),
        ));
        div()
            .mx_2()
            .my_1()
            .ml(px(44.))
            .child(self.composer_card("pr-composer-line", c, actions, theme, entity, cx))
            .into_any_element()
    }

    /// A review thread rendered inline under its diff line: comments, then a
    /// Reply… pill (or the reply composer) and Resolve / Unresolve.
    fn inline_thread_card(
        &self,
        ix: usize,
        thread: &gh::ReviewThread,
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        acting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut col = div().flex().flex_col();
        for c in &thread.comments {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(avatar(&c.author, theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_baseline()
                                    .gap_1()
                                    .child(div().text_size(px(11.5)).font_weight(gpui::FontWeight::BOLD).child(c.author.clone()))
                                    .child(div().text_size(px(10.)).text_color(theme.muted_foreground).child(relative_time(&c.created_at))),
                            )
                            .child(div().text_size(px(12.)).whitespace_normal().child(c.body.clone())),
                    ),
            );
        }
        let mut badges = div().flex().flex_row().gap_1().px_3().pt_1();
        if thread.is_resolved {
            badges = badges.child(Badge::new().color(green(theme.dark)).child("Resolved"));
        }
        if thread.is_outdated {
            let yellow = check_style(CheckStatus::Pending, theme.dark).1;
            badges = badges.child(Badge::new().color(yellow.into()).child("Outdated"));
        }
        if thread.is_resolved || thread.is_outdated {
            col = col.child(badges);
        }

        // Reply composer or the Reply… pill + Resolve.
        let reply_here = self
            .pr
            .composer
            .as_ref()
            .filter(|c| matches!(&c.at, ComposerAt::Reply { thread_id, .. } if *thread_id == thread.id));
        let resolved_now = thread.is_resolved;
        let id = thread.id.clone();
        let e = entity.clone();
        let resolve_btn = Button::new(("diff-thread-resolve", ix))
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .disabled(acting || id.is_empty())
            .child(if resolved_now { "Unresolve" } else { "✓ Resolve" })
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                let id = id.clone();
                if let Some(e) = e.upgrade() {
                    e.update(app, move |this, cx| {
                        this.spawn_pr_action(gh::Action::ResolveThread { id, resolved: !resolved_now });
                        cx.notify();
                    });
                }
            });
        if let Some(c) = reply_here {
            let reply = composer_button("pr-reply-submit", "Reply", ButtonVariant::Default, self.pr.acting, entity.clone(), |this, cx| {
                this.pr_post_reply(cx)
            });
            col = col.child(div().p_2().child(self.composer_card("pr-composer-reply", c, vec![reply], theme, entity.clone(), cx)));
        } else {
            let comment_id = thread.comments.first().and_then(|c| c.database_id);
            let thread_id = thread.id.clone();
            let e = entity.clone();
            let mut pill = div()
                .id(("diff-thread-reply", ix))
                .flex_1()
                .px_3()
                .py(px(5.))
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .bg(theme.card)
                .text_size(px(11.5))
                .text_color(theme.muted_foreground)
                .child(if comment_id.is_some() { "Reply…" } else { "Reply unavailable" });
            if let Some(cid) = comment_id {
                pill = pill.cursor_text().on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                    let at = ComposerAt::Reply { thread_id: thread_id.clone(), comment_id: cid };
                    if let Some(e) = e.upgrade() {
                        e.update(app, move |this, cx| this.pr_open_composer(at, window, cx));
                    }
                });
            }
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(7.))
                    .bg(theme.muted)
                    .child(pill)
                    .child(resolve_btn),
            );
        }

        div()
            .mx_2()
            .my_1()
            .ml(px(44.))
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.card)
            .overflow_hidden()
            .child(col)
            .into_any_element()
    }

    /// Sticky header for the topmost visible file card in a stream whose
    /// file cards start at child index `file_offset`.
    ///
    /// gpui at this rev has no `position: sticky`, so we clone the header of
    /// the file that `ScrollHandle::top_item()` reports as topmost and float
    /// it absolutely over the (relative) scroll area. On the very first frame
    /// child bounds are empty: `top_item()` is 0 and `bounds_for_item` is
    /// None, which simply pins nothing (or file 0 when there's no lead-in).
    #[allow(clippy::too_many_arguments)]
    fn sticky_file_header(
        &self,
        render: &DiffRender,
        file_offset: usize,
        surface: DiffSurface,
        threads: &[gh::ReviewThread],
        theme: &Theme,
        colors: &DiffColors,
        entity: &gpui::WeakEntity<App>,
    ) -> Option<(gpui::Stateful<gpui::Div>, f32)> {
        const STICKY_HEADER_H: f32 = 36.0; // approximate header row height (px)
        if render.files.is_empty() {
            return None;
        }
        let view = self.files_view(surface);
        let top_child = view.scroll.top_item();
        if top_child < file_offset {
            return None;
        }
        let top = (top_child - file_offset).min(render.files.len() - 1);
        let f = &render.files[top];
        let collapsed = view.collapsed.contains(&f.path);
        let viewed = view.viewed.contains(&f.path);
        let thread_count = threads.iter().filter(|t| t.path == f.path).count();
        // Shove: as the NEXT card's top approaches the viewport top, push the
        // pinned header up so it slides out exactly as the next file arrives.
        let vp_top = view.scroll.bounds().top();
        let shove = match view.scroll.bounds_for_item(top_child + 1) {
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
            colors,
            entity,
            surface,
            "diff-file-sticky",
        );
        Some((hdr, shove))
    }

    /// The local diff tool's files view: header strip, file cards in a
    /// scrolling column with a sticky header, and the file tree beside it.
    pub(crate) fn render_diff_files(
        &self,
        render: &DiffRender,
        surface: DiffSurface,
        threads: &[gh::ReviewThread],
        theme: &Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.files_view(surface);
        let colors = DiffColors::new(theme);
        let n = render.files.len();
        let total_add: u32 = render.files.iter().map(|f| f.add).sum();
        let total_del: u32 = render.files.iter().map(|f| f.del).sum();
        let viewed_count = render.files.iter().filter(|f| view.viewed.contains(&f.path)).count();

        // ── Header strip (stays put; does not scroll) ──
        let mut summary = div().flex().flex_row().items_center().gap_2().flex_none().min_w(px(0.));
        {
            let e = entity.clone();
            summary = summary.child(
                Button::new("diff-sidebar-toggle")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child(if view.sidebar_open { "◧" } else { "▸" })
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, _cx| this.diff_toggle_sidebar(surface));
                        }
                    }),
            );
        }
        summary = summary
            .child(pill(format!("files changed · {n}"), theme))
            .child(mono(format!("+{total_add}"), colors.green))
            .child(mono(format!("−{total_del}"), colors.red))
            .child(div().flex_1().min_w(px(0.)))
            .child(diff_view_toggle(self.diff_view_for(surface), surface, theme, entity.clone()))
            .child(viewed_progress(viewed_count, n, theme));

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
            cards = cards.child(self.file_card(ix, f, surface, threads, theme, &colors, entity.clone(), cx));
        }
        let sticky = self.sticky_file_header(render, 0, surface, threads, theme, &colors, &entity);
        let cards_area = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(cards)
            .when_some(sticky, |el, (hdr, shove)| el.child(sticky_overlay(hdr, shove, theme)));

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

        let sidebar = view.sidebar_open.then(|| {
            div()
                .id("diff-tree-pane")
                .flex_none()
                .w(px(190.))
                .h_full()
                .min_h(px(0.))
                .overflow_y_scroll()
                .border_r_1()
                .border_color(theme.border)
                .pr_1()
                .child(file_tree(render, threads, view, surface, 0, theme, entity.clone()))
        });

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

/// Width of the wide surface's contact card.
const CONTACT_CARD_W: f32 = 300.0;

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
    /// Whether the file-tree navigation sidebar is showing (local diff tool).
    pub sidebar_open: bool,
    /// The file last selected in the tree, highlighted there.
    pub active_file: Option<String>,
    /// Whether the initial collapse state has been seeded for the loaded diff.
    /// Seeding runs once per diff so a mid-review refresh (e.g. resolving a
    /// thread re-fetches the diff) never discards the user's expand/collapse.
    pub seeded: bool,
    /// Scroll handle for the stream / cards container, so tree links can
    /// scroll their card into view.
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

/// A split-view row: two unified rows paired side by side, or a pass-through
/// row (hunk header / gap / truncation) that spans both columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow {
    /// Indices into the file's `rows` for the old (left) and new (right) side.
    /// A context line appears on both sides; a removal only on the left; an
    /// addition only on the right.
    Pair { left: Option<usize>, right: Option<usize> },
    Other(usize),
}

/// Pair a file's unified rows for the split view: each run of removals is
/// zipped against the run of additions that follows it (GitHub's pairing),
/// the surplus side padded with blanks; context lines mirror on both sides.
pub fn split_rows(rows: &[DiffRow]) -> Vec<SplitRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut dels: Vec<usize> = Vec::new();
    let mut adds: Vec<usize> = Vec::new();
    fn flush(out: &mut Vec<SplitRow>, dels: &mut Vec<usize>, adds: &mut Vec<usize>) {
        let n = dels.len().max(adds.len());
        for k in 0..n {
            out.push(SplitRow::Pair { left: dels.get(k).copied(), right: adds.get(k).copied() });
        }
        dels.clear();
        adds.clear();
    }
    for (ix, row) in rows.iter().enumerate() {
        match row {
            DiffRow::Line { kind: LineKind::Remove, .. } => {
                // A removal after additions starts a new change block.
                if !adds.is_empty() {
                    flush(&mut out, &mut dels, &mut adds);
                }
                dels.push(ix);
            }
            DiffRow::Line { kind: LineKind::Add, .. } => adds.push(ix),
            DiffRow::Line { kind: LineKind::Context, .. } => {
                flush(&mut out, &mut dels, &mut adds);
                out.push(SplitRow::Pair { left: Some(ix), right: Some(ix) });
            }
            _ => {
                flush(&mut out, &mut dels, &mut adds);
                out.push(SplitRow::Other(ix));
            }
        }
    }
    flush(&mut out, &mut dels, &mut adds);
    out
}

/// Theme-derived colours, captured once per render.
#[derive(Clone, Copy)]
struct DiffColors {
    add_bg: gpui::Hsla,
    remove_bg: gpui::Hsla,
    hunk_bg: gpui::Hsla,
    hover_bg: gpui::Hsla,
    muted: gpui::Hsla,
    foreground: gpui::Hsla,
    green: gpui::Hsla,
    red: gpui::Hsla,
    yellow: gpui::Hsla,
    primary: gpui::Hsla,
}

impl DiffColors {
    fn new(theme: &Theme) -> Self {
        DiffColors {
            add_bg: add_bg(theme.dark).into(),
            remove_bg: remove_bg(theme.dark).into(),
            hunk_bg: hunk_bg(theme.dark).into(),
            hover_bg: alpha(theme.primary, 0.10),
            muted: theme.muted_foreground,
            foreground: theme.foreground,
            green: green(theme.dark),
            red: red(theme.dark),
            yellow: check_style(CheckStatus::Pending, theme.dark).1.into(),
            primary: theme.primary,
        }
    }
}

fn diff_gutter(n: Option<u32>, colors: &DiffColors) -> gpui::Div {
    div()
        .w(px(34.))
        .flex_none()
        .pr_1()
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

/// Highlighted code text for a diff line.
fn line_text(text: &SharedString, runs: &[(Range<usize>, gpui::Hsla)]) -> StyledText {
    let mut styled = StyledText::new(text.clone());
    if !runs.is_empty() {
        styled = styled.with_highlights(
            runs.iter()
                .map(|(r, c)| (r.clone(), HighlightStyle { color: Some(*c), ..Default::default() })),
        );
    }
    styled
}

/// Make a line row clickable: opens the line composer at `path:new`. Only
/// rows on the new side (adds + context) accept comments.
fn commentable(
    el: gpui::Div,
    key: usize,
    path: Option<&String>,
    new: Option<u32>,
    colors: &DiffColors,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    match (path, new) {
        (Some(path), Some(line)) => {
            let path = path.clone();
            el.id(("diff-ln", key))
                .cursor_pointer()
                .hover(|s| s.bg(colors.hover_bg))
                .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                    let at = ComposerAt::Line { path: path.clone(), line };
                    if let Some(e) = entity.upgrade() {
                        e.update(app, move |this, cx| this.pr_open_composer(at, window, cx));
                    }
                })
                .into_any_element()
        }
        _ => el.into_any_element(),
    }
}

/// A unified diff row. `path` (when `Some`) makes new-side lines clickable
/// to open the comment composer; `file_ix` keys the element id.
fn render_diff_row(
    row: &DiffRow,
    file_ix: usize,
    colors: &DiffColors,
    path: Option<&String>,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
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
            let el = div()
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
                .child(div().w(px(12.)).flex_none().text_color(sign_color).child(sign))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(line_text(text, runs)));
            // Key by the new-side (or old-side) line so ids stay unique per file.
            let key = file_ix * 1_000_000 + new.map(|n| n as usize).unwrap_or(0) * 2 + old.map(|_| 1).unwrap_or(0);
            commentable(el, key, path, *new, colors, entity)
        }
    }
}

/// One split-view row: the old side and the new side, half width each.
fn render_split_pair(
    left: Option<&DiffRow>,
    right: Option<&DiffRow>,
    file_ix: usize,
    colors: &DiffColors,
    path: Option<&String>,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let side = |row: Option<&DiffRow>, old_side: bool| -> gpui::Div {
        let base = div()
            .flex_1()
            .min_w(px(0.))
            .h(px(DIFF_ROW_H))
            .flex()
            .flex_row()
            .items_center()
            .whitespace_nowrap();
        match row {
            Some(DiffRow::Line { kind, old, new, text, runs }) => {
                let (bg, sign, sign_color) = match kind {
                    LineKind::Add => (Some(colors.add_bg), "+", colors.green),
                    LineKind::Remove => (Some(colors.remove_bg), "−", colors.red),
                    LineKind::Context => (None, " ", colors.muted),
                };
                base.when_some(bg, |el, c| el.bg(c))
                    .child(diff_gutter(if old_side { *old } else { *new }, colors))
                    .child(div().w(px(12.)).flex_none().text_color(sign_color).child(sign))
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(line_text(text, runs)))
            }
            _ => base.bg(alpha(colors.muted, 0.06)),
        }
    };
    let new_line = right.and_then(|r| match r {
        DiffRow::Line { new, .. } => *new,
        _ => None,
    });
    let key = file_ix * 1_000_000
        + new_line.map(|n| n as usize).unwrap_or(0) * 2
        + left.map(|_| 1).unwrap_or(0);
    let el = div()
        .w_full()
        .flex()
        .flex_row()
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(12.))
        .text_color(colors.foreground)
        .child(side(left, true).border_r_1().border_color(alpha(colors.muted, 0.25)))
        .child(side(right, false));
    commentable(el, key, path, new_line, colors, entity)
}

/// The Unified | Split segmented control.
fn diff_view_toggle(current: DiffView, surface: DiffSurface, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let seg = |label: &'static str, which: DiffView, e: gpui::WeakEntity<App>| {
        let active = current == which;
        div()
            .id(("diff-view", which as usize))
            .px_2()
            .py(px(2.))
            .rounded(px(5.))
            .text_size(px(10.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .cursor_pointer()
            .text_color(if active { theme.foreground } else { theme.muted_foreground })
            .when(active, |d| d.bg(theme.card).shadow_sm())
            .child(label)
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = e.upgrade() {
                    e.update(app, |this, cx| {
                        this.set_diff_view(surface, which);
                        cx.notify();
                    });
                }
            })
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .p(px(2.))
        .rounded(px(7.))
        .bg(theme.muted)
        .child(seg("Unified", DiffView::Unified, entity.clone()))
        .child(seg("Split", DiffView::Split, entity))
        .into_any_element()
}

/// The sticky file header floated over a scroll area.
fn sticky_overlay(hdr: gpui::Stateful<gpui::Div>, shove: f32, theme: &Theme) -> AnyElement {
    div()
        .absolute()
        .top(px(shove))
        .left_0()
        .right_0()
        // Opaque fill so scrolled lines don't bleed through.
        .bg(theme.card)
        .rounded(px(12.))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .shadow_sm()
        .child(hdr)
        .into_any_element()
}

/// The file tree (directories as labels, files as rows with status circle,
/// ± counts, thread badge and viewed check). Clicking a file expands its card
/// and scrolls it into view; `file_offset` is the stream index of file 0.
fn file_tree(
    render: &DiffRender,
    threads: &[gh::ReviewThread],
    view: &DiffViewState,
    surface: DiffSurface,
    file_offset: usize,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    use crate::file_tree::{build_file_tree, FileTreeNode};

    let colors = DiffColors::new(theme);
    let mut meta: std::collections::HashMap<&str, &FileDiff> = std::collections::HashMap::new();
    for f in &render.files {
        meta.insert(f.path.as_str(), f);
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

    let mut list = div().flex().flex_col().pb_2();
    for (ix, (depth, node)) in rows.into_iter().enumerate() {
        let indent = px(2.0 + depth as f32 * 10.0);
        match node {
            FileTreeNode::Dir { name, .. } => {
                list = list.child(
                    div()
                        .pl(indent)
                        .pt(px(7.))
                        .pb(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(name.clone()),
                );
            }
            FileTreeNode::File { name, path } => {
                let Some(f) = meta.get(path.as_str()).copied() else { continue };
                let tc = tcount.get(path.as_str()).copied().unwrap_or(0);
                let active = view.active_file.as_deref() == Some(path.as_str());
                let viewed = view.viewed.contains(path);
                let e = entity.clone();
                let p = path.clone();
                let file_ix = render.files.iter().position(|f| f.path == *path);
                let (fg, sub_add, sub_del) = if active {
                    (theme.primary_foreground, alpha(theme.primary_foreground, 0.85), alpha(theme.primary_foreground, 0.85))
                } else {
                    (if viewed { theme.muted_foreground } else { theme.foreground }, colors.green, colors.red)
                };
                let mut row = div()
                    .id(("diff-tree-file", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .pl(indent)
                    .pr(px(6.))
                    .py(px(5.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme.primary))
                    .when(!active, |d| d.hover(|s| s.bg(alpha(theme.primary, 0.08))))
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        let p = p.clone();
                        if let Some(e) = e.upgrade() {
                            e.update(app, move |this, cx| {
                                this.diff_select_file(surface, &p);
                                if let Some(ix) = file_ix {
                                    this.files_view_mut(surface).scroll.scroll_to_top_of_item(file_offset + ix);
                                }
                                cx.notify();
                            });
                        }
                    });
                row = row.child(status_circle(f.status_tag, px(24.), theme, &colors));
                let mut name_col = div().flex_1().min_w(px(0.)).child(
                    div()
                        .truncate()
                        .font_family(crate::renderer::FONT_FAMILY)
                        .text_size(px(12.))
                        .text_color(fg)
                        .child(name.clone()),
                );
                if f.binary {
                    name_col = name_col.child(div().text_size(px(9.5)).text_color(fg).child("binary"));
                } else {
                    name_col = name_col.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(mono(format!("+{}", f.add), sub_add))
                            .child(mono(format!("−{}", f.del), sub_del)),
                    );
                }
                row = row.child(name_col);
                if tc > 0 {
                    row = row.child(comment_badge(tc, theme));
                }
                if viewed {
                    row = row.child(div().flex_none().text_size(px(12.)).text_color(if active { theme.primary_foreground } else { colors.green }).child("✓"));
                }
                list = list.child(row);
            }
        }
    }
    list.into_any_element()
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
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(avatar(author, theme))
        .child(div().font_weight(gpui::FontWeight::BOLD).text_size(px(12.)).child(format!("@{author}")));
    if let Some(state) = review_state {
        head = head.child(review_badge(state, theme));
    }
    head = head
        .child(div().text_size(px(10.5)).text_color(theme.muted_foreground).child(relative_time(created_at)))
        .child(div().flex_1().min_w(px(0.)));
    Card::new()
        .size(CardSize::Sm)
        .child(CardHeader::new().size(CardSize::Sm).child(head))
        .child(CardContent::new().size(CardSize::Sm).child(crate::markdown::render_prepared(body, ctx)))
        .into_any_element()
}

/// A run of consecutive commits as one quiet line: `sha message · when`, or a
/// count plus the list when there are more than two.
fn commit_group_entry(events: &[TimelineEvent], theme: &Theme) -> AnyElement {
    let mut col = div().flex().flex_col().gap_1().px_2().py_1();
    if events.len() > 2 {
        col = col.child(
            div()
                .text_size(px(10.))
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
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .child(mono(sha7, theme.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(e.commit_message.clone().unwrap_or_default()),
                )
                .child(div().flex_none().child(relative_time(&e.created_at))),
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
        .text_size(px(10.))
        .text_color(theme.muted_foreground)
        .child(div().flex_none().text_color(color).child(glyph))
        .child(div().flex_1().min_w(px(0.)).truncate().child(event_description(e)))
        .child(div().flex_none().child(relative_time(&e.created_at)))
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

/// A review thread in the conversation stream: file:line header with
/// resolved/outdated pills and a resolve/unresolve button, the diff hunk it
/// hangs off, and its (markdown) comments.
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
                        .child(avatar(&c.author, theme))
                        .child(div().font_weight(gpui::FontWeight::MEDIUM).text_size(px(11.)).child(format!("@{}", c.author)))
                        .child(div().flex_1().min_w(px(0.)))
                        .child(div().flex_none().text_size(px(10.)).text_color(theme.muted_foreground).child(relative_time(&c.created_at))),
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

/// Whether a review can be submitted with `event`: GitHub requires a body for
/// Request changes, and a bare Comment review with nothing in it is a no-op.
/// Returns the message to show when it can't.
fn review_submittable(event: gh::ReviewEvent, has_body: bool, has_comments: bool) -> Result<(), &'static str> {
    match event {
        gh::ReviewEvent::Approve => Ok(()),
        gh::ReviewEvent::RequestChanges if has_body || has_comments => Ok(()),
        gh::ReviewEvent::RequestChanges => Err("Request changes needs a summary or at least one comment."),
        gh::ReviewEvent::Comment if has_body || has_comments => Ok(()),
        gh::ReviewEvent::Comment => Err("Nothing to submit: add a comment or pick a verdict."),
    }
}

// ── Readiness ──────────────────────────────────────────────────────────────

/// One Readiness row's presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadinessRow {
    pub glyph: &'static str,
    pub color: gpui::Hsla,
    pub title: String,
    pub sub: String,
}

/// The three Readiness rows: checks, review, and draft-or-mergeability.
pub struct Readiness {
    pub checks: ReadinessRow,
    pub review: ReadinessRow,
    pub state: ReadinessRow,
}

/// Derive the Readiness rows from a PR detail.
fn readiness(d: &PrDetail, theme: &Theme) -> Readiness {
    let dark = theme.dark;
    let muted = theme.muted_foreground;
    let (ok_c, bad_c, pend_c) = (green(dark), red(dark), check_style(CheckStatus::Pending, dark).1.into());

    let checks = {
        let total = d.checks.len();
        let passed = d.checks.iter().filter(|c| c.status == CheckStatus::Success).count();
        let failing: Vec<&str> = d
            .checks
            .iter()
            .filter(|c| matches!(c.status, CheckStatus::Failure | CheckStatus::Cancelled))
            .map(|c| c.name.as_str())
            .collect();
        let running: Vec<&str> =
            d.checks.iter().filter(|c| c.status == CheckStatus::Pending).map(|c| c.name.as_str()).collect();
        let mut sub_parts = Vec::new();
        if let Some(f) = failing.first() {
            sub_parts.push(format!("{f} failing{}", if failing.len() > 1 { format!(" (+{})", failing.len() - 1) } else { String::new() }));
        }
        if let Some(r) = running.first() {
            sub_parts.push(format!("{r} running{}", if running.len() > 1 { format!(" (+{})", running.len() - 1) } else { String::new() }));
        }
        match gh::aggregate_check_status(&d.checks) {
            None => ReadinessRow { glyph: "–", color: muted, title: "No checks".into(), sub: "nothing reported yet".into() },
            Some(CheckStatus::Failure) => ReadinessRow {
                glyph: "✕",
                color: bad_c,
                title: format!("Checks · {passed} of {total}"),
                sub: sub_parts.join(" · "),
            },
            Some(CheckStatus::Pending) => ReadinessRow {
                glyph: "●",
                color: pend_c,
                title: format!("Checks · {passed} of {total}"),
                sub: sub_parts.join(" · "),
            },
            Some(_) => ReadinessRow { glyph: "✓", color: ok_c, title: format!("Checks · {total} passed"), sub: "all green".into() },
        }
    };

    let review = match d.review_decision.as_deref() {
        Some("APPROVED") => ReadinessRow { glyph: "✓", color: ok_c, title: "Approved".into(), sub: "ready to merge".into() },
        Some("CHANGES_REQUESTED") => {
            ReadinessRow { glyph: "✕", color: bad_c, title: "Changes requested".into(), sub: "address the review".into() }
        }
        Some("REVIEW_REQUIRED") => {
            ReadinessRow { glyph: "●", color: pend_c, title: "Review required".into(), sub: "at least 1 approval".into() }
        }
        _ => ReadinessRow { glyph: "●", color: pend_c, title: "Review pending".into(), sub: "no decision yet".into() },
    };

    let conflicts = d.mergeable.as_deref() == Some("CONFLICTING");
    let state = if d.state == "merged" {
        ReadinessRow { glyph: "◆", color: pr_state_colors(dark).0.into(), title: "Merged".into(), sub: format!("into {}", d.base) }
    } else if d.state == "closed" {
        ReadinessRow { glyph: "○", color: muted, title: "Closed".into(), sub: "not merged".into() }
    } else if conflicts {
        ReadinessRow {
            glyph: "✕",
            color: bad_c,
            title: if d.is_draft { "Draft · conflicts".into() } else { "Conflicts".into() },
            sub: format!("must be resolved against {}", d.base),
        }
    } else if d.is_draft {
        ReadinessRow { glyph: "◐", color: muted, title: "Draft".into(), sub: format!("no conflicts with {}", d.base) }
    } else {
        ReadinessRow { glyph: "✓", color: ok_c, title: "Mergeable".into(), sub: format!("no conflicts with {}", d.base) }
    };

    Readiness { checks, review, state }
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

/// Uppercase section label ("READINESS", "FILES").
fn section_label(text: &str, theme: &Theme) -> AnyElement {
    div()
        .pt(px(6.))
        .pb(px(4.))
        .px(px(2.))
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme.muted_foreground)
        .child(text.to_uppercase())
        .into_any_element()
}

/// A labelled hairline divider between stream sections.
fn section_divider(text: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .mt_2()
        .px(px(2.))
        .child(
            div()
                .flex_none()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme.muted_foreground)
                .child(text.to_uppercase()),
        )
        .child(div().flex_1().h(px(1.)).bg(theme.border))
        .into_any_element()
}

/// A rounded muted pill of small bold text.
fn pill(text: String, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .px_3()
        .py(px(3.))
        .rounded_full()
        .bg(theme.muted)
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .child(text)
        .into_any_element()
}

/// Small bold monospace text (± counts, shas).
fn mono(text: String, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_none()
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// The viewed progress bar + "n/N viewed".
fn viewed_progress(viewed: usize, total: usize, theme: &Theme) -> AnyElement {
    let frac = if total == 0 { 0.0 } else { viewed as f32 / total as f32 };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .flex_none()
        .child(
            div()
                .w(px(76.))
                .h(px(4.))
                .rounded(px(2.))
                .bg(theme.muted)
                .overflow_hidden()
                .child(div().h_full().w(px(76.0 * frac)).rounded(px(2.)).bg(theme.primary)),
        )
        .child(
            div()
                .font_family(crate::renderer::FONT_FAMILY)
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
                .child(format!("{viewed}/{total} viewed")),
        )
        .into_any_element()
}

/// The thread-count badge (💬 n).
fn comment_badge(n: usize, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .px_2()
        .py(px(1.))
        .rounded_full()
        .bg(alpha(theme.primary, 0.12))
        .text_size(px(9.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme.primary)
        .child(format!("💬 {n}"))
        .into_any_element()
}

/// The round status letter (A/M/D/R/…) tinted by kind.
fn status_circle(tag: &'static str, size: gpui::Pixels, theme: &Theme, colors: &DiffColors) -> AnyElement {
    let c = match tag {
        "A" => colors.green,
        "D" => colors.red,
        "R" | "C" => colors.primary,
        _ => colors.yellow,
    };
    let _ = theme;
    div()
        .flex_none()
        .size(size)
        .rounded_full()
        .bg(alpha(c, 0.14))
        .flex()
        .items_center()
        .justify_center()
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(c)
        .child(tag)
        .into_any_element()
}

/// A small round avatar: the author's initial on a hue derived from the name.
fn avatar(author: &str, theme: &Theme) -> AnyElement {
    let hash = author.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let hue = (hash % 360) as f32 / 360.0;
    let bg = gpui::hsla(hue, 0.45, if theme.dark { 0.45 } else { 0.55 }, 1.0);
    let initial: String = author.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    div()
        .flex_none()
        .size(px(22.))
        .rounded_full()
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(gpui::white())
        .child(initial)
        .into_any_element()
}

/// A branch name as a small accent-tinted monospace chip.
fn branch_chip(name: &str, theme: &Theme) -> AnyElement {
    div()
        .max_w(px(130.))
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .bg(alpha(theme.primary, 0.10))
        .font_family(crate::renderer::FONT_FAMILY)
        .text_size(px(10.5))
        .text_color(theme.primary)
        .truncate()
        .child(name.to_string())
        .into_any_element()
}

/// A round icon button with a caption underneath (GitHub / Refresh / Session).
fn round_action(
    id: &'static str,
    glyph: &'static str,
    caption: &'static str,
    theme: &Theme,
    on_click: impl Fn(&mut Window, &mut GpuiApp) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.))
        .cursor_pointer()
        .child(
            div()
                .size(px(34.))
                .rounded_full()
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .text_color(theme.foreground)
                .child(glyph),
        )
        .child(div().text_size(px(9.5)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child(caption))
        .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| on_click(window, app))
        .into_any_element()
}

fn back_button(id: &'static str, label: &'static str, entity: gpui::WeakEntity<App>) -> AnyElement {
    Button::new(id)
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::Sm)
        .child(label)
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
            if let Some(e) = entity.upgrade() {
                e.update(app, |this, cx| {
                    this.close_pr_detail();
                    cx.notify();
                });
            }
        })
        .into_any_element()
}

fn github_button(id: &'static str, url: &str) -> AnyElement {
    let url = url.to_string();
    Button::new(id)
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::Sm)
        .child("\u{2197}")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, _app: &mut GpuiApp| {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        })
        .into_any_element()
}

/// Refresh: the open PR when one is showing, else the surface's list.
fn refresh_button(id: &'static str, all: bool, entity: gpui::WeakEntity<App>) -> AnyElement {
    Button::new(id)
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Sm)
        .child("Refresh")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
            if let Some(e) = entity.upgrade() {
                e.update(app, |this, cx| {
                    match this.pr.open {
                        Some(n) => this.refresh_open_pr(n),
                        None if all => this.spawn_pr_all_list(),
                        None => this.spawn_pr_branch_list(),
                    }
                    cx.notify();
                });
            }
        })
        .into_any_element()
}

/// A composer footer button that runs an `App` method.
fn composer_button(
    id: &'static str,
    label: &'static str,
    variant: ButtonVariant,
    acting: bool,
    entity: gpui::WeakEntity<App>,
    run: impl Fn(&mut App, &mut Context<App>) + 'static,
) -> AnyElement {
    Button::new(id)
        .variant(variant)
        .size(ButtonSize::Sm)
        .disabled(acting)
        .child(label)
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
            if let Some(e) = entity.upgrade() {
                e.update(app, |this, cx| {
                    run(this, cx);
                    cx.notify();
                });
            }
        })
        .into_any_element()
}

fn state_label(state: &str, is_draft: bool) -> &'static str {
    if is_draft {
        return "Draft";
    }
    match state {
        "merged" => "Merged",
        "open" => "Open",
        "closed" => "Closed",
        _ => "PR",
    }
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
    use crate::gh::Check;

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

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> DiffRow {
        DiffRow::Line { kind, old, new, text: "x".into(), runs: Vec::new() }
    }

    #[test]
    fn split_rows_pairs_removals_with_following_additions() {
        let rows = vec![
            DiffRow::Hunk("@@".into()),
            line(LineKind::Context, Some(1), Some(1)),
            line(LineKind::Remove, Some(2), None),
            line(LineKind::Remove, Some(3), None),
            line(LineKind::Add, None, Some(2)),
            line(LineKind::Context, Some(4), Some(3)),
            line(LineKind::Add, None, Some(4)),
            line(LineKind::Remove, Some(5), None),
        ];
        let split = split_rows(&rows);
        assert_eq!(
            split,
            vec![
                SplitRow::Other(0),
                SplitRow::Pair { left: Some(1), right: Some(1) },
                // Two removals zipped against one addition: the surplus
                // removal gets a blank right side.
                SplitRow::Pair { left: Some(2), right: Some(4) },
                SplitRow::Pair { left: Some(3), right: None },
                SplitRow::Pair { left: Some(5), right: Some(5) },
                // An addition followed by a removal is two blocks, not a pair.
                SplitRow::Pair { left: None, right: Some(6) },
                SplitRow::Pair { left: Some(7), right: None },
            ]
        );
        assert!(split_rows(&[]).is_empty());
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
            state: "open".into(),
            base: "main".into(),
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

    #[test]
    fn readiness_rows_summarize_checks_review_and_state() {
        let theme = Theme::from_chrome(&crate::theme::ARC_LIGHT);
        let mut d = detail_with(None, Some("APPROVED"), vec![CheckStatus::Success, CheckStatus::Failure, CheckStatus::Pending]);
        d.checks[1].name = "wasm-build".into();
        d.checks[2].name = "clippy".into();
        let r = readiness(&d, &theme);
        assert_eq!(r.checks.title, "Checks · 1 of 3");
        assert_eq!(r.checks.sub, "wasm-build failing · clippy running");
        assert_eq!(r.checks.glyph, "✕");
        assert_eq!(r.review.title, "Approved");
        assert_eq!(r.state.title, "Mergeable");

        let mut draft = detail_with(Some("CONFLICTING"), None, vec![]);
        draft.is_draft = true;
        let r = readiness(&draft, &theme);
        assert_eq!(r.checks.title, "No checks");
        assert_eq!(r.review.title, "Review pending");
        assert_eq!(r.state.title, "Draft · conflicts");

        let all_green = detail_with(None, Some("REVIEW_REQUIRED"), vec![CheckStatus::Success, CheckStatus::Success]);
        let r = readiness(&all_green, &theme);
        assert_eq!(r.checks.title, "Checks · 2 passed");
        assert_eq!(r.review.title, "Review required");
    }

    #[test]
    fn review_submittable_rules() {
        use gh::ReviewEvent::*;
        assert!(review_submittable(Approve, false, false).is_ok());
        assert!(review_submittable(Comment, true, false).is_ok());
        assert!(review_submittable(Comment, false, true).is_ok());
        assert!(review_submittable(Comment, false, false).is_err());
        assert!(review_submittable(RequestChanges, true, false).is_ok());
        assert!(review_submittable(RequestChanges, false, true).is_ok());
        assert!(review_submittable(RequestChanges, false, false).is_err());
    }

    #[test]
    fn state_labels() {
        assert_eq!(state_label("open", true), "Draft");
        assert_eq!(state_label("open", false), "Open");
        assert_eq!(state_label("merged", false), "Merged");
        assert_eq!(state_label("weird", false), "PR");
    }
}
