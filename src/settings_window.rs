//! The Settings window.
//!
//! Settings stops being a page of the main window and becomes a real,
//! separate window: NORMAL chrome (traffic lights, resize, minimize), opened
//! by ⌘, / the sidebar gear chip, reconciled by the frame pump the same way
//! the palette window is.
//!
//! The settings *state* stays on [`App`] (`section`, `recording`,
//! `settings_search`, `settings_query`); this view only renders it in its own
//! window (`App::settings_content`) and routes the keys the model owns
//! (`App::handle_settings_key`). Like the palette window, this view keeps the
//! shared search Input focused while the window is up so typing lands in it —
//! but never over a still-recording binding row, which needs the keystrokes.

use gpui::{
    div, point, px, size, prelude::FluentBuilder, App as GpuiApp, AppContext, Bounds, Context,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Render, Styled, Window, WindowBounds, WindowOptions,
};

use crate::pages::Section;
use crate::App;
use crate::ui::assets::{icon, ICON_KEYBOARD, ICON_PALETTE, ICON_SEARCH, ICON_SETTINGS, ICON_SLIDERS, ICON_WRENCH};
use crate::workspace::{
    settings_nav_row_rect, LayoutRect, SETTINGS_NAV_GAP, SETTINGS_NAV_INSET, SETTINGS_NAV_SPACER,
};

/// The nav pane as a rect, so the row heights and gaps come from the pure
/// geometry in `workspace.rs` rather than from ad-hoc padding here.
fn nav_pane() -> LayoutRect {
    LayoutRect { x: 0.0, y: 0.0, w: 236.0, h: 0.0 }
}
use crate::ui::theme::Theme;

/// The nav pane's width in logical px.
const NAV_W: f32 = 236.0;

/// The nav icon for a section. The rows themselves come from
/// [`Section::ALL`]; Advanced (the last entry) sits under its own caption.
fn nav_icon(section: Section) -> &'static str {
    match section {
        Section::General => ICON_SETTINGS,
        Section::Appearance => ICON_PALETTE,
        Section::Tools => ICON_WRENCH,
        Section::Keyboard => ICON_KEYBOARD,
        Section::Advanced => ICON_SLIDERS,
    }
}

/// The Settings window's view: a 236px nav sidebar + the settings content.
pub(crate) struct SettingsWindow {
    app: gpui::Entity<App>,
    focus_handle: FocusHandle,
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Style the shared search Input to the sidebar's type size; the
        // content column re-applies its own sizes each render.
        self.app.update(cx, |app, cx| {
            app.settings_search.update(cx, |input, _| {
                input.set_text_size(Some(px(12.5)));
            });
        });

        // The shared search Input is rendered here while the window is up, so
        // focus it in THIS window — but never over a recording row or a
        // focused form field, which own the keystrokes.
        let search_focus = self
            .app
            .read(cx)
            .settings_search
            .read(cx)
            .focus_handle(cx);
        let keystrokes_owned = self.app.update(cx, |app, cx| {
            app.recording.is_some()
                || app.command_input.read(cx).focus_handle(cx).is_focused(window)
                || app
                    .tool_form
                    .inputs()
                    .iter()
                    .any(|e| e.read(cx).focus_handle(cx).is_focused(window))
        });
        if !keystrokes_owned && !search_focus.is_focused(window) {
            window.focus(&search_focus, cx);
        }

        let sidebar = self.render_sidebar(window, cx);
        let content = self.app.update(cx, |app, cx| app.settings_content(cx));

        div()
            .size_full()
            .flex()
            .flex_row()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                // ⌘W closes the window — unless a binding row is recording,
                // when the chord is the recording. The model handles the rest.
                if ev.keystroke.modifiers.platform
                    && ev.keystroke.key == "w"
                    && this.app.read(cx).recording.is_none()
                {
                    this.close(window, cx);
                    return;
                }
                if ev.keystroke.key == "escape" {
                    let recording = this.app.read(cx).recording.is_some();
                    let query_empty = this.app.read(cx).settings_query.is_empty();
                    // The search field is kept focused while the window is up
                    // (see render), so focus cannot gate this: Esc closes
                    // unless a binding row is recording or a query is typed —
                    // then it cancels / clears through the model instead.
                    if !recording && query_empty {
                        this.close(window, cx);
                        return;
                    }
                }
                this.app.update(cx, |app, cx| app.handle_settings_key(ev, window, cx));
                cx.notify();
            }))
            .child(sidebar)
            .child(content)
    }
}

impl SettingsWindow {
    /// Dismiss the window: clear the model flags (the source of truth) and
    /// take the surface down.
    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.app.update(cx, |app, _| {
            app.settings_open = false;
            app.settings_window = None;
        });
        window.remove_window();
    }

    /// The left nav pane, painted on the chrome sidebar ground.
    fn render_sidebar(&self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let theme = Theme::from_chrome(crate::theme::current());
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let dark = theme.dark;
        let ink = theme.foreground;
        let muted = ink.opacity(0.55);
        let accent = crate::sidebar_ui::accent();
        let active = self.app.read(cx).section;

        let pane = nav_pane();
        let mut nav =
            div().flex().flex_col().gap(px(SETTINGS_NAV_GAP)).mx(px(SETTINGS_NAV_INSET));
        let top_rows = Section::ALL.iter().copied().filter(|s| *s != Section::Advanced);
        for (index, section) in top_rows.enumerate() {
            let is_active = section == active;
            let entity = self.app.clone();
            nav = nav.child(
                div()
                    .id(section.label())
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(settings_nav_row_rect(1.0, &pane, index).h))
                    .px(px(12.0))
                    .gap(px(9.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .when(is_active, |el| {
                        el.bg(accent)
                            .text_color(gpui::white())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .when(!is_active, |el| {
                        el.text_color(ink)
                            .hover(|s| s.bg(ink.opacity(0.06)))
                    })
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        window.prevent_default();
                        entity.update(cx, |app, cx| {
                            app.section = section;
                            app.recording = None;
                            app.clear_settings_search(cx);
                            cx.notify();
                        });
                    })
                    .child(icon(nav_icon(section), px(15.0), if is_active { gpui::white().opacity(0.9) } else { muted }))
                    .child(
                        gpui::div()
                            .text_size(px(12.5))
                            .child(section.label()),
                    ),
            );
        }

        // Search box, header strip (traffic lights), then the rows, then the
        // Advanced group, with the version footer pinned at the bottom.
        let search = self.app.read(cx).settings_search.clone();
        div()
            .w(px(NAV_W))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(crate::renderer::color(
                crate::theme::mix(crate::theme::current().gradient_to, crate::theme::current().accent, if dark { 0.06 } else { 0.03 }),
                1.0,
            ))
            // 44px header strip left empty for the traffic lights.
            .child(div().h(px(44.0)))
            .child(
                div()
                    .px(px(8.0))
                    .pb(px(10.0))
                    .child(
                        div()
                            .id("settings-search")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(9.0))
                            .py(px(6.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(if dark {
                                gpui::black().opacity(0.07)
                            } else {
                                gpui::white().opacity(0.07)
                            })
                            .bg(if dark {
                                gpui::black().opacity(0.22)
                            } else {
                                gpui::black().opacity(0.05)
                            })
                            .child(icon(ICON_SEARCH, px(13.0), muted))
                            .child(search),
                    ),
            )
            .child(nav)
            // Spacer between the page rows and the Advanced group.
            .child(div().h(px(SETTINGS_NAV_SPACER)))
            .child(
                div()
                    .px(px(12.0))
                    .pb(px(4.0))
                    .child(
                        gpui::div()
                            .text_size(px(10.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("ADVANCED"),
                    ),
            )
            .child(self.advanced_row(cx, dark, ink, muted, accent))
            .child(div().flex_1())
            .child(
                div()
                    .px(px(14.0))
                    .py(px(12.0))
                    .child(
                        gpui::div()
                            .text_size(px(10.5))
                            .text_color(muted)
                            .child(format!("pwrde {}", env!("CARGO_PKG_VERSION"))),
                    ),
            )
    }

    /// The Advanced row (diagnostics & helpers): sliders icon + label.
    fn advanced_row(
        &self,
        cx: &mut Context<Self>,
        dark: bool,
        ink: gpui::Hsla,
        muted: gpui::Hsla,
        accent: gpui::Hsla,
    ) -> gpui::Stateful<gpui::Div> {
        let is_active = self.app.read(cx).section == crate::pages::Section::Advanced;
        let entity = self.app.clone();
        let _ = dark;
        div()
            .id("Advanced")
            .mx(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .px(px(12.0))
            .py(px(7.0))
            .gap(px(9.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .when(is_active, |el| {
                el.bg(accent)
                    .text_color(gpui::white())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .when(!is_active, |el| el.text_color(ink).hover(|s| s.bg(ink.opacity(0.06))))
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                window.prevent_default();
                entity.update(cx, |app, cx| {
                    app.section = crate::pages::Section::Advanced;
                    app.recording = None;
                    app.clear_settings_search(cx);
                    cx.notify();
                });
            })
            .child(icon(
                ICON_SLIDERS,
                px(15.0),
                if is_active { gpui::white().opacity(0.9) } else { muted },
            ))
            .child(gpui::div().text_size(px(12.5)).child("Advanced"))
    }

}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Open the Settings window and store its handle on the [`App`].
///
/// Called by the frame pump while `settings_open` is set, so window lifecycle
/// stays on the foreground executor and entity code never touches a window it
/// does not own.
pub(crate) fn open_settings_window(app: gpui::Entity<App>, cx: &mut GpuiApp) {
    // Open where the user is looking: the focused window's display, then the
    // main window's, then the primary display (same preference as the palette
    // window, via its shared display picker).
    let main_window = app.read(cx).main_window;
    let display = crate::palette_window::palette_display_id(cx, main_window)
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display());
    let display_id = display.as_ref().map(|display| display.id());
    let window_size = size(px(960.0), px(620.0));
    // `Bounds::centered(None, ..)` falls back to the primary display.
    let bounds = Bounds::centered(display_id, window_size, cx);
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
            display_id,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.0), px(16.0))),
            }),
            window_min_size: Some(size(px(760.0), px(480.0))),
            is_resizable: true,
            is_minimizable: true,
            ..Default::default()
        },
        move |window, cx| {
            // The close button dismisses the window (the model is the source
            // of truth, so clearing it takes the window down for good).
            window.on_window_should_close(cx, move |_, cx| {
                let _ = app_for_close.update(cx, |app, _| {
                    app.settings_open = false;
                    app.settings_window = None;
                });
                true
            });
            cx.new(|cx| {
                let view = SettingsWindow {
                    app: app_for_view.clone(),
                    focus_handle: cx.focus_handle(),
                };
                cx.observe(&app_for_view, |_, _, cx| cx.notify()).detach();
                view
            })
        },
    );
    if let Ok(w) = handle {
        let _ = w.update(cx, |_view, window, _cx| {
            window.activate_window();
        });
        let _ = app.update(cx, |app, _| app.settings_window = Some(w));
    }
}
