//! Main-thread lifecycle for native Wry child views.
//!
//! Also the favicon plumbing for web tabs: the initialization script posts
//! each page's icon link over IPC ([`favicon_request`] validates it into a
//! [`TermEvent::WebviewFaviconChanged`]), and [`fetch_favicon`] — called from a
//! background thread only — downloads and decodes it for the tab strips.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::mpsc::Sender;

use gpui::{RenderImage, Window};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{PageLoadEvent, Rect, WebView, WebViewBuilder};

use crate::term::TermEvent;
use crate::workspace::LayoutRect;

/// Browser chrome is GPUI-owned; the native child starts below it. This is
/// the toolbar's height at the default chrome text size — the live height is
/// [`toolbar_h`].
pub const TOOLBAR_H: f32 = 48.0;

/// Safari's own user agent for the native child views. WKWebView's default
/// carries no `Version/` or `Safari/` product token, so sites like Google
/// treat it as an unknown legacy browser and serve their fallback layouts.
/// It must stay a *Safari* string rather than a Chrome one: the engine really
/// is WebKit, and Google's sign-in refuses ("This browser or app may not be
/// secure") when the advertised browser and the engine's fingerprint disagree.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15";

/// IPC prefix of the favicon report: `<prefix><icon href>\n<page URL>`.
const FAVICON_IPC: &str = "pwrde:favicon:";
/// Posted from the top frame once the document has parsed and again on load
/// (sites that inject their icon link late): the first non-SVG
/// `link[rel~="icon"]` href — empty when the page names none — and the page's
/// own URL, which [`favicon_request`] falls back to `<origin>/favicon.ico` on.
const INIT_SCRIPT: &str = "addEventListener('pointerdown',()=>window.ipc.postMessage('pwrde:webview-focus'),true);\
if(window.top===window){const post=()=>{const l=[...document.querySelectorAll('link[rel~=\"icon\"]')]\
.find(l=>l.href&&!/svg/i.test(l.type)&&!/\\.svg([?#]|$)/i.test(l.href));\
window.ipc.postMessage('pwrde:favicon:'+(l?l.href:'')+'\\n'+location.href)};\
if(document.readyState==='loading')addEventListener('DOMContentLoaded',post);else post();\
addEventListener('load',post)}";

/// Favicon fetch limits: response bytes, wall-clock seconds, and the edge the
/// decoded icon is resampled to (the strip paints it at 14 logical px, so
/// this is 1:1 on a 2x display and an exact halving on 1x).
pub const FAVICON_MAX_BYTES: usize = 512 * 1024;
const FAVICON_TIMEOUT_SECS: &str = "5";
const FAVICON_EDGE: u32 = 28;
/// The edge a transparent mark is drawn at inside that disc (the mock's 10px
/// mark in a 14px disc).
const FAVICON_INSET_EDGE: u32 = 20;
const FAVICON_MARK_ALPHA: f32 = 0.9;
/// The discs a transparent favicon is flattened onto (the mock's `#f2f2f7`,
/// and a dark one for an icon whose mean luma is above the threshold).
const FAVICON_DISC_LIGHT: [u8; 3] = [0xf2, 0xf2, 0xf7];
const FAVICON_DISC_DARK: [u8; 3] = [0x1c, 0x1c, 0x1e];
const FAVICON_LIGHT_LUMA: f32 = 170.0;
/// Decoder allocation cap for one favicon, whatever its header claims.
const FAVICON_MAX_ALLOC: u64 = 64 * 1024 * 1024;

/// `scheme://authority` of an absolute http(s) URL, `None` for anything else.
pub fn origin(url: &str) -> Option<&str> {
    let scheme = if url.starts_with("https://") {
        "https://".len()
    } else if url.starts_with("http://") {
        "http://".len()
    } else {
        return None;
    };
    let authority = url[scheme..].split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.starts_with(':') || authority.ends_with(':') {
        return None;
    }
    Some(&url[..scheme + authority.len()])
}

/// An icon URL the fetcher may be handed: absolute http(s) with a host, no
/// whitespace or control characters, and of a sane length.
pub fn favicon_fetchable(url: &str) -> bool {
    url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && origin(url).is_some()
}

/// Parse a favicon IPC report (the body after [`FAVICON_IPC`]) into the page's
/// origin and the icon URL to fetch: the page's own link when it is fetchable,
/// else `<origin>/favicon.ico`. `None` when the page itself is not http(s)
/// (a blank or internal page), which clears the tab's icon.
pub fn favicon_request(report: &str) -> Option<(String, String)> {
    let (href, page) = report.split_once('\n')?;
    let origin = origin(page.trim()).filter(|origin| favicon_fetchable(origin))?;
    let href = href.trim();
    let icon = if favicon_fetchable(href) {
        href.to_string()
    } else {
        format!("{origin}/favicon.ico")
    };
    Some((origin.to_string(), icon))
}

/// Decode fetched icon bytes to `(width, height, RGBA8 pixels)`: always an
/// opaque [`FAVICON_EDGE`] square ready for the strip's circular clip. Anything the `image` crate cannot read
/// (SVG, HTML error pages, truncated files) is `None`.
fn decode_favicon(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // A favicon-sized allocation cap: the header is the page's to forge.
    let mut reader =
        image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(FAVICON_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return None;
    }
    // Flatten at full size first (resampling straight alpha fringes the
    // edges), then resample once, here, with a proper filter: the strip then
    // paints the icon 1:1 on a 2x display instead of minifying it bilinearly.
    let mut rgba = decoded.to_rgba8();
    let (disc, transparent) = flatten_on_disc(&mut rgba);
    // A mark with transparency sits inset on its disc, as in the mock; an
    // opaque icon fills the circle.
    let inner = if transparent { FAVICON_INSET_EDGE } else { FAVICON_EDGE };
    let fit = inner as f32 / rgba.width().max(rgba.height()) as f32;
    let (w, h) = (
        ((rgba.width() as f32 * fit).round() as u32).clamp(1, inner),
        ((rgba.height() as f32 * fit).round() as u32).clamp(1, inner),
    );
    let scaled = image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Lanczos3);
    let mut canvas = image::RgbaImage::from_pixel(
        FAVICON_EDGE,
        FAVICON_EDGE,
        image::Rgba([disc[0], disc[1], disc[2], 255]),
    );
    image::imageops::replace(
        &mut canvas,
        &scaled,
        ((FAVICON_EDGE - w) / 2) as i64,
        ((FAVICON_EDGE - h) / 2) as i64,
    );
    Some((FAVICON_EDGE, FAVICON_EDGE, canvas.into_raw()))
}

/// Flatten a transparent icon onto the disc it reads against — the mock's
/// light disc, or a dark one under a light icon (GitHub serves a white mark
/// to a dark-mode page) — so the glyph shows on any ground. The strip clips
/// the result to a circle; an opaque icon comes through unchanged. Returns
/// the disc colour and whether the icon is a transparent mark (mean alpha
/// under [`FAVICON_MARK_ALPHA`]; a rounded-corner tile still counts as opaque).
fn flatten_on_disc(rgba: &mut image::RgbaImage) -> ([u8; 3], bool) {
    let (mut luma, mut weight) = (0.0f32, 0.0f32);
    for px in rgba.pixels() {
        let a = px[3] as f32 / 255.0;
        luma += a * (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32);
        weight += a;
    }
    let light = weight > 0.0 && luma / weight > FAVICON_LIGHT_LUMA;
    let disc = if light { FAVICON_DISC_DARK } else { FAVICON_DISC_LIGHT };
    for px in rgba.pixels_mut() {
        let a = px[3] as u32;
        for c in 0..3 {
            px[c] = ((px[c] as u32 * a + disc[c] as u32 * (255 - a)) / 255) as u8;
        }
        px[3] = 255;
    }
    let pixels = (rgba.width() * rgba.height()).max(1) as f32;
    (disc, weight / pixels < FAVICON_MARK_ALPHA)
}

/// Download and decode one favicon. Blocking — background threads only. The
/// URL goes to `/usr/bin/curl` as a single argv element (never a shell) with
/// globbing and `~/.curlrc` off — the page chose the URL — and curl is held to http(s), [`FAVICON_TIMEOUT_SECS`] and [`FAVICON_MAX_BYTES`], and
/// the read itself stops at the cap for responses that state no length.
pub fn fetch_favicon(url: &str) -> Option<RenderImage> {
    if !favicon_fetchable(url) {
        return None;
    }
    let max = FAVICON_MAX_BYTES.to_string();
    let mut child = std::process::Command::new("/usr/bin/curl")
        .args(["-q", "-gsfL", "--max-time", FAVICON_TIMEOUT_SECS, "--max-filesize", &max])
        .args(["--max-redirs", "5", "--proto", "=http,https", "--proto-redir", "=http,https"])
        .args(["-A", USER_AGENT, "--", url])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|out| out.take(FAVICON_MAX_BYTES as u64 + 1).read_to_end(&mut bytes));
    let oversized = bytes.len() > FAVICON_MAX_BYTES;
    if oversized {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    if oversized || !status.success() || !matches!(read, Some(Ok(_))) {
        return None;
    }
    let (w, h, rgba) = decode_favicon(&bytes)?;
    Some(crate::renderer::render_image(w, h, &rgba))
}

pub fn normalize_input(value: &str) -> Result<String, String> {
    let value = value.trim();
    let candidate = if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    crate::bus::validate_webview_url(&candidate)
}

/// Logical height of the browser toolbar: [`TOOLBAR_H`] scaled with
/// `appearance.font_size` by the factor the tab strips and the sidebar use
/// ([`crate::workspace::chrome_ui_scale`]). `webview_ui` paints the bar this
/// tall and `sync_webviews` reserves the same height above the native view.
pub fn toolbar_h() -> f32 {
    toolbar_h_at(crate::workspace::chrome_ui_scale())
}

/// [`toolbar_h`] at chrome factor `ui`. Pure, so tests can pin it.
fn toolbar_h_at(ui: f32) -> f32 {
    TOOLBAR_H * ui
}

/// Convert the tile content rect (physical pixels) into the child-view rect,
/// reserving logical pixels for the browser toolbar (`0.0` when the tab hides
/// its title bar). The Site / Tools popovers are their own window and reserve
/// nothing: the page fills the tile below the bar whether or not one is open.
pub fn child_bounds(content: LayoutRect, scale: f32, toolbar_h: f32) -> LayoutRect {
    let reserve = (toolbar_h.max(0.0) * scale).min((content.h - 1.0).max(0.0));
    LayoutRect {
        x: content.x,
        y: content.y + reserve,
        w: content.w.max(1.0),
        h: (content.h - reserve).max(1.0),
    }
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub id: u64,
    pub url: String,
    pub bounds: LayoutRect,
}

struct Entry {
    view: WebView,
    bounds: LayoutRect,
    visible: bool,
    zoom: f64,
}

#[derive(Clone, Debug)]
pub struct BrowserState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom_percent: u16,
}

#[derive(Default)]
pub struct Manager {
    entries: HashMap<u64, Entry>,
    failed: HashSet<u64>,
    focused: Option<u64>,
}

impl Manager {
    /// Reconcile native views with the tab model. `live` includes webviews in
    /// every group; `placements` contains only views visible in this frame.
    pub fn sync(
        &mut self,
        window: &Window,
        live: &HashSet<u64>,
        placements: &[Placement],
        focus: Option<u64>,
        events: &Sender<TermEvent>,
    ) -> Option<String> {
        let mut first_error = None;
        let visible: HashSet<u64> = placements.iter().map(|p| p.id).collect();
        self.entries.retain(|id, entry| {
            if live.contains(id) {
                true
            } else {
                let _ = entry.view.focus_parent();
                false
            }
        });
        if self
            .focused
            .is_some_and(|id| !self.entries.contains_key(&id))
        {
            self.focused = None;
        }
        // Suppress repeated failures while a tab stays visible, but retry after
        // the user switches away and revisits it.
        self.failed
            .retain(|id| live.contains(id) && visible.contains(id));

        for (id, entry) in &mut self.entries {
            if !visible.contains(id) && entry.visible {
                let _ = entry.view.focus_parent();
                let _ = entry.view.set_visible(false);
                entry.visible = false;
            }
        }

        for placement in placements {
            if let Some(entry) = self.entries.get_mut(&placement.id) {
                if entry.bounds != placement.bounds {
                    let _ = entry.view.set_bounds(wry_rect(&placement.bounds));
                    entry.bounds = placement.bounds;
                }
                if !entry.visible {
                    let _ = entry.view.set_visible(true);
                    entry.visible = true;
                }
            } else if !self.failed.contains(&placement.id) {
                let id = placement.id;
                let load_events = events.clone();
                let focus_events = events.clone();
                let title_events = events.clone();
                let icon_events = events.clone();
                match WebViewBuilder::new()
                    .with_url(&placement.url)
                    .with_user_agent(USER_AGENT)
                    .with_bounds(wry_rect(&placement.bounds))
                    .with_visible(true)
                    .with_devtools(true)
                    .with_initialization_script(INIT_SCRIPT)
                    .with_ipc_handler(move |request| {
                        if request.body() == "pwrde:webview-focus" {
                            let _ = focus_events.send(TermEvent::WebviewFocused { id });
                        } else if let Some(report) = request.body().strip_prefix(FAVICON_IPC) {
                            let (origin, icon) = favicon_request(report).unzip();
                            let _ = icon_events.send(TermEvent::WebviewFaviconChanged {
                                id,
                                origin: origin.unwrap_or_default(),
                                icon,
                            });
                        }
                    })
                    .with_on_page_load_handler(move |event, url| {
                        if matches!(event, PageLoadEvent::Finished) {
                            let _ = load_events.send(TermEvent::WebviewNavigated { id, url });
                        }
                    })
                    .with_document_title_changed_handler(move |title| {
                        let _ = title_events.send(TermEvent::WebviewTitleChanged { id, title });
                    })
                    .build_as_child(window)
                {
                    Ok(view) => {
                        self.entries.insert(
                            placement.id,
                            Entry {
                                view,
                                bounds: placement.bounds,
                                visible: true,
                                zoom: 1.0,
                            },
                        );
                    }
                    Err(error) => {
                        let message = format!("{}: {error}", placement.url);
                        eprintln!("webview: failed to create {message}");
                        first_error.get_or_insert(message);
                        self.failed.insert(placement.id);
                    }
                }
            }
        }

        let focus = focus.filter(|id| visible.contains(id));
        if focus != self.focused {
            if let Some(previous) = self.focused.and_then(|id| self.entries.get(&id)) {
                let _ = previous.view.focus_parent();
            }
            if let Some(entry) = focus.and_then(|id| self.entries.get(&id)) {
                let _ = entry.view.focus();
            }
            self.focused = focus;
        }
        first_error
    }

    pub fn state(&self, id: u64) -> Option<BrowserState> {
        let entry = self.entries.get(&id)?;
        Some(BrowserState {
            can_go_back: entry.view.can_go_back().unwrap_or(false),
            can_go_forward: entry.view.can_go_forward().unwrap_or(false),
            zoom_percent: (entry.zoom * 100.0).round() as u16,
        })
    }

    fn entry(&self, id: u64) -> Result<&Entry, String> {
        self.entries
            .get(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    fn entry_mut(&mut self, id: u64) -> Result<&mut Entry, String> {
        self.entries
            .get_mut(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    pub fn navigate(&self, id: u64, url: &str) -> Result<(), String> {
        self.entry(id)?
            .view
            .load_url(url)
            .map_err(|e| format!("navigate: {e}"))
    }

    pub fn reload(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .reload()
            .map_err(|e| format!("reload: {e}"))
    }

    pub fn go_back(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .go_back()
            .map_err(|e| format!("back: {e}"))
    }

    pub fn go_forward(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .go_forward()
            .map_err(|e| format!("forward: {e}"))
    }

    pub fn set_zoom(&mut self, id: u64, zoom: f64) -> Result<u16, String> {
        let entry = self.entry_mut(id)?;
        let zoom = zoom.clamp(0.5, 3.0);
        entry.view.zoom(zoom).map_err(|e| format!("zoom: {e}"))?;
        entry.zoom = zoom;
        Ok((zoom * 100.0).round() as u16)
    }

    pub fn zoom(&mut self, id: u64, delta: f64) -> Result<u16, String> {
        let current = self.entry(id)?.zoom;
        self.set_zoom(id, current + delta)
    }

    pub fn print(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .print()
            .map_err(|e| format!("print: {e}"))
    }

    pub fn open_devtools(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.view.open_devtools();
        Ok(())
    }

    pub fn cookie_count(&self, id: u64, url: &str) -> Result<usize, String> {
        self.entry(id)?
            .view
            .cookies_for_url(url)
            .map(|cookies| cookies.len())
            .map_err(|e| format!("cookies: {e}"))
    }

    pub fn clear_browsing_data(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .clear_all_browsing_data()
            .map_err(|e| format!("clear browsing data: {e}"))
    }

    pub fn find(&self, id: u64, query: &str) -> Result<(), String> {
        let query = serde_json::to_string(query).map_err(|e| e.to_string())?;
        self.entry(id)?
            .view
            .evaluate_script(&format!("window.find({query}, false, false, true)"))
            .map_err(|e| format!("find in page: {e}"))
    }

    pub fn focus(&self, id: u64) {
        if let Some(entry) = self.entries.get(&id) {
            let _ = entry.view.focus();
        }
    }
}

fn wry_rect(rect: &LayoutRect) -> Rect {
    Rect {
        position: PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32).into(),
        size: PhysicalSize::new(
            rect.w.max(1.0).round() as u32,
            rect.h.max(1.0).round() as u32,
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wry_bounds_round_and_never_collapse_to_zero() {
        let rect = wry_rect(&LayoutRect {
            x: 10.6,
            y: 20.4,
            w: 0.0,
            h: 30.6,
        });
        assert_eq!(rect.position, PhysicalPosition::new(11, 20).into());
        assert_eq!(rect.size, PhysicalSize::new(1, 31).into());
    }

    #[test]
    fn child_bounds_reserve_scaled_browser_chrome() {
        let content = LayoutRect {
            x: 10.0,
            y: 20.0,
            w: 500.0,
            h: 400.0,
        };
        assert_eq!(
            child_bounds(content, 2.0, TOOLBAR_H),
            LayoutRect {
                x: 10.0,
                y: 116.0,
                w: 500.0,
                h: 304.0
            }
        );
        let tiny = child_bounds(content, 2.0, 10_000.0);
        assert_eq!(tiny.y, 419.0);
        assert_eq!(tiny.h, 1.0);
    }

    /// The toolbar follows the chrome text size, and the native view starts
    /// exactly below the scaled bar at any display scale.
    #[test]
    fn toolbar_height_scales_with_the_chrome_factor() {
        assert_eq!(toolbar_h_at(1.0), 48.0);
        assert_eq!(toolbar_h_at(1.25), 60.0);
        // The factor is capped at 1.5× (`workspace::chrome_ui_scale`).
        assert_eq!(toolbar_h_at(1.5), 72.0);
        let content = LayoutRect { x: 10.0, y: 20.0, w: 800.0, h: 600.0 };
        for (ui, scale) in [(1.25, 1.0), (1.25, 2.0), (1.5, 2.0)] {
            let bounds = child_bounds(content, scale, toolbar_h_at(ui));
            assert_eq!(bounds.y, content.y + TOOLBAR_H * ui * scale);
            assert_eq!(bounds.h, content.h - TOOLBAR_H * ui * scale);
            assert_eq!(bounds.y + bounds.h, content.y + content.h);
        }
    }

    #[test]
    fn child_bounds_hidden_toolbar_fills_content_at_any_scale() {
        let content = LayoutRect {
            x: 4.0,
            y: 6.0,
            w: 300.0,
            h: 200.0,
        };
        for scale in [0.5, 1.0, 2.0, 3.5] {
            let bounds = child_bounds(content, scale, 0.0);
            // Independent invariant: a hidden toolbar reserves nothing, so the
            // child view covers the content rect exactly.
            assert_eq!(bounds.x, content.x);
            assert_eq!(bounds.y, content.y);
            assert_eq!(bounds.w, content.w);
            assert_eq!(bounds.h, content.h);
        }
    }

    #[test]
    fn child_bounds_toolbar_never_eats_the_whole_tile() {
        let content = LayoutRect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 300.0,
        };
        // At scale 1 the toolbar reserves exactly its logical height.
        let bounds = child_bounds(content, 1.0, TOOLBAR_H);
        assert_eq!(bounds.y, TOOLBAR_H);
        assert_eq!(bounds.h, content.h - TOOLBAR_H);
        // On a tile shorter than the bar the reserve is clamped so the child
        // keeps 1px and the child view always ends at the content's bottom edge.
        let short = LayoutRect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 30.0,
        };
        let clamped = child_bounds(short, 1.0, TOOLBAR_H);
        assert_eq!(clamped.y, short.y + short.h - 1.0);
        assert_eq!(clamped.h, 1.0);
    }

    #[test]
    fn origin_keeps_scheme_and_authority_only() {
        assert_eq!(origin("https://github.com/a/b?c#d"), Some("https://github.com"));
        assert_eq!(origin("http://localhost:3000"), Some("http://localhost:3000"));
        assert_eq!(origin("https://example.com?q=1"), Some("https://example.com"));
        assert_eq!(origin("about:blank"), None);
        assert_eq!(origin("file:///tmp/x"), None);
        assert_eq!(origin("https://"), None);
        assert_eq!(origin("https://:80/"), None);
    }

    #[test]
    fn favicon_fetchable_is_http_only_and_shell_inert() {
        assert!(favicon_fetchable("https://example.com/favicon.ico"));
        assert!(favicon_fetchable("http://localhost:3000/icon.png?v=2"));
        assert!(!favicon_fetchable(""));
        assert!(!favicon_fetchable("data:image/png;base64,AAAA"));
        assert!(!favicon_fetchable("file:///etc/passwd"));
        assert!(!favicon_fetchable("-o /tmp/x https://example.com"));
        assert!(!favicon_fetchable("https://example.com/a b.ico"));
        assert!(!favicon_fetchable("https://example.com/a\u{0}.ico"));
        assert!(!favicon_fetchable(&format!("https://example.com/{}", "a".repeat(2048))));
    }

    #[test]
    fn favicon_request_prefers_page_link_then_origin_fallback() {
        assert_eq!(
            favicon_request("https://cdn.example.com/i.png\nhttps://example.com/docs"),
            Some(("https://example.com".into(), "https://cdn.example.com/i.png".into()))
        );
        // No link, or one the fetcher refuses: the origin's /favicon.ico.
        for href in ["", "data:image/png;base64,AAAA", "blob:https://example.com/1"] {
            assert_eq!(
                favicon_request(&format!("{href}\nhttp://example.com:8080/a?b")),
                Some((
                    "http://example.com:8080".into(),
                    "http://example.com:8080/favicon.ico".into()
                ))
            );
        }
        // A page that is not http(s) has no icon at all, whatever it claims.
        assert_eq!(favicon_request("https://example.com/i.png\nabout:blank"), None);
        assert_eq!(favicon_request("\nfile:///tmp/a.html"), None);
        assert_eq!(favicon_request("no newline"), None);
    }

    #[test]
    fn init_script_reports_focus_and_favicon() {
        assert!(INIT_SCRIPT.contains("'pwrde:webview-focus'"));
        assert!(INIT_SCRIPT.contains(&format!("'{FAVICON_IPC}'")));
        assert!(INIT_SCRIPT.contains(r#"link[rel~="icon"]"#));
        assert!(INIT_SCRIPT.contains(r"'\n'"));
    }

    #[test]
    fn decode_favicon_scales_down_and_rejects_non_images() {
        let encode = |edge: u32| {
            let img = image::RgbaImage::from_pixel(edge, edge, image::Rgba([10, 20, 30, 255]));
            let mut bytes = std::io::Cursor::new(Vec::new());
            img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            bytes.into_inner()
        };
        let (w, h, rgba) = decode_favicon(&encode(16)).unwrap();
        assert_eq!((w, h, rgba.len()), (FAVICON_EDGE, FAVICON_EDGE, 28 * 28 * 4));
        assert_eq!(&rgba[..4], &[10, 20, 30, 255]);
        let (w, h, rgba) = decode_favicon(&encode(256)).unwrap();
        assert_eq!((w, h, rgba.len()), (FAVICON_EDGE, FAVICON_EDGE, 28 * 28 * 4));
        // A transparent mark is inset: the canvas corner is bare disc.
        let mut mark = image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 0, 0]));
        for (x, y, px) in mark.enumerate_pixels_mut() {
            if (8..24).contains(&x) && (8..24).contains(&y) {
                *px = image::Rgba([0, 0, 0, 255]);
            }
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        mark.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let (_, _, rgba) = decode_favicon(&bytes.into_inner()).unwrap();
        assert_eq!(&rgba[..4], &[0xf2, 0xf2, 0xf7, 255]);
        // Transparency is flattened: a light mark gets the dark disc, a dark
        // one the light disc.
        let mut white = image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 255, 255, 255]));
        white.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        assert_eq!(flatten_on_disc(&mut white), ([0x1c, 0x1c, 0x1e], true));
        assert_eq!(white.get_pixel(1, 0).0, [0x1c, 0x1c, 0x1e, 255]);
        let mut black = image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 255]));
        black.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        flatten_on_disc(&mut black);
        assert_eq!(black.get_pixel(1, 0).0, [0xf2, 0xf2, 0xf7, 255]);
        assert!(decode_favicon(b"").is_none());
        assert!(decode_favicon(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_none());
        assert!(decode_favicon(b"<!doctype html><title>404</title>").is_none());
    }

    #[test]
    fn user_agent_is_safari_not_chrome() {
        // Google's sign-in refuses a Chrome UA on a WebKit engine, while the
        // WKWebView default (no Version/ or Safari/ token) gets legacy layouts.
        assert!(USER_AGENT.contains(" Version/"));
        assert!(USER_AGENT.contains(" Safari/"));
        assert!(USER_AGENT.contains("AppleWebKit/605"));
        assert!(!USER_AGENT.contains("Chrome/"));
    }

    #[test]
    fn browser_input_accepts_hosts_and_rejects_other_schemes() {
        assert_eq!(
            normalize_input(" example.com/docs ").unwrap(),
            "https://example.com/docs"
        );
        assert_eq!(
            normalize_input("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert!(normalize_input("file:///tmp/nope").is_err());
    }
}
