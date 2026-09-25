//! The sidebar toast stack: the notification surface that replaced the
//! centered `message` overlay.
//!
//! A webview tab is a native child `NSView` (`crate::webview`), so chrome
//! painted in the window cannot be layered over it — which is why the old
//! full-viewport message panel was modal and had to know about every
//! sub-window. Toasts live inside the sidebar region instead
//! ([`App::sidebar_w`]), the one column no webview ever covers, and stack
//! upward from the region's bottom padding.
//!
//! Two kinds:
//!
//! * [`ToastKind::Status`] — direct feedback for something the user just did
//!   (screenshot copied, a worktree starting to provision). It expires on its
//!   own after [`TOAST_TTL`], so it never has to be dismissed, and it takes no
//!   clicks, so it can never swallow input meant for the rows beneath.
//! * [`ToastKind::Notification`] — a result worth keeping (a failure, a saved
//!   workspace, a web page that could not be opened). No timer: it stays until
//!   it is clicked, and it is the only kind that occludes the canvas.
//!
//! Both are capped at [`MAX_TOASTS`]; the oldest `Status` is evicted first, so
//! a burst of action feedback can never push a real notification out.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App as GpuiApp, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::App;
use crate::ui::assets::{ICON_CIRCLE_CHECK, ICON_INFO, ICON_X};
use crate::ui::icon;
use crate::ui::theme::{Theme, alpha};

/// How long a [`ToastKind::Status`] stays up before `App::drain_events`
/// expires it. The old screenshot toast note used exactly this lifetime.
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(3);

/// The most rows the stack holds. Past this the oldest `Status` is evicted
/// first — action feedback never pushes a notification out.
pub(crate) const MAX_TOASTS: usize = 4;

/// Which lifecycle a toast row follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    /// Auto-expiring feedback for an action just taken.
    Status,
    /// A result that stays until dismissed.
    Notification,
}

impl ToastKind {
    /// The name the bus reports in `state_json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ToastKind::Status => "status",
            ToastKind::Notification => "notification",
        }
    }

    pub(crate) fn is_notification(self) -> bool {
        matches!(self, ToastKind::Notification)
    }
}

/// One row of the stack, oldest first in `App::toasts`.
#[derive(Clone, Debug)]
pub(crate) struct ToastNote {
    /// Stable across evictions, so a row's element id never shifts.
    pub(crate) id: u64,
    pub(crate) kind: ToastKind,
    pub(crate) text: String,
    pub(crate) shown: Instant,
}

/// Has a status toast outlived [`TOAST_TTL`] at `now`? Notifications have no
/// deadline, so they are never passed here.
pub(crate) fn status_expired(shown: Instant, now: Instant) -> bool {
    // Saturating: an inverted pair (a `shown` stamped after `now`, which a
    // clock adjustment can produce) reads as "not yet expired" instead of
    // panicking.
    now.saturating_duration_since(shown) >= TOAST_TTL
}

/// Append a row, evicting to stay within [`MAX_TOASTS`]. Returns the new id.
/// Split out from `App` so the eviction policy is unit-testable.
fn push_toast(
    toasts: &mut Vec<ToastNote>,
    next_id: &mut u64,
    kind: ToastKind,
    text: String,
    now: Instant,
) -> u64 {
    let id = *next_id;
    *next_id += 1;
    toasts.push(ToastNote { id, kind, text, shown: now });
    while toasts.len() > MAX_TOASTS {
        // Oldest status first; with the stack all notifications, the oldest
        // row overall (the oldest real notification) gives way.
        let victim = toasts
            .iter()
            .position(|t| !t.kind.is_notification())
            .unwrap_or(0);
        toasts.remove(victim);
    }
    id
}

/// Drop every status past its deadline; returns whether any went.
/// Notifications are kept unconditionally. Pure, so the lifecycle is
/// unit-testable.
fn expire_statuses(toasts: &mut Vec<ToastNote>, now: Instant) -> bool {
    let before = toasts.len();
    toasts.retain(|t| t.kind.is_notification() || !status_expired(t.shown, now));
    toasts.len() != before
}

impl App {
    /// Report an action just taken. Auto-expires after [`TOAST_TTL`].
    pub(crate) fn toast_status(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Status, text.into());
    }

    /// Report a result worth keeping: it stays until it is clicked.
    pub(crate) fn toast_notification(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Notification, text.into());
    }

    fn push_toast(&mut self, kind: ToastKind, text: String) {
        push_toast(
            &mut self.toasts,
            &mut self.next_toast_id,
            kind,
            text,
            Instant::now(),
        );
        self.request_redraw();
    }

    /// Drop one row (a notification's click, or its ×).
    pub(crate) fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
        self.request_redraw();
    }

    /// Drop the status rows still up — a longer job finished, so the note it
    /// started from has nothing left to say. Notifications stay.
    pub(crate) fn clear_status_toasts(&mut self) {
        self.toasts.retain(|t| t.kind.is_notification());
        self.request_redraw();
    }

    /// True when at least one status toast has just expired: clears them
    /// exactly once, then stays false until the next status goes stale.
    pub(crate) fn toast_due(&mut self) -> bool {
        expire_statuses(&mut self.toasts, Instant::now())
    }

    /// The stack, laid out at the bottom of the sidebar region — inside the one
    /// column a webview child view never covers. Oldest row on top, newest
    /// nearest the region's bottom edge, where the eye lands after an action.
    pub(crate) fn toast_layer(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        if self.toasts.is_empty() {
            return div();
        }
        let list = self.list_rect();
        div()
            .absolute()
            .left(px(list.x))
            .bottom(px(list.y))
            .w(px(list.w.max(0.0)))
            .flex()
            .flex_col()
            .gap(px(crate::sidebar_ui::scaled(4.0)))
            .children(self.toasts.iter().map(|t| self.toast_row(theme, t, cx)))
    }

    /// One row: a kind glyph, one ellipsised line, and — for a notification —
    /// a trailing ×. Status rows take no clicks; a notification row (or its ×)
    /// dismisses itself.
    fn toast_row(&self, theme: &Theme, toast: &ToastNote, cx: &mut Context<Self>) -> AnyElement {
        let keep = toast.kind.is_notification();
        let (glyph, ink) = if keep {
            (ICON_INFO, theme.foreground)
        } else {
            (ICON_CIRCLE_CHECK, theme.accent)
        };
        let hover_wash = alpha(theme.muted, 0.35);
        let width = self.list_rect().w.max(0.0);
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::sidebar_ui::scaled(6.0)))
            .w(px(width))
            .rounded(px(crate::sidebar_ui::ROW_RADIUS))
            .border_1()
            .border_color(alpha(theme.foreground, 0.12))
            .bg(theme.popover)
            .px(px(crate::sidebar_ui::scaled(8.0)))
            .py(px(crate::sidebar_ui::scaled(5.0)))
            .child(icon(glyph, px(crate::sidebar_ui::scaled(12.0)), ink))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(crate::sidebar_ui::scaled(11.5)))
                    .text_color(if keep {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .child(toast.text.clone()),
            );
        // A notification row (and its ×) dismisses itself; a status row takes
        // no clicks at all, so it can never swallow one meant for the rows
        // beneath it. Both branches are the same `Div`, so the row's type does
        // not depend on its kind.
        let row: AnyElement = if keep {
            let entity = cx.entity().downgrade();
            let id = toast.id;
            row.id(ElementId::Integer(id))
                .occlude()
                .cursor_pointer()
                .hover(move |s| s.bg(hover_wash))
                .on_click(move |_ev, _win, app: &mut GpuiApp| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.dismiss_toast(id);
                            this.request_redraw();
                            cx.notify();
                        });
                    }
                })
                .child(icon(
                    ICON_X,
                    px(crate::sidebar_ui::scaled(11.0)),
                    theme.muted_foreground,
                ))
                .into_any_element()
        } else {
            row.into_any_element()
        };
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u64, kind: ToastKind, shown: Instant) -> ToastNote {
        ToastNote { id, kind, text: format!("note {id}"), shown }
    }

    /// A status row goes on its own timer; a notification never does — that is
    /// the whole difference between the two kinds.
    #[test]
    fn status_expires_and_a_notification_does_not() {
        let now = Instant::now();
        let mut toasts = vec![
            note(0, ToastKind::Status, now),
            note(1, ToastKind::Notification, now - TOAST_TTL - Duration::from_secs(60)),
        ];
        // Fresh status, stale notification: both stay up.
        assert!(!expire_statuses(&mut toasts, now), "nothing is due yet");
        assert_eq!(toasts.len(), 2);
        // At its deadline the status goes; the notification is untouched.
        let later = now + TOAST_TTL;
        assert!(expire_statuses(&mut toasts, later), "the status is due");
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].kind, ToastKind::Notification);
        // Idempotent: nothing left to expire, so no redraw is requested twice.
        assert!(!expire_statuses(&mut toasts, later), "nothing left to expire");
    }

    /// The cap evicts action feedback before a real notification, and every row
    /// keeps a distinct id so a surviving row's element id never shifts.
    #[test]
    fn the_stack_evicts_the_oldest_status_before_a_notification() {
        let now = Instant::now();
        let mut toasts = Vec::new();
        let mut next = 0;
        push_toast(&mut toasts, &mut next, ToastKind::Notification, "keep me".into(), now);
        for i in 0..MAX_TOASTS {
            push_toast(&mut toasts, &mut next, ToastKind::Status, format!("status {i}"), now);
        }
        assert_eq!(toasts.len(), MAX_TOASTS);
        assert!(
            toasts.iter().any(|t| t.kind.is_notification()),
            "the notification survived the flood"
        );
        // The first status pushed is the one that had to go; the notification
        // (pushed first of all) outlived it.
        assert!(
            !toasts.iter().any(|t| t.text == "status 0"),
            "the oldest status went first"
        );
        assert!(toasts.iter().any(|t| t.text == "status 1"), "the rest stayed");
        assert_eq!(next, MAX_TOASTS as u64 + 1, "every row took a fresh id");
        let ids: Vec<u64> = toasts.iter().map(|t| t.id).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "row ids stay distinct");
    }
}
