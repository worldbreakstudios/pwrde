//! Flow agent surface: the bottom-centered pill bar plus the multi-chat
//! surfaces above it — an iMessage-style chat list and one open conversation
//! — as a gpui element tree mounted on every page (same overlay pattern as
//! `local_diff_ui`; Flow follows the user around the app). This module owns
//! only presentation and the app-side glue (`toggle_flow`, `flow_send`,
//! composer lifecycle); the chats it renders are `flow::FlowState`, reduced
//! from backend events on the main thread, and each conversation talks to
//! its own agent behind `flow::AgentBackend` (`App::flow_backends`, keyed by
//! chat id) — nothing here knows which agent is on the other end.
//!
//! The two views mirror the GANTRY mock: `FlowView::List` shows every chat
//! as a card (title, age, last-message preview, busy/unseen badges) and
//! `FlowView::Chat` shows the active transcript as a sheet that docks onto
//! the pill bar (the bar squares its top corners while the sheet is up).
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

use crate::flow::{ActionStatus, FlowMsg, FlowView};
use crate::pages::Action;
use crate::ui::theme::Theme;
use crate::App;

/// Pill bar / chat panel width at the default app font size (shrinks on
/// narrow windows, grows with `appearance.font_size`).
const BAR_W: f32 = 480.0;
/// Chat-list width — wider than the bar, as in the mock.
const LIST_W: f32 = 560.0;
/// Pill bar height at the default app font size.
const BAR_H: f32 = 46.0;
/// Breathing room between the reserved safe area and the tile above it.
const SAFE_GAP: f32 = 8.0;
/// Gap between the bar and the window's bottom edge.
const BOTTOM_INSET: f32 = 14.0;
/// The open chat sheet never exceeds this fraction of the window height.
const PANEL_MAX_FRAC: f32 = 0.52;
/// The chat list never exceeds this fraction of the window height.
const LIST_MAX_FRAC: f32 = 0.38;
/// Starter prompts shown in a brand-new conversation.
const CHIPS: [&str; 3] = ["Spawn a session", "Review the open PR", "Archive merged sessions"];

/// Logical height of the bottom safe area the Sessions/tool-page layouts
/// reserve while the flag is on (`App::flow_inset`): inset + pill bar + gap,
/// tracking the app font scale so a larger accessibility font still clears
/// the bar.
pub fn safe_area_h() -> f32 {
    BOTTOM_INSET + BAR_H * crate::renderer::chrome_font_scale() + SAFE_GAP
}

/// The pill bar's composer: a `gpui_component` textarea plus the transcript
/// and chat-list scroll handles, created lazily on the first render after
/// the flag is on. The composer is shared across chats (drafts don't follow
/// a chat switch).
pub struct FlowComposer {
    pub editor: Entity<TextareaState>,
    scroll: ScrollHandle,
    list_scroll: ScrollHandle,
    /// (active chat id, its message count) at the last render; a change
    /// scrolls the transcript to the bottom so the newest reply is visible —
    /// switching chats counts as a change.
    seen: Cell<(u64, usize)>,
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
    /// no-op rather than a silent success. Opening for the first time starts
    /// a fresh conversation; after that it reopens whatever was on screen.
    pub(crate) fn toggle_flow(&mut self) -> bool {
        if !crate::flow::enabled() {
            return false;
        }
        self.flow.open = !self.flow.open;
        if self.flow.open {
            if self.flow.chats.is_empty() {
                self.flow.new_chat(crate::flow::now_epoch());
            }
            self.flow.wants_focus = true;
        }
        self.request_redraw();
        true
    }

    /// Spawn chat `id`'s agent on first use (never while the flag is off).
    /// On failure the reason lands in the active chat's `draft_error` for
    /// the panel to show — callers only ever ensure the active chat.
    fn ensure_flow_backend(&mut self, id: u64) -> bool {
        if !crate::flow::enabled() {
            return false;
        }
        if self.flow_backends.contains_key(&id) {
            return true;
        }
        let cwd = self
            .active_repo_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match crate::flow::spawn_flow_backend(&cwd, self.events_tx.clone(), id) {
            Some(backend) => {
                self.flow_backends.insert(id, backend);
                true
            }
            None => {
                if let Some(chat) = self.flow.active_chat_mut() {
                    chat.draft_error =
                        Some("Could not start the agent — is `claude` on your PATH?".into());
                }
                false
            }
        }
    }

    /// Hand one user request to the active chat's agent (composer ↩,
    /// `pwrde-cli flow-send`), creating a conversation if none exists. Only
    /// the receiving chat's busy state gates the send — other chats keep
    /// accepting while one works. A dead backend (write fails) is replaced
    /// once and the send retried.
    pub(crate) fn flow_send(&mut self, text: String) -> Result<(), String> {
        if !crate::flow::enabled() {
            return Err("Flow is disabled — enable it under Settings > Feature Flags".into());
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("nothing to send".into());
        }
        let now = crate::flow::now_epoch();
        let id = match self.flow.active_chat() {
            Some(chat) => chat.id,
            None => self.flow.new_chat(now),
        };
        if self.flow.active_chat().is_some_and(|c| c.busy) {
            return Err("this chat is still working — start a new one for anything else".into());
        }
        for attempt in 0..2 {
            if !self.ensure_flow_backend(id) {
                let err = self.flow.active_chat().and_then(|c| c.draft_error.clone());
                return Err(err.unwrap_or_default());
            }
            let backend = self.flow_backends.get_mut(&id).expect("ensured");
            match backend.send(&text) {
                Ok(()) => {
                    self.flow.push_user(id, text, now);
                    // Surface the panel, but leave the view alone: a bus
                    // `flow-send` must not yank the user out of the chat
                    // list — the composer path switches views itself.
                    self.flow.open = true;
                    self.request_redraw();
                    return Ok(());
                }
                Err(e) => {
                    let mut dead = self.flow_backends.remove(&id).expect("ensured");
                    dead.shutdown();
                    if attempt == 1 {
                        let msg = format!("agent write failed: {e}");
                        if let Some(chat) = self.flow.active_chat_mut() {
                            chat.draft_error = Some(msg.clone());
                        }
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
            list_scroll: ScrollHandle::new(),
            seen: Cell::new((0, 0)),
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
            Ok(()) => {
                // The user typed this — bring its conversation on screen.
                self.flow.view = FlowView::Chat;
                editor.update(cx, |s, cx| s.set_value("", window, cx));
            }
            Err(e) => {
                if let Some(chat) = self.flow.active_chat_mut() {
                    chat.draft_error = Some(e);
                }
            }
        }
        cx.notify();
    }

    /// The pill bar (always, while the flag is on, on every page) plus,
    /// while `flow.open`, either the chat list or the active conversation
    /// docked onto it.
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
        let area_w = right_edge - left_edge;
        let bar_w = (BAR_W * fs).min(area_w - 24.0).max(240.0);
        let list_w = (LIST_W * fs).min(area_w - 24.0).max(240.0);
        let open = self.flow.open;
        let in_list = open && self.flow.view == FlowView::List;
        let in_chat = open && self.flow.view == FlowView::Chat;
        // The column is as wide as its widest child so the list can outgrow
        // the bar while both stay centered on the tile area.
        let root_w = if in_list { bar_w.max(list_w) } else { bar_w };
        let left = left_edge + (area_w - root_w) / 2.0;

        let shadow = |blur: f32, y: f32, alpha: f32| {
            vec![BoxShadow {
                color: crate::renderer::color(chrome.shadow, alpha),
                offset: point(px(0.0), px(y)),
                blur_radius: px(blur),
                spread_radius: px(0.0),
                inset: false,
            }]
        };
        let ok_color = if theme.dark { gpui::rgb(0x3fb950) } else { gpui::rgb(0x1a7f37) };

        let busy = self.flow.active_chat().is_some_and(|c| c.busy);

        // ── Pill bar (docks under the chat sheet: square top, round bottom) ──
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
            .w(px(bar_w))
            .flex_none()
            .flex()
            .items_center()
            .gap(sp(10.0))
            .pl(sp(16.0))
            .pr(sp(9.0))
            .py(sp(7.0))
            .map(|el| {
                if in_chat {
                    el.rounded_b(sp(18.0)).border_color(theme.border.opacity(0.5))
                } else {
                    el.rounded(sp(999.0)).border_color(theme.border)
                }
            })
            .bg(theme.popover.opacity(0.94))
            .border_1()
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

        // ── Chat list ──
        let list = in_list.then(|| {
            let now = crate::flow::now_epoch();
            let mut order: Vec<usize> = (0..self.flow.chats.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(self.flow.chats[i].last_activity));
            let mut col = div()
                .id("flow-chat-list")
                .w(px(list_w))
                .max_h(px(win_h * LIST_MAX_FRAC))
                .overflow_y_scroll()
                .track_scroll(&self.flow_composer.as_ref().expect("ensured").list_scroll)
                .flex()
                .flex_col()
                .gap(sp(8.0))
                // The scroll container clips to its bounds, so the padding
                // has to be wider than the card shadows (blur 28, y 8) or
                // they end in a hard edge at the top and bottom.
                .px(sp(40.0))
                .py(sp(26.0));
            for i in order {
                let chat = &self.flow.chats[i];
                let id = chat.id;
                // Idle chats recede; the working and unread ones read at
                // full strength (the mock's 65% cards).
                let idle = !chat.busy && !chat.unseen;
                let dim = |c: gpui::Hsla| if idle { c.opacity(0.65) } else { c };
                let avatar = if chat.busy {
                    div().bg(theme.primary).text_color(theme.primary_foreground).child("✳")
                } else if chat.unseen {
                    div().bg(theme.muted).text_color(ok_color).child("✓")
                } else {
                    div().bg(dim(theme.muted)).text_color(dim(theme.muted_foreground)).child("✳")
                };
                let preview = match chat.messages.last() {
                    Some(FlowMsg::User(t)) | Some(FlowMsg::Assistant(t)) => t.clone(),
                    Some(FlowMsg::Action { title, .. }) => title.clone(),
                    None => "New conversation".into(),
                };
                let open_entity = entity.clone();
                col = col.child(
                    div()
                        .id(("flow-chat", id as usize))
                        .flex()
                        .items_center()
                        .gap(sp(11.0))
                        .px(sp(14.0))
                        .py(sp(10.0))
                        .rounded(sp(15.0))
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .shadow(shadow(28.0, 8.0, 0.16))
                        .cursor_pointer()
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = open_entity.upgrade() {
                                e.update(app, |this, cx| {
                                    this.flow.open_chat(id);
                                    this.flow.wants_focus = true;
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            avatar
                                .size(sp(32.0))
                                .flex_none()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(sp(14.0)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .items_baseline()
                                        .gap(sp(8.0))
                                        .child(
                                            div()
                                                .text_size(sp(12.5))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(dim(theme.foreground))
                                                .whitespace_nowrap()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(chat.title.clone().unwrap_or_else(|| "New chat".into())),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_size(sp(10.0))
                                                .text_color(dim(theme.muted_foreground))
                                                .child(crate::flow::format_age(now, chat.last_activity)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(sp(11.0))
                                        .text_color(dim(theme.muted_foreground))
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(preview),
                                ),
                        )
                        .when(chat.busy, |el| {
                            el.child(div().size(sp(6.0)).flex_none().rounded_full().bg(theme.primary))
                        }),
                );
            }
            col
        });

        // ── Open conversation (docked sheet) ──
        let panel = in_chat.then(|| {
            let title = self
                .flow
                .active_chat()
                .and_then(|c| c.title.clone())
                .unwrap_or_else(|| "New chat".into());
            let list_entity = entity.clone();
            let new_entity = entity.clone();
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
                        .text_size(sp(13.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(title),
                )
                .child(
                    div()
                        .id("flow-chats-link")
                        .flex_none()
                        .text_size(sp(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = list_entity.upgrade() {
                                e.update(app, |this, cx| {
                                    this.flow.view = FlowView::List;
                                    cx.notify();
                                });
                            }
                        })
                        .child("Chats ›"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("flow-new-chat")
                        .size(sp(24.0))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(sp(14.0))
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                            if let Some(e) = new_entity.upgrade() {
                                e.update(app, |this, cx| {
                                    this.flow.new_chat(crate::flow::now_epoch());
                                    this.flow.wants_focus = true;
                                    cx.notify();
                                });
                            }
                        })
                        .child("＋"),
                )
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

            let chat = self.flow.active_chat();
            let messages: &[FlowMsg] = chat.map(|c| c.messages.as_slice()).unwrap_or(&[]);
            let draft_error = chat.and_then(|c| c.draft_error.clone());
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
            if messages.is_empty() {
                // Brand-new conversation: the mock's centered intro + chips.
                let mut chips = div().flex().flex_wrap().justify_center().gap(sp(6.0)).mt(sp(4.0));
                for (i, chip) in CHIPS.iter().enumerate() {
                    let chip_entity = entity.clone();
                    chips = chips.child(
                        div()
                            .id(("flow-chip", i))
                            .px(sp(11.0))
                            .py(sp(4.0))
                            .rounded(sp(999.0))
                            .border_1()
                            .border_color(theme.border)
                            .text_size(sp(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.primary)
                            .cursor_pointer()
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                if let Some(e) = chip_entity.upgrade() {
                                    e.update(app, |this, cx| {
                                        let _ = this.flow_send(chip.to_string());
                                        cx.notify();
                                    });
                                }
                            })
                            .child(*chip),
                    );
                }
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(sp(10.0))
                        .pt(sp(26.0))
                        .pb(sp(14.0))
                        .child(
                            div()
                                .text_size(sp(13.0))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme.foreground)
                                .child("New conversation"),
                        )
                        .child(
                            div()
                                .text_size(sp(11.5))
                                .text_color(theme.muted_foreground)
                                .child("Flow can drive anything in the workspace"),
                        )
                        .child(chips),
                );
            }
            for msg in messages {
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
            let last_is_action = matches!(messages.last(), Some(FlowMsg::Action { .. }));
            if busy && !last_is_action {
                body = body.child(
                    div().text_size(sp(11.0)).text_color(theme.muted_foreground).child("Working…"),
                );
            }
            if let Some(err) = draft_error {
                body = body.child(
                    div().text_size(sp(11.0)).text_color(theme.destructive).child(err),
                );
            }

            div()
                .id("flow-panel")
                .occlude()
                .w(px(bar_w))
                .max_h(px(win_h * PANEL_MAX_FRAC))
                .flex()
                .flex_col()
                .rounded_t(sp(18.0))
                .overflow_hidden()
                .bg(theme.popover)
                .border_1()
                .border_b_0()
                .border_color(theme.border)
                .shadow(shadow(50.0, 18.0, 0.22))
                .child(header)
                .child(body)
        });

        // Newest reply into view once the transcript grew or changed chats.
        if let Some(c) = &self.flow_composer {
            let key = self
                .flow
                .active_chat()
                .map(|chat| (chat.id, chat.messages.len()))
                .unwrap_or((0, 0));
            if c.seen.replace(key) != key {
                c.scroll.scroll_to_bottom();
            }
        }

        div()
            .absolute()
            .left(px(left))
            .bottom(px(BOTTOM_INSET))
            .w(px(root_w))
            .flex()
            .flex_col()
            .items_center()
            // The sheet docks onto the bar; the detached list floats above it.
            .gap(if in_chat { px(0.0) } else { sp(10.0) })
            .when_some(list, |el, list| el.child(list))
            .when_some(panel, |el, panel| el.child(panel))
            .child(bar)
            .into_any_element()
    }
}
