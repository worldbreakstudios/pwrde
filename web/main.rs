//! pwrde in a browser: the same `App` the macOS binary runs, booted through
//! gpui's web platform (gpui_web + gpui_wgpu) and drawn to a full-page canvas
//! with WebGPU (or WebGL as a fallback). There is no PTY on wasm32, so the
//! workspace is seeded from recorded transcripts (`pwrde::fixture`) instead of
//! shells. Build and serve with `trunk serve` from this directory, or let
//! `scripts/web-screenshot.sh` drive it headlessly.
//!
//! Query parameters, all optional:
//!   `?page=sessions|settings|cleanup|…`  open on that page (see `pages::Page`)
//!   `?dark=1` / `?dark=0`                force the appearance polarity
//!   `?fixture=none`                      skip the demo workspace (empty state)
//!   `?backend=webgpu` / `?backend=webgl` force a renderer (default: auto)

use gpui::{App as GpuiApp, Bounds, WindowBounds, WindowOptions, px, size};

use pwrde::app::App;
use pwrde::pages::Page;
use std::borrow::Cow;

/// The faces `web/fonts/README.md` describes: the terminal/chrome family,
/// then symbol fallbacks for what it lacks.
const FONTS: &[&[u8]] = &[
    include_bytes!("fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
    include_bytes!("fonts/JetBrainsMonoNerdFontMono-Bold.ttf"),
    include_bytes!("fonts/JetBrainsMonoNerdFontMono-Italic.ttf"),
    include_bytes!("fonts/NotoSansSymbols-Regular.ttf"),
    include_bytes!("fonts/NotoSansSymbols2-Regular.ttf"),
    include_bytes!("fonts/NotoSansMath-Regular.ttf"),
    include_bytes!("fonts/NotoEmoji-Regular.ttf"),
    include_bytes!("fonts/NotoSansJP-Fullwidth.ttf"),
];

/// The URL's query string, split into `key=value` pairs.
struct Query(Vec<(String, String)>);

impl Query {
    fn from_location() -> Self {
        let search = web_sys::window()
            .and_then(|window| window.location().search().ok())
            .unwrap_or_default();
        Self(
            search
                .trim_start_matches('?')
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|p| match p.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (p.to_string(), String::new()),
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn backend(&self) -> gpui_platform::WebBackendPreference {
        match self.get("backend") {
            Some("webgpu") => gpui_platform::WebBackendPreference::WebGpu,
            Some("webgl") => gpui_platform::WebBackendPreference::WebGl,
            _ => gpui_platform::WebBackendPreference::Auto,
        }
    }

    /// `?page=settings` → `Page::Settings`; the names are the variants,
    /// case-insensitively.
    fn page(&self) -> Option<Page> {
        let want = self.get("page")?;
        Page::ALL.iter().copied().find(|p| format!("{p:?}").eq_ignore_ascii_case(want))
    }

    fn dark(&self) -> Option<bool> {
        match self.get("dark") {
            Some("1") | Some("true") => Some(true),
            Some("0") | Some("false") => Some(false),
            _ => None,
        }
    }

    fn fixture(&self) -> bool {
        !matches!(self.get("fixture"), Some("none") | Some("0"))
    }
}

fn main() {
    gpui_platform::web_init();
    let query = Query::from_location();

    // Same order as the native boot: settings first. There is no settings
    // file on the web, so this is the empty store and every default — and a
    // `?dark=` override lands in the store as the appearance mode (the write
    // to disk fails silently), which is what the window's own appearance
    // tracking would otherwise keep overriding.
    pwrde::settings::init();
    if let Some(dark) = query.dark() {
        pwrde::settings::set("appearance.mode", if dark { "dark" } else { "light" }.into());
    }

    let (events_tx, events_rx) = std::sync::mpsc::channel();
    // On wasm the browser owns the run loop: Platform::run returns immediately,
    // so `Application::run`'s stack frame — which keeps the App alive on native
    // — would drop the whole app right after launch. run_embedded returns a
    // handle instead; leak it so the app lives for the lifetime of the page.
    let app = gpui_platform::application_with_web_backend(query.backend())
        .with_assets(pwrde::ui::assets::Assets)
        .run_embedded(move |cx: &mut GpuiApp| {
            // gpui_web knows only its own embedded faces; register the family
            // the renderer and chrome name before anything measures a cell.
            if let Err(err) = cx.text_system().add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()) {
                log::warn!("could not register embedded fonts: {err}");
            }
            pwrde::app::init_globals(cx);
            let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
            let window = App::open_main_window(
                cx,
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                (events_tx, events_rx),
            );
            let _ = window.update(cx, |app, _window, cx| {
                if query.fixture() {
                    pwrde::fixture::seed(app, pwrde::fixture::DEMO);
                }
                if let Some(page) = query.page() {
                    app.set_page(page);
                }
                cx.notify();
            });
            cx.activate(true);
        });
    std::mem::forget(app);
}
