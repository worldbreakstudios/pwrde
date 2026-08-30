//! Command-bus execution: the one place every [`bus::Command`] turns into an
//! app mutation. The ⌘P palette, the rebindable keyboard `Action`s and the
//! `pwrde-cli` socket all funnel through [`App::execute`], so anything the
//! palette can do, the CLI (and an agent driving it) can do too.
//!
//! This is a child module of `main.rs` so it can reach `App`'s private fields
//! and helpers; the wire protocol itself lives in the crate-independent
//! [`crate::bus`] module (shared with the CLI binary via `#[path]`).
//!
//! Screenshots snapshot our own `NSWindow` in-process through CoreGraphics
//! (`CGWindowListCreateImage` on the window's `windowNumber`), which needs no
//! Screen Recording grant for the calling process's own windows; only the
//! `screencapture` fallback asks macOS for that permission for whatever
//! launched pwrde (`Pwrde.app`, or the terminal running `cargo run`). Window
//! resizes set the `NSWindow` frame directly; gpui picks the change up from
//! the normal `windowDidResize` notification.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::bus::{self, Command, Reply};
use crate::pages::{Action, Page};
use crate::{App, pwrspace, workspace};

impl App {
    /// Drain-loop entry: execute `cmd` and send exactly one reply.
    pub(crate) fn execute_bus(&mut self, cmd: Command, reply: Sender<Reply>) {
        // `quit` never returns: answer first so the client isn't left waiting.
        if let Command::Action { name } = &cmd
            && Action::from_name(name) == Some(Action::Quit)
        {
            let _ = reply.send(Reply::success(None));
        }
        let r = self.execute(cmd);
        let _ = reply.send(r);
    }

    /// Execute one bus command against the app state. Every arm either
    /// mutates through an existing `App` method or reports why it can't.
    pub(crate) fn execute(&mut self, cmd: Command) -> Reply {
        match cmd {
            Command::Ping => Reply::success(json!("pong")),
            Command::ListCommands => {
                let specs: Vec<Value> = bus::command_specs()
                    .iter()
                    .map(|s| json!({ "name": s.name, "args": s.args, "help": s.help }))
                    .collect();
                let actions: Vec<Value> = Action::ALL
                    .iter()
                    .map(|a| {
                        json!({
                            "name": a.name(),
                            "label": a.label(),
                            "binding": a.binding().display(),
                        })
                    })
                    .collect();
                Reply::success(json!({ "commands": specs, "actions": actions }))
            }
            Command::Action { name } => match Action::from_name(&name) {
                Some(action) => {
                    if self.run_action(action) {
                        Reply::success(None)
                    } else if self.page != Page::Sessions {
                        Reply::err(format!(
                            "{name:?} only applies on the Sessions page (currently {})",
                            page_name(self.page)
                        ))
                    } else {
                        Reply::err(format!("{name:?} needs an open session; none is open"))
                    }
                }
                None => Reply::err(format!(
                    "unknown action {name:?}; see `pwrde-cli commands` for the list"
                )),
            },
            Command::State => Reply::success(self.state_json()),
            // A registered CLI tool's page answers to `tool:<index>` or to
            // the tool's name (case-insensitive), e.g. `cleanup`.
            Command::GoToPage { page } => match resolve_page(&page, &self.tools) {
                Some(page) => {
                    self.set_page(page);
                    Reply::success(None)
                }
                None => Reply::err(format!(
                    "unknown page {page:?}; expected one of sessions, pull_requests, notes, settings, tool:<n> or a registered tool's name"
                )),
            },
            Command::NewSession { cwd, base, layout } => self.bus_new_session(cwd, base, layout),
            Command::SendText { text, group } => {
                let idx = match group {
                    Some(g) => match self.find_group(&g) {
                        Some(i) => i,
                        None => return Reply::err(format!("no group matches {g:?}")),
                    },
                    None => self.active,
                };
                if self.is_empty_state() {
                    return Reply::err("no session is open");
                }
                let Some(tab) = self.workspaces[idx].focused().and_then(|t| t.active_tab()) else {
                    return Reply::err("the target group has no focused pane");
                };
                // Raw keystrokes, not a bracketed paste: `--enter` appends
                // "\r", and control bytes (^C, escapes) pass through.
                let session_id = tab.session.id;
                tab.session.write(text.as_bytes());
                tab.session.scroll_to_bottom();
                tab.session.clear_selection();
                self.request_redraw();
                Reply::success(json!({ "group": idx, "session": session_id }))
            }
            Command::FocusGroup { group } => match self.find_group(&group) {
                Some(idx) => {
                    self.active = idx;
                    if self.page != Page::Sessions {
                        self.set_page(Page::Sessions);
                    }
                    workspace::ensure_active_section_expanded(
                        &self.workspaces,
                        &mut self.sections,
                        self.active,
                    );
                    self.sync_layout();
                    self.request_redraw();
                    self.persist_snapshot();
                    Reply::success(json!({ "group": idx }))
                }
                None => Reply::err(format!("no group matches {group:?}")),
            },
            Command::NewSection { name } => {
                let id = self.new_section(name);
                Reply::success(json!({ "section": id }))
            }
            Command::MoveGroupToSection { group, section } => {
                let Some(from) = self.find_group(&group) else {
                    return Reply::err(format!("no group matches {group:?}"));
                };
                let Some(sid) = self.find_section(&section) else {
                    return Reply::err(format!("no section matches {section:?}"));
                };
                let new_idx = workspace::append_to_section(&mut self.workspaces, from, sid);
                self.active = workspace::track_index_after_relocate(self.active, from, new_idx);
                workspace::ensure_active_section_expanded(
                    &self.workspaces,
                    &mut self.sections,
                    self.active,
                );
                self.sync_layout();
                self.request_redraw();
                self.persist_snapshot();
                Reply::success(json!({ "group": new_idx, "section": sid }))
            }
            Command::ResizeWindow { width, height } => {
                if !(width.is_finite() && height.is_finite() && width >= 200.0 && height >= 150.0) {
                    return Reply::err("width/height must be finite and at least 200×150");
                }
                match resize_main_window(width, height) {
                    Ok(()) => Reply::success(json!({ "width": width, "height": height })),
                    Err(e) => Reply::err(e),
                }
            }
            Command::Screenshot { path, clipboard } => {
                if clipboard && path.is_some() {
                    return Reply::err("screenshot: give a path or --clipboard, not both");
                }
                let target = if clipboard {
                    ScreenshotTarget::Clipboard
                } else {
                    ScreenshotTarget::File(path.unwrap_or_else(default_screenshot_path))
                };
                match screenshot_main_window(&target) {
                    Ok(()) => match target {
                        ScreenshotTarget::Clipboard => Reply::success(json!("clipboard")),
                        ScreenshotTarget::File(p) => Reply::success(json!(p.to_string_lossy())),
                    },
                    Err(e) => Reply::err(e),
                }
            }
        }
    }

    /// The `Screenshot to clipboard` / `Screenshot to file` actions: same
    /// capture as the bus command, reported through the status message.
    pub(crate) fn screenshot_action(&mut self, clipboard: bool) {
        let target = if clipboard {
            ScreenshotTarget::Clipboard
        } else {
            ScreenshotTarget::File(default_screenshot_path())
        };
        self.message = Some(match screenshot_main_window(&target) {
            Ok(()) => match target {
                ScreenshotTarget::Clipboard => ("Screenshot copied to clipboard".into(), false),
                ScreenshotTarget::File(p) => {
                    (format!("Screenshot saved to {}", p.display()), false)
                }
            },
            Err(e) => (format!("Screenshot failed: {e}"), true),
        });
        self.request_redraw();
    }

    /// Create an empty sidebar section (folder) named `name` at the bottom
    /// and return its id. The `New folder` action opens it for renaming.
    pub(crate) fn new_section(&mut self, name: String) -> u64 {
        let id = self.next_section_id;
        self.next_section_id += 1;
        let name = if name.trim().is_empty() {
            "folder".to_string()
        } else {
            name.trim().to_string()
        };
        self.sections.push(workspace::Section {
            id,
            name,
            emoji: String::new(),
            collapsed: false,
            anchor: None,
        });
        workspace::normalize_section_anchors(&self.workspaces, &mut self.sections);
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
        id
    }

    /// `New folder` from the palette / keyboard: create it and start a rename.
    pub(crate) fn new_section_action(&mut self) {
        let id = self.new_section("folder".into());
        self.editing_section = Some((id, "folder".into()));
    }

    fn bus_new_session(
        &mut self,
        cwd: PathBuf,
        base: Option<String>,
        layout: Option<String>,
    ) -> Reply {
        if !cwd.is_dir() {
            return Reply::err(format!("{} is not a directory", cwd.display()));
        }
        let name = cwd
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
        let profile = match layout {
            None => None,
            Some(wanted) => {
                let found = pwrspace::discover(&pwrspace::candidate_paths(&cwd));
                match found
                    .into_iter()
                    .find(|(p, _)| p.name.eq_ignore_ascii_case(&wanted))
                {
                    Some((p, _)) => Some(p),
                    None => {
                        return Reply::err(format!(
                            "no workspace profile named {wanted:?} for {}",
                            cwd.display()
                        ));
                    }
                }
            }
        };
        match base {
            // Open the directory itself.
            None => match profile {
                Some(profile) => self.add_group_with_profile(name.clone(), Some(cwd), &profile),
                None => self.add_group(name.clone(), Some(cwd)),
            },
            // Fork a fresh worktree through `drop`; the group appears once
            // `GroupReady` comes back. `default` forks off the remote default
            // branch, anything else is passed as `--from`.
            Some(from) => {
                if !cwd.join(".git").exists() {
                    return Reply::err(format!(
                        "{} is not a git checkout; drop `base`",
                        cwd.display()
                    ));
                }
                self.pending_group_profile = profile;
                let from = if from.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(from)
                };
                self.start_fork(cwd, name.clone(), from);
                return Reply::success(json!({ "name": name, "provisioning": true }));
            }
        }
        Reply::success(json!({ "name": name, "group": self.active }))
    }

    /// Resolve a group by exact name, case-insensitive name, sidebar title,
    /// or 0-based index.
    fn find_group(&self, key: &str) -> Option<usize> {
        if self.is_empty_state() {
            return None;
        }
        if let Ok(i) = key.parse::<usize>() {
            return (i < self.workspaces.len()).then_some(i);
        }
        self.workspaces
            .iter()
            .position(|w| w.name == key)
            .or_else(|| {
                self.workspaces
                    .iter()
                    .position(|w| w.name.eq_ignore_ascii_case(key))
            })
            .or_else(|| {
                self.workspaces
                    .iter()
                    .position(|w| w.title().eq_ignore_ascii_case(key))
            })
    }

    /// Resolve a section by exact name, case-insensitive name, or numeric id.
    fn find_section(&self, key: &str) -> Option<u64> {
        if let Ok(id) = key.parse::<u64>()
            && self.sections.iter().any(|s| s.id == id)
        {
            return Some(id);
        }
        self.sections
            .iter()
            .find(|s| s.name == key)
            .or_else(|| {
                self.sections
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(key))
            })
            .map(|s| s.id)
    }

    /// A JSON snapshot of what the app is showing, for agents to inspect.
    fn state_json(&self) -> Value {
        let groups: Vec<Value> = if self.is_empty_state() {
            Vec::new()
        } else {
            self.workspaces
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let tiles: Vec<Value> = w
                        .root
                        .tiles()
                        .into_iter()
                        .map(|t| {
                            let tabs: Vec<Value> = t
                                .tabs
                                .iter()
                                .enumerate()
                                .map(|(ti, tab)| {
                                    json!({
                                        "session": tab.session.id,
                                        "title": tab.session.title(),
                                        "active": ti == t.active,
                                        "unread": tab.unread,
                                        "cols": tab.cols,
                                        "rows": tab.rows,
                                    })
                                })
                                .collect();
                            json!({ "id": t.id, "focused": t.id == w.focused_tile, "tabs": tabs })
                        })
                        .collect();
                    json!({
                        "index": i,
                        "name": w.name,
                        "title": w.title(),
                        "cwd": w.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
                        "section": w.section,
                        "active": i == self.active,
                        "tiles": tiles,
                    })
                })
                .collect()
        };
        let sections: Vec<Value> = self
            .sections
            .iter()
            .map(|s| json!({ "id": s.id, "name": s.name, "emoji": s.emoji, "collapsed": s.collapsed }))
            .collect();
        json!({
            "page": page_name(self.page),
            "active_group": if self.is_empty_state() { Value::Null } else { json!(self.active) },
            "groups": groups,
            "sections": sections,
            "command_palette_open": self.command.is_some(),
            "message": self.message.as_ref().map(|(m, _)| m.clone()),
        })
    }
}

pub(crate) fn page_from_name(name: &str) -> Option<Page> {
    match name.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "sessions" => Some(Page::Sessions),
        "pull_requests" | "prs" | "pullrequests" => Some(Page::PullRequests),
        "notes" => Some(Page::Notes),
        "settings" => Some(Page::Settings),
        s => s.strip_prefix("tool:").and_then(|n| n.parse().ok()).map(Page::Tool),
    }
}

/// A page token resolved against the registered tools: the fixed names and
/// `tool:<n>` via [`page_from_name`] (rejecting an unregistered index, so a
/// client learns its navigation failed instead of landing on Sessions), or a
/// tool's name, case-insensitively.
pub(crate) fn resolve_page(name: &str, tools: &[crate::cli_tools::CliTool]) -> Option<Page> {
    match page_from_name(name) {
        Some(Page::Tool(i)) => (i < tools.len()).then_some(Page::Tool(i)),
        Some(page) => Some(page),
        None => tools
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(name.trim()))
            .map(Page::Tool),
    }
}

pub(crate) fn page_name(page: Page) -> String {
    match page {
        Page::Sessions => "sessions".into(),
        Page::PullRequests => "pull_requests".into(),
        Page::Tool(i) => format!("tool:{i}"),
        Page::Notes => "notes".into(),
        Page::Settings => "settings".into(),
    }
}

enum ScreenshotTarget {
    Clipboard,
    File(PathBuf),
}

/// `<tmp>/pwrde-<unix-millis>.png`.
fn default_screenshot_path() -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("pwrde-{ms}.png"))
}

/// Capture the main window. In-process first: `CGWindowListCreateImage` on
/// one of our own windows needs no Screen Recording grant (the exemption is
/// per calling process, so it works from `cargo run` and the bundle alike);
/// PNG encoding and the pasteboard go through AppKit. If CoreGraphics hands
/// back nothing, fall back to `screencapture`, which does need the grant.
/// Synchronous: a single-window capture takes well under 100 ms, and the bus
/// reply stays honest about whether the file exists.
fn screenshot_main_window(target: &ScreenshotTarget) -> Result<(), String> {
    let number = main_window_number()?;
    if let ScreenshotTarget::File(path) = target
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    match capture_window_in_process(number, target) {
        Ok(()) => return Ok(()),
        Err(e) => eprintln!("bus: in-process capture failed ({e}); trying screencapture"),
    }
    let mut cmd = std::process::Command::new("screencapture");
    // -l <id>: that window only; -x: no shutter sound; -o: no window shadow.
    cmd.arg("-l").arg(number.to_string()).arg("-x").arg("-o");
    match target {
        ScreenshotTarget::Clipboard => {
            cmd.arg("-c");
        }
        ScreenshotTarget::File(path) => {
            cmd.arg(path);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run screencapture: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("screencapture exited with {}", out.status)
        } else {
            err
        });
    }
    if let ScreenshotTarget::File(path) = target
        && !path.exists()
    {
        return Err(format!(
            "screencapture wrote nothing to {} — grant Screen Recording to the app that launched pwrde (System Settings › Privacy & Security)",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod cg {
    //! Just enough CoreGraphics to snapshot one window by id.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
    }
    pub type CGImageRef = *mut std::ffi::c_void;
    /// `kCGWindowListOptionIncludingWindow`.
    pub const INCLUDING_WINDOW: u32 = 1 << 3;
    /// `kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution`:
    /// content only (no shadow), at the display's backing scale.
    pub const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGWindowListCreateImage(
            bounds: CGRect,
            option: u32,
            window_id: u32,
            image_option: u32,
        ) -> CGImageRef;
        pub fn CGImageRelease(image: CGImageRef);
        pub fn CGImageGetWidth(image: CGImageRef) -> usize;
        pub static CGRectNull: CGRect;
    }
}

#[cfg(target_os = "macos")]
fn capture_window_in_process(number: i64, target: &ScreenshotTarget) -> Result<(), String> {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};
    let id = u32::try_from(number).map_err(|_| format!("bad window number {number}"))?;
    unsafe {
        let image = cg::CGWindowListCreateImage(
            cg::CGRectNull,
            cg::INCLUDING_WINDOW,
            id,
            cg::IMAGE_OPTIONS,
        );
        if image.is_null() || cg::CGImageGetWidth(image) == 0 {
            if !image.is_null() {
                cg::CGImageRelease(image);
            }
            return Err("CGWindowListCreateImage returned no image".into());
        }
        let result = match target {
            ScreenshotTarget::File(path) => {
                let rep: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
                let rep: *mut Object = msg_send![rep, initWithCGImage: image];
                if rep.is_null() {
                    Err("NSBitmapImageRep init failed".to_string())
                } else {
                    // NSBitmapImageFileTypePNG == 4.
                    let nil: *mut Object = std::ptr::null_mut();
                    let data: *mut Object =
                        msg_send![rep, representationUsingType: 4usize properties: nil];
                    let path_s = std::ffi::CString::new(path.to_string_lossy().as_bytes())
                        .map_err(|e| e.to_string())?;
                    let ns_path: *mut Object =
                        msg_send![class!(NSString), stringWithUTF8String: path_s.as_ptr()];
                    let ok: BOOL = if data.is_null() {
                        NO
                    } else {
                        msg_send![data, writeToFile: ns_path atomically: true]
                    };
                    let _: () = msg_send![rep, release];
                    if ok == NO {
                        Err(format!("could not write PNG to {}", path.display()))
                    } else {
                        Ok(())
                    }
                }
            }
            ScreenshotTarget::Clipboard => {
                #[repr(C)]
                struct NSSize {
                    w: f64,
                    h: f64,
                }
                let img: *mut Object = msg_send![class!(NSImage), alloc];
                let img: *mut Object =
                    msg_send![img, initWithCGImage: image size: NSSize { w: 0.0, h: 0.0 }];
                if img.is_null() {
                    Err("NSImage init failed".to_string())
                } else {
                    let pb: *mut Object = msg_send![class!(NSPasteboard), generalPasteboard];
                    let _: isize = msg_send![pb, clearContents];
                    let arr: *mut Object = msg_send![class!(NSArray), arrayWithObject: img];
                    let ok: BOOL = msg_send![pb, writeObjects: arr];
                    let _: () = msg_send![img, release];
                    if ok == NO {
                        Err("pasteboard refused the image".to_string())
                    } else {
                        Ok(())
                    }
                }
            }
        };
        cg::CGImageRelease(image);
        result
    }
}

#[cfg(not(target_os = "macos"))]
fn capture_window_in_process(_number: i64, _target: &ScreenshotTarget) -> Result<(), String> {
    Err("screenshots are macOS-only".into())
}

/// The main window's `NSWindow.windowNumber` (its CGWindowID).
#[cfg(target_os = "macos")]
fn main_window_number() -> Result<i64, String> {
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let window = main_ns_window()?;
        let number: i64 = msg_send![window, windowNumber];
        Ok(number)
    }
}

#[cfg(not(target_os = "macos"))]
fn main_window_number() -> Result<i64, String> {
    Err("screenshots are macOS-only".into())
}

/// Resize the main window's content area to `width`×`height` points,
/// keeping its top-left corner in place.
#[cfg(target_os = "macos")]
fn resize_main_window(width: f32, height: f32) -> Result<(), String> {
    use objc::{msg_send, sel, sel_impl};
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
    unsafe {
        let window = main_ns_window()?;
        let frame: CGRect = msg_send![window, frame];
        let content: CGRect = msg_send![window, contentRectForFrameRect: frame];
        let chrome_h = frame.size.height - content.size.height;
        let top = frame.origin.y + frame.size.height;
        let new_h = height as f64 + chrome_h;
        let new_frame = CGRect {
            origin: CGPoint {
                x: frame.origin.x,
                y: top - new_h,
            },
            size: CGSize {
                width: width as f64,
                height: new_h,
            },
        };
        let _: () = msg_send![window, setFrame: new_frame display: true animate: false];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn resize_main_window(_width: f32, _height: f32) -> Result<(), String> {
    Err("window resize is macOS-only".into())
}

/// The app's main `NSWindow`: `NSApp.mainWindow`, else the largest window
/// (the flyover popout is a separate, smaller window).
#[cfg(target_os = "macos")]
unsafe fn main_ns_window() -> Result<*mut objc::runtime::Object, String> {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    let cls = Class::get("NSApplication").ok_or("NSApplication class missing")?;
    let app: *mut Object = msg_send![cls, sharedApplication];
    let main: *mut Object = msg_send![app, mainWindow];
    if !main.is_null() {
        return Ok(main);
    }
    let windows: *mut Object = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    let mut best: (*mut Object, f64) = (std::ptr::null_mut(), -1.0);
    for i in 0..count {
        let w: *mut Object = msg_send![windows, objectAtIndex: i];
        #[repr(C)]
        struct R {
            x: f64,
            y: f64,
            w: f64,
            h: f64,
        }
        let r: R = msg_send![w, frame];
        let area = r.w * r.h;
        if area > best.1 {
            best = (w, area);
        }
    }
    if best.0.is_null() {
        Err("no window is open".into())
    } else {
        Ok(best.0)
    }
}

/// Where the bus listens: `~/.pwrde/bus.sock`, or the worktree-scoped
/// variant beside that worktree's settings.json.
pub(crate) fn socket_path() -> PathBuf {
    bus::socket_path(
        &crate::settings::config_dir(),
        crate::git::worktree_scope().as_deref(),
    )
}

/// Start the socket listener; every request is forwarded to the drain loop
/// as a [`crate::term::TermEvent::Bus`] and answered from the main thread.
/// Exposes the path as `PWRDE_SOCKET` so child shells (and `pwrde-cli`
/// inside them) find this instance.
pub(crate) fn start(events_tx: Sender<crate::term::TermEvent>) -> Option<PathBuf> {
    let path = socket_path();
    let forward = move |cmd: Command| -> Reply {
        let (tx, rx) = std::sync::mpsc::channel();
        if events_tx
            .send(crate::term::TermEvent::Bus { cmd, reply: tx })
            .is_err()
        {
            return Reply::err("the app is shutting down");
        }
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(reply) => reply,
            Err(_) => Reply::err("the app did not answer within 30s"),
        }
    };
    match bus::serve(path.clone(), forward) {
        Ok(_) => {
            // SAFETY: set once at startup before any session thread spawns.
            unsafe { std::env::set_var("PWRDE_SOCKET", &path) };
            Some(path)
        }
        Err(e) => {
            eprintln!("bus: could not listen on {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_names_round_trip() {
        for page in Page::all(2) {
            assert_eq!(page_from_name(&page_name(page)), Some(page));
        }
        assert_eq!(page_from_name("tool:1"), Some(Page::Tool(1)));
        assert_eq!(page_from_name("tool:x"), None);
    }

    /// Tool pages resolve by name (case-insensitive) or a registered index;
    /// an unregistered index is an error, not a silent fallback.
    #[test]
    fn tool_pages_resolve_by_name_or_registered_index() {
        let tools = crate::cli_tools::default_tools();
        assert_eq!(resolve_page("cleanup", &tools), Some(Page::Tool(0)));
        assert_eq!(resolve_page(" Cleanup ", &tools), Some(Page::Tool(0)));
        assert_eq!(resolve_page("tool:0", &tools), Some(Page::Tool(0)));
        assert_eq!(resolve_page("tool:1", &tools), None);
        assert_eq!(resolve_page("settings", &tools), Some(Page::Settings));
        assert_eq!(resolve_page("nope", &tools), None);
        assert_eq!(page_from_name("Pull-Requests"), Some(Page::PullRequests));
        assert_eq!(page_from_name("nope"), None);
    }

    #[test]
    fn default_screenshot_path_is_a_png_in_tmp() {
        let p = default_screenshot_path();
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(p.starts_with(std::env::temp_dir()));
    }
}
