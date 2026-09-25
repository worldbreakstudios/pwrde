//! The command palette's own window.
//!
//! A webview tab is a native child `NSView` painted over the app's surface,
//! so an in-window palette could only ever be drawn by hiding the webviews
//! underneath it. The palette is instead a real, separate window: hidden
//! until ⌘P (or the system-wide ⌥⌘P) asks for it, then brought to the front —
//! the Spotlight/Raycast flow, with nothing painted over a webview.
//!
//! The palette *state* stays on [`App`] (`command`, `command_scroll`,
//! `command_scroll_to`, `modal_search`); this view only renders it in its own
//! window and routes the keys the model owns. The panel itself is
//! `App::render_command_panel`.

use gpui::{
    div, px, App as GpuiApp, AppContext, Bounds, Context, Focusable, FocusHandle,
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
        if let Some(placeholder) = claim {
            self.app.update(cx, |app, cx| {
                app.modal_search.update(cx, |input, cx| {
                    input.placeholder(placeholder);
                    input.set_text("", cx);
                });
            });
            window.focus(&input.read(cx).focus_handle(cx), cx);
        }
        let size = window.viewport_size();
        let (win_w, win_h) = (f32::from(size.width), f32::from(size.height));
        let panel = self
            .app
            .update(cx, |app, cx| app.render_command_panel(win_w, win_h, cx));
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _win, cx| {
                if this.app.update(cx, |app, _| app.palette_key(ev)) {
                    cx.notify();
                }
            }))
            .child(panel)
    }
}

/// Open the command-palette window and store its handle on the [`App`].
///
/// Called by the frame pump while the palette wants a surface up (the model's
/// `command` is `Some`), so window lifecycle stays on the foreground executor
/// and entity code never touches a window it does not own.
pub(crate) fn open_palette_window(app: gpui::Entity<App>, cx: &mut GpuiApp) {
    let bounds = Bounds::centered(None, gpui::size(px(640.0), px(560.0)), cx);
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("pwrde — command".into()),
                ..Default::default()
            }),
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
        let _ = w.update(cx, |view, window, cx| {
            window.activate_window();
            let fh = view.focus_handle.clone();
            window.focus(&fh, cx);
        });
        let _ = app.update(cx, |app, _| app.palette_window = Some(w));
    }
}
