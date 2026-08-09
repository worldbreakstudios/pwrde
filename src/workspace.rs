//! Workspace model + layout math.
//!
//! A `Workspace` (a "group") is one vertical tab in the sidebar and owns a
//! binary *split tree*. Leaves are `Tile`s — each tile has a horizontal tab
//! bar at the top holding one or more terminal tabs (cmux-style). Splits
//! carry a draggable `ratio`.
//!
//! Workspaces may optionally belong to a collapsible [`Section`]. Members of
//! the same section are kept contiguous in the `workspaces` Vec; the sidebar
//! derives its visible rows from that order via [`sidebar_rows`].
//!
//! Layout is pure math over the window size so the renderer (drawing) and
//! the app (hit-testing, PTY resize, divider dragging) always agree.
//!
//! ## Collapse model
//!
//! Each `Tile` carries `collapsed: bool` (target state) and
//! `collapse_anim: f32` (0.0 = fully expanded .. 1.0 = fully collapsed).
//! A tile collapses *along its parent split's axis*: in a `Column` split it
//! shrinks to the tab-bar height (`TILE_TAB_H`); in a `Row` split it shrinks
//! to a narrow vertical strip of the same width. The split `ratio` is never
//! modified — siblings absorb freed space and the prior arrangement is
//! restored exactly on expand.
//!
//! [`tile_collapse_axis`] maps every tile id to its parent split's [`Dir`]
//! (or `None` for a root leaf that has no parent). The renderer and app use
//! this to decide caret visibility and collapsed appearance.

use crate::term::Session;

pub struct Tab {
    pub session: Session,
    /// Cached grid size; used to skip redundant PTY resizes.
    pub cols: usize,
    pub rows: usize,
    /// Whether this tab has unseen output (set by attention signal, cleared on focus).
    pub unread: bool,
}

impl Tab {
    pub fn new(session: Session) -> Self {
        Self { session, cols: 0, rows: 0, unread: false }
    }
}

/// A leaf of the split tree: a tab strip + the active tab's terminal.
pub struct Tile {
    pub id: u64,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// Whether this tile is collapsed (target state; see `collapse_anim`).
    pub collapsed: bool,
    /// Animation progress: 0.0 = fully expanded, 1.0 = fully collapsed.
    /// Advances toward `collapsed as u8 as f32` at 0.15 per tick.
    pub collapse_anim: f32,
}

impl Tile {
    pub fn new(id: u64, session: Session) -> Self {
        Self { id, tabs: vec![Tab::new(session)], active: 0, collapsed: false, collapse_anim: 0.0 }
    }

    pub fn empty(id: u64) -> Self {
        Self { id, tabs: Vec::new(), active: 0, collapsed: false, collapse_anim: 0.0 }
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

/// A collapsible sidebar section grouping one or more workspaces.
///
/// Display position is the position of the section's first member in the
/// workspaces Vec; sections with zero members render at the end of the
/// sidebar in `sections` Vec order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: u64,
    pub name: String,
    pub emoji: String,
    pub collapsed: bool,
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
    /// Sidebar section this group belongs to, if any. Workspaces sharing a
    /// section id must stay contiguous in the parent `workspaces` Vec.
    pub section: Option<u64>,
}

impl Workspace {
    /// Create a group holding a single tile, rooted at `cwd` (`None` = inherit
    /// the process launch directory). The founding tile becomes the primary.
    pub fn new(name: String, tile: Tile, cwd: Option<std::path::PathBuf>) -> Self {
        let focused_tile = tile.id;
        Self {
            name,
            root: Node::Leaf(tile),
            focused_tile,
            cwd,
            primary_tile: focused_tile,
            section: None,
        }
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

    /// Whether any tab in any tile has unread output.
    pub fn any_unread(&self) -> bool {
        self.root.tiles().iter().any(|t| t.tabs.iter().any(|tab| tab.unread))
    }

    /// Ensure `focused_tile` points at an existing tile.
    pub fn fix_focus(&mut self) {
        if self.root.find_tile(self.focused_tile).is_none()
            && let Some(first) = self.root.tiles().first()
        {
            self.focused_tile = first.id;
        }
        // A root leaf has no parent split, so collapse is inert there — but a
        // stale flag would spring back on the next split. Clear it when a
        // removal promotes a lone tile to the root.
        if let Node::Leaf(t) = &mut self.root {
            t.collapsed = false;
            t.collapse_anim = 0.0;
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

#[derive(Clone, Copy, Debug, PartialEq)]
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
/// Slimmer height for section header rows in the sidebar.
const SECTION_HEADER_H: f32 = 30.0;
/// Extra left inset for group rows nested under a section.
const MEMBER_INDENT: f32 = 12.0;
/// Vertical gap between the sidebar's rounded group rows.
const TAB_GAP: f32 = 3.0;
/// Horizontal inset of the sidebar's rows from the sidebar edges.
const SIDEBAR_PAD: f32 = 10.0;
/// Height of the "+" button row between the titlebar and the group tabs.
const NEW_GROUP_H: f32 = 30.0;
/// Gap between the side-by-side "+ group" / "+ section" buttons.
const NEW_BTN_GAP: f32 = 6.0;
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

/// Logical width of the top-left corner the native traffic lights occupy.
/// The buttons themselves end around x=59; the extra headroom keeps the
/// first tab from crowding them.
const TRAFFIC_LIGHT_SAFE_W: f32 = 78.0;

/// The window-drag corner while the sidebar is collapsed: the traffic-light
/// span of the top-left tile's tab strip, plus the sliver of padding above.
/// The native buttons float over it and handle their own clicks.
pub fn collapsed_drag_zone(scale: f32) -> LayoutRect {
    LayoutRect {
        x: 0.0,
        y: 0.0,
        w: (TRAFFIC_LIGHT_SAFE_W * scale).round(),
        h: ((AREA_PAD + TILE_TAB_H) * scale).round(),
    }
}

/// A tile's rect adjusted for tab-strip geometry: while the sidebar is
/// collapsed (`sidebar_w == 0.0`), the tile owning the area's top-left
/// corner cedes its strip's left end to the native traffic lights, pushing
/// its tabs right. Every strip consumer (painting, hit-testing, drops) must
/// feed this to the `tile_tab_*` functions so they never disagree; the card
/// and content keep the original rect.
pub fn tab_strip_rect(area: LayoutRect, rect: &LayoutRect, scale: f32, sidebar_w: f32) -> LayoutRect {
    if sidebar_w != 0.0 || rect.x > area.x || rect.y > area.y {
        return *rect;
    }
    let inset = ((TRAFFIC_LIGHT_SAFE_W * scale).round() - rect.x).clamp(0.0, rect.w);
    LayoutRect { x: rect.x + inset, w: rect.w - inset, ..*rect }
}

/// Shared geometry for the side-by-side "+ group" / "+ section" button row.
fn new_btn_row(scale: f32, sidebar_w: f32) -> (f32, f32, f32, f32) {
    let pad = (SIDEBAR_PAD * scale).round();
    let y = (TITLEBAR_H * scale).round();
    let h = (NEW_GROUP_H * scale).round();
    let full_w = ((sidebar_w * scale).round() - 2.0 * pad).max(0.0);
    (pad, y, full_w, h)
}

/// The "+ group" button (left half of the button row below the titlebar).
/// Clicking it opens the cwd picker. Inset like the group rows so it renders
/// as a rounded field floating on the gradient.
pub fn new_group_button(scale: f32, sidebar_w: f32) -> LayoutRect {
    let (pad, y, full_w, h) = new_btn_row(scale, sidebar_w);
    let gap = (NEW_BTN_GAP * scale).round();
    let w = ((full_w - gap) / 2.0).floor().max(0.0);
    LayoutRect { x: pad, y, w, h }
}

/// The "+ section" button (right half of the button row below the titlebar).
pub fn new_section_button(scale: f32, sidebar_w: f32) -> LayoutRect {
    let (pad, y, full_w, h) = new_btn_row(scale, sidebar_w);
    let gap = (NEW_BTN_GAP * scale).round();
    let left_w = ((full_w - gap) / 2.0).floor().max(0.0);
    let x = pad + left_w + gap;
    let w = (pad + full_w - x).max(0.0);
    LayoutRect { x, y, w, h }
}

/// Centered CTA used by the empty-state launch view.
pub fn empty_state_cta(width: u32, height: u32, scale: f32, sidebar_w: f32, right_w: f32) -> LayoutRect {
    let area = terminal_area(width, height, scale, sidebar_w, right_w);
    let w = (180.0 * scale).round();
    let h = (52.0 * scale).round();
    LayoutRect {
        x: area.x + ((area.w - w) / 2.0).max(0.0),
        y: area.y + ((area.h - h) / 2.0).max(0.0) - (18.0 * scale).round(),
        w,
        h,
    }
}

pub fn empty_state_hint(width: u32, height: u32, scale: f32, sidebar_w: f32, right_w: f32) -> LayoutRect {
    let cta = empty_state_cta(width, height, scale, sidebar_w, right_w);
    LayoutRect { x: cta.x, y: cta.y + cta.h + (14.0 * scale).round(), w: cta.w, h: (22.0 * scale).round() }
}

/// Group tab `index` in the sidebar, stacked below the new-group button.
/// Rows are inset from the sidebar edges (rounded pills, Arc-style).
///
/// Prefer [`sidebar_row_rect`] once the caller has a derived [`SidebarRow`]
/// list — this flat-index helper remains for callers that still treat the
/// sidebar as a uniform stack of group tabs.
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

/// The search-box slot at the top of the Settings sidebar (index 0 slot).
pub fn settings_search_rect(scale: f32, sidebar_w: f32) -> LayoutRect {
    tab_rect(0, scale, sidebar_w)
}

/// One visible row in the sidebar, derived from workspaces + sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    SectionHeader { section_idx: usize },
    Group { ws_idx: usize },
}

/// Derive the ordered sidebar rows from workspace membership and section
/// collapse state. Walks `workspaces` in order: at the first member of each
/// section emits a header, then member group rows only when expanded;
/// ungrouped workspaces emit a group row; empty sections append at the end
/// in `sections` order.
pub fn sidebar_rows(workspaces: &[Workspace], sections: &[Section]) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut emitted = vec![false; sections.len()];
    let mut i = 0;
    while i < workspaces.len() {
        match workspaces[i].section {
            Some(sid) => match sections.iter().position(|s| s.id == sid) {
                Some(section_idx) if !emitted[section_idx] => {
                    emitted[section_idx] = true;
                    rows.push(SidebarRow::SectionHeader { section_idx });
                    let collapsed = sections[section_idx].collapsed;
                    while i < workspaces.len() && workspaces[i].section == Some(sid) {
                        if !collapsed {
                            rows.push(SidebarRow::Group { ws_idx: i });
                        }
                        i += 1;
                    }
                }
                _ => {
                    // Orphan id or non-contiguous repeat: show as a bare group.
                    rows.push(SidebarRow::Group { ws_idx: i });
                    i += 1;
                }
            },
            None => {
                rows.push(SidebarRow::Group { ws_idx: i });
                i += 1;
            }
        }
    }
    for (section_idx, was_emitted) in emitted.iter().enumerate() {
        if !was_emitted {
            rows.push(SidebarRow::SectionHeader { section_idx });
        }
    }
    rows
}

/// Pixel rect for `rows[index]`. Header rows are slimmer; group rows that
/// belong to a section are indented. Painting, hit-testing, and drop
/// resolution must all use this so they never disagree.
pub fn sidebar_row_rect(
    rows: &[SidebarRow],
    index: usize,
    workspaces: &[Workspace],
    scale: f32,
    sidebar_w: f32,
) -> LayoutRect {
    let pad = (SIDEBAR_PAD * scale).round();
    let gap = (TAB_GAP * scale).round();
    let top0 = ((TITLEBAR_H + NEW_GROUP_H) * scale).round() + 2.0 * gap;
    let full_w = ((sidebar_w * scale).round() - 2.0 * pad).max(0.0);
    let mut y = top0;
    for (i, row) in rows.iter().enumerate() {
        let (h, indent) = match *row {
            SidebarRow::SectionHeader { .. } => ((SECTION_HEADER_H * scale).round(), 0.0),
            SidebarRow::Group { ws_idx } => {
                let indent = if workspaces.get(ws_idx).and_then(|w| w.section).is_some() {
                    (MEMBER_INDENT * scale).round()
                } else {
                    0.0
                };
                ((TAB_H * scale).round(), indent)
            }
        };
        if i == index {
            return LayoutRect {
                x: pad + indent,
                y,
                w: (full_w - indent).max(0.0),
                h,
            };
        }
        y += h + gap;
    }
    // Out-of-range fallback: empty rect at the stack end.
    LayoutRect { x: pad, y, w: full_w, h: 0.0 }
}

/// Row index that should show the active pill for `active` workspace.
/// When the active workspace sits inside a collapsed section, that is the
/// section header row; otherwise the visible group row.
pub fn active_row_index(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    sections: &[Section],
    active: usize,
) -> Option<usize> {
    for (i, row) in rows.iter().enumerate() {
        if let SidebarRow::Group { ws_idx } = *row {
            if ws_idx == active {
                return Some(i);
            }
        }
    }
    let sid = workspaces.get(active).and_then(|w| w.section)?;
    let section_idx = sections.iter().position(|s| s.id == sid)?;
    if !sections[section_idx].collapsed {
        return None;
    }
    rows.iter().position(|r| matches!(r, SidebarRow::SectionHeader { section_idx: si } if *si == section_idx))
}

/// Parse a section rename buffer into `(emoji, name)`.
///
/// If the first whitespace-separated token consists entirely of non-ASCII
/// characters it becomes the emoji and the remainder (trimmed) the name;
/// otherwise there is no leading emoji and the whole trimmed string is the name.
pub fn split_leading_emoji(s: &str) -> (Option<String>, String) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (None, String::new());
    }
    let first_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let first = &trimmed[..first_end];
    if !first.is_empty() && first.chars().all(|c| !c.is_ascii()) {
        let rest = trimmed[first_end..].trim_start();
        (Some(first.to_string()), rest.to_string())
    } else {
        (None, trimmed.to_string())
    }
}

/// Apply a committed rename buffer to a section. Empty name after parse keeps
/// the previous name; emoji is updated only when the buffer had a leading one.
pub fn apply_section_rename(section: &mut Section, buffer: &str) {
    let (emoji, name) = split_leading_emoji(buffer);
    if let Some(e) = emoji {
        section.emoji = e;
    }
    if !name.is_empty() {
        section.name = name;
    }
}

/// True when every section's members form a single contiguous run.
#[cfg(test)]
pub fn sections_are_contiguous(workspaces: &[Workspace]) -> bool {
    let mut seen_end: Vec<u64> = Vec::new();
    let mut i = 0;
    while i < workspaces.len() {
        match workspaces[i].section {
            Some(sid) => {
                if seen_end.contains(&sid) {
                    return false;
                }
                while i < workspaces.len() && workspaces[i].section == Some(sid) {
                    i += 1;
                }
                seen_end.push(sid);
            }
            None => i += 1,
        }
    }
    true
}

/// Inclusive-exclusive index range of members of `section_id`, if any.
pub fn section_member_range(workspaces: &[Workspace], section_id: u64) -> Option<(usize, usize)> {
    let start = workspaces.iter().position(|w| w.section == Some(section_id))?;
    let mut end = start + 1;
    while end < workspaces.len() && workspaces[end].section == Some(section_id) {
        end += 1;
    }
    Some((start, end))
}

/// Section a workspace would join when inserted at `insert_before` (before the
/// workspace currently at that index). Only positions strictly inside a
/// contiguous member run join that section; boundaries leave the tab ungrouped.
#[cfg(test)]
pub fn section_for_insert(workspaces: &[Workspace], insert_before: usize) -> Option<u64> {
    let left = insert_before
        .checked_sub(1)
        .and_then(|i| workspaces.get(i))
        .and_then(|w| w.section);
    let right = workspaces.get(insert_before).and_then(|w| w.section);
    match (left, right) {
        (Some(a), Some(b)) if a == b => Some(a),
        _ => None,
    }
}

/// Remove `from` and insert it at `insert_before` (pre-removal index), assigning
/// `new_section`. Returns the workspace's final index.
pub fn relocate_workspace(
    workspaces: &mut Vec<Workspace>,
    from: usize,
    mut insert_before: usize,
    new_section: Option<u64>,
) -> usize {
    if from >= workspaces.len() {
        return from;
    }
    let mut ws = workspaces.remove(from);
    if insert_before > from {
        insert_before -= 1;
    }
    insert_before = insert_before.min(workspaces.len());
    ws.section = new_section;
    workspaces.insert(insert_before, ws);
    insert_before
}

/// Remap an index that pointed at a workspace before [`relocate_workspace`].
pub fn track_index_after_relocate(tracked: usize, from: usize, final_idx: usize) -> usize {
    if tracked == from {
        final_idx
    } else if from < tracked && final_idx >= tracked {
        tracked - 1
    } else if from > tracked && final_idx <= tracked {
        tracked + 1
    } else {
        tracked
    }
}

/// Move workspace `from` to sit after the current last member of `section_id`
/// (or at end of list if the section is empty). Sets membership. Returns final index.
pub fn append_to_section(
    workspaces: &mut Vec<Workspace>,
    from: usize,
    section_id: u64,
) -> usize {
    if from >= workspaces.len() {
        return from;
    }
    let insert_before = match section_member_range(workspaces, section_id) {
        // End of the member run — also covers "already inside: move to end".
        Some((_, end)) => end,
        None => workspaces.len(),
    };
    relocate_workspace(workspaces, from, insert_before, Some(section_id))
}

/// Drop workspace `from` onto the middle of group `target`.
///
/// - Target in a section → join that section, placed after the target.
/// - Target ungrouped → create a new expanded section named "section" containing
///   `[target, from]` and return its id.
///
/// Returns `(new_index_of_from, created_section_id)`.
pub fn join_onto_group(
    workspaces: &mut Vec<Workspace>,
    sections: &mut Vec<Section>,
    next_section_id: &mut u64,
    from: usize,
    target: usize,
) -> (usize, Option<u64>) {
    if from >= workspaces.len() || target >= workspaces.len() || from == target {
        return (from, None);
    }
    if let Some(sid) = workspaces[target].section {
        let new_idx = relocate_workspace(workspaces, from, target + 1, Some(sid));
        return (new_idx, None);
    }
    // Create a section around [target, from].
    let id = *next_section_id;
    *next_section_id += 1;
    sections.push(Section {
        id,
        name: "section".into(),
        emoji: String::new(),
        collapsed: false,
    });
    // Place `from` immediately after `target`, then tag both.
    let new_from = relocate_workspace(workspaces, from, target + 1, Some(id));
    let new_target = if from < target { target - 1 } else { target };
    workspaces[new_target].section = Some(id);
    workspaces[new_from].section = Some(id);
    (new_from, Some(id))
}

/// Move the contiguous member block of `section_id` so it starts at
/// `dest_start` (pre-move index among current workspaces, clamped). Never
/// splits another section — `dest_start` is adjusted out of foreign runs.
/// Returns the block's new `[start, end)` range.
pub fn relocate_section_block(
    workspaces: &mut Vec<Workspace>,
    section_id: u64,
    mut dest_start: usize,
) -> Option<(usize, usize)> {
    let (start, end) = section_member_range(workspaces, section_id)?;
    let len = end - start;
    if len == 0 {
        return Some((start, end));
    }
    // Pull the block out.
    let block: Vec<Workspace> = workspaces.drain(start..end).collect();
    if dest_start > start {
        dest_start -= len;
    }
    dest_start = dest_start.min(workspaces.len());
    // Do not land inside a different section's run — snap to its boundary.
    dest_start = snap_top_level_index(workspaces, dest_start);
    for (i, ws) in block.into_iter().enumerate() {
        workspaces.insert(dest_start + i, ws);
    }
    Some((dest_start, dest_start + len))
}

/// Snap an index so it sits on a top-level boundary (not strictly inside
/// another section's member run).
fn snap_top_level_index(workspaces: &[Workspace], index: usize) -> usize {
    if index == 0 || index >= workspaces.len() {
        return index.min(workspaces.len());
    }
    let left = workspaces[index - 1].section;
    let right = workspaces[index].section;
    match (left, right) {
        (Some(a), Some(b)) if a == b => {
            // Inside run a — snap to end of run.
            let mut i = index;
            while i < workspaces.len() && workspaces[i].section == Some(a) {
                i += 1;
            }
            i
        }
        _ => index,
    }
}

/// Delete `section_id` from `sections` when it has no remaining members.
/// Returns true if the section was removed. Empty sections created via the
/// button are only pruned once they have gained and then lost members — the
/// caller decides when to invoke this (after a member leaves / group closes).
pub fn prune_section_if_empty(
    sections: &mut Vec<Section>,
    workspaces: &[Workspace],
    section_id: u64,
) -> bool {
    if workspaces.iter().any(|w| w.section == Some(section_id)) {
        return false;
    }
    if let Some(i) = sections.iter().position(|s| s.id == section_id) {
        sections.remove(i);
        true
    } else {
        false
    }
}

/// Expand the section containing `active` when it is collapsed. Returns true
/// if a section was expanded.
pub fn ensure_active_section_expanded(
    workspaces: &[Workspace],
    sections: &mut [Section],
    active: usize,
) -> bool {
    let Some(sid) = workspaces.get(active).and_then(|w| w.section) else {
        return false;
    };
    if let Some(sec) = sections.iter_mut().find(|s| s.id == sid) {
        if sec.collapsed {
            sec.collapsed = false;
            return true;
        }
    }
    false
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

// ── Appearance page geometry ─────────────────────────────────────────────────

/// Height of the header controls row (mode + preview toggle).
pub const APPEARANCE_HEADER_H: f32 = 44.0;
/// Height of the column caption band ("App Theme" / "Terminal Colors").
pub const APPEARANCE_CAPTION_H: f32 = 26.0;
/// Height of a dropdown pill field row (label + pill).
pub const APPEARANCE_DROPDOWN_ROW_H: f32 = 56.0;
/// Height of the footer action row.
pub const APPEARANCE_FOOTER_H: f32 = 40.0;
/// Gap between the two columns.
const APPEARANCE_COL_GAP: f32 = 12.0;
/// Logical height of one dropdown menu item.
pub const APPEARANCE_MENU_ITEM_H: f32 = 30.0;

/// The header controls row (mode segmented control + preview toggle), at the
/// top of the settings card body (below the title band).
pub fn appearance_header_row(card: &LayoutRect, scale: f32) -> LayoutRect {
    let pad = (SETTINGS_PAD * scale).round();
    let header = (SETTINGS_HEADER_H * scale).round();
    let h = (APPEARANCE_HEADER_H * scale).round();
    LayoutRect { x: card.x + pad, y: card.y + header, w: (card.w - 2.0 * pad).max(0.0), h }
}

/// One of the two "Preview" toggle segments (0 = Light, 1 = Dark), right-
/// aligned in the header row. Mirrors the style of `mode_segment_rect`.
pub fn preview_segment_rect(row: &LayoutRect, i: usize, cell_width: f32, scale: f32) -> LayoutRect {
    let labels = ["Light", "Dark"];
    let pad = (10.0 * scale).round();
    let gap = (4.0 * scale).round();
    let inset = (4.0 * scale).round();
    let w = |j: usize| (labels[j].chars().count() as f32 * cell_width + 2.0 * pad).round();
    let total: f32 = (0..2).map(w).sum::<f32>() + gap;
    let mut x = row.x + row.w - total;
    for j in 0..i {
        x += w(j) + gap;
    }
    LayoutRect { x: x.round(), y: row.y + inset, w: w(i), h: (row.h - 2.0 * inset).max(0.0) }
}

/// The sub-row `mode_segment_rect` positions inside on the Appearance page:
/// the header row shortened so the mode segments end just left of the
/// "Preview" caption + toggle cluster. `mode_segment_rect` right-aligns to
/// `row.x + row.w - edge` (edge = `SETTINGS_PAD`), so that inset is added
/// back here to land the segments at the intended gap.
pub fn appearance_mode_row(card: &LayoutRect, cell_width: f32, scale: f32) -> LayoutRect {
    let row = appearance_header_row(card, scale);
    let seg0 = preview_segment_rect(&row, 0, cell_width, scale);
    let caption_w = ("Preview".chars().count() as f32 * cell_width).round();
    let gap = (14.0 * scale).round();
    let edge = (SETTINGS_PAD * scale).round();
    let right = seg0.x - caption_w - 2.0 * gap;
    LayoutRect { w: (right - row.x + edge).max(0.0), ..row }
}

/// The left (App Theme, col 0) or right (Terminal Colors, col 1) column,
/// spanning from below the header row to above the footer row.
pub fn appearance_column(card: &LayoutRect, col: usize, scale: f32) -> LayoutRect {
    let pad = (SETTINGS_PAD * scale).round();
    let header_h = (SETTINGS_HEADER_H * scale).round();
    let hdr_h = (APPEARANCE_HEADER_H * scale).round();
    let footer_h = (APPEARANCE_FOOTER_H * scale).round();
    let gap = (APPEARANCE_COL_GAP * scale).round();
    let top = card.y + header_h + hdr_h;
    let bottom = card.y + card.h - footer_h;
    let h = (bottom - top).max(0.0);
    let total_w = (card.w - 2.0 * pad).max(0.0);
    let col_w = ((total_w - gap) / 2.0).floor().max(0.0);
    let x = card.x + pad + col as f32 * (col_w + gap);
    LayoutRect { x: x.round(), y: top.round(), w: col_w, h }
}

/// The two dropdown pill fields ("Light Theme"/"Dark Theme" or "Light Profile"/
/// "Dark Profile") inside column `col`. Field 0 is left, field 1 is right,
/// separated by a small gap. Each field occupies `APPEARANCE_DROPDOWN_ROW_H`
/// in height and half the column width.
pub fn appearance_dropdown_rect(
    card: &LayoutRect,
    col: usize,
    field: usize,
    scale: f32,
) -> LayoutRect {
    let column = appearance_column(card, col, scale);
    let caption = (APPEARANCE_CAPTION_H * scale).round();
    let gap = (8.0 * scale).round();
    let field_w = ((column.w - gap) / 2.0).floor().max(0.0);
    let h = (APPEARANCE_DROPDOWN_ROW_H * scale).round();
    let x = column.x + field as f32 * (field_w + gap);
    LayoutRect { x: x.round(), y: column.y + caption, w: field_w, h }
}

/// The clickable pill inside a dropdown field — the field's own caption
/// ("Light Theme" etc.) occupies the band above it.
pub fn appearance_dropdown_pill(
    card: &LayoutRect,
    col: usize,
    field: usize,
    scale: f32,
) -> LayoutRect {
    let f = appearance_dropdown_rect(card, col, field, scale);
    let cap = (20.0 * scale).round();
    let bottom_gap = (4.0 * scale).round();
    LayoutRect { y: f.y + cap, h: (f.h - cap - bottom_gap).max(0.0), ..f }
}

/// The preview card for column `col`, filling the column below the two
/// dropdown fields.
pub fn appearance_preview_card(card: &LayoutRect, col: usize, scale: f32) -> LayoutRect {
    let column = appearance_column(card, col, scale);
    let caption = (APPEARANCE_CAPTION_H * scale).round();
    let drop_h = (APPEARANCE_DROPDOWN_ROW_H * scale).round();
    let gap = (10.0 * scale).round();
    let y = column.y + caption + drop_h + gap;
    let h = (column.h - caption - drop_h - gap - (10.0 * scale).round()).max(0.0);
    LayoutRect { x: column.x, y, w: column.w, h }
}

/// The footer action row at the bottom of the settings card.
pub fn appearance_footer_row(card: &LayoutRect, scale: f32) -> LayoutRect {
    let pad = (SETTINGS_PAD * scale).round();
    let footer_h = (APPEARANCE_FOOTER_H * scale).round();
    let y = card.y + card.h - footer_h;
    LayoutRect { x: card.x + pad, y, w: (card.w - 2.0 * pad).max(0.0), h: footer_h }
}

/// A footer pill action button. `i` = 0 → "Import from Clipboard",
/// `i` = 1 → "Copy Theme String". Positioned left-to-right with a gap.
/// `cell_width` is used to size each pill to its label.
pub fn appearance_footer_action(
    card: &LayoutRect,
    i: usize,
    cell_width: f32,
    scale: f32,
) -> LayoutRect {
    let labels = ["Import from Clipboard", "Copy Theme String"];
    let footer = appearance_footer_row(card, scale);
    let pad = (10.0 * scale).round();
    let gap = (8.0 * scale).round();
    let pill_h = (28.0 * scale).round();
    let inset_y = ((footer.h - pill_h) / 2.0).round();
    let w = |j: usize| (labels[j].chars().count() as f32 * cell_width + 2.0 * pad).round();
    let mut x = footer.x;
    for j in 0..i {
        x += w(j) + gap;
    }
    LayoutRect { x: x.round(), y: footer.y + inset_y, w: w(i), h: pill_h }
}

/// The dropdown menu panel anchored below field `field` in column `col`,
/// containing `n_items` rows. The panel is clamped to stay within `card`.
pub fn appearance_menu_panel(
    card: &LayoutRect,
    col: usize,
    field: usize,
    n_items: usize,
    scale: f32,
) -> LayoutRect {
    let anchor = appearance_dropdown_pill(card, col, field, scale);
    let item_h = (APPEARANCE_MENU_ITEM_H * scale).round();
    let pad = (4.0 * scale).round();
    let h = (item_h * n_items as f32 + 2.0 * pad).round();
    let w = anchor.w.max((160.0 * scale).round());
    // Anchor below the field; clamp so panel stays inside the card.
    let max_y = card.y + card.h - h;
    let y = (anchor.y + anchor.h).min(max_y).max(card.y);
    // Clamp x so panel stays inside the card.
    let max_x = card.x + card.w - w;
    let x = anchor.x.min(max_x).max(card.x);
    LayoutRect { x: x.round(), y: y.round(), w, h }
}

/// One item row inside the dropdown menu panel.
pub fn appearance_menu_item(
    card: &LayoutRect,
    col: usize,
    field: usize,
    n_items: usize,
    item: usize,
    scale: f32,
) -> LayoutRect {
    let panel = appearance_menu_panel(card, col, field, n_items, scale);
    let item_h = (APPEARANCE_MENU_ITEM_H * scale).round();
    let pad = (4.0 * scale).round();
    LayoutRect {
        x: panel.x,
        y: (panel.y + pad + item as f32 * item_h).round(),
        w: panel.w,
        h: item_h,
    }
}

/// The region right of the sidebar where the split tree lives. Inset from the
/// window's top/right/bottom edges so the tile cards float on the gradient.
///
/// `sidebar_w == 0.0` means the sidebar is collapsed (the resize clamp keeps
/// a visible sidebar at [`SIDEBAR_MIN_W`] or wider). Collapsed, the area
/// keeps the full window height and just gains the thin left inset; the
/// native traffic lights instead carve into the top-left tile's tab strip
/// via [`tab_strip_rect`].
pub fn terminal_area(width: u32, height: u32, scale: f32, sidebar_w: f32, right_w: f32) -> LayoutRect {
    let sb = (sidebar_w * scale).round();
    let pad = (AREA_PAD * scale).round();
    let x = if sidebar_w == 0.0 { pad } else { sb };
    let right_edge = (width as f32 - right_w * scale - pad).max(x);
    LayoutRect {
        x,
        y: pad,
        w: (right_edge - x).max(0.0),
        h: (height as f32 - 2.0 * pad).max(0.0),
    }
}

pub struct Divider {
    pub path: Vec<u8>,
    pub rect: LayoutRect,
    pub dir: Dir,
}

/// Which resize handle the pointer is over (sidebar edge, a tile divider,
/// or the tool panel's left edge). Drives the cursor style and the hover
/// highlight painted in the renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum ResizeHover {
    Sidebar,
    Divider { path: Vec<u8>, dir: Dir },
    ToolPanel,
}

/// Hit-test the sidebar edge and tile dividers at `(px, py)`.
/// Matches `on_mouse_down` grab inflation/containment so the cursor and
/// highlight appear exactly where a drag would arm.
pub fn resize_hover_at(
    node: &Node,
    area: LayoutRect,
    scale: f32,
    sidebar_edge_x: f32,
    grab: f32,
    dividers_active: bool,
    px: f32,
    py: f32,
) -> Option<ResizeHover> {
    // `sidebar_edge_x == 0.0` means collapsed: there is no edge to grab.
    if sidebar_edge_x > 0.0 && (px - sidebar_edge_x).abs() <= grab {
        return Some(ResizeHover::Sidebar);
    }
    if !dividers_active {
        return None;
    }
    let (_, dividers) = layout_tiles(node, area, scale);
    dividers
        .into_iter()
        .find(|d| d.rect.inflate(grab).contains(px, py))
        .map(|d| ResizeHover::Divider { path: d.path, dir: d.dir })
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
    walk(node, rect, gap, scale, &mut Vec::new(), &mut tiles, &mut dividers);
    (tiles, dividers)
}

// ── Collapse ────────────────────────────────────────────────────────────

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// How collapsed a subtree currently *renders* (animation state, not the
/// target): a leaf's `collapse_anim`, a split the minimum of its children —
/// a split is only as collapsed as its least-collapsed child.
fn collapse_factor(node: &Node) -> f32 {
    match node {
        Node::Leaf(t) => t.collapse_anim.clamp(0.0, 1.0),
        Node::Split { a, b, .. } => collapse_factor(a).min(collapse_factor(b)),
    }
}

/// Whether every leaf of the subtree has `collapsed == true` (target state).
pub fn fully_collapsed(node: &Node) -> bool {
    match node {
        Node::Leaf(t) => t.collapsed,
        Node::Split { a, b, .. } => fully_collapsed(a) && fully_collapsed(b),
    }
}

/// Extent a fully-collapsed subtree occupies along `axis`: a leaf keeps just
/// its tab-bar height (or an equally narrow strip when collapsing sideways);
/// splits stack extents along the axis and take the max across it.
fn collapsed_extent(node: &Node, axis: Dir, scale: f32, gap: f32) -> f32 {
    match node {
        Node::Leaf(_) => (TILE_TAB_H * scale).round(),
        Node::Split { dir, a, b, .. } => {
            let ea = collapsed_extent(a, axis, scale, gap);
            let eb = collapsed_extent(b, axis, scale, gap);
            if *dir == axis { ea + gap + eb } else { ea.max(eb) }
        },
    }
}

/// Child rects + divider for a split, honoring collapse. With both collapse
/// factors at 0 this is exactly [`split_rects`]. A collapsing child heads to
/// its fixed collapsed extent while the sibling absorbs the remainder; the
/// stored `ratio` is never touched, so expanding restores the old layout and
/// multi-pane siblings keep their relative distribution.
fn split_rects_collapsed(
    rect: &LayoutRect,
    dir: Dir,
    ratio: f32,
    a: &Node,
    b: &Node,
    gap: f32,
    scale: f32,
) -> (LayoutRect, LayoutRect, LayoutRect) {
    let fa = collapse_factor(a);
    let fb = collapse_factor(b);
    if fa <= 0.0 && fb <= 0.0 {
        return split_rects(rect, dir, ratio, gap);
    }
    let total = match dir {
        Dir::Row => rect.w,
        Dir::Column => rect.h,
    };
    let nat_a = ((total - gap) * ratio).round();
    let nat_b = total - nat_a - gap;
    // The more-collapsed side is sized to its target; the other side takes
    // the remainder, so freed space always flows to the expanded panes.
    let (ea, eb) = if fa >= fb {
        let ea = lerp(nat_a, collapsed_extent(a, dir, scale, gap), fa).round();
        (ea, total - ea - gap)
    } else {
        let eb = lerp(nat_b, collapsed_extent(b, dir, scale, gap), fb).round();
        (total - eb - gap, eb)
    };
    match dir {
        Dir::Row => (
            LayoutRect { w: ea, ..*rect },
            LayoutRect { x: rect.x + ea + gap, w: eb, ..*rect },
            LayoutRect { x: rect.x + ea, y: rect.y, w: gap, h: rect.h },
        ),
        Dir::Column => (
            LayoutRect { h: ea, ..*rect },
            LayoutRect { y: rect.y + ea + gap, h: eb, ..*rect },
            LayoutRect { x: rect.x, y: rect.y + ea, w: rect.w, h: gap },
        ),
    }
}

// ── Directional navigation ──────────────────────────────────────────────

/// Direction for vim-style pane focus navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

/// Given a flat list of (id, rect) pairs (as produced by `layout_tiles`),
/// return the id of the best neighbour of tile `from` in direction `dir`.
///
/// Candidates are tiles strictly in the given direction (using facing edges
/// plus a small epsilon to handle flush splits).  Among candidates that
/// overlap `from` on the perpendicular axis the one with the smallest edge
/// gap wins; ties are broken by perpendicular center distance.  When no
/// overlapping candidate exists the function falls back to the nearest by
/// Euclidean center distance.  Returns `None` when `from` is not found or
/// there are no candidates in the requested direction (no wraparound).
pub fn directional_neighbor(tiles: &[(u64, LayoutRect)], from: u64, dir: NavDir) -> Option<u64> {
    let eps = 1.0_f32;

    // Find the source rect.
    let from_rect = tiles.iter().find(|(id, _)| *id == from).map(|(_, r)| *r)?;

    let from_cx = from_rect.x + from_rect.w / 2.0;
    let from_cy = from_rect.y + from_rect.h / 2.0;

    struct Candidate {
        id: u64,
        edge_gap: f32,
        perp_dist: f32,
        center_dist: f32,
        overlaps: bool,
    }

    let mut candidates: Vec<Candidate> = tiles
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, r)| {
            let cx = r.x + r.w / 2.0;
            let cy = r.y + r.h / 2.0;

            let (in_dir, edge_gap, overlaps) = match dir {
                NavDir::Left => {
                    let in_dir = cx < from_cx && r.x + r.w <= from_rect.x + eps;
                    let edge_gap = (from_rect.x - (r.x + r.w)).max(0.0);
                    // Vertical overlap
                    let overlaps = r.y < from_rect.y + from_rect.h && r.y + r.h > from_rect.y;
                    (in_dir, edge_gap, overlaps)
                },
                NavDir::Right => {
                    let in_dir = cx > from_cx && r.x >= from_rect.x + from_rect.w - eps;
                    let edge_gap = (r.x - (from_rect.x + from_rect.w)).max(0.0);
                    let overlaps = r.y < from_rect.y + from_rect.h && r.y + r.h > from_rect.y;
                    (in_dir, edge_gap, overlaps)
                },
                NavDir::Up => {
                    let in_dir = cy < from_cy && r.y + r.h <= from_rect.y + eps;
                    let edge_gap = (from_rect.y - (r.y + r.h)).max(0.0);
                    // Horizontal overlap
                    let overlaps = r.x < from_rect.x + from_rect.w && r.x + r.w > from_rect.x;
                    (in_dir, edge_gap, overlaps)
                },
                NavDir::Down => {
                    let in_dir = cy > from_cy && r.y >= from_rect.y + from_rect.h - eps;
                    let edge_gap = (r.y - (from_rect.y + from_rect.h)).max(0.0);
                    let overlaps = r.x < from_rect.x + from_rect.w && r.x + r.w > from_rect.x;
                    (in_dir, edge_gap, overlaps)
                },
            };

            if !in_dir {
                return None;
            }

            let perp_dist = match dir {
                NavDir::Left | NavDir::Right => (cy - from_cy).abs(),
                NavDir::Up | NavDir::Down => (cx - from_cx).abs(),
            };

            let dx = cx - from_cx;
            let dy = cy - from_cy;
            let center_dist = (dx * dx + dy * dy).sqrt();

            Some(Candidate { id: *id, edge_gap, perp_dist, center_dist, overlaps })
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Prefer overlapping candidates; fall back to nearest by center dist.
    let has_overlap = candidates.iter().any(|c| c.overlaps);
    if has_overlap {
        candidates.retain(|c| c.overlaps);
        // Pick smallest edge gap, tie-break by perp center distance.
        candidates.sort_by(|a, b| {
            a.edge_gap
                .partial_cmp(&b.edge_gap)
                .unwrap()
                .then(a.perp_dist.partial_cmp(&b.perp_dist).unwrap())
        });
    } else {
        candidates.sort_by(|a, b| a.center_dist.partial_cmp(&b.center_dist).unwrap());
    }

    Some(candidates[0].id)
}

fn walk(
    node: &Node,
    rect: LayoutRect,
    gap: f32,
    scale: f32,
    path: &mut Vec<u8>,
    tiles: &mut Vec<(u64, LayoutRect)>,
    dividers: &mut Vec<Divider>,
) {
    match node {
        Node::Leaf(t) => tiles.push((t.id, rect)),
        Node::Split { dir, ratio, a, b } => {
            let (ra, rb, div) = split_rects_collapsed(&rect, *dir, *ratio, a, b, gap, scale);
            // A collapsed edge has a fixed extent — no divider to drag.
            if !fully_collapsed(a) && !fully_collapsed(b) {
                dividers.push(Divider { path: path.clone(), rect: div, dir: *dir });
            }
            path.push(0);
            walk(a, ra, gap, scale, path, tiles, dividers);
            path.pop();
            path.push(1);
            walk(b, rb, gap, scale, path, tiles, dividers);
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
            let (ra, rb, _) = split_rects_collapsed(&rect, *dir, *ratio, a, b, gap, scale);
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

/// A square caret button at the RIGHT edge of a tile's tab bar.
/// Side length = `TILE_TAB_H * scale`. Present only when the tile has a
/// parent split (i.e. `tile_collapse_axis` returns `Some`).
pub fn tile_caret_rect(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = tile_tab_bar(rect, scale);
    let side = (TILE_TAB_H * scale).round();
    LayoutRect { x: bar.x + (bar.w - side).max(0.0), y: bar.y, w: side, h: side }
}

/// Rect of tab `i` of `n` in a tile's strip.
///
/// When `has_caret` is `true` (the tile has a parent split and therefore
/// shows a collapse caret button), the caret square at the right end of the
/// strip is excluded from the available width so tabs never overlap it.
pub fn tile_tab_rect(rect: &LayoutRect, i: usize, n: usize, scale: f32, has_caret: bool) -> LayoutRect {
    let bar = tile_tab_bar(rect, scale);
    let caret_w = if has_caret { (TILE_TAB_H * scale).round() } else { 0.0 };
    let avail_w = (bar.w - caret_w).max(0.0);
    let w = (avail_w / n.max(1) as f32).min((TILE_TAB_MAX_W * scale).round()).round();
    LayoutRect { x: bar.x + i as f32 * w, y: bar.y, w, h: bar.h }
}

/// Nearest insertion gap (`0..=n`) for pointer x over a tile's tab strip.
/// Left half of a tab resolves to the gap before it, right half to the gap
/// after — used for same-tile reorders so the drop lands where the line shows.
pub fn tile_tab_insert_gap(rect: &LayoutRect, px: f32, n: usize, scale: f32, has_caret: bool) -> usize {
    let t0 = tile_tab_rect(rect, 0, n, scale, has_caret);
    if t0.w <= 0.0 {
        return 0;
    }
    ((((px - t0.x) / t0.w + 0.5).floor().max(0.0)) as usize).min(n)
}

/// Thin vertical insertion-line rect at gap `gap` (`0..=n`) of a tile's strip.
pub fn tile_tab_insert_line(rect: &LayoutRect, gap: usize, n: usize, scale: f32, has_caret: bool) -> LayoutRect {
    let line_w = (2.0 * scale).max(1.0);
    let tr = tile_tab_rect(rect, gap.min(n.saturating_sub(1)), n, scale, has_caret);
    let x = if gap >= n { tr.x + tr.w } else { tr.x };
    LayoutRect { x: x - line_w / 2.0, y: tr.y, w: line_w, h: tr.h }
}

/// The close-button hit region at the right edge of tab `i` of `n`.
///
/// `has_caret` is forwarded to `tile_tab_rect` so the close button position
/// stays consistent with the tab's actual position.
pub fn tile_tab_close_rect(rect: &LayoutRect, i: usize, n: usize, scale: f32, has_caret: bool) -> LayoutRect {
    let tr = tile_tab_rect(rect, i, n, scale, has_caret);
    let s = (16.0 * scale).round();
    let pad = (6.0 * scale).round();
    LayoutRect {
        x: tr.x + tr.w - s - pad,
        y: (tr.y + (tr.h - s) / 2.0).round(),
        w: s,
        h: s,
    }
}

// ── Flyover panel layout ─────────────────────────────────────────────────────

/// Horizontal inset (logical px) applied to each side of the flyover panel.
const FLYOVER_INSET: f32 = 6.0;

/// Height of the flyover panel as a fraction of the window height when the
/// user hasn't drag-resized it.
pub const FLYOVER_DEFAULT_FRAC: f32 = 0.40;

/// Drag-resize bounds for the flyover height fraction.
pub const FLYOVER_MIN_FRAC: f32 = 0.15;
pub const FLYOVER_MAX_FRAC: f32 = 0.90;

/// Half-height (logical px) of the grab zone around the panel's top edge for
/// drag-resizing.
pub const FLYOVER_RESIZE_GRAB: f32 = 4.0;

/// Width (logical px) of the always-visible tool ribbon on the right window edge.
pub const RIBBON_W: f32 = 36.0;
/// Default width (logical px) of the tool panel when opened.
pub const TOOL_PANEL_DEFAULT_W: f32 = 380.0;
/// Minimum width (logical px) of the tool panel (drag-resize lower bound).
pub const TOOL_PANEL_MIN_W: f32 = 260.0;
/// Maximum width (logical px) of the tool panel (drag-resize upper bound).
pub const TOOL_PANEL_MAX_W: f32 = 640.0;
/// Half-width (logical px) of the grab zone on the panel's left edge for drag-resizing.
pub const TOOL_PANEL_RESIZE_GRAB: f32 = 4.0;

/// Resting rect of the flyover panel in physical pixels, interpolated by
/// `anim` (0.0 = fully off-screen below, 1.0 = fully visible).
///
/// The panel spans the window width minus a small horizontal inset and sits
/// above the bottom edge, `frac` of the window tall (clamped to the resize
/// bounds). `maximized` fills the whole window instead; the slide animation
/// still applies.
pub fn flyover_rect(
    width: u32,
    height: u32,
    scale: f32,
    anim: f32,
    frac: f32,
    maximized: bool,
) -> LayoutRect {
    let (x, w, panel_h) = if maximized {
        (0.0, width as f32, height as f32)
    } else {
        let inset = (FLYOVER_INSET * scale).round();
        let frac = frac.clamp(FLYOVER_MIN_FRAC, FLYOVER_MAX_FRAC);
        (inset, ((width as f32) - 2.0 * inset).max(0.0), ((height as f32) * frac).round())
    };
    let resting_y = (height as f32) - panel_h;
    // Off-screen bottom: panel sits just below the window.
    let offscreen_y = height as f32;
    let y = lerp(offscreen_y, resting_y, anim.clamp(0.0, 1.0));
    LayoutRect { x, y, w, h: panel_h }
}

/// Tab-bar strip at the top of the flyover panel (mirrors `tile_tab_bar`).
pub fn flyover_tab_bar(rect: &LayoutRect, scale: f32) -> LayoutRect {
    LayoutRect { h: (TILE_TAB_H * scale).round(), ..*rect }
}

/// Terminal content region of the flyover panel (below the tab strip).
pub fn flyover_content(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = (TILE_TAB_H * scale).round();
    LayoutRect { y: rect.y + bar, h: (rect.h - bar).max(0.0), ..*rect }
}

/// Width (physical px) reserved at the right end of the flyover tab bar for
/// the minimize/maximize buttons — two square slots, one bar-height each.
fn flyover_buttons_w(rect: &LayoutRect, scale: f32) -> f32 {
    2.0 * flyover_tab_bar(rect, scale).h
}

/// Rect of tab `i` of `n` in the flyover tab strip (mirrors `tile_tab_rect`,
/// no caret button so no `has_caret` parameter). Tabs share the bar minus
/// the window-button strip at the right; a `maximized` panel owns the
/// window's top-left corner, so the strip cedes its left end to the native
/// traffic lights like a collapsed-sidebar tile does.
pub fn flyover_tab_rect(
    rect: &LayoutRect,
    i: usize,
    n: usize,
    scale: f32,
    maximized: bool,
) -> LayoutRect {
    let bar = flyover_tab_bar(rect, scale);
    let cede = if maximized {
        (TRAFFIC_LIGHT_SAFE_W * scale).round().clamp(0.0, bar.w)
    } else {
        0.0
    };
    let avail = (bar.w - cede - flyover_buttons_w(rect, scale)).max(0.0);
    let w = (avail / n.max(1) as f32).min((TILE_TAB_MAX_W * scale).round()).round();
    LayoutRect { x: bar.x + cede + i as f32 * w, y: bar.y, w, h: bar.h }
}

/// Rect of the × close button inside flyover tab `i` (mirrors
/// `tile_tab_close_rect`).
pub fn flyover_tab_close_rect(
    rect: &LayoutRect,
    i: usize,
    n: usize,
    scale: f32,
    maximized: bool,
) -> LayoutRect {
    let tr = flyover_tab_rect(rect, i, n, scale, maximized);
    let s = (16.0 * scale).round();
    let pad = (6.0 * scale).round();
    LayoutRect {
        x: tr.x + tr.w - s - pad,
        y: (tr.y + (tr.h - s) / 2.0).round(),
        w: s,
        h: s,
    }
}

/// The minimize (−) button: second-from-right square in the flyover tab bar.
pub fn flyover_minimize_rect(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = flyover_tab_bar(rect, scale);
    LayoutRect { x: bar.x + bar.w - 2.0 * bar.h, y: bar.y, w: bar.h, h: bar.h }
}

/// The maximize button: rightmost square in the flyover tab bar.
pub fn flyover_maximize_rect(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = flyover_tab_bar(rect, scale);
    LayoutRect { x: bar.x + bar.w - bar.h, y: bar.y, w: bar.h, h: bar.h }
}

// ── Right-side tool ribbon ────────────────────────────────────────────────────

/// The slim vertical icon strip along the right window edge.
pub fn ribbon(width: u32, height: u32, scale: f32) -> LayoutRect {
    let w = (RIBBON_W * scale).round();
    LayoutRect {
        x: width as f32 - w,
        y: 0.0,
        w,
        h: height as f32,
    }
}

/// A square icon slot inside the ribbon, stacked from the top (0-indexed).
pub fn ribbon_slot_rect(i: usize, width: u32, scale: f32) -> LayoutRect {
    let w = (RIBBON_W * scale).round();
    let side = w; // square
    LayoutRect {
        x: width as f32 - w,
        y: i as f32 * side,
        w: side,
        h: side,
    }
}

/// The tool panel card, immediately left of the ribbon, vertically padded like
/// `terminal_area` (i.e. inset by `AREA_PAD` top and bottom).
pub fn tool_panel(width: u32, height: u32, scale: f32, panel_w: f32) -> LayoutRect {
    let ribbon_w = (RIBBON_W * scale).round();
    let pw = (panel_w * scale).round();
    let pad = (AREA_PAD * scale).round();
    LayoutRect {
        x: width as f32 - ribbon_w - pw,
        y: pad,
        w: pw,
        h: (height as f32 - 2.0 * pad).max(0.0),
    }
}

#[cfg(test)]
mod ribbon_tests {
    use super::*;

    #[test]
    fn area_panel_and_ribbon_do_not_overlap() {
        let (w, h, scale) = (1600, 1000, 2.0);
        let panel_w = TOOL_PANEL_DEFAULT_W;
        let area = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, RIBBON_W + panel_w);
        let panel = tool_panel(w, h, scale, panel_w);
        let rib = ribbon(w, h, scale);
        assert!(area.x + area.w <= panel.x);
        assert!(panel.x + panel.w <= rib.x);
        assert_eq!(rib.x + rib.w, w as f32);
    }

    #[test]
    fn area_narrows_by_exactly_the_panel_width() {
        let (w, h, scale) = (1600, 1000, 2.0);
        let closed = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, RIBBON_W);
        let open = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, RIBBON_W + 380.0);
        assert_eq!(closed.w - open.w, 380.0 * scale);
        assert_eq!(closed.h, open.h);
    }

    #[test]
    fn ribbon_slots_sit_inside_the_ribbon() {
        let (w, h, scale) = (1600, 1000, 2.0);
        let rib = ribbon(w, h, scale);
        for i in 0..3 {
            let slot = ribbon_slot_rect(i, w, scale);
            assert!(slot.x >= rib.x && slot.x + slot.w <= rib.x + rib.w);
            assert!(slot.y >= rib.y && slot.y + slot.h <= rib.y + rib.h);
        }
    }
}

#[cfg(test)]
mod flyover_tests {
    use super::*;

    /// At anim=1.0, the panel should be fully on-screen (resting_y = height - panel_h).
    #[test]
    fn flyover_rect_fully_visible() {
        let r = flyover_rect(1000, 800, 1.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        let expected_h = (800.0 * FLYOVER_DEFAULT_FRAC).round();
        let expected_y = 800.0 - expected_h;
        assert_eq!(r.h, expected_h);
        assert!((r.y - expected_y).abs() < 1.0, "y={} expected={}", r.y, expected_y);
        // Inset applied to both sides.
        assert_eq!(r.x, FLYOVER_INSET);
        assert_eq!(r.w, 1000.0 - 2.0 * FLYOVER_INSET);
    }

    /// At anim=0.0, the panel top should be at the bottom of the window (off-screen).
    #[test]
    fn flyover_rect_hidden() {
        let r = flyover_rect(1000, 800, 1.0, 0.0, FLYOVER_DEFAULT_FRAC, false);
        assert!((r.y - 800.0).abs() < 1.0, "y={} should equal height={}", r.y, 800.0);
    }

    /// At anim=0.5, the panel should be halfway between off-screen and resting.
    #[test]
    fn flyover_rect_mid_anim() {
        let r = flyover_rect(1000, 800, 1.0, 0.5, FLYOVER_DEFAULT_FRAC, false);
        let panel_h = (800.0 * FLYOVER_DEFAULT_FRAC).round();
        let resting_y = 800.0 - panel_h;
        let expected_y = lerp(800.0, resting_y, 0.5);
        assert!((r.y - expected_y).abs() < 1.0, "y={} expected={}", r.y, expected_y);
    }

    /// Tab-bar height matches TILE_TAB_H * scale.
    #[test]
    fn flyover_tab_bar_height() {
        let panel = flyover_rect(1000, 800, 2.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        let bar = flyover_tab_bar(&panel, 2.0);
        assert_eq!(bar.h, (TILE_TAB_H * 2.0).round());
        assert_eq!(bar.y, panel.y);
    }

    /// Content rect starts just below the tab bar.
    #[test]
    fn flyover_content_below_tab_bar() {
        let panel = flyover_rect(1000, 800, 2.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        let bar = flyover_tab_bar(&panel, 2.0);
        let content = flyover_content(&panel, 2.0);
        assert_eq!(content.y, panel.y + bar.h);
        assert_eq!(content.h, (panel.h - bar.h).max(0.0));
    }

    /// A custom height fraction drives the panel height; out-of-range values
    /// clamp to the resize bounds.
    #[test]
    fn flyover_rect_respects_frac_and_clamps() {
        let r = flyover_rect(1000, 800, 1.0, 1.0, 0.6, false);
        assert_eq!(r.h, (800.0_f32 * 0.6).round());
        let low = flyover_rect(1000, 800, 1.0, 1.0, 0.01, false);
        assert_eq!(low.h, (800.0 * FLYOVER_MIN_FRAC).round());
        let high = flyover_rect(1000, 800, 1.0, 1.0, 5.0, false);
        assert_eq!(high.h, (800.0 * FLYOVER_MAX_FRAC).round());
    }

    /// Maximized fills the window edge-to-edge regardless of frac.
    #[test]
    fn flyover_rect_maximized_fills_window() {
        let r = flyover_rect(1000, 800, 1.0, 1.0, 0.3, true);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 0.0, 1000.0, 800.0));
    }

    /// A maximized panel's first tab clears the native traffic lights.
    #[test]
    fn flyover_maximized_tabs_clear_traffic_lights() {
        let panel = flyover_rect(1000, 800, 1.0, 1.0, 0.3, true);
        let t0 = flyover_tab_rect(&panel, 0, 2, 1.0, true);
        assert_eq!(t0.x, TRAFFIC_LIGHT_SAFE_W);
        // Un-maximized panels keep their tabs at the bar's left edge.
        let normal = flyover_rect(1000, 800, 1.0, 1.0, 0.3, false);
        let n0 = flyover_tab_rect(&normal, 0, 2, 1.0, false);
        assert_eq!(n0.x, normal.x);
    }

    /// The window buttons sit inside the bar's right edge, minimize left of
    /// maximize, and tabs never overlap them.
    #[test]
    fn flyover_buttons_and_tabs_share_the_bar() {
        let panel = flyover_rect(1000, 800, 1.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        let bar = flyover_tab_bar(&panel, 1.0);
        let min = flyover_minimize_rect(&panel, 1.0);
        let max = flyover_maximize_rect(&panel, 1.0);
        assert_eq!(max.x + max.w, bar.x + bar.w);
        assert_eq!(min.x + min.w, max.x);
        let n = 3;
        let last = flyover_tab_rect(&panel, n - 1, n, 1.0, false);
        assert!(last.x + last.w <= min.x + 0.5);
    }

    /// The close button sits inside its tab.
    #[test]
    fn flyover_tab_close_rect_inside_tab() {
        let panel = flyover_rect(1000, 800, 1.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        for n in 1..=4 {
            for i in 0..n {
                let tr = flyover_tab_rect(&panel, i, n, 1.0, false);
                let close = flyover_tab_close_rect(&panel, i, n, 1.0, false);
                assert!(close.x >= tr.x && close.x + close.w <= tr.x + tr.w + 0.5);
                assert!(close.y >= tr.y && close.y + close.h <= tr.y + tr.h + 0.5);
            }
        }
    }

    /// Tab rects are evenly divided and don't exceed TILE_TAB_MAX_W.
    #[test]
    fn flyover_tab_rect_layout() {
        let panel = flyover_rect(1000, 800, 1.0, 1.0, FLYOVER_DEFAULT_FRAC, false);
        let t0 = flyover_tab_rect(&panel, 0, 3, 1.0, false);
        let t1 = flyover_tab_rect(&panel, 1, 3, 1.0, false);
        let t2 = flyover_tab_rect(&panel, 2, 3, 1.0, false);
        // All tabs same width.
        assert_eq!(t0.w, t1.w);
        assert_eq!(t1.w, t2.w);
        // Tabs are laid out left-to-right.
        assert!(t1.x > t0.x);
        assert!(t2.x > t1.x);
        // Width doesn't exceed cap.
        assert!(t0.w <= TILE_TAB_MAX_W);
    }
}

/// Maps each tile id to the [`Dir`] of its parent split, or `None` if the
/// tile is a root leaf (no parent, so collapse has no effect).
///
/// The returned `Vec` is in tree order (same as `Node::tiles()`).
pub fn tile_collapse_axis(node: &Node) -> Vec<(u64, Option<Dir>)> {
    let mut out = Vec::new();
    collect_collapse_axis(node, None, &mut out);
    out
}

fn collect_collapse_axis(node: &Node, parent_dir: Option<Dir>, out: &mut Vec<(u64, Option<Dir>)>) {
    match node {
        Node::Leaf(tile) => out.push((tile.id, parent_dir)),
        Node::Split { dir, a, b, .. } => {
            collect_collapse_axis(a, Some(*dir), out);
            collect_collapse_axis(b, Some(*dir), out);
        }
    }
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
    fn tab_close_rect_sits_inside_its_tab() {
        let rect = LayoutRect { x: 100.0, y: 50.0, w: 900.0, h: 600.0 };
        for scale in [1.0, 2.0] {
            for n in [1, 3, 8] {
                for i in 0..n {
                    for has_caret in [false, true] {
                        let tr = tile_tab_rect(&rect, i, n, scale, has_caret);
                        let close = tile_tab_close_rect(&rect, i, n, scale, has_caret);
                        assert!(close.x >= tr.x && close.x + close.w <= tr.x + tr.w);
                        assert!(close.y >= tr.y && close.y + close.h <= tr.y + tr.h);
                    }
                }
            }
        }
    }

    #[test]
    fn tab_insert_gap_resolves_to_nearest_boundary() {
        let rect = LayoutRect { x: 100.0, y: 50.0, w: 900.0, h: 600.0 };
        for scale in [1.0, 2.0] {
            for has_caret in [false, true] {
                let n = 3;
                let t0 = tile_tab_rect(&rect, 0, n, scale, has_caret);
                // Left half of a tab → gap before it; right half → gap after.
                for i in 0..n {
                    let left = t0.x + i as f32 * t0.w + t0.w * 0.25;
                    let right = t0.x + i as f32 * t0.w + t0.w * 0.75;
                    assert_eq!(tile_tab_insert_gap(&rect, left, n, scale, has_caret), i);
                    assert_eq!(tile_tab_insert_gap(&rect, right, n, scale, has_caret), i + 1);
                }
                // Clamped at both ends, even past the strip.
                assert_eq!(tile_tab_insert_gap(&rect, t0.x - 50.0, n, scale, has_caret), 0);
                assert_eq!(
                    tile_tab_insert_gap(&rect, t0.x + 100.0 * t0.w, n, scale, has_caret),
                    n
                );
            }
        }
    }

    #[test]
    fn tab_insert_line_sits_on_gap_boundaries() {
        let rect = LayoutRect { x: 100.0, y: 50.0, w: 900.0, h: 600.0 };
        let (scale, n, has_caret) = (2.0, 3, true);
        let t0 = tile_tab_rect(&rect, 0, n, scale, has_caret);
        for gap in 0..=n {
            let line = tile_tab_insert_line(&rect, gap, n, scale, has_caret);
            // Centered on the boundary between tabs gap-1 and gap.
            let boundary = t0.x + gap as f32 * t0.w;
            assert!((line.x + line.w / 2.0 - boundary).abs() < 0.51);
            // Spans the tab height, no more.
            assert_eq!(line.y, t0.y);
            assert_eq!(line.h, t0.h);
        }
    }

    #[test]
    fn any_unread_false_when_no_tabs_are_unread() {
        // Empty tile has no tabs → false.
        let ws = Workspace::new("g".into(), Tile::empty(7), None);
        assert!(!ws.any_unread());

        // Tile with a single tab that is not unread → false.
        use crate::term::Session;
        let sess = Session::placeholder();
        let tile = Tile::new(42, sess);
        let ws = Workspace::new("g".into(), tile, None);
        assert!(!ws.any_unread());
    }

    #[test]
    fn any_unread_true_when_inactive_tab_is_unread() {
        // Proves any_unread is not limited to the active tab.
        use crate::term::Session;
        let sess = Session::placeholder();
        let mut tile = Tile::new(42, sess);
        // Add a second tab and mark only it unread; leave active=0 (the first tab).
        let sess2 = Session::placeholder();
        tile.tabs.push(Tab::new(sess2));
        tile.tabs[1].unread = true;
        assert_eq!(tile.active, 0);
        let ws = Workspace::new("g".into(), tile, None);
        assert!(ws.any_unread());
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
        let area = terminal_area(w, h, scale, sidebar_w, 0.0);
        let cta = empty_state_cta(w, h, scale, sidebar_w, 0.0);
        assert!(cta.x >= area.x && cta.x + cta.w <= area.x + area.w);
        assert!(cta.y >= area.y && cta.y + cta.h <= area.y + area.h);
        // Horizontally centered.
        let left_gap = cta.x - area.x;
        let right_gap = (area.x + area.w) - (cta.x + cta.w);
        assert!((left_gap - right_gap).abs() <= 1.0);
    }

    #[test]
    fn collapsed_terminal_area_fills_the_window() {
        // sidebar_w == 0 (collapsed): full height, thin insets all around —
        // the traffic lights carve into the tab strip, not the area.
        let (w, h, scale) = (1600, 1000, 2.0);
        let area = terminal_area(w, h, scale, 0.0, 0.0);
        let pad = (AREA_PAD * scale).round();
        assert_eq!(area.x, pad);
        assert_eq!(area.y, pad);
        assert_eq!(area.w, w as f32 - 2.0 * pad);
        assert_eq!(area.h, h as f32 - 2.0 * pad);
    }

    #[test]
    fn tab_strip_inset_only_hits_the_top_left_tile_while_collapsed() {
        let (w, h, scale) = (1600, 1000, 2.0);
        let area = terminal_area(w, h, scale, 0.0, 0.0);
        let safe = (TRAFFIC_LIGHT_SAFE_W * scale).round();

        // Top-left tile: strip starts right of the traffic lights, same span
        // otherwise (right edge, y band unchanged).
        let top_left = LayoutRect { x: area.x, y: area.y, w: 800.0, h: 400.0 };
        let strip = tab_strip_rect(area, &top_left, scale, 0.0);
        assert_eq!(strip.x, safe);
        assert_eq!(strip.x + strip.w, top_left.x + top_left.w);
        assert_eq!((strip.y, strip.h), (top_left.y, top_left.h));

        // A tile in from either edge keeps its rect.
        let right = LayoutRect { x: area.x + 800.0, y: area.y, w: 800.0, h: 400.0 };
        assert_eq!(tab_strip_rect(area, &right, scale, 0.0), right);
        let below = LayoutRect { x: area.x, y: area.y + 400.0, w: 800.0, h: 400.0 };
        assert_eq!(tab_strip_rect(area, &below, scale, 0.0), below);

        // Expanded, even the top-left tile keeps its rect.
        let ex_area = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, 0.0);
        let ex_tile = LayoutRect { x: ex_area.x, y: ex_area.y, w: 800.0, h: 400.0 };
        assert_eq!(tab_strip_rect(ex_area, &ex_tile, scale, SIDEBAR_DEFAULT_W), ex_tile);

        // A tile narrower than the safe corner cedes everything, no negatives.
        let sliver = LayoutRect { x: area.x, y: area.y, w: 40.0, h: 400.0 };
        let s = tab_strip_rect(area, &sliver, scale, 0.0);
        assert_eq!(s.w, 0.0);
        assert_eq!(s.x, sliver.x + sliver.w);
    }

    #[test]
    fn expanded_terminal_area_keeps_thin_top_inset() {
        // Any visible sidebar width keeps the original geometry: flush to the
        // sidebar edge, inset only by the thin pad on top.
        let (w, h, scale) = (1600, 1000, 2.0);
        for sidebar_w in [SIDEBAR_MIN_W, SIDEBAR_DEFAULT_W, SIDEBAR_MAX_W] {
            let area = terminal_area(w, h, scale, sidebar_w, 0.0);
            let pad = (AREA_PAD * scale).round();
            assert_eq!(area.x, (sidebar_w * scale).round());
            assert_eq!(area.y, pad);
            assert_eq!(area.h, h as f32 - 2.0 * pad);
        }
    }

    #[test]
    fn collapsed_sidebar_edge_never_hovers() {
        // With the sidebar collapsed the edge sits at x=0; a pointer near the
        // window's left edge must not read as a sidebar-resize grab.
        let node = Node::Leaf(Tile::empty(1));
        let area = terminal_area(1600, 1000, 2.0, 0.0, 0.0);
        assert_eq!(resize_hover_at(&node, area, 2.0, 0.0, 12.0, false, 4.0, 500.0), None);
    }

    #[test]
    fn window_edge_padding_matches_tile_gap() {
        // The outer border around the tile area should read exactly as thin
        // as the dividers between tiles.
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let area = terminal_area(w, h, scale, sidebar_w, 0.0);
        let gap = (TILE_GAP * scale).round();
        assert_eq!(area.y, gap);
        assert_eq!((w as f32) - (area.x + area.w), gap);
        assert_eq!((h as f32) - (area.y + area.h), gap);
    }

    #[test]
    fn empty_state_hint_sits_below_cta() {
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let cta = empty_state_cta(w, h, scale, sidebar_w, 0.0);
        let hint = empty_state_hint(w, h, scale, sidebar_w, 0.0);
        assert!(hint.y >= cta.y + cta.h);
        assert_eq!(hint.x, cta.x);
    }

    fn ws(name: &str, section: Option<u64>) -> Workspace {
        let mut w = Workspace::new(name.into(), Tile::empty(0), None);
        w.section = section;
        w
    }

    fn sec(id: u64, collapsed: bool) -> Section {
        Section {
            id,
            name: format!("sec{id}"),
            emoji: String::new(),
            collapsed,
        }
    }

    // --- (a) split_leading_emoji ---

    #[test]
    fn split_emoji_and_name() {
        let (e, n) = split_leading_emoji("🚀 rockets");
        assert_eq!(e.as_deref(), Some("🚀"));
        assert_eq!(n, "rockets");
    }

    #[test]
    fn split_name_only() {
        let (e, n) = split_leading_emoji("plain section");
        assert_eq!(e, None);
        assert_eq!(n, "plain section");
    }

    #[test]
    fn split_emoji_only() {
        let (e, n) = split_leading_emoji("🔥");
        assert_eq!(e.as_deref(), Some("🔥"));
        assert_eq!(n, "");
    }

    #[test]
    fn split_empty() {
        let (e, n) = split_leading_emoji("   ");
        assert_eq!(e, None);
        assert_eq!(n, "");
    }

    #[test]
    fn split_multi_codepoint_emoji() {
        // Woman technologist: ZWJ sequence, all non-ASCII.
        let (e, n) = split_leading_emoji("👩‍💻 work");
        assert_eq!(e.as_deref(), Some("👩‍💻"));
        assert_eq!(n, "work");
    }

    #[test]
    fn apply_rename_keeps_name_when_empty_after_parse() {
        let mut s = Section {
            id: 1,
            name: "keep".into(),
            emoji: "旧".into(),
            collapsed: false,
        };
        apply_section_rename(&mut s, "✨");
        assert_eq!(s.emoji, "✨");
        assert_eq!(s.name, "keep");
    }

    // --- (b) row derivation ---

    #[test]
    fn rows_mixed_grouped_and_ungrouped() {
        let workspaces = vec![
            ws("a", None),
            ws("b", Some(1)),
            ws("c", Some(1)),
            ws("d", None),
        ];
        let sections = vec![sec(1, false)];
        let rows = sidebar_rows(&workspaces, &sections);
        assert_eq!(
            rows,
            vec![
                SidebarRow::Group { ws_idx: 0 },
                SidebarRow::SectionHeader { section_idx: 0 },
                SidebarRow::Group { ws_idx: 1 },
                SidebarRow::Group { ws_idx: 2 },
                SidebarRow::Group { ws_idx: 3 },
            ]
        );
    }

    #[test]
    fn rows_collapsed_hides_members() {
        let workspaces = vec![ws("a", Some(1)), ws("b", Some(1)), ws("c", None)];
        let sections = vec![sec(1, true)];
        let rows = sidebar_rows(&workspaces, &sections);
        assert_eq!(
            rows,
            vec![
                SidebarRow::SectionHeader { section_idx: 0 },
                SidebarRow::Group { ws_idx: 2 },
            ]
        );
    }

    #[test]
    fn rows_empty_section_at_end() {
        let workspaces = vec![ws("a", None)];
        let sections = vec![sec(9, false), sec(8, true)];
        let rows = sidebar_rows(&workspaces, &sections);
        assert_eq!(
            rows,
            vec![
                SidebarRow::Group { ws_idx: 0 },
                SidebarRow::SectionHeader { section_idx: 0 },
                SidebarRow::SectionHeader { section_idx: 1 },
            ]
        );
    }

    #[test]
    fn active_in_collapsed_maps_to_header() {
        let workspaces = vec![ws("a", Some(1)), ws("b", Some(1)), ws("c", None)];
        let sections = vec![sec(1, true)];
        let rows = sidebar_rows(&workspaces, &sections);
        assert_eq!(active_row_index(&rows, &workspaces, &sections, 1), Some(0));
        assert_eq!(active_row_index(&rows, &workspaces, &sections, 2), Some(1));
    }

    // --- (c) geometry ---

    #[test]
    fn geometry_headers_slimmer_members_indented() {
        let workspaces = vec![ws("a", Some(1)), ws("b", Some(1)), ws("c", None)];
        let sections = vec![sec(1, false)];
        let rows = sidebar_rows(&workspaces, &sections);
        let scale = 2.0;
        let sw = SIDEBAR_DEFAULT_W;
        let header = sidebar_row_rect(&rows, 0, &workspaces, scale, sw);
        let member = sidebar_row_rect(&rows, 1, &workspaces, scale, sw);
        let bare = sidebar_row_rect(&rows, 3, &workspaces, scale, sw);

        assert!(header.h < member.h);
        assert_eq!(header.h, (SECTION_HEADER_H * scale).round());
        assert_eq!(member.h, (TAB_H * scale).round());
        assert!(member.x > header.x);
        assert!((member.x + member.w - (header.x + header.w)).abs() <= 0.5);
        assert_eq!(bare.x, header.x);

        // Non-overlapping and top-to-bottom ordered.
        let mut prev_bottom = f32::NEG_INFINITY;
        for i in 0..rows.len() {
            let r = sidebar_row_rect(&rows, i, &workspaces, scale, sw);
            assert!(r.y >= prev_bottom);
            prev_bottom = r.y + r.h;
        }
    }

    #[test]
    fn new_buttons_side_by_side() {
        let scale = 2.0;
        let sw = SIDEBAR_DEFAULT_W;
        let g = new_group_button(scale, sw);
        let s = new_section_button(scale, sw);
        assert_eq!(g.y, s.y);
        assert_eq!(g.h, s.h);
        assert!(g.x + g.w <= s.x);
        assert!(s.x + s.w > g.x + g.w);
    }

    // --- (d) move / join / leave / reorder ---

    #[test]
    fn relocate_preserves_contiguity_and_active() {
        let mut workspaces = vec![
            ws("a", Some(1)),
            ws("b", Some(1)),
            ws("c", None),
            ws("d", None),
        ];
        let sections = vec![sec(1, false)];
        // Move ungrouped c into the middle of section 1 (insert before b → index 1).
        // section_for_insert(1) sees left=a(1), right=b(1) → Some(1).
        let from = 2;
        let insert_before = 1;
        let sid = section_for_insert(&workspaces, insert_before);
        assert_eq!(sid, Some(1));
        let mut active = 3; // d
        let final_idx = relocate_workspace(&mut workspaces, from, insert_before, sid);
        active = track_index_after_relocate(active, from, final_idx);
        assert!(sections_are_contiguous(&workspaces));
        assert_eq!(workspaces[final_idx].name, "c");
        assert_eq!(workspaces[final_idx].section, Some(1));
        assert_eq!(workspaces[active].name, "d");
        let _ = sections;
    }

    #[test]
    fn join_onto_ungrouped_creates_section() {
        let mut workspaces = vec![ws("a", None), ws("b", None), ws("c", None)];
        let mut sections = Vec::new();
        let mut next_id = 1u64;
        let (new_from, created) = join_onto_group(&mut workspaces, &mut sections, &mut next_id, 2, 0);
        assert_eq!(created, Some(1));
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "section");
        assert!(!sections[0].collapsed);
        assert!(sections_are_contiguous(&workspaces));
        // Order: a, c (joined), b
        assert_eq!(workspaces[0].name, "a");
        assert_eq!(workspaces[0].section, Some(1));
        assert_eq!(workspaces[new_from].name, "c");
        assert_eq!(workspaces[new_from].section, Some(1));
        assert_eq!(new_from, 1);
    }

    #[test]
    fn join_onto_section_member_appends_adjacent() {
        let mut workspaces = vec![ws("a", Some(5)), ws("b", Some(5)), ws("c", None)];
        let mut sections = vec![sec(5, false)];
        let mut next_id = 10u64;
        let (new_from, created) = join_onto_group(&mut workspaces, &mut sections, &mut next_id, 2, 0);
        assert!(created.is_none());
        assert_eq!(workspaces[new_from].section, Some(5));
        assert!(sections_are_contiguous(&workspaces));
        assert_eq!(section_member_range(&workspaces, 5), Some((0, 3)));
    }

    #[test]
    fn append_to_section_and_leave_prunes() {
        let mut workspaces = vec![ws("a", None), ws("b", Some(2)), ws("c", Some(2))];
        let mut sections = vec![sec(2, false)];
        let idx = append_to_section(&mut workspaces, 0, 2);
        assert_eq!(workspaces[idx].section, Some(2));
        assert_eq!(section_member_range(&workspaces, 2), Some((0, 3)));

        // Leave: move last remaining members out one by one.
        let n = workspaces.len();
        for _ in 0..3 {
            if let Some((start, _)) = section_member_range(&workspaces, 2) {
                relocate_workspace(&mut workspaces, start, n, None);
            }
        }
        assert!(prune_section_if_empty(&mut sections, &workspaces, 2));
        assert!(sections.is_empty());
        assert!(workspaces.iter().all(|w| w.section.is_none()));
    }

    #[test]
    fn relocate_section_block_stays_top_level() {
        let mut workspaces = vec![
            ws("a", Some(1)),
            ws("b", Some(1)),
            ws("c", None),
            ws("d", Some(2)),
            ws("e", Some(2)),
        ];
        // Move section 2 block to the front.
        let range = relocate_section_block(&mut workspaces, 2, 0).unwrap();
        assert_eq!(range, (0, 2));
        assert_eq!(workspaces[0].name, "d");
        assert_eq!(workspaces[1].name, "e");
        assert!(sections_are_contiguous(&workspaces));

        // Attempt to drop section 1 into the middle of section 2 → snaps out.
        let range = relocate_section_block(&mut workspaces, 1, 1).unwrap();
        assert!(sections_are_contiguous(&workspaces));
        let (s, e) = range;
        assert!(workspaces[s..e].iter().all(|w| w.section == Some(1)));
    }

    #[test]
    fn ensure_active_expands_collapsed() {
        let workspaces = vec![ws("a", Some(1)), ws("b", None)];
        let mut sections = vec![sec(1, true)];
        assert!(ensure_active_section_expanded(&workspaces, &mut sections, 0));
        assert!(!sections[0].collapsed);
        assert!(!ensure_active_section_expanded(&workspaces, &mut sections, 0));
    }

    #[test]
    fn empty_section_survives_until_pruned() {
        let workspaces = vec![ws("a", None)];
        let mut sections = vec![sec(3, false)];
        // Empty section still renders.
        let rows = sidebar_rows(&workspaces, &sections);
        assert!(rows.contains(&SidebarRow::SectionHeader { section_idx: 0 }));
        // Not auto-pruned just by existing empty.
        assert!(!workspaces.iter().any(|w| w.section == Some(3)));
        // Explicit prune removes it.
        assert!(prune_section_if_empty(&mut sections, &workspaces, 3));
    }

    fn row_split() -> Node {
        Node::Split {
            dir: Dir::Row,
            ratio: 0.5,
            a: Box::new(Node::Leaf(Tile::empty(1))),
            b: Box::new(Node::Leaf(Tile::empty(2))),
        }
    }

    #[test]
    fn resize_hover_at_divider_and_sidebar() {
        let scale = 2.0;
        let grab = 6.0 * scale;
        let area = LayoutRect { x: 100.0, y: 10.0, w: 800.0, h: 600.0 };
        let sidebar_edge_x = 100.0;
        let node = row_split();
        let (_, dividers) = layout_tiles(&node, area, scale);
        assert_eq!(dividers.len(), 1);
        let d = &dividers[0];
        assert_eq!(d.dir, Dir::Row);

        // Point on the divider (center of its rect) → Divider with Row dir.
        let dx = d.rect.x + d.rect.w / 2.0;
        let dy = d.rect.y + d.rect.h / 2.0;
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, grab, true, dx, dy),
            Some(ResizeHover::Divider { path: vec![], dir: Dir::Row })
        );

        // Point at the sidebar edge → Sidebar.
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, grab, true, sidebar_edge_x, 200.0),
            Some(ResizeHover::Sidebar)
        );

        // Point in a tile interior → None.
        let (tiles, _) = layout_tiles(&node, area, scale);
        let t = &tiles[0].1;
        let ix = t.x + t.w / 2.0;
        let iy = t.y + t.h / 2.0;
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, grab, true, ix, iy),
            None
        );

        // dividers_active=false suppresses divider hits but not the sidebar.
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, grab, false, dx, dy),
            None
        );
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, grab, false, sidebar_edge_x, 200.0),
            Some(ResizeHover::Sidebar)
        );
    }

    #[test]
    fn resize_hover_at_column_divider() {
        let scale = 1.0;
        let grab = 6.0;
        let area = LayoutRect { x: 0.0, y: 0.0, w: 400.0, h: 400.0 };
        let node = Node::Split {
            dir: Dir::Column,
            ratio: 0.5,
            a: Box::new(Node::Leaf(Tile::empty(1))),
            b: Box::new(Node::Leaf(Tile::empty(2))),
        };
        let (_, dividers) = layout_tiles(&node, area, scale);
        let d = &dividers[0];
        let dx = d.rect.x + d.rect.w / 2.0;
        let dy = d.rect.y + d.rect.h / 2.0;
        assert_eq!(
            resize_hover_at(&node, area, scale, -100.0, grab, true, dx, dy),
            Some(ResizeHover::Divider { path: vec![], dir: Dir::Column })
        );
    }

    // ── directional_neighbor tests ───────────────────────────────────────────

    /// Helper: build a LayoutRect from (x, y, w, h).
    fn r(x: f32, y: f32, w: f32, h: f32) -> LayoutRect {
        LayoutRect { x, y, w, h }
    }

    /// 2×2 grid:
    ///   1(TL) | 2(TR)
    ///   3(BL) | 4(BR)
    /// Each cell is 100×100; gap is 0 for simplicity.
    fn grid_2x2() -> Vec<(u64, LayoutRect)> {
        vec![
            (1, r(0.0,   0.0,   100.0, 100.0)),  // top-left
            (2, r(100.0, 0.0,   100.0, 100.0)),  // top-right
            (3, r(0.0,   100.0, 100.0, 100.0)),  // bottom-left
            (4, r(100.0, 100.0, 100.0, 100.0)),  // bottom-right
        ]
    }

    #[test]
    fn directional_neighbor_2x2_all_directions() {
        let tiles = grid_2x2();

        // From top-left (1): right→2, down→3, left→None, up→None
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Right), Some(2));
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Down),  Some(3));
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Left),  None);
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Up),    None);

        // From top-right (2): left→1, down→4, right→None, up→None
        assert_eq!(directional_neighbor(&tiles, 2, NavDir::Left),  Some(1));
        assert_eq!(directional_neighbor(&tiles, 2, NavDir::Down),  Some(4));
        assert_eq!(directional_neighbor(&tiles, 2, NavDir::Right), None);
        assert_eq!(directional_neighbor(&tiles, 2, NavDir::Up),    None);

        // From bottom-left (3): right→4, up→1
        assert_eq!(directional_neighbor(&tiles, 3, NavDir::Right), Some(4));
        assert_eq!(directional_neighbor(&tiles, 3, NavDir::Up),    Some(1));
        assert_eq!(directional_neighbor(&tiles, 3, NavDir::Left),  None);
        assert_eq!(directional_neighbor(&tiles, 3, NavDir::Down),  None);

        // From bottom-right (4): left→3, up→2
        assert_eq!(directional_neighbor(&tiles, 4, NavDir::Left),  Some(3));
        assert_eq!(directional_neighbor(&tiles, 4, NavDir::Up),    Some(2));
        assert_eq!(directional_neighbor(&tiles, 4, NavDir::Right), None);
        assert_eq!(directional_neighbor(&tiles, 4, NavDir::Down),  None);
    }

    #[test]
    fn directional_neighbor_missing_from_returns_none() {
        let tiles = grid_2x2();
        assert_eq!(directional_neighbor(&tiles, 99, NavDir::Left), None);
        assert_eq!(directional_neighbor(&tiles, 99, NavDir::Right), None);
    }

    #[test]
    fn directional_neighbor_overlap_preferred_over_nearer_nonoverlapping() {
        // Layout: tile 1 is on the left (tall).
        //         tile 2 is directly to the right of 1 (overlapping vertically).
        //         tile 3 is also to the right but far above (no vertical overlap).
        //   1 (0,50,100,100)  |  2 (100,50,100,100)   <- same vertical band
        //                        3 (100,0,40,40)       <- above, no overlap with 1
        // Euclidean center of 3 from 1: ~(150,20) vs (150,100) for 2.
        // Even if 3 were closer in raw distance, 2 overlaps so 2 wins.
        let tiles = vec![
            (1, r(0.0,  50.0, 100.0, 100.0)),
            (2, r(100.0, 50.0, 100.0, 100.0)),
            (3, r(100.0,  0.0,  40.0,  40.0)),
        ];
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Right), Some(2));
    }

    #[test]
    fn directional_neighbor_fallback_to_nearest_when_no_overlap() {
        // Two tiles to the right of 1 but neither overlaps vertically.
        // tile 2 is closer (center distance).
        let tiles = vec![
            (1, r(0.0, 0.0, 100.0, 50.0)),   // center (50, 25)
            (2, r(100.0, 60.0, 100.0, 50.0)), // center (150, 85) — closer
            (3, r(100.0, 200.0, 100.0, 50.0)),// center (150, 225) — farther
        ];
        assert_eq!(directional_neighbor(&tiles, 1, NavDir::Right), Some(2));
    }

    // --- collapse ---

    fn leaf(id: u64, collapsed: bool) -> Node {
        let mut t = Tile::empty(id);
        t.collapsed = collapsed;
        t.collapse_anim = if collapsed { 1.0 } else { 0.0 };
        Node::Leaf(t)
    }

    fn split(dir: Dir, ratio: f32, a: Node, b: Node) -> Node {
        Node::Split { dir, ratio, a: Box::new(a), b: Box::new(b) }
    }

    fn rect_of(tiles: &[(u64, LayoutRect)], id: u64) -> LayoutRect {
        tiles.iter().find(|(t, _)| *t == id).map(|(_, r)| *r).unwrap()
    }

    const AREA: LayoutRect = LayoutRect { x: 0.0, y: 0.0, w: 1200.0, h: 800.0 };

    #[test]
    fn collapsed_column_pane_shrinks_to_tab_bar() {
        let scale = 2.0;
        let gap = (TILE_GAP * scale).round();
        let ce = (TILE_TAB_H * scale).round();
        let node = split(Dir::Column, 0.5, leaf(1, false), leaf(2, true));
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        let a = rect_of(&tiles, 1);
        let b = rect_of(&tiles, 2);
        assert_eq!(b.h, ce);
        assert_eq!(a.h, AREA.h - gap - ce);
        assert_eq!(b.y, a.h + gap);
        // Widths untouched by a stacked collapse.
        assert_eq!(a.w, AREA.w);
        assert_eq!(b.w, AREA.w);
    }

    #[test]
    fn collapsed_row_pane_shrinks_to_narrow_strip() {
        let scale = 2.0;
        let gap = (TILE_GAP * scale).round();
        let ce = (TILE_TAB_H * scale).round();
        let node = split(Dir::Row, 0.5, leaf(1, true), leaf(2, false));
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        let a = rect_of(&tiles, 1);
        let b = rect_of(&tiles, 2);
        assert_eq!(a.w, ce);
        assert_eq!(b.w, AREA.w - gap - ce);
        assert_eq!(b.x, ce + gap);
    }

    #[test]
    fn siblings_absorb_freed_space_proportionally() {
        // Column(a, Column(b, c)): collapsing a hands its space to the b/c
        // subtree, which keeps splitting by its own (untouched) ratio.
        let scale = 1.0;
        let gap = (TILE_GAP * scale).round();
        let ce = (TILE_TAB_H * scale).round();
        let node = split(
            Dir::Column,
            0.5,
            leaf(1, true),
            split(Dir::Column, 0.25, leaf(2, false), leaf(3, false)),
        );
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        let a = rect_of(&tiles, 1);
        let b = rect_of(&tiles, 2);
        let c = rect_of(&tiles, 3);
        assert_eq!(a.h, ce);
        let rest = AREA.h - gap - ce;
        assert_eq!(b.h, ((rest - gap) * 0.25).round());
        assert_eq!(c.h, rest - gap - b.h);
    }

    #[test]
    fn expand_restores_exact_previous_layout() {
        let scale = 2.0;
        let mut node = split(Dir::Column, 0.37, leaf(1, false), leaf(2, false));
        let (before, _) = layout_tiles(&node, AREA, scale);
        for (collapsed, anim) in [(true, 1.0_f32), (false, 0.0)] {
            if let Some(t) = node.find_tile_mut(2) {
                t.collapsed = collapsed;
                t.collapse_anim = anim;
            }
        }
        let (after, _) = layout_tiles(&node, AREA, scale);
        for ((ida, ra), (idb, rb)) in before.iter().zip(after.iter()) {
            assert_eq!(ida, idb);
            assert_eq!((ra.x, ra.y, ra.w, ra.h), (rb.x, rb.y, rb.w, rb.h));
        }
    }

    #[test]
    fn nested_fully_collapsed_split_stacks_extents() {
        // Column(a, Column(b, c)) with b and c collapsed: the whole subtree
        // is collapsed, occupying two tab bars plus the gap between them.
        let scale = 1.0;
        let gap = (TILE_GAP * scale).round();
        let ce = (TILE_TAB_H * scale).round();
        let node = split(
            Dir::Column,
            0.5,
            leaf(1, false),
            split(Dir::Column, 0.5, leaf(2, true), leaf(3, true)),
        );
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        let a = rect_of(&tiles, 1);
        let b = rect_of(&tiles, 2);
        let c = rect_of(&tiles, 3);
        assert_eq!(b.h, ce);
        assert_eq!(c.h, ce);
        assert_eq!(a.h, AREA.h - gap - (ce + gap + ce));

        // Perpendicular nesting: Row(a, Column(b collapsed, c collapsed))
        // takes the max across the axis — one strip width.
        let node = split(
            Dir::Row,
            0.5,
            leaf(1, false),
            split(Dir::Column, 0.5, leaf(2, true), leaf(3, true)),
        );
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        assert_eq!(rect_of(&tiles, 2).w, ce);
        assert_eq!(rect_of(&tiles, 1).w, AREA.w - gap - ce);
    }

    #[test]
    fn mid_animation_extent_is_between_endpoints() {
        let scale = 1.0;
        let gap = (TILE_GAP * scale).round();
        let ce = (TILE_TAB_H * scale).round();
        let mut node = split(Dir::Column, 0.5, leaf(1, false), leaf(2, true));
        if let Some(t) = node.find_tile_mut(2) {
            t.collapse_anim = 0.5;
        }
        let (tiles, _) = layout_tiles(&node, AREA, scale);
        let b = rect_of(&tiles, 2);
        let natural = ((AREA.h - gap) * 0.5).round();
        assert!(b.h > ce && b.h < natural, "mid-anim height {} not between", b.h);
    }

    #[test]
    fn no_divider_on_a_collapsed_edge() {
        let scale = 1.0;
        let node = split(Dir::Column, 0.5, leaf(1, false), leaf(2, true));
        let (_, dividers) = layout_tiles(&node, AREA, scale);
        assert!(dividers.is_empty());
        // Expanded panes keep their divider.
        let node = split(Dir::Column, 0.5, leaf(1, false), leaf(2, false));
        let (_, dividers) = layout_tiles(&node, AREA, scale);
        assert_eq!(dividers.len(), 1);
    }

    #[test]
    fn caret_rect_right_aligned_and_tabs_avoid_it() {
        let rect = LayoutRect { x: 100.0, y: 50.0, w: 900.0, h: 600.0 };
        for scale in [1.0, 2.0] {
            let bar = tile_tab_bar(&rect, scale);
            let caret = tile_caret_rect(&rect, scale);
            // Right-aligned square inside the bar.
            assert_eq!(caret.x + caret.w, bar.x + bar.w);
            assert!(caret.y >= bar.y && caret.y + caret.h <= bar.y + bar.h);
            // Tabs start at the bar's left edge either way; with a caret the
            // last tab must end at or before the caret square.
            for n in [1, 2, 4] {
                let first = tile_tab_rect(&rect, 0, n, scale, true);
                assert_eq!(first.x, bar.x);
                let last = tile_tab_rect(&rect, n - 1, n, scale, true);
                assert!(last.x + last.w <= caret.x + 1.0);
                // Close button stays inside its tab.
                let close = tile_tab_close_rect(&rect, n - 1, n, scale, true);
                assert!(close.x >= last.x && close.x + close.w <= last.x + last.w);
            }
        }
    }

    #[test]
    fn tile_collapse_axis_maps_parent_dirs() {
        let node = split(
            Dir::Row,
            0.5,
            leaf(1, false),
            split(Dir::Column, 0.5, leaf(2, false), leaf(3, false)),
        );
        let axes = tile_collapse_axis(&node);
        assert_eq!(axes, vec![(1, Some(Dir::Row)), (2, Some(Dir::Column)), (3, Some(Dir::Column))]);
        // A root leaf cannot collapse.
        let axes = tile_collapse_axis(&leaf(9, false));
        assert_eq!(axes, vec![(9, None)]);
    }

    #[test]
    fn fix_focus_clears_stale_collapse_on_root_leaf() {
        let mut ws = Workspace::new("g".into(), Tile::empty(1), None);
        if let Node::Leaf(t) = &mut ws.root {
            t.collapsed = true;
            t.collapse_anim = 1.0;
        }
        ws.fix_focus();
        if let Node::Leaf(t) = &ws.root {
            assert!(!t.collapsed);
            assert_eq!(t.collapse_anim, 0.0);
        } else {
            panic!("root should be a leaf");
        }
    }

    // ── Appearance page geometry tests ───────────────────────────────────────

    fn appearance_card() -> LayoutRect {
        LayoutRect { x: 50.0, y: 40.0, w: 600.0, h: 500.0 }
    }

    #[test]
    fn appearance_columns_dont_overlap() {
        let card = appearance_card();
        for scale in [1.0_f32, 2.0] {
            let col0 = appearance_column(&card, 0, scale);
            let col1 = appearance_column(&card, 1, scale);
            assert!(col0.x + col0.w <= col1.x, "cols overlap at scale {scale}");
            assert!(col1.x + col1.w <= card.x + card.w + 1.0, "col1 exceeds card at scale {scale}");
        }
    }

    #[test]
    fn appearance_dropdown_fields_inside_column() {
        let card = appearance_card();
        for scale in [1.0_f32, 2.0] {
            for col in [0, 1] {
                let column = appearance_column(&card, col, scale);
                for field in [0, 1] {
                    let d = appearance_dropdown_rect(&card, col, field, scale);
                    assert!(d.x >= column.x - 1.0, "drop x < col.x at col={col} field={field} scale={scale}");
                    assert!(d.x + d.w <= column.x + column.w + 1.0, "drop overflows col at col={col} field={field} scale={scale}");
                    assert!(d.y >= column.y - 1.0, "drop y < col.y at col={col} field={field} scale={scale}");
                }
            }
        }
    }

    #[test]
    fn appearance_menu_items_tile_panel() {
        let card = appearance_card();
        for scale in [1.0_f32, 2.0] {
            let n = 4usize;
            let panel = appearance_menu_panel(&card, 0, 0, n, scale);
            for item in 0..n {
                let r = appearance_menu_item(&card, 0, 0, n, item, scale);
                // Rows must be inside the panel.
                assert!(r.x >= panel.x - 1.0 && r.x + r.w <= panel.x + panel.w + 1.0,
                    "item {item} outside panel horizontally at scale {scale}");
                assert!(r.y >= panel.y - 1.0 && r.y + r.h <= panel.y + panel.h + 1.0,
                    "item {item} outside panel vertically at scale {scale}");
                // Consecutive rows don't overlap.
                if item + 1 < n {
                    let next = appearance_menu_item(&card, 0, 0, n, item + 1, scale);
                    assert!(r.y + r.h <= next.y + 1.0,
                        "items {item} and {} overlap at scale {scale}", item + 1);
                }
            }
        }
    }

    #[test]
    fn appearance_mode_segments_end_left_of_preview_toggle() {
        let card = appearance_card();
        for scale in [1.0_f32, 2.0] {
            let row = appearance_header_row(&card, scale);
            let mode_row = appearance_mode_row(&card, 8.0, scale);
            let seg0 = preview_segment_rect(&row, 0, 8.0, scale);
            let n = crate::theme::Mode::ALL.len();
            let last = mode_segment_rect(&mode_row, n - 1, 8.0, scale);
            assert!(
                last.x + last.w < seg0.x,
                "mode segments overlap the preview toggle at scale {scale}"
            );
        }
    }

    #[test]
    fn appearance_footer_below_preview_cards() {
        let card = appearance_card();
        for scale in [1.0_f32, 2.0] {
            let footer = appearance_footer_row(&card, scale);
            for col in [0, 1] {
                let preview = appearance_preview_card(&card, col, scale);
                assert!(
                    footer.y >= preview.y + preview.h - 1.0,
                    "footer above preview card col={col} scale={scale}"
                );
            }
        }
    }
}
