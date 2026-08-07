//! pwrde — a GPU-accelerated terminal workspace for macOS.
//!
//! Window layout: borderless window (native traffic lights float over the
//! sidebar's top strip, which doubles as the window drag handle), a resizable
//! vertical-tab sidebar (one tab per *group*), and a binary split tree of
//! tiles — each tile has a horizontal tab strip (cmux-style).
//!
//! Shortcuts:
//!   ⌘D split side-by-side   ⇧⌘D split stacked    ⌘T new tab in tile
//!   ⇧⌘T new group           ⌘W close tab         ⌘1–⌘9 switch group
//!   ⌘]/⌘[ cycle tile focus  ⇧⌘]/⇧⌘[ next/prev tab   ⌘Q quit
//!
//! Mouse: drag dividers to resize splits; drag the sidebar edge to resize it;
//! drag a tile tab to reorder, move to another tile, drop on a tile edge to
//! split it out, or drop on a sidebar group to send it there.

mod rect;
mod renderer;
mod term;
mod workspace;

use std::borrow::Cow;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use renderer::Renderer;
use term::{Session, TermEvent};
use workspace::{Dir, LayoutRect, Node, Tab, Tile, Workspace};

/// How far a pressed tab must move before it becomes a drag (physical px).
const DRAG_THRESHOLD: f64 = 6.0;
/// Hit slop around dividers/edges (logical px).
const GRAB: f32 = 3.0;

enum Drag {
    None,
    /// Resizing a split; `path` addresses the Split node in the tree.
    Divider { path: Vec<u8> },
    /// Resizing the sidebar.
    Sidebar,
    /// Mouse down on a tile tab; becomes `Tab` after the threshold.
    TabPress { tile: u64, tab: usize, start: (f64, f64) },
    /// Dragging a tile tab.
    Tab { tile: u64, tab: usize },
}

#[derive(Clone, Copy, Debug)]
enum DropTarget {
    /// Insert into a tile's tab strip at `index`.
    TabBar { tile: u64, index: usize },
    /// Split `tile` in `dir`; `first` puts the dropped tab on the left/top.
    Edge { tile: u64, dir: Dir, first: bool },
    /// Append to a tile's tabs.
    Center { tile: u64 },
    /// Move to another group (its focused tile).
    Group { ws: usize },
}

struct App {
    proxy: EventLoopProxy<TermEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    workspaces: Vec<Workspace>,
    active: usize,
    next_session_id: u64,
    next_tile_id: u64,
    /// Sidebar width, logical px (user-resizable).
    sidebar_w: f32,
    modifiers: ModifiersState,
    title: String,
    cursor: (f64, f64),
    drag: Drag,
}

impl App {
    fn scale(&self) -> f32 {
        self.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32)
    }

    fn dpi(&self) -> u32 {
        (96.0 * self.scale()) as u32
    }

    fn spawn_session(&mut self) -> Session {
        let (cell_w, cell_h) = self
            .renderer
            .as_ref()
            .map_or((9, 19), |r| (r.cell_width as u16, r.cell_height as u16));
        let id = self.next_session_id;
        self.next_session_id += 1;
        if std::env::var_os("PWRDE_DEBUG").is_some() {
            eprintln!("spawn_session id={id}\n{}", std::backtrace::Backtrace::force_capture());
        }
        // Spawned at a nominal size; sync_layout() immediately corrects it.
        Session::new(id, 80, 24, cell_w, cell_h, self.dpi(), self.proxy.clone())
    }

    fn new_tile(&mut self) -> Tile {
        let session = self.spawn_session();
        let id = self.next_tile_id;
        self.next_tile_id += 1;
        Tile::new(id, session)
    }

    fn area(&self) -> LayoutRect {
        let renderer = self.renderer.as_ref().expect("renderer");
        let (w, h) = renderer.surface_size();
        workspace::terminal_area(w, h, renderer.scale, self.sidebar_w)
    }

    /// Recompute the active workspace's tile layout and push size changes to
    /// each visible PTY. Cheap when nothing changed (sizes cached per tab).
    fn sync_layout(&mut self) {
        let Some(renderer) = &self.renderer else { return };
        let area = self.area();
        let scale = renderer.scale;
        let (cell_w, cell_h) = (renderer.cell_width as u16, renderer.cell_height as u16);
        let dpi = (96.0 * scale) as u32;
        let ws = &mut self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, area, scale);
        for (id, rect) in &tiles {
            let content = workspace::tile_content(rect, scale);
            let (cols, rows) = renderer.grid_size_for(&content);
            if let Some(tab) = ws.root.find_tile_mut(*id).and_then(|t| t.active_tab_mut())
                && (cols, rows) != (tab.cols, tab.rows)
            {
                tab.cols = cols;
                tab.rows = rows;
                tab.session.resize(cols, rows, cell_w, cell_h, dpi);
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
    }

    fn new_tab(&mut self) {
        let session = self.spawn_session();
        let ws = &mut self.workspaces[self.active];
        if let Some(tile) = ws.focused_mut() {
            tile.tabs.push(Tab::new(session));
            tile.active = tile.tabs.len() - 1;
        }
        self.sync_layout();
        self.request_redraw();
    }

    fn add_workspace(&mut self) {
        let tile = self.new_tile();
        let n = self.workspaces.len() + 1;
        self.workspaces.push(Workspace::new(format!("group {n}"), tile));
        self.active = self.workspaces.len() - 1;
        self.sync_layout();
        self.request_redraw();
    }

    /// Remove tab `tab` from tile `tile` in workspace `wi`, cascading empty
    /// tiles/groups. Returns the removed Tab (unless the app exited).
    fn take_tab(
        &mut self,
        wi: usize,
        tile_id: u64,
        tab_idx: usize,
        event_loop: &ActiveEventLoop,
    ) -> Option<Tab> {
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
            if !ws.root.remove_tile(tile_id) {
                // Root leaf emptied: the group is empty.
                if self.workspaces.len() > 1 {
                    self.workspaces.remove(wi);
                    if self.active >= wi {
                        self.active = self.active.saturating_sub(1);
                    }
                } else {
                    event_loop.exit();
                    return Some(tab);
                }
            } else {
                ws.fix_focus();
            }
        }
        if let Some(ws) = self.workspaces.get_mut(wi) {
            ws.fix_focus();
        }
        self.sync_layout();
        self.request_redraw();
        Some(tab)
    }

    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        let ws = &self.workspaces[self.active];
        let tile_id = ws.focused_tile;
        let Some(tab_idx) = ws.focused().map(|t| t.active) else { return };
        // Dropping the Tab closes the PTY; the shell exits on hangup and the
        // later `Exit` event finds nothing.
        let _ = self.take_tab(self.active, tile_id, tab_idx, event_loop);
    }

    /// A shell exited on its own: remove its tab wherever it lives.
    fn remove_session(&mut self, id: u64, event_loop: &ActiveEventLoop) {
        for wi in 0..self.workspaces.len() {
            let found = self.workspaces[wi].root.tiles().iter().find_map(|tile| {
                tile.tabs
                    .iter()
                    .position(|t| t.session.id == id)
                    .map(|ti| (tile.id, ti))
            });
            if let Some((tile_id, tab_idx)) = found {
                let _ = self.take_tab(wi, tile_id, tab_idx, event_loop);
                return;
            }
        }
    }

    fn switch_workspace(&mut self, index: usize) {
        if index < self.workspaces.len() && index != self.active {
            self.active = index;
            // The window may have resized while this group was inactive.
            self.sync_layout();
            self.request_redraw();
        }
    }

    fn cycle_tile(&mut self, delta: isize) {
        let ws = &mut self.workspaces[self.active];
        let ids: Vec<u64> = ws.root.tiles().iter().map(|t| t.id).collect();
        if ids.len() > 1 {
            let cur = ids.iter().position(|&i| i == ws.focused_tile).unwrap_or(0);
            let next = (cur as isize + delta).rem_euclid(ids.len() as isize) as usize;
            ws.focused_tile = ids[next];
            self.request_redraw();
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let ws = &mut self.workspaces[self.active];
        if let Some(tile) = ws.focused_mut()
            && tile.tabs.len() > 1
        {
            tile.active =
                (tile.active as isize + delta).rem_euclid(tile.tabs.len() as isize) as usize;
        }
        self.sync_layout();
        self.request_redraw();
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// True when the session is the *visible* tab of a tile in the active
    /// workspace.
    fn is_visible(&self, id: u64) -> bool {
        self.workspaces[self.active]
            .root
            .tiles()
            .iter()
            .any(|t| t.active_tab().is_some_and(|tab| tab.session.id == id))
    }

    // ── Mouse ──────────────────────────────────────────────────────────

    fn resolve_drop(&self, px: f32, py: f32) -> Option<DropTarget> {
        let renderer = self.renderer.as_ref()?;
        let scale = renderer.scale;
        let (_, h) = renderer.surface_size();

        if workspace::sidebar(h, scale, self.sidebar_w).contains(px, py) {
            for wi in 0..self.workspaces.len() {
                if workspace::tab_rect(wi, scale, self.sidebar_w).contains(px, py) {
                    return Some(DropTarget::Group { ws: wi });
                }
            }
            return None;
        }

        let ws = &self.workspaces[self.active];
        let (tiles, _) = workspace::layout_tiles(&ws.root, self.area(), scale);
        for (id, rect) in &tiles {
            if !rect.contains(px, py) {
                continue;
            }
            let bar = workspace::tile_tab_bar(rect, scale);
            if bar.contains(px, py) {
                let n = ws.root.find_tile(*id).map_or(0, |t| t.tabs.len());
                let tab_w = workspace::tile_tab_rect(rect, 0, n.max(1), scale).w;
                let index = (((px - bar.x) / tab_w).floor() as usize).min(n);
                return Some(DropTarget::TabBar { tile: *id, index });
            }
            let content = workspace::tile_content(rect, scale);
            let rx = (px - content.x) / content.w;
            let ry = (py - content.y) / content.h;
            return Some(if rx < 0.25 {
                DropTarget::Edge { tile: *id, dir: Dir::Row, first: true }
            } else if rx > 0.75 {
                DropTarget::Edge { tile: *id, dir: Dir::Row, first: false }
            } else if ry < 0.25 {
                DropTarget::Edge { tile: *id, dir: Dir::Column, first: true }
            } else if ry > 0.75 {
                DropTarget::Edge { tile: *id, dir: Dir::Column, first: false }
            } else {
                DropTarget::Center { tile: *id }
            });
        }
        None
    }

    fn drop_hint(&self, target: DropTarget) -> Option<LayoutRect> {
        let renderer = self.renderer.as_ref()?;
        let scale = renderer.scale;
        match target {
            DropTarget::Group { ws } => Some(workspace::tab_rect(ws, scale, self.sidebar_w)),
            DropTarget::TabBar { tile, .. } | DropTarget::Center { tile }
            | DropTarget::Edge { tile, .. } => {
                let wsp = &self.workspaces[self.active];
                let (tiles, _) = workspace::layout_tiles(&wsp.root, self.area(), scale);
                let (_, rect) = tiles.into_iter().find(|(id, _)| *id == tile)?;
                Some(match target {
                    DropTarget::TabBar { .. } => workspace::tile_tab_bar(&rect, scale),
                    DropTarget::Center { .. } => workspace::tile_content(&rect, scale),
                    DropTarget::Edge { dir, first, .. } => {
                        let c = workspace::tile_content(&rect, scale);
                        match (dir, first) {
                            (Dir::Row, true) => LayoutRect { w: c.w / 2.0, ..c },
                            (Dir::Row, false) => {
                                LayoutRect { x: c.x + c.w / 2.0, w: c.w / 2.0, ..c }
                            },
                            (Dir::Column, true) => LayoutRect { h: c.h / 2.0, ..c },
                            (Dir::Column, false) => {
                                LayoutRect { y: c.y + c.h / 2.0, h: c.h / 2.0, ..c }
                            },
                        }
                    },
                    _ => unreachable!(),
                })
            },
        }
    }

    fn apply_drop(
        &mut self,
        src_tile: u64,
        src_tab: usize,
        target: DropTarget,
        event_loop: &ActiveEventLoop,
    ) {
        // No-op guards: dropping a tab onto itself.
        let src_len = self.workspaces[self.active]
            .root
            .find_tile(src_tile)
            .map_or(0, |t| t.tabs.len());
        match target {
            DropTarget::Center { tile } if tile == src_tile => return,
            DropTarget::Edge { tile, .. } if tile == src_tile && src_len <= 1 => return,
            DropTarget::Group { ws } if ws == self.active && src_len <= 1 => {
                // Moving the only tab of the only tile to its own group: noop
                // when the group holds just that tile.
                let ws_ref = &self.workspaces[self.active];
                if ws_ref.root.tiles().len() == 1 {
                    return;
                }
            },
            _ => {},
        }

        let Some(mut tab) = self.take_tab(self.active, src_tile, src_tab, event_loop) else {
            return;
        };

        match target {
            DropTarget::TabBar { tile, index } => {
                if let Some(t) = self.workspaces[self.active].root.find_tile_mut(tile) {
                    let index = index.min(t.tabs.len());
                    t.tabs.insert(index, tab);
                    t.active = index;
                    self.workspaces[self.active].focused_tile = tile;
                } // else: tree changed underneath us; tab is dropped (shell dies)
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
                tab.cols = 0; // force resize at new geometry
                let new_tile = Tile { id, tabs: vec![tab], active: 0 };
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
        }
        self.sync_layout();
        self.request_redraw();
    }

    fn on_mouse_down(&mut self, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
        let Some(renderer) = &self.renderer else { return };
        let scale = renderer.scale;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (_, h) = renderer.surface_size();
        let grab = GRAB * scale;
        let sidebar = workspace::sidebar(h, scale, self.sidebar_w);

        // Sidebar edge → resize sidebar.
        if (px - sidebar.w).abs() <= grab {
            self.drag = Drag::Sidebar;
            return;
        }

        // Split dividers → resize splits.
        let ws = &self.workspaces[self.active];
        let (tiles, dividers) = workspace::layout_tiles(&ws.root, self.area(), scale);
        if let Some(d) = dividers.iter().find(|d| d.rect.inflate(grab).contains(px, py)) {
            self.drag = Drag::Divider { path: d.path.clone() };
            return;
        }

        // Sidebar: titlebar strip = traffic lights + window drag handle.
        if sidebar.contains(px, py) {
            if workspace::titlebar(scale, self.sidebar_w).contains(px, py) {
                let grab_pad = 4.0 * scale;
                let hit =
                    (0..3).find(|&i| workspace::traffic_light(i, scale).inflate(grab_pad).contains(px, py));
                match hit {
                    Some(0) => event_loop.exit(),
                    Some(1) => {
                        if let Some(window) = &self.window {
                            window.set_minimized(true);
                        }
                    },
                    Some(2) => {
                        if let Some(window) = &self.window {
                            window.set_maximized(!window.is_maximized());
                        }
                    },
                    _ => {
                        if let Some(window) = &self.window {
                            let _ = window.drag_window();
                        }
                    },
                }
                return;
            }
            for wi in 0..self.workspaces.len() {
                if workspace::tab_rect(wi, scale, self.sidebar_w).contains(px, py) {
                    self.switch_workspace(wi);
                    return;
                }
            }
            return;
        }

        // Tiles: tab strip press (activate + arm drag) or content focus.
        for (id, rect) in &tiles {
            if !rect.contains(px, py) {
                continue;
            }
            let ws = &mut self.workspaces[self.active];
            let bar = workspace::tile_tab_bar(rect, scale);
            if bar.contains(px, py) {
                if let Some(tile) = ws.root.find_tile_mut(*id) {
                    let n = tile.tabs.len();
                    let tab_w = workspace::tile_tab_rect(rect, 0, n.max(1), scale).w;
                    let ti = (((px - bar.x) / tab_w).floor() as usize).min(n.saturating_sub(1));
                    tile.active = ti;
                    ws.focused_tile = *id;
                    self.drag = Drag::TabPress { tile: *id, tab: ti, start: self.cursor };
                    self.sync_layout();
                    self.request_redraw();
                }
            } else if ws.focused_tile != *id {
                ws.focused_tile = *id;
                self.request_redraw();
            }
            return;
        }
    }

    fn on_mouse_move(&mut self) {
        let Some(renderer) = &self.renderer else { return };
        let scale = renderer.scale;
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);

        match &self.drag {
            Drag::Sidebar => {
                self.sidebar_w =
                    (px / scale).clamp(workspace::SIDEBAR_MIN_W, workspace::SIDEBAR_MAX_W);
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
            Drag::None => {
                // Hover affordances: resize cursors near grabbable edges.
                let (_, h) = renderer.surface_size();
                let grab = GRAB * scale;
                let sidebar = workspace::sidebar(h, scale, self.sidebar_w);
                let ws = &self.workspaces[self.active];
                let (_, dividers) = workspace::layout_tiles(&ws.root, self.area(), scale);
                let icon = if (px - sidebar.w).abs() <= grab {
                    CursorIcon::ColResize
                } else if let Some(d) =
                    dividers.iter().find(|d| d.rect.inflate(grab).contains(px, py))
                {
                    match d.dir {
                        Dir::Row => CursorIcon::ColResize,
                        Dir::Column => CursorIcon::RowResize,
                    }
                } else {
                    CursorIcon::Default
                };
                if let Some(window) = &self.window {
                    window.set_cursor(winit::window::Cursor::Icon(icon));
                }
            },
        }
    }

    fn on_mouse_up(&mut self, event_loop: &ActiveEventLoop) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Tab { tile, tab } => {
                let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if let Some(target) = self.resolve_drop(px, py) {
                    self.apply_drop(tile, tab, target, event_loop);
                }
                self.request_redraw();
            },
            _ => {},
        }
    }

    fn handle_shortcut(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        let shift = self.modifiers.shift_key();
        if let Key::Character(text) = &event.logical_key {
            match (text.to_lowercase().as_str(), shift) {
                ("d", false) => self.split(Dir::Row),
                ("d", true) => self.split(Dir::Column),
                ("t", false) => self.new_tab(),
                ("t", true) => self.add_workspace(),
                ("w", _) => self.close_active_tab(event_loop),
                ("q", _) => event_loop.exit(),
                ("[" | "{", false) => self.cycle_tile(-1),
                ("]" | "}", false) => self.cycle_tile(1),
                ("[" | "{", true) => self.cycle_tab(-1),
                ("]" | "}", true) => self.cycle_tab(1),
                (s, _) => {
                    if let Some(d) = s.chars().next().and_then(|c| c.to_digit(10))
                        && d >= 1
                    {
                        self.switch_workspace(d as usize - 1);
                    }
                },
            }
        }
    }
}

impl ApplicationHandler<TermEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Fully borderless: we draw our own traffic lights and confine the
        // window-drag region to the sidebar's top strip. (A transparent
        // titlebar would leave AppKit intercepting drags across the whole
        // top edge — including tile tabs.)
        let attrs = Window::default_attributes()
            .with_title("pwrde")
            .with_decorations(false)
            .with_transparent(true) // rounded corners composite over the desktop
            .with_inner_size(LogicalSize::new(1200.0, 720.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.window = Some(Arc::clone(&window));
        self.renderer = Some(Renderer::new(window));

        let tile = self.new_tile();
        self.workspaces.push(Workspace::new("group 1".into(), tile));
        self.sync_layout();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TermEvent) {
        match event {
            // Grid changed: coalesced into at most one redraw per frame.
            // Output in hidden tabs/groups still advances its grid; it just
            // doesn't wake the renderer.
            TermEvent::Wakeup(id) => {
                if self.is_visible(id) {
                    // Keep the window title in sync with the focused tab.
                    let ws = &self.workspaces[self.active];
                    if let Some(tab) = ws.focused().and_then(|t| t.active_tab())
                        && tab.session.id == id
                    {
                        let title = tab.session.title();
                        if title != self.title {
                            if let Some(window) = &self.window {
                                window.set_title(if title.is_empty() { "pwrde" } else { &title });
                            }
                            self.title = title;
                        }
                    }
                    self.request_redraw();
                }
            },
            TermEvent::Exit(id) => self.remove_session(id, event_loop),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                self.sync_layout();
                self.request_redraw();
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                self.on_mouse_move();
            },
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => self.on_mouse_down(event_loop),
                ElementState::Released => self.on_mouse_up(event_loop),
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.modifiers.super_key() {
                    self.handle_shortcut(&event, event_loop);
                    return;
                }
                let ws = &self.workspaces[self.active];
                if let Some(tab) = ws.focused().and_then(|t| t.active_tab())
                    && let Some(bytes) = key_to_bytes(&event, self.modifiers)
                {
                    tab.session.write(bytes);
                }
            },
            WindowEvent::RedrawRequested => {
                let hint = if let Drag::Tab { .. } = self.drag {
                    self.resolve_drop(self.cursor.0 as f32, self.cursor.1 as f32)
                        .and_then(|t| self.drop_hint(t))
                } else {
                    None
                };
                if let Some(renderer) = &mut self.renderer
                    && !self.workspaces.is_empty()
                {
                    renderer.draw(&self.workspaces, self.active, self.sidebar_w, hint);
                }
            },
            _ => {},
        }
    }
}

/// Encode a key press as the byte sequence the PTY expects.
fn key_to_bytes(event: &KeyEvent, mods: ModifiersState) -> Option<Cow<'static, [u8]>> {
    match &event.logical_key {
        Key::Named(named) => {
            let bytes: &'static [u8] = match named {
                NamedKey::Enter => b"\r",
                NamedKey::Backspace => b"\x7f",
                NamedKey::Tab => b"\t",
                NamedKey::Escape => b"\x1b",
                NamedKey::Space => b" ",
                NamedKey::ArrowUp => b"\x1b[A",
                NamedKey::ArrowDown => b"\x1b[B",
                NamedKey::ArrowRight => b"\x1b[C",
                NamedKey::ArrowLeft => b"\x1b[D",
                NamedKey::Home => b"\x1b[H",
                NamedKey::End => b"\x1b[F",
                NamedKey::PageUp => b"\x1b[5~",
                NamedKey::PageDown => b"\x1b[6~",
                NamedKey::Delete => b"\x1b[3~",
                _ => return None,
            };
            Some(Cow::Borrowed(bytes))
        },
        Key::Character(text) => {
            // Ctrl+letter → C0 control byte (Ctrl+C = 0x03, etc.).
            if mods.control_key() {
                let ch = text.chars().next()?;
                if ch.is_ascii_alphabetic() || "[\\]^_@".contains(ch) {
                    return Some(Cow::Owned(vec![ch.to_ascii_uppercase() as u8 & 0x1f]));
                }
            }
            // Alt as Meta: ESC-prefix the character (readline word motions).
            if mods.alt_key() {
                let mut bytes = vec![0x1b];
                bytes.extend_from_slice(text.as_str().as_bytes());
                return Some(Cow::Owned(bytes));
            }
            Some(Cow::Owned(text.as_str().as_bytes().to_vec()))
        },
        _ => None,
    }
}

fn main() {
    let event_loop = EventLoop::<TermEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        proxy: event_loop.create_proxy(),
        window: None,
        renderer: None,
        workspaces: Vec::new(),
        active: 0,
        next_session_id: 0,
        next_tile_id: 0,
        sidebar_w: 170.0,
        modifiers: ModifiersState::default(),
        title: String::new(),
        cursor: (0.0, 0.0),
        drag: Drag::None,
    };
    event_loop.run_app(&mut app).expect("run event loop");
}
