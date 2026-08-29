//! pwrde in a browser: the pwrde library booted through gpui's web platform
//! (gpui_web + gpui_wgpu) and drawn to a full-page canvas with WebGPU (or
//! WebGL as a fallback). Build and serve with `trunk serve` from this
//! directory, or let `scripts/web-screenshot.sh` drive it headlessly.
//!
//! Today this is the boot-path smoke test — the chrome theme and the vendored
//! rcn components, proving the whole lib links and paints on wasm32. Booting
//! the real `pwrde::app::App` (with PTY-less fixture sessions) is the next
//! step; see `docs/web-build.md`.

use gpui::{
    App, AppContext, Bounds, Context, ParentElement, Render, Styled, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};

use pwrde::ui::theme::Theme;
use pwrde::ui::{
    Badge, Button, ButtonSize, ButtonVariant, Card, CardDescription, CardHeader, CardTitle, Input,
};

/// `?backend=webgpu` / `?backend=webgl` force a renderer; default auto-detects
/// (WebGPU where available, WebGL otherwise).
fn requested_backend() -> gpui_platform::WebBackendPreference {
    let search = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    let has = |needle: &str| search.trim_start_matches('?').split('&').any(|p| p == needle);
    if has("backend=webgpu") {
        gpui_platform::WebBackendPreference::WebGpu
    } else if has("backend=webgl") {
        gpui_platform::WebBackendPreference::WebGl
    } else {
        gpui_platform::WebBackendPreference::Auto
    }
}

struct Smoke;

impl Render for Smoke {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let (bg, fg, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        div()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .flex()
            .items_center()
            .justify_center()
            .child(div().w(px(420.0)).child(
                Card::new()
                    .child(
                        CardHeader::new()
                            .child(CardTitle::new().child("pwrde web smoke"))
                            .child(CardDescription::new().child(
                                "gpui_web + the pwrde library, painted from the chrome theme.",
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .px_6()
                            .pb_6()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(Badge::new().child("wasm32"))
                                    .child(Badge::new().child(format!("theme: {}", pwrde::theme::current().name))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(Button::new("primary").size(ButtonSize::Sm).child("Primary"))
                                    .child(
                                        Button::new("outline")
                                            .variant(ButtonVariant::Outline)
                                            .size(ButtonSize::Sm)
                                            .child("Outline"),
                                    ),
                            )
                            .child(div().text_sm().text_color(muted).child(
                                "If you can read this in a headless Chromium screenshot, the toolchain works.",
                            )),
                    ),
            ))
    }
}

fn main() {
    gpui_platform::web_init();
    // On wasm the browser owns the run loop: Platform::run returns immediately,
    // so `Application::run`'s stack frame — which keeps the App alive on native
    // — would drop the whole app right after launch. run_embedded returns a
    // handle instead; leak it so the app lives for the lifetime of the page.
    let app = gpui_platform::application_with_web_backend(requested_backend())
        .with_assets(pwrde::ui::assets::Assets)
        .run_embedded(|cx: &mut App| {
            // Same seeding as the native boot: the rcn Theme global tracks the
            // chrome theme, and the Input component wants its key bindings.
            cx.set_global(Theme::from_chrome(pwrde::theme::current()));
            Input::register_key_bindings(cx);
            let bounds = Bounds::centered(None, size(px(1200.0), px(720.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Smoke),
            )
            .expect("failed to open window");
            cx.activate(true);
        });
    std::mem::forget(app);
}
