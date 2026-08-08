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

mod claude_hooks;
mod cleanup;
mod git;
mod links;
mod pages;
mod palette;
mod persist;
mod picker;
mod pwrspace;
mod rect;
mod renderer;
mod settings;
mod term;
mod term_theme;
mod theme;
mod workspace;

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use gpui::{
    canvas, div, px, App as GpuiApp, AppContext, Application, Bounds, Context, CursorStyle,
    FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, Keystroke, Modifiers, MouseButton,
    ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, QuitMode, Render, ShapedLine,
    Size, Styled, TextAlign, TextRun, Window, WindowBounds, WindowOptions,
};

use pages::{Action, Binding, Page, Section};
use renderer::Renderer;
use term::{Session, TermEvent};
use workspace::{Dir, Node, Tab, Tile, Workspace};

// ── Layout / interaction constants (logical px) ──────────────────────────

/// Grab tolerance (logical px) for divider / sidebar-edge hits.
const GRAB: f32 = 4.0;
/// Pointer travel (logical px) before a tab press becomes a drag.
const DRAG_THRESHOLD: f64 = 6.0;

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
    /// (len = append), assigning `section` membership.
    SidebarInsert { before: usize, section: Option<u64> },
    /// Drop on the middle of a group row: join its section or create one.
    SidebarJoin { target: usize },
    /// Drop on the middle (or collapsed bottom) of a section header → append.
    SidebarAppend { section_id: u64 },
    /// Move a dragged section block so it starts at top-level `dest_start`.
    SectionMove { dest_start: usize },
    /// Reorder an empty section among the trailing empty headers.
    EmptySectionMove { to_idx: usize },
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
            ConfirmAction::CleanupDelete { .. } => "Delete",
        }
    }
}

/// What the confirm dialog's accept button performs.
enum ConfirmAction {
    /// Close the group whose primary pane the user asked to close. Resolved
    /// by primary tile id at confirm time so group reordering while the
    /// dialog is up can't misdirect the close.
    CloseGroup { primary_tile: u64 },
    /// Delete the selected drop worktrees, grouped `(repo_root, ids)` the way
    /// `drop rm` wants them.
    CleanupDelete { targets: Vec<(String, Vec<String>)> },
}

/// Discriminates who the directory/fork picker is being opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerTarget {
    /// Opening a new group in the workspace tree (default).
    Group,
    /// Opening a new tab in the flyover panel.
    Flyover,
}

/// A side effect [`App::popout_key`] needs applied to a window other than
/// the popout itself (entity code can't touch foreign windows directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopoutEffect {
    /// Bring the main window forward (the picker and the docked panel
    /// render there).
    ActivateMain,
}

/// What confirming the workspace-profile picker continues into: the picker is
/// interposed *before* the group exists (and, for a `drop` fork, before the
/// worktree is even provisioned), so the pending creation is carried here.
enum ProfileNext {
    /// Open the group directly at `cwd`.
    Open { name: String, cwd: std::path::PathBuf },
    /// Provision a `drop` worktree in `repo` (off `from`), then open it.
    Fork { repo: std::path::PathBuf, name: String, from: Option<String> },
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
    /// Resizing a Cleanup-table column boundary.
    CleanupColumn { boundary: usize },
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
    /// The flyover panel's top edge is being dragged to resize it.
    FlyoverResize,
    /// Sidebar group row pressed; may become a group drag past threshold.
    GroupPress { ws: usize, start: (f64, f64) },
    /// Dragging a sidebar workspace group tab.
    Group { ws: usize },
    /// Section header pressed; may become a section drag. `click_count` is
    /// preserved so mouse-up without a drag can still rename on double-click.
    SectionPress { section_id: u64, start: (f64, f64), click_count: usize },
    /// Dragging a whole section (header + contiguous member block).
    Section { section_id: u64 },
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
    next_session_id: u64,
    next_tile_id: u64,
    /// Sidebar width when expanded, logical px (user-resizable).
    sidebar_expanded_w: f32,
    /// Whether the sidebar is collapsed (⌘S toggle). Session-only, like the
    /// width; layout treats the effective width 0 as the collapsed state.
    sidebar_collapsed: bool,
    modifiers: Modifiers,
    title: String,
    cursor: (f64, f64),
    drag: Drag,
    /// Tile expanded by the first click of a potential double-click. The
    /// second click's bar double-click-to-collapse is suppressed for it, so
    /// double-clicking a collapsed pane's tab doesn't snap it shut again.
    just_expanded: Option<u64>,
    /// The open step-1 cwd picker popover, or `None` when closed.
    picker: Option<picker::Picker>,
    /// The open step-2 fork-source picker (git repos only), or `None`.
    fork: Option<picker::ForkPicker>,
    /// The open workspace-profile picker (shown only when `.pwrspace.json`
    /// profiles were discovered for the group being created), or `None`.
    profile_picker: Option<picker::ProfilePicker>,
    /// The creation the profile picker resolves into; set with `profile_picker`.
    profile_next: Option<ProfileNext>,
    /// Profile chosen for a group whose `drop` worktree is still provisioning;
    /// applied on `GroupReady`, dropped on `GroupFailed`.
    pending_group_profile: Option<pwrspace::WorkspaceProfile>,
    /// The open save-as-workspace modal, or `None`.
    save_ws: Option<SaveWorkspaceModal>,
    /// The open command palette, or `None` when closed.
    palette: Option<palette::Palette>,
    /// A centered one-line message. `bool` is `dismissable`: false while `drop`
    /// provisions (input swallowed), true for a failure note the user can close.
    message: Option<(String, bool)>,
    /// The open close-primary-pane confirmation dialog, or `None`.
    confirm: Option<ConfirmClose>,
    /// Primary-pane sessions awaiting their auto-run command, keyed by session
    /// id. The command is written on the session's first wakeup (the shell has
    /// printed its prompt by then, so startup files can't eat the input).
    pending_primary_cmd: std::collections::HashMap<u64, String>,
    /// In-progress edit buffer for the Settings → Sessions primary-command
    /// row, or `None` when not editing.
    editing_command: Option<String>,
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
    /// Cleanup page state (worktree listing, selection, filter, scroll).
    cleanup: cleanup::Cleanup,
    /// The active top-level page (Sessions / Settings).
    page: Page,
    /// The active section while the Settings page is up.
    section: Section,
    /// Keyboard-page row currently capturing a new binding, if any.
    recording: Option<Action>,
    /// Dot↔glyph crossfade progress per page slot (0..1), advanced each tick
    /// toward 1 for the hovered/active slot and 0 otherwise.
    dot_anim: Vec<f32>,
    /// Page slot currently under the pointer.
    dot_hover: Option<usize>,
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
    /// Who the picker/fork picker is currently targeting.
    picker_target: PickerTarget,
}

impl App {
    fn scale(&self) -> f32 {
        self.renderer.scale
    }

    /// Effective sidebar width for layout/hit-testing: 0 while collapsed
    /// (`workspace` geometry treats 0 as collapsed), else the user's width.
    fn sidebar_w(&self) -> f32 {
        if self.sidebar_collapsed { 0.0 } else { self.sidebar_expanded_w }
    }

    fn dpi(&self) -> u32 {
        (96.0 * self.scale()) as u32
    }

    /// Cell size in physical px, rounded for the PTY resize (u16).
    fn cell_px(&self) -> (u16, u16) {
        (self.renderer.cell_width as u16, self.renderer.cell_height as u16)
    }

    fn spawn_session(&mut self) -> Session {
        let cwd = self.workspaces.get(self.active).and_then(|ws| ws.cwd.clone());
        self.spawn_session_in(cwd.as_deref())
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
            };
            ws.fix_focus();
            self.workspaces.push(ws);
        }
        self.active = 0;
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
                for st in &saved {
                    let tab_cwd = st.cwd.as_ref().map(std::path::PathBuf::from);
                    let session = self
                        .spawn_session_named(tab_cwd.as_deref().or(cwd), st.shpool_session.clone());
                    let mut tab = Tab::new(session);
                    tab.unread = st.unread;
                    restored.tabs.push(tab);
                }
                restored.active = saved.iter().position(|st| st.active).unwrap_or(0);
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

    /// Physical-pixel terminal area (excludes the sidebar).
    fn area(&self) -> workspace::LayoutRect {
        let (w, h) = self.renderer.surface_size();
        workspace::terminal_area(w, h, self.scale(), self.sidebar_w())
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
        self.sync_layout_impl(false);
        self.sync_flyover_layout(false);
    }

    /// `force` pushes a PTY resize even when cols/rows are unchanged — needed
    /// after a display-scale change, where the cell pixel size and dpi moved
    /// but the grid dimensions may not have.
    fn sync_layout_impl(&mut self, force: bool) {
        let scale = self.scale();
        let (cw, ch) = self.cell_px();
        let dpi = self.dpi();
        let area = self.area();
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
                        tab.session.resize(cols, rows, cw, ch, dpi);
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
                tab.session.resize(cols, rows, cw, ch, dpi);
            }
        }
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
            self.active = wi;
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
        if let Some(name) = tab.session.shpool_session.as_deref() {
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
                if let Some(name) = tab.session.shpool_session.as_deref() {
                    term::shpool_kill(name);
                }
            }
        }
        let closed_section = self.workspaces.get(wi).and_then(|w| w.section);
        if self.workspaces.len() > 1 {
            self.workspaces.remove(wi);
            if self.active >= self.workspaces.len() {
                self.active = self.workspaces.len() - 1;
            }
        } else {
            self.reset_empty_workspace(0);
        }
        // Drop the section once its last member group is gone.
        if let Some(sid) = closed_section {
            workspace::prune_section_if_empty(
                &mut self.sections,
                &self.workspaces,
                sid,
            );
        }
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
            ConfirmAction::CleanupDelete { targets } => {
                let count: usize = targets.iter().map(|(_, ids)| ids.len()).sum();
                self.message = Some((format!("Deleting {count} worktree(s)…"), false));
                let tx = self.events_tx.clone();
                std::thread::spawn(move || {
                    let (removed, failed, error) = run_drop_rm(targets);
                    let _ = tx.send(TermEvent::CleanupRemoved { removed, failed, error });
                });
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
            || self.fork.is_some()
            || self.picker.is_some()
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
                    if let Some(tab) = self.flyover_tabs.get(self.flyover_active) {
                        let session = &tab.session;
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
        // Only the Sessions page has terminals to scroll; Cleanup has its own
        // scroll handling below.
        if self.page != Page::Sessions && self.page != Page::Cleanup {
            return;
        }
        // Cleanup page: scroll the worktree table.
        if self.page == Page::Cleanup {
            let scale = self.scale();
            let area = self.area();
            let notches = match delta {
                gpui::ScrollDelta::Lines(p) => p.y as f64,
                gpui::ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / (cell_height as f64 * 3.0),
            };
            let steps = scroll_steps(&mut self.scroll_accum, notches);
            if steps != 0 {
                let fit = cleanup::rows_that_fit(&area, scale);
                let max_scroll = self.cleanup.rows().len().saturating_sub(fit);
                let offset = self.cleanup.scroll as i64 - steps as i64;
                self.cleanup.scroll = offset.clamp(0, max_scroll as i64) as usize;
                self.request_redraw();
            }
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
        let session = &tab.session;
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
        let Some(text) = tab.session.selected_text() else { return };
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    }

    fn paste(&mut self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        let ws = &self.workspaces[self.active];
        let Some(tab) = ws.focused().and_then(|t| t.active_tab()) else { return };
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => tab.session.paste(&text),
            _ if clipboard.get_image().is_ok() => tab.session.write([0x16u8]),
            _ => return,
        }
        // Like typing: a paste follows the live output and drops any selection.
        tab.session.scroll_to_bottom();
        tab.session.clear_selection();
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
    fn flyover_toggle_maximized(&mut self) {
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
                    .find_map(|t| t.tabs.iter().find(|tab| tab.session.id == id))
                    .map(|tab| &tab.session)
            })
            .or_else(|| {
                self.flyover_tabs.iter().find(|tab| tab.session.id == id).map(|tab| &tab.session)
            })
    }

    /// True when the session is the active tab of the flyover and the flyover
    /// is on screen — panel open, or popout window showing. On screen
    /// regardless of which page is showing, since both surfaces overlay them.
    fn flyover_visible(&self, id: u64) -> bool {
        let showing =
            if self.flyover_windowed { self.flyover_window_visible } else { self.flyover_open };
        showing
            && self.flyover_tabs.get(self.flyover_active).is_some_and(|tab| tab.session.id == id)
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
            .any(|t| !t.collapsed && t.active_tab().is_some_and(|tab| tab.session.id == id))
            || self.flyover_visible(id)
    }

    /// Mark the tab owning session `id` unread. Returns true (and persists)
    /// only on a false→true transition, so repeated attention signals from
    /// one pane don't churn the snapshot.
    fn set_unread_by_session(&mut self, id: u64) -> bool {
        // Flyover tabs aren't persisted, so their dots skip the snapshot.
        if let Some(tab) = self.flyover_tabs.iter_mut().find(|tab| tab.session.id == id) {
            let hit = !tab.unread;
            tab.unread = true;
            return hit;
        }
        let changed = self.workspaces.iter_mut().any(|ws| {
            ws.root.tiles_mut().into_iter().any(|t| {
                t.tabs.iter_mut().any(|tab| {
                    let hit = tab.session.id == id && !tab.unread;
                    if hit {
                        tab.unread = true;
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
        if self.page != Page::Sessions
            || self.confirm.is_some()
            || self.message.is_some()
            || self.fork.is_some()
            || self.picker.is_some()
        {
            return;
        }
        let scale = self.scale();
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (_, h) = self.renderer.surface_size();

        if workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
            for (ri, row) in rows.iter().enumerate() {
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    self.sidebar_w(),
                );
                if !rect.contains(px, py) {
                    continue;
                }
                if let workspace::SidebarRow::Group { ws_idx } = *row {
                    let ws = &mut self.workspaces[ws_idx];
                    let primary = ws.primary_tile;
                    if let Some(tab) =
                        ws.root.find_tile_mut(primary).and_then(|t| t.active_tab_mut())
                        && !tab.unread
                    {
                        tab.unread = true;
                        self.persist_snapshot();
                        self.request_redraw();
                    }
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
                    self.persist_snapshot();
                    self.request_redraw();
                }
            }
            return;
        }
    }

    // ── cwd picker ──────────────────────────────────────────────────────

    fn open_picker(&mut self) {
        self.picker_target = PickerTarget::Group;
        self.picker = Some(picker::Picker::new());
        self.request_redraw();
    }

    fn open_flyover_picker(&mut self) {
        self.picker_target = PickerTarget::Flyover;
        self.picker = Some(picker::Picker::new());
        self.request_redraw();
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
    fn toggle_flyover(&mut self) {
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
                    && let Some(text) = tab.session.selected_text()
                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                {
                    let _ = clipboard.set_text(text);
                }
            },
            Action::Paste => {
                if let Some(tab) = self.flyover_tabs.get(self.flyover_active)
                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                {
                    match clipboard.get_text() {
                        Ok(text) if !text.is_empty() => {
                            tab.session.paste(&text);
                            tab.session.scroll_to_bottom();
                            tab.session.clear_selection();
                        },
                        _ if clipboard.get_image().is_ok() => {
                            tab.session.write([0x16u8]);
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
        {
            tab.session.write(bytes);
            tab.session.scroll_to_bottom();
            tab.session.clear_selection();
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

    fn confirm_picker(&mut self) {
        let Some(picker) = self.picker.as_mut() else { return };
        let Some(entry) = picker.selected_entry().cloned() else {
            self.picker = None;
            // If this was the first-ever flyover open and user escaped, close
            // whichever surface was waiting on the first tab.
            if self.picker_target == PickerTarget::Flyover && self.flyover_tabs.is_empty() {
                self.flyover_open = false;
                self.flyover_focused = false;
                self.flyover_window_visible = false;
            }
            return;
        };
        picker.record_recent(&entry.path);
        let name = group_name(&entry.path);
        if self.picker_target == PickerTarget::Flyover {
            if entry.is_git {
                // Flyover git dirs: only RepoRoot and Worktree (no drop/create).
                let all_choices = build_fork_choices(&entry.path);
                let choices = all_choices
                    .into_iter()
                    .filter(|c| matches!(c.scope, picker::ForkScope::RepoRoot | picker::ForkScope::Worktree))
                    .collect::<Vec<_>>();
                self.fork = Some(picker::ForkPicker::new(entry.path, name, choices));
                self.picker = None;
            } else {
                self.picker = None;
                self.spawn_flyover_tab(Some(entry.path));
            }
        } else {
            self.picker = None;
            self.add_group_or_pick_profile(name, entry.path);
        }
    }

    /// Step 2: confirm the fork choice. "repo root"/"attach worktree" open the
    /// group directly; the forking scopes spawn `drop` on a worker thread and
    /// show a provisioning message until it reports back over `events_tx`.
    fn confirm_fork(&mut self) {
        let Some(picker) = self.fork.as_ref() else { return };
        let Some(entry) = picker.selected_entry() else { return };
        let repo = picker.repo.clone();
        let name = picker.name.clone();
        let scope = entry.scope;
        let from = entry.from.clone();
        let path = entry.path.clone();

        if self.picker_target == PickerTarget::Flyover {
            // Flyover only supports RepoRoot and Worktree (no drop/create).
            if matches!(scope, picker::ForkScope::RepoRoot | picker::ForkScope::Worktree) {
                self.fork = None;
                self.spawn_flyover_tab(path);
            } else {
                // Should not happen (filtered above), but be safe.
                self.fork = None;
                if self.flyover_tabs.is_empty() {
                    self.flyover_open = false;
                }
            }
            return;
        }

        // "repo root" and "attach worktree" skip drop entirely: open the group
        // directly in that directory (the repo, or the existing worktree).
        if matches!(scope, picker::ForkScope::RepoRoot | picker::ForkScope::Worktree) {
            self.fork = None;
            match path {
                Some(p) => self.add_group_or_pick_profile(name, p),
                None => self.add_group(name, None),
            }
            return;
        }

        // Forking scopes: the worktree doesn't exist yet, so profiles are
        // discovered at the repo root (plus user-level) and the choice is made
        // *before* provisioning; it is applied when `GroupReady` arrives.
        let found = pwrspace::discover(&pwrspace::candidate_paths(&repo));
        if !found.is_empty() {
            self.fork = None;
            self.profile_picker = Some(picker::ProfilePicker::new(name.clone(), found));
            self.profile_next = Some(ProfileNext::Fork { repo, name, from });
            self.request_redraw();
            return;
        }

        self.fork = None;
        self.start_fork(repo, name, from);
    }

    /// Spawn `drop` on a worker thread and show the provisioning message until
    /// it reports back over `events_tx` (the tail of the pre-profile
    /// `confirm_fork`, shared with the profile picker's deferred path).
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

    /// Create the group at `cwd` immediately, or interpose the profile picker
    /// when any `.pwrspace.json` profiles exist for it (dir → repo root → user
    /// precedence; worktree dirs also see their main checkout's profiles).
    fn add_group_or_pick_profile(&mut self, name: String, cwd: std::path::PathBuf) {
        let found = pwrspace::discover(&pwrspace::candidate_paths(&cwd));
        if found.is_empty() {
            self.add_group(name, Some(cwd));
        } else {
            self.profile_picker = Some(picker::ProfilePicker::new(name.clone(), found));
            self.profile_next = Some(ProfileNext::Open { name, cwd });
            self.request_redraw();
        }
    }

    /// Confirm the highlighted profile row: launch (or provision) the pending
    /// group with the chosen profile — the default row (`profile: None`)
    /// keeps today's single-pane behavior.
    fn confirm_profile(&mut self) {
        let Some(pp) = self.profile_picker.as_ref() else { return };
        let Some(entry) = pp.selected_entry().cloned() else { return };
        self.profile_picker = None;
        match self.profile_next.take() {
            Some(ProfileNext::Open { name, cwd }) => match entry.profile {
                Some(profile) => self.add_group_with_profile(name, Some(cwd), &profile),
                None => self.add_group(name, Some(cwd)),
            },
            Some(ProfileNext::Fork { repo, name, from }) => {
                self.pending_group_profile = entry.profile;
                self.start_fork(repo, name, from);
            },
            None => {},
        }
        self.request_redraw();
    }

    /// Escape/outside-click on the profile picker: step back to the picker it
    /// came from — the fork picker for a pending `drop` fork, else the dir
    /// picker (matching how the fork picker itself steps back).
    fn cancel_profile(&mut self) {
        self.profile_picker = None;
        match self.profile_next.take() {
            Some(ProfileNext::Fork { repo, name, .. }) => {
                let choices = build_fork_choices(&repo);
                self.fork = Some(picker::ForkPicker::new(repo, name, choices));
            },
            _ => self.picker = Some(picker::Picker::new()),
        }
        self.request_redraw();
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
        {
            self.pending_primary_cmd.insert(tab.session.id, cmd);
        }
        let ws = Workspace::new(name, tile, cwd);
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
            section: None,
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

    fn resolve_drop(&self, px: f32, py: f32) -> Option<DropTarget> {
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
                let t0 = workspace::tile_tab_rect(&strip, 0, n, scale, has_caret);
                let index = ((((px - t0.x).max(0.0)) / t0.w).floor() as usize).min(n);
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
        // Section headers of collapsed sections target the section's first
        // member when one exists; empty headers are ignored.
        let (_, h) = self.renderer.surface_size();
        if workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
            for (ri, row) in rows.iter().enumerate() {
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    self.sidebar_w(),
                );
                if !rect.contains(px, py) {
                    continue;
                }
                match *row {
                    workspace::SidebarRow::Group { ws_idx } => {
                        return Some(DropTarget::Group { ws: ws_idx });
                    },
                    workspace::SidebarRow::SectionHeader { section_idx } => {
                        let sid = self.sections[section_idx].id;
                        if let Some((start, _)) =
                            workspace::section_member_range(&self.workspaces, sid)
                        {
                            return Some(DropTarget::Group { ws: start });
                        }
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
        let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
        if rows.is_empty() {
            return Some(DropTarget::SidebarInsert { before: 0, section: None });
        }
        const EDGE: f32 = 0.28;
        for (ri, row) in rows.iter().enumerate() {
            let rect = workspace::sidebar_row_rect(
                &rows,
                ri,
                &self.workspaces,
                scale,
                self.sidebar_w(),
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
                    return Some(self.sidebar_insert_at(0, false));
                }
                continue;
            }
            let rel = (py - hit.y) / hit.h.max(1.0);
            match *row {
                workspace::SidebarRow::Group { ws_idx } => {
                    if rel < EDGE {
                        return Some(self.sidebar_insert_at(ws_idx, false));
                    }
                    if rel > 1.0 - EDGE {
                        return Some(self.sidebar_insert_at(ws_idx + 1, true));
                    }
                    return Some(DropTarget::SidebarJoin { target: ws_idx });
                },
                workspace::SidebarRow::SectionHeader { section_idx } => {
                    let sid = self.sections[section_idx].id;
                    let range = workspace::section_member_range(&self.workspaces, sid);
                    if rel < EDGE {
                        let before = range.map(|(s, _)| s).unwrap_or(self.workspaces.len());
                        return Some(DropTarget::SidebarInsert { before, section: None });
                    }
                    if rel > 1.0 - EDGE {
                        match range {
                            Some((start, _)) if !self.sections[section_idx].collapsed => {
                                return Some(DropTarget::SidebarInsert {
                                    before: start,
                                    section: Some(sid),
                                });
                            },
                            _ => return Some(DropTarget::SidebarAppend { section_id: sid }),
                        }
                    }
                    return Some(DropTarget::SidebarAppend { section_id: sid });
                },
            }
        }
        // Below the last row → append ungrouped.
        if let Some(last) = rows.len().checked_sub(1) {
            let rect = workspace::sidebar_row_rect(
                &rows,
                last,
                &self.workspaces,
                scale,
                self.sidebar_w(),
            );
            if py >= rect.y + rect.h {
                return Some(DropTarget::SidebarInsert {
                    before: self.workspaces.len(),
                    section: None,
                });
            }
        }
        None
    }

    /// Build a `SidebarInsert` for gap `before`, preferring the left neighbor's
    /// section when `prefer_left` (bottom-edge drop) is set.
    fn sidebar_insert_at(&self, before: usize, prefer_left: bool) -> DropTarget {
        let before = before.min(self.workspaces.len());
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
        DropTarget::SidebarInsert { before, section }
    }

    /// Resolve a section-header drag to a top-level insertion point.
    fn resolve_section_drop(&self, px: f32, py: f32, section_id: u64) -> Option<DropTarget> {
        let scale = self.scale();
        let (_, h) = self.renderer.surface_size();
        if !workspace::sidebar(h, scale, self.sidebar_w()).contains(px, py) {
            return None;
        }
        let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
        let own_range = workspace::section_member_range(&self.workspaces, section_id);
        let is_empty = own_range.is_none();
        if is_empty {
            // Empty sections only live at the trailing end — reorder among
            // empty headers by sections-vec index.
            let empties: Vec<usize> = self
                .sections
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    !self.workspaces.iter().any(|w| w.section == Some(s.id))
                })
                .map(|(i, _)| i)
                .collect();
            for (ri, row) in rows.iter().enumerate() {
                let workspace::SidebarRow::SectionHeader { section_idx } = *row else {
                    continue;
                };
                if !empties.contains(&section_idx) {
                    continue;
                }
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    self.sidebar_w(),
                );
                let side = workspace::sidebar(h, scale, self.sidebar_w());
                if py < rect.y || py >= rect.y + rect.h {
                    continue;
                }
                if px < side.x || px > side.x + side.w {
                    continue;
                }
                let rel = (py - rect.y) / rect.h.max(1.0);
                let pos_among = empties.iter().position(|&i| i == section_idx)?;
                let to = if rel < 0.5 { pos_among } else { pos_among + 1 };
                // Map "among empties" slot back to absolute sections index.
                let to_idx = if to >= empties.len() {
                    empties.last().copied().unwrap_or(section_idx) + 1
                } else {
                    empties[to]
                };
                return Some(DropTarget::EmptySectionMove { to_idx });
            }
            return None;
        }
        const EDGE: f32 = 0.4;
        // Top-level gaps: edges of ungrouped groups + section headers (not
        // interiors of foreign member runs).
        for (ri, row) in rows.iter().enumerate() {
            let rect = workspace::sidebar_row_rect(
                &rows,
                ri,
                &self.workspaces,
                scale,
                self.sidebar_w(),
            );
            let side = workspace::sidebar(h, scale, self.sidebar_w());
            if py < rect.y || py >= rect.y + rect.h {
                if ri == 0 && py < rect.y {
                    return Some(DropTarget::SectionMove { dest_start: 0 });
                }
                continue;
            }
            if px < side.x || px > side.x + side.w {
                continue;
            }
            let rel = (py - rect.y) / rect.h.max(1.0);
            match *row {
                workspace::SidebarRow::Group { ws_idx } => {
                    // Only ungrouped rows are top-level drop targets.
                    if self.workspaces[ws_idx].section.is_some() {
                        // Snap to the boundary of this foreign section.
                        if let Some(sid) = self.workspaces[ws_idx].section {
                            if sid == section_id {
                                return None; // over own members
                            }
                            if let Some((start, end)) =
                                workspace::section_member_range(&self.workspaces, sid)
                            {
                                let dest = if rel < 0.5 { start } else { end };
                                return Some(DropTarget::SectionMove { dest_start: dest });
                            }
                        }
                        continue;
                    }
                    let dest = if rel < 0.5 { ws_idx } else { ws_idx + 1 };
                    return Some(DropTarget::SectionMove { dest_start: dest });
                },
                workspace::SidebarRow::SectionHeader { section_idx } => {
                    let sid = self.sections[section_idx].id;
                    if sid == section_id {
                        return None;
                    }
                    let range = workspace::section_member_range(&self.workspaces, sid);
                    match range {
                        Some((start, end)) => {
                            let dest = if rel < EDGE { start } else { end };
                            return Some(DropTarget::SectionMove { dest_start: dest });
                        },
                        None => {
                            // Empty header at end — place block before trailing empties
                            // (i.e. at end of workspaces).
                            return Some(DropTarget::SectionMove {
                                dest_start: self.workspaces.len(),
                            });
                        },
                    }
                },
            }
        }
        if let Some(last) = rows.len().checked_sub(1) {
            let rect = workspace::sidebar_row_rect(
                &rows,
                last,
                &self.workspaces,
                scale,
                self.sidebar_w(),
            );
            if py >= rect.y + rect.h {
                return Some(DropTarget::SectionMove {
                    dest_start: self.workspaces.len(),
                });
            }
        }
        None
    }

    /// The translucent highlight rect for a resolved drop target, in physical
    /// px. Mirrors `resolve_drop`'s geometry so the preview matches the landing.
    fn drop_hint(&self, target: DropTarget) -> Option<workspace::LayoutRect> {
        let scale = self.scale();
        let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
        let line_h = (2.0 * scale).max(1.0);
        match target {
            DropTarget::Group { ws } => {
                // Find the group row rect via the shared list.
                for (ri, row) in rows.iter().enumerate() {
                    if let workspace::SidebarRow::Group { ws_idx } = *row {
                        if ws_idx == ws {
                            return Some(workspace::sidebar_row_rect(
                                &rows,
                                ri,
                                &self.workspaces,
                                scale,
                                self.sidebar_w(),
                            ));
                        }
                    }
                }
                Some(workspace::tab_rect(ws, scale, self.sidebar_w()))
            },
            DropTarget::SidebarJoin { target } => {
                for (ri, row) in rows.iter().enumerate() {
                    if let workspace::SidebarRow::Group { ws_idx } = *row {
                        if ws_idx == target {
                            return Some(workspace::sidebar_row_rect(
                                &rows,
                                ri,
                                &self.workspaces,
                                scale,
                                self.sidebar_w(),
                            ));
                        }
                    }
                }
                None
            },
            DropTarget::SidebarAppend { section_id } => {
                let section_idx = self.sections.iter().position(|s| s.id == section_id)?;
                for (ri, row) in rows.iter().enumerate() {
                    if let workspace::SidebarRow::SectionHeader { section_idx: si } = *row {
                        if si == section_idx {
                            return Some(workspace::sidebar_row_rect(
                                &rows,
                                ri,
                                &self.workspaces,
                                scale,
                                self.sidebar_w(),
                            ));
                        }
                    }
                }
                None
            },
            DropTarget::SidebarInsert { before, .. } => {
                // Thin insertion line at the gap before `before`.
                let y = self.sidebar_gap_y(before, &rows, scale);
                Some(self.sidebar_insert_line(y, &rows, scale, line_h))
            },
            DropTarget::SectionMove { dest_start } => {
                let y = self.sidebar_gap_y(dest_start, &rows, scale);
                Some(self.sidebar_insert_line(y, &rows, scale, line_h))
            },
            DropTarget::EmptySectionMove { to_idx } => {
                // Insertion line above the empty-header slot being targeted.
                let si = to_idx.min(self.sections.len().saturating_sub(1));
                for (ri, row) in rows.iter().enumerate() {
                    if let workspace::SidebarRow::SectionHeader { section_idx } = *row {
                        if section_idx == si {
                            let r = workspace::sidebar_row_rect(
                                &rows,
                                ri,
                                &self.workspaces,
                                scale,
                                self.sidebar_w(),
                            );
                            return Some(workspace::LayoutRect {
                                x: r.x,
                                y: r.y - line_h / 2.0,
                                w: r.w,
                                h: line_h,
                            });
                        }
                    }
                }
                None
            },
            DropTarget::TabBar { tile, .. }
            | DropTarget::Center { tile }
            | DropTarget::Edge { tile, .. } => {
                let area = self.area();
                let wsp = &self.workspaces[self.active];
                let (tiles, _) = workspace::layout_tiles(&wsp.root, area, scale);
                let (_, rect) = tiles.into_iter().find(|(id, _)| *id == tile)?;
                Some(match target {
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

    /// Y coordinate of the insertion gap before workspace index `before`.
    fn sidebar_gap_y(&self, before: usize, rows: &[workspace::SidebarRow], scale: f32) -> f32 {
        // Prefer the top of the first row whose group index >= before, or the
        // top of a section header whose first member >= before; else past last.
        for (ri, row) in rows.iter().enumerate() {
            let rect = workspace::sidebar_row_rect(
                rows,
                ri,
                &self.workspaces,
                scale,
                self.sidebar_w(),
            );
            match *row {
                workspace::SidebarRow::Group { ws_idx } if ws_idx >= before => {
                    return rect.y;
                },
                workspace::SidebarRow::SectionHeader { section_idx } => {
                    let sid = self.sections[section_idx].id;
                    if let Some((start, _)) =
                        workspace::section_member_range(&self.workspaces, sid)
                    {
                        if start >= before {
                            return rect.y;
                        }
                    } else if before >= self.workspaces.len() {
                        return rect.y;
                    }
                },
                _ => {},
            }
        }
        if let Some(last) = rows.len().checked_sub(1) {
            let rect = workspace::sidebar_row_rect(
                rows,
                last,
                &self.workspaces,
                scale,
                self.sidebar_w(),
            );
            return rect.y + rect.h;
        }
        // Empty sidebar: below the button row.
        let btn = workspace::new_group_button(scale, self.sidebar_w());
        btn.y + btn.h + (6.0 * scale).round()
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
            let r = workspace::sidebar_row_rect(rows, ri, &self.workspaces, scale, self.sidebar_w());
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
            DropTarget::SidebarInsert { .. }
            | DropTarget::SidebarJoin { .. }
            | DropTarget::SidebarAppend { .. }
            | DropTarget::SectionMove { .. }
            | DropTarget::EmptySectionMove { .. } => return,
            _ => {},
        }

        let Some(mut tab) = self.take_tab(self.active, src_tile, src_tab) else {
            return;
        };

        match target {
            DropTarget::TabBar { tile, index } => {
                if let Some(t) = self.workspaces[self.active].root.find_tile_mut(tile) {
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
        let old_section = self.workspaces[from].section;
        let mut created_section: Option<u64> = None;
        let new_idx = match target {
            DropTarget::SidebarInsert { before, section } => {
                workspace::relocate_workspace(&mut self.workspaces, from, before, section)
            },
            DropTarget::SidebarJoin { target } => {
                let (idx, created) = workspace::join_onto_group(
                    &mut self.workspaces,
                    &mut self.sections,
                    &mut self.next_section_id,
                    from,
                    target,
                );
                created_section = created;
                idx
            },
            DropTarget::SidebarAppend { section_id } => {
                workspace::append_to_section(&mut self.workspaces, from, section_id)
            },
            _ => return,
        };
        self.active =
            workspace::track_index_after_relocate(self.active, from, new_idx);
        // Prune the section the workspace left, if now empty.
        if let Some(sid) = old_section {
            workspace::prune_section_if_empty(&mut self.sections, &self.workspaces, sid);
        }
        if let Some(sid) = created_section {
            // Open rename on the freshly created section.
            if let Some(sec) = self.sections.iter().find(|s| s.id == sid) {
                let buf = if sec.emoji.is_empty() {
                    sec.name.clone()
                } else {
                    format!("{} {}", sec.emoji, sec.name)
                };
                self.editing_section = Some((sid, buf));
            }
        }
        workspace::ensure_active_section_expanded(
            &self.workspaces,
            &mut self.sections,
            self.active,
        );
        self.sync_layout();
        self.request_redraw();
        self.persist_snapshot();
    }

    /// Apply a section-header drag landing on `target`.
    fn apply_section_drop(&mut self, section_id: u64, target: DropTarget) {
        match target {
            DropTarget::SectionMove { dest_start } => {
                // Track the active workspace across the block move by its
                // primary tile (unique per group), not by index.
                let active_tile = self.workspaces.get(self.active).map(|w| w.primary_tile);
                if workspace::relocate_section_block(
                    &mut self.workspaces,
                    section_id,
                    dest_start,
                )
                .is_some()
                {
                    if let Some(tile) = active_tile
                        && let Some(i) =
                            self.workspaces.iter().position(|w| w.primary_tile == tile)
                    {
                        self.active = i;
                    }
                }
            },
            DropTarget::EmptySectionMove { to_idx } => {
                let from = match self.sections.iter().position(|s| s.id == section_id) {
                    Some(i) => i,
                    None => return,
                };
                if from == to_idx || to_idx > self.sections.len() {
                    return;
                }
                let sec = self.sections.remove(from);
                let mut dest = to_idx;
                if dest > from {
                    dest -= 1;
                }
                dest = dest.min(self.sections.len());
                self.sections.insert(dest, sec);
            },
            _ => return,
        }
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
                && let Some(url) = tab.session.link_at(col, row)
            {
                let _ = std::process::Command::new("open").arg(url).spawn();
                return true;
            }
        }
        false
    }

    // ── Pointer events (from the terminal Element) ──────────────────────────

    /// Routes a click to whichever overlay is up, in priority order
    /// (message → fork picker → dir picker). Ports origin/main's overlay_click
    /// into the gpui three-field model.
    fn overlay_click(&mut self, px: f32, py: f32, width: u32, height: u32, scale: f32) {
        // Confirm dialog is topmost: the buttons decide; a click outside the
        // panel cancels (the safe default for a destructive action).
        if let Some(confirm) = self.confirm.as_ref() {
            let layout = self.renderer.confirm_layout(&confirm.text, confirm.accept_label());
            if layout.close.contains(px, py) {
                self.confirm_accept();
            } else if layout.cancel.contains(px, py) || !layout.panel.contains(px, py) {
                self.confirm = None;
            }
            self.request_redraw();
            return;
        }

        // Message overlay: a dismissable one clears on any click,
        // a modal (provisioning) one swallows the click.
        if let Some((_, dismissable)) = self.message.as_ref() {
            if *dismissable {
                self.message = None;
            }
            self.request_redraw();
            return;
        }

        // Save-as-workspace modal: a click focuses a field, picks a
        // destination row (which saves), or cancels when outside the panel.
        if let Some(modal) = self.save_ws.as_ref() {
            let dest_rows =
                if modal.dest_selected.is_some() { modal.dest_labels.len() } else { 0 };
            let layout = self.renderer.save_layout(dest_rows);
            if !layout.panel.contains(px, py) {
                self.save_ws = None;
            } else if layout.name.contains(px, py) {
                if let Some(m) = self.save_ws.as_mut() {
                    m.field = 0;
                    m.dest_selected = None;
                }
            } else if layout.desc.contains(px, py) {
                if let Some(m) = self.save_ws.as_mut() {
                    m.field = 1;
                    m.dest_selected = None;
                }
            } else if let Some(i) = layout.rows.iter().position(|r| r.contains(px, py)) {
                if let Some(m) = self.save_ws.as_mut() {
                    m.dest_selected = Some(i);
                }
                self.commit_save_workspace();
            }
            self.request_redraw();
            return;
        }

        // Profile picker (step 3): click a row to select+confirm, click
        // outside to step back.
        if let Some(pp) = &mut self.profile_picker {
            let layout =
                picker::PickerLayout::compute(width, height, scale, pp.rows.len(), pp.selected);
            if !layout.panel.contains(px, py) {
                self.cancel_profile();
                self.request_redraw();
                return;
            }
            if let Some(index) = layout.row_at(px, py) {
                pp.select(index);
                self.confirm_profile();
            }
            self.request_redraw();
            return;
        }

        // Fork picker (step 2): click a row to select+confirm, click outside to
        // step back to the dir picker.
        if let Some(fork) = &mut self.fork {
            let layout =
                picker::PickerLayout::compute(width, height, scale, fork.rows.len(), fork.selected);
            if !layout.panel.contains(px, py) {
                self.fork = None;
                self.picker = Some(picker::Picker::new());
                self.request_redraw();
                return;
            }
            if let Some(index) = layout.row_at(px, py) {
                fork.select(index);
                self.confirm_fork();
            }
            self.request_redraw();
            return;
        }

        // Dir picker (step 1): existing behaviour.
        if let Some(picker) = &mut self.picker {
            let layout = picker::PickerLayout::compute(width, height, scale, picker.rows.len(), picker.selected);
            if !layout.panel.contains(px, py) {
                self.picker = None;
                // Same as Escape: cancelling the flyover's first-open picker
                // closes the waiting surface.
                if self.picker_target == PickerTarget::Flyover && self.flyover_tabs.is_empty() {
                    self.flyover_open = false;
                    self.flyover_focused = false;
                    self.flyover_window_visible = false;
                }
                self.request_redraw();
                return;
            }
            let Some(index) = layout.row_at(px, py) else { return };
            let Some(rect) = layout.row_rect(index) else { return };
            let Some(picker::PickerRow::Entry(entry)) = picker.rows.get(index).cloned() else {
                return;
            };
            picker.select(index);
            if layout.star_rect(&rect).contains(px, py) {
                picker.toggle_pin(&entry.path);
                self.request_redraw();
                return;
            }
            self.confirm_picker();
            return;
        }

        // Command palette: click a row to run the action, click outside to close.
        if let Some(palette) = &mut self.palette {
            let layout = picker::PickerLayout::compute(width, height, scale, palette.rows.len(), palette.selected);
            if !layout.panel.contains(px, py) {
                self.palette = None;
                self.request_redraw();
                return;
            }
            if let Some(index) = layout.row_at(px, py) {
                palette.select(index);
                let action = palette.selected_action();
                self.palette = None;
                if let Some(action) = action {
                    self.run_action(action);
                }
            }
            self.request_redraw();
        }
    }

    fn on_mouse_down(&mut self, window: &mut Window, click_count: usize) {
        let scale = self.renderer.scale;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (w, h) = self.renderer.surface_size();
        let grab = GRAB * scale;

        // A fresh click sequence forgets which pane the previous one expanded.
        if click_count <= 1 {
            self.just_expanded = None;
        }

        // Overlays are modal: they intercept clicks in priority order
        // (confirm → message → fork picker → dir picker) before anything else.
        if self.confirm.is_some()
            || self.message.is_some()
            || self.save_ws.is_some()
            || self.profile_picker.is_some()
            || self.fork.is_some()
            || self.picker.is_some()
            || self.palette.is_some()
        {
            self.overlay_click(px, py, w, h, scale);
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
            // Top-edge grab zone starts a height-resize drag (not when
            // maximized — there's no meaningful height to drag).
            let grab = (workspace::FLYOVER_RESIZE_GRAB * scale).max(1.0);
            if !self.flyover_maximized && (py - panel.y).abs() <= grab {
                self.drag = Drag::FlyoverResize;
                self.request_redraw();
                return;
            }
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
                    // Determine which tab was clicked; × closes it.
                    let maxed = self.flyover_maximized;
                    let tab_rect = workspace::flyover_tab_rect(&panel, 0, n.max(1), scale, maxed);
                    let ti = ((((px - tab_rect.x).max(0.0)) / tab_rect.w).floor() as usize)
                        .min(n.saturating_sub(1));
                    if workspace::flyover_tab_close_rect(&panel, ti, n, scale, maxed).contains(px, py) {
                        self.close_flyover_tab(ti);
                        return;
                    }
                    self.flyover_active = ti;
                    self.flyover_focused = true;
                    self.flyover_mark_read();
                } else {
                    // Click in content area: focus the panel and start selection.
                    self.flyover_focused = true;
                    let content = workspace::flyover_content(&panel, scale);
                    if let Some((col, row)) = self.renderer.cell_at(&content, px, py) {
                        if let Some(tab) = self.flyover_tabs.get(self.flyover_active) {
                            tab.session.begin_selection(col, row);
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

        // Sidebar edge → resize sidebar (every page shares the width).
        // Collapsed there is no edge to grab (sidebar.w is 0).
        if sidebar.w > 0.0 && (px - sidebar.w).abs() <= grab {
            self.drag = Drag::Sidebar;
            self.resize_hover = Some(workspace::ResizeHover::Sidebar);
            return;
        }

        // Collapsed sidebar: the traffic-light corner (which the top-left
        // tile's tab strip cedes via `tab_strip_rect`) drags the window.
        if self.sidebar_collapsed && workspace::collapsed_drag_zone(scale).contains(px, py) {
            window.start_window_move();
            return;
        }

        // Empty state (Sessions only): the centered CTA is the only
        // interactive element in the content area (the placeholder tile must
        // not arm tab drags). The Settings page keeps its own hit-testing.
        if self.page == Page::Sessions && self.is_empty_state() && !sidebar.contains(px, py) {
            if workspace::empty_state_cta(w, h, scale, self.sidebar_w()).contains(px, py) {
                self.open_picker();
            }
            return;
        }

        if self.page == Page::Sessions {
            let ws = &self.workspaces[self.active];
            let (_, dividers) = workspace::layout_tiles(&ws.root, self.area(), scale);
            if let Some(d) = dividers.iter().find(|d| d.rect.inflate(grab).contains(px, py)) {
                self.drag = Drag::Divider { path: d.path.clone() };
                self.resize_hover = Some(workspace::ResizeHover::Divider {
                    path: d.path.clone(),
                    dir: d.dir,
                });
                return;
            }
        }

        // Sidebar: titlebar strip = traffic lights + window drag handle.
        if sidebar.contains(px, py) {
            // Window-drag is scoped to the titlebar strip ONLY so that clicks on
            // tile tab strips are never treated as a window move.
            if workspace::titlebar(scale, self.sidebar_w()).contains(px, py) {
                // Native traffic-light buttons handle their own clicks; a press
                // anywhere else in the strip drags the window (we own the drag).
                window.start_window_move();
                return;
            }
            // Page-dot strip at the sidebar's bottom: click navigates.
            if let Some(i) = self.page_slot_at(px, py) {
                self.set_page(Page::ALL[i]);
                return;
            }
            if self.page == Page::Settings {
                // Settings sections sit in the group rows' slots.
                for (i, section) in Section::ALL.iter().enumerate() {
                    if workspace::tab_rect(i, scale, self.sidebar_w()).contains(px, py) {
                        self.section = *section;
                        self.recording = None;
                        self.editing_command = None;
                        self.request_redraw();
                        return;
                    }
                }
                return;
            }
            if self.page == Page::Cleanup {
                // Tab 0 = "All", then one per repo.
                let repos = self.cleanup.repos();
                // "All" tab at index 0.
                if workspace::tab_rect(0, scale, self.sidebar_w()).contains(px, py) {
                    self.cleanup.repo_filter = None;
                    self.cleanup.scroll = 0;
                    self.request_redraw();
                    return;
                }
                for (i, repo) in repos.iter().enumerate() {
                    if workspace::tab_rect(i + 1, scale, self.sidebar_w()).contains(px, py) {
                        self.cleanup.repo_filter = Some(repo.root.clone());
                        self.cleanup.scroll = 0;
                        self.request_redraw();
                        return;
                    }
                }
                return;
            }
            if workspace::new_group_button(scale, self.sidebar_w()).contains(px, py) {
                self.open_picker();
                return;
            }
            if workspace::new_section_button(scale, self.sidebar_w()).contains(px, py) {
                // Append an empty expanded section and open the rename editor.
                let id = self.next_section_id;
                self.next_section_id = self.next_section_id.saturating_add(1);
                self.sections.push(workspace::Section {
                    id,
                    name: "section".into(),
                    emoji: String::new(),
                    collapsed: false,
                });
                self.editing_section = Some((id, "section".into()));
                self.persist_snapshot();
                self.request_redraw();
                return;
            }
            // Consume the shared row list so paint and hit-test never disagree.
            // Arm a press; click actions fire on mouse-up if the drag threshold
            // is never crossed (mirrors TabPress → Tab).
            let rows = workspace::sidebar_rows(&self.workspaces, &self.sections);
            for (ri, row) in rows.iter().enumerate() {
                let rect = workspace::sidebar_row_rect(
                    &rows,
                    ri,
                    &self.workspaces,
                    scale,
                    self.sidebar_w(),
                );
                if !rect.contains(px, py) {
                    continue;
                }
                match *row {
                    workspace::SidebarRow::SectionHeader { section_idx } => {
                        let section_id = self.sections[section_idx].id;
                        self.drag = Drag::SectionPress {
                            section_id,
                            start: self.cursor,
                            click_count,
                        };
                        return;
                    },
                    workspace::SidebarRow::Group { ws_idx } => {
                        self.drag = Drag::GroupPress {
                            ws: ws_idx,
                            start: self.cursor,
                        };
                        return;
                    },
                }
            }
            return;
        }

        // Settings page: the content area is the settings card.
        if self.page == Page::Settings {
            self.settings_click(px, py);
            return;
        }
        // Cleanup page: the content area is the cleanup card. A press on a
        // column boundary starts a resize drag; anything else is a click.
        if self.page == Page::Cleanup {
            if let Some(boundary) = cleanup::boundary_at(
                &self.area(),
                scale,
                &self.cleanup.col_fracs,
                px,
                py,
                GRAB * scale,
            ) {
                self.cleanup.hover = None;
                self.drag = Drag::CleanupColumn { boundary };
                return;
            }
            self.cleanup_click(px, py);
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
                self.set_collapsed(*id, false);
                self.just_expanded = Some(*id);
                self.workspaces[self.active].focused_tile = *id;
                self.request_redraw();
                return;
            }
            let bar = workspace::tile_tab_bar(&strip, scale);
            if bar.contains(px, py) {
                let has_caret = axis.is_some();
                if has_caret && workspace::tile_caret_rect(rect, scale).contains(px, py) {
                    // Only the first click of a double toggles — the second
                    // would just snap it straight back.
                    if click_count <= 1 {
                        let collapsed = ws.root.find_tile(*id).is_some_and(|t| t.collapsed);
                        self.set_collapsed(*id, !collapsed);
                        self.request_redraw();
                    }
                    return;
                }
                if let Some(tile) = ws.root.find_tile_mut(*id) {
                    let n = tile.tabs.len();
                    let t0 = workspace::tile_tab_rect(&strip, 0, n.max(1), scale, has_caret);
                    let ti =
                        ((((px - t0.x).max(0.0)) / t0.w).floor() as usize).min(n.saturating_sub(1));
                    tile.active = ti;
                    ws.focused_tile = *id;
                    if n > 0
                        && workspace::tile_tab_close_rect(&strip, ti, n, scale, has_caret)
                            .contains(px, py)
                    {
                        self.close_active_tab();
                        return;
                    }
                    if tile.collapsed {
                        // Clicking a tab name on a collapsed pane expands it.
                        self.set_collapsed(*id, false);
                        self.just_expanded = Some(*id);
                    } else if has_caret
                        && click_count >= 2
                        && self.just_expanded != Some(*id)
                    {
                        // Double-clicking the tab bar collapses the pane
                        // (unless this same double-click just expanded it).
                        self.set_collapsed(*id, true);
                        self.request_redraw();
                        return;
                    }
                    self.drag = Drag::TabPress { tile: *id, tab: ti, start: self.cursor };
                    self.sync_layout();
                    self.mark_visible_read();
                    self.request_redraw();
                }
            } else {
                ws.focused_tile = *id;
                let content = workspace::tile_content(rect, scale);
                if let Some((col, row)) = self.renderer.cell_at(&content, px, py) {
                    if let Some(tab) =
                        self.workspaces[self.active].root.find_tile(*id).and_then(|t| t.active_tab())
                    {
                        tab.session.begin_selection(col, row);
                    }
                    self.drag = Drag::Select { tile: *id };
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

        match &self.drag {
            Drag::Sidebar => {
                self.sidebar_expanded_w =
                    (px / scale).clamp(workspace::SIDEBAR_MIN_W, workspace::SIDEBAR_MAX_W);
                self.sync_layout();
                self.request_redraw();
            },
            Drag::CleanupColumn { boundary } => {
                let fracs = cleanup::drag_boundary(
                    &self.area(),
                    scale,
                    &self.cleanup.col_fracs,
                    *boundary,
                    px,
                );
                if fracs != self.cleanup.col_fracs {
                    self.cleanup.col_fracs = fracs;
                    self.request_redraw();
                }
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
            Drag::SectionPress { section_id, start, .. } => {
                let (sx, sy) = *start;
                if (self.cursor.0 - sx).abs() + (self.cursor.1 - sy).abs() > DRAG_THRESHOLD {
                    self.drag = Drag::Section { section_id: *section_id };
                    self.request_redraw();
                }
            },
            Drag::Section { .. } => self.request_redraw(),
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
                    if let Some(tab) = self.flyover_tabs.get(self.flyover_active) {
                        tab.session.update_selection(col, row);
                        self.request_redraw();
                    }
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
                    {
                        tab.session.update_selection(col, row);
                        self.request_redraw();
                    }
                }
                let _ = area;
            },
            Drag::None => {
                // Page-dot hover: the crossfade animation is driven by the
                // 16ms tick in drain_events; here we only track the target.
                self.dot_hover = self.page_slot_at(px, py);
                // Resize-handle hover: suppress while any overlay is open so the
                // cursor/highlight don't fight the modal. Hit-test matches
                // on_mouse_down exactly via workspace::resize_hover_at.
                let hover = if self.confirm.is_some()
                    || self.message.is_some()
                    || self.fork.is_some()
                    || self.picker.is_some()
                    || self.palette.is_some()
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
                    workspace::resize_hover_at(
                        &ws.root,
                        self.area(),
                        scale,
                        sidebar_edge_x,
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
                // Dirty-cell hover on the Cleanup page drives the file popover.
                let cleanup_hover = if self.page == Page::Cleanup
                    && self.confirm.is_none()
                    && self.message.is_none()
                {
                    self.cleanup_dirty_hover_at(px, py)
                } else {
                    None
                };
                if cleanup_hover != self.cleanup.hover {
                    self.cleanup.hover = cleanup_hover;
                    self.request_redraw();
                }
                // Link hover: suppress when any overlay is open or not in Sessions page.
                let link_hover = if self.page != Page::Sessions
                    || self.confirm.is_some()
                    || self.message.is_some()
                    || self.fork.is_some()
                    || self.picker.is_some()
                    || self.palette.is_some()
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
                            && tab.session.link_at(col, row).is_some()
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

    /// The id of the dirty worktree whose dirty cell is under the cursor.
    fn cleanup_dirty_hover_at(&self, px: f32, py: f32) -> Option<String> {
        let area = self.area();
        let scale = self.scale();
        let cols = cleanup::column_offsets(&area, scale, &self.cleanup.col_fracs);
        if px < cols.dirty || px >= cols.parity {
            return None;
        }
        let fit = cleanup::rows_that_fit(&area, scale);
        for vi in 0..fit {
            let row = cleanup::row_rect(&area, vi, scale)?;
            if !row.contains(px, py) {
                continue;
            }
            let idx = self.cleanup.scroll + vi;
            return match self.cleanup.rows().get(idx) {
                Some(cleanup::Row::Entry(w)) if w.dirty_count > 0 => Some(w.id.clone()),
                _ => None,
            };
        }
        None
    }

    fn on_mouse_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (window, cx);
        match std::mem::replace(&mut self.drag, Drag::None) {
            // Column resize released: keep the layout for future sessions.
            Drag::CleanupColumn { .. } => {
                settings::set(
                    "cleanup.columns",
                    cleanup::format_col_fracs(&self.cleanup.col_fracs).into(),
                );
                self.request_redraw();
            },
            Drag::Tab { tile, tab } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(target) = self.resolve_drop(px, py) {
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
            // Section header click (no drag): rename on double-click, else toggle.
            Drag::SectionPress { section_id, click_count, .. } => {
                if let Some(section_idx) =
                    self.sections.iter().position(|s| s.id == section_id)
                {
                    if click_count >= 2 {
                        // The double-click's first click already toggled
                        // collapse below; undo it before opening the editor.
                        self.sections[section_idx].collapsed =
                            !self.sections[section_idx].collapsed;
                        let sec = &self.sections[section_idx];
                        let buf = if sec.emoji.is_empty() {
                            sec.name.clone()
                        } else {
                            format!("{} {}", sec.emoji, sec.name)
                        };
                        self.editing_section = Some((sec.id, buf));
                    } else {
                        self.sections[section_idx].collapsed =
                            !self.sections[section_idx].collapsed;
                        self.persist_snapshot();
                    }
                    self.request_redraw();
                }
            },
            // Section header drag: move the whole block top-level only.
            Drag::Section { section_id } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(target) = self.resolve_section_drop(px, py, section_id) {
                    self.apply_section_drop(section_id, target);
                }
                self.request_redraw();
            },
            _ => {},
        }
    }

    // ── Keyboard ────────────────────────────────────────────────────────

    fn on_key_down(&mut self, ev: &KeyDownEvent) {
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
            || self.profile_picker.is_some()
            || self.fork.is_some()
            || self.picker.is_some()
            || self.palette.is_some()
        {
            self.handle_picker_key(ev);
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

        // Sidebar section rename captures typing; shortcuts stay muted.
        if self.editing_section.is_some() {
            self.handle_section_key(ev);
            return;
        }
        // The Settings page owns the keyboard: no PTY to type into.
        if self.page == Page::Settings {
            self.handle_settings_key(ev);
            return;
        }
        // The Cleanup page owns the keyboard too (no terminal underneath).
        if self.page == Page::Cleanup {
            self.handle_cleanup_key(ev);
            return;
        }
        // ⌘ shortcuts take priority over passing bytes to the shell.
        if ev.keystroke.modifiers.platform {
            self.handle_shortcut(ev);
            return;
        }
        if let Some(bytes) = key_to_bytes(&ev.keystroke) {
            let ws = &self.workspaces[self.active];
            if let Some(tab) = ws.focused().and_then(|t| t.active_tab()) {
                tab.session.write(bytes);
                // Typing snaps back to the live bottom and drops any
                // selection, like every other terminal.
                tab.session.scroll_to_bottom();
                tab.session.clear_selection();
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
        // The save-as-workspace modal owns typing for its two fields, then the
        // destination rows.
        if self.save_ws.is_some() {
            self.handle_save_key(ev);
            return;
        }
        // Step 3: the workspace-profile picker. Escape steps back.
        if self.profile_picker.is_some() {
            match key {
                "escape" => self.cancel_profile(),
                "enter" => self.confirm_profile(),
                "up" => {
                    if let Some(pp) = self.profile_picker.as_mut() {
                        pp.move_selection(-1);
                    }
                },
                "down" => {
                    if let Some(pp) = self.profile_picker.as_mut() {
                        pp.move_selection(1);
                    }
                },
                "backspace" => {
                    if let Some(pp) = self.profile_picker.as_mut() {
                        pp.backspace();
                    }
                },
                _ => {
                    if !ev.keystroke.modifiers.control
                        && let Some(text) = ev.keystroke.key_char.as_deref()
                        && let Some(pp) = self.profile_picker.as_mut()
                    {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            pp.push_char(ch);
                        }
                    }
                },
            }
            self.request_redraw();
            return;
        }
        // Step 2: the fork picker. Escape steps back to the dir picker.
        if self.fork.is_some() {
            match key {
                "escape" => {
                    self.fork = None;
                    self.picker = Some(picker::Picker::new());
                },
                "enter" => self.confirm_fork(),
                "up" => {
                    if let Some(f) = self.fork.as_mut() {
                        f.move_selection(-1);
                    }
                },
                "down" => {
                    if let Some(f) = self.fork.as_mut() {
                        f.move_selection(1);
                    }
                },
                "backspace" => {
                    if let Some(f) = self.fork.as_mut() {
                        f.backspace();
                    }
                },
                _ => {
                    if !ev.keystroke.modifiers.control
                        && let Some(text) = ev.keystroke.key_char.as_deref()
                        && let Some(f) = self.fork.as_mut()
                    {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            f.push_char(ch);
                        }
                    }
                },
            }
            self.request_redraw();
            return;
        }
        // Step 1: the dir picker. Escape closes the overlay entirely.
        if self.picker.is_some() {
            match key {
                "escape" => {
                    self.picker = None;
                    // Cancelling the flyover's first-open picker closes the
                    // waiting surface too — there's nothing to show yet.
                    if self.picker_target == PickerTarget::Flyover
                        && self.flyover_tabs.is_empty()
                    {
                        self.flyover_open = false;
                        self.flyover_focused = false;
                        self.flyover_window_visible = false;
                    }
                },
                "enter" => self.confirm_picker(),
                "up" => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_selection(-1);
                    }
                },
                "down" => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_selection(1);
                    }
                },
                "backspace" => {
                    if let Some(p) = self.picker.as_mut() {
                        p.backspace();
                    }
                },
                _ => {
                    // A printable character extends the query. gpui hands us the
                    // already-composed text (respecting shift/dead keys) in
                    // key_char; ignore control chords and non-text keys.
                    if !ev.keystroke.modifiers.control
                        && let Some(text) = ev.keystroke.key_char.as_deref()
                        && let Some(p) = self.picker.as_mut()
                    {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            p.push_char(ch);
                        }
                    }
                },
            }
        } else if self.palette.is_some() {
            // The palette's own chord closes it again — the overlay owns the
            // keyboard, so the toggle in run_action is unreachable from here.
            if Action::CommandPalette.binding().matches(&ev.keystroke) {
                self.palette = None;
                self.request_redraw();
                return;
            }
            match key {
                "escape" => self.palette = None,
                "enter" => {
                    let action = self
                        .palette
                        .as_ref()
                        .and_then(|p| p.selected_action());
                    self.palette = None;
                    if let Some(action) = action {
                        self.run_action(action);
                    }
                },
                "up" => {
                    if let Some(p) = self.palette.as_mut() {
                        p.move_selection(-1);
                    }
                },
                "down" => {
                    if let Some(p) = self.palette.as_mut() {
                        p.move_selection(1);
                    }
                },
                "backspace" => {
                    if let Some(p) = self.palette.as_mut() {
                        p.backspace();
                    }
                },
                _ => {
                    if !ev.keystroke.modifiers.control
                        && let Some(text) = ev.keystroke.key_char.as_deref()
                        && let Some(p) = self.palette.as_mut()
                    {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            p.push_char(ch);
                        }
                    }
                },
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
                    "escape" => modal.dest_selected = None,
                    "up" => modal.dest_selected = Some(sel.saturating_sub(1)),
                    "down" => {
                        let last = modal.dest_labels.len().saturating_sub(1);
                        modal.dest_selected = Some((sel + 1).min(last));
                    },
                    "enter" => commit = true,
                    _ => {},
                }
            } else {
                match key {
                    "escape" => close = true,
                    "tab" => modal.field = (modal.field + 1) % 2,
                    "enter" => {
                        if modal.field == 0 {
                            modal.field = 1;
                        } else if modal.name.trim().is_empty() {
                            // A profile needs a name before it can move on.
                            modal.field = 0;
                        } else {
                            modal.dest_selected = Some(0);
                        }
                    },
                    "backspace" => {
                        let buf =
                            if modal.field == 0 { &mut modal.name } else { &mut modal.description };
                        buf.pop();
                    },
                    _ => {
                        if !ev.keystroke.modifiers.control
                            && !ev.keystroke.modifiers.platform
                            && let Some(text) = ev.keystroke.key_char.as_deref()
                        {
                            let buf = if modal.field == 0 {
                                &mut modal.name
                            } else {
                                &mut modal.description
                            };
                            for ch in text.chars().filter(|c| !c.is_control()) {
                                buf.push(ch);
                            }
                        }
                    },
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
    fn handle_settings_key(&mut self, ev: &KeyDownEvent) {
        // An editing primary-command row captures typing: chars append, Enter
        // saves, Escape cancels.
        if self.editing_command.is_some() {
            match ev.keystroke.key.as_str() {
                "escape" => self.editing_command = None,
                "enter" => {
                    let value = self.editing_command.take().unwrap_or_default();
                    settings::set("session.primary_command", value.trim().into());
                },
                "backspace" => {
                    if let Some(buf) = self.editing_command.as_mut() {
                        buf.pop();
                    }
                },
                _ => {
                    if !ev.keystroke.modifiers.control
                        && !ev.keystroke.modifiers.platform
                        && let Some(text) = ev.keystroke.key_char.as_deref()
                        && let Some(buf) = self.editing_command.as_mut()
                    {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            buf.push(ch);
                        }
                    }
                },
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

    /// Run a rebindable action. Page navigation and quit work everywhere;
    /// terminal-layout actions only make sense on the Sessions page.
    fn run_action(&mut self, action: Action) {
        match action {
            Action::PrevPage => return self.cycle_page(-1),
            Action::NextPage => return self.cycle_page(1),
            Action::PrevSidebarTab => return self.cycle_sidebar_tab(-1),
            Action::NextSidebarTab => return self.cycle_sidebar_tab(1),
            Action::ToggleSidebar => return self.toggle_sidebar(),
            Action::OpenSettings => return self.set_page(Page::Settings),
            Action::Quit => std::process::exit(0),
            Action::CommandPalette => {
                if self.palette.is_some() {
                    self.palette = None;
                } else {
                    self.palette = Some(palette::Palette::new());
                }
                return;
            },
            _ => {},
        }
        if self.page != Page::Sessions {
            return;
        }
        // Empty state: there is no pane to act on. New tab / new group start
        // a group via the picker; everything else is a no-op.
        if self.is_empty_state() {
            if matches!(action, Action::NewTab | Action::NewGroup) {
                self.open_picker();
            }
            return;
        }
        match action {
            Action::SplitRight => self.split(Dir::Row),
            Action::SplitDown => self.split(Dir::Column),
            Action::NewTab => self.new_tab(),
            Action::NewGroup => self.open_picker(),
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::CloseTab => self.close_active_tab(),
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
            Action::PrevSidebarTab
            | Action::NextSidebarTab
            | Action::ToggleSidebar
            | Action::PrevPage
            | Action::NextPage
            | Action::OpenSettings
            | Action::Quit
            | Action::CommandPalette => {},
            Action::ToggleFlyover => self.toggle_flyover(),
            Action::FlyoverPopout => self.flyover_toggle_windowed(),
        }
    }

    /// ⌘S: collapse/expand the sidebar. Layout re-syncs so the PTYs pick up
    /// the reclaimed (or surrendered) width immediately.
    fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.sync_layout();
        self.request_redraw();
    }

    /// ⌘⇧←/→: step through `Page::ALL`, wrapping at both ends.
    fn cycle_page(&mut self, delta: isize) {
        let i = pages::cycle(self.page.index(), Page::ALL.len(), delta);
        self.set_page(Page::ALL[i]);
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
            Page::Cleanup => {
                let repos = self.cleanup.repos();
                // Tabs: 0 = All, 1..=n = per-repo
                let n_tabs = repos.len() + 1;
                let cur = match &self.cleanup.repo_filter {
                    None => 0,
                    Some(root) => repos.iter().position(|r| &r.root == root).map(|i| i + 1).unwrap_or(0),
                };
                let next = pages::cycle(cur, n_tabs, delta);
                self.cleanup.repo_filter = if next == 0 {
                    None
                } else {
                    repos.get(next - 1).map(|r| r.root.clone())
                };
                self.cleanup.scroll = 0;
                self.request_redraw();
            },
        }
    }

    fn set_page(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.recording = None;
            self.editing_command = None;
            // Grids may have gone stale while the Settings page was up.
            if page == Page::Sessions {
                self.sync_layout();
                self.mark_visible_read();
            }
            // Entering the Cleanup page triggers a fresh scan.
            if page == Page::Cleanup {
                self.spawn_cleanup_scan();
            }
        }
        self.request_redraw();
    }

    /// The page slot under a point in the sidebar's bottom strip, if any
    /// (slightly inflated so the small dots are easy to hit).
    fn page_slot_at(&self, px: f32, py: f32) -> Option<usize> {
        let (_, h) = self.renderer.surface_size();
        let scale = self.scale();
        let n = Page::ALL.len();
        (0..n).find(|&i| {
            workspace::page_slot_rect(i, n, h, scale, self.sidebar_w())
                .inflate((3.0 * scale).round())
                .contains(px, py)
        })
    }

    /// Route a click inside the cleanup card to the row or button it hit.
    fn cleanup_click(&mut self, px: f32, py: f32) {
        let area = self.area();
        let scale = self.scale();
        let cell_w = self.renderer.cell_width;
        // Refresh button.
        if cleanup::refresh_button_rect(&area, scale, cell_w).contains(px, py) {
            self.spawn_cleanup_scan();
            return;
        }
        // Delete button — only active when selection is non-empty.
        let delete = cleanup::delete_button_rect(&area, scale, cell_w);
        if delete.contains(px, py) && !self.cleanup.selected.is_empty() {
            let targets = self.cleanup.selected_by_repo();
            let count: usize = targets.iter().map(|(_, ids)| ids.len()).sum();
            let dirty = self.cleanup.selected_dirty_count();
            let plural = if count == 1 { "" } else { "s" };
            let text = if dirty > 0 {
                format!(
                    "Delete {count} worktree{plural}? {dirty} ha{} uncommitted changes — those are discarded.",
                    if dirty == 1 { "s" } else { "ve" }
                )
            } else {
                format!("Delete {count} worktree{plural}? Unmerged branches are kept.")
            };
            self.confirm = Some(ConfirmClose {
                text,
                action: ConfirmAction::CleanupDelete { targets },
            });
            self.request_redraw();
            return;
        }
        // Row clicks: toggle an entry's selection, or filter to a header's
        // repo (row_rect is None past the footer).
        enum Hit {
            Toggle(String),
            Filter(String),
        }
        let fit = cleanup::rows_that_fit(&area, scale);
        let mut hit: Option<Hit> = None;
        for vi in 0..fit {
            let Some(row) = cleanup::row_rect(&area, vi, scale) else { break };
            if !row.contains(px, py) {
                continue;
            }
            let idx = self.cleanup.scroll + vi;
            hit = self.cleanup.rows().get(idx).map(|r| match r {
                cleanup::Row::Entry(w) => Hit::Toggle(w.id.clone()),
                cleanup::Row::Header { root, .. } => Hit::Filter(root.to_string()),
            });
            break;
        }
        match hit {
            Some(Hit::Toggle(id)) => {
                self.cleanup.toggle(&id);
                self.request_redraw();
            },
            Some(Hit::Filter(root)) => {
                self.cleanup.repo_filter = Some(root);
                self.cleanup.scroll = 0;
                self.request_redraw();
            },
            None => {},
        }
    }

    /// Kick off a background `drop -d --json` sweep and show the scanning
    /// state until its `TermEvent` lands.
    fn spawn_cleanup_scan(&mut self) {
        self.cleanup.set_scanning();
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let event = match run_drop_status() {
                Ok(worktrees) => TermEvent::CleanupScanned(worktrees),
                Err(e) => TermEvent::CleanupScanFailed(e),
            };
            let _ = tx.send(event);
        });
        self.request_redraw();
    }

    /// Handle keyboard input on the Cleanup page (mirrors handle_settings_key).
    fn handle_cleanup_key(&mut self, ev: &KeyDownEvent) {
        if ev.keystroke.modifiers.platform {
            self.handle_shortcut(ev);
            return;
        }
        match ev.keystroke.key.as_str() {
            "a" => {
                self.cleanup.select_all_visible();
                self.request_redraw();
            },
            "m" => {
                self.cleanup.select_merged_visible();
                self.request_redraw();
            },
            "r" => {
                self.spawn_cleanup_scan();
                self.request_redraw();
            },
            "escape" => {
                self.cleanup.clear_selection();
                self.request_redraw();
            },
            _ => {},
        }
    }

    /// Route a click inside the settings card to the row it hit.
    fn settings_click(&mut self, px: f32, py: f32) {
        let area = self.area();
        let scale = self.scale();
        match self.section {
            Section::Sessions => {
                if workspace::settings_row_rect(&area, 0, scale).contains(px, py) {
                    // Edit in place, starting from the current value.
                    self.editing_command = Some(settings::primary_command());
                } else {
                    // A click anywhere else cancels an in-progress edit.
                    self.editing_command = None;
                }
            },
            Section::Keyboard => {
                for (i, action) in Action::ALL.iter().enumerate() {
                    if workspace::settings_row_rect(&area, i, scale).contains(px, py) {
                        self.recording = Some(*action);
                        self.request_redraw();
                        return;
                    }
                }
                // A click anywhere else cancels an armed recording.
                self.recording = None;
            },
            Section::Appearance => {
                for (row, col, item) in pages::appearance_layout() {
                    let slot = workspace::appearance_slot_rect(
                        &area,
                        row,
                        col,
                        item.full_width(),
                        scale,
                    );
                    if !slot.contains(px, py) {
                        continue;
                    }
                    match item {
                        pages::AppearanceItem::Mode => {
                            for (i, m) in theme::Mode::ALL.into_iter().enumerate() {
                                let seg = workspace::mode_segment_rect(
                                    &slot,
                                    i,
                                    self.renderer.cell_width,
                                    scale,
                                );
                                if seg.contains(px, py) {
                                    settings::set("appearance.mode", m.name().into());
                                    break;
                                }
                            }
                        },
                        pages::AppearanceItem::Header(_) => {},
                        pages::AppearanceItem::Theme(t) => {
                            settings::set(theme::setting_key(t.dark), t.name.into());
                        },
                        // The clipboard's token string becomes its polarity's
                        // custom theme and is selected right away; anything
                        // unparseable changes nothing.
                        pages::AppearanceItem::ImportTheme => {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                if let Some(tokens) = clipboard
                                    .get_text()
                                    .ok()
                                    .as_deref()
                                    .and_then(theme::parse_tokens)
                                {
                                    let dark = theme::is_dark_color(tokens[0]);
                                    settings::set(
                                        theme::custom_key(dark),
                                        theme::serialize_tokens(&tokens).into(),
                                    );
                                    settings::set(
                                        theme::setting_key(dark),
                                        theme::custom_name(dark).into(),
                                    );
                                }
                            }
                        },
                        pages::AppearanceItem::ExportTheme => {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(theme::export_current());
                            }
                        },
                        // Adaptive default applies to both polarities at once.
                        pages::AppearanceItem::TermDefault => {
                            settings::set(term_theme::setting_key(false), "default".into());
                            settings::set(term_theme::setting_key(true), "default".into());
                        },
                        pages::AppearanceItem::Term(t) => {
                            settings::set(term_theme::setting_key(t.dark), t.name.into());
                        },
                    }
                    break;
                }
            },
            Section::Terminal => {
                let row = workspace::settings_row_rect(&area, pages::PERSIST_TOGGLE_ROW, scale);
                if row.contains(px, py) {
                    let on = settings::get_bool("terminal.persist", false);
                    settings::set("terminal.persist", (!on).into());
                    // Snapshot right away so enabling then restarting (with no
                    // further mutations) still restores the current groups.
                    self.persist_snapshot();
                }
            },
            Section::Debug => {
                let row = workspace::settings_row_rect(&area, pages::DEBUG_TOGGLE_ROW, scale);
                if row.contains(px, py) {
                    let on = settings::get_bool("debug.overlay", false);
                    settings::set("debug.overlay", (!on).into());
                }
            },
        }
        self.request_redraw();
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
                        if let Some(tab) = ws.focused().and_then(|t| t.active_tab())
                            && tab.session.id == id
                        {
                            let title = tab.session.title();
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
                    redraw = true;
                },
                TermEvent::GroupFailed { message } => {
                    self.pending_group_profile = None;
                    self.message = Some((format!("drop failed: {message}"), true));
                    redraw = true;
                },
                TermEvent::CleanupScanned(worktrees) => {
                    self.cleanup.set_ready(worktrees);
                    redraw = true;
                },
                TermEvent::CleanupScanFailed(msg) => {
                    self.cleanup.set_failed(msg);
                    redraw = true;
                },
                TermEvent::CleanupRemoved { removed, failed, error } => {
                    // Replace the modal "Deleting…" message with the outcome
                    // (dismissable), and refresh the table to match disk.
                    let msg = if failed == 0 {
                        format!("Removed {removed} worktree{}.", if removed == 1 { "" } else { "s" })
                    } else {
                        let reason =
                            error.map(|e| format!(" — {e}")).unwrap_or_default();
                        format!("Removed {removed}, failed {failed}{reason}")
                    };
                    self.message = Some((msg, true));
                    self.spawn_cleanup_scan();
                    redraw = true;
                },
                // A pane signaled for attention (OSC 9, emitted by the Claude
                // Code hooks). On-screen tabs of the active group are being
                // watched, so only hidden tabs gain the unread dot.
                TermEvent::Attention(id) => {
                    let watched = (self.page == Page::Sessions && self.is_visible(id))
                        || self.flyover_visible(id);
                    if !watched && self.set_unread_by_session(id) {
                        redraw = true;
                    }
                },
            }
        }
        // Advance the page-dot crossfades: hovered or active slots head to 1,
        // the rest back to 0. Redraw while any slot is mid-flight.
        let active = self.page.index();
        for (i, p) in self.dot_anim.iter_mut().enumerate() {
            let target = if i == active || Some(i) == self.dot_hover { 1.0 } else { 0.0 };
            let next =
                if *p < target { (*p + 0.15).min(target) } else { (*p - 0.15).max(target) };
            if next != *p {
                *p = next;
                redraw = true;
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
        redraw || self.dirty
    }

    fn remove_session(&mut self, id: u64) {
        // Flyover shells live outside the workspace tree: drop the tab and
        // close the panel when the last one goes.
        if let Some(ti) = self.flyover_tabs.iter().position(|tab| tab.session.id == id) {
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
                        .position(|tab| tab.session.id == id)
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
                    tab.session.begin_frame();
                }
            }
        }
        // Also call begin_frame on all flyover sessions every frame.
        for tab in &self.flyover_tabs {
            tab.session.begin_frame();
        }
    }
}

/// Human-friendly group name for a cwd: `~` for the home dir, else the last
/// path component.
fn group_name(dir: &std::path::Path) -> String {
    if dirs::home_dir().is_some_and(|home| home == dir) {
        return "~".into();
    }
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
}

/// Abbreviate `path` with `~` for display.
fn tilde(path: &std::path::Path) -> String {
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
                .map(|tab| pwrspace::ProfileTab { command: tab_command(tab) })
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
    let session = &tab.session;
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

/// Scan all drop-managed worktrees via `drop -d --json`. Run from the home
/// directory (outside any repo) so drop sweeps favorites, recents, and its
/// reposDir instead of just one repo. Runs synchronously — callers spawn it
/// on a background thread.
fn run_drop_status() -> Result<Vec<cleanup::WorktreeInfo>, String> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let mut cmd = git::augmented_command("drop");
    cmd.args(["-d", "--json"]).current_dir(home);
    let output = cmd.output().map_err(|e| format!("could not run drop: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason =
            stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(if reason.is_empty() { "drop -d exited with an error".into() } else { reason });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = serde_json::from_str::<Vec<cleanup::WorktreeInfo>>(&stdout)
        .map_err(|e| format!("could not parse drop output: {e}"))?;
    // Enrich dirty worktrees with per-file +/- details for the hover popover.
    // Best-effort: a git hiccup just leaves the list empty.
    for w in worktrees.iter_mut().filter(|w| w.dirty_count > 0) {
        w.dirty_files = dirty_file_details(std::path::Path::new(&w.path));
    }
    Ok(worktrees)
}

/// `git diff HEAD --numstat` + untracked listing for one worktree.
fn dirty_file_details(worktree: &std::path::Path) -> Vec<cleanup::DirtyFile> {
    let run = |args: &[&str]| -> String {
        let mut cmd = git::augmented_command("git");
        cmd.arg("-C").arg(worktree).args(args);
        cmd.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut files = cleanup::parse_numstat(&run(&["diff", "HEAD", "--numstat"]));
    // `--directory` collapses whole untracked directories to one `dir/` entry
    // (like `git status` does) — an unignored build dir reads as one line, not
    // thousands of files. Gitignored files are excluded outright.
    for line in run(&[
        "ls-files",
        "--others",
        "--exclude-standard",
        "--directory",
        "--no-empty-directory",
    ])
    .lines()
    {
        let path = line.trim();
        if !path.is_empty() {
            files.push(cleanup::DirtyFile {
                path: path.to_string(),
                added: None,
                removed: None,
                untracked: true,
            });
        }
    }
    files
}

/// Remove worktrees via `drop rm <ids...> --repo <root> --force --json`, once
/// per repo — drop resolves ids only within a single repo. `--force` mirrors
/// drop's own TUI: the user explicitly selected and confirmed these rows, so
/// dirty worktrees are removed too (branch deletion stays safe-only either
/// way — unmerged branches survive). Returns `(removed, failed)` totals plus
/// the first failure's reason. Runs synchronously — callers spawn it on a
/// background thread.
fn run_drop_rm(targets: Vec<(String, Vec<String>)>) -> (usize, usize, Option<String>) {
    #[derive(serde::Deserialize)]
    struct RmResult {
        removed: bool,
        error: Option<String>,
    }
    let mut removed = 0;
    let mut failed = 0;
    let mut first_error: Option<String> = None;
    for (repo, ids) in targets {
        let count = ids.len();
        let mut cmd = git::augmented_command("drop");
        cmd.arg("rm").args(&ids).arg("--repo").arg(&repo).arg("--force").arg("--json");
        let results: Vec<RmResult> = match cmd.output() {
            Ok(o) => {
                let parsed: Vec<RmResult> =
                    serde_json::from_slice(&o.stdout).unwrap_or_default();
                if parsed.is_empty() && first_error.is_none() {
                    // drop itself failed to run — its last stderr line says why.
                    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
                    first_error =
                        stderr.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string);
                }
                parsed
            },
            Err(e) => {
                first_error.get_or_insert(format!("could not run drop: {e}"));
                Vec::new()
            },
        };
        let ok = results.iter().filter(|r| r.removed).count();
        if first_error.is_none() {
            first_error = results.iter().filter_map(|r| r.error.clone()).next();
        }
        removed += ok;
        failed += count.saturating_sub(ok);
    }
    (removed, failed, first_error)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A full-window canvas element that paints the terminal frame.
        let view = cx.entity();
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(|app, ev: &KeyDownEvent, _win, cx| {
                app.on_key_down(ev);
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
                    app.on_mouse_down(window, ev.click_count);
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
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|app, _ev: &MouseUpEvent, window, cx| {
                    app.on_mouse_up(window, cx);
                    cx.notify();
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
                        view.update(cx, |app, cx| {
                            app.paint_terminal(bounds, window, cx);
                        });
                    },
                )
                // Without an explicit size the canvas resolves to 0 width and
                // the terminal never paints.
                .size_full(),
            )
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
        // The window moved to a display with a different backing scale: font
        // size, cell metrics, and dpi all derive from it, so re-measure the
        // cell and force the PTYs to learn the new cell size even if the
        // grid dimensions happen to be unchanged.
        let rescaled = scale != self.renderer.scale;
        if rescaled {
            let cell_width = renderer::measure_cell_width(window, scale);
            self.renderer.update_scale(scale, cell_width);
        }
        self.renderer.resize(phys_w, phys_h);
        self.sync_layout_impl(rescaled);
        if rescaled {
            self.sync_flyover_layout(true);
        }
        self.begin_frame();

        // Cursor style must be set during paint (gpui asserts the phase). Sticky
        // resize_hover keeps the resize cursor for the whole drag, even when the
        // pointer strays off the handle. An overlay opened by keyboard while
        // hovering leaves resize_hover stale, so overlays suppress it here too.
        let overlay_open = self.confirm.is_some()
            || self.message.is_some()
            || self.fork.is_some()
            || self.picker.is_some()
            || self.palette.is_some();
        let resize_hover = if overlay_open
            || !matches!(self.drag, Drag::None | Drag::Sidebar | Drag::Divider { .. })
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
        // landing zone and compute its translucent preview rect.
        // `self.cursor` is already physical px (see the mouse listeners), so
        // it must NOT be scaled again — doing so put the preview at cursor×
        // scale² and made it disagree with the drop resolved on mouse-up.
        let (cursor_x, cursor_y) = self.cursor;
        let drop_hint = match self.drag {
            Drag::Tab { .. } => {
                self.resolve_drop(cursor_x as f32, cursor_y as f32)
                    .and_then(|t| self.drop_hint(t))
            },
            Drag::Group { .. } => self
                .resolve_sidebar_group_drop(cursor_x as f32, cursor_y as f32)
                .and_then(|t| self.drop_hint(t)),
            Drag::Section { section_id } => self
                .resolve_section_drop(cursor_x as f32, cursor_y as f32, section_id)
                .and_then(|t| self.drop_hint(t)),
            _ => None,
        };

        let chrome = renderer::ChromeState {
            page: self.page,
            section: self.section,
            dot_anim: &self.dot_anim,
            recording: self.recording,
            editing_command: self.editing_command.as_deref(),
            sections: &self.sections,
            editing_section: self
                .editing_section
                .as_ref()
                .map(|(id, buf)| (*id, buf.as_str())),
            cleanup: &self.cleanup,
            // Overlay scoping happens in the renderer (only overlay elements
            // hover while one is up). Here we suppress hover mid-drag, and
            // for the chrome under an open flyover panel — clicks inside the
            // panel never fall through, so hover mustn't either. Modal
            // overlays sit above the flyover, so they keep the live cursor.
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
        let save_view = self.save_ws.as_ref().map(|m| renderer::SaveModalView {
            name: &m.name,
            description: &m.description,
            field: m.field,
            dest: m.dest_selected.map(|sel| (m.dest_labels.as_slice(), sel)),
        });
        let mut frame = self.renderer.build_frame(

            &self.workspaces,
            self.active,
            self.sidebar_w(),
            drop_hint,
            resize_hover,
            link_hover_suppressed,
            self.picker.as_ref(),
            self.fork.as_ref(),
            self.profile_picker.as_ref(),
            save_view.as_ref(),
            self.palette.as_ref(),
            self.message.as_ref(),
            self.confirm.as_ref().map(|c| (c.text.as_str(), c.accept_label())),
            &chrome,
        );
        // The flyover panel lives outside the workspace tree, so its layer is
        // built here from App state and slotted into the frame's flyover
        // fields (painted above tiles/labels, below the modal overlays).
        // In windowed mode the popout window renders it instead.
        if self.flyover_anim > 0.0 && !self.flyover_windowed {
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
        let font_size = px(self.renderer.font_size() * inv);
        let line_height = px(self.renderer.cell_height * inv);
        let cell_height = self.renderer.cell_height;

        // Theme colors resolved once per frame (gradient + shadow ink).
        let th = self.renderer.theme();
        let shadow_rgb = th.shadow;

        // Paint inside an explicit content mask over our bounds. Text glyphs
        // paint into their own pushed layer (via gpui's paint_layer); without an
        // established content-mask context those sub-layers don't composite —
        // this mirrors how Zed's own TerminalElement paints.
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            // 0) the themed window gradient every card and sidebar row floats
            // on (mockup 3a's tinted wrapper).
            window.paint_quad(gpui::fill(
                bounds,
                gpui::linear_gradient(
                    135.0,
                    gpui::linear_color_stop(renderer::color(th.gradient_from, 1.0), 0.0),
                    gpui::linear_color_stop(renderer::color(th.gradient_to, 1.0), 1.0),
                ),
            ));

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
                let size = label.size.map_or(font_size, |s| px(s * inv));
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
                    let _ = shaped.paint(p, line_height, TextAlign::Left, None, window, cx);
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

            // 5) picker / fork / message overlay.
            for q in &frame.picker_quads {
                paint_quad(window, origin, inv, q, shadow_rgb);
            }
            for label in &frame.picker_labels {
                let runs = [TextRun {
                    len: label.text.len(),
                    font: font.clone(),
                    color: label.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let size = label.size.map_or(font_size, |s| px(s * inv));
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
                    let _ = shaped.paint(p, line_height, TextAlign::Left, None, window, cx);
                });
            }
        });

        self.dirty = false;
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
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Explicit trait call: gpui's `Window` has an inherent `window_handle()`
    // (returning `AnyWindowHandle`) that would otherwise shadow the trait's.
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    unsafe {
        let ns_view = appkit.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
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
        renderer::Shadow::Soft => Some((0.10, 1.0, 3.0)),
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
    font_size: Pixels,
    line_height: Pixels,
    cell_height: f32,
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
        let size = label.size.map_or(m.font_size, |s| px(s * m.inv));
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
            let _ = shaped.paint(p, m.line_height, TextAlign::Left, None, window, cx);
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
    /// True while a selection drag is in flight.
    selecting: bool,
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

    fn on_mouse_down(&mut self, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let panel = self.panel_rect();
        let tab_bar = workspace::flyover_tab_bar(&panel, scale);
        let content = workspace::flyover_content(&panel, scale);
        let cell = self.renderer.cell_at(&content, mx, my);
        let mut selecting = false;
        self.app.update(cx, |app, _| {
            let n = app.flyover_tabs.len();
            if n == 0 {
                return;
            }
            if tab_bar.contains(mx, my) {
                let tr = workspace::flyover_tab_rect(&panel, 0, n, scale, false);
                let ti = (((mx - tr.x).max(0.0) / tr.w).floor() as usize).min(n - 1);
                if workspace::flyover_tab_close_rect(&panel, ti, n, scale, false).contains(mx, my) {
                    app.close_flyover_tab(ti);
                    return;
                }
                app.flyover_active = ti;
                app.flyover_mark_read();
                app.request_redraw();
            } else if let Some((col, row)) = cell {
                if let Some(tab) = app.flyover_tabs.get(app.flyover_active) {
                    tab.session.begin_selection(col, row);
                    selecting = true;
                }
                app.request_redraw();
            }
        });
        self.selecting = selecting;
        cx.notify();
    }

    fn on_mouse_move(&mut self, cx: &mut Context<Self>) {
        let scale = self.renderer.scale;
        let (mx, my) = (self.cursor.0 as f32, self.cursor.1 as f32);
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
                if let Some(tab) = app.flyover_tabs.get(app.flyover_active) {
                    tab.session.update_selection(col, row);
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
            if let Some(tab) = app.flyover_tabs.get(app.flyover_active) {
                let up = steps > 0;
                if tab.session.app_consumes_wheel() {
                    let (col, row) = cell.unwrap_or((0, 0));
                    for _ in 0..steps.unsigned_abs() {
                        tab.session.forward_wheel(up, col, row);
                    }
                } else {
                    tab.session.scroll_by(steps * 3);
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
        if scale != self.renderer.scale {
            let cell_width = renderer::measure_cell_width(window, scale);
            self.renderer.update_scale(scale, cell_width);
        }
        self.renderer.resize(phys_w, phys_h);

        let panel = self.panel_rect();
        let content = workspace::flyover_content(&panel, scale);
        let (cols, rows) = self.renderer.grid_size_for(&content);
        let (cw, ch) = (self.renderer.cell_width as u16, self.renderer.cell_height as u16);
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
                    tab.session.resize(cols, rows, cw, ch, dpi);
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
                this.on_mouse_move(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    let s = f64::from(this.renderer.scale);
                    this.cursor = (f64::from(ev.position.x) * s, f64::from(ev.position.y) * s);
                    this.on_mouse_down(cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _window, cx| {
                    this.selecting = false;
                    cx.notify();
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
            let cell_width = renderer::measure_cell_width(window, scale);
            cx.new(|cx| FlyoverPopout {
                app: app_for_view.clone(),
                renderer: Renderer::new(scale, cell_width, 0, 0),
                focus_handle: cx.focus_handle(),
                cursor: (0.0, 0.0),
                selecting: false,
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
    let app = Application::with_platform(platform).with_quit_mode(QuitMode::LastWindowClosed);
    app.run(|cx: &mut GpuiApp| {
        let bounds = Bounds::centered(None, gpui::size(px(1200.0), px(720.0)), cx);
        let (events_tx, events_rx) = mpsc::channel::<TermEvent>();

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
                    traffic_light_position: None,
                }),
                is_resizable: true,
                app_owns_titlebar_drag: true,
                ..Default::default()
            },
            |window, cx| {
                #[cfg(target_os = "macos")]
                hide_titlebar_decoration(window);

                let scale = window.scale_factor();
                // Measure a monospace cell at the default font size.
                let cell_width = renderer::measure_cell_width(window, scale);
                let phys_w = (f32::from(window.viewport_size().width) * scale) as u32;
                let phys_h = (f32::from(window.viewport_size().height) * scale) as u32;
                let renderer = Renderer::new(scale, cell_width, phys_w.max(1), phys_h.max(1));

                let entity = cx.new(|cx| {
                    let mut app = App {
                        events_rx,
                        events_tx: events_tx.clone(),
                        renderer,
                        workspaces: Vec::new(),
                        active: 0,
                        sections: Vec::new(),
                        next_section_id: 0,
                        next_session_id: 0,
                        next_tile_id: 0,
                        sidebar_expanded_w: workspace::SIDEBAR_DEFAULT_W,
                        sidebar_collapsed: false,
                        modifiers: Modifiers::default(),
                        title: String::new(),
                        cursor: (0.0, 0.0),
                        drag: Drag::None,
                        just_expanded: None,
                        picker: None,
                        fork: None,
                        profile_picker: None,
                        profile_next: None,
                        pending_group_profile: None,
                        save_ws: None,
                        palette: None,
                        message: None,
                        confirm: None,
                        pending_primary_cmd: std::collections::HashMap::new(),
                        editing_command: None,
                        editing_section: None,
                        // Single focus handle, minted once; focused below.
                        focus_handle: cx.focus_handle(),
                        dirty: true,
                        scroll_accum: 0.0,
                        page: Page::Sessions,
                        section: Section::Keyboard,
                        recording: None,
                        // The active page's slot starts fully glyphed.
                        dot_anim: {
                            let mut v = vec![0.0; Page::ALL.len()];
                            v[Page::Sessions.index()] = 1.0;
                            v
                        },
                        dot_hover: None,
                        resize_hover: None,
                        cleanup: {
                            let mut c = cleanup::Cleanup::default();
                            if let Some(s) = settings::get_str("cleanup.columns") {
                                c.col_fracs = cleanup::parse_col_fracs(&s);
                            }
                            c
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
                        picker_target: PickerTarget::Group,
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
                            let (redraw, want_popout, popout) =
                                app.update(cx, |app: &mut App, cx| {
                                    let redraw = app.drain_events();
                                    if redraw {
                                        cx.notify();
                                    }
                                    (
                                        redraw,
                                        app.flyover_windowed && app.flyover_window_visible,
                                        app.flyover_window,
                                    )
                                });
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
                            entity.update(cx, |app, cx| {
                                app.request_redraw();
                                cx.notify();
                            });
                        }
                    })
                    .detach();

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
