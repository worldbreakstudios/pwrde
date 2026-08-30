//! Flow agent surface: the bottom-centered pill bar and the chat panel that
//! expands above it, as a gpui element tree mounted on every page — Flow
//! follows the user around the app (same overlay pattern as `local_diff_ui`). This module owns only presentation
//! and the app-side glue (`toggle_flow`, `flow_send`, composer lifecycle);
//! the transcript it renders is `flow::FlowState`, reduced from backend
//! events on the main thread, and the agent itself sits behind
//! `flow::AgentBackend` — nothing here knows which agent is on the other end.
//!
//! Gated on the experimental `features.flow` flag; `main.rs` only mounts
//! `render_flow` while it is on, and `toggle_flow`/`flow_send` refuse when
//! it is off so the bus and the palette report a no-op instead of spawning.
use std::cell::Cell;

use gpui::{
    AppContext as _, div, point, prelude::FluentBuilder as _, px, AnyElement, App as GpuiApp, BoxShadow, ClickEvent,
    Context, Entity, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};

use crate::flow::{ActionStatus, FlowMsg};
use crate::pages::Action;
use crate::ui::theme::Theme;
use crate::App;

/// Pill bar / panel width at the default app font size (shrinks on narrow
/// windows, grows with `appearance.font_size`).
const BAR_W: f32 = 480.0;
/// Pill bar height at the default app font size.
const BAR_H: f32 = 46.0;
/// Breathing room between the reserved safe area and the tile above it.
const SAFE_GAP: f32 = 8.0;
/// Gap between the bar and the window's bottom edge.
const BOTTOM_INSET: f32 = 14.0;
/// The expanded panel never exceeds this fraction of the window height.
const PANEL_MAX_FRAC: f32 = 0.52;

/// Logical height of the bottom safe area the Sessions/tool-page layouts
/// reserve while the flag is on (`App::flow_inset`): inset + pill bar + gap,
/// tracking the app font scale so a larger accessibility font still clears
/// the bar.
pub fn safe_area_h() -> f32 {
    BOTTOM_INSET + BAR_H * crate::renderer::chrome_font_scale() + SAFE_GAP
}

/// The pill bar's composer: a `gpui_component` textarea plus the transcript
/// scroll handle, created lazily on the first render after the flag is on.
pub struct FlowComposer {
    pub editor: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Message count at the last render; a change scrolls the transcript
    /// to the bottom so the newest reply is visible.
    seen: Cell<usize>,
    _sub: Subscription,
}

impl App {
    /// True while the Flow composer owns keyboard focus (`on_key_down` then
    /// leaves typing to the textarea).
    pub fn flow_editor_focused(&self, window: &Window, cx: &GpuiApp) -> bool {
        use gpui::Focusable as _;
        self.flow_composer
            .as_ref()
            .is_some_and(|c| c.editor.read(cx).focus_handle(cx).is_focused(window))
    }

    /// `Action::ToggleFlow`: expand/collapse the panel (any page). Returns
    /// whether it applied — false when the flag is off, so the bus reports a
    /// no-op rather than a silent success.
    pub(crate) fn toggle_flow(&mut self) -> bool {
        if !crate::flow::enabled() {
            return false;
        }
        self.flow.open = !self.flow.open;
        if self.flow.open {
            self.ensure_flow_backend();
            self.flow.wants_focus = true;
        }
        self.request_redraw();
        true
    }

    /// Spawn the agent on first use (never while the flag is off). On
    /// failure the reason lands in `draft_error` for the panel to show.
    fn ensure_flow_backend(&mut self) -> bool {
        if !crate::flow::enabled() {
            return false;
        }
        if self.flow_backend.is_some() {
            return true;
        }
        let cwd = self
            .active_repo_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match crate::flow::spawn_flow_backend(&cwd, self.events_tx.clone()) {
            Some(backend) => {
                self.flow_backend = Some(backend);
                true
            }
            None => {
                self.flow.draft_error =
                    Some("Could not start the agent — is `claude` on your PATH?".into());
                false
            }
        }
    }

    /// Hand one user request to the agent (composer ↩, `pwrde-cli flow-send`).
    /// A dead backend (write fails) is replaced once and the send retried.
    pub(crate) fn flow_send(&mut self, text: String) -> Result<(), String> {
        if !crate::flow::enabled() {
            return Err("Flow is disabled — enable it under Settings > Feature Flags".into());
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("nothing to send".into());
        }
        if self.flow.busy {
            return Err("Flow is still working on the previous request".into());
        }
        for attempt in 0..2 {
            if !self.ensure_flow_backend() {
                return Err(self.flow.draft_error.clone().unwrap_or_default());
            }
            let backend = self.flow_backend.as_mut().expect("ensured");
            match backend.send(&text) {
                Ok(()) => {
                    self.flow.push_user(text);
                    self.flow.open = true;
                    self.request_redraw();
                    return Ok(());
                }
                Err(e) => {
                    backend.shutdown();
                    self.flow_backend = None;
                    if attempt == 1 {
                        let msg = format!("agent write failed: {e}");
                        self.flow.draft_error = Some(msg.clone());
                        self.request_redraw();
                        return Err(msg);
                    }
                }
            }
        }
        unreachable!()
    }

    fn ensure_flow_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.flow_composer.is_some() {
            return;
        }
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Ask Flow to do anything — spawn, review, merge…")
        });
        // ↩ submits (⇧↩ inserts a newline; `submit_on_enter` handles that).
        let sub = cx.subscribe_in(
            &editor,
            window,
            |this: &mut App, _editor, ev: &InputEvent, window: &mut Window, cx: &mut Context<App>| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.flow_submit_composer(window, cx);
                }
            },
        );
        self.flow_composer = Some(FlowComposer {
            editor,
            scroll: ScrollHandle::new(),
            seen: Cell::new(0),
            _sub: sub,
        });
    }

    /// Send the composer's text and clear it on success; a refusal shows
    /// under the transcript instead.
    fn flow_submit_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.flow_composer.as_ref().map(|c| c.editor.clone()) else { return };
        let text = editor.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        match self.flow_send(text) {
            Ok(()) => editor.update(cx, |s, cx| s.set_value("", window, cx)),
            Err(e) => self.flow.draft_error = Some(e),
        }
        cx.notify();
    }

    /// The pill bar (always, while the flag is on, on every page) plus the
    /// chat panel above it while `flow.open`.
    pub fn render_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_flow_composer(window, cx);
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let entity = cx.entity().downgrade();
        // Track the app text size (Settings → Accessibility): every size in
        // the bar and panel scales with `appearance.font_size`.
        let fs = crate::renderer::chrome_font_scale();
        let sp = move |v: f32| px(v * fs);

        let editor = self.flow_composer.as_ref().expect("ensured").editor.clone();
        if self.flow.wants_focus {
            self.flow.wants_focus = false;
            editor.update(cx, |s, cx| s.focus(window, cx));
        }

        // Center over the tile area (between the sidebar and the ribbon).
        let scale = self.scale();
        let (surface_w, surface_h) = self.renderer.surface_size();
        let (win_w, win_h) = (surface_w as f32 / scale, surface_h as f32 / scale);
        let left_edge = self.sidebar_w();
        let right_edge = win_w - crate::workspace::RIBBON_W;
        let bar_w = (BAR_W * fs).min(right_edge - left_edge - 24.0).max(240.0);
        let left = left_edge + ((right_edge - left_edge) - bar_w) / 2.0;

        let shadow = |blur: f32, y: f32, alpha: f32| {
            vec![BoxShadow {
                color: crate::renderer::color(chrome.shadow, alpha),
                offset: point(px(0.0), px(y)),
                blur_radius: px(blur),
                spread_radius: px(0.0),
                inset: false,
            }]
        };

        let busy = self.flow.busy;
        let open = self.flow.open;

        // ── Pill bar ──
        let send_entity = entity.clone();
        let send = div()
            .id("flow-send")
            .size(sp(28.0))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.primary.opacity(if busy { 0.45 } else { 1.0 }))
            .text_color(theme.primary_foreground)
            .text_size(sp(13.0))
            .cursor_pointer()
            .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                if let Some(e) = send_entity.upgrade() {
                    e.update(app, |this, cx| this.flow_submit_composer(window, cx));
                }
            })
            .child("↑");
        let focus_editor = editor.clone();
        let bar = div()
            .id("flow-bar")
            .occlude()
            .w_full()
            .flex()
            .items_center()
            .gap(sp(10.0))
            .pl(sp(16.0))
            .pr(sp(9.0))
            .py(sp(7.0))
            .rounded(sp(999.0))
            .bg(theme.popover.opacity(0.94))
            .border_1()
            .border_color(theme.border)
            .shadow(shadow(24.0, 6.0, 0.22))
            .cursor_text()
            .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                focus_editor.update(app, |s, cx| s.focus(window, cx));
            })
            .child(div().flex_none().text_size(sp(14.0)).text_color(theme.primary).child("✳"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(sp(13.0))
                    .text_color(theme.foreground)
                    .child(Textarea::new(&editor).appearance(false).bordered(false)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(sp(10.5))
                    .text_color(theme.muted_foreground)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(sp(6.0))
                    .px(sp(6.0))
                    .py(sp(2.0))
                    .child(Action::ToggleFlow.binding().display()),
            )
            .child(send);

        // ── Panel ──
        let panel = open.then(|| {
            let collapse_entity = entity.clone();
            let header = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(sp(8.0))
                .px(sp(16.0))
                .py(sp(12.0))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .size(sp(22.0))
                        .rounded_full()
                        .bg(theme.primary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.primary_foreground)
                        .text_size(sp(12.0))
                        .child("✳"),
                )
                .child(
                    div()
                        .text_size(sp(13.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child("Flow"),
                )
                .child(
                    div()
                        .text_size(sp(11.0))
                        .text_color(theme.muted_foreground)
                        .child("can drive the whole workspace"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("flow-collapse")
                        .px(sp(6.0))
                        .py(sp(2.0))
                        .text_size(sp(12.0))
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = collapse_entity.upgrade() {
                                e.update(app, |this, cx| {
                                    this.toggle_flow();
                                    cx.notify();
                                });
                            }
                        })
                        .child("⌄"),
                );

            let ok_color = if theme.dark { gpui::rgb(0x3fb950) } else { gpui::rgb(0x1a7f37) };
            let mut body = div()
                .id("flow-transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.flow_composer.as_ref().expect("ensured").scroll)
                .flex()
                .flex_col()
                .gap(sp(12.0))
                .px(sp(16.0))
                .py(sp(14.0))
                .text_size(sp(12.5))
                .line_height(sp(18.0));
            if self.flow.messages.is_empty() {
                body = body.child(
                    div()
                        .text_size(sp(12.0))
                        .text_color(theme.muted_foreground)
                        .child("Ask for anything the workspace can do — open a session, run a command, review a PR."),
                );
            }
            for msg in &self.flow.messages {
                body = body.child(match msg {
                    FlowMsg::User(text) => div()
                        .self_end()
                        .max_w(px(bar_w * 0.82))
                        .px(sp(13.0))
                        .py(sp(8.0))
                        .rounded_tl(sp(16.0))
                        .rounded_tr(sp(16.0))
                        .rounded_bl(sp(16.0))
                        .rounded_br(sp(4.0))
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .child(text.clone())
                        .into_any_element(),
                    FlowMsg::Assistant(text) => div()
                        .self_start()
                        .max_w(px(bar_w * 0.88))
                        .px(sp(13.0))
                        .py(sp(8.0))
                        .rounded_tl(sp(16.0))
                        .rounded_tr(sp(16.0))
                        .rounded_br(sp(16.0))
                        .rounded_bl(sp(4.0))
                        .bg(theme.muted)
                        .text_color(theme.foreground)
                        .child(text.clone())
                        .into_any_element(),
                    FlowMsg::Action { title, detail, status, .. } => {
                        let glyph = match status {
                            ActionStatus::Running => div()
                                .size(sp(7.0))
                                .mx(sp(3.0))
                                .rounded_full()
                                .bg(theme.primary)
                                .into_any_element(),
                            ActionStatus::Done => div()
                                .text_size(sp(13.0))
                                .text_color(ok_color)
                                .child("✓")
                                .into_any_element(),
                            ActionStatus::Failed => div()
                                .text_size(sp(13.0))
                                .text_color(theme.destructive)
                                .child("✗")
                                .into_any_element(),
                        };
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(sp(10.0))
                            .px(sp(12.0))
                            .py(sp(9.0))
                            .rounded(sp(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .child(div().flex_none().flex().items_center().justify_center().w(sp(14.0)).child(glyph))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(sp(12.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(sp(11.0))
                                            .text_color(theme.muted_foreground)
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(detail.clone()),
                                    ),
                            )
                            .into_any_element()
                    }
                });
            }
            let last_is_action = matches!(self.flow.messages.last(), Some(FlowMsg::Action { .. }));
            if busy && !last_is_action {
                body = body.child(
                    div().text_size(sp(11.0)).text_color(theme.muted_foreground).child("Working…"),
                );
            }
            if let Some(err) = &self.flow.draft_error {
                body = body.child(
                    div().text_size(sp(11.0)).text_color(theme.destructive).child(err.clone()),
                );
            }

            div()
                .id("flow-panel")
                .occlude()
                .w_full()
                .max_h(px(win_h * PANEL_MAX_FRAC))
                .flex()
                .flex_col()
                .rounded(sp(18.0))
                .overflow_hidden()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border)
                .shadow(shadow(50.0, 18.0, 0.22))
                .child(header)
                .child(body)
        });

        // Newest reply into view once the transcript grew.
        if let Some(c) = &self.flow_composer {
            let n = self.flow.messages.len();
            if c.seen.replace(n) != n {
                c.scroll.scroll_to_bottom();
            }
        }

        div()
            .absolute()
            .left(px(left))
            .bottom(px(BOTTOM_INSET))
            .w(px(bar_w))
            .flex()
            .flex_col()
            .items_center()
            .gap(sp(10.0))
            .when_some(panel, |el, panel| el.child(panel))
            .child(bar)
            .into_any_element()
    }
}
