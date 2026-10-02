//! The webview Site / Tools popovers' own window.
//!
//! A webview tab is a native child `NSView` painted over the app's surface,
//! so a popover drawn in the main window would sit *under* the page it is
//! meant to float over. Like the command palette (`palette_window`), the
//! popover is therefore a real, separate window: chrome-less, transparent,
//! exactly the card's size, opened under the control it hangs from.
//!
//! The popover *state* stays on [`App`] (`webview_panel` is the source of
//! truth; `webview_find` is the find field); this view only renders
//! `App::webview_popover_card` and routes its keys to
//! `App::webview_popover_key`. The frame pump calls [`reconcile`] every tick:
//! it opens the window when a panel is set, moves it when the anchor moves
//! (a resize, the sidebar, a split), reopens it when the panel changes kind
//! or tab, and takes it down when `webview_panel` clears.
//!
//! Dismissal. Besides ⎋, the popover's own toggle and its action rows, the
//! popover closes when:
//! - its window loses key status — a press anywhere else in the main window
//!   (the page included), another window, or another app. The window only
//!   starts watching once it exists, so a popover opened while pwrde is in
//!   the background (over the bus) stays up until pwrde is next used;
//! - its anchor goes away (`App::webview_popover_anchor` is `None`: the tab,
//!   tile, group or page changed, or a modal / the flyover covers the chrome);
//! - the main window moves, changes display, or is gone — so no popover can
//!   outlive it.

use gpui::{
    div, point, px, size, App as GpuiApp, AppContext, Bounds, Context, DisplayId, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render, Size,
    Styled, Window, WindowBounds, WindowOptions,
};

use crate::webview_ui::PanelKind;
use crate::workspace::LayoutRect;
use crate::App;

/// The window's title: never drawn, it is how the bus tells this window from
/// the main one (`bus_exec::main_ns_window`), since a key popover is also
/// AppKit's "main window" while it is up.
pub(crate) const WINDOW_TITLE: &str = "pwrde webview popover";
/// The gap between a control and the card under it (the mock's `top:40px`
/// under a 32px control).
const GAP: f32 = 8.0;
/// How close to a display edge the card may be pushed.
const EDGE: f32 = 8.0;

/// The popover window's view: one card, the size of the window it owns.
pub(crate) struct WebviewPopoverWindow {
    app: gpui::Entity<App>,
    focus_handle: FocusHandle,
}

/// Where the open popover window was put, and for what: the pump compares
/// this against the placement it wants each tick.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    kind: PanelKind,
    id: u64,
    /// The main window's origin on its display when the popover opened; the
    /// popover does not follow a moved window, it closes.
    main_origin: (f32, f32),
    display: Option<DisplayId>,
    /// The popover window's bounds, in its display's coordinate space.
    bounds: Bounds<Pixels>,
    /// AppKit caches a window's shadow from its first frame; a transparent
    /// window's must be recomputed once the card has actually painted.
    shadow_stale: bool,
}

impl Render for WebviewPopoverWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.app.update(cx, |app, cx| app.webview_popover_card(window, cx));
        // The card focuses the find field when it is claimed (⌘F, or a press
        // on it); otherwise the root holds focus so ⎋ and the ⌘ chords land.
        let find_focused = self
            .app
            .read(cx)
            .webview_find
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !find_focused && !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.app.update(cx, |app, cx| {
                    app.webview_popover_key(ev, window, cx);
                    cx.notify();
                });
                cx.notify();
            }))
            .child(card)
    }
}

impl Focusable for WebviewPopoverWindow {
    fn focus_handle(&self, _cx: &GpuiApp) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl App {
    /// Where the open popover belongs right now, or `None` when it has no
    /// anchor (see `webview_popover_anchor`). Called on the main window.
    fn webview_popover_placement(&self, window: &Window, cx: &GpuiApp) -> Option<Placed> {
        let (kind, id, anchor) = self.webview_popover_anchor()?;
        let main = window.bounds();
        // The main window's content is full-size under a transparent
        // titlebar; any titlebar it did have would push the content down.
        let content_top =
            (f32::from(main.size.height) - f32::from(window.viewport_size().height)).max(0.0);
        let display = window.display(cx);
        let (w, h) = crate::webview_ui::card_size(kind);
        let bounds = popover_bounds(
            main,
            content_top,
            kind,
            anchor,
            size(px(w), px(h)),
            display.as_ref().map(|display| display.bounds().size),
        );
        Some(Placed {
            kind,
            id,
            main_origin: (f32::from(main.origin.x), f32::from(main.origin.y)),
            display: display.map(|display| display.id()),
            bounds,
            shadow_stale: true,
        })
    }
}

/// Where the popover window sits, in the main window's display's coordinate
/// space: `GAP` under `anchor` (the control's rect in the main window's
/// logical content coordinates) — a Site card centred on it, a Tools card
/// with its right edge on the control's — then pushed back inside the
/// display, `EDGE` clear of its sides, when one is known, and snapped to
/// whole points.
///
/// Pure so the placement is unit-tested without a window server.
/// `main` is the main window's bounds on that display and `content_top` how
/// far its content starts below its top edge.
pub(crate) fn popover_bounds(
    main: Bounds<Pixels>,
    content_top: f32,
    kind: PanelKind,
    anchor: LayoutRect,
    card: Size<Pixels>,
    display: Option<Size<Pixels>>,
) -> Bounds<Pixels> {
    let (card_w, card_h) = (f32::from(card.width), f32::from(card.height));
    let left = match kind {
        PanelKind::Site => anchor.x + (anchor.w - card_w) / 2.0,
        PanelKind::Tools => anchor.x + anchor.w - card_w,
    };
    let mut x = f32::from(main.origin.x) + left;
    let mut y = f32::from(main.origin.y) + content_top + anchor.y + anchor.h + GAP;
    if let Some(display) = display {
        // `max` last: a card larger than the display keeps its top-left on it.
        x = x.min(f32::from(display.width) - EDGE - card_w).max(EDGE);
        y = y.min(f32::from(display.height) - EDGE - card_h).max(EDGE);
    }
    // Whole points: a centred card can land on a half point, which a 1x
    // display would smear across two pixels.
    Bounds { origin: point(px(x.round()), px(y.round())), size: card }
}

/// Reconcile the popover window with `App::webview_panel`. Called by the
/// frame pump every tick, outside any entity update, so window lifecycle
/// stays on the foreground executor like the palette's and Settings'.
pub(crate) fn reconcile(app: &gpui::Entity<App>, cx: &mut GpuiApp, redraw: bool) {
    let (wanted, handle, placed, main) = {
        let app = app.read(cx);
        (
            app.webview_panel.is_some(),
            app.webview_popover_window,
            app.webview_popover_placed.clone(),
            app.main_window.and_then(|window| window.downcast::<App>()),
        )
    };
    if !wanted && handle.is_none() {
        return;
    }
    // A missing main window yields no placement, which closes the popover.
    let want = wanted
        .then(|| {
            main.and_then(|main| {
                main.update(cx, |app, window, cx| app.webview_popover_placement(window, cx))
                    .ok()
                    .flatten()
            })
        })
        .flatten();
    let close = |cx: &mut GpuiApp, dismiss: bool| {
        // Forget the handle first: the window's own key-status observer must
        // not read this removal as the user clicking away.
        app.update(cx, |app, cx| {
            app.webview_popover_window = None;
            app.webview_popover_placed = None;
            if dismiss && app.webview_panel.is_some() {
                app.close_webview_panel();
                cx.notify();
            }
        });
        if let Some(window) = handle {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    };
    match (want, handle, placed) {
        // Nothing to hang from any more (or the panel was closed).
        (None, _, _) => close(cx, true),
        (Some(want), None, _) => open(app, want, cx),
        (Some(want), Some(window), Some(placed)) => {
            let same_panel = (want.kind, want.id) == (placed.kind, placed.id);
            if want.main_origin != placed.main_origin || want.display != placed.display {
                // The main window moved under it.
                close(cx, true);
            } else if !same_panel {
                // Another card: a fresh window of its size opens next tick.
                close(cx, false);
            } else {
                let moved = want.bounds != placed.bounds;
                let live = window
                    .update(cx, |_, window, cx| {
                        if moved {
                            move_window(window, want.bounds.origin);
                        }
                        if placed.shadow_stale {
                            invalidate_shadow(window);
                        }
                        if redraw || moved {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !live {
                    close(cx, true);
                } else if moved || placed.shadow_stale {
                    app.update(cx, |app, _| {
                        app.webview_popover_placed = Some(Placed { shadow_stale: false, ..want });
                    });
                }
            }
        },
        // A window without a record cannot be reconciled; start over.
        (Some(_), Some(_), None) => close(cx, false),
    }
}

/// Open the popover window at `placed` and store its handle on the [`App`].
fn open(app: &gpui::Entity<App>, placed: Placed, cx: &mut GpuiApp) {
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
            display_id: placed.display,
            window_bounds: Some(WindowBounds::Windowed(placed.bounds)),
            // Chrome-less the palette's way: a titled window (so it can be
            // key and its find field typeable) whose titlebar is transparent
            // and whose buttons are hidden right after open.
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(WINDOW_TITLE.into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            // Shown by `activate_window` below, once the chrome is stripped.
            show: false,
            // The rounded card is the window's whole silhouette.
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        move |window, cx| {
            window.on_window_should_close(cx, move |_, cx| {
                app_for_close.update(cx, |app, _| {
                    app.webview_popover_window = None;
                    app.webview_popover_placed = None;
                    app.close_webview_panel();
                });
                true
            });
            cx.new(|cx| {
                cx.observe(&app_for_view, |_, _, cx| cx.notify()).detach();
                // Losing key status dismisses the popover — but only while
                // this window is still the app's popover: the pump clears the
                // handle before it removes a window itself. And only while it
                // still shows the current panel: a press on the *other* toggle
                // sets the new panel before this resign-key arrives, and that
                // one must survive for the pump to swap the window.
                cx.observe_window_activation(window, |view: &mut WebviewPopoverWindow, window, cx| {
                    if window.is_window_active() {
                        return;
                    }
                    let this_window = window.window_handle();
                    view.app.update(cx, |app, cx| {
                        if app.webview_popover_window.map(gpui::AnyWindowHandle::from)
                            == Some(this_window)
                            && app.webview_popover_placed.as_ref().zip(app.webview_panel.as_ref()).is_some_and(
                                |(placed, panel)| (placed.kind, placed.id) == (panel.kind(), panel.id()),
                            )
                        {
                            app.dismiss_webview_panel_on_blur();
                            cx.notify();
                        }
                    });
                })
                .detach();
                WebviewPopoverWindow { app: app_for_view.clone(), focus_handle: cx.focus_handle() }
            })
        },
    );
    match handle {
        Ok(window) => {
            let _ = window.update(cx, |_view, window, _cx| {
                strip_window_chrome(window);
                window.activate_window();
            });
            app.update(cx, |app, _| {
                app.webview_popover_window = Some(window);
                app.webview_popover_placed = Some(placed);
            });
        },
        // No window: drop the panel rather than have the pump retry the
        // open every tick.
        Err(_) => app.update(cx, |app, _| app.close_webview_panel()),
    }
}

/// AppKit geometry declared locally: the crate has no CoreGraphics structs
/// and `objc` needs a type encoding to send these selectors.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

unsafe impl objc::Encode for CGPoint {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
    }
}

unsafe impl objc::Encode for CGSize {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGSize=dd}") }
    }
}

unsafe impl objc::Encode for CGRect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

/// Move the window's top-left corner to `origin`, a point in its display's
/// top-left coordinate space (what [`popover_bounds`] returns), converted to
/// AppKit's bottom-left screen space through the window's own screen so it
/// holds on any monitor.
fn move_window(window: &Window, origin: gpui::Point<Pixels>) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};

    let Some(ns_window) = crate::ns_window(window) else {
        return;
    };
    unsafe {
        let screen: *mut Object = msg_send![ns_window, screen];
        if screen.is_null() {
            return;
        }
        let screen_frame: CGRect = msg_send![screen, frame];
        let top_left = CGPoint {
            x: screen_frame.origin.x + f32::from(origin.x) as f64,
            y: screen_frame.origin.y + screen_frame.size.height - f32::from(origin.y) as f64,
        };
        let _: () = msg_send![ns_window, setFrameTopLeftPoint: top_left];
    }
}

/// Recompute the window shadow from what is painted now (the rounded card).
fn invalidate_shadow(window: &Window) {
    use objc::{msg_send, sel, sel_impl};

    if let Some(ns_window) = crate::ns_window(window) {
        unsafe {
            let _: () = msg_send![ns_window, invalidateShadow];
        }
    }
}

/// Remove the macOS window chrome the popover never asked for — the traffic
/// lights and the title, in the titlebar and the Window menu alike — and let
/// it join a full-screen main window's space instead of opening on the
/// desktop behind it.
fn strip_window_chrome(window: &Window) {
    use objc::runtime::{Object, YES};
    use objc::{msg_send, sel, sel_impl};

    let Some(ns_window) = crate::ns_window(window) else {
        return;
    };
    unsafe {
        // NSWindowCloseButton = 0, NSWindowMiniaturizeButton = 1,
        // NSWindowZoomButton = 2.
        for which in [0isize, 1, 2] {
            let button: *mut Object = msg_send![ns_window, standardWindowButton: which];
            if !button.is_null() {
                let _: () = msg_send![button, setHidden: YES];
            }
        }
        // NSWindowTitleHidden = 1.
        let _: () = msg_send![ns_window, setTitleVisibility: 1isize];
        let _: () = msg_send![ns_window, setTitlebarAppearsTransparent: YES];
        let _: () = msg_send![ns_window, setExcludedFromWindowsMenu: YES];
        // NSWindowCollectionBehaviorTransient (1 << 3, out of Mission Control
        // and the window cycle) | FullScreenAuxiliary (1 << 8).
        let _: () = msg_send![ns_window, setCollectionBehavior: ((1usize << 3) | (1usize << 8))];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_window() -> Bounds<Pixels> {
        Bounds { origin: point(px(200.0), px(100.0)), size: size(px(1200.0), px(800.0)) }
    }

    fn display() -> Option<Size<Pixels>> {
        Some(size(px(1728.0), px(1117.0)))
    }

    fn origin(bounds: Bounds<Pixels>) -> (f32, f32) {
        (f32::from(bounds.origin.x), f32::from(bounds.origin.y))
    }

    #[test]
    fn site_card_centres_under_the_address_pill() {
        let pill = LayoutRect { x: 421.0, y: 58.0, w: 420.0, h: 32.0 };
        let card = size(px(300.0), px(212.0));
        let bounds = popover_bounds(main_window(), 0.0, PanelKind::Site, pill, card, display());
        // Pill centre 631 → card left 481, on a window at x 200; 8px under
        // the pill's bottom edge (58 + 32), on a window at y 100.
        assert_eq!(origin(bounds), (681.0, 198.0));
        assert_eq!(bounds.size, card);
    }

    #[test]
    fn tools_card_right_aligns_under_the_more_button() {
        let more = LayoutRect { x: 1058.0, y: 58.0, w: 32.0, h: 32.0 };
        let card = size(px(272.0), px(315.0));
        let bounds = popover_bounds(main_window(), 0.0, PanelKind::Tools, more, card, display());
        // Right edges meet: 200 + 1090 − 272.
        assert_eq!(origin(bounds), (1018.0, 198.0));
    }

    #[test]
    fn a_half_point_centre_snaps_to_a_whole_point() {
        let pill = LayoutRect { x: 421.0, y: 58.5, w: 419.0, h: 32.0 };
        let card = size(px(300.0), px(212.0));
        let bounds = popover_bounds(main_window(), 0.0, PanelKind::Site, pill, card, display());
        // Unrounded: (680.5, 198.5).
        assert_eq!(origin(bounds), (681.0, 199.0));
    }

    #[test]
    fn a_titlebar_inset_pushes_the_card_down() {
        let more = LayoutRect { x: 1058.0, y: 58.0, w: 32.0, h: 32.0 };
        let card = size(px(272.0), px(315.0));
        let bounds = popover_bounds(main_window(), 28.0, PanelKind::Tools, more, card, display());
        assert_eq!(origin(bounds).1, 226.0);
    }

    #[test]
    fn the_card_is_clamped_inside_the_display() {
        let card = size(px(300.0), px(212.0));
        // A pill near the window's left edge, with the window hanging off the
        // display's left: the card stops `EDGE` in from the display's side.
        let off_left = Bounds { origin: point(px(-300.0), px(100.0)), ..main_window() };
        let pill = LayoutRect { x: 212.0, y: 58.0, w: 138.0, h: 32.0 };
        let bounds = popover_bounds(off_left, 0.0, PanelKind::Site, pill, card, display());
        assert_eq!(origin(bounds).0, EDGE);
        // A window low and far right on the display: pulled back in and up.
        let low = Bounds { origin: point(px(900.0), px(900.0)), ..main_window() };
        let more = LayoutRect { x: 1158.0, y: 58.0, w: 32.0, h: 32.0 };
        let bounds = popover_bounds(low, 0.0, PanelKind::Site, more, card, display());
        assert_eq!(origin(bounds), (1728.0 - EDGE - 300.0, 1117.0 - EDGE - 212.0));
    }

    #[test]
    fn without_a_display_the_card_is_not_clamped() {
        let card = size(px(300.0), px(212.0));
        let off_left = Bounds { origin: point(px(-300.0), px(100.0)), ..main_window() };
        let pill = LayoutRect { x: 212.0, y: 58.0, w: 138.0, h: 32.0 };
        let bounds = popover_bounds(off_left, 0.0, PanelKind::Site, pill, card, None);
        // Pill centre 281 → card left 131, on a window at x −300.
        assert_eq!(origin(bounds), (-169.0, 198.0));
    }
}
