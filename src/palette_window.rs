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
//! The window contains nothing but that one field, so this view keeps the
//! shared `Input` focused for as long as the window is up: opening the palette
//! is immediately typeable, with no click needed.

use gpui::{
    div, px, size, App as GpuiApp, AppContext, Bounds, Context, Focusable, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions,
};

use crate::App;

/// The palette window's view: one panel, sized to the window it owns.
pub(crate) struct PaletteWindow {
    app: gpui::Entity<App>,
    focus_handle: FocusHandle,
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
                    .on_children_prepainted(|bounds, window, _cx| {
                        let Some(card) = bounds.first() else { return };
                        let current = f32::from(window.viewport_size().height);
                        if (f32::from(card.size.height) - current).abs() > 1.0 {
                            window.resize(card.size);
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
    let bounds = Bounds::centered(None, size(px(crate::command_ui::PANEL_W), px(240.0)), cx);
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
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
            cx.new(|cx| PaletteWindow {
                app: app_for_view.clone(),
                focus_handle: cx.focus_handle(),
            })
        },
    );
    if let Ok(w) = handle {
        let _ = w.update(cx, |_view, window, _cx| {
            strip_window_chrome(window);
            // The field claims focus on the first render
            // (`PaletteWindow::render`), so do not park focus on the root view
            // here: that would steal it back from the input on open.
            window.activate_window();
        });
        let _ = app.update(cx, |app, _| app.palette_window = Some(w));
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
