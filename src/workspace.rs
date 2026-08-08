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

    /// Whether the primary tile's active tab has unread output.
    pub fn primary_unread(&self) -> bool {
        self.root
            .find_tile(self.primary_tile)
            .and_then(|t| t.active_tab())
            .map(|tab| tab.unread)
            .unwrap_or(false)
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

/// Which resize handle the pointer is over (sidebar edge or a tile divider).
/// Drives the cursor style and the hover highlight painted in the renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum ResizeHover {
    Sidebar,
    Divider { path: Vec<u8>, dir: Dir },
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
    if (px - sidebar_edge_x).abs() <= grab {
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

/// A square caret button at the LEFT edge of a tile's tab bar.
/// Side length = `TILE_TAB_H * scale`. Present only when the tile has a
/// parent split (i.e. `tile_collapse_axis` returns `Some`).
pub fn tile_caret_rect(rect: &LayoutRect, scale: f32) -> LayoutRect {
    let bar = tile_tab_bar(rect, scale);
    let side = (TILE_TAB_H * scale).round();
    LayoutRect { x: bar.x, y: bar.y, w: side, h: side }
}

/// Rect of tab `i` of `n` in a tile's strip.
///
/// When `has_caret` is `true` (the tile has a parent split and therefore
/// shows a collapse caret button), the available width starts after the
/// caret square so tabs never overlap it.
pub fn tile_tab_rect(rect: &LayoutRect, i: usize, n: usize, scale: f32, has_caret: bool) -> LayoutRect {
    let bar = tile_tab_bar(rect, scale);
    let caret_w = if has_caret { (TILE_TAB_H * scale).round() } else { 0.0 };
    let avail_w = (bar.w - caret_w).max(0.0);
    let w = (avail_w / n.max(1) as f32).min((TILE_TAB_MAX_W * scale).round()).round();
    LayoutRect { x: bar.x + caret_w + i as f32 * w, y: bar.y, w, h: bar.h }
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
    fn primary_unread_false_on_placeholder_or_missing_primary() {
        // Empty tile has no active tab → false.
        let ws = Workspace::new("g".into(), Tile::empty(7), None);
        assert!(!ws.primary_unread());

        // Vanished primary tile → false, no panic.
        let mut ws = Workspace::new("g".into(), Tile::empty(7), None);
        ws.primary_tile = 999;
        assert!(!ws.primary_unread());
    }

    #[test]
    fn primary_unread_true_when_active_tab_is_unread() {
        use crate::term::Session;
        let sess = Session::placeholder();
        let mut tile = Tile::new(42, sess);
        let tile_id = tile.id;
        if let Some(tab) = tile.active_tab_mut() {
            tab.unread = true;
        }
        let ws = Workspace::new("g".into(), tile, None);
        assert_eq!(ws.primary_tile, tile_id);
        assert!(ws.primary_unread());
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
    fn caret_rect_sits_inside_bar_and_offsets_tabs() {
        let rect = LayoutRect { x: 100.0, y: 50.0, w: 900.0, h: 600.0 };
        for scale in [1.0, 2.0] {
            let bar = tile_tab_bar(&rect, scale);
            let caret = tile_caret_rect(&rect, scale);
            assert!(caret.x >= bar.x && caret.x + caret.w <= bar.x + bar.w);
            assert!(caret.y >= bar.y && caret.y + caret.h <= bar.y + bar.h);
            let with = tile_tab_rect(&rect, 0, 2, scale, true);
            let without = tile_tab_rect(&rect, 0, 2, scale, false);
            assert_eq!(with.x, bar.x + caret.w);
            assert_eq!(without.x, bar.x);
            // Close button stays inside its (shifted) tab.
            let close = tile_tab_close_rect(&rect, 0, 2, scale, true);
            assert!(close.x >= with.x && close.x + close.w <= with.x + with.w);
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
}
