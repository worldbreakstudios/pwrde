//! pwrde — a terminal workspace for macOS, rendered with gpui.
//!
//! Window layout: a gpui window with a resizable vertical-tab sidebar (one
//! tab per *group*) and a binary split tree of tiles — each tile has a
//! horizontal tab strip (cmux-style). Rendering is done by a single custom
//! gpui `Element` whose `paint()` consumes the stateless `renderer::Frame`.
//!
//! Shortcuts:
//!   ⌘D split side-by-side   ⇧⌘D split stacked    ⌘T new tab in tile
//!   ⇧⌘T new group           ⌘W close tab         ⌘1–⌘9 switch group
//!   ⌘]/⌘[ cycle tile focus  ⇧⌘]/⇧⌘[ next/prev tab   ⌘Q quit
//!   ⌘V paste                ⌘-click open link
//!
//! Port note: this file was ported from winit+wgpu to gpui. The old
//! `EventLoop`/`ApplicationHandler`/`Window` are replaced by a gpui
//! `Application`, a window, and a terminal `Element`. Terminal wakeups arrive
//! over an `mpsc` channel drained on gpui's foreground executor.

mod backdrop;
mod bus;
mod bus_exec;
mod claude_hooks;
mod cli_tools;
mod command;
mod command_ui;
mod features;
mod flow;
mod flow_ui;
mod folders_ui;
mod flyover_ui;
mod gh;
mod git;
mod git_context;
mod lfg;
mod modal_ui;
mod settings_ui;
mod links;
mod pages;
mod palette;
mod persist;
mod picker;
mod pwrspace;
mod rect;
mod renderer;
mod resize_ui;
mod save_ui;
mod settings;
mod sidebar_card;
mod sidebar_ui;
mod term;
mod tile_ui;
mod term_theme;
mod theme;
// Vendored shadcn-style component copies (see ui/mod.rs). Kept faithful to
// their rcn source rather than pruned to current usage, so the unused parts
// of the library surface are expected dead code in this bin crate.
#[allow(dead_code)]
mod ui;
mod webview;
mod webview_ui;
mod workspace;

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime};

use gpui::{
    canvas, div, px, App as GpuiApp, AppContext, Application, Bounds, Context, CursorStyle,
    FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, Keystroke, Modifiers, MouseButton,
    ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, QuitMode, Render, ShapedLine,
    Size, Styled, TextAlign, TextRun, Window, WindowBounds, WindowOptions,
    prelude::FluentBuilder,
};

use pages::{Action, Binding, Page, Section};
use renderer::Renderer;
use term::{MouseBtn, MousePhase, Session, TermEvent};
use workspace::{Dir, Node, Tab, Tile, Workspace};

// ── Layout / interaction constants (logical px) ──────────────────────────

/// Grab tolerance (logical px) for divider / sidebar-edge hits.
pub(crate) const GRAB: f32 = 4.0;
/// Pointer travel (logical px) before a tab press becomes a drag.
const DRAG_THRESHOLD: f64 = 6.0;
/// How often the sidebar cards are topped up while the user stays inside one
/// group. Comfortably longer than the cache's `FRESH_WINDOW`, so a tick only
/// walks entries that have actually aged out; a tick with nothing stale hands
/// the worker slot straight back.
const GIT_CTX_POLL: std::time::Duration = std::time::Duration::from_secs(6);
/// How often panes that still have *no name at all* — no emulator title and no
/// process-derived one — are looked up. Short, because this is exactly the
/// window after a restart in which restored shpool tabs would otherwise read
/// "wezterm", and it costs nothing once every pane has been named.
const PROC_TITLE_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// How often already-named title-less panes are re-resolved, so a tab follows
/// the program running in it. Much slower than the first-name poll: a pane
/// whose shell never sets a title stays in this sweep for the life of the app,
/// and `ps -axE` dumps every process's environment, so the steady-state cost
/// has to be an occasional sweep rather than a treadmill.
const PROC_TITLE_REFRESH: std::time::Duration = std::time::Duration::from_secs(15);
/// Consecutive sweeps a pane may fail to resolve before it drops off the fast
/// tick. Some panes can never be named — a detached shpool session whose shell
/// is gone — and they must not hold the 2s sweep open for the whole session.
const PROC_TITLE_MISS_LIMIT: usize = 3;

/// Whether a process-title sweep is due, and whether it should also re-resolve
/// the panes that already have a name (`Some(renew)`). Pure, for tests.
fn proc_title_due(
    since_poll: std::time::Duration,
    since_renew: std::time::Duration,
) -> Option<bool> {
    (since_poll >= PROC_TITLE_POLL).then_some(since_renew >= PROC_TITLE_REFRESH)
}

/// A drop landing zone resolved from the pointer during a tab drag.
#[derive(Clone, Copy, Debug)]
enum DropTarget {
    /// Insert into a tile's tab bar at `index`.
    TabBar { tile: u64, index: usize },
    /// Drop onto a tile's body → append as a tab.
    Center { tile: u64 },
    /// Drop onto a tile edge → split.
    Edge { tile: u64, dir: Dir, first: bool },
    /// Drop a terminal tab onto a group's sidebar row.
    Group { ws: usize },
    /// Insert a dragged sidebar group before workspace index `before`
    /// (len = append), assigning `section` membership and, when `pinned`
    /// is set, moving it into or out of the list's "Pinned" section. `gap`
    /// is the visual row gap the insertion line is drawn at (rows.len() =
    /// below the last row) — with pins listed first, workspace index and
    /// row order no longer coincide.
    SidebarInsert { before: usize, section: Option<u64>, pinned: Option<bool>, gap: usize },
    /// Drop on the middle (or collapsed bottom) of a section header → append.
    SidebarAppend { section_id: u64 },
    /// Re-order a folder: insert the dragged section before section index
    /// `before`; `gap` is the folders-card row gap the line previews at.
    FolderInsert { before: usize, gap: usize },
}

impl DropTarget {
    /// Whether this target's preview lands inside the sidebar panel.
    ///
    /// The panel is an opaque gpui element painted *over* the canvas, so a
    /// hint pushed as a canvas quad here would be swallowed. These variants
    /// are drawn by [`crate::sidebar_ui`] instead; the rest stay on the
    /// canvas, where nothing occludes them.
    fn in_sidebar(self) -> bool {
        matches!(
            self,
            DropTarget::Group { .. }
                | DropTarget::SidebarInsert { .. }
                | DropTarget::SidebarAppend { .. }
                | DropTarget::FolderInsert { .. }
        )
    }
}

/// A pending destructive action waiting behind the modal confirm dialog.
struct ConfirmClose {
    text: String,
    action: ConfirmAction,
}

impl ConfirmClose {
    /// The accept button's label — named for the action it performs.
    fn accept_label(&self) -> &'static str {
        match self.action {
            ConfirmAction::CloseGroup { .. } => "Close group",
            ConfirmAction::ClearWebviewData { .. } => "Clear data",
        }
    }
}

/// What the confirm dialog's accept button performs.
enum ConfirmAction {
    /// Close the group whose primary pane the user asked to close. Resolved
    /// by primary tile id at confirm time so group reordering while the
    /// dialog is up can't misdirect the close.
    CloseGroup { primary_tile: u64 },
    /// Clear the shared Wry website-data store after an explicit warning.
    ClearWebviewData { id: u64 },
}

struct WebviewPrompt {
    query: String,
    error: Option<String>,
}

/// A CLI tool page's terminal. `exited` keeps the last frame on screen after
/// the command ends so its final output stays readable; ⏎ (or revisiting the
/// page) relaunches it.
struct ToolSession {
    tab: workspace::Tab,
    exited: bool,
}

/// The Settings → Tools add form: one rcn Input per field.
struct ToolForm {
    name: gpui::Entity<crate::ui::Input>,
    command: gpui::Entity<crate::ui::Input>,
    cwd: gpui::Entity<crate::ui::Input>,
    icon: gpui::Entity<crate::ui::Input>,
}

impl ToolForm {
    fn inputs(&self) -> [&gpui::Entity<crate::ui::Input>; 4] {
        [&self.name, &self.command, &self.cwd, &self.icon]
    }
}

/// A side effect [`App::popout_key`] needs applied to a window other than
/// the popout itself (entity code can't touch foreign windows directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopoutEffect {
    /// Bring the main window forward (the picker and the docked panel
    /// render there).
    ActivateMain,
}

/// The save-as-workspace modal: two text buffers, the focused field, and —
/// once Enter moves past the fields — the destination choice.
struct SaveWorkspaceModal {
    name: String,
    description: String,
    /// 0 = Name focused, 1 = Description.
    field: usize,
    /// `Some(selected)` while the destination rows are up, `None` while the
    /// text fields are being edited.
    dest_selected: Option<usize>,
    /// Row labels for the destination stage, parallel to `dest_paths`.
    dest_labels: Vec<String>,
    /// The `.pwrspace.json` path each destination row writes to.
    dest_paths: Vec<std::path::PathBuf>,
}

/// The in-flight pointer drag gesture.
#[derive(Clone, Debug)]
enum Drag {
    None,
    /// Resizing the sidebar.
    Sidebar,
    /// Resizing the folders card (the band in the card/list gap).
    Folders,
    /// Folder row pressed; may become a folder drag past the threshold.
    FolderPress { si: usize, start: (f64, f64) },
    /// Dragging a folder row to re-order the folders card.
    Folder { si: usize },
    /// Resizing a split divider at `path`.
    Divider { path: Vec<u8> },
    /// A tab was pressed; may become a drag past the threshold.
    TabPress { tile: u64, tab: usize, start: (f64, f64) },
    /// A tab is being dragged.
    Tab { tile: u64, tab: usize },
    /// A text selection is being dragged inside a tile's content area.
    Select { tile: u64 },
    /// A text selection is being dragged inside the flyover panel.
    FlyoverSelect,
    /// A text selection is being dragged inside a tool page's terminal.
    ToolSelect,
    /// The flyover panel's top edge is being dragged to resize it.
    FlyoverResize,
    /// Sidebar group row pressed; may become a group drag past threshold.
    GroupPress { ws: usize, start: (f64, f64) },
    /// Dragging a sidebar workspace group tab.
    Group { ws: usize },
}

/// Which pane a forwarded mouse report belongs to — a tile in the active
/// group, or the flyover panel's active tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MouseLoc {
    Tile(u64),
    Flyover,
    /// The active tool page's terminal.
    Tool,
}

/// An in-flight mouse-button grab by a tracking TUI: which pane got the
/// press and which buttons are still held (bit 0 = left, 1 = middle,
/// 2 = right), so motion/release forward to that same pane until every
/// button lifts.
#[derive(Clone, Copy)]
struct MouseReport {
    loc: MouseLoc,
    buttons: u8,
}

impl MouseReport {
    fn bit(btn: MouseBtn) -> u8 {
        match btn {
            MouseBtn::Left => 1,
            MouseBtn::Middle => 1 << 1,
            MouseBtn::Right => 1 << 2,
        }
    }
}

/// The whole application state. Under gpui this is the `Entity` that owns the
/// terminal `Element` and all workspaces.
struct App {
    /// Wakeups from PTY reader threads are drained from here per frame.
    events_rx: Receiver<TermEvent>,
    /// Cloned into each spawned `Session` so its reader thread can wake us.
    events_tx: Sender<TermEvent>,
    /// Stateless metrics/color/layout helper. Created once we know the scale.
    renderer: Renderer,
    workspaces: Vec<Workspace>,
    active: usize,
    /// Collapsible sidebar sections. Membership is on each `Workspace.section`.
    sections: Vec<workspace::Section>,
    /// Monotonic id counter for newly created sidebar sections.
    next_section_id: u64,
    /// Per-group git/PR aggregates behind a 5s stale-while-revalidate cache.
    /// The paint path only ever *reads* it (`get`); every write arrives as a
    /// `TermEvent::GitContextReady` from the refresh worker.
    git_contexts: git_context::GitContextCache,
    /// True while the serial git-context worker is alive, so a burst of
    /// refresh triggers cannot fan out into N simultaneous `gh` calls.
    git_ctx_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Guards the ⇧⌘G fallback lookup (`open_pr_in_github`): one `pr list`
    /// in flight at a time, so repeated presses cannot pop several tabs.
    open_pr_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// A refresh trigger that arrived while the worker was busy. Remembered
    /// rather than dropped, and re-fired once the worker is done.
    git_ctx_pending: bool,
    /// When the periodic sidebar-card poll last fired. Cards quote a working
    /// tree that changes under us (commits, dirty counts), so a group the user
    /// never leaves still has to be topped up on a timer.
    git_ctx_polled_at: std::time::Instant,
    /// True while the process-title sweep is alive, so the throttle can never
    /// stack `ps` sweeps on top of each other.
    proc_title_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When the first-name process-title poll last fired.
    proc_title_polled_at: std::time::Instant,
    /// When the slower re-resolve of already-named title-less panes last fired.
    proc_title_refreshed_at: std::time::Instant,
    next_session_id: u64,
    next_tile_id: u64,
    next_webview_id: u64,
    /// Native child views stay on the foreground thread with the gpui window.
    webviews: webview::Manager,
    /// In-app New webview URL prompt, sharing the command palette input.
    webview_prompt: Option<WebviewPrompt>,
    /// One live address field is mounted in the focused webview toolbar.
    webview_address: gpui::Entity<crate::ui::Input>,
    webview_address_for: Option<u64>,
    /// Find-in-page replaces the address field while active.
    webview_find: gpui::Entity<crate::ui::Input>,
    webview_find_for: Option<u64>,
    /// Expanded site-information or browser-tools panel.
    webview_panel: Option<webview_ui::Panel>,
    /// Sidebar width when expanded, logical px (user-resizable).
    sidebar_expanded_w: f32,
    /// Whether the sidebar is collapsed (⌘S toggle). Session-only, like the
    /// width; layout treats the effective width 0 as the collapsed state.
    sidebar_collapsed: bool,
    folders_open: bool,
    folder_filter: Option<u64>,
    /// Folders card width, logical px (user-resizable; session-only like
    /// the sidebar width).
    folders_w: f32,
    /// Wheel scroll of the sessions rows / the folders-card rows, logical px
    /// (clamped on read — see `sessions_scroll` / `folders_scroll`).
    sessions_scroll: f32,
    folders_scroll: f32,
    /// The folders card's "Pinned tools" run is folded (`sidebar.tools_collapsed`).
    tools_collapsed: bool,
    /// The sessions list's "Pinned" run is folded (`sidebar.pinned_collapsed`).
    pinned_collapsed: bool,
    /// The spot the native traffic lights were last positioned for
    /// (`workspace::traffic_light_spot`); `render` re-syncs on change.
    traffic_lights_for: Option<workspace::TrafficLightSpot>,
    modifiers: Modifiers,
    title: String,
    cursor: (f64, f64),
    drag: Drag,
    /// The pane currently receiving forwarded mouse-button reports (a
    /// mouse-tracking TUI holding a button), or `None`. Kept separate from
    /// `drag` so motion and release reach the pane that got the press even
    /// after the cursor leaves it, and so a right/middle press mid-left-drag
    /// doesn't clobber the drag state.
    mouse_report: Option<MouseReport>,
    /// Tile expanded by the first click of a potential double-click. The
    /// second click's bar double-click-to-collapse is suppressed for it, so
    /// double-clicking a collapsed pane's tab doesn't snap it shut again.
    just_expanded: Option<u64>,
    /// Profile chosen for a group whose `drop` worktree is still provisioning;
    /// applied on `GroupReady`, dropped on `GroupFailed`.
    pending_group_profile: Option<pwrspace::WorkspaceProfile>,
    /// Sidebar section chosen for that same provisioning group; the section
    /// already exists (a "New folder…" pick creates it at launch), only the
    /// membership waits for `GroupReady`.
    pending_group_section: Option<u64>,
    /// The open save-as-workspace modal, or `None`.
    save_ws: Option<SaveWorkspaceModal>,
    /// The save modal's Name / Description fields: rcn text inputs whose
    /// text is mirrored into `save_ws` (see `save_ui`).
    save_name: gpui::Entity<crate::ui::Input>,
    save_desc: gpui::Entity<crate::ui::Input>,
    /// Raised whenever the save modal's state changes under the fields
    /// (open, Tab/Enter, a refused empty name): the next render seeds the
    /// inputs from the modal and moves focus to `SaveWorkspaceModal::field`.
    save_sync: bool,
    /// The unified command palette (root commands and the New-session flow),
    /// or `None` when closed. See `command` / `command_ui`.
    command: Option<command::CommandPalette>,
    /// The palette list's scroll position, so keyboard navigation can keep
    /// the selected row in view (`command_scroll_to`) while the wheel still
    /// scrolls freely between key presses.
    command_scroll: gpui::ScrollHandle,
    command_scroll_to: Option<usize>,
    /// The search field the element-tree modals share (the command palette
    /// now; the pickers later): an rcn text input whose text an observer
    /// feeds into whichever modal is open.
    modal_search: gpui::Entity<crate::ui::Input>,
    /// `Some(placeholder)` right after a modal opened: the next render clears
    /// the field, sets that placeholder and focuses it (opening happens in
    /// handlers without a `Window`).
    modal_search_reset: Option<String>,
    /// A centered one-line message. `bool` is `dismissable`: false while `drop`
    /// provisions (input swallowed), true for a failure note the user can close.
    message: Option<(String, bool)>,
    /// The open close-primary-pane confirmation dialog, or `None`.
    confirm: Option<ConfirmClose>,
    /// Primary-pane sessions awaiting their auto-run command, keyed by session
    /// id. The command is written on the session's first wakeup (the shell has
    /// printed its prompt by then, so startup files can't eat the input).
    pending_primary_cmd: std::collections::HashMap<u64, String>,
    /// rcn Input entity for the Settings → Sessions primary-command row,
    /// created at App construction and seeded from settings.
    command_input: gpui::Entity<crate::ui::Input>,
    /// The Settings sidebar's search field: a real rcn text input. Its text
    /// is mirrored into `settings_query` (an observer keeps them in step) so
    /// gpui-free readers keep working, and its focus is reflected into
    /// `settings_search_focus` once per render.
    settings_search: gpui::Entity<crate::ui::Input>,
    /// Search query in the Settings sidebar search box.
    settings_query: String,
    /// Whether the Settings sidebar search box has keyboard focus.
    settings_search_focus: bool,
    /// In-progress sidebar section rename: `(section_id, buffer)`, or `None`
    /// when not editing. Enter commits via `apply_section_rename`, Esc cancels.
    editing_section: Option<(u64, String)>,
    /// The single focus handle for the terminal element. Minted once in the
    /// constructor and focused when the window opens; keyboard events only
    /// reach us while it holds focus.
    focus_handle: FocusHandle,
    /// Whether a redraw is currently needed (set by wakeups, mouse, keys).
    dirty: bool,
    /// Sub-notch wheel travel carried between scroll events so tiny deltas
    /// accumulate into whole scroll steps instead of being lost.
    scroll_accum: f64,
    /// Registered CLI tool pages (see [`cli_tools`]), mirrored from settings
    /// on each change so the folders card and page cycling read one snapshot.
    tools: Vec<cli_tools::CliTool>,
    /// One slot per registered tool: its terminal, spawned lazily on the
    /// first visit. Held outside the workspace tree, so never persisted and
    /// never reattached — quitting pwrde ends the tool.
    tool_sessions: Vec<Option<ToolSession>>,
    /// Settings → Tools add-form inputs.
    tool_form: ToolForm,
    /// The active top-level page (Sessions / Settings).
    page: Page,
    /// The active section while the Settings page is up.
    section: Section,
    /// Keyboard-page row currently capturing a new binding, if any.
    recording: Option<Action>,
    /// Resize handle currently under the pointer (sidebar edge or tile divider).
    /// Drives the cursor style and hover highlight; sticky for the drag duration.
    resize_hover: Option<workspace::ResizeHover>,
    /// Link currently under the pointer: (tile id, col, row).
    /// Used to brighten the hovered link and show a pointing-hand cursor.
    link_hover: Option<(u64, usize, usize)>,
    /// Interactive chrome rects from the most recent frame, for hover testing.
    hot_rects: Vec<workspace::LayoutRect>,
    /// Index into hot_rects of the currently hovered element (topmost wins).
    ui_hover: Option<usize>,
    // ── Flyover terminal panel ─────────────────────────────────────────────
    /// Tabs held by the flyover panel (independent of the workspace tree).
    flyover_tabs: Vec<workspace::Tab>,
    /// Index of the active flyover tab.
    flyover_active: usize,
    /// Whether the flyover panel is currently visible.
    flyover_open: bool,
    /// Slide animation progress: 0.0 = fully hidden (below screen), 1.0 = fully open.
    flyover_anim: f32,
    /// Whether keyboard focus is currently inside the flyover panel.
    flyover_focused: bool,
    /// Panel height as a fraction of the window, drag-resizable from the
    /// panel's top edge; persisted under the `flyover.height` settings key.
    flyover_height_frac: f32,
    /// Whether the panel fills the whole window (the □ button toggles it).
    flyover_maximized: bool,
    /// Whether the flyover lives in its own popout window instead of the
    /// in-window panel.
    flyover_windowed: bool,
    /// Desired visibility of the popout window; the frame pump reconciles
    /// the actual window against this (⌘` flips it in windowed mode).
    flyover_window_visible: bool,
    /// The open popout window, when the pump has one up.
    flyover_window: Option<gpui::WindowHandle<FlyoverPopout>>,
    /// The main window, so popout-initiated flows (new-tab picker, docking)
    /// can bring it forward.
    main_window: Option<gpui::AnyWindowHandle>,
    /// Keystrokes queued by the bus `key` command, drained through the real
    /// gpui key-down handler so bindings and overlay routing are exercised.
    pending_keys: Vec<gpui::Keystroke>,
    /// Polarity shown in the Appearance preview cards (independent of the
    /// system/user mode setting); seeded from the active polarity at launch.
    preview_dark: bool,
    /// The appearance dropdown that currently has its option menu open, if any.
    appearance_menu: Option<pages::AppearanceDropdown>,
    /// Whether a group cwd sits inside a git checkout, memoized per path —
    /// probed by git-backed actions on every invocation.
    git_cwd_cache: std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, bool>>,
    // ── Flow agent (bottom pill + chat panel) ─────────────────────────────
    /// Transcript/panel state; updated only via `FlowState::apply` from
    /// `TermEvent::Flow` events on the main thread.
    flow: crate::flow::FlowState,
    /// The agent process, spawned lazily on first open/send so no `claude`
    /// child exists until Flow is actually used.
    flow_backends: std::collections::HashMap<u64, Box<dyn crate::flow::AgentBackend>>,
    /// The `lfg events` SSE tail process, kept alive while the app runs (its
    /// reader thread forwards cache-updated events). `None` when the async path
    /// is off or `lfg` couldn't launch.
    _lfg_events_child: Option<std::process::Child>,
    /// The pill bar's composer entity, created lazily on first render.
    flow_composer: Option<flow_ui::FlowComposer>,
    /// Blurred impression of the canvas under the liquid-glass overlays
    /// (`backdrop.rs`), rebuilt in `paint_terminal` only while a glass
    /// surface is open and only when the picture changed.
    glass_backdrop: Option<std::sync::Arc<gpui::RenderImage>>,
    glass_backdrop_key: u64,
    /// Logical window size the impression covers.
    glass_backdrop_size: (f32, f32),
}

impl App {
    /// The blurred canvas impression as a glass container's backdrop child
    /// (`ui::glass::backdrop`), or `None` while no impression is live.
    pub(crate) fn glass_backdrop_el(&self, corners: gpui::Corners<gpui::Pixels>) -> Option<gpui::AnyElement> {
        self.glass_backdrop
            .clone()
            .map(|img| crate::ui::glass::backdrop(img, self.glass_backdrop_size, corners))
    }

    fn scale(&self) -> f32 {
        self.renderer.scale
    }

    /// Effective sidebar-region width for layout/hit-testing: 0 while
    /// collapsed (`workspace` geometry treats 0 as collapsed), else the
    /// sessions list plus the folders column — see
    /// `workspace::sidebar_region_w`; the list itself is only the inner
    /// column ([`App::sessions_list`]).
    fn sidebar_w(&self) -> f32 {
        if self.sidebar_collapsed {
            0.0
        } else {
            workspace::sidebar_region_w(
                self.sidebar_expanded_w,
                self.folders_w,
                self.folders_visible(),
            )
        }
    }

    /// Whether the folders card is showing: the user's toggle, except on the
    /// Settings page, whose sidebar is its own section list and has no
    /// folders to filter.
    pub(crate) fn folders_visible(&self) -> bool {
        self.folders_open && self.page != Page::Settings
    }

    /// The sessions list rect (physical px at `scale`) every row helper takes.
    pub(crate) fn sessions_list(&self, scale: f32) -> workspace::LayoutRect {
        let (_, h) = self.renderer.surface_size();
        workspace::sessions_list_rect(
            self.sidebar_expanded_w,
            self.folders_w,
            self.folders_visible(),
            h,
            scale,
        )
    }

    /// The sessions list rect the *rows* lay out in: [`App::sessions_list`]
    /// shifted up by the wheel scroll, so every row helper (paint, hit-test,
    /// drop preview) sees the same scrolled stack. The header and the clip
    /// band keep the unshifted rect.
    pub(crate) fn sessions_rows_list(&self, scale: f32) -> workspace::LayoutRect {
        let mut list = self.sessions_list(scale);
        list.y -= (self.sessions_scroll() * scale).round();
        list
    }

    /// The sessions list's wheel scroll, logical px, clamped so the last
    /// row never scrolls above the bottom of the viewport.
    pub(crate) fn sessions_scroll(&self) -> f32 {
        self.sessions_scroll.clamp(0.0, self.sessions_scroll_max())
    }

    fn sessions_scroll_max(&self) -> f32 {
        let scale = self.scale();
        let list = self.sessions_list(scale);
        let rows = self.sidebar_rows();
        let extent = workspace::sidebar_rows_extent(&rows, &self.workspaces, scale, &list);
        let viewport = (list.h - (workspace::SESSIONS_HEADER_H * scale).round()).max(0.0);
        workspace::max_scroll(extent, viewport) / scale
    }

    /// The folders card rect (physical px at `scale`) every folder-row
    /// helper takes.
    pub(crate) fn folders_card(&self, scale: f32) -> workspace::LayoutRect {
        let (_, h) = self.renderer.surface_size();
        workspace::folders_card_rect(h, self.folders_w, scale)
    }

    /// The folders card's rows in paint order, with the "Pinned tools" run
    /// folded away when collapsed.
    pub(crate) fn folder_rows(&self) -> Vec<workspace::FolderRow> {
        workspace::folder_rows(self.tools.len(), self.tools_collapsed, self.sections.len())
    }

    /// The folders card's wheel scroll, logical px, clamped like
    /// [`App::sessions_scroll`].
    pub(crate) fn folders_scroll(&self) -> f32 {
        self.folders_scroll.clamp(0.0, self.folders_scroll_max())
    }

    fn folders_scroll_max(&self) -> f32 {
        let scale = self.scale();
        let card = self.folders_card(scale);
        let rows = self.folder_rows();
        let extent = workspace::folder_rows_extent(&card, &rows, scale);
        let footer = workspace::folders_footer_rect(&card, scale);
        let viewport = (footer.y - card.y - (workspace::FOLDERS_HEADER_H * scale).round()).max(0.0);
        workspace::max_scroll(extent, viewport) / scale
    }

    /// Wheel travel in logical px for the sidebar's scroll containers: a
    /// line notch is ~40px, trackpad pixels pass through.
    fn wheel_px(delta: gpui::ScrollDelta) -> f32 {
        match delta {
            gpui::ScrollDelta::Lines(p) => p.y * 40.0,
            gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
        }
    }

    /// Scroll the folders card's rows by a wheel event (clamped).
    pub(crate) fn scroll_folders(&mut self, delta: gpui::ScrollDelta) {
        let next =
            (self.folders_scroll() - Self::wheel_px(delta)).clamp(0.0, self.folders_scroll_max());
        if next != self.folders_scroll {
            self.folders_scroll = next;
            self.request_redraw();
        }
    }

    /// Scroll the sessions rows by a wheel event (clamped).
    fn scroll_sessions(&mut self, delta: gpui::ScrollDelta) {
        let next =
            (self.sessions_scroll() - Self::wheel_px(delta)).clamp(0.0, self.sessions_scroll_max());
        if next != self.sessions_scroll {
            self.sessions_scroll = next;
            self.request_redraw();
        }
    }

    /// Fold or unfold the folders card's "Pinned tools" run.
    pub(crate) fn toggle_tools_collapsed(&mut self) {
        self.tools_collapsed = !self.tools_collapsed;
        settings::set("sidebar.tools_collapsed", self.tools_collapsed.into());
        self.request_redraw();
    }

    /// Fold or unfold the sessions list's "Pinned" run.
    pub(crate) fn toggle_pinned_collapsed(&mut self) {
        self.pinned_collapsed = !self.pinned_collapsed;
        settings::set("sidebar.pinned_collapsed", self.pinned_collapsed.into());
        self.request_redraw();
    }

    /// A folder row press: select the folder now (`press_folder_row`) and
    /// arm a re-order drag that goes live past the drag threshold.
    pub(crate) fn press_folder_drag(&mut self, si: usize) {
        self.drag = Drag::FolderPress { si, start: self.cursor };
    }

    /// The flat session rows the list paints and hit-tests: the active folder
    /// filter applied to the non-pinned groups, and nothing on the Settings
    /// page (its sidebar holds section tabs, not groups).
    pub(crate) fn sidebar_rows(&self) -> Vec<workspace::SidebarRow> {
        if self.page == Page::Settings {
            return Vec::new();
        }
        workspace::sidebar_rows_filtered(
            &self.workspaces,
            &self.sections,
            self.folder_filter,
            self.pinned_collapsed,
        )
    }

    /// Whether the current page paints the session rows (every page but
    /// Settings, whose sidebar is its section list). Drawing and hit-testing
    /// both go through this so they can't drift apart.
    fn card_rows(&self) -> bool {
        self.page != Page::Settings
    }

    /// The active group's cwd (or the process cwd when the group inherits it),
    /// used as the working directory for git / PR CLI invocations. `None` only
    /// when neither is available.
    fn active_repo_dir(&self) -> Option<std::path::PathBuf> {
        match self.workspaces.get(self.active).and_then(|ws| ws.cwd.clone()) {
            Some(p) => Some(p),
            None => std::env::current_dir().ok(),
        }
    }

    /// Whether the active group's cwd (or an ancestor) is a git checkout.
    fn active_cwd_is_git(&self) -> bool {
        let cwd = match &self.workspaces[self.active].cwd {
            Some(p) => p.clone(),
            // `None` inherits the directory pwrde was launched from.
            None => match std::env::current_dir() {
                Ok(p) => p,
                Err(_) => return false,
            },
        };
        if let Some(&hit) = self.git_cwd_cache.borrow().get(&cwd) {
            return hit;
        }
        let hit = cwd.ancestors().any(|a| a.join(".git").exists());
        self.git_cwd_cache.borrow_mut().insert(cwd, hit);
        hit
    }

    /// ⇧⌘G (`Action::OpenPrInGithub`): open the active group's pull request
    /// in the browser. The sidebar's git-context cache usually already holds
    /// the PR its card shows, so the common case is instant. A cached "no
    /// PR" is an honest no-op that also bumps that group's rollup stale, so a
    /// PR opened since the last poll is picked up on the next press. Only a
    /// group whose context hasn't been fetched yet falls back to one
    /// background `pr list`, guarded by `open_pr_busy` so repeated presses
    /// can't queue up browser tabs. Returns whether the action applied — the
    /// tab was opened or the lookup that will open it was started — so the
    /// bus can report a no-op everywhere else.
    fn open_pr_in_github(&mut self) -> bool {
        use std::sync::atomic::Ordering;
        if self.page != Page::Sessions || self.is_empty_state() || !self.active_cwd_is_git() {
            return false;
        }
        let Some(cwd) = self.active_repo_dir() else {
            return false;
        };
        if let Some(ctx) = self.git_contexts.get(&cwd) {
            // Cached: open what the card shows, or report "no PR" honestly.
            // An empty URL means a PR CLI that predates the `url` field.
            match ctx.pr.as_ref().map(|pr| pr.url.as_str()) {
                Some(url) if !url.is_empty() => {
                    open_in_browser(url);
                    return true;
                },
                _ => {
                    self.git_contexts.mark_stale(&cwd);
                    self.spawn_git_context_refresh();
                    return false;
                },
            }
        }
        if self
            .open_pr_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // A lookup is already in flight — possibly for another group,
            // since the guard is global — so nothing new applied here.
            return false;
        }
        let busy = self.open_pr_busy.clone();
        std::thread::spawn(move || {
            if let Some(branch) = git::current_branch(&cwd)
                && let Some(pr) = gh::pr_list_for_branch(&cwd, &branch)
                    .ok()
                    .and_then(git_context::select_pr)
                && !pr.url.is_empty()
            {
                open_in_browser(&pr.url);
            }
            busy.store(false, Ordering::Release);
        });
        true
    }

    fn dpi(&self) -> u32 {
        (96.0 * self.scale()) as u32
    }

    /// Cell size in physical px, rounded for the PTY resize (u16).
    fn cell_px(&self) -> (u16, u16) {
        self.renderer.pty_cell_size()
    }

    fn spawn_session(&mut self) -> Session {
        let cwd = self.workspaces.get(self.active).and_then(|ws| ws.cwd.clone());
        self.spawn_session_in(cwd.as_deref())
    }

    fn new_webview_tab(&mut self, url: String) -> Tab {
        let id = self.next_webview_id;
        self.next_webview_id += 1;
        Tab::webview(id, url)
    }

    pub(crate) fn add_webview_tab_to_group(
        &mut self,
        group_idx: usize,
        url: String,
    ) -> Result<u64, String> {
        if group_idx >= self.workspaces.len() || self.workspaces[group_idx].focused().is_none() {
            return Err("the target group has no focused pane".into());
        }
        let tab = self.new_webview_tab(url);
        let id = tab.webview_id().expect("webview constructor assigns an id");
        let tile = self.workspaces[group_idx]
            .focused_mut()
            .expect("focused pane checked before tab allocation");
        tile.tabs.push(tab);
        tile.active = tile.tabs.len() - 1;
        self.active = group_idx;
        workspace::ensure_active_section_expanded(
            &self.workspaces,
            &mut self.sections,
            self.active,
        );
        if self.page != Page::Sessions {
            self.set_page(Page::Sessions);
        } else {
            self.sync_layout();
            self.request_redraw();
        }
        self.persist_snapshot();
        Ok(id)
    }

    fn set_webview_url(&mut self, id: u64, url: String) -> bool {
        for workspace in &mut self.workspaces {
            for tile in workspace.root.tiles_mut() {
                if let Some(tab) = tile.tabs.iter_mut().find(|tab| tab.webview_id() == Some(id)) {
                    return tab.set_webview_url(url);
                }
            }
        }
        false
    }

    fn open_new_webview_prompt(&mut self) {
        self.command = None;
        self.webview_panel = None;
        self.webview_prompt = Some(WebviewPrompt { query: String::new(), error: None });
        self.modal_search_reset = Some("Enter a URL or hostname…".into());
        self.request_redraw();
    }

    fn submit_new_webview_prompt(&mut self) {
        let raw = self
            .webview_prompt
            .as_ref()
            .map(|prompt| prompt.query.clone())
            .unwrap_or_default();
        let url = match webview::normalize_input(&raw) {
            Ok(url) => url,
            Err(error) => {
                if let Some(prompt) = self.webview_prompt.as_mut() {
                    prompt.error = Some(error);
                }
                self.request_redraw();
                return;
            },
        };
        match self.add_webview_tab_to_group(self.active, url) {
            Ok(_) => self.webview_prompt = None,
            Err(error) => {
                if let Some(prompt) = self.webview_prompt.as_mut() {
                    prompt.error = Some(error);
                }
            },
        }
        self.request_redraw();
    }

    /// Keep Wry child views aligned with the visible active webview tabs.
    fn sync_webviews(&mut self, window: &Window) {
        let live: std::collections::HashSet<u64> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.root.tiles())
            .flat_map(|tile| tile.tabs.iter())
            .filter_map(Tab::webview_id)
            .collect();
        if self.webview_panel.as_ref().is_some_and(|panel| !live.contains(&panel.id())) {
            self.webview_panel = None;
        }
        let obscured = self.page != Page::Sessions
            || self.modal_overlay_open()
            || (self.flyover_anim > 0.0 && !self.flyover_windowed)
            || self.flow.open
            || matches!(self.drag, Drag::Tab { .. } | Drag::Group { .. } | Drag::Folder { .. });
        let mut placements = Vec::new();
        let mut focus = None;
        if !obscured {
            let ws = &self.workspaces[self.active];
            let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), self.scale());
            for (tile_id, rect) in tiles {
                let Some(tile) = ws.root.find_tile(tile_id) else { continue };
                if tile.collapsed || tile.collapse_anim > 0.0 {
                    continue;
                }
                let Some(tab) = tile.active_tab() else { continue };
                let (Some(id), Some(url)) = (tab.webview_id(), tab.url()) else { continue };
                let content = workspace::tile_content(&rect, self.scale());
                if content.w < 1.0 || content.h < 1.0 {
                    continue;
                }
                let panel_h = webview_ui::panel_height(self.webview_panel.as_ref(), id);
                let bounds = webview::child_bounds(content, self.scale(), panel_h);
                placements.push(webview::Placement { id, url: url.to_string(), bounds });
                if tile_id == ws.focused_tile {
                    focus = Some(id);
                }
            }
        }
        if let Some(error) =
            self.webviews.sync(window, &live, &placements, focus, &self.events_tx)
        {
            self.message = Some((format!("Could not open webview: {error}"), true));
            self.request_redraw();
        }
    }

    /// Spawn a session whose shell starts in `cwd` (`None` inherits our own).
    /// With persistence on, the shell runs inside a freshly named shpool
    /// session so it survives app restarts.
    fn spawn_session_in(&mut self, cwd: Option<&std::path::Path>) -> Session {
        let shpool_session = if settings::get_bool("terminal.persist", false) {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Some(format!(
                "pwrde-{}-{}{}",
                self.next_session_id,
                std::process::id(),
                nanos % 1_000_000
            ))
        } else {
            None
        };
        self.spawn_session_named(cwd, shpool_session)
    }

    /// Spawn a session bound to a specific shpool session name (the restore
    /// path reattaches by saved name), or a plain shell when `shpool` is None.
    fn spawn_session_named(
        &mut self,
        cwd: Option<&std::path::Path>,
        shpool: Option<String>,
    ) -> Session {
        let id = self.next_session_id;
        self.next_session_id += 1;
        let (cw, ch) = self.cell_px();
        // Start with a nominal grid; the first sync_layout resizes it.
        Session::new(
            id,
            80,
            24,
            cw,
            ch,
            self.dpi(),
            cwd,
            None,
            self.events_tx.clone(),
            shpool,
        )
    }

    fn persist_snapshot(&self) {
        if !settings::get_bool("terminal.persist", false) {
            return;
        }
        let saved = persist::workspaces_to_saved(&self.workspaces);
        let sections = persist::sections_to_saved(&self.sections);
        if let Err(e) = persist::save_snapshot_default(&saved, &sections) {
            eprintln!("persist_snapshot error: {}", e);
        }
    }

    /// Rebuild workspaces and sidebar sections from the persisted snapshot,
    /// reattaching each tab to its saved shpool session. Returns false when
    /// there is nothing to restore (caller falls back to the empty state).
    fn restore_workspaces(&mut self) -> bool {
        let (saved, saved_sections) = persist::load_snapshot_default();
        // Sections alone aren't enough to restore a session; fall back to the
        // empty-state placeholder when no groups were saved.
        if saved.is_empty() {
            return false;
        }
        self.sections = persist::saved_to_sections(&saved_sections);
        self.next_section_id = self
            .sections
            .iter()
            .map(|s| s.id)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        for group in &saved {
            let cwd = group.cwd.as_ref().map(std::path::PathBuf::from);
            let root = self.restore_node(&group.layout, &group.tabs, cwd.as_deref());
            // The first leaf anchors the group's lifetime, mirroring how a
            // freshly created group treats its founding tile as primary.
            let primary_tile =
                root.tiles().first().map(|t| t.id).unwrap_or(group.focused_tile as u64);
            let mut ws = Workspace {
                name: group.name.clone(),
                root,
                focused_tile: group.focused_tile as u64,
                cwd,
                primary_tile,
                section: group.section_id,
                pinned: group.pinned,
            };
            ws.fix_focus();
            self.workspaces.push(ws);
        }
        self.active = 0;
        workspace::normalize_section_anchors(&self.workspaces, &mut self.sections);
        // Load the saved folder filter (absent = all sessions), then drop
        // it if the restored sections no longer contain that id — a stale
        // filter would otherwise show a permanently empty list.
        if self.folder_filter.is_none() {
            self.folder_filter = settings::get_str("sidebar.folder").and_then(|s| s.parse::<u64>().ok());
        }
        if self
            .folder_filter
            .is_some_and(|id| !self.sections.iter().any(|s| s.id == id))
        {
            self.folder_filter = None;
        }
        true
    }

    /// Recursively rebuild a split tree from its saved layout, spawning a
    /// reattached session for every saved tab of each leaf tile.
    fn restore_node(
        &mut self,
        node: &persist::LayoutNode,
        tabs: &[persist::SavedTab],
        cwd: Option<&std::path::Path>,
    ) -> Node {
        match node {
            persist::LayoutNode::Leaf { tile, collapsed } => {
                let tile_id = *tile as u64;
                self.next_tile_id = self.next_tile_id.max(tile_id + 1);
                let mut saved: Vec<&persist::SavedTab> =
                    tabs.iter().filter(|t| t.tile_id == *tile).collect();
                saved.sort_by_key(|t| t.tab_index);
                let mut restored = Tile::empty(tile_id);
                restored.active = saved.iter().position(|st| st.active).unwrap_or(0);
                for st in &saved {
                    let mut tab = match st.kind {
                        persist::SavedTabKind::Webview => {
                            match st
                                .url
                                .as_deref()
                                .and_then(|url| bus::validate_webview_url(url).ok())
                            {
                                Some(url) => self.new_webview_tab(url),
                                None => Tab::new(self.spawn_session_named(cwd, None)),
                            }
                        },
                        persist::SavedTabKind::Terminal => {
                            let tab_cwd = st.cwd.as_ref().map(std::path::PathBuf::from);
                            let session = self.spawn_session_named(
                                tab_cwd.as_deref().or(cwd),
                                st.shpool_session.clone(),
                            );
                            Tab::new(session)
                        },
                    };
                    tab.unread = st.unread;
                    tab.unread_at = persist::from_epoch_secs(st.unread_at);
                    restored.tabs.push(tab);
                }
                restored.collapsed = *collapsed;
                restored.collapse_anim = if *collapsed { 1.0 } else { 0.0 };
                Node::Leaf(restored)
            }
            persist::LayoutNode::Split { dir, ratio, a, b } => Node::Split {
                dir: if dir == "column" { Dir::Column } else { Dir::Row },
                ratio: *ratio,
                a: Box::new(self.restore_node(a, tabs, cwd)),
                b: Box::new(self.restore_node(b, tabs, cwd)),
            },
        }
    }

    fn new_tile(&mut self) -> Tile {
        let id = self.next_tile_id;
        self.next_tile_id += 1;
        let session = self.spawn_session();
        Tile::new(id, session)
    }

    /// Bottom safe area (logical px) reserved for the Flow pill bar so
    /// terminal rows reflow above it instead of hiding beneath it. Zero
    /// while the `features.flow` flag is off; tracks the app font scale.
    fn flow_inset(&self) -> f32 {
        if crate::flow::enabled() { flow_ui::safe_area_h() } else { 0.0 }
    }

    /// Physical-pixel terminal area (excludes the sidebar).
    fn area(&self) -> workspace::LayoutRect {
        let (w, h) = self.renderer.surface_size();
        workspace::terminal_area(
            w,
            h,
            self.scale(),
            self.sidebar_w(),
            self.flow_inset(),
        )
    }

    /// The screen-space rect of a tile in the active workspace, if present.
    fn tile_rect(&self, id: u64) -> Option<workspace::LayoutRect> {
        let scale = self.scale();
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        tiles.into_iter().find(|(tid, _)| *tid == id).map(|(_, rect)| rect)
    }

    /// Re-measure every visible tile and push grid sizes to the PTYs.
    fn sync_layout(&mut self) {
        workspace::normalize_section_anchors(&self.workspaces, &mut self.sections);
        self.sync_layout_impl(false);
        self.sync_flyover_layout(false);
        self.sync_tool_layout(false);
    }

    /// `force` pushes a PTY resize even when cols/rows are unchanged — needed
    /// after a display-scale change, where the cell pixel size and dpi moved
    /// but the grid dimensions may not have.
    fn sync_layout_impl(&mut self, force: bool) {
        let scale = self.scale();
        let (cw, ch) = self.cell_px();
        let dpi = self.dpi();
        // Pin PTY sizing to Sessions geometry: other pages drop the flow inset
        // from `area()`, and shells must not get resized just because the user
        // flipped to Settings and back.
        let (w, h) = self.renderer.surface_size();
        let area = workspace::terminal_area(
            w,
            h,
            scale,
            self.sidebar_w(),
            self.flow_inset(),
        );
        let ws = &mut self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        let axes = workspace::tile_collapse_axis(&ws.root);
        for (id, rect) in &tiles {
            let content = workspace::tile_content(rect, scale);
            // `grid_size_for` subtracts 2*PANE_PAD, matching the renderer's
            // content_origin inset — so the PTY size tracks the padded render area.
            let (cols, rows) = self.renderer.grid_size_for(&content);
            let in_split = axes.iter().any(|(tid, a)| tid == id && a.is_some());
            if let Some(tile) = ws.root.find_tile_mut(*id) {
                // Collapsed or mid-animation panes keep their last grid so the
                // shell isn't squished into a strip-sized PTY.
                if in_split && (tile.collapsed || tile.collapse_anim > 0.0) {
                    continue;
                }
                if let Some(tab) = tile.active_tab_mut() {
                    if force || (cols, rows) != (tab.cols, tab.rows) {
                        tab.cols = cols;
                        tab.rows = rows;
                        if let Some(session) = tab.session() {
                            session.resize(cols, rows, cw, ch, dpi);
                        }
                    }
                }
            }
        }
    }

    /// Resize flyover PTY sessions to match the current flyover content rect.
    /// Only acts when the panel is open AND the animation is settled (anim == 1.0).
    /// `force` pushes a PTY resize even when cols/rows are unchanged — needed
    /// after a display-scale change.
    fn sync_flyover_layout(&mut self, force: bool) {
        if !self.flyover_open || self.flyover_anim < 1.0 {
            return;
        }
        let scale = self.scale();
        let panel = self.flyover_rect_now();
        let content = workspace::flyover_content(&panel, scale);
        let (cols, rows) = self.renderer.grid_size_for(&content);
        let (cw, ch) = self.cell_px();
        let dpi = self.dpi();
        for tab in &mut self.flyover_tabs {
            if force || (cols, rows) != (tab.cols, tab.rows) {
                tab.cols = cols;
                tab.rows = rows;
                if let Some(session) = tab.session() {
                    session.resize(cols, rows, cw, ch, dpi);
                }
            }
        }
    }

    // ── CLI tool pages ──────────────────────────────────────────────────

    fn n_tools(&self) -> usize {
        self.tools.len()
    }

    /// The tool page's terminal card: the whole content area.
    fn tool_area(&self) -> workspace::LayoutRect {
        let (w, h) = self.renderer.surface_size();
        workspace::terminal_area(w, h, self.scale(), self.sidebar_w(), self.flow_inset())
    }

    fn active_tool_session(&self) -> Option<&ToolSession> {
        match self.page {
            Page::Tool(i) => self.tool_sessions.get(i).and_then(|s| s.as_ref()),
            _ => None,
        }
    }

    /// A tool page's terminal: the user's login shell running `command` from
    /// `cwd`. Never persisted, never shpool-attached.
    fn spawn_tool_session(&mut self, cwd: &std::path::Path, command: &str) -> Session {
        let id = self.next_session_id;
        self.next_session_id += 1;
        let (cw, ch) = self.cell_px();
        Session::new(
            id,
            80,
            24,
            cw,
            ch,
            self.dpi(),
            Some(cwd),
            Some(command),
            self.events_tx.clone(),
            None,
        )
    }

    /// Launch tool `i`'s command unless it is already running (first visit,
    /// or its last run exited).
    fn ensure_tool_session(&mut self, i: usize) {
        let Some(tool) = self.tools.get(i).cloned() else { return };
        let live =
            self.tool_sessions.get(i).and_then(|s| s.as_ref()).is_some_and(|s| !s.exited);
        if live {
            return;
        }
        let cwd = cli_tools::expand_cwd(&tool.cwd);
        let session = self.spawn_tool_session(&cwd, &tool.command);
        self.tool_sessions[i] =
            Some(ToolSession { tab: workspace::Tab::new(session), exited: false });
        self.sync_tool_layout(true);
        self.request_redraw();
    }

    /// Resize the active tool page's PTY to the content area, the way
    /// `sync_flyover_layout` does for the panel.
    fn sync_tool_layout(&mut self, force: bool) {
        let Page::Tool(i) = self.page else { return };
        let content = workspace::tile_content(&self.tool_area(), self.scale());
        let (cols, rows) = self.renderer.grid_size_for(&content);
        let (cw, ch) = self.cell_px();
        let dpi = self.dpi();
        if let Some(Some(ts)) = self.tool_sessions.get_mut(i) {
            let tab = &mut ts.tab;
            if force || (cols, rows) != (tab.cols, tab.rows) {
                tab.cols = cols;
                tab.rows = rows;
                if let Some(session) = tab.session() {
                    session.resize(cols, rows, cw, ch, dpi);
                }
            }
        }
    }

    /// Write a plain (non-⌘) keystroke to the active tool page's terminal.
    /// Once the command has exited only ⏎ does anything: it relaunches.
    fn tool_write_key(&mut self, keystroke: &Keystroke) {
        let Page::Tool(i) = self.page else { return };
        let exited =
            self.tool_sessions.get(i).and_then(|s| s.as_ref()).is_none_or(|s| s.exited);
        if exited {
            if keystroke.key == "enter" {
                self.ensure_tool_session(i);
            }
            return;
        }
        if let Some(bytes) = key_to_bytes(keystroke)
            && let Some(Some(ts)) = self.tool_sessions.get(i)
            && let Some(session) = ts.tab.session()
        {
            session.write(bytes);
            session.scroll_to_bottom();
            session.clear_selection();
        }
    }

    /// ⌘C on a tool page: copy its terminal's selection.
    fn tool_copy(&mut self) {
        if let Some(ts) = self.active_tool_session()
            && let Some(text) = ts.tab.session().and_then(Session::selected_text)
            && let Ok(mut clipboard) = arboard::Clipboard::new()
        {
            let _ = clipboard.set_text(text);
        }
    }

    /// ⌘V on a tool page: paste into its terminal (mirrors `paste`). An
    /// exited tool has no PTY to paste into — ⏎ relaunches it first.
    fn tool_paste(&mut self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        let Some(ts) = self.active_tool_session().filter(|ts| !ts.exited) else { return };
        let Some(session) = ts.tab.session() else { return };
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => session.paste(&text),
            _ if clipboard.get_image().is_ok() => session.write([0x16u8]),
            _ => return,
        }
        session.scroll_to_bottom();
        session.clear_selection();
        self.request_redraw();
    }

    /// Re-read the registered tools after a settings change, keeping the
    /// session slots aligned with the list.
    fn reload_tools(&mut self) {
        self.tools = cli_tools::tools();
        let n = self.tools.len();
        self.tool_sessions.resize_with(n, || None);
        if let Page::Tool(i) = self.page
            && i >= n
        {
            self.set_page(Page::Settings);
        }
    }

    /// Settings → Tools "Add": register the form's tool and clear the form.
    /// Only the command is required; the rest default sensibly.
    pub(crate) fn add_tool_from_form(&mut self, cx: &mut Context<Self>) {
        let [name, command, cwd, icon] =
            self.tool_form.inputs().map(|e| e.read(cx).text().trim().to_string());
        if command.is_empty() {
            return;
        }
        let tool = cli_tools::CliTool {
            name: if name.is_empty() { command.clone() } else { name },
            command,
            cwd: if cwd.is_empty() { "~".into() } else { cwd },
            icon: if icon.is_empty() { ">_".into() } else { icon },
        };
        cli_tools::add_tool(tool);
        for e in self.tool_form.inputs() {
            e.update(cx, |i, cx| i.set_text("", cx));
        }
        self.reload_tools();
        self.request_redraw();
    }

    /// Settings → Tools "Remove": drop tool `i` along with its session.
    /// Tool identity is positional (`tools`, `tool_sessions`, `Page::Tool`),
    /// so a page at or past `i` shifts with the list — only reachable from
    /// Settings today, but kept honest for any future caller.
    pub(crate) fn remove_tool(&mut self, i: usize) {
        if i < self.tool_sessions.len() {
            self.tool_sessions.remove(i);
        }
        cli_tools::remove_tool(i);
        if let Page::Tool(j) = self.page {
            if j == i {
                self.page = Page::Settings;
            } else if j > i {
                self.page = Page::Tool(j - 1);
            }
        }
        self.reload_tools();
        self.request_redraw();
    }

    fn split(&mut self, dir: Dir) {
        let tile = self.new_tile();
        let new_id = tile.id;
        let ws = &mut self.workspaces[self.active];
        let focused = ws.focused_tile;
        if ws.root.split_tile(focused, dir, &mut Some(tile), false) {
            ws.focused_tile = new_id;
        }
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    fn new_tab(&mut self) {
        let session = self.spawn_session();
        let ws = &mut self.workspaces[self.active];
        let focused = ws.focused_tile;
        if let Some(tile) = ws.root.find_tile_mut(focused) {
            tile.tabs.push(Tab::new(session));
            tile.active = tile.tabs.len() - 1;
        }
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    fn switch_workspace(&mut self, wi: usize) {
        if wi < self.workspaces.len() {
            let changed = self.active != wi;
            self.active = wi;
            // A different group means a different repo: the sidebar card for
            // the group we just left (and the one we arrived at) may be stale
            // by now.
            if changed {
                self.spawn_git_context_refresh();
            }
            // Activating a member of a collapsed section expands it so the
            // active group is visible in the sidebar.
            if workspace::ensure_active_section_expanded(
                &self.workspaces,
                &mut self.sections,
                self.active,
            ) {
                self.persist_snapshot();
            }
            self.sync_layout();
            self.mark_visible_read();
            self.request_redraw();
        }
    }

    fn cycle_tile(&mut self, delta: isize) {
        let ws = &mut self.workspaces[self.active];
        let ids: Vec<u64> = ws.root.tiles().iter().map(|t| t.id).collect();
        if ids.is_empty() {
            return;
        }
        let cur = ids.iter().position(|&id| id == ws.focused_tile).unwrap_or(0);
        let n = ids.len() as isize;
        let next = (((cur as isize + delta) % n + n) % n) as usize;
        ws.focused_tile = ids[next];
        self.mark_visible_read();
        self.request_redraw();
    }

    /// Move focus to the pane in the given direction, if one exists.
    fn focus_dir(&mut self, dir: workspace::NavDir) {
        let ws = &self.workspaces[self.active];
        let scale = self.scale();
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        let from = ws.focused_tile;
        if let Some(id) = workspace::directional_neighbor(&tiles, from, dir) {
            self.workspaces[self.active].focused_tile = id;
            self.request_redraw();
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let ws = &mut self.workspaces[self.active];
        let focused = ws.focused_tile;
        if let Some(tile) = ws.root.find_tile_mut(focused) {
            let n = tile.tabs.len();
            if n == 0 {
                return;
            }
            let cur = tile.active as isize;
            let n = n as isize;
            tile.active = (((cur + delta) % n + n) % n) as usize;
        }
        self.sync_layout();
        self.mark_visible_read();
        self.request_redraw();
    }

    /// A press on tab `ti` of tile `id` — from the strip element or the
    /// canvas fallback: activate + focus, × closes, a collapsed pane
    /// expands, a double-click on a split pane collapses it, and otherwise
    /// the press arms a tab drag.
    pub(crate) fn press_tile_tab(&mut self, id: u64, ti: usize, close: bool, click_count: usize) {
        // A fresh click sequence forgets which pane the previous one expanded.
        if click_count <= 1 {
            self.just_expanded = None;
        }
        let has_caret = workspace::tile_collapse_axis(&self.workspaces[self.active].root)
            .iter()
            .any(|(tid, axis)| *tid == id && axis.is_some());
        let ws = &mut self.workspaces[self.active];
        let Some(tile) = ws.root.find_tile_mut(id) else { return };
        // A tab-less tile exists transiently; it still takes focus and arms
        // the press exactly as the canvas path did.
        let n = tile.tabs.len();
        let ti = ti.min(n.saturating_sub(1));
        tile.active = ti;
        ws.focused_tile = id;
        if close && n > 0 {
            self.close_active_tab();
            return;
        }
        if tile.collapsed {
            // Clicking a tab name on a collapsed pane expands it.
            self.set_collapsed(id, false);
            self.just_expanded = Some(id);
        } else if has_caret && click_count >= 2 && self.just_expanded != Some(id) {
            // Double-clicking the tab bar collapses the pane (unless this
            // same double-click just expanded it).
            self.set_collapsed(id, true);
            self.request_redraw();
            return;
        }
        self.drag = Drag::TabPress { tile: id, tab: ti, start: self.cursor };
        self.sync_layout();
        self.mark_visible_read();
        self.request_redraw();
    }

    /// A press on a folder row in the folders card: the delete chip (when the
    /// row isn't being renamed) deletes; a double-click opens the inline
    /// rename; any click selects the folder as the list's filter.
    pub(crate) fn press_folder_row(&mut self, section_id: u64, delete: bool, click_count: usize) {
        let editing_this = self.editing_section.as_ref().is_some_and(|(id, _)| *id == section_id);
        if delete && !editing_this {
            self.delete_section(section_id);
            return;
        }
        let Some(sec) = self.sections.iter().find(|s| s.id == section_id) else { return };
        if click_count >= 2 {
            let buf = if sec.emoji.is_empty() {
                sec.name.clone()
            } else {
                format!("{} {}", sec.emoji, sec.name)
            };
            self.editing_section = Some((section_id, buf));
        }
        self.set_folder_filter(Some(section_id));
    }

    /// Select which folder the sessions list shows (`None` = All sessions)
    /// and remember it under `sidebar.folder`. A Tool page hops back to
    /// Sessions so the filtered list is what the user is looking at.
    pub(crate) fn set_folder_filter(&mut self, filter: Option<u64>) {
        if filter.is_some_and(|id| !self.sections.iter().any(|s| s.id == id)) {
            return;
        }
        self.folder_filter = filter;
        match filter {
            Some(id) => crate::settings::set("sidebar.folder", serde_json::Value::String(id.to_string())),
            None => crate::settings::set("sidebar.folder", serde_json::Value::Null),
        }
        if let Page::Tool(_) = self.page {
            self.set_page(Page::Sessions);
        }
        self.request_redraw();
    }

    /// A press on a group card arms a group press; the click fires on
    /// mouse-up if the drag threshold is never crossed (mirrors TabPress).
    pub(crate) fn press_group_row(&mut self, ws_idx: usize) {
        self.drag = Drag::GroupPress { ws: ws_idx, start: self.cursor };
    }

    /// A press on a tile's collapse caret toggles the pane — only on the
    /// first click of a double, or the second would snap it straight back.
    pub(crate) fn press_tile_caret(&mut self, id: u64, click_count: usize) {
        if click_count > 1 {
            return;
        }
        let collapsed =
            self.workspaces[self.active].root.find_tile(id).is_some_and(|t| t.collapsed);
        self.set_collapsed(id, !collapsed);
        self.request_redraw();
    }

    /// A press anywhere on a sideways-collapsed strip expands and focuses it.
    pub(crate) fn press_tile_expand(&mut self, id: u64) {
        self.set_collapsed(id, false);
        self.just_expanded = Some(id);
        self.workspaces[self.active].focused_tile = id;
        self.request_redraw();
    }

    /// A press on flyover tab `ti`: activate + focus the panel, or × closes.
    pub(crate) fn press_flyover_tab(&mut self, ti: usize, close: bool) {
        if ti >= self.flyover_tabs.len() {
            return;
        }
        if close {
            self.close_flyover_tab(ti);
            return;
        }
        self.flyover_active = ti;
        self.flyover_focused = true;
        self.flyover_mark_read();
        self.request_redraw();
    }

    /// Record a pointer position from an element event (logical px) in the
    /// physical-px form the canvas mouse path keeps.
    pub(crate) fn note_pointer(&mut self, ev: &MouseDownEvent) {
        self.note_cursor(ev.position);
        self.modifiers = ev.modifiers;
    }

    /// Record a logical pointer position as the physical `cursor`, for
    /// element-tree layers that occlude the canvas mouse-move listener.
    pub(crate) fn note_cursor(&mut self, position: gpui::Point<Pixels>) {
        let s = self.scale() as f64;
        self.cursor = (f64::from(position.x) * s, f64::from(position.y) * s);
    }

    fn close_active_tab(&mut self) {
        let ws = &mut self.workspaces[self.active];
        let focused = ws.focused_tile;
        let primary = ws.primary_tile;
        let Some(tile) = ws.root.find_tile_mut(focused) else { return };
        if tile.tabs.is_empty() {
            return;
        }
        // Closing the primary pane closes the whole group — confirm first.
        if focused == primary && tile.tabs.len() == 1 {
            self.confirm = Some(ConfirmClose {
                text: "Closing the primary pane closes this group.".into(),
                action: ConfirmAction::CloseGroup { primary_tile: primary },
            });
            self.request_redraw();
            return;
        }
        let tab_idx = tile.active;
        let tab = tile.tabs.remove(tab_idx);
        // Explicit close ends the persistent session too; a shell that merely
        // exited goes through remove_session instead, where the shpool session
        // is already gone.
        if let Some(name) = tab.session().and_then(|session| session.shpool_session.as_deref()) {
            term::shpool_kill(name);
        }
        if tile.active >= tile.tabs.len() {
            tile.active = tile.tabs.len().saturating_sub(1);
        }
        if tile.tabs.is_empty() {
            if !ws.root.remove_tile(focused) {
                // Was the last tile of the group: close the group.
                if self.workspaces.len() > 1 {
                    self.workspaces.remove(self.active);
                    if self.active >= self.workspaces.len() {
                        self.active = self.workspaces.len() - 1;
                    }
                } else {
                    // Last pane of the last group: back to the empty state
                    // rather than a dead window (⌘Q / traffic lights quit).
                    self.reset_empty_workspace(0);
                }
            }
        }
        drop(tab);
        // A tile removal may leave `focused_tile` dangling.
        self.workspaces[self.active].fix_focus();
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// ⌘⇧W: close the active group from any focused pane, through the same
    /// confirm dialog as ⌘W on the primary pane's last tab.
    fn close_focused_group(&mut self) {
        let primary_tile = self.workspaces[self.active].primary_tile;
        self.confirm = Some(ConfirmClose {
            text: "Closing this group closes all of its panes.".into(),
            action: ConfirmAction::CloseGroup { primary_tile },
        });
        self.request_redraw();
    }

    /// Toggle collapse/expand all panes *other than* the focused one (the ⌘⇧F
    /// action). If any other tile is expanded, collapse them all; otherwise
    /// expand them all. A single-tile workspace is left alone.
    fn toggle_focus_others(&mut self) {
        let ws = &self.workspaces[self.active];
        let focused = ws.focused_tile;
        let mut others = Vec::new();
        let mut any_expanded = false;
        for t in ws.root.tiles() {
            if t.id != focused {
                others.push(t.id);
                any_expanded |= !t.collapsed;
            }
        }
        if others.is_empty() {
            return;
        }
        for id in others {
            self.set_collapsed(id, any_expanded);
        }
        // Focus mode means the focused pane is the one on screen — make sure
        // it isn't itself collapsed when everything else folds away.
        if any_expanded {
            self.set_collapsed(focused, false);
        }
        self.request_redraw();
    }

    /// Toggle collapse on the focused pane (the ⌘⇧M action). A root leaf has
    /// no split to collapse into, so it is left alone.
    fn toggle_focused_collapse(&mut self) {
        let ws = &self.workspaces[self.active];
        let id = ws.focused_tile;
        let in_split = workspace::tile_collapse_axis(&ws.root)
            .iter()
            .any(|(tid, a)| *tid == id && a.is_some());
        if !in_split {
            return;
        }
        let collapsed = ws.root.find_tile(id).is_some_and(|t| t.collapsed);
        self.set_collapsed(id, !collapsed);
        self.request_redraw();
    }

    /// Collapse or expand a pane. The animation tick in `drain_events` walks
    /// `collapse_anim` toward the new target; the split ratio is untouched so
    /// expanding restores the previous arrangement.
    fn set_collapsed(&mut self, id: u64, collapsed: bool) {
        let ws = &mut self.workspaces[self.active];
        let Some(tile) = ws.root.find_tile_mut(id) else { return };
        if tile.collapsed == collapsed {
            return;
        }
        tile.collapsed = collapsed;
        // Collapsing the focused pane moves focus to an expanded one so
        // keystrokes keep landing somewhere visible.
        if collapsed
            && ws.focused_tile == id
            && let Some(t) = ws.root.tiles().iter().find(|t| !t.collapsed)
        {
            ws.focused_tile = t.id;
        }
        self.persist_snapshot();
        if !collapsed {
            // Expanding puts the pane's content back on screen — mark it read.
            self.mark_visible_read();
        }
    }

    /// Close workspace `wi` entirely (all tiles and their sessions). The last
    /// group resets to the empty state instead of leaving a dead window.
    fn close_group(&mut self, wi: usize) {
        // Explicit close: end the group's persistent sessions too.
        for tile in self.workspaces[wi].root.tiles() {
            for tab in &tile.tabs {
                if let Some(name) = tab.session().and_then(|session| session.shpool_session.as_deref()) {
                    term::shpool_kill(name);
                }
            }
        }
        if self.workspaces.len() > 1 {
            self.workspaces.remove(wi);
            if self.active >= self.workspaces.len() {
                self.active = self.workspaces.len() - 1;
            }
        } else {
            self.reset_empty_workspace(0);
        }
        // A section outlives its groups: closing the last member leaves the
        // (now empty) section in place. Sections are removed only by the
        // explicit delete-section button.
        workspace::ensure_active_section_expanded(
            &self.workspaces,
            &mut self.sections,
            self.active,
        );
        self.workspaces[self.active].fix_focus();
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Delete a sidebar section, keeping its groups: members are ungrouped
    /// (they stay as top-level groups), then the section entry is removed.
    /// Nothing is closed — this only undoes the grouping.
    /// ⌥⌘S / palette: show or hide the folders card beside the sessions
    /// list. Persisted under `sidebar.folders` like the collapse flag.
    fn toggle_folders(&mut self) {
        self.folders_open = !self.folders_open;
        crate::settings::set("sidebar.folders", self.folders_open.into());
        self.request_redraw();
    }

    fn delete_section(&mut self, section_id: u64) {
        if !workspace::delete_section(
            &mut self.sections,
            &mut self.workspaces,
            section_id,
        ) {
            return;
        }
        if self
            .editing_section
            .as_ref()
            .is_some_and(|(id, _)| *id == section_id)
        {
            self.editing_section = None;
        }
        // Dropping a folder clears an active filter on it.
        if self.folder_filter == Some(section_id) {
            self.folder_filter = None;
        }
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Confirm-dialog accept: perform whatever action the dialog was guarding.
    fn confirm_accept(&mut self) {
        let Some(confirm) = self.confirm.take() else { return };
        match confirm.action {
            ConfirmAction::CloseGroup { primary_tile } => {
                if let Some(wi) =
                    self.workspaces.iter().position(|ws| ws.primary_tile == primary_tile)
                {
                    self.close_group(wi);
                }
            },
            ConfirmAction::ClearWebviewData { id } => {
                if let Err(error) = self.webviews.clear_browsing_data(id) {
                    self.message = Some((error, true));
                }
                self.webview_panel = None;
            },
        }
        self.request_redraw();
    }

    /// Mouse wheel / trackpad → scroll the pane under the cursor. On the
    /// primary screen this scrolls our own scrollback (positive = back into
    /// history, matching macOS natural scrolling). But a full-screen TUI or a
    /// mouse-tracking app owns the wheel — there we forward it to the app
    /// (which scrolls its own content), since the alternate screen has no
    /// scrollback of ours to move.
    fn on_scroll(&mut self, delta: gpui::ScrollDelta, cell_height: f32) {
        if self.confirm.is_some()
            || self.message.is_some()
            || self.webview_prompt.is_some()
            || self.command.is_some()
        {
            return;
        }
        // Flyover panel scroll: intercept first when panel is open and cursor is inside.
        if self.flyover_open && !self.flyover_tabs.is_empty() {
            let scale = self.scale();
            let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
            let panel = self.flyover_rect_now();
            let content = workspace::flyover_content(&panel, scale);
            if panel.contains(px, py) {
                let cell_h = cell_height as f64;
                let notches = match delta {
                    gpui::ScrollDelta::Lines(p) => p.y as f64,
                    gpui::ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / (cell_h * 3.0),
                };
                let steps = scroll_steps(&mut self.scroll_accum, notches);
                if steps != 0 {
                    if let Some(session) = self
                        .flyover_tabs
                        .get(self.flyover_active)
                        .and_then(Tab::session)
                    {
                        let up = steps > 0;
                        if session.app_consumes_wheel() {
                            let (col, row) =
                                self.renderer.cell_at(&content, px, py).unwrap_or((0, 0));
                            for _ in 0..steps.unsigned_abs() {
                                session.forward_wheel(up, col, row);
                            }
                        } else {
                            session.scroll_by(steps * 3);
                        }
                        self.request_redraw();
                    }
                }
                return;
            }
        }
        // The sidebar's scroll containers: the folders card and the sessions
        // rows (Settings' tab list is short and never scrolls).
        if !self.sidebar_collapsed {
            let scale = self.scale();
            let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
            if self.folders_visible() && self.folders_card(scale).contains(px, py) {
                self.scroll_folders(delta);
                return;
            }
            if self.page != Page::Settings && self.sessions_list(scale).contains(px, py) {
                self.scroll_sessions(delta);
                return;
            }
        }
        // A tool page's terminal takes the wheel (a TUI usually claims it).
        if let Page::Tool(i) = self.page {
            if let Some(Some(ts)) = self.tool_sessions.get(i) {
                let scale = self.scale();
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                let notches = match delta {
                    gpui::ScrollDelta::Lines(p) => p.y as f64,
                    gpui::ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / (cell_height as f64 * 3.0),
                };
                let steps = scroll_steps(&mut self.scroll_accum, notches);
                if steps != 0 {
                    let Some(session) = ts.tab.session() else { return };
                    let content = workspace::tile_content(&self.tool_area(), scale);
                    let up = steps > 0;
                    if session.app_consumes_wheel() {
                        let (col, row) = self.renderer.cell_at(&content, px, py).unwrap_or((0, 0));
                        for _ in 0..steps.unsigned_abs() {
                            session.forward_wheel(up, col, row);
                        }
                    } else {
                        session.scroll_by(steps * 3);
                    }
                    self.request_redraw();
                }
            }
            return;
        }
        // Only the Sessions page has terminals to scroll; the other pages'
        // gpui scroll containers handle their own wheel events.
        if self.page != Page::Sessions {
            return;
        }
        let scale = self.scale();
        let cell_h = cell_height as f64;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        // Accumulate into whole wheel "steps": one per wheel notch, or one per
        // ~3 cell-heights of trackpad travel, so sub-step deltas aren't lost.
        let notches = match delta {
            gpui::ScrollDelta::Lines(p) => p.y as f64,
            gpui::ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / (cell_h * 3.0),
        };
        let steps = scroll_steps(&mut self.scroll_accum, notches);
        if steps == 0 {
            return;
        }

        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        let Some((id, rect)) =
            tiles.iter().find(|(_, r)| workspace::tile_content(r, scale).contains(px, py))
        else {
            return;
        };
        let Some(tab) = ws.root.find_tile(*id).and_then(|t| t.active_tab()) else {
            return;
        };
        let Some(session) = tab.session() else { return };
        let up = steps > 0;

        if session.app_consumes_wheel() {
            // Hand the wheel to the app (mouse report, or alternate-scroll arrow
            // keys on the alternate screen) — once per accumulated step.
            let content = workspace::tile_content(rect, scale);
            let (col, row) = self.renderer.cell_at(&content, px, py).unwrap_or((0, 0));
            for _ in 0..steps.unsigned_abs() {
                session.forward_wheel(up, col, row);
            }
        } else {
            // Primary screen: scroll our own scrollback, ~3 lines per notch.
            session.scroll_by(steps * 3);
        }
        self.request_redraw();
    }

    fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// ⌘V: clipboard → focused terminal (bracketed-paste aware).
    /// Copy the active selection's text to the system clipboard.
    fn copy(&mut self) {
        let ws = &self.workspaces[self.active];
        let Some(tab) = ws.focused().and_then(|t| t.active_tab()) else { return };
        let Some(text) = tab.session().and_then(Session::selected_text) else { return };
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    }

    fn paste(&mut self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        let ws = &self.workspaces[self.active];
        let Some(tab) = ws.focused().and_then(|t| t.active_tab()) else { return };
        let Some(session) = tab.session() else { return };
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => session.paste(&text),
            _ if clipboard.get_image().is_ok() => session.write([0x16u8]),
            _ => return,
        }
        // Like typing: a paste follows the live output and drops any selection.
        session.scroll_to_bottom();
        session.clear_selection();
        self.request_redraw();
    }

    /// Clear the unread dot on the flyover tab that just came on screen
    /// (panel opened or active tab switched).
    fn flyover_mark_read(&mut self) {
        if !self.flyover_open && !(self.flyover_windowed && self.flyover_window_visible) {
            return;
        }
        if let Some(tab) = self.flyover_tabs.get_mut(self.flyover_active) {
            tab.unread = false;
        }
    }

    /// The flyover panel's rect for the current animation/size state.
    fn flyover_rect_now(&self) -> workspace::LayoutRect {
        let (w, h) = self.renderer.surface_size();
        workspace::flyover_rect(
            w,
            h,
            self.scale(),
            self.flyover_anim,
            self.flyover_height_frac,
            self.flyover_maximized,
        )
    }

    /// Close flyover tab `ti`, dropping its session (which kills the PTY).
    /// The last tab closing hides whichever surface was showing the flyover.
    fn close_flyover_tab(&mut self, ti: usize) {
        if ti >= self.flyover_tabs.len() {
            return;
        }
        self.flyover_tabs.remove(ti);
        if self.flyover_tabs.is_empty() {
            self.flyover_open = false;
            self.flyover_focused = false;
            self.flyover_window_visible = false;
        } else {
            if ti < self.flyover_active {
                self.flyover_active -= 1;
            }
            self.flyover_active = self.flyover_active.min(self.flyover_tabs.len() - 1);
        }
        self.request_redraw();
    }

    /// Toggle the flyover panel between its resizable height and filling the
    /// whole window.
    pub(crate) fn flyover_toggle_maximized(&mut self) {
        self.flyover_maximized = !self.flyover_maximized;
        self.sync_flyover_layout(true);
        self.request_redraw();
    }

    /// Move the flyover between the in-window panel and its own popout
    /// window. The sessions never move — only which surface renders them.
    fn flyover_toggle_windowed(&mut self) {
        if self.flyover_windowed {
            // Dock back: the pump closes the window; the panel takes over.
            self.flyover_windowed = false;
            self.flyover_window_visible = false;
            self.flyover_open = true;
            self.flyover_focused = true;
            self.flyover_mark_read();
        } else {
            // Pop out: the panel slides away; the pump opens the window.
            self.flyover_windowed = true;
            self.flyover_window_visible = true;
            self.flyover_open = false;
            self.flyover_focused = false;
            if self.flyover_tabs.is_empty() {
                self.open_flyover_picker();
            }
        }
        self.request_redraw();
    }

    /// The session with `id`, wherever it lives (any workspace, tile, tab, or
    /// the flyover panel).
    fn find_session(&self, id: u64) -> Option<&Session> {
        self.workspaces
            .iter()
            .find_map(|ws| {
                ws.root
                    .tiles()
                    .into_iter()
                    .flat_map(|t| t.tabs.iter())
                    .filter_map(Tab::session)
                    .find(|session| session.id == id)
            })
            .or_else(|| self.flyover_tabs.iter().filter_map(Tab::session).find(|s| s.id == id))
            .or_else(|| {
                self.tool_sessions
                    .iter()
                    .flatten()
                    .filter_map(|ts| ts.tab.session())
                    .find(|session| session.id == id)
            })
    }

    /// True when the session is the active tab of the flyover and the flyover
    /// is on screen — panel open, or popout window showing. On screen
    /// regardless of which page is showing, since both surfaces overlay them.
    fn flyover_visible(&self, id: u64) -> bool {
        let showing =
            if self.flyover_windowed { self.flyover_window_visible } else { self.flyover_open };
        showing
            && self
                .flyover_tabs
                .get(self.flyover_active)
                .and_then(Tab::session)
                .is_some_and(|session| session.id == id)
    }

    /// True when the session is the *visible* tab of a tile in the active
    /// workspace, or the active tab of the open flyover panel.
    fn is_visible(&self, id: u64) -> bool {
        // A collapsed pane's content is hidden, so its tabs are not watched
        // even though they sit in the active workspace.
        self.workspaces[self.active]
            .root
            .tiles()
            .iter()
            .any(|t| {
                !t.collapsed
                    && t.active_tab()
                        .and_then(Tab::session)
                        .is_some_and(|session| session.id == id)
            })
            || self.flyover_visible(id)
            || self
                .active_tool_session()
                .and_then(|ts| ts.tab.session())
                .is_some_and(|session| session.id == id)
    }

    /// Record that the tab owning session `id` asked for attention, without
    /// dotting it — the pane is on screen, so the user is already watching it.
    ///
    /// The stamp still moves so the group's card can say when its work last
    /// spoke up; only the unread dot is withheld. An already-unread tab keeps
    /// its existing stamp, exactly as the mark-unread paths do: that stamp is
    /// the moment it started waiting, which is what a card reports, and it
    /// also keeps a chatty pane from rewriting the snapshot on every toast.
    /// Returns whether a stamp was written, so the caller knows to redraw.
    fn stamp_attention_by_session(&mut self, id: u64) -> bool {
        let now = SystemTime::now();
        if let Some(tab) = self
            .flyover_tabs
            .iter_mut()
            .find(|tab| tab.session().is_some_and(|session| session.id == id))
        {
            if !tab.unread {
                tab.unread_at = Some(now);
            }
            return false; // Flyover tabs have no sidebar card to restamp.
        }
        let changed = self.workspaces.iter_mut().any(|ws| {
            ws.root.tiles_mut().into_iter().any(|t| {
                t.tabs.iter_mut().any(|tab| {
                    let hit = tab.session().is_some_and(|session| session.id == id) && !tab.unread;
                    if hit {
                        tab.unread_at = Some(now);
                    }
                    hit
                })
            })
        });
        if changed {
            self.persist_snapshot();
        }
        changed
    }

    /// Mark the tab owning session `id` unread. Returns true (and persists)
    /// only on a false→true transition, so repeated attention signals from
    /// one pane don't churn the snapshot.
    fn set_unread_by_session(&mut self, id: u64) -> bool {
        // Flyover tabs aren't persisted, so their dots skip the snapshot.
        if let Some(tab) = self
            .flyover_tabs
            .iter_mut()
            .find(|tab| tab.session().is_some_and(|session| session.id == id))
        {
            let hit = !tab.unread;
            tab.unread = true;
            if hit {
                tab.unread_at = Some(SystemTime::now());
            }
            return hit;
        }
        let changed = self.workspaces.iter_mut().any(|ws| {
            ws.root.tiles_mut().into_iter().any(|t| {
                t.tabs.iter_mut().any(|tab| {
                    let hit = tab.session().is_some_and(|session| session.id == id) && !tab.unread;
                    if hit {
                        tab.unread = true;
                        tab.unread_at = Some(SystemTime::now());
                    }
                    hit
                })
            })
        });
        if changed {
            self.persist_snapshot();
        }
        changed
    }

    /// The "visible = read" sweep: clear the unread dot on every on-screen
    /// tab (each tile's active tab) of the active group. Called after actions
    /// that change what's on screen — never per frame, or a manual
    /// mark-as-unread on a visible tab would clear before it could be seen.
    fn mark_visible_read(&mut self) {
        if self.page != Page::Sessions {
            return;
        }
        let mut changed = false;
        for tile in self.workspaces[self.active].root.tiles_mut() {
            // Collapsed panes stay unread — their content isn't on screen.
            if tile.collapsed {
                continue;
            }
            if let Some(tab) = tile.active_tab_mut()
                && tab.unread
            {
                tab.unread = false;
                changed = true;
            }
        }
        if changed {
            self.persist_snapshot();
            self.request_redraw();
        }
    }

    /// Right-click marks things unread again — the "come back to this later"
    /// gesture. A sidebar group card re-dots its primary pane's active tab
    /// (the same tab the card's dot mirrors); a tile tab re-dots that tab.
    /// Never changes focus.
    fn on_right_mouse_down(&mut self) {
        // A mouse-tracking TUI under the cursor gets the right-click as a
        // report (before any sidebar context handling below).
        if self.try_forward_secondary_press(MouseBtn::Right) {
            return;
        }
        if self.page != Page::Sessions
            || self.confirm.is_some()
            || self.message.is_some()
            || self.webview_prompt.is_some()
            || self.command.is_some()
        {
            return;
        }
        let scale = self.scale();
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (_, h) = self.renderer.surface_size();

        if workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            let rows = self.sidebar_rows();
            for (ri, row) in rows.iter().enumerate() {
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    &self.sessions_rows_list(scale),
                );
                if !rect.contains(px, py) {
                    continue;
                }
                let workspace::SidebarRow { ws_idx } = *row;
                let ws = &mut self.workspaces[ws_idx];
                let primary = ws.primary_tile;
                if let Some(tab) = ws.root.find_tile_mut(primary).and_then(|t| t.active_tab_mut())
                    && !tab.unread
                {
                    tab.unread = true;
                    tab.unread_at = Some(SystemTime::now());
                    self.persist_snapshot();
                    self.request_redraw();
                }
                return;
            }
            return;
        }

        // Tile tab strips of the active group.
        let area = self.area();
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        let axes = workspace::tile_collapse_axis(&ws.root);
        for (id, rect) in &tiles {
            let strip = workspace::tab_strip_rect(area, rect, scale, self.sidebar_w());
            let bar = workspace::tile_tab_bar(&strip, scale);
            if !bar.contains(px, py) {
                continue;
            }
            let has_caret = axes.iter().any(|(tid, a)| tid == id && a.is_some());
            if let Some(tile) = self.workspaces[self.active].root.find_tile_mut(*id) {
                let n = tile.tabs.len();
                if n == 0 {
                    return;
                }
                let t0 = workspace::tile_tab_rect(&strip, 0, n, scale, has_caret);
                let ti = ((((px - t0.x).max(0.0)) / t0.w).floor() as usize).min(n - 1);
                if let Some(tab) = tile.tabs.get_mut(ti)
                    && !tab.unread
                {
                    tab.unread = true;
                    tab.unread_at = Some(SystemTime::now());
                    self.persist_snapshot();
                    self.request_redraw();
                }
            }
            return;
        }
    }

    // ── cwd picker ──────────────────────────────────────────────────────

    /// True while the command palette owns the shared search field.
    fn search_modal_open(&self) -> bool {
        self.command.is_some() || self.webview_prompt.is_some()
    }

    /// Point the shared search field at the palette's current stage: clear
    /// it, set the stage's placeholder, focus it (consumed in `render`) —
    /// and, when the stage is the repo step, scan the directories for it
    /// (the model is filesystem-free; only the app scans).
    fn claim_command_search(&mut self) {
        if let Some(pal) = self.command.as_mut()
            && pal.needs_repo()
        {
            pal.provide_repo(picker::Picker::new());
        }
        self.command_scroll_to = self.command.as_ref().map(|c| c.selected());
        self.modal_search_reset = Some(self.command_placeholder().to_string());
    }

    /// Toggle the palette at its command root (⌘P).
    fn toggle_command_root(&mut self) {
        if self.command.is_some() {
            self.close_command();
        } else {
            self.command = Some(command::CommandPalette::root());
            self.claim_command_search();
        }
        self.request_redraw();
    }

    /// Open the palette with the New-session command already committed —
    /// the ＋ button and ⇧⌘T — for a group, or for the flyover terminal.
    fn open_command_new_session(&mut self, for_flyover: bool) {
        self.command = Some(command::CommandPalette::new_session(for_flyover));
        self.claim_command_search();
        self.request_redraw();
    }

    /// Close the palette without choosing. Cancelling the flyover's
    /// first-open flow closes the waiting surface too — there is nothing
    /// to show yet.
    pub(crate) fn close_command(&mut self) {
        let for_flyover = self.command.as_ref().is_some_and(|c| c.for_flyover);
        self.command = None;
        if for_flyover && self.flyover_tabs.is_empty() {
            self.flyover_open = false;
            self.flyover_focused = false;
            self.flyover_window_visible = false;
        }
        self.request_redraw();
    }

    /// ↩ (or a row click) on the palette's current stage.
    pub(crate) fn command_enter(&mut self) {
        use command::Outcome;
        let Some(outcome) = self.command.as_mut().map(|c| c.enter()) else { return };
        match outcome {
            Outcome::Nothing => self.claim_command_search(),
            Outcome::Run(action) => {
                self.command = None;
                self.run_action(action);
            },
            Outcome::RepoChosen(entry) => {
                let for_flyover = {
                    let pal = self.command.as_mut().expect("palette open");
                    if let Some(p) = pal.repo.as_mut() {
                        p.record_recent(&entry.path);
                    }
                    pal.for_flyover
                };
                if for_flyover && !entry.is_git {
                    self.command = None;
                    self.spawn_flyover_tab(Some(entry.path));
                } else if entry.is_git {
                    let all = build_fork_choices(&entry.path);
                    let choices = if for_flyover {
                        all.into_iter()
                            .filter(|c| {
                                matches!(
                                    c.scope,
                                    picker::ForkScope::RepoRoot | picker::ForkScope::Worktree
                                )
                            })
                            .collect()
                    } else {
                        all
                    };
                    if let Some(pal) = self.command.as_mut() {
                        pal.provide_base(picker::ForkPicker::new(entry.path, choices));
                    }
                    self.claim_command_search();
                } else {
                    let found = pwrspace::discover(&pwrspace::candidate_paths(&entry.path));
                    if let Some(pal) = self.command.as_mut() {
                        pal.provide_layout(picker::ProfilePicker::new(found));
                    }
                    self.claim_command_search();
                }
            },
            Outcome::BaseChosen(entry) => {
                let (for_flyover, repo) = {
                    let pal = self.command.as_ref().expect("palette open");
                    (pal.for_flyover, pal.base.as_ref().map(|b| b.repo.clone()))
                };
                if for_flyover {
                    self.command = None;
                    if matches!(entry.scope, picker::ForkScope::RepoRoot | picker::ForkScope::Worktree)
                    {
                        self.spawn_flyover_tab(entry.path);
                    } else if self.flyover_tabs.is_empty() {
                        self.flyover_open = false;
                    }
                } else {
                    let root = entry.path.clone().or(repo);
                    let found = root
                        .map(|p| pwrspace::discover(&pwrspace::candidate_paths(&p)))
                        .unwrap_or_default();
                    if let Some(pal) = self.command.as_mut() {
                        pal.provide_layout(picker::ProfilePicker::new(found));
                    }
                    self.claim_command_search();
                }
            },
            Outcome::LayoutChosen => {
                let folders: Vec<picker::FolderSource> = self
                    .sections
                    .iter()
                    .map(|s| picker::FolderSource {
                        id: s.id,
                        name: s.name.clone(),
                        emoji: s.emoji.clone(),
                        groups: self.workspaces.iter().filter(|w| w.section == Some(s.id)).count(),
                    })
                    .collect();
                if let Some(pal) = self.command.as_mut() {
                    pal.provide_folder(picker::FolderPicker::new(folders));
                }
                self.claim_command_search();
            },
            Outcome::Launch(session) => {
                self.command = None;
                self.launch_new_session(session);
            },
            Outcome::Close => self.close_command(),
        }
        self.request_redraw();
    }

    /// ⌫ on an empty query (or the rail's "⌫ back"): pop the last token.
    pub(crate) fn command_back(&mut self) {
        let Some(outcome) = self.command.as_mut().map(|c| c.pop()) else { return };
        if outcome == command::Outcome::Close {
            self.close_command();
        } else {
            self.claim_command_search();
        }
        self.request_redraw();
    }

    /// The Done card's "Start over": back to the repo step, picks cleared.
    pub(crate) fn command_start_over(&mut self) {
        let for_flyover = self.command.as_ref().is_some_and(|c| c.for_flyover);
        self.open_command_new_session(for_flyover);
    }

    /// Create the group (or flyover tab) the New-session flow described.
    fn launch_new_session(&mut self, session: command::NewSession) {
        let command::NewSession { name, repo, base, layout, folder, for_flyover } = session;
        if for_flyover {
            let cwd = base.and_then(|b| b.path).or(Some(repo.path));
            self.spawn_flyover_tab(cwd);
            return;
        }
        // Resolve the folder pick to a section id up front — a "New folder…"
        // pick creates its (empty) section right away — so both the in-place
        // and the async fork paths can file the group once it exists.
        let section = folder.and_then(|f| match f.kind {
            picker::FolderKind::TopLevel => None,
            picker::FolderKind::Existing { id } => Some(id),
            picker::FolderKind::New { name } => Some(self.new_section(name)),
        });
        let profile = layout.and_then(|l| l.profile);
        match base {
            // A plain directory: open it, with the chosen layout.
            None => {
                match profile {
                    Some(profile) => self.add_group_with_profile(name, Some(repo.path), &profile),
                    None => self.add_group(name, Some(repo.path)),
                }
                self.file_active_group(section);
            },
            // The repo root or an existing worktree: open in place.
            Some(b)
                if matches!(b.scope, picker::ForkScope::RepoRoot | picker::ForkScope::Worktree) =>
            {
                match (b.path, profile) {
                    (Some(p), Some(profile)) => self.add_group_with_profile(name, Some(p), &profile),
                    (Some(p), None) => self.add_group(name, Some(p)),
                    (None, _) => self.add_group(name, None),
                }
                self.file_active_group(section);
            },
            // A fresh worktree off a branch: provision through `drop`, and
            // apply the layout and folder once the worktree exists.
            Some(b) => {
                self.pending_group_profile = profile;
                self.pending_group_section = section;
                self.start_fork(repo.path, name, b.from);
            },
        }
    }

    /// File the just-created active group under `section`, keeping the
    /// section's member block contiguous and its header expanded.
    fn file_active_group(&mut self, section: Option<u64>) {
        let Some(section_id) = section else { return };
        self.active = workspace::append_to_section(&mut self.workspaces, self.active, section_id);
        workspace::normalize_section_anchors(&self.workspaces, &mut self.sections);
        workspace::ensure_active_section_expanded(&self.workspaces, &mut self.sections, self.active);
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    fn open_picker(&mut self) {
        self.open_command_new_session(false);
    }

    fn open_flyover_picker(&mut self) {
        self.open_command_new_session(true);
    }

    /// Spawn a new flyover tab with a plain (non-persisted) session at `cwd`.
    fn spawn_flyover_tab(&mut self, cwd: Option<std::path::PathBuf>) {
        let session = self.spawn_session_named(cwd.as_deref(), None);
        self.flyover_tabs.push(workspace::Tab::new(session));
        self.flyover_active = self.flyover_tabs.len() - 1;
        self.flyover_focused = true;
        self.sync_layout();
        self.request_redraw();
    }

    /// Toggle the flyover panel open/closed. In windowed mode this shows or
    /// hides the popout window instead (the pump reconciles the actual
    /// window); sessions keep running either way.
    pub(crate) fn toggle_flyover(&mut self) {
        if self.flyover_windowed {
            self.flyover_window_visible = !self.flyover_window_visible;
            if self.flyover_window_visible {
                self.flyover_mark_read();
                if self.flyover_tabs.is_empty() {
                    self.open_flyover_picker();
                }
            }
            self.request_redraw();
            return;
        }
        if self.flyover_open {
            // Close: hide but keep sessions running.
            self.flyover_open = false;
            self.flyover_focused = false;
            self.request_redraw();
        } else {
            // Open: show the panel.
            self.flyover_open = true;
            self.flyover_focused = true;
            self.flyover_mark_read();
            if self.flyover_tabs.is_empty() {
                // First-ever open: open directory picker to create the first tab.
                self.open_flyover_picker();
            }
            self.request_redraw();
        }
    }

    /// Apply a ⌘ action to the flyover's tabs — the subset of shortcuts the
    /// flyover captures while it has focus. Returns false when the action
    /// isn't flyover-scoped so the caller can fall through to global routing.
    fn flyover_shortcut(&mut self, action: Action) -> bool {
        match action {
            Action::NewTab => {
                self.open_flyover_picker();
            },
            Action::CloseTab => {
                self.close_flyover_tab(self.flyover_active);
            },
            Action::PrevTab => {
                if !self.flyover_tabs.is_empty() {
                    self.flyover_active =
                        pages::cycle(self.flyover_active, self.flyover_tabs.len(), -1);
                    self.flyover_mark_read();
                }
            },
            Action::NextTab => {
                if !self.flyover_tabs.is_empty() {
                    self.flyover_active =
                        pages::cycle(self.flyover_active, self.flyover_tabs.len(), 1);
                    self.flyover_mark_read();
                }
            },
            Action::Copy => {
                if let Some(tab) = self.flyover_tabs.get(self.flyover_active)
                    && let Some(text) = tab.session().and_then(Session::selected_text)
                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                {
                    let _ = clipboard.set_text(text);
                }
            },
            Action::Paste => {
                if let Some(tab) = self.flyover_tabs.get(self.flyover_active)
                    && let Some(session) = tab.session()
                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                {
                    match clipboard.get_text() {
                        Ok(text) if !text.is_empty() => {
                            session.paste(&text);
                            session.scroll_to_bottom();
                            session.clear_selection();
                        },
                        _ if clipboard.get_image().is_ok() => {
                            session.write([0x16u8]);
                        },
                        _ => {},
                    }
                }
            },
            _ => return false,
        }
        true
    }

    /// Write a plain (non-⌘) keystroke's bytes to the active flyover session.
    fn flyover_write_key(&mut self, keystroke: &Keystroke) {
        if let Some(bytes) = key_to_bytes(keystroke)
            && let Some(tab) = self.flyover_tabs.get(self.flyover_active)
            && let Some(session) = tab.session()
        {
            session.write(bytes);
            session.scroll_to_bottom();
            session.clear_selection();
        }
    }

    /// Keyboard routing for the popout window. Global actions that concern
    /// the main window (group switching, settings, …) are ignored here rather
    /// than fired against a window that isn't showing. Returns an effect the
    /// popout view must apply outside this entity.
    fn popout_key(&mut self, ev: &KeyDownEvent) -> Option<PopoutEffect> {
        self.modifiers = ev.keystroke.modifiers;
        if ev.keystroke.modifiers.platform {
            if pages::Action::ToggleFlyover.binding().matches(&ev.keystroke) {
                // Hide: the pump closes the window; sessions keep running.
                self.flyover_window_visible = false;
                self.request_redraw();
                return None;
            }
            if let Some(action) = pages::match_action(&ev.keystroke) {
                if action == Action::FlyoverPopout {
                    self.flyover_toggle_windowed();
                    return Some(PopoutEffect::ActivateMain);
                }
                if self.flyover_shortcut(action) {
                    self.request_redraw();
                    // The new-tab picker renders in the main window.
                    if action == Action::NewTab {
                        return Some(PopoutEffect::ActivateMain);
                    }
                }
            }
            return None;
        }
        self.flyover_write_key(&ev.keystroke);
        self.request_redraw();
        None
    }

    fn start_fork(&mut self, repo: std::path::PathBuf, name: String, from: Option<String>) {
        self.message = Some((format!("Provisioning worktree for {name}…"), false));
        self.request_redraw();

        let events_tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let event = match run_drop(&repo, from.as_deref()) {
                Ok(cwd) => TermEvent::GroupReady { name, cwd },
                Err(message) => TermEvent::GroupFailed { message },
            };
            let _ = events_tx.send(event);
        });
    }

    fn new_tile_in(&mut self, cwd: Option<&std::path::Path>) -> Tile {
        let id = self.next_tile_id;
        self.next_tile_id += 1;
        let session = self.spawn_session_in(cwd);
        Tile::new(id, session)
    }

    /// True when the app shows the empty state: a sole, tab-less group.
    fn is_empty_state(&self) -> bool {
        self.workspaces.len() == 1 && self.workspaces[0].is_empty()
    }

    /// Return to the empty state: swap the sole leftover workspace for a
    /// fresh placeholder so it matches launch exactly (name and cwd reset).
    fn reset_empty_workspace(&mut self, wi: usize) {
        self.workspaces[wi] = Workspace::placeholder();
        self.active = wi;
        self.persist_snapshot();
    }

    /// Open a new group named `name`, rooted at `cwd`, and make it active.
    /// From the empty state the new group replaces the placeholder instead of
    /// stacking beside it.
    fn add_group(&mut self, name: String, cwd: Option<std::path::PathBuf>) {
        let empty = self.is_empty_state();
        let tile = self.new_tile_in(cwd.as_deref());
        // Queue the primary command for the founding pane; it is written on
        // the session's first wakeup so the shell's startup files can't eat it.
        let cmd = settings::primary_command();
        if !cmd.trim().is_empty()
            && let Some(tab) = tile.tabs.first()
            && let Some(session) = tab.session()
        {
            self.pending_primary_cmd.insert(session.id, cmd);
        }
        let mut ws = Workspace::new(name, tile, cwd);
        // A group made while a folder is showing belongs to that folder.
        ws.section = self.folder_filter;
        if empty {
            self.workspaces[0] = ws;
            self.active = 0;
        } else {
            self.workspaces.push(ws);
            self.active = self.workspaces.len() - 1;
        }
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Open a new group at `cwd` laid out per `profile`: the saved split tree
    /// is rebuilt with fresh tiles/sessions and every tab's command is queued
    /// for its pane's first wakeup — the same mechanism as the primary
    /// command, so shell startup files can't eat it.
    fn add_group_with_profile(
        &mut self,
        name: String,
        cwd: Option<std::path::PathBuf>,
        profile: &pwrspace::WorkspaceProfile,
    ) {
        let empty = self.is_empty_state();
        let root = self.build_profile_node(&profile.layout, cwd.as_deref());
        // The first leaf anchors the group's lifetime, mirroring add_group's
        // founding tile.
        let primary_tile = root.tiles().first().map(|t| t.id).unwrap_or(0);
        let mut ws = Workspace {
            name,
            root,
            focused_tile: primary_tile,
            cwd,
            primary_tile,
            section: self.folder_filter,
            pinned: false,
        };
        ws.fix_focus();
        if empty {
            self.workspaces[0] = ws;
            self.active = 0;
        } else {
            self.workspaces.push(ws);
            self.active = self.workspaces.len() - 1;
        }
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Recursively build a live split tree from a profile node, spawning a
    /// session in `cwd` for every tab and queueing its command. A leaf with no
    /// tabs still gets one bare shell so no tile is ever empty; ratios are
    /// clamped so a hand-edited file can't collapse a pane to nothing.
    fn build_profile_node(
        &mut self,
        node: &pwrspace::ProfileNode,
        cwd: Option<&std::path::Path>,
    ) -> Node {
        match node {
            pwrspace::ProfileNode::Leaf(leaf) => {
                let id = self.next_tile_id;
                self.next_tile_id += 1;
                let mut tile = Tile::empty(id);
                let bare = [pwrspace::ProfileTab::default()];
                let tabs: &[pwrspace::ProfileTab] =
                    if leaf.tabs.is_empty() { &bare } else { &leaf.tabs };
                for profile_tab in tabs {
                    if profile_tab.kind == pwrspace::ProfileTabKind::Webview
                        && let Some(url) = profile_tab
                            .url
                            .as_deref()
                            .and_then(|url| bus::validate_webview_url(url).ok())
                    {
                        tile.tabs.push(self.new_webview_tab(url));
                    } else {
                        let session = self.spawn_session_in(cwd);
                        if let Some(cmd) = profile_tab
                            .command
                            .as_deref()
                            .map(str::trim)
                            .filter(|c| !c.is_empty())
                        {
                            self.pending_primary_cmd.insert(session.id, cmd.to_string());
                        }
                        tile.tabs.push(Tab::new(session));
                    }
                }
                tile.active = leaf.active.min(tile.tabs.len() - 1);
                Node::Leaf(tile)
            },
            pwrspace::ProfileNode::Split(split) => Node::Split {
                dir: match split.split {
                    pwrspace::SplitDir::Row => Dir::Row,
                    pwrspace::SplitDir::Column => Dir::Column,
                },
                ratio: split.ratio.clamp(0.05, 0.95),
                a: Box::new(self.build_profile_node(&split.a, cwd)),
                b: Box::new(self.build_profile_node(&split.b, cwd)),
            },
        }
    }

    /// Open the save-as-workspace modal over the active group, prefilled with
    /// the group's name. The repo destination row is offered only when the
    /// group's cwd resolves to a git repo (for a worktree that is the *main*
    /// checkout root, so the profile is visible to every future worktree).
    fn open_save_workspace(&mut self) {
        if self.is_empty_state() {
            return;
        }
        let ws = &self.workspaces[self.active];
        let mut dest_labels = Vec::new();
        let mut dest_paths = Vec::new();
        if let Some(root) = ws.cwd.as_deref().and_then(git::repo_root) {
            let path = root.join(".pwrspace.json");
            dest_labels.push(format!("This repo — {}", tilde(&path)));
            dest_paths.push(path);
        }
        let user = settings::config_dir().join("pwrspace.json");
        dest_labels.push(format!("User — {}", tilde(&user)));
        dest_paths.push(user);
        self.save_ws = Some(SaveWorkspaceModal {
            name: ws.name.clone(),
            description: String::new(),
            field: 0,
            dest_selected: None,
            dest_labels,
            dest_paths,
        });
        self.save_sync = true;
        self.request_redraw();
    }

    /// Write the modal's profile to the selected destination and close it,
    /// reporting the outcome via the message overlay. A blank name refuses to
    /// commit (focus returns to the Name field instead).
    fn commit_save_workspace(&mut self) {
        let Some(modal) = self.save_ws.take() else { return };
        let name = modal.name.trim().to_string();
        if name.is_empty() {
            self.save_ws =
                Some(SaveWorkspaceModal { field: 0, dest_selected: None, ..modal });
            self.save_sync = true;
            return;
        }
        let Some(path) = modal.dest_selected.and_then(|i| modal.dest_paths.get(i)) else {
            return;
        };
        let profile = pwrspace::WorkspaceProfile {
            name,
            description: modal.description.trim().to_string(),
            layout: capture_profile_node(&self.workspaces[self.active].root),
        };
        match pwrspace::save_profile(path, &profile) {
            Ok(()) => {
                self.message = Some((
                    format!("Saved workspace \"{}\" to {}", profile.name, tilde(path)),
                    true,
                ));
            },
            Err(e) => {
                self.message = Some((format!("Save failed: {e}"), true));
            },
        }
        self.request_redraw();
    }

    // ── Tab drag / drop ───────────────────────────────────────────────────

    fn take_tab(&mut self, wi: usize, tile_id: u64, tab_idx: usize) -> Option<Tab> {
        let ws = self.workspaces.get_mut(wi)?;
        let tile = ws.root.find_tile_mut(tile_id)?;
        if tab_idx >= tile.tabs.len() {
            return None;
        }
        let tab = tile.tabs.remove(tab_idx);
        if tile.active >= tile.tabs.len() {
            tile.active = tile.tabs.len().saturating_sub(1);
        }
        if tile.tabs.is_empty() {
            let _ = ws.root.remove_tile(tile_id);
            // The removed tile may have been the focused one.
            ws.fix_focus();
        }
        Some(tab)
    }

    /// Resolve a terminal-tab drag (whose source is `src_tile`) to a landing
    /// zone. Taken as a parameter — not read from `self.drag` — because
    /// mouse-up clears the drag before resolving the drop.
    fn resolve_drop(&self, px: f32, py: f32, src_tile: u64) -> Option<DropTarget> {
        let scale = self.scale();
        let area = self.area();
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        let axes = workspace::tile_collapse_axis(&ws.root);
        for (id, rect) in &tiles {
            if !rect.contains(px, py) {
                continue;
            }
            let strip = workspace::tab_strip_rect(area, rect, scale, self.sidebar_w());
            let bar = workspace::tile_tab_bar(&strip, scale);
            if bar.contains(px, py) {
                let has_caret = axes.iter().any(|(tid, a)| tid == id && a.is_some());
                let n = ws.root.find_tile(*id).map_or(1, |t| t.tabs.len()).max(1);
                // Same-tile reorder resolves to the nearest gap (matching the
                // insertion-line preview); cross-tile keeps the hovered cell.
                let index = if src_tile == *id {
                    workspace::tile_tab_insert_gap(&strip, px, n, scale, has_caret)
                } else {
                    let t0 = workspace::tile_tab_rect(&strip, 0, n, scale, has_caret);
                    ((((px - t0.x).max(0.0)) / t0.w).floor() as usize).min(n)
                };
                return Some(DropTarget::TabBar { tile: *id, index });
            }
            let content = workspace::tile_content(rect, scale);
            // Edge bands: outer eighth of the content on each side splits.
            let ex = content.w / 4.0;
            let ey = content.h / 4.0;
            if px < content.x + ex {
                return Some(DropTarget::Edge { tile: *id, dir: Dir::Row, first: true });
            }
            if px > content.x + content.w - ex {
                return Some(DropTarget::Edge { tile: *id, dir: Dir::Row, first: false });
            }
            if py < content.y + ey {
                return Some(DropTarget::Edge { tile: *id, dir: Dir::Column, first: true });
            }
            if py > content.y + content.h - ey {
                return Some(DropTarget::Edge { tile: *id, dir: Dir::Column, first: false });
            }
            return Some(DropTarget::Center { tile: *id });
        }
        // Terminal-tab → sidebar group: hit-test via the shared row list.
        let (_, h) = self.renderer.surface_size();
        if workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            let rows = self.sidebar_rows();
            for (ri, row) in rows.iter().enumerate() {
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    &self.sessions_rows_list(scale),
                );
                if !rect.contains(px, py) {
                    continue;
                }
                match *row {
                    workspace::SidebarRow { ws_idx } => {
                        return Some(DropTarget::Group { ws: ws_idx });
                    },
                }
            }
        }
        None
    }

    /// Resolve a sidebar-group drag against the shared row list.
    /// Edge zones are insertion points; row middles join/append.
    fn resolve_sidebar_group_drop(&self, px: f32, py: f32) -> Option<DropTarget> {
        let scale = self.scale();
        let (_, h) = self.renderer.surface_size();
        if !workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            return None;
        }
        // Folder rows in the folders card: drop into that folder ("All
        // sessions" drops the group out of every folder, at the end).
        if self.folders_visible() {
            let card = self.folders_card(scale);
            if card.contains(px, py) {
                let rows = self.folder_rows();
                let scroll = self.folders_scroll() * scale;
                for (i, row) in rows.iter().enumerate() {
                    if !workspace::folder_row_rect(&card, &rows, i, scroll, scale).contains(px, py) {
                        continue;
                    }
                    return match *row {
                        workspace::FolderRow::Section(si) => {
                            Some(DropTarget::SidebarAppend { section_id: self.sections[si].id })
                        },
                        workspace::FolderRow::AllSessions => Some(DropTarget::SidebarInsert {
                            before: self.workspaces.len(),
                            section: None,
                            pinned: None,
                            gap: self.sidebar_rows().len(),
                        }),
                        _ => None,
                    };
                }
                return None;
            }
        }
        let rows = self.sidebar_rows();
        // The "Pinned" caption is a landing zone of its own: drop there to
        // pin (at the head of the run), whatever folder the group is in.
        let zone = workspace::pinned_drop_zone(
            &rows,
            &self.workspaces,
            scale,
            &self.sessions_rows_list(scale),
        );
        if zone.contains(px, py) {
            let own = match self.drag {
                Drag::Group { ws } => self.workspaces.get(ws).and_then(|w| w.section),
                _ => None,
            };
            let before = self
                .workspaces
                .iter()
                .position(|w| w.pinned)
                .or_else(|| rows.first().map(|r| r.ws_idx))
                .unwrap_or(self.workspaces.len());
            return Some(DropTarget::SidebarInsert {
                before,
                section: self.folder_filter.or(own),
                pinned: Some(true),
                gap: 0,
            });
        }
        if rows.is_empty() {
            return Some(DropTarget::SidebarInsert { before: 0, section: None, pinned: None, gap: 0 });
        }
        for (ri, row) in rows.iter().enumerate() {
            let rect = workspace::sidebar_row_rect(
                &rows,
                ri,
                &self.workspaces,
                scale,
                &self.sessions_rows_list(scale),
            );
            // Full sidebar x-span for the row's y band (indented members still hit).
            let side = workspace::sidebar(h, scale, self.sidebar_w());
            let hit = workspace::LayoutRect {
                x: side.x,
                y: rect.y,
                w: side.w,
                h: rect.h,
            };
            if py < hit.y || py >= hit.y + hit.h {
                // Allow first-row top slack / last-row bottom handled below.
                if ri == 0 && py < hit.y && py >= hit.y - hit.h * 0.5 {
                    // above first row → insert at start
                    let ws_idx = row.ws_idx;
                    return Some(self.sidebar_insert_at(ws_idx, false, 0, ws_idx));
                }
                continue;
            }
            // The row is two landing zones, its top half and its bottom
            // half: a sort, never a join (dropping a group onto another used
            // to make a folder; folders come from the card now).
            let rel = (py - hit.y) / hit.h.max(1.0);
            let ws_idx = row.ws_idx;
            if rel < 0.5 {
                return Some(self.sidebar_insert_at(ws_idx, false, ri, ws_idx));
            }
            return Some(self.sidebar_insert_at(ws_idx + 1, true, ri + 1, ws_idx));
        }
        // Below the last row → append ungrouped.
        if let Some(last) = rows.len().checked_sub(1) {
            let rect = workspace::sidebar_row_rect(
                &rows,
                last,
                &self.workspaces,
                scale,
                &self.sessions_rows_list(scale),
            );
            if py >= rect.y + rect.h {
                return Some(DropTarget::SidebarInsert {
                    before: self.workspaces.len(),
                    section: None,
                    pinned: Some(false),
                    gap: rows.len(),
                });
            }
        }
        None
    }

    /// Build a `SidebarInsert` for gap `before`, preferring the left neighbor's
    /// section when `prefer_left` (bottom-edge drop) is set. `edge` is the row
    /// whose edge was hit: the drop takes its pinned state, so a group sorted
    /// into the "Pinned" section pins and one sorted out of it unpins. A pin
    /// keeps its folder (the filter's, or its own under "All sessions") — the
    /// pinned run is one visual section over any number of folders.
    fn sidebar_insert_at(&self, before: usize, prefer_left: bool, gap: usize, edge: usize) -> DropTarget {
        let before = before.min(self.workspaces.len());
        if self.workspaces.get(edge).is_some_and(|w| w.pinned) {
            let own = match self.drag {
                Drag::Group { ws } => self.workspaces.get(ws).and_then(|w| w.section),
                _ => None,
            };
            let section = self.folder_filter.or(own);
            return DropTarget::SidebarInsert { before, section, pinned: Some(true), gap };
        }
        let left = before
            .checked_sub(1)
            .and_then(|i| self.workspaces.get(i))
            .and_then(|w| w.section);
        let right = self.workspaces.get(before).and_then(|w| w.section);
        let section = match (left, right) {
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(a), _) if prefer_left => Some(a),
            (_, Some(b)) if !prefer_left => Some(b),
            _ => None,
        };
        DropTarget::SidebarInsert { before, section, pinned: Some(false), gap }
    }

    /// The landing zone the pointer is over right now, if a drag is live.
    ///
    /// The canvas hint and the sidebar's own element-tree feedback both read
    /// this, so a preview can never disagree with the drop `mouse_up` resolves
    /// — they run the same resolvers against the same cursor. `self.cursor` is
    /// already physical px (see the mouse listeners), so it must NOT be scaled
    /// again: doing so put the preview at cursor×scale² and made it disagree
    /// with the landing.
    fn current_drop_target(&self) -> Option<DropTarget> {
        let (x, y) = (self.cursor.0 as f32, self.cursor.1 as f32);
        match self.drag {
            Drag::Tab { tile, .. } => self.resolve_drop(x, y, tile),
            Drag::Group { .. } => self.resolve_sidebar_group_drop(x, y),
            Drag::Folder { .. } => self.resolve_folder_drop(x, y),
            _ => None,
        }
    }

    /// Where a dragged folder row lands: the top half of a folder row sorts
    /// before it, the bottom half after; the run's top/bottom slack maps to
    /// its ends. Only the folders card's section rows are landing zones.
    fn resolve_folder_drop(&self, px: f32, py: f32) -> Option<DropTarget> {
        if !self.folders_visible() {
            return None;
        }
        let scale = self.scale();
        let card = self.folders_card(scale);
        if !card.contains(px, py) {
            return None;
        }
        let rows = self.folder_rows();
        let scroll = self.folders_scroll() * scale;
        let mut first: Option<(usize, usize)> = None;
        let mut last: Option<(usize, usize, workspace::LayoutRect)> = None;
        for (i, row) in rows.iter().enumerate() {
            let workspace::FolderRow::Section(si) = *row else { continue };
            let rect = workspace::folder_row_rect(&card, &rows, i, scroll, scale);
            first.get_or_insert((i, si));
            last = Some((i, si, rect));
            if py < rect.y || py >= rect.y + rect.h {
                continue;
            }
            let rel = (py - rect.y) / rect.h.max(1.0);
            return Some(if rel < 0.5 {
                DropTarget::FolderInsert { before: si, gap: i }
            } else {
                DropTarget::FolderInsert { before: si + 1, gap: i + 1 }
            });
        }
        let (fi, fsi) = first?;
        let (li, lsi, lrect) = last?;
        let frect = workspace::folder_row_rect(&card, &rows, fi, scroll, scale);
        if py < frect.y {
            return Some(DropTarget::FolderInsert { before: fsi, gap: fi });
        }
        if py >= lrect.y + lrect.h {
            return Some(DropTarget::FolderInsert { before: lsi + 1, gap: li + 1 });
        }
        None
    }

    /// The translucent highlight rect for a resolved drop target, at `scale`.
    /// Mirrors `resolve_drop`'s geometry so the preview matches the landing.
    ///
    /// Pass the device scale for canvas painting; `sidebar_ui` passes 1.0
    /// because gpui lays out in logical px. Only the sidebar arms honour that —
    /// the tile arms read `self.area()`, which is physical — so element-tree
    /// callers must keep to [`DropTarget::in_sidebar`] targets.
    fn drop_hint(&self, target: DropTarget, scale: f32) -> Option<workspace::LayoutRect> {
        let rows = self.sidebar_rows();
        let line_h = (2.0 * scale).max(1.0);
        match target {
            DropTarget::Group { ws } => {
                // Find the group row rect via the shared list.
                for (ri, row) in rows.iter().enumerate() {
                    let workspace::SidebarRow { ws_idx } = *row;
                    if ws_idx == ws {
                        return Some(workspace::sidebar_row_rect(
                            &rows,
                            ri,
                            &self.workspaces,
                            scale,
                            &self.sessions_rows_list(scale),
                        ));
                    }
                }
                Some(workspace::tab_rect(ws, scale, &self.sessions_list(scale)))
            },
            DropTarget::SidebarAppend { section_id } => {
                // Highlight the folder's row in the folders card.
                if !self.folders_visible() {
                    return None;
                }
                let section_idx = self.sections.iter().position(|s| s.id == section_id)?;
                let card = self.folders_card(scale);
                let frows = self.folder_rows();
                let index = frows
                    .iter()
                    .position(|r| *r == workspace::FolderRow::Section(section_idx))?;
                let scroll = self.folders_scroll() * scale;
                Some(workspace::folder_row_rect(&card, &frows, index, scroll, scale))
            },
            DropTarget::SidebarInsert { gap, pinned, .. } => {
                // Thin insertion line at the visual row gap. A drop that
                // pins at the end of the pinned run previews above the
                // section divider, not under it.
                let n_pinned = workspace::pinned_run(&rows, &self.workspaces);
                let y = if pinned == Some(true) && gap == n_pinned && gap > 0 {
                    let r = workspace::sidebar_row_rect(
                        &rows,
                        gap - 1,
                        &self.workspaces,
                        scale,
                        &self.sessions_rows_list(scale),
                    );
                    r.y + r.h
                } else {
                    self.sidebar_gap_y(gap, &rows, scale)
                };
                Some(self.sidebar_insert_line(y, &rows, scale, line_h))
            },
            DropTarget::FolderInsert { gap, .. } => {
                if !self.folders_visible() {
                    return None;
                }
                let card = self.folders_card(scale);
                let frows = self.folder_rows();
                let scroll = self.folders_scroll() * scale;
                let y = if gap < frows.len() {
                    workspace::folder_row_rect(&card, &frows, gap, scroll, scale).y
                } else {
                    let last = frows.len().checked_sub(1)?;
                    let r = workspace::folder_row_rect(&card, &frows, last, scroll, scale);
                    r.y + r.h
                };
                let inset = (8.0 * scale).round();
                Some(workspace::LayoutRect {
                    x: card.x + inset,
                    y: y - line_h / 2.0,
                    w: (card.w - 2.0 * inset).max(0.0),
                    h: line_h,
                })
            },
            DropTarget::TabBar { tile, .. }
            | DropTarget::Center { tile }
            | DropTarget::Edge { tile, .. } => {
                let area = self.area();
                let wsp = &self.workspaces[self.active];
                let (tiles, _) = workspace::layout_tiles(&wsp.root, area, scale);
                let (_, rect) = tiles.into_iter().find(|(id, _)| *id == tile)?;
                Some(match target {
                    // Same-tile reorder previews as a thin insertion line at
                    // the landing gap; cross-tile keeps the whole-bar hint.
                    DropTarget::TabBar { index, .. }
                        if matches!(self.drag, Drag::Tab { tile: src, .. } if src == tile) =>
                    {
                        let strip =
                            workspace::tab_strip_rect(area, &rect, scale, self.sidebar_w());
                        let axes = workspace::tile_collapse_axis(&wsp.root);
                        let has_caret =
                            axes.iter().any(|(tid, a)| *tid == tile && a.is_some());
                        let n = wsp.root.find_tile(tile).map_or(1, |t| t.tabs.len()).max(1);
                        workspace::tile_tab_insert_line(&strip, index, n, scale, has_caret)
                    },
                    DropTarget::TabBar { .. } => workspace::tile_tab_bar(
                        &workspace::tab_strip_rect(area, &rect, scale, self.sidebar_w()),
                        scale,
                    ),
                    DropTarget::Center { .. } => workspace::tile_content(&rect, scale),
                    DropTarget::Edge { dir, first, .. } => {
                        let c = workspace::tile_content(&rect, scale);
                        match (dir, first) {
                            (Dir::Row, true) => {
                                workspace::LayoutRect { w: c.w / 2.0, ..c }
                            },
                            (Dir::Row, false) => workspace::LayoutRect {
                                x: c.x + c.w / 2.0,
                                w: c.w / 2.0,
                                ..c
                            },
                            (Dir::Column, true) => {
                                workspace::LayoutRect { h: c.h / 2.0, ..c }
                            },
                            (Dir::Column, false) => workspace::LayoutRect {
                                y: c.y + c.h / 2.0,
                                h: c.h / 2.0,
                                ..c
                            },
                        }
                    },
                    _ => unreachable!(),
                })
            },
        }
    }

    /// Y coordinate of visual row gap `gap`: the top of `rows[gap]`, or the
    /// bottom of the last row when `gap` is past the end.
    fn sidebar_gap_y(&self, gap: usize, rows: &[workspace::SidebarRow], scale: f32) -> f32 {
        if gap < rows.len() {
            return workspace::sidebar_row_rect(
                rows,
                gap,
                &self.workspaces,
                scale,
                &self.sessions_rows_list(scale),
            )
            .y;
        }
        if let Some(last) = rows.len().checked_sub(1) {
            let rect = workspace::sidebar_row_rect(
                rows,
                last,
                &self.workspaces,
                scale,
                &self.sessions_rows_list(scale),
            );
            return rect.y + rect.h;
        }
        // Empty list: just below its header.
        let list = self.sessions_rows_list(scale);
        list.y + (workspace::SESSIONS_HEADER_H * scale).round() + (6.0 * scale).round()
    }

    /// Thin horizontal insertion-line rect centered on gap `y`, spanning the
    /// sidebar at the least-indented row edge (symmetric margins).
    fn sidebar_insert_line(
        &self,
        y: f32,
        rows: &[workspace::SidebarRow],
        scale: f32,
        line_h: f32,
    ) -> workspace::LayoutRect {
        let (_, h) = self.renderer.surface_size();
        let side = workspace::sidebar(h, scale, self.sidebar_w());
        let mut x = side.x + (8.0 * scale).round();
        for ri in 0..rows.len() {
            let r = workspace::sidebar_row_rect(
                rows,
                ri,
                &self.workspaces,
                scale,
                &self.sessions_rows_list(scale),
            );
            x = if ri == 0 { r.x } else { x.min(r.x) };
        }
        let w = side.w - (x - side.x) * 2.0;
        workspace::LayoutRect { x, y: y - line_h / 2.0, w: w.max(0.0), h: line_h }
    }

    fn apply_drop(&mut self, src_tile: u64, src_tab: usize, target: DropTarget) {
        let src_len = self.workspaces[self.active]
            .root
            .find_tile(src_tile)
            .map_or(0, |t| t.tabs.len());
        match target {
            DropTarget::Center { tile } if tile == src_tile => return,
            DropTarget::Edge { tile, .. } if tile == src_tile && src_len <= 1 => return,
            DropTarget::Group { ws } if ws == self.active && src_len <= 1 => {
                let ws_ref = &self.workspaces[self.active];
                if ws_ref.root.tiles().len() == 1 {
                    return;
                }
            },
            // Terminal-tab drops only land on tile/group targets.
            DropTarget::SidebarInsert { .. } | DropTarget::SidebarAppend { .. } => return,
            _ => {},
        }

        let Some(mut tab) = self.take_tab(self.active, src_tile, src_tab) else {
            return;
        };

        match target {
            DropTarget::TabBar { tile, index } => {
                if let Some(t) = self.workspaces[self.active].root.find_tile_mut(tile) {
                    // Same-tile reorder: the source tab is already removed, so
                    // gaps past it shift down one to land where the line showed.
                    let index = if tile == src_tile && index > src_tab { index - 1 } else { index };
                    let index = index.min(t.tabs.len());
                    t.tabs.insert(index, tab);
                    t.active = index;
                    self.workspaces[self.active].focused_tile = tile;
                }
            },
            DropTarget::Center { tile } => {
                if let Some(t) = self.workspaces[self.active].root.find_tile_mut(tile) {
                    t.tabs.push(tab);
                    t.active = t.tabs.len() - 1;
                    self.workspaces[self.active].focused_tile = tile;
                }
            },
            DropTarget::Edge { tile, dir, first } => {
                let id = self.next_tile_id;
                self.next_tile_id += 1;
                tab.cols = 0;
                let new_tile =
                    Tile { id, tabs: vec![tab], active: 0, collapsed: false, collapse_anim: 0.0 };
                let ws = &mut self.workspaces[self.active];
                if ws.root.split_tile(tile, dir, &mut Some(new_tile), first) {
                    ws.focused_tile = id;
                }
            },
            DropTarget::Group { ws } => {
                if let Some(w) = self.workspaces.get_mut(ws) {
                    if let Some(t) = w.focused_mut() {
                        t.tabs.push(tab);
                        t.active = t.tabs.len() - 1;
                    }
                }
            },
            _ => {},
        }
        self.sync_layout();
        self.mark_visible_read();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Apply a sidebar-group drag landing on `target`, keeping `active` pinned
    /// to the same workspace identity and preserving section contiguity.
    fn apply_sidebar_group_drop(&mut self, from: usize, target: DropTarget) {
        if from >= self.workspaces.len() {
            return;
        }
        let new_idx = match target {
            DropTarget::SidebarInsert { before, section, pinned, .. } => {
                let idx =
                    workspace::relocate_workspace(&mut self.workspaces, from, before, section);
                if let Some(pinned) = pinned {
                    self.workspaces[idx].pinned = pinned;
                }
                idx
            },
            DropTarget::SidebarAppend { section_id } => {
                workspace::append_to_section(&mut self.workspaces, from, section_id)
            },
            _ => return,
        };
        self.active =
            workspace::track_index_after_relocate(self.active, from, new_idx);
        // A section a group leaves stays put even when now empty; sections are
        // removed only by the explicit delete-section button.
        workspace::ensure_active_section_expanded(
            &self.workspaces,
            &mut self.sections,
            self.active,
        );
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// ⌘-click: open the link under the cursor, if any.
    fn open_link_at(&self, px: f32, py: f32) -> bool {
        let scale = self.renderer.scale;
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        for (id, rect) in &tiles {
            let content = workspace::tile_content(rect, scale);
            if !content.contains(px, py) {
                continue;
            }
            if let Some((col, row)) = self.renderer.cell_at(&content, px, py)
                && let Some(tab) = ws.root.find_tile(*id).and_then(|t| t.active_tab())
                && let Some(url) = tab.session().and_then(|session| session.link_at(col, row))
            {
                open_in_browser(&url);
                return true;
            }
        }
        false
    }

    // ── Pointer events (from the terminal Element) ──────────────────────────

    // ── Mouse-report forwarding (mouse-tracking TUIs) ────────────────────
    //
    // When a TUI enables xterm mouse tracking, clicks and drags belong to the
    // app, not pwrde's text selection. These helpers mirror `forward_wheel`:
    // the press gate decides whether a pane owns the click, and the grab is
    // tracked so motion/release reach the same pane after the cursor leaves
    // it. Holding Shift always forces local selection (the xterm convention
    // for overriding an app's mouse grab).

    /// The session backing a mouse-report location, if it still exists.
    fn loc_session(&self, loc: MouseLoc) -> Option<&Session> {
        match loc {
            MouseLoc::Flyover => self.flyover_tabs.get(self.flyover_active).and_then(Tab::session),
            MouseLoc::Tool => self.active_tool_session().and_then(|ts| ts.tab.session()),
            MouseLoc::Tile(id) => self.workspaces[self.active]
                .root
                .find_tile(id)
                .and_then(|t| t.active_tab())
                .and_then(Tab::session),
        }
    }

    /// True when a click on `loc` should be forwarded to the app as a mouse
    /// report rather than starting a text selection: the pane's app has
    /// grabbed the mouse and Shift isn't held to override it.
    fn pane_grabs_mouse(&self, loc: MouseLoc) -> bool {
        !self.modifiers.shift && self.loc_session(loc).is_some_and(|s| s.app_grabs_mouse())
    }

    /// Forward one button event for `loc` as a mouse report, mapping the
    /// pointer to a cell in that pane's content rect. Motion and release clamp
    /// to the pane's origin when the pointer has slid off it (as `forward_wheel`
    /// does), so a drag that leaves the pane still reports.
    fn forward_mouse_report(&mut self, loc: MouseLoc, phase: MousePhase, btn: MouseBtn, px: f32, py: f32) {
        let scale = self.renderer.scale;
        let content = match loc {
            MouseLoc::Flyover => workspace::flyover_content(&self.flyover_rect_now(), scale),
            MouseLoc::Tool => workspace::tile_content(&self.tool_area(), scale),
            MouseLoc::Tile(id) => match self.tile_rect(id) {
                Some(rect) => workspace::tile_content(&rect, scale),
                None => return,
            },
        };
        let (col, row) = self.renderer.cell_at(&content, px, py).unwrap_or((0, 0));
        let m = self.modifiers;
        if let Some(session) = self.loc_session(loc) {
            session.forward_mouse(phase, btn, col, row, m.shift, m.alt, m.control);
        }
        self.request_redraw();
    }

    /// Record a forwarded button press: remember the pane and mark the button
    /// held. A press over a different pane than an in-flight grab retargets to
    /// the new one (the old pane already saw its press; cross-pane chording is
    /// not a real workflow).
    fn press_mouse_report(&mut self, loc: MouseLoc, btn: MouseBtn) {
        let bit = MouseReport::bit(btn);
        match &mut self.mouse_report {
            Some(r) if r.loc == loc => r.buttons |= bit,
            _ => self.mouse_report = Some(MouseReport { loc, buttons: bit }),
        }
    }

    /// Clear a forwarded button on release, returning the pane its press went
    /// to (so the release can be forwarded there) when the button was actually
    /// held. The grab ends once the last held button lifts.
    fn release_mouse_report(&mut self, btn: MouseBtn) -> Option<MouseLoc> {
        let bit = MouseReport::bit(btn);
        let r = self.mouse_report.as_mut()?;
        if r.buttons & bit == 0 {
            return None;
        }
        let loc = r.loc;
        r.buttons &= !bit;
        if r.buttons == 0 {
            self.mouse_report = None;
        }
        Some(loc)
    }

    /// Route a button press over a pane: if the pane's app grabs the mouse,
    /// forward the press as a mouse report, record the grab, and report `true`
    /// so the caller skips its normal (selection / context-menu) handling.
    fn try_forward_press(&mut self, loc: MouseLoc, btn: MouseBtn, px: f32, py: f32) -> bool {
        if !self.pane_grabs_mouse(loc) {
            return false;
        }
        self.forward_mouse_report(loc, MousePhase::Press, btn, px, py);
        self.press_mouse_report(loc, btn);
        true
    }

    /// Forward a button release to the pane that got its press, if that button
    /// was held. Returns `true` when the release was consumed as a report.
    fn try_forward_release(&mut self, btn: MouseBtn) -> bool {
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        match self.release_mouse_report(btn) {
            Some(loc) => {
                self.forward_mouse_report(loc, MousePhase::Release, btn, px, py);
                true
            },
            None => false,
        }
    }

    /// The terminal pane whose *content* is under (px, py): the flyover panel
    /// when it's open and hit, else a tile on the Sessions page. Right- and
    /// middle-button presses use this to find their target, since they don't
    /// run the left handler's tile loop.
    fn pane_at(&self, px: f32, py: f32) -> Option<MouseLoc> {
        let scale = self.renderer.scale;
        if self.flyover_open
            && !self.flyover_tabs.is_empty()
            && workspace::flyover_content(&self.flyover_rect_now(), scale).contains(px, py)
        {
            return Some(MouseLoc::Flyover);
        }
        if let Page::Tool(_) = self.page {
            return workspace::tile_content(&self.tool_area(), scale)
                .contains(px, py)
                .then_some(MouseLoc::Tool);
        }
        if self.page != Page::Sessions {
            return None;
        }
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        tiles
            .iter()
            .find(|(_, r)| workspace::tile_content(r, scale).contains(px, py))
            .map(|(id, _)| MouseLoc::Tile(*id))
    }

    /// True while a modal overlay is intercepting input (the same set the left
    /// mouse-down handler treats as modal), so mouse reports must not fire
    /// underneath it.
    fn modal_overlay_open(&self) -> bool {
        self.confirm.is_some()
            || self.message.is_some()
            || self.save_ws.is_some()
            || self.webview_prompt.is_some()
            || self.command.is_some()
    }

    /// Right/middle button press: forward it to a mouse-tracking pane under the
    /// cursor, if any. Returns `true` when consumed as a report.
    fn try_forward_secondary_press(&mut self, btn: MouseBtn) -> bool {
        if self.modal_overlay_open() {
            return false;
        }
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        match self.pane_at(px, py) {
            Some(loc) if self.try_forward_press(loc, btn, px, py) => {
                self.request_redraw();
                true
            },
            _ => false,
        }
    }

    fn on_middle_mouse_down(&mut self) {
        self.try_forward_secondary_press(MouseBtn::Middle);
    }

    fn on_mouse_down(&mut self, window: &mut Window, click_count: usize, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (_, h) = self.renderer.surface_size();

        // A fresh click sequence forgets which pane the previous one expanded.
        if click_count <= 1 {
            self.just_expanded = None;
        }

        // Overlays are modal: they intercept clicks in priority order
        // (confirm → message → fork picker → dir picker) before anything else.
        if self.confirm.is_some()
            || self.message.is_some()
            || self.save_ws.is_some()
            || self.webview_prompt.is_some()
            || self.command.is_some()
        {
            // Every modal is an element tree now (modal_ui / save_ui /
            // palette_ui / picker_ui): its occluding scrim keeps the click
            // off the canvas, so there is nothing to resolve here.
            return;
        }

        // A click anywhere while a section rename is open commits it, so the
        // editor never lingers and silently swallows terminal keystrokes.
        if self.editing_section.is_some() {
            self.commit_section_rename();
        }

        // Flyover panel hit-testing: after modal-overlay check, before sidebar/tiles.
        if self.flyover_open && !self.flyover_tabs.is_empty() {
            let panel = self.flyover_rect_now();
            // The top-edge height-resize grab is an element handle now
            // (`resize_ui`), which arms `Drag::FlyoverResize` itself.
            if panel.contains(px, py) {
                let tab_bar = workspace::flyover_tab_bar(&panel, scale);
                let n = self.flyover_tabs.len();
                if tab_bar.contains(px, py) && n > 0 {
                    // Window buttons at the bar's right edge.
                    if workspace::flyover_minimize_rect(&panel, scale).contains(px, py) {
                        self.toggle_flyover();
                        return;
                    }
                    if workspace::flyover_maximize_rect(&panel, scale).contains(px, py) {
                        self.flyover_toggle_maximized();
                        return;
                    }
                    // The tabs and window buttons are element click targets
                    // (`flyover_ui`) that stop the press first; the bar's
                    // empty run still selects the nearest tab from here.
                    let maxed = self.flyover_maximized;
                    let tab_rect = workspace::flyover_tab_rect(&panel, 0, n.max(1), scale, maxed);
                    let ti = ((((px - tab_rect.x).max(0.0)) / tab_rect.w).floor() as usize)
                        .min(n.saturating_sub(1));
                    let close =
                        workspace::flyover_tab_close_rect(&panel, ti, n, scale, maxed).contains(px, py);
                    self.press_flyover_tab(ti, close);
                } else {
                    // Click in content area: focus the panel, then either
                    // forward the click to a mouse-tracking TUI or start a
                    // text selection.
                    self.flyover_focused = true;
                    if self.try_forward_press(MouseLoc::Flyover, MouseBtn::Left, px, py) {
                        self.request_redraw();
                        return;
                    }
                    let content = workspace::flyover_content(&panel, scale);
                    if let Some((col, row)) = self.renderer.cell_at(&content, px, py) {
                        if let Some(session) = self
                            .flyover_tabs
                            .get(self.flyover_active)
                            .and_then(Tab::session)
                        {
                            session.begin_selection(col, row);
                        }
                        self.drag = Drag::FlyoverSelect;
                    }
                }
                self.request_redraw();
                return;
            } else {
                // Clicked outside panel: move focus to tile without closing panel.
                self.flyover_focused = false;
                // Don't return — let tile hit-testing proceed below.
            }
        }

        let sidebar = workspace::sidebar(h, scale, self.sidebar_w());

        // ⌘-click opens links instead of focusing (Sessions only — the
        // terminal grids aren't visible on other pages).
        if self.page == Page::Sessions && self.modifiers.platform && self.open_link_at(px, py) {
            return;
        }

        // The sidebar edge, the tile dividers and the flyover edge are
        // element handles now (`resize_ui`): they arm their drags and stop
        // the press, so none of them reach here.

        // Window drags (the titlebar strip, or the traffic-light corner
        // while the sidebar is folded) start from an element now
        // (`sidebar_ui::render_window_drag_zones`), which stops the press,
        // so nothing reaches here from those regions.

        // Empty state (Sessions only): the centered CTA is an element-tree
        // button now (`sidebar_ui::render_empty_state`), so the content area
        // has nothing for the canvas to resolve — and the placeholder tile
        // must not arm tab drags. The Settings page keeps its own hit-testing.
        if self.page == Page::Sessions && self.is_empty_state() && !sidebar.contains(px, py) {
            return;
        }


        // Sidebar: titlebar strip = traffic lights + window drag handle.
        if sidebar.contains(px, py) {
            // The header chips (show folders / focus terminals / "＋" /
            // gear) and the whole folders card are element click targets
            // that occlude the canvas (`sidebar_ui::sessions_header`,
            // `folders_ui`), so a press that reaches here is never on one of
            // them.
            if self.page == Page::Settings {
                // The search field and the section rows are element click
                // targets now (`sidebar_ui::settings_row_layer`), occluding
                // the canvas; a click anywhere else in the sidebar only blurs
                // the search field, as it always did.
                self.blur_settings_search(window, cx);
                return;
            }
            // Every other sidebar row — the session rows (pinned or not) and
            // the folder rows with their delete chip — is an element click
            // target now (`sidebar_ui`, `folders_ui`),
            // which arms the same presses this branch used to; a press that
            // reaches here landed between rows.
            return;
        }

        // Settings page: the gpui overlay owns all content-area clicks.
        if self.page == Page::Settings {
            return;
        }
        // Tool page: the whole content area is one terminal — forward the
        // click to a mouse-tracking TUI or start a text selection, exactly
        // like a click in the flyover's content.
        if let Page::Tool(_) = self.page {
            let content = workspace::tile_content(&self.tool_area(), scale);
            if !content.contains(px, py) {
                return;
            }
            if self.try_forward_press(MouseLoc::Tool, MouseBtn::Left, px, py) {
                self.request_redraw();
                return;
            }
            if let Some((col, row)) = self.renderer.cell_at(&content, px, py)
                && let Some(ts) = self.active_tool_session()
                && let Some(session) = ts.tab.session()
            {
                session.begin_selection(col, row);
                self.drag = Drag::ToolSelect;
            }
            self.request_redraw();
            return;
        }
        let area = self.area();
        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        let axes = workspace::tile_collapse_axis(&ws.root);

        // Tiles: caret/collapse handling, tab strip press (activate + arm
        // drag), or content focus.
        for (id, rect) in &tiles {
            if !rect.contains(px, py) {
                continue;
            }
            let strip = workspace::tab_strip_rect(area, rect, scale, self.sidebar_w());
            let axis = axes.iter().find(|(tid, _)| tid == id).and_then(|(_, a)| *a);
            let ws = &mut self.workspaces[self.active];
            // A sideways-collapsed strip has no usable tab bar: any click
            // expands and focuses it.
            if axis == Some(Dir::Row)
                && ws.root.find_tile(*id).is_some_and(|t| t.collapsed)
            {
                // Element-owned too (`tile_ui`); kept for a press that slips
                // past the element (it never should).
                self.press_tile_expand(*id);
                return;
            }
            let bar = workspace::tile_tab_bar(&strip, scale);
            if bar.contains(px, py) {
                let has_caret = axis.is_some();
                if has_caret && workspace::tile_caret_rect(rect, scale).contains(px, py) {
                    // Element-owned too (`tile_ui`).
                    self.press_tile_caret(*id, click_count);
                    return;
                }
                // The tabs themselves are element click targets (`tile_ui`)
                // that stop the press before it reaches here; what still
                // lands on the canvas is the bar's empty run past the last
                // tab, which selects the nearest tab as it always did.
                if let Some(tile) = ws.root.find_tile(*id) {
                    let n = tile.tabs.len();
                    let t0 = workspace::tile_tab_rect(&strip, 0, n.max(1), scale, has_caret);
                    let ti =
                        ((((px - t0.x).max(0.0)) / t0.w).floor() as usize).min(n.saturating_sub(1));
                    let close = n > 0
                        && workspace::tile_tab_close_rect(&strip, ti, n, scale, has_caret)
                            .contains(px, py);
                    self.press_tile_tab(*id, ti, close, click_count);
                }
            } else {
                ws.focused_tile = *id;
                // A mouse-tracking TUI owns the click: forward it as a report
                // instead of starting a text selection.
                if self.try_forward_press(MouseLoc::Tile(*id), MouseBtn::Left, px, py) {
                    self.mark_visible_read();
                    self.request_redraw();
                    return;
                }
                let content = workspace::tile_content(rect, scale);
                if let Some((col, row)) = self.renderer.cell_at(&content, px, py) {
                    if let Some(tab) =
                        self.workspaces[self.active].root.find_tile(*id).and_then(|t| t.active_tab())
                        && let Some(session) = tab.session()
                    {
                        session.begin_selection(col, row);
                        self.drag = Drag::Select { tile: *id };
                    }
                }
                self.mark_visible_read();
                self.request_redraw();
            }
            return;
        }
    }

    fn on_mouse_move(&mut self, window: &mut Window) {
        let _ = window;
        let scale = self.renderer.scale;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);

        // A mouse-tracking TUI holding a button gets drag-motion reports; the
        // held button is carried in `mouse_report`, so no local drag runs.
        if let Some(report) = self.mouse_report {
            for btn in [MouseBtn::Left, MouseBtn::Middle, MouseBtn::Right] {
                if report.buttons & MouseReport::bit(btn) != 0 {
                    self.forward_mouse_report(report.loc, MousePhase::Move, btn, px, py);
                }
            }
            return;
        }

        match &self.drag {
            Drag::Sidebar => {
                // The grab band rides the region's right edge; the pointer x
                // is the region width, so take the folders column back off
                // before it becomes the sessions-list width.
                self.sidebar_expanded_w =
                    workspace::sessions_w_for_pointer(px / scale, self.folders_w, self.folders_open);
                self.sync_layout();
                self.request_redraw();
            },
            Drag::Folders => {
                self.folders_w = workspace::folders_w_for_pointer(px / scale);
                self.sync_layout();
                self.request_redraw();
            },
            Drag::Divider { path } => {
                let path = path.clone();
                let area = self.area();
                let ws = &mut self.workspaces[self.active];
                let rect = workspace::rect_at_path(&ws.root, area, &path, scale);
                if let Some(Node::Split { dir, ratio, .. }) = ws.root.node_at_path_mut(&path) {
                    let r = match dir {
                        Dir::Row => (px - rect.x) / rect.w,
                        Dir::Column => (py - rect.y) / rect.h,
                    };
                    *ratio = r.clamp(0.1, 0.9);
                }
                self.sync_layout();
                self.request_redraw();
            },
            Drag::TabPress { tile, tab, start } => {
                let (sx, sy) = *start;
                if (self.cursor.0 - sx).abs() + (self.cursor.1 - sy).abs() > DRAG_THRESHOLD {
                    self.drag = Drag::Tab { tile: *tile, tab: *tab };
                    self.request_redraw();
                }
            },
            Drag::Tab { .. } => self.request_redraw(),
            Drag::GroupPress { ws, start } => {
                let (sx, sy) = *start;
                if (self.cursor.0 - sx).abs() + (self.cursor.1 - sy).abs() > DRAG_THRESHOLD {
                    self.drag = Drag::Group { ws: *ws };
                    self.request_redraw();
                }
            },
            Drag::Group { .. } => self.request_redraw(),
            Drag::FolderPress { si, start } => {
                let (sx, sy) = *start;
                if (self.cursor.0 - sx).abs() + (self.cursor.1 - sy).abs() > DRAG_THRESHOLD {
                    self.drag = Drag::Folder { si: *si };
                    self.request_redraw();
                }
            },
            Drag::Folder { .. } => self.request_redraw(),
            Drag::FlyoverResize => {
                // Top edge follows the pointer; PTYs resize on release.
                let (_, h) = self.renderer.surface_size();
                let frac = ((h as f32 - self.cursor.1 as f32) / h as f32)
                    .clamp(workspace::FLYOVER_MIN_FRAC, workspace::FLYOVER_MAX_FRAC);
                if frac != self.flyover_height_frac {
                    self.flyover_height_frac = frac;
                    self.request_redraw();
                }
            },
            Drag::FlyoverSelect => {
                let scale = self.renderer.scale;
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                let panel = self.flyover_rect_now();
                let content = workspace::flyover_content(&panel, scale);
                if let Some((col, row)) = self.renderer.cell_at(&content, px, py) {
                    if let Some(session) = self
                        .flyover_tabs
                        .get(self.flyover_active)
                        .and_then(Tab::session)
                    {
                        session.update_selection(col, row);
                        self.request_redraw();
                    }
                }
            },
            Drag::ToolSelect => {
                let scale = self.renderer.scale;
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                let content = workspace::tile_content(&self.tool_area(), scale);
                if let Some((col, row)) = self.renderer.cell_at(&content, px, py)
                    && let Some(ts) = self.active_tool_session()
                    && let Some(session) = ts.tab.session()
                {
                    session.update_selection(col, row);
                    self.request_redraw();
                }
            },
            Drag::Select { tile } => {
                let tile = *tile;
                let area = self.area();
                let scale = self.renderer.scale;
                if let Some(rect) = self.tile_rect(tile) {
                    let content = workspace::tile_content(&rect, scale);
                    if let Some((col, row)) = self.renderer.cell_at(&content, px, py)
                        && let Some(tab) = self.workspaces[self.active]
                            .root
                            .find_tile(tile)
                            .and_then(|t| t.active_tab())
                        && let Some(session) = tab.session()
                    {
                        session.update_selection(col, row);
                        self.request_redraw();
                    }
                }
                let _ = area;
            },
            Drag::None => {
                // Resize-handle hover: suppress while any overlay is open so the
                // cursor/highlight don't fight the modal. Hit-test matches
                // on_mouse_down exactly via workspace::resize_hover_at.
                let hover = if self.confirm.is_some()
                    || self.message.is_some()
                    || self.webview_prompt.is_some()
                    || self.command.is_some()
                {
                    None
                } else if self.flyover_open
                    && !self.flyover_windowed
                    && !self.flyover_tabs.is_empty()
                    && self.flyover_rect_now().contains(px, py)
                {
                    // The flyover overlays the sidebar edge and the tile
                    // dividers — no resize affordance underneath it.
                    None
                } else {
                    let (_, h) = self.renderer.surface_size();
                    let sidebar_edge_x = workspace::sidebar(h, scale, self.sidebar_w()).w;
                    let grab = GRAB * scale;
                    let dividers_active =
                        self.page == Page::Sessions && !self.is_empty_state();
                    let ws = &self.workspaces[self.active];
                    let folders_edge_x = if self.sidebar_collapsed {
                        None
                    } else {
                        workspace::folders_edge_x(self.folders_w, self.folders_visible())
                            .map(|x| x * scale)
                    };
                    workspace::resize_hover_at(
                        &ws.root,
                        self.area(),
                        scale,
                        sidebar_edge_x,
                        folders_edge_x,
                        grab,
                        dividers_active,
                        px,
                        py,
                    )
                };
                if hover != self.resize_hover {
                    self.resize_hover = hover;
                    self.request_redraw();
                }
                // Link hover: suppress when any overlay is open or not in Sessions page.
                let link_hover = if self.page != Page::Sessions
                    || self.confirm.is_some()
                    || self.message.is_some()
                    || self.webview_prompt.is_some()
                    || self.command.is_some()
                {
                    None
                } else {
                    let scale = self.renderer.scale;
                    let ws = &self.workspaces[self.active];
                    let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
                    let mut found = None;
                    for (id, rect) in &tiles {
                        let content = workspace::tile_content(rect, scale);
                        if !content.contains(px, py) {
                            continue;
                        }
                        if let Some((col, row)) = self.renderer.cell_at(&content, px, py)
                            && let Some(tab) =
                                ws.root.find_tile(*id).and_then(|t| t.active_tab())
                            && tab.session().is_some_and(|session| session.link_at(col, row).is_some())
                        {
                            found = Some((*id, col, row));
                        }
                        break;
                    }
                    found
                };
                if link_hover != self.link_hover {
                    self.link_hover = link_hover;
                    self.request_redraw();
                }
                // UI-element hover: iterate hot rects in reverse (topmost wins).
                let ui_hover = self.hot_rects.iter().enumerate().rev()
                    .find(|(_, r)| r.contains(px, py))
                    .map(|(i, _)| i);
                if ui_hover != self.ui_hover {
                    self.ui_hover = ui_hover;
                    self.request_redraw();
                }
            },
        }
    }

    fn on_mouse_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (window, cx);
        // A left-button release held by a mouse-tracking TUI is a report, not
        // the end of a drag — forward it and skip the drag machinery.
        if self.try_forward_release(MouseBtn::Left) {
            return;
        }
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Tab { tile, tab } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(target) = self.resolve_drop(px, py, tile) {
                    self.apply_drop(tile, tab, target);
                }
                self.request_redraw();
            },
            // Resize released: fit the PTYs to the final height and keep it
            // for future launches.
            Drag::FlyoverResize => {
                settings::set(
                    "flyover.height",
                    format!("{:.3}", self.flyover_height_frac).into(),
                );
                self.sync_flyover_layout(true);
                self.request_redraw();
            },
            // Sidebar group click (no drag): activate the workspace.
            Drag::GroupPress { ws, .. } => {
                if let Page::Tool(_) = self.page {
                    self.set_page(Page::Sessions);
                }
                self.switch_workspace(ws);
            },
            // Sidebar group drag: resolve against the shared row list.
            Drag::Group { ws } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(target) = self.resolve_sidebar_group_drop(px, py) {
                    self.apply_sidebar_group_drop(ws, target);
                }
                self.request_redraw();
            },
            // Folder drag: re-order the folders card (persisted with the
            // sections' positions).
            Drag::Folder { si } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(DropTarget::FolderInsert { before, .. }) =
                    self.resolve_folder_drop(px, py)
                {
                    workspace::reorder_section(&mut self.sections, si, before);
                    self.persist_snapshot();
                }
                self.request_redraw();
            },
            _ => {},
        }
    }

    // ── Keyboard ────────────────────────────────────────────────────────

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.modifiers = ev.keystroke.modifiers;
        // cmd+` toggles the flyover panel from ANY state (overlays, pages, etc.).
        if ev.keystroke.modifiers.platform
            && pages::Action::ToggleFlyover.binding().matches(&ev.keystroke)
        {
            self.toggle_flyover();
            return;
        }
        // An open overlay owns the keyboard: route to it before ⌘ shortcuts or
        // the PTY so typing filters the list rather than reaching the shell.
        if self.confirm.is_some()
            || self.message.is_some()
            || self.save_ws.is_some()
            || self.webview_prompt.is_some()
            || self.command.is_some()
        {
            self.handle_picker_key(ev);
            return;
        }
        // Browser chrome fields own plain typing and their Enter/Escape
        // semantics; no keystroke from them should leak into a terminal.
        if self.webview_input_focused(window, cx) {
            self.handle_webview_input_key(ev, window, cx);
            return;
        }
        // Flyover panel: when open AND focused it owns the keyboard (except
        // the cmd+` toggle above and overlay keys above that).
        if self.flyover_open && self.flyover_focused {
            if ev.keystroke.modifiers.platform {
                // Flyover-scoped cmd shortcuts; the rest keep global meaning.
                if let Some(action) = pages::match_action(&ev.keystroke)
                    && self.flyover_shortcut(action)
                {
                    self.request_redraw();
                    return;
                }
                self.handle_shortcut(ev);
                self.request_redraw();
                return;
            }
            // Plain key: write bytes to the active flyover session.
            self.flyover_write_key(&ev.keystroke);
            self.request_redraw();
            return;
        }

        // A focused Flow composer owns the keyboard on every page (ahead of the
        // Settings/tool-page handlers, since Flow follows the user): the Textarea
        // edits itself and submits on ↩ (see `flow_ui`); ⎋ collapses the
        // panel and hands focus back; ⌘ chords still resolve so ⌘J closes.
        if self.flow_editor_focused(window, cx) {
            if ev.keystroke.modifiers.platform {
                self.handle_shortcut(ev);
            } else if ev.keystroke.key == "escape" {
                self.flow.open = false;
                window.focus(&self.focus_handle, cx);
            }
            self.request_redraw();
            return;
        }
        // Sidebar section rename captures typing; shortcuts stay muted.
        if self.editing_section.is_some() {
            self.handle_section_key(ev);
            return;
        }
        // The Settings page owns the keyboard: no PTY to type into.
        if self.page == Page::Settings {
            self.handle_settings_key(ev, window, cx);
            return;
        }
        // A tool page's terminal owns the keyboard; ⌘ shortcuts stay global.
        if let Page::Tool(_) = self.page {
            if ev.keystroke.modifiers.platform {
                self.handle_shortcut(ev);
            } else {
                self.tool_write_key(&ev.keystroke);
            }
            self.request_redraw();
            return;
        }
        // ⌘ shortcuts take priority over passing bytes to the shell.
        if ev.keystroke.modifiers.platform {
            self.handle_shortcut(ev);
            return;
        }
        if let Some(bytes) = key_to_bytes(&ev.keystroke) {
            let ws = &self.workspaces[self.active];
            if let Some(session) = ws
                .focused()
                .and_then(|tile| tile.active_tab())
                .and_then(Tab::session)
            {
                session.write(bytes);
                // Typing snaps back to the live bottom and drops any
                // selection, like every other terminal.
                session.scroll_to_bottom();
                session.clear_selection();
            }
        }
    }

    /// Route a keystroke to the active overlay in priority order (message →
    /// fork → dir picker): typing filters, arrows move the highlight, Enter
    /// confirms, Escape closes/steps back. Called only while an overlay is
    /// open, so it always consumes the key.
    fn handle_picker_key(&mut self, ev: &KeyDownEvent) {
        let key = ev.keystroke.key.as_str();
        // The confirm dialog is topmost: Enter accepts its pending action,
        // Escape cancels, everything else is swallowed.
        if self.confirm.is_some() {
            match key {
                "enter" => self.confirm_accept(),
                "escape" => self.confirm = None,
                _ => {},
            }
            self.request_redraw();
            return;
        }
        // A provisioning/error message overlay is topmost. A dismissable one
        // clears on any key; a non-dismissable one swallows the key while work
        // is in flight. Either way the key is consumed here.
        if let Some((_, dismissable)) = self.message.as_ref() {
            if *dismissable {
                self.message = None;
            }
            self.request_redraw();
            return;
        }
        if self.webview_prompt.is_some() {
            match key {
                "enter" => self.submit_new_webview_prompt(),
                "escape" => self.webview_prompt = None,
                _ => {},
            }
            self.request_redraw();
            return;
        }
        // The save-as-workspace modal owns typing for its two fields, then the
        // destination rows.
        if self.save_ws.is_some() {
            self.handle_save_key(ev);
            return;
        }
        // The unified command palette: navigation here, editing in the
        // shared Input (the root listener still fires while it is focused,
        // and never stops it).
        if let Some(pal) = self.command.as_ref() {
            let query_empty = pal.query().is_empty();
            if Action::CommandPalette.binding().matches(&ev.keystroke) {
                self.close_command();
            } else {
                match key {
                    "escape" => self.close_command(),
                    "enter" => self.command_enter(),
                    "up" | "down" => {
                        if let Some(c) = self.command.as_mut() {
                            c.move_selection(if key == "up" { -1 } else { 1 });
                            self.command_scroll_to = Some(c.selected());
                        }
                    },
                    "backspace" if query_empty => self.command_back(),
                    _ => {},
                }
            }
        }
        self.request_redraw();
    }

    /// Keyboard routing for the save-as-workspace modal. Field stage: typing
    /// edits the focused field, Tab toggles fields, Enter advances (Name →
    /// Description → destination rows), Esc cancels. Destination stage:
    /// arrows move, Enter saves, Esc steps back to the fields.
    fn handle_save_key(&mut self, ev: &KeyDownEvent) {
        let key = ev.keystroke.key.as_str();
        let mut close = false;
        let mut commit = false;
        if let Some(modal) = self.save_ws.as_mut() {
            if let Some(sel) = modal.dest_selected {
                match key {
                    "escape" => {
                        modal.dest_selected = None;
                        self.save_sync = true;
                    },
                    "up" => modal.dest_selected = Some(sel.saturating_sub(1)),
                    "down" => {
                        let last = modal.dest_labels.len().saturating_sub(1);
                        modal.dest_selected = Some((sel + 1).min(last));
                    },
                    "enter" => commit = true,
                    _ => {},
                }
            } else {
                // Editing keys (chars, backspace, selection, clipboard) belong
                // to the focused rcn Input; only navigation is handled here.
                match key {
                    "escape" => close = true,
                    "tab" => {
                        modal.field = (modal.field + 1) % 2;
                        self.save_sync = true;
                    },
                    "enter" => {
                        if modal.field == 0 {
                            modal.field = 1;
                        } else if modal.name.trim().is_empty() {
                            // A profile needs a name before it can move on.
                            modal.field = 0;
                        } else {
                            modal.dest_selected = Some(0);
                        }
                        self.save_sync = true;
                    },
                    _ => {},
                }
            }
        }
        if close {
            self.save_ws = None;
        }
        if commit {
            self.commit_save_workspace();
        }
        self.request_redraw();
    }

    /// Keyboard routing while a sidebar section header is being renamed.
    /// Enter commits via `apply_section_rename`, Esc cancels, Backspace pops,
    /// printable chars append. Sidebar shortcuts stay muted while editing.
    fn handle_section_key(&mut self, ev: &KeyDownEvent) {
        match ev.keystroke.key.as_str() {
            "escape" => {
                self.editing_section = None;
            },
            "enter" => {
                self.commit_section_rename();
            },
            "backspace" => {
                if let Some((_, buf)) = self.editing_section.as_mut() {
                    buf.pop();
                }
            },
            _ => {
                if !ev.keystroke.modifiers.control
                    && !ev.keystroke.modifiers.platform
                    && let Some(text) = ev.keystroke.key_char.as_deref()
                    && let Some((_, buf)) = self.editing_section.as_mut()
                {
                    for ch in text.chars().filter(|c| !c.is_control()) {
                        buf.push(ch);
                    }
                }
            },
        }
        self.request_redraw();
    }

    /// Commit the in-progress section rename, if any, parsing the buffer's
    /// leading emoji into the icon. No-op when no editor is open.
    fn commit_section_rename(&mut self) {
        if let Some((id, buf)) = self.editing_section.take() {
            if let Some(sec) = self.sections.iter_mut().find(|s| s.id == id) {
                workspace::apply_section_rename(sec, &buf);
            }
            self.persist_snapshot();
        }
    }

    /// Keyboard routing while the Settings page is up. A recording keyboard
    /// row captures the next ⌘ chord as its new binding; otherwise ⌘
    /// shortcuts still dispatch and plain typing is swallowed.
    /// Empty the Settings search field (entity and mirror together).
    pub(crate) fn clear_settings_search(&mut self, cx: &mut Context<Self>) {
        self.settings_query.clear();
        self.settings_search.update(cx, |input, cx| input.set_text("", cx));
    }

    /// Move keyboard focus off the Settings search field, if it has it.
    pub(crate) fn blur_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_search.read(cx).focus_handle(cx).is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
    }

    fn handle_settings_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A focused sidebar search box captures typing: chars filter, Enter
        // jumps to the first match's section, Escape cancels. It also wins
        // over a still-focused primary-command Input (clicking the search box
        // doesn't blur the Input's own focus handle), so typing can't land in
        // both.
        if self.settings_search.read(cx).focus_handle(cx).is_focused(window) {
            // The rcn Input owns editing (chars, backspace, selection,
            // clipboard); only the two commands are handled here.
            match ev.keystroke.key.as_str() {
                "escape" => {
                    self.clear_settings_search(cx);
                    window.focus(&self.focus_handle, cx);
                },
                "enter" => {
                    if let Some(entry) = pages::search_settings(&self.settings_query).first() {
                        self.section = entry.section;
                    }
                    self.clear_settings_search(cx);
                    window.focus(&self.focus_handle, cx);
                },
                _ => {},
            }
            self.request_redraw();
            return;
        }
        // Focused primary-command Input: enter saves, escape resets + blurs.
        // Character editing is handled by the Input entity itself; other keys
        // fall through to it (this handler doesn't stop propagation).
        if self.command_input.read(cx).focus_handle(cx).is_focused(window) {
            let input = self.command_input.clone();
            match ev.keystroke.key.as_str() {
                "enter" => {
                    let value = input.read(cx).text().trim().to_string();
                    settings::set("session.primary_command", value.into());
                    // Move focus back to the app handle (Input has no blur()).
                    window.focus(&self.focus_handle, cx);
                },
                "escape" => {
                    let reset = settings::primary_command();
                    input.update(cx, |input, cx| {
                        input.set_text(reset, cx);
                    });
                    window.focus(&self.focus_handle, cx);
                },
                _ => {},
            }
            self.request_redraw();
            return;
        }
        // Focused Tools add-form Input: enter adds, escape blurs; the Input
        // itself owns character editing.
        if self.tool_form.inputs().iter().any(|e| e.read(cx).focus_handle(cx).is_focused(window)) {
            match ev.keystroke.key.as_str() {
                "enter" => {
                    self.add_tool_from_form(cx);
                    window.focus(&self.focus_handle, cx);
                },
                "escape" => window.focus(&self.focus_handle, cx),
                _ => {},
            }
            self.request_redraw();
            return;
        }
        if let Some(action) = self.recording {
            if ev.keystroke.key == "escape" {
                self.recording = None;
            } else if let Some(binding) = Binding::from_keystroke(&ev.keystroke) {
                settings::set(&action.setting_key(), binding.serialize().into());
                self.recording = None;
            }
            self.request_redraw();
            return;
        }
        if ev.keystroke.modifiers.platform {
            self.handle_shortcut(ev);
        }
    }

    /// Dispatch a ⌘ chord through the rebindable-action table (settings-backed
    /// bindings with defaults). ⌘1–9 group switching stays fixed.
    fn handle_shortcut(&mut self, ev: &KeyDownEvent) {
        if let Some(action) = pages::match_action(&ev.keystroke) {
            self.run_action(action);
            self.request_redraw();
            return;
        }
        if self.page == Page::Sessions
            && let Some(d) = ev.keystroke.key.chars().next().and_then(|c| c.to_digit(10))
            && d >= 1
        {
            self.switch_workspace(d as usize - 1);
        }
        self.request_redraw();
    }

    /// Actions that work on every page. Returns whether `action` was one.
    fn run_global_action(&mut self, action: Action) -> bool {
        match action {
            Action::PrevPage => self.cycle_page(-1),
            Action::NextPage => self.cycle_page(1),
            Action::PrevSidebarTab => self.cycle_sidebar_tab(-1),
            Action::NextSidebarTab => self.cycle_sidebar_tab(1),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::ToggleFolders => self.toggle_folders(),
            Action::OpenSettings => self.set_page(Page::Settings),
            Action::Quit => {
                // Take the agent children down before the abrupt exit below;
                // each backend kills its CLI and unblocks its reader thread.
                for (_, mut backend) in self.flow_backends.drain() {
                    backend.shutdown();
                }
                std::process::exit(0);
            }
            Action::CommandPalette => self.toggle_command_root(),
            Action::IncreaseFontSize => self.zoom_font(renderer::FONT_SIZE_STEP),
            Action::DecreaseFontSize => self.zoom_font(-renderer::FONT_SIZE_STEP),
            Action::GoToSessions => self.set_page(Page::Sessions),
            Action::GoToTool => self.set_page(Page::Tool(0)),
            Action::ScreenshotToClipboard => self.screenshot_action(true),
            Action::ScreenshotToFile => self.screenshot_action(false),
            Action::NewSection => self.new_section_action(),
            Action::ToggleFlow => return self.toggle_flow(),
            _ => return false,
        }
        true
    }

    /// Run a rebindable action. Page navigation and quit work everywhere;
    /// terminal-layout actions only make sense on the Sessions page. Returns
    /// whether the action applied (false: wrong page or no open session).
    fn run_action(&mut self, action: Action) -> bool {
        if self.run_global_action(action) {
            return true;
        }
        // A tool page is one terminal: only the clipboard actions apply.
        if let Page::Tool(_) = self.page {
            return match action {
                Action::Copy => {
                    self.tool_copy();
                    true
                },
                Action::Paste => {
                    self.tool_paste();
                    true
                },
                _ => false,
            };
        }
        if self.page != Page::Sessions {
            return false;
        }
        // Empty state: terminal actions need a group, but a webview can fill
        // the placeholder tile directly because it does not require a cwd.
        if self.is_empty_state() {
            if action == Action::NewWebview {
                self.open_new_webview_prompt();
                return true;
            }
            if matches!(action, Action::NewTab | Action::NewGroup) {
                self.open_picker();
                return true;
            }
            return false;
        }
        match action {
            Action::SplitRight => self.split(Dir::Row),
            Action::SplitDown => self.split(Dir::Column),
            Action::NewTab => self.new_tab(),
            Action::NewWebview => self.open_new_webview_prompt(),
            Action::NewGroup => self.open_picker(),
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::CloseTab => self.close_active_tab(),
            Action::CloseGroup => self.close_focused_group(),
            Action::TogglePin => {
                if self.page == Page::Sessions {
                    if let Some(ws) = self.workspaces.get_mut(self.active) {
                        ws.pinned = !ws.pinned;
                        self.persist_snapshot();
                        self.request_redraw();
                    }
                }
            }
            Action::PrevTile => self.cycle_tile(-1),
            Action::NextTile => self.cycle_tile(1),
            Action::PrevTab => self.cycle_tab(-1),
            Action::NextTab => self.cycle_tab(1),
            Action::FocusLeft => self.focus_dir(workspace::NavDir::Left),
            Action::FocusDown => self.focus_dir(workspace::NavDir::Down),
            Action::FocusUp => self.focus_dir(workspace::NavDir::Up),
            Action::FocusRight => self.focus_dir(workspace::NavDir::Right),
            Action::ToggleCollapse => self.toggle_focused_collapse(),
            Action::SaveWorkspace => self.open_save_workspace(),
            Action::ToggleFocusOthers => self.toggle_focus_others(),
            // Handled earlier in `run_action` (page-agnostic), so they never
            // reach this Sessions-only match; listed to keep it exhaustive.
            Action::PrevSidebarTab
            | Action::NextSidebarTab
            | Action::ToggleSidebar
            | Action::ToggleFolders
            | Action::PrevPage
            | Action::NextPage
            | Action::OpenSettings
            | Action::Quit
            | Action::CommandPalette
            | Action::IncreaseFontSize
            | Action::DecreaseFontSize
            | Action::GoToSessions
            | Action::GoToTool
            | Action::ScreenshotToClipboard
            | Action::ScreenshotToFile
            | Action::NewSection
            | Action::ToggleFlow => {},
            Action::ToggleFlyover => self.toggle_flyover(),
            Action::FlyoverPopout => self.flyover_toggle_windowed(),
            // Open the active group's pull request in the browser; no-op (and
            // reported as such over the bus) when the group is known to have
            // no PR or isn't git-backed.
            Action::OpenPrInGithub => return self.open_pr_in_github(),
        }
        true
    }

    /// ⌘= / ⌘-: grow or shrink a font size. Context-aware — the terminal
    /// font when a terminal surface is focused (the Sessions page), the
    /// app/chrome font otherwise. The next paint detects the settings change,
    /// re-measures the cell, and reflows the PTYs.
    fn zoom_font(&mut self, delta: f32) {
        let key = if self.page == Page::Sessions {
            "terminal.font_size"
        } else {
            "appearance.font_size"
        };
        renderer::bump_font(key, delta);
        self.request_redraw();
    }

    /// ⌘S: collapse/expand the sidebar. Layout re-syncs so the PTYs pick up
    /// the reclaimed (or surrendered) width immediately.
    fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.sync_layout();
        self.request_redraw();
    }


    /// ⌘⇧←/→: step through `Page::all`, wrapping at both ends.
    fn cycle_page(&mut self, delta: isize) {
        let all = Page::all(self.n_tools());
        let cur = all.iter().position(|p| *p == self.page).unwrap_or(0);
        let i = pages::cycle(cur, all.len(), delta);
        self.set_page(all[i]);
    }

    /// ⌘⇧↑/↓: step through the sidebar's tabs, wrapping at both ends —
    /// groups on the Sessions page, sections on the Settings page.
    fn cycle_sidebar_tab(&mut self, delta: isize) {
        match self.page {
            Page::Sessions => {
                let i = pages::cycle(self.active, self.workspaces.len(), delta);
                self.switch_workspace(i);
            },
            Page::Settings => {
                let cur = Section::ALL.iter().position(|s| *s == self.section).unwrap_or(0);
                let i = pages::cycle(cur, Section::ALL.len(), delta);
                self.section = Section::ALL[i];
                self.request_redraw();
            },
            // Tool pages have no sidebar tabs of their own.
            Page::Tool(_) => {},
        }
    }

    fn set_page(&mut self, page: Page) {
        // A tool page whose tool was unregistered is gone too.
        if let Page::Tool(i) = page
            && i >= self.n_tools()
        {
            self.set_page(Page::Sessions);
            return;
        }
        if self.page != page {
            self.page = page;
            self.recording = None;
            self.settings_query.clear();
            self.settings_search_focus = false;
            // The sidebar cards are on screen again; top up whatever went
            // stale while another page was up.
            if self.card_rows() {
                self.spawn_git_context_refresh();
            }
            // Grids may have gone stale while the Settings page was up.
            if page == Page::Sessions {
                self.sync_layout();
                self.mark_visible_read();
            }
            // Entering a tool page launches its command (or relaunches one
            // that has since exited) and fits its PTY to the content area.
            if let Page::Tool(i) = page {
                self.ensure_tool_session(i);
                self.sync_tool_layout(true);
            }
        }
        self.request_redraw();
    }

    /// Refresh the per-group git/PR aggregates the sidebar cards read.
    ///
    /// `git_context::fetch` shells out to `git` and `gh`, so it must never be
    /// reachable from the paint path: every stale group is handed to a single
    /// background worker that walks them *serially* and posts each result back
    /// as a `TermEvent::GitContextReady`. The serial walk (guarded by
    /// `git_ctx_busy`) is the concurrency cap — N groups can never become N
    /// simultaneous `gh` calls, however often this is triggered. Groups
    /// without a `cwd` have no repo to describe and are skipped.
    fn spawn_git_context_refresh(&mut self) {
        use std::sync::atomic::Ordering;
        // Claim the worker slot in one atomic step, so the guard does not rest
        // on an unstated "only ever called from the foreground thread".
        if self
            .git_ctx_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // A walk is already in flight. Remember the trigger rather than
            // dropping it — `drain_events` re-fires it once the slot frees.
            self.git_ctx_pending = true;
            return;
        }
        self.git_ctx_pending = false;
        // Deduplicated: several groups can point at one checkout, and each
        // fetch is a fistful of shell-outs worth doing once.
        let mut stale: Vec<std::path::PathBuf> = Vec::new();
        for cwd in self.workspaces.iter().filter_map(|w| w.cwd.clone()) {
            if self.git_contexts.needs_refresh(&cwd) && !stale.contains(&cwd) {
                stale.push(cwd);
            }
        }
        if stale.is_empty() {
            // Nothing to walk, so hand the slot straight back.
            self.git_ctx_busy.store(false, Ordering::Release);
            return;
        }
        let tx = self.events_tx.clone();
        let busy = self.git_ctx_busy.clone();
        std::thread::spawn(move || {
            // Clear the flag on the way out however we leave, so a panic in a
            // shell-out can't wedge refreshes off for the rest of the session.
            struct Clear(std::sync::Arc<std::sync::atomic::AtomicBool>);
            impl Drop for Clear {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _clear = Clear(busy);

            for cwd in stale {
                let ctx = git_context::fetch(&cwd);
                if tx.send(TermEvent::GitContextReady { cwd, ctx }).is_err() {
                    break;
                }
            }
        });
    }

    /// Re-name every pane that still has no emulator title from its foreground
    /// process, off-thread. Modelled on `spawn_git_context_refresh`: the slot
    /// is claimed in one atomic step so the guard doesn't rest on an unstated
    /// "foreground thread only", and a `ps` sweep never runs on the render
    /// thread. Unlike the card walk there is no pending-trigger retry — the
    /// poll fires again in `PROC_TITLE_POLL` anyway. Returns whether a sweep
    /// was actually started, so the caller knows whether a `renew` tick was
    /// spent or swallowed.
    fn spawn_proc_title_refresh(&mut self, renew: bool) -> bool {
        use std::sync::atomic::Ordering;
        if self
            .proc_title_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        // Only panes the emulator hasn't named: a real OSC title always wins.
        // On a fast tick, only the ones with no name at all — the panes a user
        // is actually staring at after a restart; the rest wait for `renew`.
        let specs: Vec<(u64, Option<String>, Option<u32>)> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.root.tiles())
            .flat_map(|tile| tile.tabs.iter())
            .chain(self.flyover_tabs.iter())
            .filter_map(Tab::session)
            .filter(|session| {
                // A pane with neither a pid nor a shpool session can never
                // resolve (placeholder, or a spawn that failed), and would
                // otherwise keep the fast sweep alive for the life of the app.
                (session.child_pid.is_some() || session.shpool_session.is_some())
                    && session.needs_proc_title()
                    && (renew
                        || (!session.has_proc_title()
                            && session.proc_title_misses() < PROC_TITLE_MISS_LIMIT))
            })
            .map(|session| {
                (session.id, session.shpool_session.clone(), session.child_pid)
            })
            .collect();
        if specs.is_empty() {
            // Nothing to name, so hand the slot straight back. Still counts as
            // a sweep: there was no work, not a lost turn.
            self.proc_title_busy.store(false, Ordering::Release);
            return true;
        }
        let tx = self.events_tx.clone();
        let busy = self.proc_title_busy.clone();
        std::thread::spawn(move || {
            // Clear the flag however we leave, so a panic in a shell-out can't
            // wedge title refreshes off for the rest of the session.
            struct Clear(std::sync::Arc<std::sync::atomic::AtomicBool>);
            impl Drop for Clear {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _clear = Clear(busy);

            let titles = term::foreground_titles(&specs);
            let asked = specs.iter().map(|(id, _, _)| *id).collect();
            let _ = tx.send(TermEvent::ProcTitlesReady { asked, titles });
        });
        true
    }

    /// Drain PTY wakeups coalesced since the last frame; returns true if a
    /// redraw is needed.
    fn drain_events(&mut self) -> bool {
        let mut redraw = false;
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                TermEvent::Wakeup(id) => {
                    // First output from a fresh primary pane: the prompt is
                    // up, so the queued primary command can be typed now.
                    if let Some(cmd) = self.pending_primary_cmd.remove(&id)
                        && let Some(session) = self.find_session(id)
                    {
                        session.write(format!("{cmd}\r"));
                    }
                    if self.is_visible(id) {
                        let ws = &self.workspaces[self.active];
                        if let Some(session) = ws
                            .focused()
                            .and_then(|tile| tile.active_tab())
                            .and_then(Tab::session)
                            && session.id == id
                        {
                            let title = session.title();
                            if title != self.title {
                                self.title = title;
                            }
                        }
                        redraw = true;
                    }
                },
                TermEvent::Exit(id) => {
                    self.pending_primary_cmd.remove(&id);
                    self.remove_session(id);
                    redraw = true;
                },
                TermEvent::WebviewNavigated { id, url } => {
                    if crate::bus::validate_webview_url(&url).is_ok()
                        && self.set_webview_url(id, url)
                    {
                        self.persist_snapshot();
                        redraw = true;
                    }
                },
                TermEvent::WebviewFocused { id } => {
                    let tile = self.workspaces[self.active].root.tiles().iter().find_map(|tile| {
                        tile.active_tab()
                            .is_some_and(|tab| tab.webview_id() == Some(id))
                            .then_some(tile.id)
                    });
                    if let Some(tile) = tile
                        && self.workspaces[self.active].focused_tile != tile
                    {
                        self.workspaces[self.active].focused_tile = tile;
                        self.mark_visible_read();
                        redraw = true;
                    }
                },
                // The `ps` sweep came back: fill in the fallback names. Every
                // surface reads `Session::title()`, so a redraw is all it takes
                // for tab strips and sidebar cards to pick them up — and only
                // a name that actually moved is worth one, since the sweep
                // re-resolves the same name for as long as a pane stays
                // title-less.
                TermEvent::ProcTitlesReady { asked, titles } => {
                    for (id, title) in &titles {
                        if let Some(session) = self.find_session(*id)
                            && session.set_proc_title(Some(title.clone()))
                        {
                            redraw = true;
                        }
                    }
                    // Panes the sweep couldn't name: count the miss so a pane
                    // that can never resolve stops holding the fast tick open.
                    for id in asked.iter().filter(|id| !titles.iter().any(|(i, _)| i == *id)) {
                        if let Some(session) = self.find_session(*id) {
                            session.note_proc_title_miss();
                        }
                    }
                },
                // A backgrounded worktree drop finished: clear the provisioning
                // message and open the new group — with the profile chosen
                // before provisioning, when there was one — or surface the
                // failure (dropping any pending profile with it).
                TermEvent::GroupReady { name, cwd } => {
                    self.message = None;
                    match self.pending_group_profile.take() {
                        Some(profile) => self.add_group_with_profile(name, Some(cwd), &profile),
                        None => self.add_group(name, Some(cwd)),
                    }
                    let section = self.pending_group_section.take();
                    self.file_active_group(section);
                    redraw = true;
                },
                TermEvent::Bus { cmd, reply } => {
                    self.execute_bus(cmd, reply);
                    redraw = true;
                },
                TermEvent::GroupFailed { message } => {
                    self.pending_group_profile = None;
                    self.pending_group_section = None;
                    self.message = Some((format!("drop failed: {message}"), true));
                    redraw = true;
                },
                // A directory opened from outside the app (Finder, `open -a`, a
                // `pwrde://` deep link, or argv). macOS brings the app forward
                // on its own for these, so we just add the group.
                TermEvent::OpenDir { cwd } => {
                    let name = cwd
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
                    let found = pwrspace::discover(&pwrspace::candidate_paths(&cwd));
                    if found.is_empty() {
                        self.add_group(name, Some(cwd));
                    } else {
                        // Profiles exist for it: let the palette's layout
                        // step choose one.
                        let label = name.clone();
                        let entry = picker::PickerEntry::new(cwd, label);
                        self.command = Some(command::CommandPalette::for_directory(
                            entry,
                            picker::ProfilePicker::new(found),
                        ));
                        self.claim_command_search();
                    }
                    redraw = true;
                },
                // A pane signaled for attention (OSC 9, emitted by the Claude
                // Code hooks). On-screen tabs of the active group are being
                // watched, so only hidden tabs gain the unread dot — but a
                // watched pane still stamps its attention time, or the one
                // group the user is looking at would be the only card whose
                // timestamp never moves.
                TermEvent::Attention(id) => {
                    let watched = (self.page == Page::Sessions && self.is_visible(id))
                        || self.flyover_visible(id);
                    if watched {
                        if self.stamp_attention_by_session(id) {
                            redraw = true;
                        }
                    } else if self.set_unread_by_session(id) {
                        redraw = true;
                    }
                },
                TermEvent::PrCacheUpdated => {
                    // The PR cache moved under us, so every card rollup that
                    // quotes it is due for a re-fetch — but a wipe would blink
                    // every card back to its bare title until some unrelated
                    // trigger refilled it. Bump staleness instead, keeping the
                    // last-known-good values on screen (and available to
                    // `merge`) until the new ones land.
                    self.git_contexts.mark_all_stale();
                    self.spawn_git_context_refresh();
                },
                TermEvent::GitContextReady { cwd, ctx } => {
                    // Folded through the cache's `merge`, then repainted the
                    // same way every other event here does: `redraw` is what
                    // the caller turns into `cx.notify()`.
                    self.git_contexts.insert(&cwd, ctx);
                    redraw = true;
                },
                TermEvent::Flow { chat, ev } => {
                    self.flow.apply(chat, ev, crate::flow::now_epoch());
                    redraw = true;
                },
            }
        }
        // Advance pane collapse/expand animations the same way. PTY grids are
        // synced only when a pane settles so shells aren't resized mid-flight.
        let mut collapse_settled = false;
        for ws in &mut self.workspaces {
            for tile in ws.root.tiles_mut() {
                let target = if tile.collapsed { 1.0 } else { 0.0 };
                let p = tile.collapse_anim;
                let next =
                    if p < target { (p + 0.15).min(target) } else { (p - 0.15).max(target) };
                if next != p {
                    tile.collapse_anim = next;
                    redraw = true;
                    if next == target {
                        collapse_settled = true;
                    }
                }
            }
        }
        if collapse_settled {
            self.sync_layout();
        }
        // Advance flyover slide animation (±0.15 per tick, same cadence as
        // collapse_anim). PTY grids are only resized when the anim settles.
        let flyover_target = if self.flyover_open { 1.0_f32 } else { 0.0_f32 };
        let fa = self.flyover_anim;
        let fa_next = if fa < flyover_target {
            (fa + 0.15).min(flyover_target)
        } else {
            (fa - 0.15).max(flyover_target)
        };
        if fa_next != fa {
            self.flyover_anim = fa_next;
            redraw = true;
            if fa_next == flyover_target && self.flyover_open {
                // Anim settled at the open position — resize flyover PTYs now.
                // Forced, so cell-metric changes made while hidden still land.
                self.sync_flyover_layout(true);
            }
        }
        // Re-fire a git-context trigger that arrived while the serial worker
        // was mid-walk. This drain runs on the foreground executor every
        // ~16ms, so the retry costs one atomic until the slot frees and is
        // never dropped on the floor the way a plain early return would.
        if self.git_ctx_pending {
            self.spawn_git_context_refresh();
        }
        // Keep the cards honest while the user stays put: commits land and
        // dirty counts move inside a single group, and nothing else would
        // ever trigger a walk. Polling on this same foreground drain (no
        // extra thread, no timer per group) is what makes the cache's 5s
        // `FRESH_WINDOW` reachable at all — a tick with nothing stale hands
        // the worker slot straight back, so the idle cost is one atomic.
        if self.card_rows()
            && !self.sidebar_collapsed
            && self.git_ctx_polled_at.elapsed() >= GIT_CTX_POLL
        {
            self.git_ctx_polled_at = std::time::Instant::now();
            self.spawn_git_context_refresh();
        }
        // Name the panes the emulator hasn't named. Not gated on the sidebar
        // the way the card walk is: tab strips show these titles whether or
        // not any card is visible.
        if let Some(renew) = proc_title_due(
            self.proc_title_polled_at.elapsed(),
            self.proc_title_refreshed_at.elapsed(),
        ) {
            self.proc_title_polled_at = std::time::Instant::now();
            // Panes that already have a name only ride along on the slow tick,
            // so the steady state is a sweep every `PROC_TITLE_REFRESH`.
            // Only credit the slow tick once the sweep is actually running:
            // a `renew` swallowed by a still-busy slot would otherwise not be
            // retried for another `PROC_TITLE_REFRESH`.
            if self.spawn_proc_title_refresh(renew) && renew {
                self.proc_title_refreshed_at = std::time::Instant::now();
            }
        }
        redraw || self.dirty
    }

    fn remove_session(&mut self, id: u64) {
        // A tool page's command ended: keep its last frame up (⏎ relaunches).
        if let Some(ts) = self
            .tool_sessions
            .iter_mut()
            .flatten()
            .find(|ts| ts.tab.session().is_some_and(|session| session.id == id))
        {
            ts.exited = true;
            return;
        }
        // Flyover shells live outside the workspace tree: drop the tab and
        // close the panel when the last one goes.
        if let Some(ti) = self
            .flyover_tabs
            .iter()
            .position(|tab| tab.session().is_some_and(|session| session.id == id))
        {
            self.flyover_tabs.remove(ti);
            if self.flyover_tabs.is_empty() {
                self.flyover_open = false;
                self.flyover_focused = false;
                self.flyover_window_visible = false;
            } else {
                self.flyover_active = self.flyover_active.min(self.flyover_tabs.len() - 1);
            }
            return;
        }
        // Find and remove the tab whose session matches, cascading empties.
        for wi in 0..self.workspaces.len() {
            let tile_tab = {
                let ws = &self.workspaces[wi];
                ws.root.tiles().iter().find_map(|t| {
                    t.tabs
                        .iter()
                        .position(|tab| tab.session().is_some_and(|session| session.id == id))
                        .map(|ti| (t.id, ti))
                })
            };
            if let Some((tile_id, tab_idx)) = tile_tab {
                // The primary pane's shell exited: the whole group goes with
                // it. No confirmation — the process is already gone.
                let ws = &self.workspaces[wi];
                if ws.primary_tile == tile_id
                    && ws.root.find_tile(tile_id).is_some_and(|t| t.tabs.len() == 1)
                {
                    if self.confirm.as_ref().is_some_and(|c| {
                        matches!(c.action,
                            ConfirmAction::CloseGroup { primary_tile } if primary_tile == tile_id)
                    }) {
                        self.confirm = None;
                    }
                    self.close_group(wi);
                    return;
                }
                let _ = self.take_tab(wi, tile_id, tab_idx);
                // A sole emptied tile stays present but tab-less, so
                // `tiles().is_empty()` never fires — test for zero tabs.
                let group_empty = |ws: &Workspace| ws.root.tiles().iter().all(|t| t.tabs.is_empty());
                if group_empty(&self.workspaces[wi]) && self.workspaces.len() > 1 {
                    self.workspaces.remove(wi);
                    if self.active >= self.workspaces.len() {
                        self.active = self.workspaces.len().saturating_sub(1);
                    }
                } else if self.workspaces.len() == 1 && group_empty(&self.workspaces[0]) {
                    self.reset_empty_workspace(0);
                } else {
                    self.workspaces[wi].fix_focus();
                }
                self.sync_layout();
                self.request_redraw();
                self.persist_snapshot();
                return;
            }
        }
    }

    /// Call begin_frame() on every visible session to keep wakeup coalescing.
    fn begin_frame(&self) {
        for ws in &self.workspaces {
            for tile in ws.root.tiles() {
                if let Some(tab) = tile.active_tab() {
                    if let Some(session) = tab.session() {
                        session.begin_frame();
                    }
                }
            }
        }
        // Also call begin_frame on all flyover sessions every frame.
        for tab in &self.flyover_tabs {
            if let Some(session) = tab.session() {
                session.begin_frame();
            }
        }
        // And the tool pages' terminals — they coalesce wakeups the same
        // way, so a skipped reset would freeze a tool after its first paint.
        for ts in self.tool_sessions.iter().flatten() {
            if let Some(session) = ts.tab.session() {
                session.begin_frame();
            }
        }
    }
}

/// Abbreviate `path` with `~` for display.
pub(crate) fn tilde(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Snapshot a live split tree into a profile layout: dirs and ratios verbatim,
/// every tab captured with its best-effort running command.
fn capture_profile_node(node: &Node) -> pwrspace::ProfileNode {
    match node {
        Node::Leaf(tile) => pwrspace::ProfileNode::Leaf(pwrspace::ProfileLeaf {
            tabs: tile
                .tabs
                .iter()
                .map(|tab| match tab.url() {
                    Some(url) => pwrspace::ProfileTab {
                        kind: pwrspace::ProfileTabKind::Webview,
                        command: None,
                        url: Some(url.to_string()),
                    },
                    None => pwrspace::ProfileTab {
                        command: tab_command(tab),
                        ..Default::default()
                    },
                })
                .collect(),
            active: tile.active,
        }),
        Node::Split { dir, ratio, a, b } => pwrspace::ProfileNode::Split(pwrspace::ProfileSplit {
            split: match dir {
                Dir::Row => pwrspace::SplitDir::Row,
                Dir::Column => pwrspace::SplitDir::Column,
            },
            ratio: *ratio,
            a: Box::new(capture_profile_node(a)),
            b: Box::new(capture_profile_node(b)),
        }),
    }
}

/// Best-effort capture of the command running in a tab's pane. Plain panes
/// walk the shell child's process tree; shpool panes walk from the daemon-side
/// session shell instead (the pane's own child is just `shpool attach`).
/// `None` — including every failure — saves the tab as a bare shell.
fn tab_command(tab: &workspace::Tab) -> Option<String> {
    let session = tab.session()?;
    let cmd = if let Some(name) = &session.shpool_session {
        term::shpool_foreground_command(name)
    } else {
        session.child_pid.and_then(term::foreground_command)
    }?;
    let cmd = cmd.trim();
    if cmd.is_empty() { None } else { Some(cmd.to_string()) }
}

/// Build the fork picker's rows for a git repo: a default "new branch" row and
/// a repo-root row, followed by existing worktrees, then local and remote
/// branches.
fn build_fork_choices(repo: &std::path::Path) -> Vec<picker::ForkEntry> {
    use picker::{ForkEntry, ForkScope};
    let default = git::default_remote_branch(repo);
    let base_label = default.clone().unwrap_or_else(|| "HEAD".into());
    let branches = git::list_branches(repo);

    let mut out = vec![
        ForkEntry {
            label: format!("↪ new branch off default ({base_label})"),
            from: None,
            path: None,
            scope: ForkScope::Default,
        },
        ForkEntry {
            label: "⌂ repo root (no worktree)".into(),
            from: None,
            path: Some(repo.to_path_buf()),
            scope: ForkScope::RepoRoot,
        },
    ];
    // Existing worktrees (drop's and any others) — attach a group to one
    // instead of forking a new tree. The main working tree is the repo root,
    // already offered above, so skip it.
    for wt in git::list_worktrees(repo).into_iter().filter(|w| !w.is_main) {
        let location = wt
            .path
            .strip_prefix(repo)
            .unwrap_or(&wt.path)
            .to_string_lossy()
            .into_owned();
        let branch = wt.branch.as_deref().unwrap_or("detached");
        out.push(ForkEntry {
            label: format!("worktree  {branch}  ({location})"),
            from: None,
            path: Some(wt.path),
            scope: ForkScope::Worktree,
        });
    }
    for b in branches.iter().filter(|b| !b.is_remote) {
        let mut marks = Vec::new();
        if b.is_current {
            marks.push("current");
        }
        if b.is_default {
            marks.push("default");
        }
        let suffix = if marks.is_empty() { String::new() } else { format!("  ({})", marks.join(", ")) };
        out.push(ForkEntry {
            label: format!("local   {}{}", b.name, suffix),
            from: Some(b.name.clone()),
            path: None,
            scope: ForkScope::Local,
        });
    }
    for b in branches.iter().filter(|b| b.is_remote) {
        out.push(ForkEntry {
            label: format!("remote  {}", b.name),
            from: Some(b.name.clone()),
            path: None,
            scope: ForkScope::Remote,
        });
    }
    out
}

/// Provision a new worktree via `drop new`, returning its path. Runs
/// synchronously — callers spawn it on a background thread.
fn run_drop(repo: &std::path::Path, from: Option<&str>) -> Result<std::path::PathBuf, String> {
    let mut cmd = git::augmented_command("drop");
    cmd.arg("new").arg("--repo").arg(repo).arg("--print-path").arg("--yes");
    if let Some(from) = from {
        cmd.arg("--from").arg(from);
    }
    let output = cmd.output().map_err(|e| format!("could not run drop: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Err(if reason.is_empty() { "drop exited with an error".into() } else { reason.to_string() });
    }
    // With --print-path, stdout is just the worktree path.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if path.is_empty() {
        return Err("drop produced no worktree path".into());
    }
    Ok(std::path::PathBuf::from(path))
}

/// Convert accumulated fractional wheel travel into whole scroll steps,
/// carrying the sub-step remainder in `accum` so tiny deltas aren't lost.
fn scroll_steps(accum: &mut f64, notches: f64) -> isize {
    *accum += notches;
    let steps = accum.trunc() as isize;
    *accum -= steps as f64;
    steps
}

/// Map a gpui keystroke to the bytes a terminal expects, or `None` when the
/// key is not something we send to the PTY.
fn key_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    let key = ks.key.as_str();

    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4). Cmd never
    // reaches here (it's routed to app shortcuts in on_key_down).
    let mod_param = 1 + m.shift as u8 + ((m.alt as u8) << 1) + ((m.control as u8) << 2);

    // CSI-final-letter keys: plain `CSI <c>`, modified `CSI 1;<mod> <c>`.
    let csi_letter = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(c) = csi_letter {
        return Some(if mod_param > 1 {
            format!("\x1b[1;{mod_param}{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        });
    }

    // Tilde keys: plain `CSI <n>~`, modified `CSI <n>;<mod>~`.
    let tilde = match key {
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "delete" => Some(3),
        _ => None,
    };
    if let Some(n) = tilde {
        return Some(if mod_param > 1 {
            format!("\x1b[{n};{mod_param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        });
    }

    // Remaining named keys → fixed sequences.
    let seq: Option<&[u8]> = match key {
        "enter" => Some(b"\r"),
        "tab" if m.shift => Some(b"\x1b[Z"),
        "tab" => Some(b"\t"),
        "backspace" => Some(b"\x7f"),
        "escape" => Some(b"\x1b"),
        _ => None,
    };
    if let Some(seq) = seq {
        return Some(seq.to_vec());
    }

    // gpui gives the already-composed text for character keys (respecting
    // shift/dead keys) in key_char.
    if let Some(ref text) = ks.key_char {
        if !text.is_empty() {
            // Ctrl+letter → control byte.
            if m.control && text.len() == 1 {
                let c = text.as_bytes()[0];
                if c.is_ascii_alphabetic() {
                    return Some(vec![c.to_ascii_uppercase() & 0x1f]);
                }
            }
            return Some(text.as_bytes().to_vec());
        }
    }

    // Fall back to single-character keys (e.g. "a").
    if key.chars().count() == 1 {
        let c = key.chars().next().unwrap();
        if m.control && c.is_ascii_alphabetic() {
            return Some(vec![(c as u8).to_ascii_uppercase() & 0x1f]);
        }
        let mut buf = [0u8; 4];
        return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
    }

    None
}

// ── gpui Render / Element wiring ──────────────────────────────────────────

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Native traffic lights follow whichever surface owns the top-left
        // corner: inside the sidebar panel while it is open, over the first
        // tile's tab strip when it collapses, and over a maximized flyover's
        // tab strip while that covers the window.
        let flyover_maxed = self.flyover_open
            && !self.flyover_windowed
            && !self.flyover_tabs.is_empty()
            && self.flyover_maximized;
        let spot = workspace::traffic_light_spot(self.sidebar_collapsed, flyover_maxed);
        if self.traffic_lights_for != Some(spot) {
            let (x, y) = workspace::traffic_light_origin(spot);
            window.set_traffic_light_position(gpui::point(px(x), px(y)));
            self.traffic_lights_for = Some(spot);
        }
        // Settings search field: focus lives in the window, so reflect it
        // into the flag the sidebar styles from, and drop it (and any stale
        // query) the moment the field is off screen — leaving Settings must
        // not leave a hidden field eating keystrokes.
        let search_focused = self.settings_search.read(cx).focus_handle(cx).is_focused(window);
        if self.page != Page::Settings {
            if search_focused {
                window.focus(&self.focus_handle, cx);
            }
            if !self.settings_search.read(cx).text().is_empty() {
                self.clear_settings_search(cx);
            }
        }
        self.settings_search_focus = self.page == Page::Settings && search_focused;
        self.sync_save_focus(window, cx);
        // Shared modal search field: a freshly opened modal claims it (clear,
        // placeholder, focus); a closed one releases it back to the app.
        if let Some(placeholder) = self.modal_search_reset.take() {
            self.modal_search.update(cx, |input, cx| {
                input.placeholder(placeholder);
                input.set_text("", cx);
            });
            window.focus(&self.modal_search.read(cx).focus_handle(cx), cx);
        } else if !self.search_modal_open()
            && self.modal_search.read(cx).focus_handle(cx).is_focused(window)
        {
            window.focus(&self.focus_handle, cx);
        }
        self.sync_webview_input_focus(window, cx);

        // A full-window canvas element that paints the terminal frame.
        let view = cx.entity();
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|app, ev: &KeyDownEvent, window, cx| {
                app.on_key_down(ev, window, cx);
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|app, ev: &MouseMoveEvent, window, cx| {
                app.cursor = (f64::from(ev.position.x), f64::from(ev.position.y));
                // Scale logical → physical for internal geometry.
                let s = app.scale() as f64;
                app.cursor = (app.cursor.0 * s, app.cursor.1 * s);
                app.on_mouse_move(window);
                cx.notify();
            }))
            .on_modifiers_changed(cx.listener(|app, ev: &ModifiersChangedEvent, _window, cx| {
                app.modifiers = ev.modifiers;
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, ev: &MouseDownEvent, window, cx| {
                    let s = app.scale() as f64;
                    app.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    app.modifiers = ev.modifiers;
                    app.on_mouse_down(window, ev.click_count, cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|app, ev: &MouseDownEvent, _window, cx| {
                    let s = app.scale() as f64;
                    app.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    app.modifiers = ev.modifiers;
                    app.on_right_mouse_down();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|app, ev: &MouseDownEvent, _window, cx| {
                    let s = app.scale() as f64;
                    app.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    app.modifiers = ev.modifiers;
                    app.on_middle_mouse_down();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|app, _ev: &MouseUpEvent, window, cx| {
                    app.on_mouse_up(window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|app, ev: &MouseUpEvent, _window, cx| {
                    let s = app.scale() as f64;
                    app.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    app.modifiers = ev.modifiers;
                    if app.try_forward_release(MouseBtn::Right) {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|app, ev: &MouseUpEvent, _window, cx| {
                    let s = app.scale() as f64;
                    app.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    app.modifiers = ev.modifiers;
                    if app.try_forward_release(MouseBtn::Middle) {
                        cx.notify();
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(|app, ev: &gpui::ScrollWheelEvent, _win, cx| {
                app.on_scroll(ev.delta, app.renderer.cell_height);
                cx.notify();
            }))
            .child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, _prepaint, window, cx| {
                        // While a drag is armed, drive it from window-level
                        // capture listeners instead of the hover-gated div
                        // listeners above: overlay roots (Flow, Settings)
                        // `occlude()`, so once the pointer crosses onto one the canvas
                        // stops being hovered and would never hear the
                        // release — leaving the resize following the mouse
                        // with no button down. Moves stop propagating so the
                        // div listener does not apply the same move twice;
                        // the release keeps bubbling (the div's handler then
                        // sees `Drag::None` and no-ops) so element `on_click`s
                        // on the same mouse-up are never swallowed.
                        // gpui scopes `window.on_mouse_event` listeners to the
                        // frame they are registered in, so re-registering on every
                        // paint never stacks them.
                        let drag_view = view.clone();
                        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                            if phase != gpui::DispatchPhase::Capture {
                                return;
                            }
                            drag_view.update(cx, |app, cx| {
                                if matches!(app.drag, Drag::None) {
                                    return;
                                }
                                let s = app.scale() as f64;
                                app.cursor =
                                    (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                                app.modifiers = ev.modifiers;
                                app.on_mouse_move(window);
                                cx.stop_propagation();
                                cx.notify();
                            });
                        });
                        let drag_view = view.clone();
                        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                            if phase != gpui::DispatchPhase::Capture
                                || ev.button != MouseButton::Left
                            {
                                return;
                            }
                            drag_view.update(cx, |app, cx| {
                                if matches!(app.drag, Drag::None) {
                                    return;
                                }
                                let s = app.scale() as f64;
                                app.cursor =
                                    (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                                app.modifiers = ev.modifiers;
                                app.on_mouse_up(window, cx);
                                cx.notify();
                            });
                        });
                        view.update(cx, |app, cx| {
                            app.paint_terminal(bounds, window, cx);
                        });
                    },
                )
                // Without an explicit size the canvas resolves to 0 width and
                // the terminal never paints.
                .size_full(),
            )
            // Sessions sidebar: an element tree drawn as a sibling of the
            // canvas. Geometry still comes from `workspace::sidebar_row_rect`,
            // so the canvas underneath keeps resolving every click and drag.
            // Returns an empty element when collapsed or on the canvas-sidebar
            // pages.
            // Window-drag region (titlebar strip / folded traffic-light
            // corner): an element that starts the native window move.
            .child(self.render_window_drag_zones())
            // Tile tab strips: pixels on the element tree, clipped per
            // strip; clicks and drags still resolve on the canvas rects.
            .child(self.render_tile_chrome(cx))
            // Browser chrome sits in the strip reserved above each native
            // child webview and remains GPUI-owned for consistent controls.
            .child(self.render_webview_chrome(window, cx))
            // Flyover tab strip: same pixels-on-elements split (`flyover_ui`).
            .child(self.render_flyover_chrome(cx))
            .child(self.render_sidebar(cx))
            // Floating "Show sessions" button beside the relocated traffic
            // lights while the whole region is hidden.
            .child(self.render_collapsed_overlay(cx))
            // Sessions empty state ("New group" pill + hint): element tree in
            // the terminal area; its click resolves on the element.
            .child(self.render_empty_state(cx))
            // Settings page overlay: element tree over the canvas. Sidebar search +
            // section tabs stay canvas-painted; the content card is elements.
            .when(self.page == Page::Settings, |el| el.child(self.render_settings(cx)))
            // Flow agent (experimental, `features.flow`): bottom-centered pill
            // bar + chat panel, on every page so it follows the user (`flow_ui`).
            .when(
                crate::flow::enabled() && !self.modal_overlay_open(),
                |el| el.child(self.render_flow(window, cx)),
            )
            // Resize handles (sidebar edge, dividers, flyover
            // top edge): elements own the cursor and the drag start; the
            // canvas still drives the drag (`resize_ui`).
            .child(self.render_resize_handles(cx))
            // Command palette and the pickers (element trees; see
            // `palette_ui` / `picker_ui`).
            .child(self.render_command(cx))
            .child(self.render_new_webview_prompt(cx))
            // Save-as-workspace modal (element tree; see `save_ui`).
            .child(self.render_save(cx))
            // Modal overlays (confirm dialog, message panel): last, so they
            // sit above every page overlay; the canvas flyover is painted
            // inside the canvas element, so they cover it too.
            .child(self.render_modals(cx))
    }
}


impl App {
    /// Paint the whole terminal frame into `bounds`. Follows the verified
    /// build order: bg quads → per-pane text → fg quads → labels → picker.
    fn paint_terminal(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut GpuiApp) {
        // Keep the renderer's surface size in sync with the window.
        let scale = window.scale_factor();
        let phys_w = (f32::from(bounds.size.width) * scale) as u32;
        let phys_h = (f32::from(bounds.size.height) * scale) as u32;
        // The window moved to a display with a different backing scale, or the
        // user changed a font-size setting (⌘= / ⌘- or the Accessibility
        // steppers): cell metrics and dpi derive from both, so re-measure the
        // terminal and chrome cells and force the PTYs to learn the new cell
        // size even if the grid dimensions happen to be unchanged.
        let term_font = renderer::terminal_font();
        let chrome_font = renderer::chrome_font();
        let rescaled = scale != self.renderer.scale
            || term_font != self.renderer.term_font()
            || chrome_font != self.renderer.chrome_font_logical();
        if rescaled {
            let term_cw = renderer::measure_cell_width(window, scale, term_font);
            let chrome_cw = renderer::measure_cell_width(window, scale, chrome_font);
            self.renderer.update_metrics(scale, term_font, term_cw, chrome_font, chrome_cw);
        }
        self.renderer.resize(phys_w, phys_h);
        self.sync_layout_impl(rescaled);
        if rescaled {
            self.sync_flyover_layout(true);
            self.sync_tool_layout(true);
        }
        self.sync_webviews(window);
        self.begin_frame();

        // Cursor style must be set during paint (gpui asserts the phase). Sticky
        // resize_hover keeps the resize cursor for the whole drag, even when the
        // pointer strays off the handle. An overlay opened by keyboard while
        // hovering leaves resize_hover stale, so overlays suppress it here too.
        let overlay_open = self.modal_overlay_open();
        let resize_hover = if overlay_open
            || !matches!(
                self.drag,
                Drag::None | Drag::Sidebar | Drag::Folders | Drag::Divider { .. }
            )
        {
            None
        } else {
            self.resize_hover.as_ref()
        };
        if let Some(hover) = resize_hover {
            let style = match hover {
                workspace::ResizeHover::Divider { dir: Dir::Column, .. } => {
                    CursorStyle::ResizeUpDown
                },
                _ => CursorStyle::ResizeLeftRight,
            };
            window.set_window_cursor_style(style);
        }
        // The flyover's top edge resizes: show the up-down cursor while
        // hovering the grab zone or mid-drag.
        if !overlay_open
            && self.flyover_open
            && !self.flyover_windowed
            && !self.flyover_maximized
            && !self.flyover_tabs.is_empty()
        {
            let panel = self.flyover_rect_now();
            let grab = (workspace::FLYOVER_RESIZE_GRAB * scale).max(1.0);
            if (self.cursor.1 as f32 - panel.y).abs() <= grab
                || matches!(self.drag, Drag::FlyoverResize)
            {
                window.set_window_cursor_style(CursorStyle::ResizeUpDown);
            }
        }

        // While a tab/group/section is being dragged, resolve the current
        // landing zone and compute its translucent preview rect. Sidebar
        // targets are deliberately filtered out: the panel is an opaque
        // element sibling painted after the canvas, so a fg_quad in sidebar
        // coordinates would be occluded. `sidebar_ui::drop_feedback_layer`
        // draws those, at the very same rects.
        let drop_hint = self
            .current_drop_target()
            .filter(|t| !t.in_sidebar())
            .and_then(|t| self.drop_hint(t, scale));

        let chrome = renderer::ChromeState {
            page: self.page,
            flow_inset: self.flow_inset(),
            // Overlay scoping happens in the renderer (only overlay elements
            // hover while one is up). Here we suppress hover mid-drag, and
            // for the chrome under an open flyover panel — clicks inside the
            // panel never fall through, so hover mustn't either. Modal
            // overlays sit above the flyover, so they keep the live cursor.
            element_modal: self.confirm.is_some()
                || self.message.is_some()
                || self.save_ws.is_some()
                || self.search_modal_open(),
            cursor: {
                let (cx, cy) = (self.cursor.0 as f32, self.cursor.1 as f32);
                let flyover_covers = !overlay_open
                    && self.flyover_open
                    && !self.flyover_windowed
                    && !self.flyover_tabs.is_empty()
                    && self.flyover_rect_now().contains(cx, cy);
                if matches!(self.drag, Drag::None) && !flyover_covers {
                    Some((cx, cy))
                } else {
                    None
                }
            },
        };
        let link_hover_suppressed = if overlay_open
            || !matches!(self.drag, Drag::None)
        {
            None
        } else {
            self.link_hover
        };
        // Show pointing-hand cursor when hovering a link while ⌘ is held.
        if resize_hover.is_none() && link_hover_suppressed.is_some() && self.modifiers.platform {
            window.set_window_cursor_style(CursorStyle::PointingHand);
        }
        let mut frame = self.renderer.build_frame(

            &self.workspaces,
            self.active,
            self.sidebar_w(),
            drop_hint,
            link_hover_suppressed,
            &chrome,
        );
        // A tool page's terminal lives outside the workspace tree too, so its
        // card is built here from App state and slotted into the frame's
        // ordinary layers (it *is* the page content, under the flyover).
        if let Page::Tool(i) = self.page
            && let Some(Some(ts)) = self.tool_sessions.get(i)
        {
            let area = self.tool_area();
            let title = self.tools.get(i).map(|t| t.name.as_str()).unwrap_or("");
            let (quads, pane, fg_quads, labels) =
                self.renderer.tool_page(&ts.tab, &area, title, ts.exited, !overlay_open);
            frame.bg_quads.extend(quads);
            frame.panes.push(pane);
            frame.fg_quads.extend(fg_quads);
            frame.labels.extend(labels);
        }
        // The flyover panel lives outside the workspace tree, so its layer is
        // built here from App state and slotted into the frame's flyover
        // fields (painted above tiles/labels, below the modal overlays).
        // In windowed mode the popout window renders it instead. The gate
        // matches the on_mouse_down hit-test (and sidebar_ui::flyover_ceiling):
        // with no tabs there is nothing to paint, and an empty card would
        // otherwise linger over the Sessions empty state during the close
        // animation.
        if self.flyover_anim > 0.0 && !self.flyover_tabs.is_empty() && !self.flyover_windowed {
            let panel = self.flyover_rect_now();
            let flyover_cursor = if overlay_open || !matches!(self.drag, Drag::None) {
                None
            } else {
                Some((self.cursor.0 as f32, self.cursor.1 as f32))
            };
            let mut flyover_hot = Vec::new();
            let (quads, panes, fg_quads, labels) = self.renderer.flyover_overlay(
                &self.flyover_tabs,
                self.flyover_active,
                &panel,
                self.flyover_focused,
                !overlay_open,
                true,
                self.flyover_maximized,
                flyover_cursor,
                // The strip's pixels are an element tree (`flyover_ui`).
                false,
                &mut flyover_hot,
            );
            frame.flyover_quads = quads;
            frame.flyover_panes = panes;
            frame.flyover_fg_quads = fg_quads;
            frame.flyover_labels = labels;
            // The panel paints above the chrome, so its controls append last
            // (topmost) — but never over a modal overlay, which owns the frame.
            if !overlay_open {
                frame.hot.extend(flyover_hot);
            }
        }
        // Impression blur for the liquid-glass overlays (`backdrop.rs`): only
        // while one is on screen, and only re-blurred + re-uploaded when the
        // coarse picture changed. Overlays are element trees rendered before
        // this paint, so a fresh image asks for one more render to show up.
        if self.flow_inset() > 0.0 || self.search_modal_open() {
            let ground = renderer::color(self.renderer.term_scheme_bg(), 1.0);
            let mut imp = backdrop::rasterize(
                &frame,
                ground,
                phys_w as f32,
                phys_h as f32,
                self.renderer.cell_width,
                self.renderer.cell_height,
            );
            let key = imp.fingerprint();
            if key != self.glass_backdrop_key || self.glass_backdrop.is_none() {
                imp.blur(backdrop::BLUR_RADIUS);
                imp.grade(backdrop::SATURATE, backdrop::BRIGHTNESS);
                if let Some(old) = self.glass_backdrop.take() {
                    let _ = window.drop_image(old);
                }
                self.glass_backdrop = Some(std::sync::Arc::new(imp.to_render_image()));
                self.glass_backdrop_key = key;
                self.glass_backdrop_size =
                    (f32::from(bounds.size.width), f32::from(bounds.size.height));
                window.refresh();
            }
        } else if let Some(old) = self.glass_backdrop.take() {
            let _ = window.drop_image(old);
        }
        // The frame's hot list is the authority on what's clickable this
        // paint. Recompute the hover index from it right away (rather than
        // trusting the value on_mouse_move derived from the previous frame)
        // so a keyboard-opened overlay or layout change can't leave a stale
        // pointing hand; on_mouse_move only change-detects to trigger redraws.
        self.hot_rects = frame.hot.clone();
        self.ui_hover = if matches!(self.drag, Drag::None) {
            let (cx, cy) = (self.cursor.0 as f32, self.cursor.1 as f32);
            self.hot_rects
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.contains(cx, cy))
                .map(|(i, _)| i)
        } else {
            None
        };
        // Show pointing-hand cursor when hovering any interactive chrome
        // element (links and resize handles keep priority).
        if resize_hover.is_none() && link_hover_suppressed.is_none() && self.ui_hover.is_some() {
            window.set_window_cursor_style(CursorStyle::PointingHand);
        }

        let origin = bounds.origin;
        let inv = 1.0 / scale; // physical px → logical px for gpui coords.
        let font = gpui::font(renderer::FONT_FAMILY);
        // Terminal grid text uses the terminal cell metrics; chrome labels use
        // the (independently zoomable) chrome cell metrics.
        let font_size = px(self.renderer.font_size() * inv);
        let line_height = px(self.renderer.cell_height * inv);
        let cell_height = self.renderer.cell_height;
        let chrome_font_size = px(self.renderer.chrome_font_size() * inv);
        let chrome_line_height = px(self.renderer.chrome_cell_height * inv);

        // Theme colors resolved once per frame (gradient + shadow ink).
        let th = self.renderer.theme();
        let shadow_rgb = th.shadow;

        // Paint inside an explicit content mask over our bounds. Text glyphs
        // paint into their own pushed layer (via gpui's paint_layer); without an
        // established content-mask context those sub-layers don't composite —
        // this mirrors how Zed's own TerminalElement paints.
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            // 0) the ground every card and sidebar row floats on.
            //
            // Messages-style blending: the ground is the *terminal* background,
            // not a tinted gradient, so an unfocused pane has nothing to stand
            // out against and reads as part of the window. Only the focused
            // pane keeps its border and shadow (see `renderer.rs`), and the
            // sidebar stays a floating card — the same trick Messages plays
            // with its conversation list against a flat message area.
            //
            // `term_scheme_bg()` and not the chrome theme's `term_bg`: those
            // are different colours whenever a terminal scheme is selected, and
            // it is the *scheme's* background the panes actually paint. Reading
            // the chrome value put a dark ground behind white panes and killed
            // the whole effect. Also why this is not a literal white — it
            // tracks whichever terminal theme is live, so a dark scheme blends
            // just as cleanly.
            //
            // Opaque even under vibrancy: a translucent ground lets the desktop
            // through, which reintroduces exactly the contrast the panes are
            // supposed to be losing.
            let ground = renderer::color(self.renderer.term_scheme_bg(), 1.0);
            window.paint_quad(gpui::fill(bounds, ground));

            // 1) background quads.
            for q in &frame.bg_quads {
                paint_quad(window, origin, inv, q, shadow_rgb);
            }

            // 2) per-pane foreground text.
            for pane in &frame.panes {
                let (ox, oy) = pane.origin;
                for (ri, row) in pane.rows.iter().enumerate() {
                    if row.is_empty() {
                        continue;
                    }
                    let mut text = String::new();
                    let mut runs: Vec<TextRun> = Vec::new();
                    for span in row {
                        text.push_str(&span.text);
                        runs.push(TextRun {
                            len: span.text.len(),
                            font: font.clone(),
                            color: span.color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        });
                    }
                    let shaped: ShapedLine =
                        window.text_system().shape_line(text.into(), font_size, &runs, None);
                    let p = Point::new(
                        origin.x + px(ox * inv),
                        origin.y + px((oy + ri as f32 * cell_height) * inv),
                    );
                    let _ = shaped.paint(p, line_height, TextAlign::Left, None, window, cx);
                }
            }

            // 3) foreground quads (box-drawing / block glyphs from rect.rs).
            for q in &frame.fg_quads {
                paint_quad(window, origin, inv, q, shadow_rgb);
            }

            // 3.5) collapse carets. Quads can't rotate, so each chevron is a
            // small filled gpui path: a V polyline thickened vertically, its
            // points rotated around the caret center by the animated angle.
            for c in &frame.carets {
                let (sin, cos) = c.angle.sin_cos();
                let pt = |x: f32, y: f32| Point::new(
                    origin.x + px((c.cx + x * cos - y * sin) * inv),
                    origin.y + px((c.cy + x * sin + y * cos) * inv),
                );
                let w = c.size;
                let d = w * 0.55;
                let t = w * 0.75;
                let mut path = gpui::Path::new(pt(-w, -d));
                path.line_to(pt(0.0, d));
                path.line_to(pt(w, -d));
                path.line_to(pt(w, -d + t));
                path.line_to(pt(0.0, d + t));
                path.line_to(pt(-w, -d + t));
                window.paint_path(path, c.color);
            }

            // 4) labels (tab titles, sidebar text, etc.).
            for label in &frame.labels {
                let runs = [TextRun {
                    len: label.text.len(),
                    font: font.clone(),
                    color: label.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let size = label.size.map_or(chrome_font_size, |s| px(s * inv));
                let shaped =
                    window.text_system().shape_line(label.text.clone().into(), size, &runs, None);
                let p = Point::new(origin.x + px(label.left * inv), origin.y + px(label.top * inv));
                let clip_bounds = Bounds {
                    origin: Point::new(
                        origin.x + px(label.clip.x * inv),
                        origin.y + px(label.clip.y * inv),
                    ),
                    size: Size::new(px(label.clip.w * inv), px(label.clip.h * inv)),
                };
                window.with_content_mask(Some(gpui::ContentMask { bounds: clip_bounds }), |window| {
                    let _ = shaped.paint(p, chrome_line_height, TextAlign::Left, None, window, cx);
                });
            }

            // 4.5) flyover terminal panel — above the workspace chrome,
            // below the modal overlays and their scrim.
            let metrics = FlyoverPaintMetrics {
                origin,
                inv,
                font: font.clone(),
                font_size,
                line_height,
                cell_height,
                chrome_font_size,
                chrome_line_height,
                shadow_rgb,
            };
            paint_flyover_layer(
                window,
                cx,
                &metrics,
                &frame.flyover_quads,
                &frame.flyover_panes,
                &frame.flyover_fg_quads,
                &frame.flyover_labels,
            );

            // 5) The modal overlays (pickers, palette, save, confirm, message)
            //    are element trees now — nothing paints above the flyover here.
        });

        self.dirty = false;
    }
}

/// The `NSWindow` behind a gpui window, for the AppKit surgery below.
#[cfg(target_os = "macos")]
fn ns_window(window: &Window) -> Option<*mut objc::runtime::Object> {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Explicit trait call: gpui's `Window` has an inherent `window_handle()`
    // (returning `AnyWindowHandle`) that would otherwise shadow the trait's.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
    let ns_view = appkit.ns_view.as_ptr() as *mut Object;
    let ns_window: *mut Object = unsafe { msg_send![ns_view, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// Darwin kernel major version (`uname -r`), e.g. 25 on macOS 26 Tahoe.
#[cfg(target_os = "macos")]
fn darwin_major() -> u32 {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return 0;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
    release
        .to_str()
        .ok()
        .and_then(|r| r.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

/// Give the window an empty, invisible `NSToolbar` on macOS 26 (Tahoe) and
/// later. Tahoe rounds windows two ways: titlebar-only windows get the compact
/// corner radius, windows that carry a toolbar get the large one used by
/// Messages, Calculator and the rest of the system apps. We draw our own
/// chrome under a transparent titlebar, so the toolbar has no items and no
/// separator — it exists only to opt into the larger radius. gpui re-frames
/// the titlebar container around the traffic lights on every layout, so the
/// toolbar adds no visible height either.
#[cfg(target_os = "macos")]
fn attach_empty_toolbar(window: &Window) {
    use objc::runtime::{Object, NO};
    use objc::{class, msg_send, sel, sel_impl};

    if darwin_major() < 25 {
        return;
    }
    let Some(ns_window) = ns_window(window) else { return };
    unsafe {
        let ident: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c"pwrde.window".as_ptr()];
        let toolbar: *mut Object = msg_send![class!(NSToolbar), alloc];
        let toolbar: *mut Object = msg_send![toolbar, initWithIdentifier: ident];
        if toolbar.is_null() {
            return;
        }
        let _: () = msg_send![toolbar, setShowsBaselineSeparator: NO];
        let _: () = msg_send![toolbar, setAllowsUserCustomization: NO];
        // NSWindowToolbarStyleUnified: the full-height toolbar layout whose
        // radius matches Messages (`unifiedCompact` rounds less).
        let _: () = msg_send![ns_window, setToolbarStyle: 3isize];
        let _: () = msg_send![ns_window, setToolbar: toolbar];
        let _: () = msg_send![toolbar, release];
    }
}

/// Hide AppKit's private `_NSTitlebarDecorationView`, which draws a ~1px
/// light highlight hairline across the top edge of the window frame. With our
/// transparent titlebar over dark content that hairline shows as a stray
/// white line at the top of the window. The traffic-light widgets live in the
/// sibling `NSTitlebarView`, so hiding the decoration view leaves them
/// untouched.
#[cfg(target_os = "macos")]
fn hide_titlebar_decoration(window: &Window) {
    use objc::runtime::{Object, YES};
    use objc::{msg_send, sel, sel_impl};

    let Some(ns_window) = ns_window(window) else { return };
    unsafe {
        let content: *mut Object = msg_send![ns_window, contentView];
        if content.is_null() {
            return;
        }
        // contentView's superview is the NSThemeFrame; the decoration view
        // lives inside its NSTitlebarContainerView child.
        let frame: *mut Object = msg_send![content, superview];
        if frame.is_null() {
            return;
        }
        let subviews: *mut Object = msg_send![frame, subviews];
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let container: *mut Object = msg_send![subviews, objectAtIndex: i];
            if !(*container).class().name().contains("NSTitlebarContainerView") {
                continue;
            }
            let inner: *mut Object = msg_send![container, subviews];
            let n: usize = msg_send![inner, count];
            for j in 0..n {
                let v: *mut Object = msg_send![inner, objectAtIndex: j];
                if (*v).class().name().contains("TitlebarDecoration") {
                    let _: () = msg_send![v, setHidden: YES];
                }
            }
        }
    }
}

/// Paint one renderer `Quad` (physical-px coords) as a gpui fill, with its
/// optional drop shadow (under, in the theme's shadow ink) and border.
fn paint_quad(
    window: &mut Window,
    origin: Point<Pixels>,
    inv: f32,
    q: &renderer::Quad,
    shadow_rgb: (u8, u8, u8),
) {
    let b = Bounds {
        origin: Point::new(origin.x + px(q.x * inv), origin.y + px(q.y * inv)),
        size: Size::new(px(q.w * inv), px(q.h * inv)),
    };
    let radii = gpui::Corners::all(px(q.radius * inv));
    // (alpha, y-offset, blur) in logical px, matching mock 3a's card/row shadows.
    let shadow = match q.shadow {
        renderer::Shadow::None => None,
        renderer::Shadow::Card => Some((0.22, 8.0, 28.0)),
    };
    if let Some((alpha, dy, blur)) = shadow {
        window.paint_drop_shadows(
            b,
            radii,
            &[gpui::BoxShadow {
                color: renderer::color(shadow_rgb, alpha),
                offset: Point::new(px(0.0), px(dy)),
                blur_radius: px(blur),
                spread_radius: px(0.0),
                inset: false,
            }],
        );
    }
    let mut quad = gpui::fill(b, q.color);
    // Honor the renderer's corner radius (physical px → logical), so the tile
    // cards, sidebar rows, and picker panel/search box round.
    quad.corner_radii = radii;
    if q.border > 0.0 {
        quad.border_widths = gpui::Edges::all(px(q.border * inv));
        quad.border_color = q.border_color;
    }
    window.paint_quad(quad);
}

/// Per-frame constants the flyover layer painter needs — one bundle so the
/// main window's paint and the popout window's paint stay in lockstep.
struct FlyoverPaintMetrics {
    origin: Point<Pixels>,
    /// Physical px → logical px (1.0 / scale).
    inv: f32,
    font: gpui::Font,
    /// Terminal-grid text metrics (flyover panes).
    font_size: Pixels,
    line_height: Pixels,
    cell_height: f32,
    /// Chrome text metrics (flyover tab-strip labels).
    chrome_font_size: Pixels,
    chrome_line_height: Pixels,
    shadow_rgb: (u8, u8, u8),
}

/// Paint one flyover layer (card + tab-strip quads, terminal text, geometry
/// quads, clipped labels). Shared by `paint_terminal`'s 4.5 step and the
/// popout window's paint.
fn paint_flyover_layer(
    window: &mut Window,
    cx: &mut GpuiApp,
    m: &FlyoverPaintMetrics,
    quads: &[renderer::Quad],
    panes: &[renderer::PaneText],
    fg_quads: &[renderer::Quad],
    labels: &[renderer::LabelSpec],
) {
    for q in quads {
        paint_quad(window, m.origin, m.inv, q, m.shadow_rgb);
    }
    for pane in panes {
        let (ox, oy) = pane.origin;
        for (ri, row) in pane.rows.iter().enumerate() {
            if row.is_empty() {
                continue;
            }
            let mut text = String::new();
            let mut runs: Vec<TextRun> = Vec::new();
            for span in row {
                text.push_str(&span.text);
                runs.push(TextRun {
                    len: span.text.len(),
                    font: m.font.clone(),
                    color: span.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            let shaped: ShapedLine =
                window.text_system().shape_line(text.into(), m.font_size, &runs, None);
            let p = Point::new(
                m.origin.x + px(ox * m.inv),
                m.origin.y + px((oy + ri as f32 * m.cell_height) * m.inv),
            );
            let _ = shaped.paint(p, m.line_height, TextAlign::Left, None, window, cx);
        }
    }
    for q in fg_quads {
        paint_quad(window, m.origin, m.inv, q, m.shadow_rgb);
    }
    for label in labels {
        let runs = [TextRun {
            len: label.text.len(),
            font: m.font.clone(),
            color: label.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        let size = label.size.map_or(m.chrome_font_size, |s| px(s * m.inv));
        let shaped = window.text_system().shape_line(label.text.clone().into(), size, &runs, None);
        let p = Point::new(
            m.origin.x + px(label.left * m.inv),
            m.origin.y + px(label.top * m.inv),
        );
        let clip_bounds = Bounds {
            origin: Point::new(
                m.origin.x + px(label.clip.x * m.inv),
                m.origin.y + px(label.clip.y * m.inv),
            ),
            size: Size::new(px(label.clip.w * m.inv), px(label.clip.h * m.inv)),
        };
        window.with_content_mask(Some(gpui::ContentMask { bounds: clip_bounds }), |window| {
            let _ = shaped.paint(p, m.chrome_line_height, TextAlign::Left, None, window, cx);
        });
    }
}

/// Root view of the flyover's popout window: a thin shell that renders the
/// flyover tabs straight out of the shared [`App`] entity with its own
/// [`Renderer`]. The sessions never move — only which surface paints them.
/// The frame pump opens/closes this window to match
/// `App::flyover_window_visible`.
struct FlyoverPopout {
    app: gpui::Entity<App>,
    renderer: Renderer,
    focus_handle: FocusHandle,
    /// Pointer position in this window's physical px.
    cursor: (f64, f64),
    /// Keyboard modifiers from the last pointer event, so a mouse-tracking TUI
    /// sees Shift/Alt/Ctrl on forwarded clicks (and Shift can override the grab).
    modifiers: Modifiers,
    /// True while a selection drag is in flight.
    selecting: bool,
    /// Buttons currently forwarded to a mouse-tracking TUI in this popout
    /// (bit 0 = left, 1 = middle, 2 = right); mirrors `App::mouse_report`.
    mouse_report_buttons: u8,
    /// Sub-notch wheel travel, as in `App::scroll_accum`.
    scroll_accum: f64,
    /// Interactive rects from the last paint (tab strip controls), mirroring
    /// `App::hot_rects` so hover repaints and the pointing hand work here too.
    hot_rects: Vec<workspace::LayoutRect>,
    /// Index into `hot_rects` of the hovered control (topmost wins).
    ui_hover: Option<usize>,
}

impl FlyoverPopout {
    /// The flyover fills the whole popout window.
    fn panel_rect(&self) -> workspace::LayoutRect {
        let (w, h) = self.renderer.surface_size();
        workspace::LayoutRect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 }
    }

    /// True when the popout's active terminal has grabbed the mouse and Shift
    /// isn't held to force local selection.
    fn popout_grabs_mouse(&self, cx: &mut Context<Self>) -> bool {
        if self.modifiers.shift {
            return false;
        }
        let app = self.app.read(cx);
        app.flyover_tabs
            .get(app.flyover_active)
            .and_then(Tab::session)
            .is_some_and(Session::app_grabs_mouse)
    }

    /// Forward one button event to the popout's active terminal as a mouse
    /// report, mapping the pointer through this window's renderer. Mirrors
    /// [`App::forward_mouse_report`].
    fn forward_popout_mouse(&self, phase: MousePhase, btn: MouseBtn, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let content = workspace::flyover_content(&self.panel_rect(), scale);
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (col, row) = self.renderer.cell_at(&content, mx, my).unwrap_or((0, 0));
        let m = self.modifiers;
        self.app.update(cx, |app, _| {
            if let Some(session) = app
                .flyover_tabs
                .get(app.flyover_active)
                .and_then(Tab::session)
            {
                session.forward_mouse(phase, btn, col, row, m.shift, m.alt, m.control);
            }
        });
    }

    /// Forward a button press to a mouse-tracking terminal in the popout and
    /// mark it held; `true` when consumed (so the caller skips selection).
    fn popout_press(&mut self, btn: MouseBtn, cx: &mut Context<Self>) -> bool {
        if !self.popout_grabs_mouse(cx) {
            return false;
        }
        self.forward_popout_mouse(MousePhase::Press, btn, cx);
        self.mouse_report_buttons |= MouseReport::bit(btn);
        cx.notify();
        true
    }

    /// Forward a button release for a held forwarded button; `true` when it
    /// was held (and thus consumed as a report).
    fn popout_release(&mut self, btn: MouseBtn, cx: &mut Context<Self>) -> bool {
        let bit = MouseReport::bit(btn);
        if self.mouse_report_buttons & bit == 0 {
            return false;
        }
        self.mouse_report_buttons &= !bit;
        self.forward_popout_mouse(MousePhase::Release, btn, cx);
        cx.notify();
        true
    }

    fn on_mouse_down(&mut self, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let panel = self.panel_rect();
        let tab_bar = workspace::flyover_tab_bar(&panel, scale);
        let content = workspace::flyover_content(&panel, scale);
        let cell = self.renderer.cell_at(&content, mx, my);

        let n = self.app.read(cx).flyover_tabs.len();
        if n == 0 {
            return;
        }
        if tab_bar.contains(mx, my) {
            let tr = workspace::flyover_tab_rect(&panel, 0, n, scale, false);
            let ti = (((mx - tr.x).max(0.0) / tr.w).floor() as usize).min(n - 1);
            self.app.update(cx, |app, _| {
                if workspace::flyover_tab_close_rect(&panel, ti, n, scale, false).contains(mx, my) {
                    app.close_flyover_tab(ti);
                } else {
                    app.flyover_active = ti;
                    app.flyover_mark_read();
                    app.request_redraw();
                }
            });
            cx.notify();
            return;
        }
        // Content: a mouse-tracking TUI takes the click; else select text.
        if self.popout_press(MouseBtn::Left, cx) {
            return;
        }
        if let Some((col, row)) = cell {
            self.app.update(cx, |app, _| {
                if let Some(session) = app
                    .flyover_tabs
                    .get(app.flyover_active)
                    .and_then(Tab::session)
                {
                    session.begin_selection(col, row);
                }
                app.request_redraw();
            });
            self.selecting = true;
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
        // A held forwarded button turns motion into drag reports.
        if self.mouse_report_buttons != 0 {
            for btn in [MouseBtn::Left, MouseBtn::Middle, MouseBtn::Right] {
                if self.mouse_report_buttons & MouseReport::bit(btn) != 0 {
                    self.forward_popout_mouse(MousePhase::Move, btn, cx);
                }
            }
            cx.notify();
            return;
        }
        if !self.selecting {
            // Control hover: repaint only when the hovered control changes
            // (mirrors `App::on_mouse_move`'s change detection).
            let ui_hover = self
                .hot_rects
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.contains(mx, my))
                .map(|(i, _)| i);
            if ui_hover != self.ui_hover {
                self.ui_hover = ui_hover;
                cx.notify();
            }
            return;
        }
        let panel = self.panel_rect();
        let content = workspace::flyover_content(&panel, scale);
        if let Some((col, row)) = self.renderer.cell_at(&content, mx, my) {
            self.app.update(cx, |app, _| {
                if let Some(session) = app
                    .flyover_tabs
                    .get(app.flyover_active)
                    .and_then(Tab::session)
                {
                    session.update_selection(col, row);
                    app.request_redraw();
                }
            });
            cx.notify();
        }
    }

    fn on_scroll(&mut self, delta: gpui::ScrollDelta, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let panel = self.panel_rect();
        let content = workspace::flyover_content(&panel, scale);
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let cell_h = f64::from(self.renderer.cell_height);
        let notches = match delta {
            gpui::ScrollDelta::Lines(p) => f64::from(p.y),
            gpui::ScrollDelta::Pixels(p) => f64::from(f32::from(p.y)) / (cell_h * 3.0),
        };
        let steps = scroll_steps(&mut self.scroll_accum, notches);
        if steps == 0 {
            return;
        }
        let cell = self.renderer.cell_at(&content, mx, my);
        self.app.update(cx, |app, _| {
            if let Some(session) = app
                .flyover_tabs
                .get(app.flyover_active)
                .and_then(Tab::session)
            {
                let up = steps > 0;
                if session.app_consumes_wheel() {
                    let (col, row) = cell.unwrap_or((0, 0));
                    for _ in 0..steps.unsigned_abs() {
                        session.forward_wheel(up, col, row);
                    }
                } else {
                    session.scroll_by(steps * 3);
                }
                app.request_redraw();
            }
        });
        cx.notify();
    }

    /// Paint the flyover into the popout window. Mirrors `paint_terminal`'s
    /// scale/resize discipline, then reuses the shared layer painter.
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let scale = window.scale_factor();
        let phys_w = (f32::from(bounds.size.width) * scale) as u32;
        let phys_h = (f32::from(bounds.size.height) * scale) as u32;
        let term_font = renderer::terminal_font();
        let chrome_font = renderer::chrome_font();
        if scale != self.renderer.scale
            || term_font != self.renderer.term_font()
            || chrome_font != self.renderer.chrome_font_logical()
        {
            let term_cw = renderer::measure_cell_width(window, scale, term_font);
            let chrome_cw = renderer::measure_cell_width(window, scale, chrome_font);
            self.renderer.update_metrics(scale, term_font, term_cw, chrome_font, chrome_cw);
        }
        self.renderer.resize(phys_w, phys_h);

        let panel = self.panel_rect();
        let content = workspace::flyover_content(&panel, scale);
        let (cols, rows) = self.renderer.grid_size_for(&content);
        let (cw, ch) = self.renderer.pty_cell_size();
        let dpi = (96.0 * scale) as u32;
        let focused = window.is_window_active();

        let renderer = &self.renderer;
        let popout_cursor = if self.selecting {
            None
        } else {
            Some((self.cursor.0 as f32, self.cursor.1 as f32))
        };
        let mut hot = Vec::new();
        let (quads, panes, fg_quads, labels) = self.app.update(cx, |app, _| {
            // The popout owns these grids while windowed: keep the PTYs sized
            // to this window, not the main panel.
            for tab in &mut app.flyover_tabs {
                if (cols, rows) != (tab.cols, tab.rows) {
                    tab.cols = cols;
                    tab.rows = rows;
                    if let Some(session) = tab.session() {
                        session.resize(cols, rows, cw, ch, dpi);
                    }
                }
            }
            renderer.flyover_overlay(
                &app.flyover_tabs,
                app.flyover_active,
                &panel,
                focused,
                true,
                false,
                false,
                popout_cursor,
                // The popout paints its own strip on the canvas.
                true,
                &mut hot,
            )
        });
        self.hot_rects = hot;
        self.ui_hover = popout_cursor.and_then(|(mx, my)| {
            self.hot_rects
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.contains(mx, my))
                .map(|(i, _)| i)
        });
        if self.ui_hover.is_some() {
            window.set_window_cursor_style(CursorStyle::PointingHand);
        }

        let origin = bounds.origin;
        let inv = 1.0 / scale;
        let th = self.renderer.theme();
        let metrics = FlyoverPaintMetrics {
            origin,
            inv,
            font: gpui::font(renderer::FONT_FAMILY),
            font_size: px(self.renderer.font_size() * inv),
            line_height: px(self.renderer.cell_height * inv),
            cell_height: self.renderer.cell_height,
            chrome_font_size: px(self.renderer.chrome_font_size() * inv),
            chrome_line_height: px(self.renderer.chrome_cell_height * inv),
            shadow_rgb: th.shadow,
        };
        let term_bg = self.renderer.term_scheme_bg();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            window.paint_quad(gpui::fill(bounds, renderer::color(term_bg, 1.0)));
            paint_flyover_layer(window, cx, &metrics, &quads, &panes, &fg_quads, &labels);
        });
    }
}

impl Render for FlyoverPopout {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _win, cx| {
                let effect = this.app.update(cx, |app, _| app.popout_key(ev));
                if effect == Some(PopoutEffect::ActivateMain)
                    && let Some(main) = this.app.read(cx).main_window
                {
                    let _ = main.update(cx, |_, window, _| window.activate_window());
                }
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                let s = f64::from(this.renderer.scale);
                this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                this.modifiers = ev.modifiers;
                this.on_mouse_move(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    this.on_mouse_down(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    this.popout_press(MouseBtn::Right, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    this.popout_press(MouseBtn::Middle, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    if this.popout_release(MouseBtn::Left, cx) {
                        return;
                    }
                    this.selecting = false;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    this.popout_release(MouseBtn::Right, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.modifiers = ev.modifiers;
                    this.popout_release(MouseBtn::Middle, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _win, cx| {
                this.on_scroll(ev.delta, cx);
            }))
            .child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, _prepaint, window, cx| {
                        view.update(cx, |this, cx| {
                            this.paint(bounds, window, cx);
                        });
                    },
                )
                .size_full(),
            )
    }
}

/// Open the flyover popout window and store its handle on the [`App`].
/// Called by the frame pump when windowed mode wants a window up.
fn open_flyover_window(app: gpui::Entity<App>, cx: &mut GpuiApp) {
    let bounds = Bounds::centered(None, gpui::size(px(880.0), px(480.0)), cx);
    let app_for_view = app.clone();
    let app_for_close = app.clone();
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("pwrde — flyover".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            // The close button hides the window (sessions keep running);
            // ⌘` or the dock action bring it back.
            window.on_window_should_close(cx, move |_, cx| {
                let _ = app_for_close.update(cx, |app, _| {
                    app.flyover_window_visible = false;
                    app.flyover_window = None;
                });
                true
            });
            let scale = window.scale_factor();
            let cell_width = renderer::measure_cell_width(window, scale, renderer::FONT_SIZE);
            cx.new(|cx| FlyoverPopout {
                app: app_for_view.clone(),
                renderer: Renderer::new(scale, cell_width, 0, 0),
                focus_handle: cx.focus_handle(),
                cursor: (0.0, 0.0),
                modifiers: Modifiers::default(),
                selecting: false,
                mouse_report_buttons: 0,
                hot_rects: Vec::new(),
                ui_hover: None,
                scroll_accum: 0.0,
            })
        },
    );
    if let Ok(w) = handle {
        let _ = w.update(cx, |view, window, cx| {
            window.activate_window();
            let fh = view.focus_handle.clone();
            window.focus(&fh, cx);
        });
        let _ = app.update(cx, |app, _| app.flyover_window = Some(w));
    }
}

/// Percent-decode a URL component (`%20` → space, and so on).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Map an incoming open URL to a directory path, or `None` when it isn't one we
/// can open. Handles `file://` URLs (Finder "Open With" / `open -a`) and
/// `pwrde://open?cwd=<path>` deep links; a value that isn't an existing
/// directory is dropped.
fn parse_open_dir(url: &str) -> Option<std::path::PathBuf> {
    let path = if let Some(rest) = url.strip_prefix("file://") {
        // Skip an optional authority (empty or `localhost`) before the path.
        let slash = rest.find('/')?;
        percent_decode(&rest[slash..])
    } else if let Some(rest) = url.strip_prefix("pwrde://") {
        match rest
            .split_once('?')
            .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("cwd=")))
        {
            Some(cwd) => percent_decode(cwd),
            // Fall back to a bare path after the authority (`pwrde:///path`).
            None => percent_decode(&rest[rest.find('/')?..]),
        }
    } else {
        return None;
    };
    let path = std::path::PathBuf::from(path);
    path.is_dir().then_some(path)
}

/// macOS's accent color (System Settings → Appearance → Accent color) as sRGB:
/// `NSColor.controlAccentColor`, converted through the sRGB color space so the
/// catalog color resolves under the current appearance. `None` if AppKit
/// hands back nothing (or off macOS).
#[cfg(target_os = "macos")]
fn read_system_accent() -> Option<(u8, u8, u8)> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let color: *mut Object = msg_send![class!(NSColor), controlAccentColor];
        if color.is_null() {
            return None;
        }
        let space: *mut Object = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let color: *mut Object = msg_send![color, colorUsingColorSpace: space];
        if color.is_null() {
            return None;
        }
        let (mut r, mut g, mut b, mut a): (f64, f64, f64, f64) = (0.0, 0.0, 0.0, 0.0);
        let _: () = msg_send![color, getRed: &mut r as *mut f64 green: &mut g as *mut f64 blue: &mut b as *mut f64 alpha: &mut a as *mut f64];
        let ch = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some((ch(r), ch(g), ch(b)))
    }
}

#[cfg(not(target_os = "macos"))]
fn read_system_accent() -> Option<(u8, u8, u8)> {
    None
}

fn main() {
    // Settings must be in memory before anything reads a binding or theme.
    settings::init();
    // Install the Claude Code attention hooks (script + settings merge);
    // warns and continues on any failure, never blocks launch.
    claude_hooks::install();
    // At this gpui rev the platform lives in the gpui_platform crate; zed's own
    // main builds it the same way (current_platform → Application::with_platform).
    let platform = gpui_platform::current_platform(false);
    // macOS's default keeps the process alive after the last window closes
    // (document-app convention); a single-window terminal should just quit.
    let app = Application::with_platform(platform)
        .with_assets(ui::assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed);

    let (events_tx, events_rx) = mpsc::channel::<TermEvent>();
    // Command bus: listen before any session spawns so child shells inherit
    // `PWRDE_SOCKET` and `pwrde-cli` inside a tab targets this instance.
    bus_exec::start(events_tx.clone());
    // React to a directory arriving from outside the app. Registered on the
    // Application before `run` so a cold-launch `application:openURLs:`
    // (delivered just after launch) isn't missed — gpui drops the event when no
    // callback is set yet. Both file:// (Finder "Open With" / `open -a`) and
    // pwrde:// deep links land here; the drain loop opens each as a group once
    // the window is up.
    app.on_open_urls({
        let tx = events_tx.clone();
        move |urls| {
            for url in urls {
                if let Some(cwd) = parse_open_dir(&url) {
                    let _ = tx.send(TermEvent::OpenDir { cwd });
                }
            }
        }
    });

    app.run(move |cx: &mut GpuiApp| {
        // Seed the rcn Theme global before any window opens so Theme::of
        // never panics; the overlays re-sync it each frame from chrome tokens.
        cx.set_global(ui::theme::Theme::from_chrome(crate::theme::current()));
        crate::ui::Input::register_key_bindings(cx);
        // Initialize gpui-component (theme + text/textarea key bindings).
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, gpui::size(px(1200.0), px(720.0)), cx);

        // `pwrde /some/dir` from a shell: open the argv path the same way.
        if let Some(arg) = std::env::args().nth(1) {
            let path = std::path::PathBuf::from(&arg);
            if path.is_dir() {
                let _ = events_tx.send(TermEvent::OpenDir { cwd: path });
            }
        }

        let main_window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // A transparent native titlebar keeps OS edge-resize and the
                // native traffic-light buttons, while our content draws under a
                // full-size content view. `app_owns_titlebar_drag` stops AppKit
                // from dragging the window off the whole top bar — we move it
                // ourselves from the sidebar strip via `start_window_move`, so
                // dragging a tile's tab no longer moves the window.
                titlebar: Some(gpui::TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    // Inside the sidebar panel, not on the window gutter;
                    // `App::render` moves them when the sidebar collapses.
                    traffic_light_position: Some(gpui::point(
                        px(workspace::TRAFFIC_LIGHT_ORIGIN),
                        px(workspace::TRAFFIC_LIGHT_ORIGIN),
                    )),
                }),
                is_resizable: true,
                app_owns_titlebar_drag: true,
                // Opaque: every glass surface blurs *in-app* content (`backdrop.rs`);
                // window vibrancy would show the desktop through any
                // transparent pixel and fight that effect.
                window_background: gpui::WindowBackgroundAppearance::Opaque,
                ..Default::default()
            },
            |window, cx| {
                #[cfg(target_os = "macos")]
                {
                    attach_empty_toolbar(window);
                    hide_titlebar_decoration(window);
                }

                let scale = window.scale_factor();
                // Measure a monospace cell at the default font size; the first
                // paint re-measures against the live terminal/chrome font
                // settings and reflows if they differ.
                let cell_width = renderer::measure_cell_width(window, scale, renderer::FONT_SIZE);
                let phys_w = (f32::from(window.viewport_size().width) * scale) as u32;
                let phys_h = (f32::from(window.viewport_size().height) * scale) as u32;
                let renderer = Renderer::new(scale, cell_width, phys_w.max(1), phys_h.max(1));

                let entity = cx.new(|cx| {
                    let tools = cli_tools::tools();
                    let mut app = App {
                        events_rx,
                        events_tx: events_tx.clone(),
                        renderer,
                        workspaces: Vec::new(),
                        active: 0,
                        sections: Vec::new(),
                        next_section_id: 0,
                        git_contexts: git_context::GitContextCache::new(),
                        git_ctx_busy: std::sync::Arc::new(
                            std::sync::atomic::AtomicBool::new(false),
                        ),
                        open_pr_busy: std::sync::Arc::new(
                            std::sync::atomic::AtomicBool::new(false),
                        ),
                        git_ctx_pending: false,
                        // Backdated a full interval so the first foreground
                        // drain walks the groups immediately. Starting at
                        // `now()` would leave every card showing its bare
                        // title for the first `GIT_CTX_POLL` after launch,
                        // which is exactly when the user is looking at them.
                        git_ctx_polled_at: std::time::Instant::now()
                            .checked_sub(GIT_CTX_POLL)
                            .unwrap_or_else(std::time::Instant::now),
                        proc_title_busy: std::sync::Arc::new(
                            std::sync::atomic::AtomicBool::new(false),
                        ),
                        // Backdated for the same reason: restored panes are
                        // named on the first drain rather than after a beat of
                        // showing the stock "wezterm".
                        proc_title_polled_at: std::time::Instant::now()
                            .checked_sub(PROC_TITLE_POLL)
                            .unwrap_or_else(std::time::Instant::now),
                        proc_title_refreshed_at: std::time::Instant::now(),
                        next_session_id: 0,
                        next_tile_id: 0,
                        next_webview_id: 0,
                        webviews: webview::Manager::default(),
                        webview_prompt: None,
                        webview_address: cx.new(|cx| {
                            let mut input = crate::ui::Input::new(cx);
                            input.set_bare(true);
                            input
                        }),
                        webview_address_for: None,
                        webview_find: cx.new(|cx| {
                            let mut input = crate::ui::Input::new(cx);
                            input.set_bare(true);
                            input.placeholder("Find in page…");
                            input
                        }),
                        webview_find_for: None,
                        webview_panel: None,
                        sidebar_expanded_w: workspace::SIDEBAR_DEFAULT_W,
                        sidebar_collapsed: false,
                        folders_open: settings::get_bool("sidebar.folders", true),
                        // Restored here so the first frame (and the bus
                        // snapshot) already carry the persisted filter; the
                        // persistence load validates it against the
                        // sections once they exist.
                        folder_filter: settings::get_str("sidebar.folder")
                            .and_then(|s| s.parse::<u64>().ok()),
                        folders_w: workspace::FOLDERS_CARD_W,
                        sessions_scroll: 0.0,
                        folders_scroll: 0.0,
                        tools_collapsed: settings::get_bool("sidebar.tools_collapsed", false),
                        pinned_collapsed: settings::get_bool("sidebar.pinned_collapsed", false),
                        traffic_lights_for: None,
                        modifiers: Modifiers::default(),
                        mouse_report: None,
                        title: String::new(),
                        cursor: (0.0, 0.0),
                        drag: Drag::None,
                        just_expanded: None,
                        pending_group_profile: None,
                        pending_group_section: None,
                        save_ws: None,
                        message: None,
                        confirm: None,
                        pending_primary_cmd: std::collections::HashMap::new(),
                        command_input: {
                            let seed = settings::primary_command();
                            cx.new(|cx| {
                                let mut input = crate::ui::Input::new(cx);
                                input.set_text(seed, cx);
                                input
                            })
                        },
                        settings_query: String::new(),
                        settings_search: {
                            let input = cx.new(|cx| {
                                let mut input = crate::ui::Input::new(cx);
                                input.set_bare(true);
                                input.placeholder(sidebar_ui::SEARCH_SETTINGS_PLACEHOLDER);
                                input
                            });
                            cx.observe(&input, |this: &mut App, input, cx| {
                                let text = input.read(cx).text().to_string();
                                if this.settings_query != text {
                                    this.settings_query = text;
                                    this.request_redraw();
                                    cx.notify();
                                }
                            })
                            .detach();
                            input
                        },
                        save_name: {
                            let input = cx.new(|cx| {
                                let mut input = crate::ui::Input::new(cx);
                                input.set_bare(true);
                                input
                            });
                            cx.observe(&input, |this: &mut App, input, cx| {
                                let text = input.read(cx).text().to_string();
                                if let Some(m) = this.save_ws.as_mut()
                                    && m.name != text
                                {
                                    m.name = text;
                                    this.request_redraw();
                                    cx.notify();
                                }
                            })
                            .detach();
                            input
                        },
                        save_desc: {
                            let input = cx.new(|cx| {
                                let mut input = crate::ui::Input::new(cx);
                                input.set_bare(true);
                                input
                            });
                            cx.observe(&input, |this: &mut App, input, cx| {
                                let text = input.read(cx).text().to_string();
                                if let Some(m) = this.save_ws.as_mut()
                                    && m.description != text
                                {
                                    m.description = text;
                                    this.request_redraw();
                                    cx.notify();
                                }
                            })
                            .detach();
                            input
                        },
                        save_sync: false,
                        command: None,
                        command_scroll: gpui::ScrollHandle::new(),
                        command_scroll_to: None,
                        modal_search: {
                            let input = cx.new(|cx| {
                                let mut input = crate::ui::Input::new(cx);
                                input.set_bare(true);
                                input
                            });
                            cx.observe(&input, |this: &mut App, input, cx| {
                                let text = input.read(cx).text().to_string();
                                // The palette's current stage takes the query.
                                let changed = if let Some(p) = this.command.as_mut() {
                                    p.set_query(&text);
                                    true
                                } else if let Some(prompt) = this.webview_prompt.as_mut() {
                                    prompt.query = text;
                                    prompt.error = None;
                                    true
                                } else {
                                    false
                                };
                                if changed {
                                    this.request_redraw();
                                    cx.notify();
                                }
                            })
                            .detach();
                            input
                        },
                        modal_search_reset: None,
                        settings_search_focus: false,
                        editing_section: None,
                        // Single focus handle, minted once; focused below.
                        focus_handle: cx.focus_handle(),
                        dirty: true,
                        scroll_accum: 0.0,
                        page: Page::Sessions,
                        section: Section::Keyboard,
                        recording: None,
                        resize_hover: None,
                        tool_sessions: (0..tools.len()).map(|_| None).collect(),
                        tools,
                        tool_form: ToolForm {
                            name: cx.new(crate::ui::Input::new),
                            command: cx.new(crate::ui::Input::new),
                            cwd: cx.new(crate::ui::Input::new),
                            icon: cx.new(crate::ui::Input::new),
                        },
                        link_hover: None,
                        hot_rects: Vec::new(),
                        ui_hover: None,
                        flyover_tabs: Vec::new(),
                        flyover_active: 0,
                        flyover_open: false,
                        flyover_anim: 0.0,
                        flyover_focused: false,
                        flyover_height_frac: settings::get_str("flyover.height")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(workspace::FLYOVER_DEFAULT_FRAC),
                        flyover_maximized: false,
                        flyover_windowed: false,
                        flyover_window_visible: false,
                        flyover_window: None,
                        main_window: None,
                        pending_keys: Vec::new(),
                        preview_dark: theme::dark_active(),
                        appearance_menu: None,
                        git_cwd_cache: Default::default(),
                        flow: crate::flow::FlowState::default(),
                        flow_backends: std::collections::HashMap::new(),
                        flow_composer: None,
                        glass_backdrop: None,
                        glass_backdrop_key: 0,
                        glass_backdrop_size: (0.0, 0.0),
                        _lfg_events_child: crate::lfg::spawn_event_stream(events_tx.clone()),
                    };
                    // With persistence on, reattach to the previous session's
                    // groups; otherwise launch into the empty state — no shell
                    // is spawned until the user starts a group (CTA or ⇧⌘T).
                    if !(settings::get_bool("terminal.persist", false)
                        && app.restore_workspaces())
                    {
                        app.workspaces.push(Workspace::placeholder());
                    }
                    app.sync_layout();
                    // Cards launch empty otherwise: without a first walk the
                    // sidebar shows the bare `Workspace::title()` fallback —
                    // no branch or diffstat — until the user happens to switch
                    // groups. This only enqueues the serial worker.
                    app.spawn_git_context_refresh();

                    // Drain PTY wakeups on the foreground executor: poll the
                    // mpsc channel and notify when a redraw is needed. This
                    // preserves the old coalescing (begin_frame per paint).
                    let handle = cx.entity().downgrade();
                    cx.spawn(async move |_this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(Duration::from_millis(16))
                                .await;
                            let Some(app) = handle.upgrade() else { break };
                            let (redraw, want_popout, popout, (pending_keys, main)) =
                                app.update(cx, |app: &mut App, cx| {
                                    let redraw = app.drain_events();
                                    if redraw {
                                        cx.notify();
                                    }
                                    (
                                        redraw,
                                        app.flyover_windowed && app.flyover_window_visible,
                                        app.flyover_window,
                                        // Only dequeue once the main window
                                        // exists: keys pressed over the bus
                                        // during startup wait rather than
                                        // vanishing after their `queued` ack.
                                        match app.main_window.and_then(|w| w.downcast::<App>()) {
                                            Some(main) => (std::mem::take(&mut app.pending_keys), Some(main)),
                                            None => (Vec::new(), None),
                                        },
                                    )
                                });
                            // Keystrokes queued by the bus `key` command go
                            // through the main window's `on_key_down` (which
                            // needs a Window the entity update above lacks),
                            // so app-level bindings and overlay/flyover routing
                            // are exercised. This enters at the App handler,
                            // not gpui's focus tree: a focused child view
                            // (Settings Input, PR composer) is not reached.
                            if let (false, Some(main)) = (pending_keys.is_empty(), main) {
                                let n = pending_keys.len();
                                if main
                                    .update(cx, |app, window, cx| {
                                        for keystroke in pending_keys {
                                            let ev = KeyDownEvent {
                                                keystroke,
                                                is_held: false,
                                                prefer_character_input: false,
                                            };
                                            app.on_key_down(&ev, window, cx);
                                        }
                                        // No key-up follows a synthetic press:
                                        // clear the latched modifiers so the
                                        // next real click isn't read as ⌘-click.
                                        app.modifiers = Modifiers::default();
                                        cx.notify();
                                    })
                                    .is_err()
                                {
                                    eprintln!("bus key: main window gone, dropped {n} keystroke(s)");
                                }
                            }
                            // Reconcile the popout window with the desired
                            // state — window lifecycle stays here, on the
                            // foreground executor, so entity code never has
                            // to touch a window it doesn't own.
                            match (want_popout, popout) {
                                // Desired but not open: spawn it.
                                (true, None) => {
                                    let app_entity = app.clone();
                                    let _ =
                                        cx.update(|cx| open_flyover_window(app_entity, cx));
                                },
                                // Open but no longer desired: close it.
                                (false, Some(w)) => {
                                    let _ =
                                        w.update(cx, |_, window, _| window.remove_window());
                                    let _ = app.update(cx, |app: &mut App, _| {
                                        app.flyover_window = None;
                                    });
                                },
                                // Steady state: forward redraws to the popout.
                                (_, Some(w)) => {
                                    if redraw {
                                        let _ = w.update(cx, |_, _, cx| cx.notify());
                                    }
                                },
                                (false, None) => {},
                            }
                        }
                    })
                    .detach();

                    app
                });
                // Track macOS dark/light for the "System" appearance mode:
                // seed from the window's current appearance, then follow
                // changes live (themes re-resolve on the next paint).
                let is_dark = |a: gpui::WindowAppearance| {
                    matches!(
                        a,
                        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
                    )
                };
                theme::set_system_dark(is_dark(window.appearance()));
                window
                    .observe_window_appearance({
                        let entity = entity.clone();
                        move |window, cx| {
                            theme::set_system_dark(is_dark(window.appearance()));
                            // AppKit tunes the accent per polarity too.
                            theme::set_system_accent(read_system_accent());
                            entity.update(cx, |app, cx| {
                                app.request_redraw();
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                // Follow the macOS accent color for the "System" accent
                // setting: seed now, then re-read whenever the window regains
                // focus — the user changed it in System Settings and came
                // back. No AppKit notification is wired for it.
                theme::set_system_accent(read_system_accent());
                entity.update(cx, |_, cx| {
                    cx.observe_window_activation(window, |app, window, cx| {
                        if window.is_window_active() {
                            theme::set_system_accent(read_system_accent());
                            app.request_redraw();
                            cx.notify();
                        }
                    })
                    .detach();
                });

                // Establish keyboard focus so key events reach the terminal.
                let handle = entity.read(cx).focus_handle.clone();
                window.focus(&handle, cx);
                entity
            },
        )
        .expect("open window");
        // Remember the main window so popout flows can bring it forward.
        let _ = main_window.update(cx, |app, _, _| app.main_window = Some(main_window.into()));

        cx.activate(true);
    });
}

#[cfg(test)]
mod key_to_bytes_tests {
    use super::*;

    fn ks(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: None }
    }

    const SHIFT: Modifiers = Modifiers {
        shift: true,
        control: false,
        alt: false,
        platform: false,
        function: false,
    };
    const CTRL: Modifiers = Modifiers { control: true, shift: false, ..SHIFT };
    const ALT: Modifiers = Modifiers { alt: true, shift: false, ..SHIFT };

    #[test]
    fn unmodified_named_keys_keep_plain_sequences() {
        let none = Modifiers::default();
        assert_eq!(key_to_bytes(&ks("tab", none)).unwrap(), b"\t");
        assert_eq!(key_to_bytes(&ks("up", none)).unwrap(), b"\x1b[A");
        assert_eq!(key_to_bytes(&ks("home", none)).unwrap(), b"\x1b[H");
        assert_eq!(key_to_bytes(&ks("pageup", none)).unwrap(), b"\x1b[5~");
        assert_eq!(key_to_bytes(&ks("delete", none)).unwrap(), b"\x1b[3~");
    }

    #[test]
    fn shift_tab_sends_backtab() {
        assert_eq!(key_to_bytes(&ks("tab", SHIFT)).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn modified_csi_letter_keys_encode_xterm_params() {
        assert_eq!(key_to_bytes(&ks("up", SHIFT)).unwrap(), b"\x1b[1;2A");
        assert_eq!(key_to_bytes(&ks("left", ALT)).unwrap(), b"\x1b[1;3D");
        assert_eq!(key_to_bytes(&ks("right", CTRL)).unwrap(), b"\x1b[1;5C");
        let ctrl_shift = Modifiers { shift: true, ..CTRL };
        assert_eq!(key_to_bytes(&ks("end", ctrl_shift)).unwrap(), b"\x1b[1;6F");
    }

    #[test]
    fn modified_tilde_keys_encode_xterm_params() {
        assert_eq!(key_to_bytes(&ks("delete", SHIFT)).unwrap(), b"\x1b[3;2~");
        assert_eq!(key_to_bytes(&ks("pageup", CTRL)).unwrap(), b"\x1b[5;5~");
        assert_eq!(key_to_bytes(&ks("pagedown", ALT)).unwrap(), b"\x1b[6;3~");
    }
}

#[cfg(test)]
mod capture_profile_tests {
    use super::*;
    use crate::term::Session;

    fn tile_with_tabs(id: u64, n: usize, active: usize) -> Tile {
        let mut tile = Tile::empty(id);
        for _ in 0..n {
            tile.tabs.push(Tab::new(Session::placeholder()));
        }
        tile.active = active;
        tile
    }

    /// Capturing a live split tree preserves dirs, ratios, tab counts and the
    /// active index; placeholder sessions (no child pid) capture as bare
    /// shells, and the result serializes in the `.pwrspace.json` format.
    #[test]
    fn capture_preserves_tree_shape_and_roundtrips() {
        let root = Node::Split {
            dir: Dir::Row,
            ratio: 0.3,
            a: Box::new(Node::Leaf(tile_with_tabs(1, 1, 0))),
            b: Box::new(Node::Leaf(tile_with_tabs(2, 2, 1))),
        };
        let captured = capture_profile_node(&root);

        let pwrspace::ProfileNode::Split(split) = &captured else {
            panic!("expected split at root");
        };
        assert!(matches!(split.split, pwrspace::SplitDir::Row));
        assert!((split.ratio - 0.3).abs() < 1e-6);
        let pwrspace::ProfileNode::Leaf(a) = split.a.as_ref() else { panic!("leaf a") };
        assert_eq!(a.tabs.len(), 1);
        assert_eq!(a.active, 0);
        let pwrspace::ProfileNode::Leaf(b) = split.b.as_ref() else { panic!("leaf b") };
        assert_eq!(b.tabs.len(), 2);
        assert_eq!(b.active, 1);
        assert!(
            b.tabs.iter().all(|t| t.command.is_none()),
            "placeholder panes capture as bare shells"
        );

        // The captured tree round-trips through the on-disk JSON format.
        let json = serde_json::to_string(&captured).unwrap();
        assert!(json.contains(r#""split":"row""#), "split dir serialized: {json}");
        let back: pwrspace::ProfileNode = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, pwrspace::ProfileNode::Split(_)));
    }
}

#[cfg(test)]
mod open_url_tests {
    use super::{parse_open_dir, percent_decode};

    #[test]
    fn percent_decode_handles_spaces_and_literals() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("/no/encoding"), "/no/encoding");
        // A stray, incomplete escape is left untouched.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn file_url_resolves_to_dir() {
        let tmp = std::env::temp_dir();
        let url = format!("file://{}", tmp.to_string_lossy());
        assert_eq!(parse_open_dir(&url).as_deref(), Some(tmp.as_path()));
    }

    #[test]
    fn pwrde_scheme_reads_cwd_query() {
        let tmp = std::env::temp_dir();
        let url = format!("pwrde://open?cwd={}", tmp.to_string_lossy());
        assert_eq!(parse_open_dir(&url).as_deref(), Some(tmp.as_path()));
    }

    #[test]
    fn non_dir_and_foreign_scheme_are_dropped() {
        assert_eq!(parse_open_dir("file:///no/such/path/here"), None);
        assert_eq!(parse_open_dir("https://example.com"), None);
    }
}

#[cfg(test)]
mod proc_title_tests {
    use super::{PROC_TITLE_POLL, PROC_TITLE_REFRESH, proc_title_due};
    use std::time::Duration;

    /// Nothing is due until the fast tick elapses, however stale the slow one.
    #[test]
    fn no_sweep_before_the_fast_tick() {
        assert_eq!(proc_title_due(Duration::ZERO, PROC_TITLE_REFRESH), None);
        assert_eq!(
            proc_title_due(PROC_TITLE_POLL - Duration::from_millis(1), PROC_TITLE_REFRESH),
            None
        );
    }

    /// The fast tick sweeps only the nameless panes until the slow tick is
    /// also due, at which point the already-named ones are re-resolved too.
    #[test]
    fn renew_rides_the_slow_tick() {
        assert_eq!(proc_title_due(PROC_TITLE_POLL, Duration::ZERO), Some(false));
        assert_eq!(
            proc_title_due(PROC_TITLE_POLL, PROC_TITLE_REFRESH - Duration::from_millis(1)),
            Some(false)
        );
        assert_eq!(proc_title_due(PROC_TITLE_POLL, PROC_TITLE_REFRESH), Some(true));
    }
}

/// Hand a URL to the default browser (macOS `open`), fire-and-forget.
fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}
