//! Main-thread lifecycle of web tabs: one off-screen Chromium page per tab.
//!
//! [`Manager::sync`] reconciles the pages with the tab model — creating a
//! page for a newly visible tab, resizing, hiding (`was_hidden`) and closing
//! them — and the rest of [`Manager`] is what the browser chrome drives:
//! navigation, zoom, find, print, DevTools, cookies. The engine itself lives
//! in [`crate::webview_cef`]; nothing here calls CEF directly.
//!
//! Chromium renders each page off-screen. [`Manager::page_images`] turns the
//! frames that changed into `RenderImage`s the terminal `Element` paints at
//! the placement's bounds ([`child_bounds`]), and the pointer / key methods
//! forward gpui input to the page under the pointer or with focus.
//!
//! Also the favicon plumbing for web tabs: Chromium reports each page's icon
//! links ([`favicon_choice`] picks one, [`favicon_request`] validates it into
//! a [`TermEvent::WebviewFaviconChanged`]), and [`fetch_favicon`] — called
//! from a background thread only — downloads and decodes it for the tab
//! strips.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use gpui::{RenderImage, Window};

use crate::term::TermEvent;
use crate::webview_cef::{self as engine, Button, EditCommand, Host, Mods};
use crate::workspace::LayoutRect;

/// Browser chrome is GPUI-owned; the page starts below it. This is the
/// toolbar's height at the default chrome text size — the live height is
/// [`toolbar_h`].
pub const TOOLBAR_H: f32 = 48.0;

/// The user agent the favicon fetcher presents: the reduced Chrome string
/// the pages themselves see from the engine, so an icon host answers curl as
/// it would the tab.
const FETCH_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36";

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

/// The icon to ask for among the links a page declared (Chromium's
/// `on_favicon_urlchange` list, in document order): the first one the fetcher
/// accepts that is not an SVG, which the decoder cannot read. Empty when
/// there is none, which [`favicon_request`] turns into `/favicon.ico`.
pub fn favicon_choice(icons: &[String]) -> &str {
    icons
        .iter()
        .map(String::as_str)
        .find(|href| {
            let path = href.split(['?', '#']).next().unwrap_or(href);
            favicon_fetchable(href) && !path.to_ascii_lowercase().ends_with(".svg")
        })
        .unwrap_or("")
}

/// Parse a favicon report (`<icon href>\n<page URL>`) into the page's
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
        .args(["-A", FETCH_USER_AGENT, "--", url])
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
/// tall and `sync_webviews` reserves the same height above the page.
pub fn toolbar_h() -> f32 {
    toolbar_h_at(crate::workspace::chrome_ui_scale())
}

/// [`toolbar_h`] at chrome factor `ui`. Pure, so tests can pin it.
fn toolbar_h_at(ui: f32) -> f32 {
    TOOLBAR_H * ui
}

/// Convert the tile content rect (physical pixels) into the page rect,
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
    host: Host,
    bounds: LayoutRect,
    visible: bool,
    /// The last frame Chromium painted, as gpui paints it, and its size in
    /// device pixels.
    image: Option<(Arc<RenderImage>, u32, u32)>,
}

#[derive(Clone, Debug)]
pub struct BrowserState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom_percent: u16,
}

/// One visible page for the terminal `Element` to paint: `image` is
/// `w`×`h` device pixels and goes 1:1 at the top-left of `bounds` (physical
/// pixels), clipped to it.
pub struct PageImage {
    pub bounds: LayoutRect,
    pub image: Arc<RenderImage>,
    pub w: u32,
    pub h: u32,
}

/// A gpui image over one BGRA frame. gpui's `RenderImage` frames are BGRA
/// already (see `renderer::render_image`), and the page is opaque, so
/// Chromium's buffer goes in as is.
fn page_image(w: u32, h: u32, bgra: Vec<u8>) -> Option<RenderImage> {
    let buffer = image::RgbaImage::from_raw(w, h, bgra)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

#[derive(Default)]
pub struct Manager {
    entries: HashMap<u64, Entry>,
    failed: HashSet<u64>,
    focused: Option<u64>,
    /// Display scale the visible pages were last laid out at.
    scale: f32,
    events: Option<Sender<TermEvent>>,
    /// The page a mouse button went down in and the buttons still down
    /// there: it owns the pointer until the last of them comes back up — a
    /// drag selection may leave the page.
    pressed: Option<(u64, Vec<Button>)>,
    /// The page the pointer was last over, owed a leave event.
    hovered: Option<u64>,
    /// Replaced or orphaned frame images whose gpui textures are still to be
    /// dropped ([`Manager::take_retired`]).
    retired: Vec<Arc<RenderImage>>,
}

impl Manager {
    /// Reconcile pages with the tab model. `live` includes webviews in
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
        let scale = window.scale_factor();
        let rescaled = scale != self.scale;
        self.scale = scale;
        self.events.get_or_insert_with(|| events.clone());
        let visible: HashSet<u64> = placements.iter().map(|p| p.id).collect();
        let retired = &mut self.retired;
        self.entries.retain(|id, entry| {
            if live.contains(id) {
                true
            } else {
                entry.host.close();
                retired.extend(entry.image.take().map(|(image, ..)| image));
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

        // Before the page is hidden, while it still takes input.
        if self.captor().is_some_and(|id| !visible.contains(&id)) {
            self.release_pointer();
        }
        for (id, entry) in &mut self.entries {
            if !visible.contains(id) && entry.visible {
                entry.host.set_focus(false);
                entry.host.set_hidden(true);
                entry.visible = false;
            }
        }
        if self.hovered.is_some_and(|id| !visible.contains(&id)) {
            self.hovered = None;
        }

        for placement in placements {
            if let Some(entry) = self.entries.get_mut(&placement.id) {
                if entry.bounds != placement.bounds || rescaled {
                    entry.host.resize(engine::view_size(placement.bounds, scale), scale);
                    entry.bounds = placement.bounds;
                }
                if !entry.visible {
                    entry.host.set_hidden(false);
                    entry.visible = true;
                }
            } else if !self.failed.contains(&placement.id) {
                match Host::create(
                    placement.id,
                    &placement.url,
                    engine::view_size(placement.bounds, scale),
                    scale,
                    parent_view(window),
                    events,
                ) {
                    Ok(host) => {
                        self.entries.insert(
                            placement.id,
                            Entry {
                                host,
                                bounds: placement.bounds,
                                visible: true,
                                image: None,
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
                previous.host.set_focus(false);
            }
            if let Some(entry) = focus.and_then(|id| self.entries.get(&id)) {
                entry.host.set_focus(true);
            }
            self.focused = focus;
        }
        first_error
    }

    /// The visible pages to paint this frame. Pages whose pixels changed
    /// since the last call get a new image; the one it replaces is retired.
    pub fn page_images(&mut self) -> Vec<PageImage> {
        let mut pages = Vec::new();
        for entry in self.entries.values_mut().filter(|entry| entry.visible) {
            if let Some((w, h, bgra)) = entry.host.take_frame()
                && let Some(image) = page_image(w, h, bgra)
            {
                let old = entry.image.replace((Arc::new(image), w, h));
                self.retired.extend(old.map(|(image, ..)| image));
            }
            if let Some((image, w, h)) = &entry.image {
                pages.push(PageImage { bounds: entry.bounds, image: image.clone(), w: *w, h: *h });
            }
        }
        pages
    }

    /// Images no page shows any more; the caller drops their gpui textures
    /// (`Window::drop_image`), which needs the window this type never holds.
    pub fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }

    /// The visible page under a physical-pixel window point.
    fn page_at(&self, x: f32, y: f32) -> Option<u64> {
        self.entries
            .iter()
            .find(|(_, entry)| entry.visible && engine::in_bounds(entry.bounds, x, y))
            .map(|(id, _)| *id)
    }

    /// The page that has the keyboard: the focused tile's visible web tab.
    pub fn focused(&self) -> Option<u64> {
        self.focused
    }

    /// Whether a page holds the pointer because a button went down in it.
    pub fn pointer_captured(&self) -> bool {
        self.pressed.is_some()
    }

    /// The cursor the page under the point asks for (the capturing page's
    /// during a drag).
    pub fn cursor_at(&self, x: f32, y: f32) -> Option<gpui::CursorStyle> {
        let id = self.captor().or_else(|| self.page_at(x, y))?;
        self.entries.get(&id)?.host.cursor()
    }

    /// The page holding the pointer, if a button is down in one.
    fn captor(&self) -> Option<u64> {
        self.pressed.as_ref().map(|(id, _)| *id)
    }

    /// Give the capturing page the release of every button it still holds
    /// (it is going away or being hidden mid-press, so the real release will
    /// not reach it) and drop the capture.
    fn release_pointer(&mut self) {
        let Some((id, held)) = self.pressed.take() else { return };
        if let Some(entry) = self.entries.get(&id) {
            for button in held {
                entry.host.mouse_button(0, 0, 0, button, true, 1);
            }
        }
    }

    fn point(&self, entry: &Entry, x: f32, y: f32) -> (i32, i32) {
        engine::page_point(entry.bounds, self.scale, x, y)
    }

    /// Pointer motion at a physical-pixel window point: to the page holding
    /// the pointer, else the one under it; a page the pointer just left gets
    /// its leave event. True when a page took the move.
    pub fn pointer_move(&mut self, x: f32, y: f32, mods: Mods) -> bool {
        let held = self.pressed.as_ref().and_then(|(_, held)| held.first().copied());
        let target = self.captor().or_else(|| self.page_at(x, y));
        let flags = engine::event_flags(mods, held);
        if self.hovered != target {
            if let Some(entry) = self.hovered.and_then(|id| self.entries.get(&id)) {
                let (px, py) = self.point(entry, x, y);
                entry.host.mouse_move(px, py, flags, true);
            }
            self.hovered = target;
        }
        let Some(entry) = target.and_then(|id| self.entries.get(&id)) else { return false };
        let (px, py) = self.point(entry, x, y);
        entry.host.mouse_move(px, py, flags, false);
        true
    }

    /// A button press at a window point. When it lands in a page the page
    /// gets it, captures the pointer, and reports itself focused
    /// ([`TermEvent::WebviewFocused`]); returns that page's id.
    pub fn pointer_down(
        &mut self,
        x: f32,
        y: f32,
        button: Button,
        clicks: usize,
        mods: Mods,
    ) -> Option<u64> {
        // A second button while one is held goes to the page holding the first.
        let id = self.captor().or_else(|| self.page_at(x, y))?;
        let entry = self.entries.get(&id)?;
        let (px, py) = self.point(entry, x, y);
        entry.host.mouse_button(px, py, engine::event_flags(mods, Some(button)), button, false, clicks);
        let held = &mut self.pressed.get_or_insert_with(|| (id, Vec::new())).1;
        if !held.contains(&button) {
            held.push(button);
        }
        if let Some(events) = &self.events {
            let _ = events.send(TermEvent::WebviewFocused { id });
        }
        Some(id)
    }

    /// The release of a button that went down in a page. False when no page
    /// holds that button, so the caller handles the release itself.
    pub fn pointer_up(&mut self, x: f32, y: f32, button: Button, mods: Mods) -> bool {
        let Some((id, held)) = &mut self.pressed else { return false };
        let Some(at) = held.iter().position(|held| *held == button) else { return false };
        held.remove(at);
        let id = *id;
        if held.is_empty() {
            self.pressed = None;
        }
        if let Some(entry) = self.entries.get(&id) {
            let (px, py) = self.point(entry, x, y);
            entry.host.mouse_button(px, py, engine::event_flags(mods, None), button, true, 1);
        }
        true
    }

    /// A wheel / trackpad scroll over a page. `lines` for wheel notches,
    /// otherwise the deltas are logical pixels.
    pub fn wheel(&mut self, x: f32, y: f32, lines: bool, dx: f32, dy: f32, mods: Mods) -> bool {
        let Some(entry) = self.page_at(x, y).and_then(|id| self.entries.get(&id)) else {
            return false;
        };
        let (px, py) = self.point(entry, x, y);
        let precise = if lines { 0 } else { engine::FLAG_PRECISION_SCROLL };
        let flags = engine::event_flags(mods, None) | precise;
        entry.host.mouse_wheel(px, py, flags, engine::wheel_delta(lines, dx, dy));
        true
    }

    /// A key-down for the focused page: the ⌘ editing chords run as frame
    /// commands, everything else is forwarded as key and character events.
    pub fn key_down(&self, id: u64, key: &str, key_char: Option<&str>, mods: Mods, repeat: bool) {
        let Some(entry) = self.entries.get(&id) else { return };
        if let Some(command) = engine::edit_command(key, mods) {
            entry.host.edit(command);
            return;
        }
        entry.host.key_down(&engine::key_down_plan(key, key_char, mods, repeat));
    }

    pub fn key_up(&self, id: u64, key: &str, mods: Mods) {
        if let (Some(entry), Some(codes)) = (self.entries.get(&id), engine::key_codes(key)) {
            entry.host.key_up(codes, engine::event_flags(mods, None) | engine::implied_shift(key));
        }
    }

    /// Run an editing command (copy, paste, …) in page `id`.
    pub fn edit(&self, id: u64, command: EditCommand) {
        if let Some(entry) = self.entries.get(&id) {
            entry.host.edit(command);
        }
    }

    /// Close every page and shut the engine down. Run on quit, and again —
    /// as a no-op — when the manager drops.
    pub fn shutdown(&mut self) {
        for (_, entry) in self.entries.drain() {
            entry.host.close();
        }
        self.focused = None;
        self.pressed = None;
        self.hovered = None;
        engine::shutdown();
    }

    pub fn state(&self, id: u64) -> Option<BrowserState> {
        let entry = self.entries.get(&id)?;
        Some(BrowserState {
            can_go_back: entry.host.can_go_back(),
            can_go_forward: entry.host.can_go_forward(),
            zoom_percent: (entry.host.zoom() * 100.0).round() as u16,
        })
    }

    fn entry(&self, id: u64) -> Result<&Entry, String> {
        self.entries
            .get(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    pub fn navigate(&self, id: u64, url: &str) -> Result<(), String> {
        self.entry(id)?.host.navigate(url)
    }

    pub fn reload(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.host.reload();
        Ok(())
    }

    pub fn go_back(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.host.go_back();
        Ok(())
    }

    pub fn go_forward(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.host.go_forward();
        Ok(())
    }

    pub fn set_zoom(&mut self, id: u64, zoom: f64) -> Result<u16, String> {
        let entry = self.entry(id)?;
        let zoom = zoom.clamp(0.5, 3.0);
        entry.host.set_zoom(zoom).map_err(|e| format!("zoom: {e}"))?;
        Ok((zoom * 100.0).round() as u16)
    }

    pub fn zoom(&mut self, id: u64, delta: f64) -> Result<u16, String> {
        // From the page's live zoom, rounded to the stepper's whole percent.
        let current = (self.entry(id)?.host.zoom() * 100.0).round() / 100.0;
        self.set_zoom(id, current + delta)
    }

    pub fn print(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.host.print().map_err(|e| format!("print: {e}"))
    }

    pub fn open_devtools(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .host
            .open_devtools()
            .map_err(|e| format!("developer tools: {e}"))
    }

    /// The number of cookies `url` is sent. Answered from a cache Chromium
    /// refreshes in the background (the main thread never waits on it): a
    /// changed count arrives as a redraw, by which time this returns it.
    pub fn cookie_count(&self, id: u64, url: &str) -> Result<usize, String> {
        self.entry(id)?;
        let events = self.events.as_ref().ok_or("webview is not ready yet")?;
        engine::cookie_count(url, id, events).map_err(|e| format!("cookies: {e}"))
    }

    /// Clear cookies and the HTTP cache for every web tab, and the stored
    /// data (local storage, IndexedDB, …) of the site tab `id` is showing.
    pub fn clear_browsing_data(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .host
            .clear_browsing_data()
            .map_err(|e| format!("clear browsing data: {e}"))
    }

    pub fn find(&self, id: u64, query: &str) -> Result<(), String> {
        self.entry(id)?
            .host
            .find(query)
            .map_err(|e| format!("find in page: {e}"))
    }

    pub fn focus(&self, id: u64) {
        if let Some(entry) = self.entries.get(&id) {
            entry.host.set_focus(true);
        }
    }
}

/// Closing the window releases the app view — and this with it — before
/// gpui's quit observers run, so the engine is shut down here too.
impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The gpui window's `NSView`, which Chromium parents its dialogs to; null
/// (the main screen, no parent) when the handle is unavailable.
fn parent_view(window: &Window) -> *mut std::os::raw::c_void {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Explicit trait call: gpui's `Window` has an inherent `window_handle()`.
    match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::AppKit(appkit)) => appkit.ns_view.as_ptr(),
        _ => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The toolbar follows the chrome text size, and the page starts
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
            // page covers the content rect exactly.
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
        // keeps 1px and the page always ends at the content's bottom edge.
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
    fn favicon_choice_skips_svg_and_unfetchable_links() {
        let icons = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            favicon_choice(&icons(&["https://a.test/i.svg", "https://a.test/i.png"])),
            "https://a.test/i.png"
        );
        assert_eq!(
            favicon_choice(&icons(&["https://a.test/I.SVG?v=2", "data:image/png;base64,AA"])),
            ""
        );
        assert_eq!(favicon_choice(&icons(&["https://a.test/favicon.ico"])), "https://a.test/favicon.ico");
        assert_eq!(favicon_choice(&[]), "");
        // The choice feeds `favicon_request`, whose fallback covers "none".
        assert_eq!(
            favicon_request(&format!("{}\nhttps://a.test/x", favicon_choice(&[]))),
            Some(("https://a.test".into(), "https://a.test/favicon.ico".into()))
        );
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
    fn fetcher_presents_as_the_engine() {
        // The pages run in Chromium; the icon fetch says so too.
        assert!(FETCH_USER_AGENT.contains(" Chrome/"));
        assert!(FETCH_USER_AGENT.contains("AppleWebKit/537.36"));
        assert!(!FETCH_USER_AGENT.contains(" Version/"));
    }

    #[test]
    fn page_image_takes_a_whole_bgra_frame_only() {
        let image = page_image(2, 3, vec![7; 2 * 3 * 4]).unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (2, 3));
        assert!(page_image(2, 3, vec![7; 5]).is_none());
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
