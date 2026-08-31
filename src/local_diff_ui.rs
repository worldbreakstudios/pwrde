//! Local diff tool: a gpui element-tree panel that reviews the working tree's
//! own changes, rendered with the same syntax-highlighted hunks as the PR
//! files view.
//!
//! Two modes toggle what's shown: **working** (uncommitted changes vs `HEAD`,
//! including untracked files) and **branch** (this branch's committed changes
//! vs its merge-base with the remote default branch). The diff is gathered off
//! the UI thread via [`crate::git::local_diff`] and delivered back as a
//! `TermEvent`. Like the PR tool, this only appears for git-backed groups.

use gpui::{
    div, px, AnyElement, App as GpuiApp, ClickEvent, Context, IntoElement, ParentElement, Styled,
    Window,
};

use std::sync::Arc;

use crate::git::DiffMode;
use crate::pr_ui::{build_diff_render, DiffSurface, DiffView, DiffViewState, Load, LocalDiffRender};
use crate::ui::theme::Theme;
use crate::ui::{Button, ButtonSize, ButtonVariant, CardTitle};
use crate::App;

/// The local diff tool's state.
pub struct LocalDiffState {
    pub mode: DiffMode,
    pub data: Load<LocalDiffRender>,
    /// Collapse/viewed state for the local diff files list.
    pub files_view: DiffViewState,
    /// Unified or split diff bodies.
    pub diff_view: DiffView,
}

impl Default for LocalDiffState {
    fn default() -> Self {
        LocalDiffState {
            mode: DiffMode::Branch,
            data: Load::Idle,
            files_view: DiffViewState::default(),
            diff_view: DiffView::Unified,
        }
    }
}

impl App {
    /// (Re)gather the local diff for the active group's repo in the current mode.
    pub fn spawn_local_diff(&mut self) {
        let Some(dir) = self.active_repo_dir() else { return };
        let mode = self.local_diff.mode;
        self.local_diff.data = Load::Loading;
        let tx = self.events_tx.clone();
        let dark = crate::theme::dark_active();
        std::thread::spawn(move || {
            // Gather, parse, and highlight off-thread; the UI only stores the
            // ready-to-render result.
            let result = crate::git::local_diff(&dir, mode, None).map(|ld| LocalDiffRender {
                base_ref: ld.base_ref,
                render: Arc::new(build_diff_render(&ld.files, dark)),
            });
            let _ = tx.send(crate::term::TermEvent::LocalDiffLoaded(result));
        });
        self.request_redraw();
    }

    /// Switch mode and re-gather.
    pub fn set_local_diff_mode(&mut self, mode: DiffMode) {
        if self.local_diff.mode != mode {
            self.local_diff.mode = mode;
            // A different mode is a different file set, so re-seed collapse.
            self.local_diff.files_view = crate::pr_ui::DiffViewState::default();
            self.spawn_local_diff();
        }
    }

    pub fn on_local_diff_loaded(&mut self, result: Result<LocalDiffRender, String>) {
        self.local_diff.data = match result {
            Ok(d) => Load::Ready(d),
            Err(e) => Load::Failed(e),
        };
        if let Load::Ready(d) = &self.local_diff.data {
            let render = d.render.clone();
            self.local_diff.files_view.seed(&render.files);
        }
    }

    /// Build the Local diff panel overlay. Call only when the local diff tool
    /// is the visible tool.
    pub fn render_local_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        // Mode toggle + refresh.
        let mode = self.local_diff.mode;
        let base_label = match &self.local_diff.data {
            Load::Ready(d) => format!("vs {}", d.base_ref),
            _ => match mode {
                DiffMode::Working => "vs HEAD".into(),
                DiffMode::Branch => "vs base".into(),
            },
        };

        let mk_mode = |id: &'static str, label: &str, which: DiffMode| {
            let active = mode == which;
            let e = entity.clone();
            Button::new(id)
                .variant(if active { ButtonVariant::Default } else { ButtonVariant::Ghost })
                .size(ButtonSize::Sm)
                .child(label.to_string())
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(e) = e.upgrade() {
                        e.update(app, |this, cx| {
                            this.set_local_diff_mode(which);
                            cx.notify();
                        });
                    }
                })
        };

        let float_entity = entity.clone();
        let refresh_entity = entity.clone();
        let actions = div()
            .flex_1()
            .flex()
            .justify_end()
            .child(div().flex().flex_row().items_center().gap_2()
                .child(
                    Button::new("ld-float-toggle")
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
                )
                .child(
                    Button::new("ld-refresh")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .child("Refresh")
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = refresh_entity.upgrade() {
                                e.update(app, |this, cx| {
                                    this.spawn_local_diff();
                                    cx.notify();
                                });
                            }
                        }),
                ),
            );
        let header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap_2()
            .child(CardTitle::new().child("Local diff"))
            .child(mk_mode("ld-branch", "Branch", DiffMode::Branch))
            .child(mk_mode("ld-working", "Working", DiffMode::Working))
            .child(actions);

        let sub = div()
            .px_1()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .child(base_label);

        let body = match &self.local_diff.data {
            Load::Idle | Load::Loading => skeleton(),
            Load::Failed(e) => failed(e, &theme),
            Load::Ready(d) if d.render.is_empty() => centered(
                match mode {
                    DiffMode::Working => "No uncommitted changes.",
                    DiffMode::Branch => "No changes vs base branch.",
                },
                theme.muted_foreground,
            ),
            Load::Ready(d) => {
                self.render_diff_files(&d.render, DiffSurface::LocalDiff, &[], &theme, entity.clone(), cx)
            }
        };

        crate::pr_ui::tool_panel_overlay(
            self.tool_panel_floating,
            self.tool_panel_w,
            div().flex().flex_col().gap_1().child(header_row).child(sub).into_any_element(),
            body.into_any_element(),
            self.glass_backdrop_el(gpui::Corners::all(theme.radius_xl())),
        )
    }
}

fn skeleton() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .p(px(8.))
        .children((0..6).map(|i| {
            crate::ui::Skeleton::new()
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
