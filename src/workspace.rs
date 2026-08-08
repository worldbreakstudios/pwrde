//! Workspace model + layout math.
//!
//! A `Workspace` (a "group") is one vertical tab in the sidebar and owns a
//! binary *split tree*. Leaves are `Tile`s — each tile has a horizontal tab
//! bar at the top holding one or more terminal tabs (cmux-style). Splits
//! carry a draggable `ratio`.
//!
//! Layout is pure math over the window size so the renderer (drawing) and
//! the app (hit-testing, PTY resize, divider dragging) always agree.

use crate::term::Session;

pub struct Tab {
    pub session: Session,
    /// Cached grid size; used to skip redundant PTY resizes.
    pub cols: usize,
    pub rows: usize,
}

impl Tab {
    pub fn new(session: Session) -> Self {
        Self { session, cols: 0, rows: 0 }
    }
}

/// A leaf of the split tree: a tab strip + the active tab's terminal.
pub struct Tile {
    pub id: u64,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Tile {
    pub fn new(id: u64, session: Session) -> Self {
        Self { id, tabs: vec![Tab::new(session)], active: 0 }
    }

    pub fn empty(id: u64) -> Self {
        Self { id, tabs: Vec::new(), active: 0 }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Side-by-side (children left|right) — a "vertical split".
    Row,
    /// Stacked (children top/bottom) — a "horizontal split".
    Column,
}

pub enum Node {
    Leaf(Tile),
    Split { dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub fn tiles(&self) -> Vec<&Tile> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Tile>) {
        match self {
            Node::Leaf(t) => out.push(t),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            },
        }
    }

    pub fn tiles_mut(&mut self) -> Vec<&mut Tile> {
        let mut out = Vec::new();
        self.collect_mut(&mut out);
        out
    }

    fn collect_mut<'a>(&'a mut self, out: &mut Vec<&'a mut Tile>) {
        match self {
            Node::Leaf(t) => out.push(t),
            Node::Split { a, b, .. } => {
                a.collect_mut(out);
                b.collect_mut(out);
            },
        }
    }

    pub fn find_tile(&self, id: u64) -> Option<&Tile> {
        self.tiles().into_iter().find(|t| t.id == id)
    }

    pub fn find_tile_mut(&mut self, id: u64) -> Option<&mut Tile> {
        self.tiles_mut().into_iter().find(|t| t.id == id)
    }

    /// Replace the leaf `id` with a split of the old leaf and `new`;
    /// `new_first` puts the new tile on the left/top side.
    pub fn split_tile(&mut self, id: u64, dir: Dir, new: &mut Option<Tile>, new_first: bool) -> bool {
        match self {
            Node::Leaf(t) if t.id == id => {
                let Some(new_tile) = new.take() else { return false };
                let old = std::mem::replace(self, Node::Leaf(Tile::empty(u64::MAX)));
                let new_leaf = Node::Leaf(new_tile);
                let (a, b) = if new_first { (new_leaf, old) } else { (old, new_leaf) };
                *self = Node::Split { dir, ratio: 0.5, a: Box::new(a), b: Box::new(b) };
                true
            },
            Node::Split { a, b, .. } => {
                a.split_tile(id, dir, new, new_first) || b.split_tile(id, dir, new, new_first)
            },
            Node::Leaf(_) => false,
        }
    }

    /// Remove the leaf `id`, promoting its sibling. Returns false if `id` is
    /// the root leaf (caller decides what an empty workspace means).
    pub fn remove_tile(&mut self, id: u64) -> bool {
        if let Node::Split { a, b, .. } = self {
            let hit_a = matches!(&**a, Node::Leaf(t) if t.id == id);
            let hit_b = matches!(&**b, Node::Leaf(t) if t.id == id);
            if hit_a || hit_b {
                let survivor = if hit_a { b } else { a };
                let survivor =
                    std::mem::replace(&mut **survivor, Node::Leaf(Tile::empty(u64::MAX)));
                *self = survivor;
                return true;
            }
            return a.remove_tile(id) || b.remove_tile(id);
        }
        false
    }

    /// Walk a path of 0 (first child) / 1 (second child) steps.
    pub fn node_at_path_mut(&mut self, path: &[u8]) -> Option<&mut Node> {
        let mut node = self;
        for step in path {
            match node {
                Node::Split { a, b, .. } => {
                    node = if *step == 0 { a } else { b };
                },
                Node::Leaf(_) => return None,
            }
        }
        Some(node)
    }
}

pub struct Workspace {
    pub name: String,
    pub root: Node,
    pub focused_tile: u64,
    /// Directory every shell opened in this group starts in. `None` inherits
    /// the directory pwrde itself was launched from.
    pub cwd: Option<std::path::PathBuf>,
    /// The first tile spawned with the group. Closing it closes the whole
    /// group (after a confirmation), so it anchors the group's lifetime.
    pub primary_tile: u64,
}

impl Workspace {
    /// Create a group holding a single tile, rooted at `cwd` (`None` = inherit
    /// the process launch directory). The founding tile becomes the primary.
    pub fn new(name: String, tile: Tile, cwd: Option<std::path::PathBuf>) -> Self {
        let focused_tile = tile.id;
        Self { name, root: Node::Leaf(tile), focused_tile, cwd, primary_tile: focused_tile }
    }

    /// The tab-less workspace shown behind the empty-state CTA. Kept around
    /// (instead of allowing `workspaces` to be empty) because much of the app
    /// indexes `workspaces[active]` unguarded.
    pub fn placeholder() -> Self {
        Self::new("group 1".into(), Tile::empty(0), None)
    }

    /// True when no tile has any tab — the empty-state predicate.
    pub fn is_empty(&self) -> bool {
        self.root.tiles().iter().all(|t| t.tabs.is_empty())
    }

    pub fn focused(&self) -> Option<&Tile> {
        self.root.find_tile(self.focused_tile)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Tile> {
        self.root.find_tile_mut(self.focused_tile)
    }

    /// Sidebar display title: the primary pane's active-tab title, so the
    /// group card always tracks what its founding pane is running. Falls
    /// back to the static group `name` while that pane has no title yet.
    pub fn title(&self) -> String {
        self.root
            .find_tile(self.primary_tile)
            .and_then(|t| t.active_tab())
            .map(|tab| tab.session.title())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.name.clone())
    }

    /// Ensure `focused_tile` points at an existing tile.
    pub fn fix_focus(&mut self) {
        if self.root.find_tile(self.focused_tile).is_none()
            && let Some(first) = self.root.tiles().first()
        {
            self.focused_tile = first.id;
        }
    }
}

/// Display form of a path with a leading `$HOME` shortened to `~`.
pub fn display_path(dir: &std::path::Path, home: Option<&std::path::Path>) -> String {
    if let Some(home) = home {
        if dir == home {
            return "~".into();
        }
        if let Ok(rest) = dir.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    dir.display().to_string()
}

/// Display form of a group's cwd for the sidebar card's second line. `None`
/// (inherit) falls back to the process working directory.
pub fn display_cwd(cwd: Option<&std::path::Path>) -> String {
    let fallback;
    let dir = match cwd {
        Some(d) => d,
        None => {
            fallback = std::env::current_dir().unwrap_or_default();
            &fallback
        },
    };
    display_path(dir, dirs::home_dir().as_deref())
}

// ─── Layout ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LayoutRect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// Grown by `m` on every side (for forgiving divider hit-tests).
    pub fn inflate(&self, m: f32) -> LayoutRect {
        LayoutRect { x: self.x - m, y: self.y - m, w: self.w + 2.0 * m, h: self.h + 2.0 * m }
    }
}

/// Logical (pre-scale) dimensions.
pub const SIDEBAR_MIN_W: f32 = 120.0;
pub const SIDEBAR_DEFAULT_W: f32 = 240.0;
pub const SIDEBAR_MAX_W: f32 = 360.0;
/// Top strip of the sidebar: native traffic lights float here and the rest
/// is the window drag handle.
pub const TITLEBAR_H: f32 = 44.0;
const TAB_H: f32 = 48.0;
/// Vertical gap between the sidebar's rounded group rows.
const TAB_GAP: f32 = 3.0;
/// Horizontal inset of the sidebar's rows from the sidebar edges.
const SIDEBAR_PAD: f32 = 10.0;
/// Height of the "+" new-group button between the titlebar and the group tabs.
const NEW_GROUP_H: f32 = 30.0;
/// Height of the horizontal tab strip atop each tile.
const TILE_TAB_H: f32 = 28.0;
/// Gap between tile cards; doubles as the divider drag handle (hit tests
/// inflate it, so a slim gap still drags fine).
const TILE_GAP: f32 = 3.0;
/// Padding between the tile cards and the window edges (top/right/bottom).
/// Matches TILE_GAP so the outer border reads as thin as the inner dividers.
const AREA_PAD: f32 = TILE_GAP;
const TILE_TAB_MAX_W: f32 = 180.0;

/// `sidebar_w` is the user-adjustable sidebar width in logical px.
pub fn sidebar(height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    LayoutRect { x: 0.0, y: 0.0, w: (sidebar_w * scale).round(), h: height as f32 }
}

/// The traffic-light / window-drag strip at the top of the sidebar.
pub fn titlebar(scale: f32, sidebar_w: f32) -> LayoutRect {
    LayoutRect { x: 0.0, y: 0.0, w: (sidebar_w * scale).round(), h: (TITLEBAR_H * scale).round() }
}

/// The "+" new-group button, directly below the titlebar strip and above the
/// group tabs. Clicking it opens the cwd picker. Inset like the group rows so
/// it renders as a rounded field floating on the gradient.
pub fn new_group_button(scale: f32, sidebar_w: f32) -> LayoutRect {
    let pad = (SIDEBAR_PAD * scale).round();
    LayoutRect {
        x: pad,
        y: (TITLEBAR_H * scale).round(),
        w: ((sidebar_w * scale).round() - 2.0 * pad).max(0.0),
        h: (NEW_GROUP_H * scale).round(),
    }
}

/// Centered CTA used by the empty-state launch view.
pub fn empty_state_cta(width: u32, height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    let area = terminal_area(width, height, scale, sidebar_w);
    let w = (180.0 * scale).round();
    let h = (52.0 * scale).round();
    LayoutRect {
        x: area.x + ((area.w - w) / 2.0).max(0.0),
        y: area.y + ((area.h - h) / 2.0).max(0.0) - (18.0 * scale).round(),
        w,
        h,
    }
}

pub fn empty_state_hint(width: u32, height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    let cta = empty_state_cta(width, height, scale, sidebar_w);
    LayoutRect { x: cta.x, y: cta.y + cta.h + (14.0 * scale).round(), w: cta.w, h: (22.0 * scale).round() }
}

/// Group tab `index` in the sidebar, stacked below the new-group button.
/// Rows are inset from the sidebar edges (rounded pills, Arc-style).
pub fn tab_rect(index: usize, scale: f32, sidebar_w: f32) -> LayoutRect {
    let pad = (SIDEBAR_PAD * scale).round();
    let h = (TAB_H * scale).round();
    let gap = (TAB_GAP * scale).round();
    let top = ((TITLEBAR_H + NEW_GROUP_H) * scale).round() + 2.0 * gap;
    LayoutRect {
        x: pad,
        y: top + index as f32 * (h + gap),
        w: ((sidebar_w * scale).round() - 2.0 * pad).max(0.0),
        h,
    }
}

/// Height of the page-dot strip pinned to the sidebar's bottom edge.
pub const PAGE_STRIP_H: f32 = 36.0;
/// Square hit target for one page slot in the strip.
const PAGE_SLOT: f32 = 22.0;
const PAGE_SLOT_GAP: f32 = 10.0;

/// The page-dot strip across the bottom of the sidebar (present on every
/// page, so the sidebar chrome is identical everywhere).
pub fn page_strip(height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    let h = (PAGE_STRIP_H * scale).round();
    LayoutRect { x: 0.0, y: height as f32 - h, w: (sidebar_w * scale).round(), h }
}

/// Slot `i` of `n` in the page strip: a horizontally centered row of squares.
pub fn page_slot_rect(i: usize, n: usize, height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    let strip = page_strip(height, scale, sidebar_w);
    let slot = (PAGE_SLOT * scale).round();
    let gap = (PAGE_SLOT_GAP * scale).round();
    let total = n as f32 * slot + n.saturating_sub(1) as f32 * gap;
    let x0 = strip.x + ((strip.w - total) / 2.0).round();
    LayoutRect {
        x: x0 + i as f32 * (slot + gap),
        y: strip.y + ((strip.h - slot) / 2.0).round(),
        w: slot,
        h: slot,
    }
}

/// Header band inside the settings card (the section title).
pub const SETTINGS_HEADER_H: f32 = 52.0;
/// One settings row inside the card.
pub const SETTINGS_ROW_H: f32 = 36.0;
/// Inset of settings rows from the card edges.
const SETTINGS_PAD: f32 = 14.0;

/// Row `i` of the settings card `card` (which is the whole terminal area —
/// the settings page renders as one tile-style card). Shared by the renderer
/// (drawing) and main.rs (hit-testing) so clicks always agree with pixels.
pub fn settings_row_rect(card: &LayoutRect, i: usize, scale: f32) -> LayoutRect {
    let pad = (SETTINGS_PAD * scale).round();
    let header = (SETTINGS_HEADER_H * scale).round();
    let h = (SETTINGS_ROW_H * scale).round();
    LayoutRect {
        x: card.x + pad,
        y: card.y + header + i as f32 * h,
        w: (card.w - 2.0 * pad).max(0.0),
        h,
    }
}

/// One Appearance-page slot inside settings row `row`: the whole row for
/// full-width items, else the left (`col` 0) or right (`col` 1) half with an
/// inner gap. Shared by the renderer and main.rs like `settings_row_rect`.
pub fn appearance_slot_rect(
    card: &LayoutRect,
    row: usize,
    col: usize,
    full_width: bool,
    scale: f32,
) -> LayoutRect {
    let r = settings_row_rect(card, row, scale);
    if full_width {
        return r;
    }
    let gap = (8.0 * scale).round();
    let w = ((r.w - gap) / 2.0).floor().max(0.0);
    LayoutRect { x: if col == 0 { r.x } else { r.x + w + gap }, w, ..r }
}

/// The `i`-th of the three mode segments (System/Dark/Light), right-aligned
/// inside the Appearance page's mode row. Sized from the label text so the
/// renderer's pill and main.rs's hit-test share the same pixels.
pub fn mode_segment_rect(row: &LayoutRect, i: usize, cell_width: f32, scale: f32) -> LayoutRect {
    let pad = (10.0 * scale).round();
    let gap = (4.0 * scale).round();
    let inset = (4.0 * scale).round();
    let edge = (SETTINGS_PAD * scale).round();
    let w = |i: usize| {
        (crate::theme::Mode::ALL[i].label().chars().count() as f32 * cell_width + 2.0 * pad)
            .round()
    };
    let total: f32 = (0..crate::theme::Mode::ALL.len()).map(w).sum::<f32>()
        + (crate::theme::Mode::ALL.len() - 1) as f32 * gap;
    let mut x = row.x + row.w - edge - total;
    for j in 0..i {
        x += w(j) + gap;
    }
    LayoutRect { x: x.round(), y: row.y + inset, w: w(i), h: (row.h - 2.0 * inset).max(0.0) }
}

/// The region right of the sidebar where the split tree lives. Inset from the
/// window's top/right/bottom edges so the tile cards float on the gradient.
pub fn terminal_area(width: u32, height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    let sb = (sidebar_w * scale).round();
    let pad = (AREA_PAD * scale).round();
    LayoutRect {
        x: sb,
        y: pad,
        w: (width as f32 - sb - pad).max(0.0),
        h: (height as f32 - 2.0 * pad).max(0.0),
    }
}

pub struct Divider {
    pub path: Vec<u8>,
    pub rect: LayoutRect,
    pub dir: Dir,
}

/// Compute every tile's rect and every divider, in tree order.
pub fn layout_tiles(
    node: &Node,
    rect: LayoutRect,
    scale: f32,
) -> (Vec<(u64, LayoutRect)>, Vec<Divider>) {
    let mut tiles = Vec::new();
    let mut dividers = Vec::new();
    let gap = (TILE_GAP * scale).round();
    walk(node, rect, gap, &mut Vec::new(), &mut tiles, &mut dividers);
    (tiles, dividers)
}

fn walk(
    node: &Node,
    rect: LayoutRect,
    gap: f32,
    path: &mut Vec<u8>,
    tiles: &mut Vec<(u64, LayoutRect)>,
    dividers: &mut Vec<Divider>,
) {
    match node {
        Node::Leaf(t) => tiles.push((t.id, rect)),
        Node::Split { dir, ratio, a, b } => {
            let (ra, rb, div) = split_rects(&rect, *dir, *ratio, gap);
            dividers.push(Divider { path: path.clone(), rect: div, dir: *dir });
            path.push(0);
            walk(a, ra, gap, path, tiles, dividers);
            path.pop();
            path.push(1);
            walk(b, rb, gap, path, tiles, dividers);
            path.pop();
        },
    }
}

fn split_rects(rect: &LayoutRect, dir: Dir, ratio: f32, gap: f32) -> (LayoutRect, LayoutRect, LayoutRect) {
    match dir {
        Dir::Row => {
            let aw = ((rect.w - gap) * ratio).round();
            let a = LayoutRect { w: aw, ..*rect };
            let b = LayoutRect { x: rect.x + aw + gap, w: rect.w - aw - gap, ..*rect };
            let d = LayoutRect { x: rect.x + aw, y: rect.y, w: gap, h: rect.h };
            (a, b, d)
        },
        Dir::Column => {
            let ah = ((rect.h - gap) * ratio).round();
            let a = LayoutRect { h: ah, ..*rect };
            let b = LayoutRect { y: rect.y + ah + gap, h: rect.h - ah - gap, ..*rect };
            let d = LayoutRect { x: rect.x, y: rect.y + ah, w: rect.w, h: gap };
            (a, b, d)
        },
    }
}

/// The rect of the subtree at `path` (for divider dragging).
pub fn rect_at_path(node: &Node, rect: LayoutRect, path: &[u8], scale: f32) -> LayoutRect {
    let gap = (TILE_GAP * scale).round();
    let mut node = node;
    let mut rect = rect;
    for step in path {
        if let Node::Split { dir, ratio, a, b } = node {
            let (ra, rb, _) = split_rects(&rect, *dir, *ratio, gap);
            if *step == 0 {
                node = a;
                rect = ra;
            } else {
                node = b;
                rect = rb;
            }
        }
    }
    rect
}

/// The tab strip across the top of a tile.
pub fn tile_tab_bar(rect: &LayoutRect, scale: f32) -> LayoutRect {
    LayoutRect { h: (TILE_TAB_H * scale).round(), ..*rect }
}

/// The terminal content region of a tile (below the tab strip).
pub fn tile_content(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = (TILE_TAB_H * scale).round();
    LayoutRect { y: rect.y + bar, h: (rect.h - bar).max(0.0), ..*rect }
}

/// Rect of tab `i` of `n` in a tile's strip.
pub fn tile_tab_rect(rect: &LayoutRect, i: usize, n: usize, scale: f32) -> LayoutRect {
    let bar = tile_tab_bar(rect, scale);
    let w = (bar.w / n.max(1) as f32).min((TILE_TAB_MAX_W * scale).round()).round();
    LayoutRect { x: bar.x + i as f32 * w, y: bar.y, w, h: bar.h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_empty() {
        let ws = Workspace::placeholder();
        assert!(ws.is_empty());
        assert!(ws.cwd.is_none());
        // Exactly one (tab-less) tile, so unguarded workspace indexing and
        // layout code always have something to work with.
        assert_eq!(ws.root.tiles().len(), 1);
        assert!(ws.focused().is_some());
    }

    #[test]
    fn new_workspace_marks_founding_tile_primary() {
        let ws = Workspace::new("g".into(), Tile::empty(7), None);
        assert_eq!(ws.primary_tile, 7);
        assert_eq!(ws.focused_tile, 7);
    }

    #[test]
    fn title_falls_back_to_name_without_a_primary_pane_title() {
        // A tab-less primary tile has no session title to mirror.
        let ws = Workspace::new("g".into(), Tile::empty(7), None);
        assert_eq!(ws.title(), "g");

        // A vanished primary tile must not panic — fall back to the name.
        let mut ws = Workspace::new("g".into(), Tile::empty(7), None);
        ws.primary_tile = 999;
        assert_eq!(ws.title(), "g");
    }

    #[test]
    fn display_path_shortens_home() {
        use std::path::Path;
        let home = Path::new("/Users/me");
        assert_eq!(display_path(home, Some(home)), "~");
        assert_eq!(display_path(Path::new("/Users/me/src/app"), Some(home)), "~/src/app");
        assert_eq!(display_path(Path::new("/tmp/x"), Some(home)), "/tmp/x");
        assert_eq!(display_path(Path::new("/tmp/x"), None), "/tmp/x");
    }

    #[test]
    fn empty_state_cta_centered_in_terminal_area() {
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let area = terminal_area(w, h, scale, sidebar_w);
        let cta = empty_state_cta(w, h, scale, sidebar_w);
        assert!(cta.x >= area.x && cta.x + cta.w <= area.x + area.w);
        assert!(cta.y >= area.y && cta.y + cta.h <= area.y + area.h);
        // Horizontally centered.
        let left_gap = cta.x - area.x;
        let right_gap = (area.x + area.w) - (cta.x + cta.w);
        assert!((left_gap - right_gap).abs() <= 1.0);
    }

    #[test]
    fn window_edge_padding_matches_tile_gap() {
        // The outer border around the tile area should read exactly as thin
        // as the dividers between tiles.
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let area = terminal_area(w, h, scale, sidebar_w);
        let gap = (TILE_GAP * scale).round();
        assert_eq!(area.y, gap);
        assert_eq!((w as f32) - (area.x + area.w), gap);
        assert_eq!((h as f32) - (area.y + area.h), gap);
    }

    #[test]
    fn empty_state_hint_sits_below_cta() {
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let cta = empty_state_cta(w, h, scale, sidebar_w);
        let hint = empty_state_hint(w, h, scale, sidebar_w);
        assert!(hint.y >= cta.y + cta.h);
        assert_eq!(hint.x, cta.x);
    }
}
