//! The command palette's own window.
//!
//! A webview tab is a native child `NSView` painted over the app's surface,
//! so an in-window palette could only ever be drawn by hiding the webviews
//! underneath it. The palette is instead a real, separate window: hidden
//! until cmd-P (or the system-wide opt-cmd-P) asks for it, then brought to the
//! front -- the Spotlight/Raycast flow, with nothing painted over a webview.
//!
//! The window *is* the card: it opens `PANEL_W` wide, its titlebar strip and
//! traffic lights are removed natively, and it shrinks to the panel's measured
//! height, so the rounded glass card has no transparent margin around it.
//!
//! The palette *state* stays on [`App`] (`command`, `command_scroll`,
//! `command_scroll_to`, `modal_search`); this view only renders it in its own
//! window and routes the keys the model owns. The panel itself is
//! `App::command_panel_card`.
//!
//! Losing key status (a click on the main window or another app) hides the
//! palette instead of leaving it buried behind other windows: the view marks
//! it hidden on [`App`] and the pump removes the window, but the model stays,
//! so the hotkey reopens the window where the user left off (query, selection,
//! token chips). Hidden for `command::HIDDEN_RESET` (2 minutes), it is closed
//! for good and the next hotkey opens a fresh root palette.
//!
//! The window contains nothing but that one field, so this view keeps the
//! shared `Input` focused for as long as the window is up: opening the palette
//! is immediately typeable, with no click needed.

use gpui::{
    div, point, px, size, AnyWindowHandle, App as GpuiApp, AppContext, Bounds, Context, DisplayId,
    Focusable, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels,
    Render, Size, Styled, Window, WindowBounds, WindowOptions,
};

use crate::App;

/// How far down its display the card's top edge sits: 30% of the display's
/// height. Dead center (50%) leaves the card floating in the middle of the
/// screen; a search-bar height reads as "hanging from above".
const TOP_FRAC: f32 = 0.30;

/// The palette window's view: one panel, sized to the window it owns.
pub(crate) struct PaletteWindow {
    app: gpui::Entity<App>,
    focus_handle: FocusHandle,
    /// The card's top edge, in logical px from the top of its display. The
    /// card is only measured after prepaint and the window resizes from its
    /// bottom-left corner, so the top is re-anchored to this whenever the
    /// card's height changes.
    top_px: f32,
    /// Whether this window has been key yet. A resign-key before that is not
    /// the user leaving the palette (it opened while pwrde was in the
    /// background, or the notification beat the first activation), so it
    /// must not hide it.
    was_active: bool,
}

impl Render for PaletteWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The shared search Input is rendered here while the palette is up,
        // so this is where a freshly opened stage's claim is consumed: set the
        // stage's placeholder, clear it, and focus it in THIS window.
        let claim = self.app.update(cx, |app, _| app.modal_search_reset.take());
        let input = self.app.read(cx).modal_search.clone();
        let input_focus = input.read(cx).focus_handle(cx);
        if let Some(placeholder) = claim {
            self.app.update(cx, |app, cx| {
                app.modal_search.update(cx, |input, cx| {
                    input.placeholder(placeholder);
                    input.set_text("", cx);
                });
            });
        }
        // The palette window holds exactly one editable field and nothing else,
        // so that field is always the focused element. This covers a freshly
        // opened window (whose creation focused the root view, not the field), a
        // window that just regained key status after pwrde sat in the background,
        // and each stage's claim. Without it the palette opens with the window
        // key but the field unfocused, so the first keystroke goes nowhere until
        // the user clicks the field.
        if !input_focus.is_focused(window) {
            window.focus(&input_focus, cx);
        }
        let panel = self.app.update(cx, |app, cx| app.command_panel_card(cx));
        let top_px = self.top_px;
        div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _win, cx| {
                if this.app.update(cx, |app, _| app.palette_key(ev)) {
                    cx.notify();
                }
            }))
            // The window is sized to the card, not the other way around: the
            // card is measured after prepaint and the window shrinks to it, so
            // no transparent margin is ever left around the panel.
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .on_children_prepainted(move |bounds, window, _cx| {
                        let Some(card) = bounds.first() else { return };
                        let current = f32::from(window.viewport_size().height);
                        if (f32::from(card.size.height) - current).abs() > 1.0 {
                            window.resize(card.size);
                            // AppKit keeps the bottom-left corner fixed through a
                            // resize, so without this the top edge would creep
                            // down (card shrinks) or hang off the display (card
                            // grows). Put it back on the 30% line it opened on.
                            pin_window_top(window, top_px);
                        }
                    })
                    .child(panel),
            )
    }
}

/// Open the command-palette window and store its handle on the [`App`].
///
/// Called by the frame pump while the palette wants a surface up (the model's
/// `command` is `Some`), so window lifecycle stays on the foreground executor
/// and entity code never touches a window it does not own.
pub(crate) fn open_palette_window(app: gpui::Entity<App>, cx: &mut GpuiApp) {
    // The window is the card: exactly `PANEL_W` wide (narrower than
    // `Bounds::centered`'s default), then shrunk to the panel's height on the
    // first prepaint. The height here is only the pre-measure placeholder.
    let panel_size = size(px(crate::command_ui::PANEL_W), px(240.0));
    // Open where the user is looking. `Bounds::centered(None, ..)` always fell
    // back to the primary display; on a multi-monitor setup that is not where
    // the focused window is. Preference order: the focused window's display
    // (⌘P inside pwrde), then pwrde's main window (the global ⌥⌘P hotkey can
    // fire while another app is frontmost), then the primary display.
    let main_window = app.read(cx).main_window;
    let display = palette_display_id(cx, main_window)
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display());
    let display_id = display.as_ref().map(|display| display.id());
    let bounds = match display.as_ref() {
        Some(display) => palette_bounds(display.bounds(), panel_size),
        None => Bounds { origin: point(px(0.), px(0.)), size: panel_size },
    };
    let top_px = f32::from(bounds.origin.y);
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
            display_id,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // Chrome-less: the traffic lights are hidden right after open and
            // `appears_transparent` gives the card the whole frame (no title
            // strip above it), so the rounded corners sit at the window edge.
            titlebar: Some(gpui::TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        move |window, cx| {
            // The close button dismisses the palette (the model is the source
            // of truth, so clearing it takes the window down for good).
            window.on_window_should_close(cx, move |_, cx| {
                let _ = app_for_close.update(cx, |app, _| {
                    app.close_command();
                    app.palette_window = None;
                });
                true
            });
            cx.new(|cx| {
                // Losing key status hides the palette (the model is kept) —
                // but only once this window has been key, and only while it
                // is still the app's palette window: the pump clears the
                // handle before it removes a window itself.
                cx.observe_window_activation(window, |view: &mut PaletteWindow, window, cx| {
                    if window.is_window_active() {
                        // Key in a background app is not the user being
                        // here: AppKit takes it straight back.
                        view.was_active |= app_is_active();
                        return;
                    }
                    if !view.was_active {
                        return;
                    }
                    let this_window = window.window_handle();
                    view.app.update(cx, |app, cx| {
                        if app.palette_window.map(AnyWindowHandle::from) == Some(this_window) {
                            app.hide_palette_on_blur();
                            cx.notify();
                        }
                    });
                })
                .detach();
                PaletteWindow {
                    app: app_for_view.clone(),
                    focus_handle: cx.focus_handle(),
                    top_px,
                    was_active: false,
                }
            })
        },
    );
    if let Ok(w) = handle {
        let _ = w.update(cx, |_view, window, _cx| {
            strip_window_chrome(window);
            // Enforce the placement on the window we just opened too, so the
            // pre-measure height cannot leave the card off the 30% line.
            pin_window_top(window, top_px);
            // The field claims focus on the first render
            // (`PaletteWindow::render`), so do not park focus on the root view
            // here: that would steal it back from the input on open.
            window.activate_window();
        });
        let _ = app.update(cx, |app, _| app.palette_window = Some(w));
    }
}

/// Where the palette window sits on `display_bounds` — expressed in that
/// display's own coordinate space: horizontally centered, with its top edge at
/// [`TOP_FRAC`] of the display's height.
///
/// Pure so the placement is unit-tested without a window server. A card taller
/// than the space below the line is pulled up until it fits, so a full-height
/// window can never hang off the bottom of the display.
pub(crate) fn palette_bounds(
    display_bounds: Bounds<Pixels>,
    size: Size<Pixels>,
) -> Bounds<Pixels> {
    let display_top = f32::from(display_bounds.origin.y);
    let display_h = f32::from(display_bounds.size.height);
    let top = display_top + display_h * TOP_FRAC;
    let max_top = display_top + (display_h - f32::from(size.height)).max(0.0);
    let x = f32::from(display_bounds.origin.x)
        + (f32::from(display_bounds.size.width) - f32::from(size.width)) * 0.5;
    let origin = point(px(x), px(top.min(max_top)));
    Bounds { origin, size }
}

/// The display the palette should open on: the focused window's display when
/// there is one, else the app's main window's, else `None` (the caller falls
/// back to the primary display).
pub(crate) fn palette_display_id(
    cx: &mut GpuiApp,
    main_window: Option<AnyWindowHandle>,
) -> Option<DisplayId> {
    let display_of = |cx: &mut GpuiApp, handle: AnyWindowHandle| {
        handle
            .update(cx, |_, window, cx| {
                window.display(cx).map(|display| display.id())
            })
            .ok()
            .flatten()
    };
    let active = cx.active_window();
    if let Some(id) = active.and_then(|handle| display_of(cx, handle)) {
        return Some(id);
    }
    main_window.and_then(|handle| display_of(cx, handle))
}

/// Re-anchor the window's top edge to `logical_top` (px from the top of its
/// display) after AppKit resized it from the bottom-left corner.
///
/// The logical coordinate is converted back to AppKit's bottom-left screen
/// space using the window's own screen, so it holds on any monitor of a
/// multi-monitor setup.
fn pin_window_top(window: &Window, logical_top: f32) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};

    // AppKit geometry declared locally: the crate has no CoreGraphics structs
    // and `objc` needs a type encoding to send these selectors.
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

    let Some(ns_window) = crate::ns_window(window) else {
        return;
    };
    unsafe {
        let screen: *mut Object = msg_send![ns_window, screen];
        if screen.is_null() {
            return;
        }
        let screen_frame: CGRect = msg_send![screen, frame];
        let frame: CGRect = msg_send![ns_window, frame];
        let top = screen_frame.origin.y + screen_frame.size.height - logical_top as f64;
        let _: () = msg_send![ns_window, setFrameTopLeftPoint: CGPoint { x: frame.origin.x, y: top }];
    }
}

/// Whether pwrde is the frontmost app (`NSApp.isActive`).
fn app_is_active() -> bool {
    use objc::runtime::{Object, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let active: objc::runtime::BOOL = msg_send![app, isActive];
        active == YES
    }
}

/// Remove the macOS window chrome the palette never asked for: the traffic
/// lights, and the window's own opaque background -- the card's rounded
/// corners and glass fill are the window's whole silhouette.
fn strip_window_chrome(window: &Window) {
    use objc::runtime::{Object, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    let Some(ns_window) = crate::ns_window(window) else {
        return;
    };
    unsafe {
        // NSWindowCloseButton = 0, NSWindowMiniaturizeButton = 1,
        // NSWindowZoomButton = 2. Hiding the buttons keeps the titled (key)
        // window behaviour while the titlebar strip stays invisible.
        for which in [0isize, 1, 2] {
            let button: *mut Object = msg_send![ns_window, standardWindowButton: which];
            if !button.is_null() {
                let _: () = msg_send![button, setHidden: YES];
            }
        }
        let clear: *mut Object = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![ns_window, setBackgroundColor: clear];
        let _: () = msg_send![ns_window, setOpaque: NO];
        // NSWindowTitleHidden = 1: the full-size content view still lets
        // AppKit draw the window title in the (invisible) titlebar strip, so
        // hide it explicitly rather than merely leaving `title: None`.
        let _: () = msg_send![ns_window, setTitleVisibility: 1isize];
        let _: () = msg_send![ns_window, setTitlebarAppearsTransparent: YES];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    fn display(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    fn card() -> Size<Pixels> {
        size(px(crate::command_ui::PANEL_W), px(240.0))
    }

    #[test]
    fn top_edge_sits_at_thirty_percent_of_the_display() {
        let bounds = palette_bounds(display(0.0, 0.0, 1440.0, 900.0), card());
        let top = f32::from(bounds.origin.y);
        let left = f32::from(bounds.origin.x);
        assert!((top - 270.0).abs() < 0.01, "top {top} != 30% of 900");
        assert!((left - 440.0).abs() < 0.01, "left {left} != centered");
    }

    #[test]
    fn top_follows_a_secondary_displays_origin() {
        // A display laid out to the right of the primary: the card must land on
        // that display's own 30% line, not the primary's.
        let bounds = palette_bounds(display(1920.0, 0.0, 1440.0, 900.0), card());
        assert!((f32::from(bounds.origin.x) - 2360.0).abs() < 0.01);
        assert!((f32::from(bounds.origin.y) - 270.0).abs() < 0.01);
    }

    #[test]
    fn a_card_taller_than_the_space_below_is_pulled_up() {
        let card = size(px(560.0), px(800.0));
        let bounds = palette_bounds(display(0.0, 0.0, 1440.0, 900.0), card);
        assert!((f32::from(bounds.origin.y) - 100.0).abs() < 0.01, "card must stay on screen");
    }
}
