//! Chromium engine for web tabs: CEF (the `cef` crate, tauri-apps/cef-rs) in
//! off-screen rendering (windowless) mode.
//!
//! [`crate::webview::Manager`] owns the tab-facing lifecycle; this file is the
//! only code that talks to CEF. Three layers:
//!
//! - **Engine** — [`ensure_initialized`] loads the framework from the app
//!   bundle (`Contents/Frameworks/Chromium Embedded Framework.framework`,
//!   assembled by `scripts/make-app.sh`) and calls `cef::initialize` once, on
//!   the main thread, with `windowless_rendering_enabled` and an external
//!   message pump. gpui owns the `NSApplication` run loop, so CEF's own loop
//!   never runs: `on_schedule_message_pump_work` queues [`pump`] on the main
//!   dispatch queue, and the app's 16ms foreground pump calls it as a floor.
//!   CEF needs the application object to answer `CefAppProtocol`
//!   (`isHandlingSendEvent` / `setHandlingSendEvent:`), which gpui's
//!   `GPUIApplication` does not, so [`install_app_protocol`] adds those
//!   methods (and a `sendEvent:` wrapper that keeps the flag) at runtime.
//!   [`shutdown`] closes what is left and calls `cef::shutdown` on quit.
//!   Child processes are the separate `pwrde-helper` binary
//!   (`browser_subprocess_path`), never this executable. Running outside a
//!   bundle is an `Err` the caller shows as a toast, not a crash.
//! - **[`Host`]** — one windowless browser per live web tab. Its render
//!   handler answers `view_rect` / `screen_info` from the tab's logical size
//!   and the display scale, and `on_paint` copies each BGRA dirty rect into a
//!   per-tab [`PageFrame`]; a fresh frame sends one coalesced
//!   [`TermEvent::WebviewFrame`] so gpui repaints only when pixels changed.
//!   The display / load / life-span handlers report address, title, favicon
//!   and cursor changes; a popup (`window.open`, `target=_blank`) is cancelled
//!   and loaded in the same tab.
//! - **Pure logic** — event-flag and key-code mapping, the key-down plan, the
//!   frame / dirty-rect copy, view-size and pointer math, zoom levels, bundle
//!   and cache paths. No CEF calls, so the unit tests below run without the
//!   framework loaded.
//!
//! Every CEF callback runs on the main thread (the UI thread *is* the main
//! thread under the external pump), possibly re-entrantly from inside a call
//! this file makes, so handler state is short-lived locks and atomics and no
//! lock is held across a CEF call. Nothing here waits on a callback: the
//! cookie count is a cache refreshed by a visitor ([`cookie_count`]).
//!
//! Omissions vs a native browser view: IME composition (dead keys and
//! marked text; committed text and plain typing work), page-initiated drag
//! and drop, the native context menu (the page still gets `contextmenu`),
//! downloads, and permission / HTTP-auth prompts (CEF's defaults deny).
//! Cookies are encrypted with Chromium's mock keychain so launch never
//! prompts for Keychain access — at rest that is no stronger than the
//! WebKit cookie file it replaces.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::raw::{c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cef::*;

use crate::term::TermEvent;
use crate::workspace::LayoutRect;

// ── Pure logic ──────────────────────────────────────────────────────────

/// `cef_event_flags_t` bits, restated so the mapping below is testable
/// without the framework (the tests pin them to the bindings).
pub const FLAG_SHIFT: u32 = 1 << 1;
pub const FLAG_CONTROL: u32 = 1 << 2;
pub const FLAG_ALT: u32 = 1 << 3;
pub const FLAG_LEFT_BUTTON: u32 = 1 << 4;
pub const FLAG_MIDDLE_BUTTON: u32 = 1 << 5;
pub const FLAG_RIGHT_BUTTON: u32 = 1 << 6;
pub const FLAG_COMMAND: u32 = 1 << 7;
pub const FLAG_IS_REPEAT: u32 = 1 << 13;
pub const FLAG_PRECISION_SCROLL: u32 = 1 << 14;

/// Logical pixels one wheel "line" scrolls a page.
const WHEEL_LINE_PX: f32 = 40.0;
/// Frames per second CEF paints a visible page at.
const FRAME_RATE: c_int = 60;
/// Opaque white: pages without their own background paint on it, and the
/// frame buffer never carries alpha gpui would have to blend.
const PAGE_BACKGROUND: u32 = 0xFFFF_FFFF;
/// How stale a cached cookie count may get before a lookup refreshes it.
const COOKIE_REFRESH: Duration = Duration::from_secs(1);
/// Upper bound on the quit-time wait for browsers to finish closing.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1500);

/// The modifier keys of an input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// CEF's `modifiers` word for `mods`, with `held`'s button bit set while a
/// button is down (drag-moves and the press itself carry it).
pub fn event_flags(mods: Mods, held: Option<Button>) -> u32 {
    let mut flags = 0;
    for (on, bit) in [
        (mods.shift, FLAG_SHIFT),
        (mods.control, FLAG_CONTROL),
        (mods.alt, FLAG_ALT),
        (mods.command, FLAG_COMMAND),
    ] {
        if on {
            flags |= bit;
        }
    }
    flags
        | match held {
            Some(Button::Left) => FLAG_LEFT_BUTTON,
            Some(Button::Middle) => FLAG_MIDDLE_BUTTON,
            Some(Button::Right) => FLAG_RIGHT_BUTTON,
            None => 0,
        }
}

/// What CEF needs to know about one physical key: the macOS virtual key code
/// (`native_key_code` — what Chromium actually reads on macOS), the Windows
/// `VKEY` and the character AppKit would report for it unmodified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCodes {
    pub native: i32,
    pub windows: i32,
    pub character: u16,
}

/// Codes for a gpui key name (`"a"`, `"enter"`, `"left"`, `"f5"`, …) on the
/// US ANSI layout; `None` for a key this table does not know, which is then
/// typed as text only.
pub fn key_codes(key: &str) -> Option<KeyCodes> {
    let codes = |native, windows, character| Some(KeyCodes { native, windows, character });
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        // kVK_ANSI_* in key order; the letters' VKEY is their upper case.
        const LETTERS: [i32; 26] = [
            0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13,
            7, 16, 6,
        ];
        const DIGITS: [i32; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
        return match c {
            'a'..='z' => {
                codes(LETTERS[(c as u8 - b'a') as usize], c.to_ascii_uppercase() as i32, c as u16)
            },
            '0'..='9' => codes(DIGITS[(c as u8 - b'0') as usize], c as i32, c as u16),
            '=' => codes(24, 187, c as u16),
            '-' => codes(27, 189, c as u16),
            ']' => codes(30, 221, c as u16),
            '[' => codes(33, 219, c as u16),
            '\'' => codes(39, 222, c as u16),
            ';' => codes(41, 186, c as u16),
            '\\' => codes(42, 220, c as u16),
            ',' => codes(43, 188, c as u16),
            '/' => codes(44, 191, c as u16),
            '.' => codes(47, 190, c as u16),
            '`' => codes(50, 192, c as u16),
            // A shifted key is its base key typing the shifted character.
            _ => shifted_base(c)
                .and_then(|base| key_codes(base.encode_utf8(&mut [0; 4])))
                .map(|base| KeyCodes { character: c as u16, ..base }),
        };
    }
    match key {
        "enter" => codes(36, 13, 0x0d),
        "tab" => codes(48, 9, 0x09),
        "space" => codes(49, 32, 0x20),
        "backspace" => codes(51, 8, 0x7f),
        "escape" => codes(53, 27, 0x1b),
        // AppKit's function-key characters (NSUpArrowFunctionKey, …).
        "up" => codes(126, 38, 0xf700),
        "down" => codes(125, 40, 0xf701),
        "left" => codes(123, 37, 0xf702),
        "right" => codes(124, 39, 0xf703),
        "delete" => codes(117, 46, 0xf728),
        "home" => codes(115, 36, 0xf729),
        "end" => codes(119, 35, 0xf72b),
        "pageup" => codes(116, 33, 0xf72c),
        "pagedown" => codes(121, 34, 0xf72d),
        _ => {
            const FUNCTION: [i32; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];
            let n: usize = key.strip_prefix('f')?.parse().ok()?;
            let native = *FUNCTION.get(n.checked_sub(1)?)?;
            codes(native, 111 + n as i32, 0xf703 + n as u16)
        },
    }
}

/// The editing commands a page gets from ⌘ chords. On macOS Blink leaves
/// these to the application's Edit menu, so a forwarded key event alone
/// would do nothing; they run as frame commands instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Copy,
    Cut,
    Paste,
    SelectAll,
    Undo,
    Redo,
}

/// The edit command a ⌘ chord means, if any (⌘C/X/V/A/Z and ⇧⌘Z).
pub fn edit_command(key: &str, mods: Mods) -> Option<EditCommand> {
    if !mods.command || mods.control || mods.alt {
        return None;
    }
    match (key, mods.shift) {
        ("c", false) => Some(EditCommand::Copy),
        ("x", false) => Some(EditCommand::Cut),
        ("v", false) => Some(EditCommand::Paste),
        ("a", false) => Some(EditCommand::SelectAll),
        ("z", false) => Some(EditCommand::Undo),
        ("z", true) => Some(EditCommand::Redo),
        _ => None,
    }
}

/// One CEF key event, minus its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub codes: KeyCodes,
    /// The character this press produces (the typed one when there is one).
    pub character: u16,
    pub flags: u32,
}

/// What a key-down turns into, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyStep {
    /// A raw key-down: shortcuts, navigation, DOM `keydown`.
    Down(KeySpec),
    /// The character the press types (DOM `keypress` / text insertion).
    Char(KeySpec),
    /// Text that is not a single UTF-16 unit, or whose key is unknown:
    /// committed as IME text.
    Commit(String),
}

/// Plan the CEF events for a gpui key-down: the raw key-down when the key is
/// known, then the text it types — a `Char` for one UTF-16 unit, a commit for
/// anything longer. ⌃ and ⌘ chords type nothing; ↩ types a carriage return.
pub fn key_down_plan(key: &str, key_char: Option<&str>, mods: Mods, repeat: bool) -> Vec<KeyStep> {
    let flags =
        event_flags(mods, None) | implied_shift(key) | if repeat { FLAG_IS_REPEAT } else { 0 };
    let codes = key_codes(key);
    let text: Option<&str> = if mods.command || mods.control {
        None
    } else if key == "enter" {
        Some("\r")
    } else {
        key_char.filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
    };
    let mut units = [0u16; 2];
    let unit = text.and_then(|text| {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.len_utf16() == 1 => Some(c.encode_utf16(&mut units)[0]),
            _ => None,
        }
    });
    let mut steps = Vec::new();
    if let Some(codes) = codes {
        let character = unit.unwrap_or(codes.character);
        steps.push(KeyStep::Down(KeySpec { codes, character, flags }));
        if let Some(unit) = unit {
            steps.push(KeyStep::Char(KeySpec { codes, character: unit, flags }));
            return steps;
        }
    }
    if let Some(text) = text {
        steps.push(KeyStep::Commit(text.to_string()));
    }
    steps
}

/// A rectangle of device pixels inside a frame buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One web tab's pixels as CEF last painted them: tightly packed BGRA rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageFrame {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl PageFrame {
    /// Fold one `on_paint` into the frame. `src` is CEF's whole `w`×`h`
    /// buffer; only `dirty` changed. A buffer of a new size (or the first
    /// one) replaces the frame outright, otherwise just the dirty rects are
    /// copied, each clipped to the buffer. False — and no change — when `src`
    /// is not `w * h * 4` bytes.
    pub fn apply(&mut self, src: &[u8], w: u32, h: u32, dirty: &[DirtyRect]) -> bool {
        let len = w as usize * h as usize * 4;
        if w == 0 || h == 0 || src.len() != len {
            return false;
        }
        if self.w != w || self.h != h || self.bgra.len() != len || dirty.is_empty() {
            self.w = w;
            self.h = h;
            self.bgra.clear();
            self.bgra.extend_from_slice(src);
            return true;
        }
        let stride = w as usize * 4;
        for rect in dirty {
            let x0 = rect.x.clamp(0, w as i32) as usize;
            let y0 = rect.y.clamp(0, h as i32) as usize;
            let x1 = rect.x.saturating_add(rect.w).clamp(0, w as i32) as usize;
            let y1 = rect.y.saturating_add(rect.h).clamp(0, h as i32) as usize;
            if x1 <= x0 {
                continue;
            }
            for y in y0..y1 {
                let (from, to) = (y * stride + x0 * 4, y * stride + x1 * 4);
                self.bgra[from..to].copy_from_slice(&src[from..to]);
            }
        }
        true
    }

    /// Copy `over` onto this frame with its top-left at device pixel
    /// `(x, y)`, clipped to the frame — how a `<select>` popup, which CEF
    /// paints as its own element, lands on the page.
    pub fn blit(&mut self, over: &PageFrame, x: i32, y: i32) {
        let (dw, dh) = (self.w as i32, self.h as i32);
        let (sw, sh) = (over.w as i32, over.h as i32);
        if over.bgra.len() != (sw as usize) * (sh as usize) * 4 {
            return;
        }
        let x0 = x.max(0);
        let x1 = x.saturating_add(sw).min(dw);
        if x1 <= x0 {
            return;
        }
        for dy in y.max(0)..y.saturating_add(sh).min(dh) {
            let sy = dy - y;
            let from = (sy * sw + (x0 - x)) as usize * 4;
            let to = (dy * dw + x0) as usize * 4;
            let len = (x1 - x0) as usize * 4;
            self.bgra[to..to + len].copy_from_slice(&over.bgra[from..from + len]);
        }
    }
}

/// The logical (DIP) size CEF lays a page out at for a placement of physical
/// pixels on a display of `scale`. Rounded up, so the device-pixel buffer
/// (`size * scale`) always covers the placement and paints 1:1, clipped.
pub fn view_size(bounds: LayoutRect, scale: f32) -> (i32, i32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (
        ((bounds.w / scale).ceil() as i32).max(1),
        ((bounds.h / scale).ceil() as i32).max(1),
    )
}

/// A window point in physical pixels as page (logical view) coordinates.
pub fn page_point(bounds: LayoutRect, scale: f32, x: f32, y: f32) -> (i32, i32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (
        ((x - bounds.x) / scale).floor() as i32,
        ((y - bounds.y) / scale).floor() as i32,
    )
}

/// Whether a physical-pixel window point lies inside a placement.
pub fn in_bounds(bounds: LayoutRect, x: f32, y: f32) -> bool {
    x >= bounds.x && y >= bounds.y && x < bounds.x + bounds.w && y < bounds.y + bounds.h
}

/// Wheel deltas for CEF, in logical pixels: trackpad deltas pass through,
/// wheel lines scroll [`WHEEL_LINE_PX`] each.
pub fn wheel_delta(lines: bool, x: f32, y: f32) -> (i32, i32) {
    let unit = if lines { WHEEL_LINE_PX } else { 1.0 };
    ((x * unit).round() as i32, (y * unit).round() as i32)
}

/// Chromium's zoom *level* for a zoom *factor* (1.0 = 100%): each level is a
/// 1.2× step, level 0 is 100%.
pub fn zoom_level(factor: f64) -> f64 {
    factor.max(0.01).ln() / 1.2f64.ln()
}

/// [`FLAG_SHIFT`] for a key name that is itself a shifted character, which
/// gpui delivers without the shift modifier; 0 otherwise.
pub fn implied_shift(key: &str) -> u32 {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if shifted_base(c).is_some() => FLAG_SHIFT,
        _ => 0,
    }
}

/// The inverse of [`zoom_level`]: the zoom factor a Chromium level paints at.
pub fn zoom_factor(level: f64) -> f64 {
    1.2f64.powf(level)
}

/// The unshifted US ANSI key that types `c` with ⇧ held, for the shifted
/// punctuation and capitals gpui reports as the key itself (⇧1 arrives as
/// `"!"` with no shift modifier).
pub fn shifted_base(c: char) -> Option<char> {
    const SHIFTED: &str = "!@#$%^&*()_+{}|:\"<>?~";
    const BASE: &str = "1234567890-=[]\\;',./`";
    if c.is_ascii_uppercase() {
        return Some(c.to_ascii_lowercase());
    }
    SHIFTED.find(c).and_then(|at| BASE[at..].chars().next())
}

/// The gpui cursor for a CEF cursor type (`cef_cursor_type_t` as an integer);
/// `None` where gpui has no counterpart and the arrow stays.
pub fn cursor_style(kind: u32) -> Option<gpui::CursorStyle> {
    use cef::sys::cef_cursor_type_t as T;
    use gpui::CursorStyle as S;
    const fn raw(kind: cef::sys::cef_cursor_type_t) -> u32 {
        kind as u32
    }
    Some(match kind {
        k if k == raw(T::CT_POINTER) => S::Arrow,
        k if k == raw(T::CT_HAND) => S::PointingHand,
        k if k == raw(T::CT_IBEAM) => S::IBeam,
        k if k == raw(T::CT_VERTICALTEXT) => S::IBeamCursorForVerticalLayout,
        k if k == raw(T::CT_CROSS) => S::Crosshair,
        k if k == raw(T::CT_GRAB) => S::OpenHand,
        k if k == raw(T::CT_GRABBING) => S::ClosedHand,
        k if k == raw(T::CT_EASTRESIZE) => S::ResizeRight,
        k if k == raw(T::CT_WESTRESIZE) => S::ResizeLeft,
        k if k == raw(T::CT_NORTHRESIZE) => S::ResizeUp,
        k if k == raw(T::CT_SOUTHRESIZE) => S::ResizeDown,
        k if k == raw(T::CT_EASTWESTRESIZE) => S::ResizeLeftRight,
        k if k == raw(T::CT_NORTHSOUTHRESIZE) => S::ResizeUpDown,
        k if k == raw(T::CT_COLUMNRESIZE) => S::ResizeColumn,
        k if k == raw(T::CT_ROWRESIZE) => S::ResizeRow,
        k if k == raw(T::CT_NORTHEASTSOUTHWESTRESIZE)
            || k == raw(T::CT_NORTHEASTRESIZE)
            || k == raw(T::CT_SOUTHWESTRESIZE) =>
        {
            S::ResizeUpRightDownLeft
        },
        k if k == raw(T::CT_NORTHWESTSOUTHEASTRESIZE)
            || k == raw(T::CT_NORTHWESTRESIZE)
            || k == raw(T::CT_SOUTHEASTRESIZE) =>
        {
            S::ResizeUpLeftDownRight
        },
        k if k == raw(T::CT_NOTALLOWED) || k == raw(T::CT_NODROP) => S::OperationNotAllowed,
        k if k == raw(T::CT_ALIAS) => S::DragLink,
        _ => return None,
    })
}

/// Where CEF's pieces live relative to the running executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundlePaths {
    /// `Pwrde.app`.
    pub bundle: PathBuf,
    /// `Contents/Frameworks/Chromium Embedded Framework.framework`.
    pub framework: PathBuf,
    /// The base helper executable; CEF derives the `(GPU)` / `(Renderer)` /
    /// … siblings from it.
    pub helper: PathBuf,
}

const FRAMEWORK_DIR: &str = "Chromium Embedded Framework.framework";
const FRAMEWORK_BINARY: &str = "Chromium Embedded Framework";
const HELPER_NAME: &str = "Pwrde Helper";

/// The bundle layout around `exe` (`<bundle>/Contents/MacOS/<exe>`), or
/// `None` when the executable is not inside an `.app` at all.
pub fn bundle_paths(exe: &Path) -> Option<BundlePaths> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    if macos.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle.extension()? != "app" {
        return None;
    }
    let frameworks = contents.join("Frameworks");
    Some(BundlePaths {
        bundle: bundle.to_path_buf(),
        framework: frameworks.join(FRAMEWORK_DIR),
        helper: frameworks
            .join(format!("{HELPER_NAME}.app"))
            .join("Contents")
            .join("MacOS")
            .join(HELPER_NAME),
    })
}

/// Chromium's profile directory: `<data_dir>/pwrde/cef`, or under
/// `worktrees/<slug>/` for an app launched from a linked git worktree — the
/// same scoping as the settings and state DB, and required here because two
/// running instances cannot share one profile.
pub fn cache_dir_in(data_dir: &Path, scope: Option<&str>) -> PathBuf {
    let mut path = data_dir.join("pwrde");
    if let Some(slug) = scope {
        path = path.join("worktrees").join(slug);
    }
    path.join("cef")
}

/// Where a popup request (`window.open`, `target=_blank`) goes: the same tab,
/// when its target is a URL a web tab may show. Anything else is dropped.
pub fn popup_navigation(target: &str) -> Option<String> {
    crate::bus::validate_webview_url(target).ok()
}

// ── Engine ──────────────────────────────────────────────────────────────

enum EngineState {
    Unstarted,
    Ready,
    Failed(String),
    Stopped,
}

thread_local! {
    /// Main thread only: CEF is initialized, pumped and shut down there.
    static ENGINE: RefCell<EngineState> = const { RefCell::new(EngineState::Unstarted) };
    /// Guards `do_message_loop_work` against re-entry.
    static PUMPING: Cell<bool> = const { Cell::new(false) };
    /// A [`pump`] arrived while one was running (a nested run loop, e.g. a
    /// modal panel CEF opened); it is re-queued once the outer one returns.
    static PUMP_MISSED: Cell<bool> = const { Cell::new(false) };
}

/// Browsers created and not yet through `on_before_close`.
static LIVE_BROWSERS: AtomicI32 = AtomicI32::new(0);
/// An immediate [`pump`] is already queued on the main dispatch queue.
static PUMP_QUEUED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_after_f(
        when: u64,
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_time(when: u64, delta: i64) -> u64;
}

extern "C" fn pump_now(_: *mut c_void) {
    PUMP_QUEUED.store(false, Ordering::Release);
    pump();
}

extern "C" fn pump_later(_: *mut c_void) {
    pump();
}

/// CEF asked for message-loop work in `delay_ms` (any thread): run [`pump`]
/// on the main queue then. Immediate requests coalesce into one block.
fn schedule_pump(delay_ms: i64) {
    let queue = &raw const _dispatch_main_q;
    if delay_ms <= 0 {
        if !PUMP_QUEUED.swap(true, Ordering::AcqRel) {
            unsafe { dispatch_async_f(queue, std::ptr::null_mut(), pump_now) };
        }
    } else {
        unsafe {
            let when = dispatch_time(0, delay_ms.saturating_mul(1_000_000));
            dispatch_after_f(when, queue, std::ptr::null_mut(), pump_later);
        }
    }
}

/// Run CEF's pending work. Main thread; a no-op until the engine is up, and
/// when called from inside itself.
pub fn pump() {
    if !ENGINE.with_borrow(|state| matches!(state, EngineState::Ready)) {
        return;
    }
    if PUMPING.replace(true) {
        PUMP_MISSED.set(true);
        return;
    }
    do_message_loop_work();
    PUMPING.set(false);
    if PUMP_MISSED.replace(false) {
        schedule_pump(0);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

wrap_app! {
    struct EngineApp {
        process: BrowserProcessHandler,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            // Browser process only (its type is empty).
            if process_type.is_some_and(|kind| !kind.to_string().is_empty()) {
                return;
            }
            let Some(command_line) = command_line else { return };
            // No Keychain prompt at launch; see the module header.
            command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.process.clone())
        }
    }
}

wrap_browser_process_handler! {
    struct EngineProcess;

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            schedule_pump(delay_ms);
        }

        /// A second pwrde started on this profile. Handled (nonzero) so CEF
        /// does not open its default Chrome-styled window in this process;
        /// that instance reports the profile as in use on its own.
        fn on_already_running_app_relaunch(
            &self,
            _command_line: Option<&mut CommandLine>,
            _current_directory: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }
    }
}

/// Add CEF's `CefAppProtocol` to gpui's `NSApplication` subclass at runtime:
/// `isHandlingSendEvent` / `setHandlingSendEvent:` over one process-wide
/// flag, a `sendEvent:` wrapper that sets it for the duration of the
/// original, and the protocol conformances themselves when the framework
/// registered them. Idempotent.
fn install_app_protocol() {
    use objc::runtime::{self, BOOL, Class, Imp, Method, NO, Object, Sel, YES};
    use objc::{class, msg_send, sel, sel_impl};

    static HANDLING: AtomicBool = AtomicBool::new(false);
    static ORIGINAL_SEND_EVENT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn is_handling(_: &Object, _: Sel) -> BOOL {
        if HANDLING.load(Ordering::Relaxed) { YES } else { NO }
    }
    extern "C" fn set_handling(_: &Object, _: Sel, handling: BOOL) {
        HANDLING.store(handling != NO, Ordering::Relaxed);
    }
    extern "C" fn send_event(this: &Object, sel: Sel, event: *mut Object) {
        let was = HANDLING.swap(true, Ordering::Relaxed);
        let original = ORIGINAL_SEND_EVENT.load(Ordering::Relaxed);
        if original != 0 {
            let original: extern "C" fn(&Object, Sel, *mut Object) =
                unsafe { std::mem::transmute(original) };
            original(this, sel, event);
        }
        HANDLING.store(was, Ordering::Relaxed);
    }

    // `BOOL` is a real bool on arm64 and a signed char on x86_64.
    let (getter_types, setter_types) = if cfg!(target_arch = "aarch64") {
        (c"B@:", c"v@:B")
    } else {
        (c"c@:", c"v@:c")
    };
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let responds: BOOL = msg_send![app, respondsToSelector: sel!(isHandlingSendEvent)];
        if responds != NO {
            return;
        }
        let class = runtime::object_getClass(app) as *mut Class;
        let getter: Imp =
            std::mem::transmute(is_handling as extern "C" fn(&Object, Sel) -> BOOL);
        let setter: Imp =
            std::mem::transmute(set_handling as extern "C" fn(&Object, Sel, BOOL));
        let wrapper: Imp =
            std::mem::transmute(send_event as extern "C" fn(&Object, Sel, *mut Object));
        runtime::class_addMethod(class, sel!(isHandlingSendEvent), getter, getter_types.as_ptr());
        runtime::class_addMethod(class, sel!(setHandlingSendEvent:), setter, setter_types.as_ptr());
        // The class inherits `sendEvent:` from NSApplication: add an override
        // that calls the inherited implementation. Should a future gpui define
        // its own, replace that one instead.
        let method = runtime::class_getInstanceMethod(class, sel!(sendEvent:)) as *mut Method;
        if !method.is_null() {
            let original = runtime::method_getImplementation(method);
            ORIGINAL_SEND_EVENT.store(original as usize, Ordering::Relaxed);
            if runtime::class_addMethod(class, sel!(sendEvent:), wrapper, c"v@:@".as_ptr()) == NO {
                runtime::method_setImplementation(method, wrapper);
            }
        }
        for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
            let protocol = runtime::objc_getProtocol(name.as_ptr());
            if !protocol.is_null() {
                runtime::class_addProtocol(class, protocol);
            }
        }
    }
}

/// Bring CEF up on first use. `Ok` once it is running; the first failure is
/// remembered and returned to every later caller.
pub fn ensure_initialized() -> Result<(), String> {
    let known = ENGINE.with_borrow(|state| match state {
        EngineState::Unstarted => None,
        EngineState::Ready => Some(Ok(())),
        EngineState::Failed(error) => Some(Err(error.clone())),
        EngineState::Stopped => Some(Err("Chromium has shut down".to_string())),
    });
    if let Some(known) = known {
        return known;
    }
    let result = initialize_engine();
    ENGINE.set(match &result {
        Ok(()) => EngineState::Ready,
        Err(error) => {
            eprintln!("webview: {error}");
            EngineState::Failed(error.clone())
        },
    });
    result
}

fn initialize_engine() -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;

    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|error| format!("cannot resolve the executable: {error}"))?;
    let unbundled = || {
        "Chromium is not bundled with this build — web tabs need the app bundle \
         (scripts/make-app.sh, or `scripts/make-app.sh debug` for a dev build)"
            .to_string()
    };
    let paths = bundle_paths(&exe).ok_or_else(unbundled)?;
    let library = paths.framework.join(FRAMEWORK_BINARY);
    if !library.is_file() || !paths.helper.is_file() {
        return Err(unbundled());
    }
    let data_dir =
        dirs::data_dir().ok_or_else(|| "no data directory for the Chromium profile".to_string())?;
    let cache = cache_dir_in(&data_dir, crate::git::worktree_scope().as_deref());
    std::fs::create_dir_all(&cache)
        .map_err(|error| format!("cannot create {}: {error}", cache.display()))?;

    let library = std::ffi::CString::new(library.as_os_str().as_bytes())
        .map_err(|_| "framework path contains a NUL".to_string())?;
    if load_library(Some(unsafe { &*library.as_ptr().cast() })) != 1 {
        return Err("cannot load the Chromium Embedded Framework".into());
    }
    // Pin the API version the wrapper was compiled against.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    install_app_protocol();

    // Only argv[0]: pwrde's own arguments (a directory to open) mean nothing
    // to Chromium, which would treat a bare path as a URL to load.
    let program = std::ffi::CString::new(exe.as_os_str().as_bytes())
        .map_err(|_| "executable path contains a NUL".to_string())?;
    let mut argv = [program.as_ptr().cast_mut()];
    let args = MainArgs { argc: 1, argv: argv.as_mut_ptr() };

    let path = |path: &Path| CefString::from(path.to_string_lossy().as_ref());
    let settings = Settings {
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        command_line_args_disabled: 1,
        browser_subprocess_path: path(&paths.helper),
        framework_dir_path: path(&paths.framework),
        main_bundle_path: path(&paths.bundle),
        root_cache_path: path(&cache),
        cache_path: path(&cache),
        background_color: PAGE_BACKGROUND,
        log_severity: LogSeverity::WARNING,
        log_file: path(&cache.join("cef.log")),
        ..Default::default()
    };
    let mut app = EngineApp::new(EngineProcess::new());
    if initialize(Some(&args), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        return Err(
            "Chromium failed to start (another pwrde may be using the same profile)".into(),
        );
    }
    Ok(())
}

/// Shut CEF down on quit: give the browsers the caller just closed a bounded
/// moment to finish, then `cef::shutdown`. Safe to call when CEF never
/// started, and more than once.
pub fn shutdown() {
    if !ENGINE.with_borrow(|state| matches!(state, EngineState::Ready)) {
        return;
    }
    let deadline = Instant::now() + SHUTDOWN_WAIT;
    while LIVE_BROWSERS.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    pump();
    ENGINE.set(EngineState::Stopped);
    cef::shutdown();
}

// ── Cookie counts ───────────────────────────────────────────────────────

struct CookieCount {
    count: usize,
    at: Instant,
    pending: bool,
}

static COOKIE_COUNTS: LazyLock<Mutex<HashMap<String, CookieCount>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One in-flight count. The visitor learns the total from its first cookie
/// and is never called for a URL with none, so the result is recorded when
/// CEF releases the visitor.
struct CookieTally {
    url: String,
    id: u64,
    count: AtomicUsize,
    events: Sender<TermEvent>,
}

impl Drop for CookieTally {
    fn drop(&mut self) {
        let count = self.count.load(Ordering::Relaxed);
        let changed = {
            let mut counts = lock(&COOKIE_COUNTS);
            let changed = counts.get(&self.url).is_none_or(|known| known.count != count);
            counts.insert(
                self.url.clone(),
                CookieCount { count, at: Instant::now(), pending: false },
            );
            changed
        };
        if changed {
            let _ = self.events.send(TermEvent::WebviewFrame { id: self.id });
        }
    }
}

wrap_cookie_visitor! {
    struct CookieCounter {
        tally: Arc<CookieTally>,
    }

    impl CookieVisitor {
        fn visit(
            &self,
            _cookie: Option<&Cookie>,
            _count: c_int,
            total: c_int,
            _delete_cookie: Option<&mut c_int>,
        ) -> c_int {
            self.tally.count.store(total.max(0) as usize, Ordering::Relaxed);
            // The total is all this wants: stop visiting.
            0
        }
    }
}

/// The number of cookies `url` would be sent, from a cache: the answer is the
/// last count a visitor finished (0 before the first), and a lookup of a
/// stale entry starts a new visit whose result arrives as a
/// [`TermEvent::WebviewFrame`] for tab `id` when it differs. Never blocks.
pub fn cookie_count(url: &str, id: u64, events: &Sender<TermEvent>) -> Result<usize, String> {
    if !ENGINE.with_borrow(|state| matches!(state, EngineState::Ready)) {
        return Err("webview is not ready yet".into());
    }
    let (count, refresh) = {
        let mut counts = lock(&COOKIE_COUNTS);
        match counts.get_mut(url) {
            Some(known) => {
                let refresh = !known.pending && known.at.elapsed() >= COOKIE_REFRESH;
                known.pending |= refresh;
                (known.count, refresh)
            },
            None => {
                counts.insert(
                    url.to_string(),
                    CookieCount { count: 0, at: Instant::now(), pending: true },
                );
                (0, true)
            },
        }
    };
    if refresh {
        let manager =
            cookie_manager_get_global_manager(None).ok_or_else(|| "no cookie store".to_string())?;
        let mut visitor = CookieCounter::new(Arc::new(CookieTally {
            url: url.to_string(),
            id,
            count: AtomicUsize::new(0),
            events: events.clone(),
        }));
        manager.visit_url_cookies(Some(&CefString::from(url)), 1, Some(&mut visitor));
    }
    Ok(count)
}

// ── One page ────────────────────────────────────────────────────────────

/// The size CEF should lay the page out at, and the display scale.
#[derive(Clone, Copy)]
struct View {
    w: i32,
    h: i32,
    scale: f32,
}

#[derive(Default)]
struct Painted {
    view: PageFrame,
    /// A `<select>` list or similar, painted by CEF as a separate element at
    /// `popup_at` (device pixels) while `popup_shown`.
    popup: PageFrame,
    popup_at: (i32, i32),
    popup_shown: bool,
    /// Pixels changed since [`Host::take_frame`].
    dirty: bool,
}

/// State one tab's CEF handlers share with its [`Host`].
struct Page {
    id: u64,
    events: Sender<TermEvent>,
    view: Mutex<View>,
    painted: Mutex<Painted>,
    /// A `WebviewFrame` event is queued and not yet consumed by a paint.
    notified: AtomicBool,
    /// `cef_cursor_type_t` the page last asked for.
    cursor: AtomicU32,
    /// The current document already reported its icon links.
    icon_reported: AtomicBool,
}

impl Page {
    fn send(&self, event: TermEvent) {
        let _ = self.events.send(event);
    }

    /// Ask for one repaint; coalesced until the next [`Host::take_frame`].
    fn notify(&self) {
        if !self.notified.swap(true, Ordering::AcqRel) {
            self.send(TermEvent::WebviewFrame { id: self.id });
        }
    }

    fn report_favicon(&self, href: &str, page: &str) {
        let (origin, icon) = crate::webview::favicon_request(&format!("{href}\n{page}")).unzip();
        self.send(TermEvent::WebviewFaviconChanged {
            id: self.id,
            origin: origin.unwrap_or_default(),
            icon,
        });
    }
}

fn frame_url(frame: &Frame) -> String {
    CefString::from(&frame.url()).to_string()
}

wrap_render_handler! {
    struct PageRender {
        page: Arc<Page>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else { return };
            let view = *lock(&self.page.view);
            rect.x = 0;
            rect.y = 0;
            rect.width = view.w.max(1);
            rect.height = view.h.max(1);
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> c_int {
            let Some(info) = screen_info else { return 0 };
            let view = *lock(&self.page.view);
            let rect = Rect { x: 0, y: 0, width: view.w.max(1), height: view.h.max(1) };
            info.device_scale_factor = view.scale;
            info.rect = rect.clone();
            info.available_rect = rect;
            1
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: c_int) {
            {
                let mut painted = lock(&self.page.painted);
                painted.popup_shown = show != 0;
                if show == 0 {
                    painted.popup = PageFrame::default();
                }
                painted.dirty = true;
            }
            self.page.notify();
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            let Some(rect) = rect else { return };
            let scale = lock(&self.page.view).scale;
            lock(&self.page.painted).popup_at = (
                (rect.x as f32 * scale).round() as i32,
                (rect.y as f32 * scale).round() as i32,
            );
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: c_int,
            height: c_int,
        ) {
            if buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let (w, h) = (width as u32, height as u32);
            // CEF owns the buffer for the duration of this call.
            let src = unsafe { std::slice::from_raw_parts(buffer, w as usize * h as usize * 4) };
            {
                let mut painted = lock(&self.page.painted);
                if type_ == PaintElementType::POPUP {
                    painted.popup.apply(src, w, h, &[]);
                } else {
                    let dirty: Vec<DirtyRect> = dirty_rects
                        .unwrap_or_default()
                        .iter()
                        .map(|rect| DirtyRect { x: rect.x, y: rect.y, w: rect.width, h: rect.height })
                        .collect();
                    painted.view.apply(src, w, h, &dirty);
                }
                painted.dirty = true;
            }
            self.page.notify();
        }
    }
}

wrap_display_handler! {
    struct PageDisplay {
        page: Arc<Page>,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let (Some(frame), Some(url)) = (frame, url) else { return };
            if frame.is_main() != 0 {
                self.page.send(TermEvent::WebviewNavigated { id: self.page.id, url: url.to_string() });
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let title = title.map(|title| title.to_string()).unwrap_or_default();
            self.page.send(TermEvent::WebviewTitleChanged { id: self.page.id, title });
        }

        fn on_favicon_urlchange(
            &self,
            browser: Option<&mut Browser>,
            icon_urls: Option<&mut CefStringList>,
        ) {
            let Some(page) = browser.and_then(|browser| browser.main_frame()).map(|frame| frame_url(&frame))
            else {
                return;
            };
            let icons: Vec<String> =
                icon_urls.map(|urls| urls.clone().into_iter().collect()).unwrap_or_default();
            self.page.icon_reported.store(true, Ordering::Release);
            self.page.report_favicon(crate::webview::favicon_choice(&icons), &page);
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: *mut u8,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> c_int {
            if self.page.cursor.swap(type_.get_raw(), Ordering::AcqRel) != type_.get_raw() {
                // The cursor style is set during paint.
                self.page.notify();
            }
            1
        }
    }
}

wrap_load_handler! {
    struct PageLoad {
        page: Arc<Page>,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            _is_loading: c_int,
            _can_go_back: c_int,
            _can_go_forward: c_int,
        ) {
            // The toolbar's back / forward state is read at render.
            self.page.notify();
        }

        fn on_load_start(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.page.icon_reported.store(false, Ordering::Release);
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: c_int,
        ) {
            let Some(frame) = frame.filter(|frame| frame.is_main() != 0) else { return };
            let url = frame_url(frame);
            // A document that named no icon: the origin's /favicon.ico, or
            // no icon at all for a page that is not http(s).
            if !self.page.icon_reported.swap(true, Ordering::AcqRel) {
                self.page.report_favicon("", &url);
            }
            // Warm the Site popover's cookie count for this page.
            let _ = cookie_count(&url, self.page.id, &self.page.events);
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpan;

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
            // A tab is one page: load the popup's target here instead of
            // letting CEF open a window of its own.
            let target = target_url.map(|url| url.to_string()).unwrap_or_default();
            if let Some(url) = popup_navigation(&target)
                && let Some(frame) = browser.and_then(|browser| browser.main_frame())
            {
                frame.load_url(Some(&CefString::from(url.as_str())));
            }
            1
        }

        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            LIVE_BROWSERS.fetch_add(1, Ordering::AcqRel);
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            LIVE_BROWSERS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

wrap_context_menu_handler! {
    struct PageMenu;

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            // An empty model shows no menu: CEF's native one needs a view to
            // hang from, which an off-screen page does not have.
            if let Some(model) = model {
                model.clear();
            }
        }
    }
}

wrap_client! {
    struct PageClient {
        render: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        menu: ContextMenuHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.menu.clone())
        }
    }
}

/// One live web tab's windowless browser. Main thread only.
pub struct Host {
    browser: Browser,
    page: Arc<Page>,
    /// The last find-in-page query, so a repeat steps to the next match.
    find: RefCell<String>,
}

impl Host {
    /// Create the browser for tab `id` and start loading `url` at the given
    /// logical size and display scale. `parent_view` is the gpui window's
    /// `NSView`: CEF only uses it to parent dialogs and pick the screen.
    pub fn create(
        id: u64,
        url: &str,
        size: (i32, i32),
        scale: f32,
        parent_view: *mut c_void,
        events: &Sender<TermEvent>,
    ) -> Result<Host, String> {
        ensure_initialized()?;
        let page = Arc::new(Page {
            id,
            events: events.clone(),
            view: Mutex::new(View { w: size.0, h: size.1, scale }),
            painted: Mutex::new(Painted::default()),
            notified: AtomicBool::new(false),
            cursor: AtomicU32::new(CursorType::POINTER.get_raw()),
            icon_reported: AtomicBool::new(false),
        });
        let mut client = PageClient::new(
            PageRender::new(page.clone()),
            PageDisplay::new(page.clone()),
            PageLoad::new(page.clone()),
            PageLifeSpan::new(),
            PageMenu::new(),
        );
        let window_info = WindowInfo::default().set_as_windowless(parent_view);
        let settings = BrowserSettings {
            windowless_frame_rate: FRAME_RATE,
            background_color: PAGE_BACKGROUND,
            ..Default::default()
        };
        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&settings),
            None,
            None,
        )
        .ok_or_else(|| "Chromium could not create the page".to_string())?;
        Ok(Host { browser, page, find: RefCell::new(String::new()) })
    }

    fn host(&self) -> Result<BrowserHost, String> {
        self.browser.host().ok_or_else(|| "webview is closing".to_string())
    }

    /// The tab moved to a new logical size and/or display scale.
    pub fn resize(&self, size: (i32, i32), scale: f32) {
        let rescaled = {
            let mut view = lock(&self.page.view);
            let rescaled = view.scale != scale;
            *view = View { w: size.0, h: size.1, scale };
            rescaled
        };
        if let Ok(host) = self.host() {
            if rescaled {
                host.notify_screen_info_changed();
            }
            host.was_resized();
        }
    }

    /// Stop (or resume) painting while the tab is not on screen.
    pub fn set_hidden(&self, hidden: bool) {
        if !hidden {
            // The paint that consumes a queued notice may never have come.
            self.page.notified.store(false, Ordering::Release);
        }
        if let Ok(host) = self.host() {
            host.was_hidden(hidden as c_int);
            if !hidden {
                host.invalidate(PaintElementType::VIEW);
            }
        }
    }

    pub fn set_focus(&self, focus: bool) {
        if let Ok(host) = self.host() {
            host.set_focus(focus as c_int);
        }
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let frame = self.browser.main_frame().ok_or_else(|| "navigate: no page".to_string())?;
        frame.load_url(Some(&CefString::from(url)));
        Ok(())
    }

    pub fn reload(&self) {
        self.browser.reload();
    }

    pub fn go_back(&self) {
        self.browser.go_back();
    }

    pub fn go_forward(&self) {
        self.browser.go_forward();
    }

    pub fn can_go_back(&self) -> bool {
        self.browser.can_go_back() != 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.browser.can_go_forward() != 0
    }

    /// Set the zoom factor (1.0 = 100%).
    pub fn set_zoom(&self, factor: f64) -> Result<(), String> {
        self.host()?.set_zoom_level(zoom_level(factor));
        Ok(())
    }

    /// The zoom factor the page paints at now. Chromium keeps zoom per host
    /// in the profile, so this — not what a tab last asked for — is the truth
    /// after a navigation, in a second tab on the same site, or on relaunch.
    pub fn zoom(&self) -> f64 {
        self.host().map_or(1.0, |host| zoom_factor(host.zoom_level()))
    }

    /// Highlight `query` in the page and select its first match; the same
    /// query again steps to the next one, an empty one clears the search.
    pub fn find(&self, query: &str) -> Result<(), String> {
        let host = self.host()?;
        if query.is_empty() {
            host.stop_finding(1);
            self.find.borrow_mut().clear();
            return Ok(());
        }
        let next = *self.find.borrow() == query;
        host.find(Some(&CefString::from(query)), 1, 0, next as c_int);
        *self.find.borrow_mut() = query.to_string();
        Ok(())
    }

    pub fn print(&self) -> Result<(), String> {
        self.host()?.print();
        Ok(())
    }

    /// Open Chromium's DevTools for this page in a window of its own.
    pub fn open_devtools(&self) -> Result<(), String> {
        self.host()?.show_dev_tools(
            Some(&WindowInfo::default()),
            None,
            Some(&BrowserSettings::default()),
            None,
        );
        Ok(())
    }

    /// Delete every cookie and the HTTP cache (both profile-wide), and the
    /// storage (local storage, IndexedDB, service workers, …) of the origin
    /// this page is on. Other origins' storage is left alone: CEF has no
    /// profile-wide call for it.
    pub fn clear_browsing_data(&self) -> Result<(), String> {
        let host = self.host()?;
        let url = self.browser.main_frame().map(|frame| frame_url(&frame)).unwrap_or_default();
        let origin = crate::webview::origin(&url);
        let cookies =
            cookie_manager_get_global_manager(None).ok_or_else(|| "no cookie store".to_string())?;
        if cookies.delete_cookies(None, None, None) == 0 {
            return Err("clear browsing data: the cookie store refused".into());
        }
        lock(&COOKIE_COUNTS).clear();
        host.execute_dev_tools_method(0, Some(&CefString::from("Network.clearBrowserCache")), None);
        if let Some(origin) = origin
            && let Some(mut params) = dictionary_value_create()
        {
            params.set_string(Some(&CefString::from("origin")), Some(&CefString::from(origin)));
            params.set_string(Some(&CefString::from("storageTypes")), Some(&CefString::from("all")));
            host.execute_dev_tools_method(
                0,
                Some(&CefString::from("Storage.clearDataForOrigin")),
                Some(&mut params),
            );
        }
        Ok(())
    }

    /// Run an editing command in the frame that has the caret.
    pub fn edit(&self, command: EditCommand) {
        let Some(frame) = self.browser.focused_frame().or_else(|| self.browser.main_frame()) else {
            return;
        };
        match command {
            EditCommand::Copy => frame.copy(),
            EditCommand::Cut => frame.cut(),
            EditCommand::Paste => frame.paste(),
            EditCommand::SelectAll => frame.select_all(),
            EditCommand::Undo => frame.undo(),
            EditCommand::Redo => frame.redo(),
        }
    }

    /// Pointer motion at page point `(x, y)`; `leave` when it left the page.
    pub fn mouse_move(&self, x: i32, y: i32, flags: u32, leave: bool) {
        if let Ok(host) = self.host() {
            host.send_mouse_move_event(Some(&MouseEvent { x, y, modifiers: flags }), leave as c_int);
        }
    }

    pub fn mouse_button(&self, x: i32, y: i32, flags: u32, button: Button, up: bool, clicks: usize) {
        let Ok(host) = self.host() else { return };
        let kind = match button {
            Button::Left => MouseButtonType::LEFT,
            Button::Middle => MouseButtonType::MIDDLE,
            Button::Right => MouseButtonType::RIGHT,
        };
        host.send_mouse_click_event(
            Some(&MouseEvent { x, y, modifiers: flags }),
            kind,
            up as c_int,
            clicks.clamp(1, 3) as c_int,
        );
    }

    pub fn mouse_wheel(&self, x: i32, y: i32, flags: u32, delta: (i32, i32)) {
        if let Ok(host) = self.host() {
            host.send_mouse_wheel_event(Some(&MouseEvent { x, y, modifiers: flags }), delta.0, delta.1);
        }
    }

    fn key_event(spec: KeySpec, kind: KeyEventType) -> KeyEvent {
        KeyEvent {
            type_: kind,
            modifiers: spec.flags,
            windows_key_code: spec.codes.windows,
            native_key_code: spec.codes.native,
            character: spec.character,
            unmodified_character: spec.codes.character,
            ..Default::default()
        }
    }

    /// Send the events of one key-down (see [`key_down_plan`]).
    pub fn key_down(&self, steps: &[KeyStep]) {
        let Ok(host) = self.host() else { return };
        for step in steps {
            match step {
                KeyStep::Down(spec) => {
                    host.send_key_event(Some(&Self::key_event(*spec, KeyEventType::RAWKEYDOWN)));
                },
                KeyStep::Char(spec) => {
                    host.send_key_event(Some(&Self::key_event(*spec, KeyEventType::CHAR)));
                },
                KeyStep::Commit(text) => {
                    host.ime_commit_text(Some(&CefString::from(text.as_str())), None, 0);
                },
            }
        }
    }

    pub fn key_up(&self, codes: KeyCodes, flags: u32) {
        if let Ok(host) = self.host() {
            let spec = KeySpec { codes, character: codes.character, flags };
            host.send_key_event(Some(&Self::key_event(spec, KeyEventType::KEYUP)));
        }
    }

    /// The page's pixels if they changed since the last call: `(w, h, BGRA)`
    /// with any open popup composited in. Consumes the pending repaint
    /// notice, so the next `on_paint` sends a new one.
    pub fn take_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.page.notified.store(false, Ordering::Release);
        let mut painted = lock(&self.page.painted);
        if !painted.dirty || painted.view.bgra.is_empty() {
            return None;
        }
        painted.dirty = false;
        let mut frame = painted.view.clone();
        if painted.popup_shown && !painted.popup.bgra.is_empty() {
            let (x, y) = painted.popup_at;
            frame.blit(&painted.popup, x, y);
        }
        Some((frame.w, frame.h, frame.bgra))
    }

    /// The cursor the page wants under the pointer.
    pub fn cursor(&self) -> Option<gpui::CursorStyle> {
        cursor_style(self.page.cursor.load(Ordering::Acquire))
    }

    /// Close the browser (and its DevTools window). CEF finishes the close
    /// asynchronously; nothing is painted or reported after this returns
    /// that the tab model still cares about.
    pub fn close(&self) {
        if let Ok(host) = self.host() {
            if host.has_dev_tools() != 0 {
                host.close_dev_tools();
            }
            host.close_browser(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> LayoutRect {
        LayoutRect { x, y, w, h }
    }

    #[test]
    fn flag_bits_match_the_bindings() {
        use cef::sys::cef_event_flags_t as F;
        assert_eq!(FLAG_SHIFT, F::EVENTFLAG_SHIFT_DOWN.0);
        assert_eq!(FLAG_CONTROL, F::EVENTFLAG_CONTROL_DOWN.0);
        assert_eq!(FLAG_ALT, F::EVENTFLAG_ALT_DOWN.0);
        assert_eq!(FLAG_COMMAND, F::EVENTFLAG_COMMAND_DOWN.0);
        assert_eq!(FLAG_LEFT_BUTTON, F::EVENTFLAG_LEFT_MOUSE_BUTTON.0);
        assert_eq!(FLAG_MIDDLE_BUTTON, F::EVENTFLAG_MIDDLE_MOUSE_BUTTON.0);
        assert_eq!(FLAG_RIGHT_BUTTON, F::EVENTFLAG_RIGHT_MOUSE_BUTTON.0);
        assert_eq!(FLAG_IS_REPEAT, F::EVENTFLAG_IS_REPEAT.0);
        assert_eq!(FLAG_PRECISION_SCROLL, F::EVENTFLAG_PRECISION_SCROLLING_DELTA.0);
    }

    #[test]
    fn event_flags_combine_modifiers_and_the_held_button() {
        assert_eq!(event_flags(Mods::default(), None), 0);
        let mods = Mods { shift: true, command: true, ..Mods::default() };
        assert_eq!(event_flags(mods, None), FLAG_SHIFT | FLAG_COMMAND);
        assert_eq!(
            event_flags(Mods { control: true, alt: true, ..Mods::default() }, Some(Button::Left)),
            FLAG_CONTROL | FLAG_ALT | FLAG_LEFT_BUTTON
        );
        assert_eq!(event_flags(Mods::default(), Some(Button::Right)), FLAG_RIGHT_BUTTON);
        assert_eq!(event_flags(Mods::default(), Some(Button::Middle)), FLAG_MIDDLE_BUTTON);
    }

    #[test]
    fn key_codes_cover_text_and_navigation_keys() {
        assert_eq!(key_codes("a"), Some(KeyCodes { native: 0, windows: 65, character: 97 }));
        assert_eq!(key_codes("z"), Some(KeyCodes { native: 6, windows: 90, character: 122 }));
        assert_eq!(key_codes("1"), Some(KeyCodes { native: 18, windows: 49, character: 49 }));
        assert_eq!(key_codes("0"), Some(KeyCodes { native: 29, windows: 48, character: 48 }));
        assert_eq!(key_codes("/"), Some(KeyCodes { native: 44, windows: 191, character: 47 }));
        assert_eq!(key_codes("enter"), Some(KeyCodes { native: 36, windows: 13, character: 13 }));
        assert_eq!(key_codes("backspace").unwrap().native, 51);
        assert_eq!(key_codes("left"), Some(KeyCodes { native: 123, windows: 37, character: 0xf702 }));
        assert_eq!(key_codes("f1"), Some(KeyCodes { native: 122, windows: 112, character: 0xf704 }));
        assert_eq!(key_codes("f12").unwrap().windows, 123);
        // Every letter has a distinct virtual key.
        let mut seen = std::collections::HashSet::new();
        for c in 'a'..='z' {
            assert!(seen.insert(key_codes(&c.to_string()).unwrap().native));
        }
        for unknown in ["", "é", "f0", "f13", "fn", "media-play"] {
            assert_eq!(key_codes(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn edit_commands_are_the_plain_command_chords() {
        let cmd = Mods { command: true, ..Mods::default() };
        assert_eq!(edit_command("c", cmd), Some(EditCommand::Copy));
        assert_eq!(edit_command("x", cmd), Some(EditCommand::Cut));
        assert_eq!(edit_command("v", cmd), Some(EditCommand::Paste));
        assert_eq!(edit_command("a", cmd), Some(EditCommand::SelectAll));
        assert_eq!(edit_command("z", cmd), Some(EditCommand::Undo));
        assert_eq!(edit_command("z", Mods { shift: true, ..cmd }), Some(EditCommand::Redo));
        // ⇧⌘C is the app's Copy link, and a bare key is typing.
        assert_eq!(edit_command("c", Mods { shift: true, ..cmd }), None);
        assert_eq!(edit_command("c", Mods::default()), None);
        assert_eq!(edit_command("c", Mods { alt: true, ..cmd }), None);
        assert_eq!(edit_command("r", cmd), None);
    }

    #[test]
    fn key_down_plan_types_text_and_keeps_chords_silent() {
        // A plain letter: key-down then the character it types.
        let plan = key_down_plan("a", Some("a"), Mods::default(), false);
        let codes = key_codes("a").unwrap();
        assert_eq!(
            plan,
            vec![
                KeyStep::Down(KeySpec { codes, character: 97, flags: 0 }),
                KeyStep::Char(KeySpec { codes, character: 97, flags: 0 }),
            ]
        );
        // Shift types the upper-case character on the same key.
        let shift = Mods { shift: true, ..Mods::default() };
        let plan = key_down_plan("a", Some("A"), shift, false);
        assert_eq!(plan[1], KeyStep::Char(KeySpec { codes, character: 65, flags: FLAG_SHIFT }));
        // ↩ types a carriage return even without a key_char.
        let plan = key_down_plan("enter", None, Mods::default(), false);
        assert!(matches!(plan[1], KeyStep::Char(KeySpec { character: 13, .. })));
        // Navigation keys and ⌘ / ⌃ chords are a key-down only.
        assert_eq!(key_down_plan("left", None, Mods::default(), false).len(), 1);
        let cmd = Mods { command: true, ..Mods::default() };
        assert_eq!(
            key_down_plan("r", Some("r"), cmd, false),
            vec![KeyStep::Down(KeySpec { codes: key_codes("r").unwrap(), character: 114, flags: FLAG_COMMAND })]
        );
        // A held key repeats.
        let plan = key_down_plan("a", Some("a"), Mods::default(), true);
        assert!(matches!(plan[0], KeyStep::Down(KeySpec { flags: FLAG_IS_REPEAT, .. })));
        // Text that is not one UTF-16 unit, or from an unknown key, commits.
        assert_eq!(
            key_down_plan("a", Some("😀"), Mods::default(), false).last(),
            Some(&KeyStep::Commit("😀".into()))
        );
        assert_eq!(
            key_down_plan("é", Some("é"), Mods::default(), false),
            vec![KeyStep::Commit("é".into())]
        );
        assert!(key_down_plan("unknown", None, Mods::default(), false).is_empty());
    }

    #[test]
    fn page_frame_applies_only_the_dirty_rects() {
        let mut frame = PageFrame::default();
        let first = vec![1u8; 4 * 3 * 4];
        // The first buffer is taken whole, whatever the dirty list says.
        assert!(frame.apply(&first, 4, 3, &[DirtyRect { x: 0, y: 0, w: 1, h: 1 }]));
        assert_eq!((frame.w, frame.h), (4, 3));
        assert_eq!(frame.bgra, first);
        // Same size: only the dirty rect is copied.
        let second = vec![2u8; 4 * 3 * 4];
        assert!(frame.apply(&second, 4, 3, &[DirtyRect { x: 1, y: 1, w: 2, h: 1 }]));
        for y in 0..3usize {
            for x in 0..4usize {
                let want = if y == 1 && (1..3).contains(&x) { 2 } else { 1 };
                assert_eq!(frame.bgra[(y * 4 + x) * 4], want, "({x}, {y})");
            }
        }
        // Out-of-range rects are clipped, never a panic.
        let third = vec![3u8; 4 * 3 * 4];
        assert!(frame.apply(
            &third,
            4,
            3,
            &[DirtyRect { x: -5, y: 2, w: 7, h: 99 }, DirtyRect { x: 9, y: 9, w: 4, h: 4 }]
        ));
        assert_eq!(frame.bgra[(2 * 4) * 4], 3);
        assert_eq!(frame.bgra[(2 * 4 + 1) * 4], 3);
        assert_eq!(frame.bgra[(2 * 4 + 2) * 4], 1);
        // A new size replaces the frame; a short buffer is refused.
        let resized = vec![4u8; 2 * 2 * 4];
        assert!(frame.apply(&resized, 2, 2, &[DirtyRect { x: 0, y: 0, w: 1, h: 1 }]));
        assert_eq!((frame.w, frame.h, frame.bgra.len()), (2, 2, 16));
        assert!(frame.bgra.iter().all(|&b| b == 4));
        assert!(!frame.apply(&[0u8; 5], 2, 2, &[]));
        assert!(!frame.apply(&[], 0, 0, &[]));
        assert!(frame.bgra.iter().all(|&b| b == 4));
    }

    #[test]
    fn blit_clips_the_popup_to_the_frame() {
        let mut frame = PageFrame { w: 4, h: 4, bgra: vec![0; 64] };
        let popup = PageFrame { w: 2, h: 2, bgra: vec![9; 16] };
        frame.blit(&popup, 3, 3);
        frame.blit(&popup, -1, -1);
        let at = |x: usize, y: usize| frame.bgra[(y * 4 + x) * 4];
        assert_eq!(at(3, 3), 9);
        assert_eq!(at(0, 0), 9);
        assert_eq!(at(1, 1), 0);
        assert_eq!(at(2, 3), 0);
        assert_eq!(frame.bgra.iter().filter(|&&b| b == 9).count(), 8);
        // Entirely outside: untouched.
        let before = frame.clone();
        frame.blit(&popup, 10, 0);
        frame.blit(&popup, 0, -5);
        assert_eq!(frame, before);
    }

    #[test]
    fn view_size_covers_the_placement_at_any_scale() {
        assert_eq!(view_size(rect(0.0, 0.0, 1600.0, 1200.0), 2.0), (800, 600));
        // An odd physical size rounds up, so `size * scale` covers it.
        assert_eq!(view_size(rect(0.0, 0.0, 1601.0, 1199.0), 2.0), (801, 600));
        assert_eq!(view_size(rect(0.0, 0.0, 800.0, 600.0), 1.0), (800, 600));
        assert_eq!(view_size(rect(0.0, 0.0, 0.0, 0.5), 2.0), (1, 1));
        assert_eq!(view_size(rect(0.0, 0.0, 300.0, 200.0), 0.0), (300, 200));
        for (w, scale) in [(1001.0, 2.0), (777.0, 1.5), (640.0, 3.0)] {
            let (logical, _) = view_size(rect(0.0, 0.0, w, 10.0), scale);
            assert!(logical as f32 * scale >= w);
        }
    }

    #[test]
    fn page_point_is_relative_to_the_placement_in_logical_pixels() {
        let bounds = rect(200.0, 100.0, 800.0, 600.0);
        assert_eq!(page_point(bounds, 2.0, 200.0, 100.0), (0, 0));
        assert_eq!(page_point(bounds, 2.0, 301.0, 150.0), (50, 25));
        assert_eq!(page_point(bounds, 1.0, 301.0, 150.0), (101, 50));
        // Outside the page (a drag that left it) goes negative.
        assert_eq!(page_point(bounds, 2.0, 190.0, 90.0), (-5, -5));
        assert!(in_bounds(bounds, 200.0, 100.0));
        assert!(in_bounds(bounds, 999.9, 699.9));
        assert!(!in_bounds(bounds, 1000.0, 400.0));
        assert!(!in_bounds(bounds, 199.9, 400.0));
    }

    #[test]
    fn wheel_and_zoom_units() {
        assert_eq!(wheel_delta(false, 3.4, -12.6), (3, -13));
        assert_eq!(wheel_delta(true, 0.0, 1.0), (0, 40));
        assert_eq!(wheel_delta(true, -2.0, 0.5), (-80, 20));
        assert!(zoom_level(1.0).abs() < 1e-9);
        assert!((zoom_level(1.2) - 1.0).abs() < 1e-9);
        assert!((zoom_level(1.44) - 2.0).abs() < 1e-9);
        assert!(zoom_level(0.5) < 0.0);
        for factor in [0.5, 1.0, 1.1, 1.5, 3.0] {
            assert!((zoom_factor(zoom_level(factor)) - factor).abs() < 1e-9);
        }
    }

    #[test]
    fn shifted_keys_use_their_base_key_and_imply_shift() {
        // ⇧1 arrives from gpui as "!" with no shift modifier.
        let bang = key_codes("!").unwrap();
        let one = key_codes("1").unwrap();
        assert_eq!((bang.native, bang.windows), (one.native, one.windows));
        assert_eq!(bang.character, '!' as u16);
        assert_eq!(key_codes("?").unwrap().native, key_codes("/").unwrap().native);
        assert_eq!(key_codes("A").unwrap().windows, key_codes("a").unwrap().windows);
        assert_eq!(implied_shift("?"), FLAG_SHIFT);
        assert_eq!(implied_shift("/"), 0);
        assert_eq!(implied_shift("enter"), 0);
        assert!(key_codes("é").is_none());
        let steps = key_down_plan("?", Some("?"), Mods::default(), false);
        assert!(matches!(
            steps.as_slice(),
            [KeyStep::Down(down), KeyStep::Char(_)] if down.flags & FLAG_SHIFT != 0
        ));
    }

    #[test]
    fn cursor_styles_map_where_gpui_has_one() {
        use cef::sys::cef_cursor_type_t as T;
        assert_eq!(cursor_style(T::CT_POINTER as u32), Some(gpui::CursorStyle::Arrow));
        assert_eq!(cursor_style(T::CT_HAND as u32), Some(gpui::CursorStyle::PointingHand));
        assert_eq!(cursor_style(T::CT_IBEAM as u32), Some(gpui::CursorStyle::IBeam));
        assert_eq!(
            cursor_style(T::CT_EASTWESTRESIZE as u32),
            Some(gpui::CursorStyle::ResizeLeftRight)
        );
        assert_eq!(cursor_style(T::CT_WAIT as u32), None);
        assert_eq!(cursor_style(T::CT_CUSTOM as u32), None);
        assert_eq!(cursor_style(9999), None);
    }

    #[test]
    fn bundle_paths_resolve_inside_an_app_only() {
        let paths = bundle_paths(Path::new("/Applications/Pwrde.app/Contents/MacOS/pwrde")).unwrap();
        assert_eq!(paths.bundle, Path::new("/Applications/Pwrde.app"));
        assert_eq!(
            paths.framework,
            Path::new("/Applications/Pwrde.app/Contents/Frameworks/Chromium Embedded Framework.framework")
        );
        assert_eq!(
            paths.helper,
            Path::new(
                "/Applications/Pwrde.app/Contents/Frameworks/Pwrde Helper.app/Contents/MacOS/Pwrde Helper"
            )
        );
        assert_eq!(bundle_paths(Path::new("/src/pwrde/target/debug/pwrde")), None);
        assert_eq!(bundle_paths(Path::new("/x/Contents/MacOS/pwrde")), None);
        assert_eq!(bundle_paths(Path::new("pwrde")), None);
    }

    #[test]
    fn cache_dir_is_worktree_scoped_like_the_state_db() {
        let data = Path::new("/data");
        assert_eq!(cache_dir_in(data, None), Path::new("/data/pwrde/cef"));
        assert_eq!(
            cache_dir_in(data, Some("feature")),
            Path::new("/data/pwrde/worktrees/feature/cef")
        );
    }

    #[test]
    fn popups_navigate_only_to_web_urls() {
        assert_eq!(
            popup_navigation("https://example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(popup_navigation("about:blank"), None);
        assert_eq!(popup_navigation("javascript:alert(1)"), None);
        assert_eq!(popup_navigation(""), None);
    }
}
