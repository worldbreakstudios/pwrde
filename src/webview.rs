//! Main-thread lifecycle of the Chromium browsers behind webview tabs.
//!
//! Each tab's page is a CEF (Chromium Embedded Framework) *windowed child
//! browser*: Chromium creates an `NSView` for it under the gpui window's own
//! view, and [`Manager::sync`] reconciles those views with the tab model every
//! frame — create, place, show / hide, close — and hands the keyboard between
//! the focused browser and gpui's view (the terminals). `cef_app.rs` owns
//! CEF itself (initialization, the message pump, shutdown, where the
//! framework and helper live); nothing here runs before it reports ready.
//!
//! Page events come from CEF handlers (the private `handlers` module):
//! `DisplayHandler::on_title_change`, `on_favicon_urlchange` and
//! `LoadHandler::on_load_end` become the [`TermEvent`]s `main.rs` folds into
//! the tab, a click's `FocusHandler::on_got_focus` moves pwrde's focus to the
//! tab's tile, and `LifeSpanHandler::on_before_popup` keeps `target=_blank`
//! in the tab. Chromium calls them all on its UI thread, which is this one;
//! they only send over the channel.
//!
//! Also the favicon plumbing for web tabs: Chromium's icon candidates and the
//! page URL go through [`favicon_request`] (validation, and the
//! `<origin>/favicon.ico` fallback) into a
//! [`TermEvent::WebviewFaviconChanged`], and [`fetch_favicon`] — called from a
//! background thread only — downloads and decodes the icon for the tab strips.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use cef::{
    ImplBrowser, ImplBrowserHost, ImplCookieManager, ImplDictionaryValue, ImplFrame,
    ImplRequestContext,
};
use gpui::{RenderImage, Window};
use objc::runtime::{BOOL, NO, Object, YES};
use objc::{class, msg_send, sel, sel_impl};

use crate::term::TermEvent;
use crate::workspace::LayoutRect;

/// Browser chrome is GPUI-owned; the native child starts below it. This is
/// the toolbar's height at the default chrome text size — the live height is
/// [`toolbar_h`].
pub const TOOLBAR_H: f32 = 48.0;

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

/// Parse a favicon report — `<icon href>\n<page URL>`, the href empty when
/// the page names no usable icon — into the page's
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
        .args(["--", url])
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

/// A view frame in the parent `NSView`'s own coordinates (logical points).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewFrame {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Round a physical-pixel rect onto the pixel grid; a view never collapses
/// to zero size.
fn pixel_rect(rect: &LayoutRect) -> (i32, i32, u32, u32) {
    (
        rect.x.round() as i32,
        rect.y.round() as i32,
        rect.w.max(1.0).round() as u32,
        rect.h.max(1.0).round() as u32,
    )
}

/// The child view's frame for a placement in physical pixels (top-left
/// origin, as the layout computes it): snapped to the pixel grid, converted
/// to points at the window's `scale`, and — in a parent that is not flipped —
/// measured from the parent's bottom edge, `parent_h` points tall.
fn view_frame(bounds: &LayoutRect, scale: f32, parent_h: f64, flipped: bool) -> ViewFrame {
    let (x, y, w, h) = pixel_rect(bounds);
    let scale = if scale > 0.0 { scale as f64 } else { 1.0 };
    let (x, y, w, h) = (x as f64 / scale, y as f64 / scale, w as f64 / scale, h as f64 / scale);
    ViewFrame { x, y: if flipped { y } else { parent_h - y - h }, w, h }
}

/// Chromium zoom levels are logarithmic: each level is a factor of 1.2, and
/// level 0 is 100 %.
const ZOOM_LEVEL_BASE: f64 = 1.2;

/// The CEF zoom level for a zoom factor (`1.0` = 100 %).
fn zoom_level(factor: f64) -> f64 {
    factor.max(f64::MIN_POSITIVE).ln() / ZOOM_LEVEL_BASE.ln()
}

/// The zoom factor a CEF zoom level stands for.
fn zoom_factor(level: f64) -> f64 {
    ZOOM_LEVEL_BASE.powf(level)
}

/// A zoom factor as the whole percent the Tools popover shows.
fn zoom_percent(factor: f64) -> u16 {
    (factor * 100.0).round() as u16
}

/// The icon link to report for a page: the first of Chromium's candidates
/// that is not an SVG (the decoder cannot read one), empty when there is
/// none — [`favicon_request`] then falls back to `<origin>/favicon.ico`.
fn pick_icon(candidates: &[String]) -> &str {
    candidates
        .iter()
        .map(String::as_str)
        .find(|url| {
            let path = url.split(['?', '#']).next().unwrap_or(url);
            !url.is_empty() && !path.to_ascii_lowercase().ends_with(".svg")
        })
        .unwrap_or_default()
}

/// The focus gate's value while no webview holds pwrde's keyboard focus.
const NO_FOCUS: u64 = u64::MAX;

/// The cookie count Chromium last reported for each tab's page, by webview
/// id. Chromium's cookie store only answers asynchronously, and its message
/// loop must not be pumped from inside a gpui callback (see `cef_app::pump`),
/// so the count is kept warm instead: recounted whenever a page finishes
/// loading and whenever it is read.
type CookieCounts = Arc<Mutex<HashMap<u64, usize>>>;

/// One running count: visits add up, and the last reference going away —
/// Chromium releases the visitor after the final cookie, or at once when
/// there are none — publishes the total.
struct CookieTally {
    id: u64,
    seen: AtomicUsize,
    counts: CookieCounts,
}

impl Drop for CookieTally {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.counts.lock() {
            counts.insert(self.id, self.seen.load(Ordering::Relaxed));
        }
    }
}

/// Start counting the cookies Chromium would send to `url` for the tab `id`.
/// A page that is not http(s) has none.
fn count_cookies(id: u64, url: &str, counts: &CookieCounts) -> Result<(), String> {
    if origin(url).is_none() {
        if let Ok(mut counts) = counts.lock() {
            counts.insert(id, 0);
        }
        return Ok(());
    }
    let manager = cef::cookie_manager_get_global_manager(None)
        .ok_or("cookies: the cookie store is unavailable")?;
    let tally = Arc::new(CookieTally { id, seen: AtomicUsize::new(0), counts: counts.clone() });
    let mut visitor = handlers::cookie_counter(tally);
    if manager.visit_url_cookies(Some(&cef::CefString::from(url)), 1, Some(&mut visitor)) != 1 {
        return Err("cookies: the cookie store is unavailable".into());
    }
    Ok(())
}

/// The CEF handlers behind one browser. Chromium calls every one of them on
/// its UI thread — pwrde's main thread — and each only sends a [`TermEvent`]
/// (or flips an atomic), so none would be unsafe to call from elsewhere. The
/// wrap macros resolve cef's types unqualified, hence the glob import in a
/// module of its own.
mod handlers {
    use std::os::raw::c_int;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::Sender;

    use cef::*;

    use crate::term::TermEvent;

    fn frame_url(frame: &Frame) -> String {
        CefString::from(&frame.url()).to_string()
    }

    wrap_display_handler! {
        pub struct PageDisplay {
            id: u64,
            events: Sender<TermEvent>,
            icon_seen: Arc<AtomicBool>,
        }

        impl DisplayHandler {
            fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
                let title = title.map(|title| title.to_string()).unwrap_or_default();
                let _ = self.events.send(TermEvent::WebviewTitleChanged { id: self.id, title });
            }

            fn on_favicon_urlchange(
                &self,
                browser: Option<&mut Browser>,
                icon_urls: Option<&mut CefStringList>,
            ) {
                let candidates: Vec<String> =
                    icon_urls.map(|urls| urls.clone().into_iter().collect()).unwrap_or_default();
                let page = browser
                    .and_then(|browser| browser.main_frame())
                    .map(|frame| frame_url(&frame))
                    .unwrap_or_default();
                self.icon_seen.store(true, Ordering::Relaxed);
                let report = format!("{}\n{page}", super::pick_icon(&candidates));
                let (origin, icon) = super::favicon_request(&report).unzip();
                let _ = self.events.send(TermEvent::WebviewFaviconChanged {
                    id: self.id,
                    origin: origin.unwrap_or_default(),
                    icon,
                });
            }
        }
    }

    wrap_load_handler! {
        pub struct PageLoad {
            id: u64,
            events: Sender<TermEvent>,
            icon_seen: Arc<AtomicBool>,
            cookies: super::CookieCounts,
        }

        impl LoadHandler {
            fn on_load_start(
                &self,
                _browser: Option<&mut Browser>,
                frame: Option<&mut Frame>,
                _transition_type: TransitionType,
            ) {
                if frame.is_some_and(|frame| frame.is_main() == 1) {
                    self.icon_seen.store(false, Ordering::Relaxed);
                }
            }

            fn on_load_end(
                &self,
                _browser: Option<&mut Browser>,
                frame: Option<&mut Frame>,
                _http_status_code: c_int,
            ) {
                let Some(frame) = frame.filter(|frame| frame.is_main() == 1) else { return };
                let url = frame_url(frame);
                // Keep the Site popover's cookie count warm.
                let _ = super::count_cookies(self.id, &url, &self.cookies);
                // A page Chromium reported no icon for by the time it loaded
                // gets the origin's /favicon.ico (or, off http(s), none).
                if !self.icon_seen.load(Ordering::Relaxed) {
                    let (origin, icon) = super::favicon_request(&format!("\n{url}")).unzip();
                    let _ = self.events.send(TermEvent::WebviewFaviconChanged {
                        id: self.id,
                        origin: origin.unwrap_or_default(),
                        icon,
                    });
                }
                let _ = self.events.send(TermEvent::WebviewNavigated { id: self.id, url });
            }
        }
    }

    wrap_life_span_handler! {
        // `page`: a tab's page (`true`) or its DevTools window (`false`).
        pub struct PageLifeSpan {
            page: bool,
        }

        impl LifeSpanHandler {
            fn on_before_popup(
                &self,
                browser: Option<&mut Browser>,
                _frame: Option<&mut Frame>,
                _popup_id: c_int,
                target_url: Option<&CefString>,
                _target_frame_name: Option<&CefString>,
                _target_disposition: WindowOpenDisposition,
                _user_gesture: c_int,
                _popup_features: Option<&PopupFeatures>,
                _window_info: Option<&mut WindowInfo>,
                _client: Option<&mut Option<Client>>,
                _settings: Option<&mut BrowserSettings>,
                _extra_info: Option<&mut Option<DictionaryValue>>,
                _no_javascript_access: Option<&mut c_int>,
            ) -> c_int {
                if !self.page {
                    return 0;
                }
                // A tab has no window to pop out of: `target=_blank` and
                // `window.open` load in the tab itself.
                if let Some(frame) = browser.and_then(|browser| browser.main_frame()) {
                    frame.load_url(target_url);
                }
                1
            }

            fn do_close(&self, browser: Option<&mut Browser>) -> c_int {
                if !self.page {
                    return 0;
                }
                // Chromium's default close sends `performClose:` to the
                // browser's top-level window — pwrde's main window, which
                // would quit the app with the tab. Take the close over: only
                // the browser's own view goes, and with it the browser.
                if let Some(browser) = browser {
                    super::detach_view(browser);
                }
                1
            }

            fn on_after_created(&self, _browser: Option<&mut Browser>) {
                crate::cef_app::browser_opened();
            }

            fn on_before_close(&self, _browser: Option<&mut Browser>) {
                crate::cef_app::browser_closed();
            }
        }
    }

    wrap_focus_handler! {
        pub struct PageFocus {
            id: u64,
            events: Sender<TermEvent>,
            focus_gate: Arc<AtomicU64>,
        }

        impl FocusHandler {
            fn on_set_focus(&self, _browser: Option<&mut Browser>, source: FocusSource) -> c_int {
                // A load finishing in a tile that does not hold pwrde's focus
                // must not pull the keyboard out of a terminal; a click
                // (`SYSTEM`) always may.
                let denied = source == FocusSource::NAVIGATION
                    && self.focus_gate.load(Ordering::Relaxed) != self.id;
                denied as c_int
            }

            fn on_got_focus(&self, _browser: Option<&mut Browser>) {
                let _ = self.events.send(TermEvent::WebviewFocused { id: self.id });
            }
        }
    }

    wrap_client! {
        pub struct PageClient {
            display: Option<DisplayHandler>,
            load: Option<LoadHandler>,
            life_span: LifeSpanHandler,
            focus: Option<FocusHandler>,
        }

        impl Client {
            fn display_handler(&self) -> Option<DisplayHandler> {
                self.display.clone()
            }

            fn load_handler(&self) -> Option<LoadHandler> {
                self.load.clone()
            }

            fn life_span_handler(&self) -> Option<LifeSpanHandler> {
                Some(self.life_span.clone())
            }

            fn focus_handler(&self) -> Option<FocusHandler> {
                self.focus.clone()
            }
        }
    }

    wrap_cookie_visitor! {
        pub struct CookieCounter {
            tally: Arc<super::CookieTally>,
        }

        impl CookieVisitor {
            fn visit(
                &self,
                _cookie: Option<&Cookie>,
                _count: c_int,
                _total: c_int,
                _delete_cookie: Option<&mut c_int>,
            ) -> c_int {
                self.tally.seen.fetch_add(1, Ordering::Relaxed);
                1
            }
        }
    }

    /// The client of the tab `id`'s browser.
    pub fn page_client(
        id: u64,
        events: &Sender<TermEvent>,
        focus_gate: &Arc<AtomicU64>,
        cookies: &super::CookieCounts,
    ) -> Client {
        let icon_seen = Arc::new(AtomicBool::new(false));
        PageClient::new(
            Some(PageDisplay::new(id, events.clone(), icon_seen.clone())),
            Some(PageLoad::new(id, events.clone(), icon_seen, cookies.clone())),
            PageLifeSpan::new(true),
            Some(PageFocus::new(id, events.clone(), focus_gate.clone())),
        )
    }

    /// The client of a DevTools window: counted for shutdown, nothing else.
    pub fn devtools_client() -> Client {
        PageClient::new(None, None, PageLifeSpan::new(false), None)
    }

    pub fn cookie_counter(tally: Arc<super::CookieTally>) -> CookieVisitor {
        CookieCounter::new(tally)
    }
}

/// Remove a closing browser's `NSView` from gpui's view. Chromium destroys
/// the browser when its host view is deallocated; the removal is posted to
/// the main run loop so it lands after the `do_close` callback returns.
fn detach_view(browser: &cef::Browser) {
    let Some(host) = browser.host() else { return };
    let view = host.window_handle() as *mut Object;
    if view.is_null() {
        return;
    }
    let nil: *mut Object = std::ptr::null_mut();
    let _: () = unsafe {
        msg_send![view, performSelectorOnMainThread: sel!(removeFromSuperview)
                                         withObject: nil
                                      waitUntilDone: NO]
    };
}

/// gpui's `NSView` behind `window`: the parent of every browser view.
fn parent_view(window: &Window) -> Option<*mut Object> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Explicit trait call: gpui's `Window` has an inherent `window_handle()`.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
    Some(appkit.ns_view.as_ptr() as *mut Object)
}

/// The frame `bounds` maps to inside `parent` right now.
fn frame_in(parent: *mut Object, bounds: &LayoutRect, scale: f32) -> ViewFrame {
    let flipped: BOOL = unsafe { msg_send![parent, isFlipped] };
    let parent_bounds: ViewFrame = unsafe { msg_send![parent, bounds] };
    view_frame(bounds, scale, parent_bounds.h, flipped != NO)
}

struct Entry {
    browser: cef::Browser,
    /// gpui's view, which takes the keyboard back when the browser loses it.
    parent: *mut Object,
    frame: ViewFrame,
    visible: bool,
    /// The last find-in-page query, so a repeat steps to the next match.
    last_find: RefCell<String>,
}

impl Entry {
    fn host(&self) -> Result<cef::BrowserHost, String> {
        self.browser.host().ok_or_else(|| "webview is closing".into())
    }

    /// The browser's own `NSView`, a child of [`Entry::parent`].
    fn view(&self) -> Option<*mut Object> {
        let view = self.browser.host()?.window_handle() as *mut Object;
        (!view.is_null()).then_some(view)
    }

    fn set_frame(&mut self, frame: ViewFrame) {
        if let Some(view) = self.view() {
            let _: () = unsafe { msg_send![view, setFrame: frame] };
        }
        self.frame = frame;
    }

    fn set_visible(&mut self, visible: bool) {
        if let Some(view) = self.view() {
            let hidden = if visible { NO } else { YES };
            let _: () = unsafe { msg_send![view, setHidden: hidden] };
        }
        self.visible = visible;
    }

    fn focus(&self) {
        if let Ok(host) = self.host() {
            host.set_focus(1);
        }
    }

    /// Hand the keyboard back to gpui's view — the terminal side — when the
    /// window's first responder is this browser (or a view inside it).
    fn focus_parent(&self) {
        let Some(view) = self.view() else { return };
        if let Ok(host) = self.host() {
            host.set_focus(0);
        }
        unsafe {
            let window: *mut Object = msg_send![self.parent, window];
            if window.is_null() {
                return;
            }
            let responder: *mut Object = msg_send![window, firstResponder];
            if responder.is_null() {
                return;
            }
            let is_view: BOOL = msg_send![responder, isKindOfClass: class!(NSView)];
            if is_view == NO {
                return;
            }
            let inside: BOOL = msg_send![responder, isDescendantOf: view];
            if inside != NO {
                let _: BOOL = msg_send![window, makeFirstResponder: self.parent];
            }
        }
    }

    /// Ask Chromium to destroy the browser (and its DevTools window). The
    /// view goes with it; hiding it first keeps a closed tab from lingering
    /// on screen until the asynchronous close lands.
    fn close(mut self) {
        self.focus_parent();
        self.set_visible(false);
        if let Ok(host) = self.host() {
            host.close_dev_tools();
            host.close_browser(1);
        }
    }

    fn url(&self) -> Option<String> {
        let frame = self.browser.main_frame()?;
        Some(cef::CefString::from(&frame.url()).to_string())
    }
}

#[derive(Clone, Debug)]
pub struct BrowserState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom_percent: u16,
}

pub struct Manager {
    entries: HashMap<u64, Entry>,
    failed: HashSet<u64>,
    focused: Option<u64>,
    /// [`Manager::focused`] for the focus handlers ([`NO_FOCUS`] when none).
    focus_gate: Arc<AtomicU64>,
    cookies: CookieCounts,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            failed: HashSet::new(),
            focused: None,
            focus_gate: Arc::new(AtomicU64::new(NO_FOCUS)),
            cookies: CookieCounts::default(),
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.close_all();
    }
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
        // `Action::Quit` has closed every browser; the tabs are still in the
        // model, and must not be given new ones in the frames before exit.
        if crate::cef_app::quit_requested() {
            return None;
        }
        let mut first_error = None;
        let visible: HashSet<u64> = placements.iter().map(|p| p.id).collect();
        let gone: Vec<u64> = self.entries.keys().copied().filter(|id| !live.contains(id)).collect();
        for id in gone {
            if let Some(entry) = self.entries.remove(&id) {
                entry.close();
            }
            if let Ok(mut cookies) = self.cookies.lock() {
                cookies.remove(&id);
            }
        }
        if self
            .focused
            .is_some_and(|id| !self.entries.contains_key(&id))
        {
            self.set_focused(None);
        }
        // Suppress repeated failures while a tab stays visible, but retry after
        // the user switches away and revisits it.
        self.failed
            .retain(|id| live.contains(id) && visible.contains(id));

        for (id, entry) in &mut self.entries {
            if !visible.contains(id) && entry.visible {
                entry.focus_parent();
                entry.set_visible(false);
            }
        }

        let parent = parent_view(window);
        let scale = window.scale_factor();
        for placement in placements {
            if let Some(entry) = self.entries.get_mut(&placement.id) {
                let frame = frame_in(entry.parent, &placement.bounds, scale);
                if entry.frame != frame {
                    entry.set_frame(frame);
                }
                if !entry.visible {
                    entry.set_visible(true);
                }
            } else if !self.failed.contains(&placement.id) {
                let created = match parent {
                    Some(parent) => {
                        create(parent, placement, scale, events, &self.focus_gate, &self.cookies)
                    }
                    None => Err("the window has no native view".to_string()),
                };
                match created {
                    Ok(entry) => {
                        self.entries.insert(placement.id, entry);
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
                previous.focus_parent();
            }
            // The gate opens before the browser is asked to take focus.
            self.set_focused(focus);
            if let Some(entry) = focus.and_then(|id| self.entries.get(&id)) {
                entry.focus();
            }
        }
        first_error
    }

    fn set_focused(&mut self, focus: Option<u64>) {
        self.focused = focus;
        self.focus_gate.store(focus.unwrap_or(NO_FOCUS), Ordering::Relaxed);
    }

    /// Close every browser, for quit: `cef_app::shutdown` then waits for the
    /// closes to land. Also what dropping the manager does.
    pub fn close_all(&mut self) {
        for (_, entry) in self.entries.drain() {
            entry.close();
        }
        if let Ok(mut cookies) = self.cookies.lock() {
            cookies.clear();
        }
        self.set_focused(None);
    }

    pub fn state(&self, id: u64) -> Option<BrowserState> {
        let entry = self.entries.get(&id)?;
        Some(BrowserState {
            can_go_back: entry.browser.can_go_back() == 1,
            can_go_forward: entry.browser.can_go_forward() == 1,
            // Read live: Chromium keeps zoom per site, so a navigation (or
            // another tab on the same site) can change it under the tab.
            zoom_percent: entry
                .host()
                .map_or(100, |host| zoom_percent(zoom_factor(host.zoom_level()))),
        })
    }

    fn entry(&self, id: u64) -> Result<&Entry, String> {
        self.entries
            .get(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    pub fn navigate(&self, id: u64, url: &str) -> Result<(), String> {
        let frame = self.entry(id)?.browser.main_frame().ok_or("navigate: the page has no frame")?;
        frame.load_url(Some(&cef::CefString::from(url)));
        Ok(())
    }

    pub fn reload(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.browser.reload();
        Ok(())
    }

    pub fn go_back(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.browser.go_back();
        Ok(())
    }

    pub fn go_forward(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.browser.go_forward();
        Ok(())
    }

    /// Set the zoom factor (`1.0` = 100 %, clamped to 50–300 %) and return
    /// the resulting percent.
    pub fn set_zoom(&mut self, id: u64, zoom: f64) -> Result<u16, String> {
        let host = self.entry(id)?.host().map_err(|e| format!("zoom: {e}"))?;
        let zoom = zoom.clamp(0.5, 3.0);
        host.set_zoom_level(zoom_level(zoom));
        Ok(zoom_percent(zoom))
    }

    pub fn zoom(&mut self, id: u64, delta: f64) -> Result<u16, String> {
        let host = self.entry(id)?.host().map_err(|e| format!("zoom: {e}"))?;
        // Step from the whole percent on show, so repeated steps do not drift.
        let current = zoom_percent(zoom_factor(host.zoom_level())) as f64 / 100.0;
        self.set_zoom(id, current + delta)
    }

    pub fn print(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.host().map_err(|e| format!("print: {e}"))?.print();
        Ok(())
    }

    /// Open Chromium's DevTools for the page in a window of their own.
    pub fn open_devtools(&self, id: u64) -> Result<(), String> {
        let host = self.entry(id)?.host().map_err(|e| format!("developer tools: {e}"))?;
        let mut client = handlers::devtools_client();
        host.show_dev_tools(
            Some(&cef::WindowInfo::default()),
            Some(&mut client),
            Some(&cef::BrowserSettings::default()),
            None,
        );
        Ok(())
    }

    /// The cookies Chromium would send to `url`, as last counted for the
    /// tab's page — at its last load, or at the previous call — and start a
    /// fresh count, which [`Manager::cookies_seen`] picks up once Chromium
    /// answers. `Err` until the first count has landed.
    pub fn cookie_count(&self, id: u64, url: &str) -> Result<usize, String> {
        self.entry(id)?;
        let seen = self.cookies_seen(id);
        count_cookies(id, url, &self.cookies)?;
        seen.ok_or_else(|| "cookies: not counted yet".into())
    }

    /// The latest finished cookie count for the tab's page, without starting
    /// another: what an open Site popover re-reads as it renders.
    pub fn cookies_seen(&self, id: u64) -> Option<usize> {
        self.cookies.lock().ok()?.get(&id).copied()
    }

    /// Clear the shared profile: every cookie, the HTTP cache, and — through
    /// the DevTools protocol, which works per origin — the site storage
    /// (localStorage, IndexedDB, service workers, cache storage) of every
    /// origin open in a webview tab right now. CEF has no call that drops
    /// site storage for origins it is not showing.
    pub fn clear_browsing_data(&self, id: u64) -> Result<(), String> {
        let host = self.entry(id)?.host().map_err(|e| format!("clear browsing data: {e}"))?;
        let cookies = cef::cookie_manager_get_global_manager(None)
            .ok_or("clear browsing data: the cookie store is unavailable")?;
        if cookies.delete_cookies(None, None, None) != 1 {
            return Err("clear browsing data: the cookie store is unavailable".into());
        }
        cookies.flush_store(None);
        if let Some(context) = host.request_context() {
            context.clear_http_cache(None);
        }
        let mut cleared = HashSet::new();
        for entry in self.entries.values() {
            let Some(url) = entry.url() else { continue };
            let Some(origin) = origin(&url) else { continue };
            if !cleared.insert(origin.to_string()) {
                continue;
            }
            let (Ok(host), Some(mut params)) = (entry.host(), cef::dictionary_value_create()) else {
                continue;
            };
            let text = |value: &str| cef::CefString::from(value);
            params.set_string(Some(&text("origin")), Some(&text(origin)));
            params.set_string(Some(&text("storageTypes")), Some(&text("all")));
            host.execute_dev_tools_method(
                0,
                Some(&text("Storage.clearDataForOrigin")),
                Some(&mut params),
            );
        }
        Ok(())
    }

    /// Find `query` in the page; the same query again steps to the next
    /// match, and an empty one clears the highlight.
    pub fn find(&self, id: u64, query: &str) -> Result<(), String> {
        let entry = self.entry(id)?;
        let host = entry.host().map_err(|e| format!("find in page: {e}"))?;
        let mut last = entry.last_find.borrow_mut();
        if query.is_empty() {
            host.stop_finding(1);
            last.clear();
            return Ok(());
        }
        let next = *last == query;
        host.find(Some(&cef::CefString::from(query)), 1, 0, next as _);
        *last = query.to_string();
        Ok(())
    }

    pub fn focus(&self, id: u64) {
        if let Some(entry) = self.entries.get(&id) {
            entry.focus();
        }
    }
}

/// Create the browser for `placement` as a windowed child of `parent`.
fn create(
    parent: *mut Object,
    placement: &Placement,
    scale: f32,
    events: &Sender<TermEvent>,
    focus_gate: &Arc<AtomicU64>,
    cookies: &CookieCounts,
) -> Result<Entry, String> {
    crate::cef_app::ready()?;
    let frame = frame_in(parent, &placement.bounds, scale);
    let window_info = cef::WindowInfo {
        parent_view: parent.cast(),
        bounds: cef::Rect {
            x: frame.x.round() as i32,
            y: frame.y.round() as i32,
            width: frame.w.round().max(1.0) as i32,
            height: frame.h.round().max(1.0) as i32,
        },
        // A native-parented child view; Chrome style would bring its own UI.
        runtime_style: cef::RuntimeStyle::ALLOY,
        ..Default::default()
    };
    let mut client = handlers::page_client(placement.id, events, focus_gate, cookies);
    let browser = cef::browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&cef::CefString::from(placement.url.as_str())),
        Some(&cef::BrowserSettings::default()),
        None,
        None,
    )
    .ok_or("Chromium could not create the browser")?;
    let mut entry =
        Entry { browser, parent, frame, visible: true, last_find: RefCell::new(String::new()) };
    if let Some(view) = entry.view() {
        // pwrde places the view itself every frame; AppKit must not stretch
        // it with the window in between (NSViewNotSizable).
        let _: () = unsafe { msg_send![view, setAutoresizingMask: 0usize] };
        // gpui's view hosts its own Metal layer, and AppKit only composites
        // the subviews of a layer-hosting view that carry layers themselves.
        let _: () = unsafe { msg_send![view, setWantsLayer: YES] };
    }
    // The exact (fractional) frame; `WindowInfo` only carries whole points.
    entry.set_frame(frame);
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_rect_rounds_and_never_collapses_to_zero() {
        let rect = pixel_rect(&LayoutRect { x: 10.6, y: 20.4, w: 0.0, h: 30.6 });
        assert_eq!(rect, (11, 20, 1, 31));
    }

    #[test]
    fn view_frame_converts_pixels_to_points_in_either_orientation() {
        let bounds = LayoutRect { x: 20.0, y: 100.0, w: 800.0, h: 600.0 };
        // A flipped parent shares the layout's top-left origin.
        assert_eq!(
            view_frame(&bounds, 2.0, 720.0, true),
            ViewFrame { x: 10.0, y: 50.0, w: 400.0, h: 300.0 }
        );
        // An unflipped one measures from its bottom edge.
        assert_eq!(
            view_frame(&bounds, 2.0, 720.0, false),
            ViewFrame { x: 10.0, y: 370.0, w: 400.0, h: 300.0 }
        );
        // Odd pixels stay on the pixel grid as half points at 2x.
        let odd = view_frame(&LayoutRect { x: 1.4, y: 3.0, w: 5.0, h: 7.0 }, 2.0, 100.0, true);
        assert_eq!(odd, ViewFrame { x: 0.5, y: 1.5, w: 2.5, h: 3.5 });
        // A nonsense scale is treated as 1x rather than dividing by zero.
        assert_eq!(view_frame(&bounds, 0.0, 720.0, true).w, 800.0);
    }

    #[test]
    fn zoom_percent_and_cef_zoom_level_round_trip() {
        // Level 0 is 100 %, and each level is a factor of 1.2.
        assert_eq!(zoom_level(1.0), 0.0);
        assert!((zoom_level(1.2) - 1.0).abs() < 1e-9);
        assert!((zoom_level(1.44) - 2.0).abs() < 1e-9);
        assert!(zoom_level(0.5) < 0.0);
        assert!((zoom_factor(-1.0) - 1.0 / 1.2).abs() < 1e-9);
        // Every percent the stepper can land on survives the conversion.
        for percent in (50..=300).step_by(5) {
            let factor = percent as f64 / 100.0;
            assert_eq!(zoom_percent(zoom_factor(zoom_level(factor))), percent as u16);
        }
        assert!(zoom_level(0.0).is_finite());
    }

    #[test]
    fn pick_icon_skips_svg_and_empty_candidates() {
        let urls = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_icon(&urls(&["https://a.test/i.svg", "https://a.test/i.SVG?v=2", "https://a.test/i.png"])),
            "https://a.test/i.png"
        );
        assert_eq!(pick_icon(&urls(&["", "https://a.test/favicon.ico#x"])), "https://a.test/favicon.ico#x");
        assert_eq!(pick_icon(&urls(&["https://a.test/i.svg"])), "");
        assert_eq!(pick_icon(&[]), "");
        // An empty pick is what makes `favicon_request` fall back.
        assert_eq!(
            favicon_request(&format!("{}\nhttps://a.test/docs", pick_icon(&[]))),
            Some(("https://a.test".into(), "https://a.test/favicon.ico".into()))
        );
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
