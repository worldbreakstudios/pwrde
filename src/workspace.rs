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
//! One exception, deliberately narrow: sidebar row heights also follow the
//! `appearance.font_size` setting, because the rows are an element tree
//! ([`crate::sidebar_ui`]) whose type scales with it. The formula itself stays
//! pure — see [`sidebar_row_h`] — and only [`sidebar_row_rect`] and
//! [`tab_rect`] read the setting, so paint and hit-test still share one answer.
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

pub enum TabContent {
    Terminal(Session),
    Webview(WebviewTab),
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebviewTab {
    /// Runtime-only identity retained when a tab is moved or reordered.
    pub id: u64,
    pub url: String,
    /// Runtime-only launch command from the tab's workspace profile: a shell
    /// command whose first non-empty stdout line becomes this tab's URL once
    /// it resolves. Never persisted; `None` for ordinary webviews.
    pub url_command: Option<String>,
    /// The page's document title as last reported by the native view; empty
    /// or absent while a document is loading, so `Tab::title` falls back to
    /// the URL-derived label. Runtime-only: it arrives again on every load.
    pub title: Option<String>,
    /// Whether the tab's own back/forward/reload/URL toolbar is collapsed so
    /// the page fills the tile. Persisted; terminals have no toolbar.
    pub toolbar_hidden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Terminal,
    Webview,
}

pub struct Tab {
    pub content: TabContent,
    /// Cached grid size; meaningful only for terminal tabs.
    pub cols: usize,
    pub rows: usize,
    /// Whether this tab has unseen output (set by attention signal, cleared on focus).
    pub unread: bool,
    /// When this tab last asked for attention; retained after it is read so a
    /// card can still say how long ago that was. Set by the mark-unread paths
    /// and by an on-screen pane's attention signal, which stamps without
    /// dotting — so a stamp does not imply the tab is currently unread.
    pub unread_at: Option<std::time::SystemTime>,
    /// Browser-style pin: pinned tabs sort to the FRONT of their tile's tab
    /// strip, keep their full title, and cannot be closed. Persisted.
    pub pinned: bool,
}

impl Tab {
    /// Whether this tab draws its back/forward/reload/URL toolbar. Only
    /// webview tabs have one; terminals always report `false`.
    pub fn toolbar_hidden(&self) -> bool {
        match &self.content {
            TabContent::Terminal(_) => false,
            TabContent::Webview(webview) => webview.toolbar_hidden,
        }
    }

    /// Collapse (`hidden = true`) or restore the tab's webview toolbar.
    /// Returns whether the state changed; terminal tabs have no toolbar and
    /// always report `false`.
    pub fn set_toolbar_hidden(&mut self, hidden: bool) -> bool {
        let TabContent::Webview(webview) = &mut self.content else { return false };
        if webview.toolbar_hidden == hidden {
            return false;
        }
        webview.toolbar_hidden = hidden;
        true
    }

    pub fn new(session: Session) -> Self {
        Self {
            content: TabContent::Terminal(session),
            cols: 0,
            rows: 0,
            unread: false,
            unread_at: None,
            pinned: false,
        }
    }

    pub fn webview(id: u64, url: String) -> Self {
        Self::webview_with_command(id, url, None)
    }

    /// Like [`Tab::webview`], but records the profile `url_command` the tab was
    /// materialized from. Runtime-only metadata: see [`WebviewTab::url_command`].
    pub fn webview_with_command(id: u64, url: String, url_command: Option<String>) -> Self {
        Self {
            content: TabContent::Webview(WebviewTab {
                id,
                url,
                url_command,
                title: None,
                toolbar_hidden: false,
            }),
            cols: 0,
            rows: 0,
            unread: false,
            unread_at: None,
            pinned: false,
        }
    }

    pub fn kind(&self) -> TabKind {
        match &self.content {
            TabContent::Terminal(_) => TabKind::Terminal,
            TabContent::Webview(_) => TabKind::Webview,
        }
    }

    pub fn session(&self) -> Option<&Session> {
        match &self.content {
            TabContent::Terminal(session) => Some(session),
            TabContent::Webview(_) => None,
        }
    }

    pub fn session_mut(&mut self) -> Option<&mut Session> {
        match &mut self.content {
            TabContent::Terminal(session) => Some(session),
            TabContent::Webview(_) => None,
        }
    }

    pub fn webview_id(&self) -> Option<u64> {
        match &self.content {
            TabContent::Terminal(_) => None,
            TabContent::Webview(webview) => Some(webview.id),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match &self.content {
            TabContent::Terminal(_) => None,
            TabContent::Webview(webview) => Some(&webview.url),
        }
    }

    pub fn set_webview_url(&mut self, url: String) -> bool {
        let TabContent::Webview(webview) = &mut self.content else { return false };
        if webview.url == url {
            return false;
        }
        webview.url = url;
        true
    }

    /// The profile `url_command` this webview tab was launched from, if any.
    /// Runtime-only; see [`WebviewTab::url_command`].
    pub fn url_command(&self) -> Option<&str> {
        match &self.content {
            TabContent::Terminal(_) => None,
            TabContent::Webview(webview) => webview.url_command.as_deref(),
        }
    }

    /// Record the document title the native view reported. An empty title
    /// (a new document starting to load) clears the cached one so the tab
    /// shows the URL label again. Returns whether the cached title changed.
    pub fn set_webview_title(&mut self, title: String) -> bool {
        let TabContent::Webview(webview) = &mut self.content else { return false };
        let next = Some(title.trim().to_string()).filter(|title| !title.is_empty());
        if webview.title == next {
            return false;
        }
        webview.title = next;
        true
    }

    pub fn title(&self) -> String {
        match &self.content {
            TabContent::Terminal(session) => session.title(),
            TabContent::Webview(webview) => webview
                .title
                .clone()
                .unwrap_or_else(|| webview_title(&webview.url)),
        }
    }
}

pub fn webview_title(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if let Some((_, after_host)) = rest.split_once('/') {
        let path = after_host.split(['?', '#']).next().unwrap_or("").trim_matches('/');
        if let Some(segment) = path.split('/').next().filter(|part| !part.is_empty()) {
            return format!("{host}/{segment}");
        }
    }
    if host.is_empty() { url.to_string() } else { host.to_string() }
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

    /// Number of pinned tabs: tabs are kept ordered pinned-first, so this is
    /// the leading run of `pinned` tabs.
    pub fn pinned_count(&self) -> usize {
        self.tabs.iter().take_while(|tab| tab.pinned).count()
    }

    /// Insert `tab` as close to `index` as the pinned-first order allows: a
    /// pinned tab never lands after an unpinned one and an unpinned tab never
    /// lands inside the pinned run. Returns where it landed. This is the one
    /// entry for drops and moves, so the invariant survives every path.
    pub fn insert_tab(&mut self, index: usize, tab: Tab) -> usize {
        let run = self.pinned_count();
        let index = if tab.pinned { index.min(run) } else { index.clamp(run, self.tabs.len()) };
        self.tabs.insert(index, tab);
        index
    }

    /// Pin or unpin the tab at `i`, keeping the tile's tabs ordered
    /// pinned-first: pinning moves the tab to the END of the pinned run,
    /// unpinning to the START of the unpinned run. The tile's `active` index
    /// follows the moved tab; other tabs keep their relative order. Returns
    /// the tab's new index — the old one unchanged when the flag already
    /// matches — or `None` for an out-of-range index.
    pub fn set_tab_pinned(&mut self, i: usize, pinned: bool) -> Option<usize> {
        if i >= self.tabs.len() {
            return None;
        }
        if self.tabs[i].pinned == pinned {
            return Some(i);
        }
        let run = self.pinned_count();
        let tab = self.tabs.remove(i);
        let was_active = self.active == i;
        // After the removal the surviving pinned run is `0..run`, where `run`
        // counts the pinned tabs BEFORE the flip (the moved tab is excluded:
        // it sat below the run when pinning, inside it when unpinning), so
        // the insertion point lands exactly on the run's boundary.
        let j = if pinned { run } else { run - 1 };
        self.tabs.insert(j, tab);
        self.tabs[j].pinned = pinned;
        if was_active {
            self.active = j;
        } else {
            // Compensate `active` for the remove-then-insert shifting the
            // tabs between the old and new positions.
            let mut a = self.active;
            if i < a {
                a -= 1;
            }
            if j <= a {
                a += 1;
            }
            self.active = a;
        }
        Some(j)
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
/// A populated section's header renders at the position of its first member in
/// the workspaces Vec. An empty section (no member groups) renders immediately
/// before the group whose `primary_tile` matches its [`anchor`]; sections with
/// a `None` (or dangling) anchor render at the trailing end of the sidebar in
/// `sections` Vec order.
///
/// [`anchor`]: Section::anchor
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: u64,
    pub name: String,
    pub emoji: String,
    pub collapsed: bool,
    /// For an empty section (no member groups), the `primary_tile` of the group
    /// its header renders immediately before; `None` = render at the trailing
    /// end (bottom). Maintained by [`normalize_section_anchors`]; ignored while
    /// the section has members.
    pub anchor: Option<u64>,
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
    /// When true, this group lives in the pinned-bubble strip above the
    /// Sessions card list instead of in the list itself: [`sidebar_rows`]
    /// leaves it out, the way Messages lifts a pinned conversation out of the
    /// scroll. Its section membership survives the pin so unpinning drops it
    /// straight back where it was.
    pub pinned: bool,
    /// When true, this group is tucked into the "Snoozed" section at the
    /// bottom of the sessions list and its row's unread dot is suppressed: the
    /// user has said "not now", so the sidebar must stop asking for attention.
    /// Section membership survives the snooze, exactly like a pin, so
    /// unsnoozing drops it straight back where it was. `pinned` and `snoozed`
    /// are mutually exclusive — toggling one clears the other.
    pub snoozed: bool,
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
            pinned: false,
            snoozed: false,
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
            .map(Tab::title)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.name.clone())
    }

    /// Whether any tab in any tile has unread output.
    pub fn any_unread(&self) -> bool {
        self.root.tiles().iter().any(|t| t.tabs.iter().any(|tab| tab.unread))
    }

    /// Whether the sessions list should paint this group's unread dot. A
    /// snoozed group never does: snoozing is an explicit "hide it for later",
    /// so the row stops advertising unread output until it is woken. The
    /// underlying [`Self::any_unread`] signal survives, so unsnoozing brings
    /// the dot straight back.
    pub fn shows_unread_dot(&self) -> bool {
        self.any_unread() && !self.snoozed
    }

    /// Return the moment this workspace most recently asked for attention.
    ///
    /// When tabs are currently unread, this is the oldest timestamp among
    /// those tabs (ignoring missing timestamps). Otherwise it is the newest
    /// timestamp across all tabs, retained after tabs are read. Returns
    /// `None` when no tab has ever been marked unread.
    pub fn attention_at(&self) -> Option<std::time::SystemTime> {
        let tabs = self.root.tiles().into_iter().flat_map(|tile| tile.tabs.iter());
        let unread = tabs.clone().filter(|tab| tab.unread).filter_map(|tab| tab.unread_at);
        if let Some(oldest) = unread.min() {
            return Some(oldest);
        }
        tabs.filter_map(|tab| tab.unread_at).max()
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

/// How many leading rows of `rows` are pinned groups. [`sidebar_rows_filtered`]
/// lists a folder's pins first, so this is the size of the "Pinned" section
/// the list draws above the rest — zero means no section at all.
pub fn pinned_run(rows: &[SidebarRow], workspaces: &[Workspace]) -> usize {
    rows.iter()
        .take_while(|r| workspaces.get(r.ws_idx).is_some_and(|w| w.pinned))
        .count()
}

/// How many trailing rows of `rows` are snoozed groups. [`sidebar_rows_filtered`]
/// lists them last, under the "Snoozed" caption at the bottom of the list —
/// the mirror of the pinned run at the top. Zero means no snoozed section.
pub fn snoozed_run(rows: &[SidebarRow], workspaces: &[Workspace]) -> usize {
    rows.iter()
        .rev()
        .take_while(|r| workspaces.get(r.ws_idx).is_some_and(|w| w.snoozed))
        .count()
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

/// Logical (pre-scale) dimensions. All three bound the **sessions list**
/// width; the GANTRY region adds the folders column on top
/// ([`sidebar_region_w`]).
pub const SIDEBAR_MIN_W: f32 = 180.0;
/// Default sessions-list width — the GANTRY mock's 260px.
pub const SIDEBAR_DEFAULT_W: f32 = 260.0;
/// Upper bound on the drag. Generous rather than snug, so the default is
/// somewhere to start from rather than already at the ceiling.
pub const SIDEBAR_MAX_W: f32 = 520.0;
/// Top strip of the sidebar: native traffic lights float here and the rest
/// is the window drag handle.
pub const TITLEBAR_H: f32 = 44.0;
/// Height of a one-line Settings sidebar row (iTerm2/native-mac source-list
/// style).
const TAB_H: f32 = 28.0;
/// Height of a two-line session row (GANTRY mock): 10px padding, a 12.5px
/// title line, a 2px gap and an 11px diffstat line, 10px padding, with
/// headroom so descenders never clip. Group rows use this; `tab_rect`'s
/// Settings rows keep [`TAB_H`].
pub const CARD_H: f32 = 52.0;

/// Ceiling on the sidebar's text-size factor.
///
/// The sidebar does not scroll yet — [`crate::sidebar_ui::clipped_row_layer`]
/// clips the row stack to the panel — so every point a row grows is a point of
/// group list that becomes unreachable, and only the first nine groups have a
/// ⌘-number fallback. `appearance.font_size` goes to 40 (≈2.7×), which would
/// leave about five rows visible; capping here keeps the setting useful without
/// letting it hide groups the user has no other way to reach. Lift the cap when
/// the scroll pass lands.
const MAX_ROW_FONT_SCALE: f32 = 1.5;

/// The live app-text-size factor the sidebar scales by, capped.
///
/// The rows are an element tree ([`crate::sidebar_ui`]) whose type scales with
/// the `appearance.font_size` setting, so a fixed row height would clip that
/// type at the larger sizes. This is the module's one read of global state; the
/// height formulas below stay pure so they remain testable at any size.
///
/// `sidebar_ui` reads its type scale from here too, rather than going straight
/// to [`crate::renderer::chrome_font_scale`] — one factor, so the rows and the
/// text inside them can never scale apart.
pub fn row_font_scale() -> f32 {
    cap_row_font_scale(crate::renderer::chrome_font_scale())
}

/// [`MAX_ROW_FONT_SCALE`] applied to `f`. Pure, so the cap itself is pinned by
/// a test rather than only reachable through the live setting.
fn cap_row_font_scale(f: f32) -> f32 {
    f.min(MAX_ROW_FONT_SCALE)
}

/// Height of a sidebar session row at `font_scale`, in logical px. Pure, so
/// the tests can pin the formula at a non-default text size instead of only
/// at the ambient default.
fn sidebar_row_h(font_scale: f32) -> f32 {
    CARD_H * font_scale
}

/// Vertical gap between the sidebar's rounded group rows.
const TAB_GAP: f32 = 3.0;
/// Side of the square header chips (the folders card's hide/new-folder pair
/// and the sessions header's show-folders/focus/plus/gear). The mock draws
/// bare 17px glyphs on an 8px pitch; a 20px hit square on a 4px gap keeps
/// that pitch while giving each glyph a hover well.
const HEADER_CHIP: f32 = 20.0;
/// Gap between neighbouring header chips.
const HEADER_CHIP_GAP: f32 = 4.0;
/// Gap between the native traffic lights and the "Show folders" chip while
/// the folders card is hidden — the mock leaves 16px of air after the green
/// light; with the chip's glyph ~2px inside its square this gives 14px.
const SHOW_FOLDERS_GAP: f32 = 12.0;
/// Height of the horizontal tab strip atop each tile.
const TILE_TAB_H: f32 = 28.0;
/// Gap between tile cards; doubles as the divider drag handle (hit tests
/// inflate it, so a slim gap still drags fine).
const TILE_GAP: f32 = 3.0;
/// Padding between the tile cards and the window edges (top/right/bottom).
/// Matches TILE_GAP so the outer border reads as thin as the inner dividers.
/// Public so the Cleanup overlay can mirror [`terminal_area`]'s insets in
/// gpui layout (edge insets track live window resizes; a computed w/h from
/// the last-painted surface size would lag).
pub const AREA_PAD: f32 = TILE_GAP;
const TILE_TAB_MAX_W: f32 = 180.0;

// ─── GANTRY two-part sidebar region ─────────────────────────────────────
//
// The left region is two panels: a floating "folders" card (pinned CLI
// tools, All sessions, one row per section — the mock's "folders") and,
// beside it, a flat sessions list. `App::sidebar_expanded_w` is the
// *sessions list* width; the region's full width adds the folders column
// on top. These pure helpers are the single authority for that math, so
// painting, hit-testing and `terminal_area`'s `sidebar_w` agree by
// construction.

/// Left inset the region keeps when the folders card is closed.
pub const FOLDERS_CLOSED_INSET: f32 = 16.0;
/// Default width of the folders card (mock spec); the user drags it between
/// [`FOLDERS_MIN_W`] and [`FOLDERS_MAX_W`] on the band beside the card.
pub const FOLDERS_CARD_W: f32 = 200.0;
pub const FOLDERS_MIN_W: f32 = 150.0;
pub const FOLDERS_MAX_W: f32 = 360.0;
/// Region content inset: top, bottom and left. The list's right inset is
/// [`REGION_RIGHT_PAD`]; the card→list gutter is [`REGION_GAP`].
pub const REGION_PAD: f32 = 10.0;
/// Right inset of the region's content — the resize grab band's home.
pub const REGION_RIGHT_PAD: f32 = 6.0;
/// Gap between the folders card and the sessions list (mock spec).
pub const REGION_GAP: f32 = 10.0;
/// Height of the sessions list's header block (title, "N sessions" and the
/// chips) — one [`TITLEBAR_H`] strip, so the native traffic lights, which
/// float at [`TRAFFIC_LIGHT_ORIGIN`], sit centred in it while the folders
/// card is hidden.
pub const SESSIONS_HEADER_H: f32 = TITLEBAR_H;
/// Height of the folders card's header strip: the same [`TITLEBAR_H`] band
/// for the same traffic-light reason (they land in the card while it is
/// open — the card's 10px inset keeps [`TRAFFIC_LIGHT_ORIGIN`] inside it).
pub const FOLDERS_HEADER_H: f32 = TITLEBAR_H;
/// Height of the folders card's footer line ("N sessions").
pub const FOLDERS_FOOTER_H: f32 = 34.0;
/// Horizontal inset of the folder rows from the card edge (mock: body
/// padding `0 8`).
pub const FOLDER_BODY_PAD: f32 = 8.0;
/// Height of a folder / "All sessions" / "Pinned tools" row (mock: 7px of
/// padding around a 12.5px line).
pub const FOLDER_ROW_H: f32 = 30.0;
/// Height of a pinned CLI-tool row (mock: 6px around an 11.5px mono line).
pub const TOOL_ROW_H: f32 = 26.0;
/// Vertical gap between the folders card's rows.
pub const FOLDER_ROW_GAP: f32 = 1.0;
/// Space the separator between the pinned tools and the folders takes: a
/// 1px line with 6px margins above and below.
pub const FOLDER_SEPARATOR_H: f32 = 13.0;
/// Inset of that separator (and the footer text) from the card's edges.
const FOLDER_SEPARATOR_INSET: f32 = 12.0;

/// Width of the whole left region in logical px: the sessions list plus
/// the folders column when it is open, or the slim closed inset. A zero
/// sessions width stays zero — that is the collapsed state's "region
/// hidden" signal, which `terminal_area` keys off.
pub fn sidebar_region_w(sessions_w: f32, folders_w: f32, folders_open: bool) -> f32 {
    if sessions_w <= 0.0 {
        return 0.0;
    }
    sessions_w + if folders_open { folders_col_w(folders_w) } else { FOLDERS_CLOSED_INSET }
}

/// The folders column's share of the region while the card shows: the
/// card and the padding either side of it (plus the list's right pad, so
/// the sessions width is exactly the list's width).
fn folders_col_w(folders_w: f32) -> f32 {
    REGION_PAD + folders_w + REGION_GAP + REGION_RIGHT_PAD
}

/// The folders-card width a resize drag should store when the pointer sits
/// at logical `x` on the band between the card and the list: the band's
/// centre tracks the pointer, then the clamp applies.
pub fn folders_w_for_pointer(x: f32) -> f32 {
    (x - REGION_PAD - REGION_GAP / 2.0).clamp(FOLDERS_MIN_W, FOLDERS_MAX_W)
}

/// How far past the region's narrowest width the sidebar resize drag has
/// to travel before the region folds away (macOS-style fluid collapse), and
/// the point it must come back past to reopen.
pub const SIDEBAR_COLLAPSE_SLACK: f32 = 60.0;

/// Whether a sidebar resize drag with the pointer at logical `x` should
/// leave the region collapsed: the pointer is well inside the narrowest
/// region the folders column and the minimum list width allow.
pub fn sidebar_drag_collapses(x: f32, folders_w: f32, folders_open: bool) -> bool {
    x < sidebar_region_w(SIDEBAR_MIN_W, folders_w, folders_open) - SIDEBAR_COLLAPSE_SLACK
}

/// The folders card's twin of [`SIDEBAR_COLLAPSE_SLACK`]: how far inside
/// the card's minimum width its resize drag folds the card away.
pub const FOLDERS_COLLAPSE_SLACK: f32 = 50.0;

/// Whether a folders-card resize drag with the pointer at logical `x`
/// should leave the card hidden (and reopen it once dragged back out).
pub fn folders_drag_collapses(x: f32) -> bool {
    x < REGION_PAD + REGION_GAP / 2.0 + FOLDERS_MIN_W - FOLDERS_COLLAPSE_SLACK
}

/// Logical x of the folders-card resize band's centre (the middle of the
/// gap between the card and the list), or `None` while the card is hidden.
pub fn folders_edge_x(folders_w: f32, folders_open: bool) -> Option<f32> {
    folders_open.then(|| REGION_PAD + folders_w + REGION_GAP / 2.0)
}

/// The sessions-list width a resize drag should store when the pointer sits
/// at logical `x` on the region's right-edge grab band: the folders column
/// (or the slim closed inset) comes back off the region width, then the
/// drag clamp applies. Only meaningful while the region is shown — the
/// band is not armed while it is collapsed.
pub fn sessions_w_for_pointer(x: f32, folders_w: f32, folders_open: bool) -> f32 {
    let folders_col = if folders_open { folders_col_w(folders_w) } else { FOLDERS_CLOSED_INSET };
    (x - folders_col).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W)
}

/// Rect of the floating folders card, in physical px: full region height,
/// [`FOLDERS_CARD_W`] wide, inset by [`REGION_PAD`]. Pure — painting and
/// hit-testing both read this one rect.
pub fn folders_card_rect(height: u32, folders_w: f32, scale: f32) -> LayoutRect {
    let pad = (REGION_PAD * scale).round();
    LayoutRect {
        x: pad,
        y: pad,
        w: (folders_w * scale).round(),
        h: (height as f32 - 2.0 * pad).max(0.0),
    }
}

/// Rect of the flat sessions list in physical px: right of the folders
/// column, inset by the region padding top and bottom. Its first
/// [`SESSIONS_HEADER_H`] is the list header; rows start below that (see
/// [`sidebar_row_rect`]).
pub fn sessions_list_rect(
    sessions_w: f32,
    folders_w: f32,
    folders_open: bool,
    height: u32,
    scale: f32,
) -> LayoutRect {
    let region = sidebar_region_w(sessions_w, folders_w, folders_open) * scale;
    let pad = (REGION_PAD * scale).round();
    let gap = (REGION_GAP * scale).round();
    let x = if folders_open {
        pad + (folders_w * scale).round() + gap
    } else {
        (FOLDERS_CLOSED_INSET * scale).round()
    };
    let right = (region - (REGION_RIGHT_PAD * scale).round()).max(x);
    let bottom = (height as f32 - pad).max(pad);
    LayoutRect { x, y: pad, w: right - x, h: bottom - pad }
}

/// One row of the folders card, top to bottom: the "Pinned tools" caption
/// and one row per registered CLI tool (only when there are tools), then
/// "All sessions" and one row per section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderRow {
    PinnedHeader,
    Tool(usize),
    AllSessions,
    Section(usize),
}

/// The folders card's row list for `n_tools` CLI tools and `n_sections`
/// sections, in paint order.
pub fn folder_rows(n_tools: usize, tools_collapsed: bool, n_sections: usize) -> Vec<FolderRow> {
    let mut rows = Vec::with_capacity(n_tools + n_sections + 2);
    if n_tools > 0 {
        rows.push(FolderRow::PinnedHeader);
        if !tools_collapsed {
            rows.extend((0..n_tools).map(FolderRow::Tool));
        }
    }
    rows.push(FolderRow::AllSessions);
    rows.extend((0..n_sections).map(FolderRow::Section));
    rows
}

fn folder_row_h(row: FolderRow) -> f32 {
    match row {
        FolderRow::Tool(_) => TOOL_ROW_H,
        _ => FOLDER_ROW_H,
    }
}

/// Rect of `rows[index]` inside `card` (physical px): rows stack below the
/// card header, inset by [`FOLDER_BODY_PAD`], with the separator's space
/// opening above "All sessions" whenever tool rows precede it. Painting,
/// hovering and group-drop hit-testing all read this one rect.
pub fn folder_row_rect(
    card: &LayoutRect,
    rows: &[FolderRow],
    index: usize,
    scroll: f32,
    scale: f32,
) -> LayoutRect {
    let pad = (FOLDER_BODY_PAD * scale).round();
    let gap = (FOLDER_ROW_GAP * scale).round();
    let x = card.x + pad;
    let w = (card.w - 2.0 * pad).max(0.0);
    let mut y = card.y + (FOLDERS_HEADER_H * scale).round() - scroll.round();
    for (i, row) in rows.iter().enumerate() {
        if matches!(row, FolderRow::AllSessions) && i > 0 {
            y += (FOLDER_SEPARATOR_H * scale).round();
        }
        let h = (folder_row_h(*row) * scale).round();
        if i == index {
            return LayoutRect { x, y, w, h };
        }
        y += h + gap;
    }
    LayoutRect { x, y, w, h: 0.0 }
}

/// The 1px separator between the tool rows and "All sessions", centred in
/// the [`FOLDER_SEPARATOR_H`] gap; `None` when no tool rows precede it.
pub fn folder_separator_rect(
    card: &LayoutRect,
    rows: &[FolderRow],
    scroll: f32,
    scale: f32,
) -> Option<LayoutRect> {
    let i = rows.iter().position(|r| matches!(r, FolderRow::AllSessions))?;
    if i == 0 {
        return None;
    }
    let all = folder_row_rect(card, rows, i, scroll, scale);
    let inset = (FOLDER_SEPARATOR_INSET * scale).round();
    let line = scale.round().max(1.0);
    Some(LayoutRect {
        x: card.x + inset,
        y: all.y - ((FOLDER_SEPARATOR_H * scale) / 2.0).round(),
        w: (card.w - 2.0 * inset).max(0.0),
        h: line,
    })
}

/// Height of the folders card's row stack (unscrolled, device px): from the
/// header's bottom to the last row's bottom. With the footer band this is
/// what bounds the card's scroll ([`max_scroll`]).
pub fn folder_rows_extent(card: &LayoutRect, rows: &[FolderRow], scale: f32) -> f32 {
    match rows.len().checked_sub(1) {
        Some(last) => {
            let r = folder_row_rect(card, rows, last, 0.0, scale);
            r.y + r.h - (card.y + (FOLDERS_HEADER_H * scale).round())
        },
        None => 0.0,
    }
}

/// The largest scroll offset a stack `content_h` tall may take inside a
/// `viewport_h` window: never negative, so a short list stays put.
pub fn max_scroll(content_h: f32, viewport_h: f32) -> f32 {
    (content_h - viewport_h).max(0.0)
}

/// The folders card's footer band (the "N sessions" line), pinned to the
/// card's bottom edge; rows are clipped above it.
pub fn folders_footer_rect(card: &LayoutRect, scale: f32) -> LayoutRect {
    let h = (FOLDERS_FOOTER_H * scale).round().min(card.h);
    LayoutRect { x: card.x, y: card.y + card.h - h, w: card.w, h }
}

/// The folders card's header chips, right-aligned in its header strip:
/// `(hide_folders, new_folder)`.
pub fn folders_header_chips(card: &LayoutRect, scale: f32) -> (LayoutRect, LayoutRect) {
    let side = (HEADER_CHIP * scale).round();
    let gap = (HEADER_CHIP_GAP * scale).round();
    let y = card.y + (((FOLDERS_HEADER_H - HEADER_CHIP) / 2.0) * scale).round();
    let right = card.x + card.w - (REGION_PAD * scale).round();
    let new = LayoutRect { x: right - side, y, w: side, h: side };
    let hide = LayoutRect { x: new.x - gap - side, y, w: side, h: side };
    (hide, new)
}

/// The chips in the sessions list header, all [`HEADER_CHIP`] squares
/// centred in [`SESSIONS_HEADER_H`]: `show_folders` at the left (only while
/// the folders card is hidden — it sits past the traffic-light safe span the
/// header then has to reserve), and `focus` (hide the whole region), `plus`
/// (new session) and `gear` (Settings) clustered at the right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SessionsHeaderChips {
    pub show_folders: Option<LayoutRect>,
    pub focus: LayoutRect,
    pub plus: LayoutRect,
    pub gear: LayoutRect,
}

/// Right inset of the header chip cluster from the list's edge.
const HEADER_CHIP_INSET: f32 = 2.0;

pub fn sessions_header_chips(list: &LayoutRect, folders_open: bool, scale: f32) -> SessionsHeaderChips {
    let side = (HEADER_CHIP * scale).round().min(list.w.max(0.0));
    let gap = (HEADER_CHIP_GAP * scale).round();
    let y = list.y + (((SESSIONS_HEADER_H - HEADER_CHIP) / 2.0) * scale).round();
    let right = list.x + list.w - (HEADER_CHIP_INSET * scale).round();
    let gear = LayoutRect { x: right - side, y, w: side, h: side };
    let plus = LayoutRect { x: gear.x - gap - side, y, w: side, h: side };
    let focus = LayoutRect { x: plus.x - gap - side, y, w: side, h: side };
    let show_folders = (!folders_open).then(|| LayoutRect {
        x: ((TRAFFIC_LIGHT_END + SHOW_FOLDERS_GAP) * scale).round().max(list.x),
        y,
        w: side,
        h: side,
    });
    SessionsHeaderChips { show_folders, focus, plus, gear }
}


/// Flat sessions rows for the GANTRY list: one [`SidebarRow`] per group
/// — no section headers — for the groups `filter` admits. `None` (All
/// sessions) admits every group in `workspaces` order, regardless of any
/// section's `collapsed` flag (the list has no headers to fold under);
/// `Some` admits only that section's members, in the same order. An unknown
/// or dangling section id filters to nothing, which the list surfaces as its
/// empty state. The admitted pinned groups come first, in their own order:
/// they form the "Pinned" section at the top of the list ([`pinned_run`]) —
/// folded away entirely while `pinned_collapsed` — and the snoozed ones
/// last, under the bottom caption ([`snoozed_run`]) — likewise folded away
/// while `snoozed_collapsed`.
pub fn sidebar_rows_filtered(
    workspaces: &[Workspace],
    sections: &[Section],
    filter: Option<u64>,
    pinned_collapsed: bool,
    snoozed_collapsed: bool,
) -> Vec<SidebarRow> {
    let admit = |ws: &Workspace| match filter {
        None => true,
        Some(id) => ws.section == Some(id),
    };
    if let Some(id) = filter
        && !sections.iter().any(|s| s.id == id)
    {
        // An id no section owns filters to nothing.
        return Vec::new();
    }
    let admitted = || workspaces.iter().enumerate().filter(|(_, ws)| admit(ws));
    admitted()
        .filter(|(_, ws)| ws.pinned && !pinned_collapsed)
        // Pinned groups head the list (the "Pinned" section), snoozed ones
        // trail it (the "Snoozed" section). The two flags are mutually
        // exclusive; pinned wins the sort if hand-edited data sets both.
        .chain(admitted().filter(|(_, ws)| !ws.pinned && !ws.snoozed))
        .chain(admitted().filter(|(_, ws)| ws.snoozed && !ws.pinned && !snoozed_collapsed))
        .map(|(ws_idx, _)| SidebarRow { ws_idx })
        .collect()
}

/// The whole left region in physical px. `sidebar_w` is the region width in
/// logical px ([`sidebar_region_w`]); 0 means collapsed.
pub fn sidebar(height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    LayoutRect { x: 0.0, y: 0.0, w: (sidebar_w * scale).round(), h: height as f32 }
}

/// The window-drag strip across the top of the region: the folders card's
/// header and the sessions list's header share this one [`TITLEBAR_H`]
/// band, and the native traffic lights float in it.
pub fn titlebar(scale: f32, sidebar_w: f32) -> LayoutRect {
    LayoutRect { x: 0.0, y: 0.0, w: (sidebar_w * scale).round(), h: (TITLEBAR_H * scale).round() }
}

/// Where the native traffic lights sit (top-left of the close button, logical
/// px) while the sidebar is open: 12px inside the sidebar's 8px window
/// gutter, which also centers the 12px buttons in the panel's header strip —
/// Messages puts them there rather than at macOS's default (12, 12), which
/// now lands on the gutter.
pub const TRAFFIC_LIGHT_ORIGIN: f32 = 20.0;

/// Which surface owns the window's top-left corner, and so where the native
/// traffic lights float.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrafficLightSpot {
    /// The open sidebar's header strip.
    Sidebar,
    /// The top-left tile's tab strip once the sidebar collapses.
    CollapsedTile,
    /// A maximized flyover panel's tab strip, which fills the window from
    /// (0, 0) and cedes its left end to the lights (`flyover_tab_rect`).
    MaximizedFlyover,
}

/// The spot for the current chrome state: a maximized flyover covers
/// everything else, so it wins over the sidebar state.
pub fn traffic_light_spot(sidebar_collapsed: bool, flyover_maximized: bool) -> TrafficLightSpot {
    if flyover_maximized {
        TrafficLightSpot::MaximizedFlyover
    } else if sidebar_collapsed {
        TrafficLightSpot::CollapsedTile
    } else {
        TrafficLightSpot::Sidebar
    }
}

/// The traffic lights' origin for `spot`. Over a tab strip the lights sit
/// centered on that 28px strip — at the tile gap for the collapsed sidebar
/// (essentially macOS's default spot), flush with the window top for the
/// maximized flyover whose strip starts at y = 0.
pub fn traffic_light_origin(spot: TrafficLightSpot) -> (f32, f32) {
    match spot {
        TrafficLightSpot::Sidebar => (TRAFFIC_LIGHT_ORIGIN, TRAFFIC_LIGHT_ORIGIN),
        TrafficLightSpot::CollapsedTile => (AREA_PAD + 9.0, AREA_PAD + (TILE_TAB_H - 12.0) / 2.0),
        TrafficLightSpot::MaximizedFlyover => (AREA_PAD + 9.0, (TILE_TAB_H - 12.0) / 2.0),
    }
}

/// Logical width of the top-left corner the native traffic lights occupy.
/// The buttons themselves end at [`TRAFFIC_LIGHT_END`]; the extra headroom keeps the first tab
/// from crowding them.
pub const TRAFFIC_LIGHT_SAFE_W: f32 = 90.0;
/// Where the third traffic light ends. macOS 26 draws the lights 14px wide
/// on a 23px pitch from [`TRAFFIC_LIGHT_ORIGIN`] (measured off a window
/// capture: 20–34, 43–57, 66–80), not the nominal 12px/8px — the
/// sessions-list header's "Show folders" chip hugs this rather than the
/// roomier [`TRAFFIC_LIGHT_SAFE_W`], so the title beside it keeps enough
/// room to read at the default list width.
pub const TRAFFIC_LIGHT_END: f32 = TRAFFIC_LIGHT_ORIGIN + 2.0 * 23.0 + 14.0;

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

/// Side of the "Show sessions" button that floats beside the traffic
/// lights while the whole region is hidden.
pub const SHOW_SESSIONS_BTN: f32 = 26.0;
/// Gap between the last traffic light and that button's box; the glyph
/// inside is inset another 4.5px, so the visible gap matches the one the
/// tab strip keeps on the button's other side.
pub const SHOW_SESSIONS_GAP: f32 = 4.0;

/// Rect of that button (physical px): just past the traffic-light safe
/// span, vertically centred on the top-left tile's tab strip, which
/// [`COLLAPSED_STRIP_INSET`] pushes clear of it.
pub fn show_sessions_button(scale: f32) -> LayoutRect {
    let side = (SHOW_SESSIONS_BTN * scale).round();
    LayoutRect {
        x: ((TRAFFIC_LIGHT_END + SHOW_SESSIONS_GAP) * scale).round(),
        y: ((AREA_PAD + (TILE_TAB_H - SHOW_SESSIONS_BTN) / 2.0) * scale).round(),
        w: side,
        h: side,
    }
}

/// Left inset the top-left tile's tab strip cedes while the sidebar is
/// collapsed: the traffic lights, the floating "Show sessions" button
/// beside them (`sidebar_ui::render_collapsed_overlay`) and a gap that
/// reads the same as the one before the button.
pub const COLLAPSED_STRIP_INSET: f32 =
    TRAFFIC_LIGHT_END + SHOW_SESSIONS_GAP + SHOW_SESSIONS_BTN + SHOW_SESSIONS_GAP;

/// A tile's rect adjusted for tab-strip geometry: while the sidebar is
/// collapsed (`sidebar_w == 0.0`), the tile owning the area's top-left
/// corner cedes its strip's left end to the native traffic lights, pushing
/// its tabs right. The inset also clears the floating
/// "Show sessions" button that rides beside the lights. Every strip consumer (painting, hit-testing, drops) must
/// feed this to the `tile_tab_*` functions so they never disagree; the card
/// and content keep the original rect.
pub fn tab_strip_rect(area: LayoutRect, rect: &LayoutRect, scale: f32, sidebar_w: f32) -> LayoutRect {
    if sidebar_w != 0.0 || rect.x > area.x || rect.y > area.y {
        return *rect;
    }
    let inset = ((COLLAPSED_STRIP_INSET * scale).round() - rect.x).clamp(0.0, rect.w);
    LayoutRect { x: rect.x + inset, w: rect.w - inset, ..*rect }
}

/// Centered CTA used by the empty-state launch view.
pub fn empty_state_cta(width: u32, height: u32, scale: f32, sidebar_w: f32) -> LayoutRect {
    // The CTA centers in the un-insetted area: the empty state has nothing
    // the pill bar could hide.
    let area = terminal_area(width, height, scale, sidebar_w, 0.0);
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

/// One-line row `index` of the Settings sidebar, stacked below the list
/// header inside `list` (the [`sessions_list_rect`]). Group rows use
/// [`sidebar_row_rect`] instead — this flat-index helper is for the pages
/// whose sidebar is a uniform stack.
pub fn tab_rect(index: usize, scale: f32, list: &LayoutRect) -> LayoutRect {
    tab_rect_at(index, scale, row_font_scale(), list)
}

/// [`tab_rect`] at an explicit text-size factor. Pure, so the tests can pin the
/// wiring at a non-default size instead of only at the ambient default.
fn tab_rect_at(index: usize, scale: f32, font_scale: f32, list: &LayoutRect) -> LayoutRect {
    let h = (TAB_H * font_scale * scale).round();
    let gap = (TAB_GAP * scale).round();
    let top = list.y + (SESSIONS_HEADER_H * scale).round() + 2.0 * gap;
    LayoutRect { x: list.x, y: top + index as f32 * (h + gap), w: list.w.max(0.0), h }
}

/// The search-box slot at the top of the Settings sidebar (index 0 slot).
pub fn settings_search_rect(scale: f32, list: &LayoutRect) -> LayoutRect {
    tab_rect(0, scale, list)
}

/// One visible row of the flat sessions list: the group it shows. Section
/// headers left the list for the folders card, so a row is always a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    pub ws_idx: usize,
}

/// Height of the "Pinned" caption above the pinned rows (logical px).
pub const PINNED_CAPTION_H: f32 = 22.0;
/// Band between the last pinned row and the first unpinned one, with the
/// section divider centred in it.
pub const PINNED_SECTION_GAP: f32 = 12.0;

/// Device-px rect of the "Pinned" caption: the band just under the list
/// header that the pinned rows hang from. Always present — it is the
/// fold toggle and the drop zone that pins a dragged row — so the rows
/// (pinned or not) start under it. `list` is the *scrolled* list rect.
pub fn pinned_caption_rect(scale: f32, list: &LayoutRect) -> LayoutRect {
    LayoutRect {
        x: list.x,
        y: list.y + (SESSIONS_HEADER_H * scale).round(),
        w: list.w.max(0.0),
        h: (PINNED_CAPTION_H * scale).round(),
    }
}

/// State of the bottom "Snoozed" section: how many admitted groups are
/// snoozed (folded or not) and whether the run is folded away.
///
/// The caption band outlives the run — it is the click target that folds it
/// and brings it back, exactly as [`pinned_caption_rect`] heads the pinned
/// run — so the geometry cannot read the count off `rows` the way
/// [`snoozed_run`] does: a folded run contributes no rows at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnoozedSection {
    /// Snoozed groups the current folder filter admits. Zero hides the band.
    pub total: usize,
    /// The run is folded (`sidebar.snoozed_collapsed`): the caption stays,
    /// the rows leave the list.
    pub collapsed: bool,
}

/// Device-px rect of the "Snoozed" caption — the label heading the run of
/// snoozed groups at the bottom of the list, and the toggle that folds it.
/// Zero-height while `snoozed.total` is 0. The section gap sits *above* it,
/// so the caption reads as the closing band of the ordinary run. `list` is
/// the *scrolled* list rect.
pub fn snoozed_caption_rect(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    pinned_section: bool,
    snoozed: SnoozedSection,
    scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    if snoozed.total == 0 {
        return LayoutRect { x: list.x, y: list.y, w: 0.0, h: 0.0 };
    }
    // Rows the list actually lays out: zero while the run is folded.
    let n_snoozed = snoozed_run(rows, workspaces);
    let n_pinned = pinned_run(rows, workspaces);
    let n_ordinary = rows.len() - n_snoozed;
    let h = (sidebar_row_h(row_font_scale()) * scale).round();
    let mut y = list.y + (SESSIONS_HEADER_H * scale).round();
    if pinned_section {
        y += (PINNED_CAPTION_H * scale).round();
    }
    y += n_ordinary as f32 * h;
    // Same boundary rule [`sidebar_row_rect_at`] uses: the pinned gap is
    // added at the first unpinned row, which lies inside the ordinary run.
    if pinned_section && n_ordinary >= n_pinned {
        y += (PINNED_SECTION_GAP * scale).round();
    }
    y += (PINNED_SECTION_GAP * scale).round();
    LayoutRect { x: list.x, y, w: list.w.max(0.0), h: (PINNED_CAPTION_H * scale).round() }
}

/// The hairline closing the ordinary run: centred in the section gap above
/// the "Snoozed" caption, mirroring [`pinned_divider_rect`] at the top of
/// the list. Zero-height while nothing is snoozed. Device px; `list` is
/// scrolled.
pub fn snoozed_divider_rect(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    pinned_section: bool,
    snoozed: SnoozedSection,
    scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    let cap = snoozed_caption_rect(rows, workspaces, pinned_section, snoozed, scale, list);
    if cap.h == 0.0 {
        return LayoutRect { x: list.x, y: list.y, w: 0.0, h: 0.0 };
    }
    let inset = (12.0 * scale).round();
    LayoutRect {
        x: list.x + inset,
        y: cap.y - ((PINNED_SECTION_GAP * scale).round() / 2.0).round(),
        w: (list.w - 2.0 * inset).max(0.0),
        h: scale.round().max(1.0),
    }
}

/// The hairline closing the "Pinned" section: centred in the gap under
/// the last pinned row, or straight under the caption when nothing is
/// pinned (or the section is folded). Device px; `list` is scrolled.
pub fn pinned_divider_rect(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    pinned_section: bool,
    scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    if !pinned_section {
        return LayoutRect { x: list.x, y: list.y, w: 0.0, h: 0.0 };
    }
    let cap = pinned_caption_rect(scale, list);
    let n_pinned = pinned_run(rows, workspaces);
    let h = (sidebar_row_h(row_font_scale()) * scale).round();
    let gap = (PINNED_SECTION_GAP * scale).round();
    let inset = (12.0 * scale).round();
    LayoutRect {
        x: list.x + inset,
        y: cap.y + cap.h + n_pinned as f32 * h + (gap / 2.0).round(),
        w: (list.w - 2.0 * inset).max(0.0),
        h: scale.round().max(1.0),
    }
}

/// The band a dragged row can be dropped on to pin it: the caption plus,
/// when nothing is pinned, the empty gap under it.
pub fn pinned_drop_zone(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    pinned_section: bool,
    scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    if !pinned_section {
        return LayoutRect { x: list.x, y: list.y, w: 0.0, h: 0.0 };
    }
    let cap = pinned_caption_rect(scale, list);
    let extra = if pinned_run(rows, workspaces) == 0 {
        (PINNED_SECTION_GAP * scale).round()
    } else {
        0.0
    };
    LayoutRect { h: cap.h + extra, ..cap }
}

/// Height of the sessions list's row stack (unscrolled, device px): from
/// the header's bottom to the last row's bottom, caption and section gap
/// included. Bounds the list's scroll ([`max_scroll`]).
pub fn sidebar_rows_extent(
    rows: &[SidebarRow],
    workspaces: &[Workspace],
    pinned_section: bool,
    snoozed: SnoozedSection,
    scale: f32,
    list: &LayoutRect,
) -> f32 {
    let top = list.y + (SESSIONS_HEADER_H * scale).round();
    match rows.len().checked_sub(1) {
        Some(last) => {
            let r = sidebar_row_rect(rows, last, workspaces, pinned_section, scale, list);
            // A folded snoozed run leaves its caption below the last row.
            let cap = snoozed_caption_rect(rows, workspaces, pinned_section, snoozed, scale, list);
            (r.y + r.h).max(cap.y + cap.h) - top
        },
        None if pinned_section => {
            let d = pinned_divider_rect(rows, workspaces, true, scale, list);
            let cap = snoozed_caption_rect(rows, workspaces, true, snoozed, scale, list);
            (d.y + (PINNED_SECTION_GAP * scale).round() / 2.0).max(cap.y + cap.h) - top
        },
        None => {
            let cap = snoozed_caption_rect(rows, workspaces, false, snoozed, scale, list);
            (cap.y + cap.h - top).max(0.0)
        },
    }
}

/// Pixel rect for `rows[index]` inside `list` (the [`sessions_list_rect`]):
/// rows stack below the list header and the "Pinned" caption, the pinned
/// run then the section gap, span the
/// list's full width and touch (the row paints its own hairline separator).
/// Painting, hit-testing, and drop resolution must all use this so they
/// never disagree.
pub fn sidebar_row_rect(
    rows: &[SidebarRow],
    index: usize,
    workspaces: &[Workspace],
    pinned_section: bool,
    scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    sidebar_row_rect_at(rows, index, workspaces, pinned_section, scale, row_font_scale(), list)
}

/// [`sidebar_row_rect`] at an explicit text-size factor. Pure, so the tests can
/// pin the row pitch at a non-default size.
fn sidebar_row_rect_at(
    rows: &[SidebarRow],
    index: usize,
    workspaces: &[Workspace],
    pinned_section: bool,
    scale: f32,
    font_scale: f32,
    list: &LayoutRect,
) -> LayoutRect {
    let n_pinned = pinned_run(rows, workspaces);
    let n_snoozed = snoozed_run(rows, workspaces);
    let mut y = list.y + (SESSIONS_HEADER_H * scale).round();
    if pinned_section {
        y += (PINNED_CAPTION_H * scale).round();
    }
    let w = list.w.max(0.0);
    let h = (sidebar_row_h(font_scale) * scale).round();
    for i in 0..=rows.len() {
        // The section gap (and its divider) sits before the first unpinned
        // row — right under the caption when nothing is pinned. Without
        // the section (nothing pinned, no drag) rows sit straight under
        // the header.
        if i == n_pinned && pinned_section {
            y += (PINNED_SECTION_GAP * scale).round();
        }
        // The "Snoozed" band sits between the last ordinary row and the
        // snoozed run at the bottom of the list: a gap, the caption, then
        // the rows.
        if n_snoozed > 0 && i == rows.len() - n_snoozed {
            y += (PINNED_SECTION_GAP * scale).round() + (PINNED_CAPTION_H * scale).round();
        }
        if i == index {
            return LayoutRect { x: list.x, y, w, h };
        }
        y += h;
    }
    // Out-of-range fallback: empty rect at the stack end.
    LayoutRect { x: list.x, y, w, h: 0.0 }
}

/// The delete-section button hit region at the right edge of a section-header
/// row (`header` = its [`sidebar_row_rect`]). Painting and hit-testing both
/// derive it from the header rect so they never disagree; mirrors
/// [`tile_tab_close_rect`]'s sizing.
pub fn section_delete_rect(header: &LayoutRect, scale: f32) -> LayoutRect {
    let s = (16.0 * scale).round();
    let pad = (6.0 * scale).round();
    LayoutRect {
        x: header.x + header.w - s - pad,
        y: (header.y + (header.h - s) / 2.0).round(),
        w: s,
        h: s,
    }
}

/// Row index that shows the active pill for the `active` workspace, if it
/// has a row in `rows` (pinned groups and groups hidden by the folder
/// filter have none).
pub fn active_row_index(rows: &[SidebarRow], active: usize) -> Option<usize> {
    rows.iter().position(|r| r.ws_idx == active)
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

/// Refresh section anchors so an emptied section keeps its place. For each
/// section with members, set `anchor` to the `primary_tile` of the group
/// immediately after its member block (`None` if the block ends the list),
/// so the anchor is already correct the moment the section empties. For an
/// empty section, leave `anchor` as-is but (1) clear it to `None` if it points
/// at a `primary_tile` no longer present in `workspaces` (dangling), and
/// (2) if it points at an interior (non-first) member of another section,
/// re-point it to that section's first member — `sidebar_rows` only emits
/// anchored headers at run boundaries, so an interior anchor would otherwise
/// silently drop the empty section to the bottom.
pub fn normalize_section_anchors(workspaces: &[Workspace], sections: &mut [Section]) {
    for section in sections.iter_mut() {
        match section_member_range(workspaces, section.id) {
            Some((_start, end)) => {
                section.anchor = workspaces.get(end).map(|w| w.primary_tile);
            }
            None => {
                if let Some(t) = section.anchor {
                    match workspaces.iter().position(|w| w.primary_tile == t) {
                        None => section.anchor = None,
                        Some(pos) => {
                            if let Some(sid) = workspaces[pos].section
                                && let Some((start, _)) =
                                    section_member_range(workspaces, sid)
                            {
                                section.anchor = Some(workspaces[start].primary_tile);
                            }
                        }
                    }
                }
            }
        }
    }
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

/// Move `sections[from]` so it sits before the pre-removal index
/// `insert_before` (len = last); the folders card's drag-to-reorder. Only
/// the card's order changes — membership is by id. Returns the final index.
pub fn reorder_section(sections: &mut Vec<Section>, from: usize, mut insert_before: usize) -> usize {
    if from >= sections.len() {
        return from;
    }
    let section = sections.remove(from);
    if insert_before > from {
        insert_before -= 1;
    }
    insert_before = insert_before.min(sections.len());
    sections.insert(insert_before, section);
    insert_before
}

/// Delete `section_id`: ungroup its member groups — they survive as top-level
/// groups, keeping their order and position — and remove the section entry.
/// Returns true if the section existed and was removed. Groups are never
/// closed: a section is only a sidebar grouping, not an owner of its groups.
pub fn delete_section(
    sections: &mut Vec<Section>,
    workspaces: &mut [Workspace],
    section_id: u64,
) -> bool {
    for w in workspaces.iter_mut() {
        if w.section == Some(section_id) {
            w.section = None;
        }
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



/// The region right of the sidebar where the split tree lives. Inset from the
/// window's top/right/bottom edges so the tile cards float on the gradient.
///
/// `sidebar_w == 0.0` means the sidebar is collapsed (the resize clamp keeps
/// a visible sidebar at [`SIDEBAR_MIN_W`] or wider). Collapsed, the area
/// keeps the full window height and just gains the thin left inset; the
/// native traffic lights instead carve into the top-left tile's tab strip
/// via [`tab_strip_rect`].
pub fn terminal_area(
    width: u32,
    height: u32,
    scale: f32,
    sidebar_w: f32,
    bottom_inset: f32,
) -> LayoutRect {
    let sb = (sidebar_w * scale).round();
    let pad = (AREA_PAD * scale).round();
    let x = if sidebar_w == 0.0 { pad } else { sb };
    let right_edge = (width as f32 - pad).max(x);
    // `bottom_inset` (logical px) reserves a safe area above the window's
    // bottom edge — the Flow pill bar floats there, and terminal rows must
    // reflow above it rather than hide beneath it.
    LayoutRect {
        x,
        y: pad,
        w: (right_edge - x).max(0.0),
        h: (height as f32 - 2.0 * pad - (bottom_inset * scale).round()).max(0.0),
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
    /// The band between the folders card and the sessions list.
    Folders,
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
    folders_edge_x: Option<f32>,
    grab: f32,
    dividers_active: bool,
    px: f32,
    py: f32,
) -> Option<ResizeHover> {
    // `sidebar_edge_x == 0.0` means collapsed: there is no edge to grab.
    if sidebar_edge_x > 0.0 && (px - sidebar_edge_x).abs() <= grab {
        return Some(ResizeHover::Sidebar);
    }
    if sidebar_edge_x > 0.0
        && let Some(fx) = folders_edge_x
        && (px - fx).abs() <= grab
    {
        return Some(ResizeHover::Folders);
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

#[cfg(test)]
mod area_tests {
    use super::*;

    /// The right edge no longer reserves a tool-panel strip: the terminal area
    /// always reaches the window edge minus the normal `AREA_PAD`.
    #[test]
    fn area_runs_to_the_right_edge_minus_pad() {
        let (w, h, scale) = (1600, 1000, 2.0);
        let area = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, 0.0);
        assert_eq!(area.x + area.w, w as f32 - AREA_PAD * scale);
        assert_eq!(area.y + area.h, h as f32 - AREA_PAD * scale);
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

    /// The lights move onto a maximized flyover's strip, centered on it at
    /// the window top, and that spot beats the sidebar state.
    #[test]
    fn traffic_lights_follow_the_maximized_flyover() {
        use TrafficLightSpot::*;
        assert_eq!(traffic_light_spot(false, false), Sidebar);
        assert_eq!(traffic_light_spot(true, false), CollapsedTile);
        assert_eq!(traffic_light_spot(false, true), MaximizedFlyover);
        assert_eq!(traffic_light_spot(true, true), MaximizedFlyover);
        let (x, y) = traffic_light_origin(MaximizedFlyover);
        let panel = flyover_rect(1000, 800, 1.0, 1.0, 0.3, true);
        let bar = flyover_tab_bar(&panel, 1.0);
        assert!(x + 12.0 * 3.0 + 8.0 * 2.0 < TRAFFIC_LIGHT_SAFE_W);
        assert_eq!(y + 6.0, bar.y + bar.h / 2.0);
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

    fn term_tile(tabs: usize) -> Tile {
        let mut tile = Tile::empty(1);
        for _ in 0..tabs {
            tile.tabs.push(Tab::new(Session::placeholder()));
        }
        tile
    }

    fn labels(tile: &Tile) -> Vec<bool> {
        tile.tabs.iter().map(|tab| tab.pinned).collect()
    }

    #[test]
    fn set_tab_pinned_moves_tab_to_partition_boundary() {
        // Pinning the middle tab of an all-unpinned strip puts it at the end
        // of the (newly created) pinned run, i.e. the front of the strip.
        let mut tile = term_tile(3);
        assert_eq!(tile.pinned_count(), 0);
        assert_eq!(tile.set_tab_pinned(1, true), Some(0));
        assert_eq!(labels(&tile), vec![true, false, false]);
        assert_eq!(tile.pinned_count(), 1);
        // Pinning another tab appends it after the existing pinned run.
        assert_eq!(tile.set_tab_pinned(2, true), Some(1));
        assert_eq!(labels(&tile), vec![true, true, false]);
        // Unpinning the second pinned tab moves it to the START of the
        // unpinned run, directly after the remaining pinned one.
        assert_eq!(tile.set_tab_pinned(1, false), Some(1));
        assert_eq!(labels(&tile), vec![true, false, false]);
        // Unpinning the last pinned tab drops the run to zero; the tab
        // stays at the front of the (now fully unpinned) strip.
        assert_eq!(tile.set_tab_pinned(0, false), Some(0));
        assert_eq!(labels(&tile), vec![false, false, false]);
        assert_eq!(tile.pinned_count(), 0);
    }

    #[test]
    fn insert_tab_clamps_into_the_matching_run() {
        // Strip: [pinned, pinned, unpinned].
        let mut tile = term_tile(3);
        tile.set_tab_pinned(0, true);
        tile.set_tab_pinned(1, true);
        // An unpinned drop aimed at the front lands right after the pins.
        let unpinned = Tab::new(Session::placeholder());
        assert_eq!(tile.insert_tab(0, unpinned), 2);
        assert_eq!(labels(&tile), vec![true, true, false, false]);
        // A pinned drop aimed at the end lands at the end of the pinned run.
        let mut pinned = Tab::new(Session::placeholder());
        pinned.pinned = true;
        assert_eq!(tile.insert_tab(99, pinned), 2);
        assert_eq!(labels(&tile), vec![true, true, true, false, false]);
        // Inside the right run the requested index is honoured.
        let mut pinned = Tab::new(Session::placeholder());
        pinned.pinned = true;
        assert_eq!(tile.insert_tab(1, pinned), 1);
        let unpinned = Tab::new(Session::placeholder());
        assert_eq!(tile.insert_tab(5, unpinned), 5);
        assert_eq!(tile.pinned_count(), 4);
    }

    #[test]
    fn set_tab_pinned_no_ops_and_rejects_invalid_indexes() {
        let mut tile = term_tile(2);
        // Already in the requested state: index unchanged, tab not moved.
        assert_eq!(tile.set_tab_pinned(0, false), Some(0));
        assert_eq!(labels(&tile), vec![false, false]);
        // Pinning the trailing tab moves it to the FRONT (end of the empty
        // pinned run); pinning it again at its new index is a no-op.
        assert_eq!(tile.set_tab_pinned(1, true), Some(0));
        assert_eq!(labels(&tile), vec![true, false]);
        assert_eq!(tile.set_tab_pinned(1, true), Some(1));
        assert_eq!(labels(&tile), vec![true, true]);
        // Out-of-range indexes (and the empty tile) report failure.
        assert_eq!(tile.set_tab_pinned(2, true), None);
        assert_eq!(Tile::empty(9).set_tab_pinned(0, true), None);
    }

    #[test]
    fn set_tab_pinned_active_follows_the_moved_tab() {
        // Active ON the moved tab: pinning drags the selection with it.
        let mut tile = term_tile(3);
        tile.active = 1;
        assert_eq!(tile.set_tab_pinned(1, true), Some(0));
        assert_eq!(tile.active, 0);
        assert!(tile.active_tab().unwrap().pinned);
        // Unpinning it again keeps it at the front of the strip with the
        // selection still on it.
        assert_eq!(tile.set_tab_pinned(0, false), Some(0));
        assert_eq!(tile.active, 0);

        // Active AFTER the moved tab: shift compensation keeps it on the
        // same tab across the remove-then-insert.
        let mut tile = term_tile(3);
        tile.active = 2;
        assert_eq!(tile.set_tab_pinned(0, true), Some(0));
        assert_eq!(labels(&tile), vec![true, false, false]);
        assert_eq!(tile.active, 2);

        // Active BETWEEN old and new positions while unpinning a pinned tab.
        let mut tile = term_tile(3);
        tile.tabs[0].pinned = true;
        tile.tabs[1].pinned = true;
        tile.active = 2;
        assert_eq!(tile.set_tab_pinned(1, false), Some(1));
        assert_eq!(labels(&tile), vec![true, false, false]);
        assert_eq!(tile.active, 2);
    }

    #[test]
    fn set_tab_pinned_keeps_other_tabs_around_active_stable() {
        // Moving a tab on either side of the active one must not change
        // which tab the tile considers active.
        let mut tile = term_tile(4);
        tile.active = 2;
        // A LATER tab is pinned past the active one: the selection shifts
        // right with the strip but still names the same tab.
        assert_eq!(tile.set_tab_pinned(3, true), Some(0));
        assert_eq!(tile.active, 3);
        // An EARLIER unpinned tab joins the run in front of the active one.
        assert_eq!(tile.set_tab_pinned(1, true), Some(1));
        assert_eq!(tile.active, 3);
        assert_eq!(labels(&tile), vec![true, true, false, false]);
    }

    #[test]
    fn toolbar_hidden_is_terminal_safe() {
        let mut terminal = Tab::new(Session::placeholder());
        assert!(!terminal.toolbar_hidden());
        // Terminals have no toolbar: the setter is a no-op reporting no
        // change, and the getter stays false.
        assert!(!terminal.set_toolbar_hidden(true));
        assert!(!terminal.toolbar_hidden());

        let mut webview = Tab::webview(3, "https://example.com".into());
        assert!(!webview.toolbar_hidden());
        assert!(webview.set_toolbar_hidden(true));
        assert!(webview.toolbar_hidden());
        // Setting the same state again reports no change.
        assert!(!webview.set_toolbar_hidden(true));
        assert!(webview.set_toolbar_hidden(false));
        assert!(!webview.toolbar_hidden());
        assert!(!webview.set_toolbar_hidden(false));
    }


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
    fn webview_tab_exposes_kind_identity_url_and_title() {
        let mut tab = Tab::webview(42, "https://github.com/tauri-apps/wry".into());
        assert_eq!(tab.kind(), TabKind::Webview);
        assert_eq!(tab.webview_id(), Some(42));
        assert_eq!(tab.url(), Some("https://github.com/tauri-apps/wry"));
        assert_eq!(tab.title(), "github.com/tauri-apps");
        assert!(tab.session().is_none());
        assert!(tab.set_webview_url("https://example.com/docs".into()));
        assert_eq!(tab.url(), Some("https://example.com/docs"));
        assert!(!tab.set_webview_url("https://example.com/docs".into()));
    }

    #[test]
    fn webview_url_command_metadata_stays_runtime_only() {
        // Plain constructor: url_command defaults to None.
        let tab = Tab::webview(1, "https://example.com".into());
        assert_eq!(tab.url_command(), None);
        // Command-aware constructor records the command; mutating the URL
        // leaves the runtime-only command untouched.
        let mut tab = Tab::webview_with_command(
            2,
            "about:blank".into(),
            Some("echo example.com".into()),
        );
        assert_eq!(tab.url_command(), Some("echo example.com"));
        assert!(tab.set_webview_url("https://example.com/loaded".into()));
        assert_eq!(tab.url_command(), Some("echo example.com"));
        // None is accepted as well, and terminal tabs never carry a command.
        let tab = Tab::webview_with_command(3, "https://example.com".into(), None);
        assert_eq!(tab.url_command(), None);
        let tab = Tab::new(crate::term::Session::placeholder());
        assert_eq!(tab.url_command(), None);
    }

    #[test]
    fn webview_tab_prefers_document_title_and_falls_back_to_url() {
        let mut tab = Tab::webview(7, "https://www.google.com/".into());
        assert_eq!(tab.title(), "www.google.com");
        assert!(tab.set_webview_title("  Google  ".into()));
        assert_eq!(tab.title(), "Google");
        assert!(!tab.set_webview_title("Google".into()));
        // A navigation keeps the last title until the new document reports one.
        assert!(tab.set_webview_url("https://example.com/docs".into()));
        assert_eq!(tab.title(), "Google");
        // The native view reports an empty title while a document loads.
        assert!(tab.set_webview_title(String::new()));
        assert_eq!(tab.title(), "example.com/docs");
        assert!(!tab.set_webview_title("   ".into()));
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

    fn tab_with_attention(unread: bool, seconds: u64) -> Tab {
        use crate::term::Session;
        let mut tab = Tab::new(Session::placeholder());
        tab.unread = unread;
        tab.unread_at = Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
        tab
    }

    #[test]
    fn attention_at_returns_none_with_no_history() {
        let ws = Workspace::new("g".into(), Tile::empty(7), None);
        assert_eq!(ws.attention_at(), None);
    }

    #[test]
    fn attention_at_returns_oldest_unread_tab_stamp() {
        let mut tile = Tile::new(7, crate::term::Session::placeholder());
        tile.tabs.push(tab_with_attention(true, 30));
        tile.tabs.push(tab_with_attention(true, 10));
        let ws = Workspace::new("g".into(), tile, None);
        let expected = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10);
        assert_eq!(ws.attention_at(), Some(expected));
    }

    #[test]
    fn attention_at_ignores_read_tabs_while_any_tab_is_unread() {
        let mut tile = Tile::new(7, crate::term::Session::placeholder());
        tile.tabs.push(tab_with_attention(false, 5));
        tile.tabs.push(tab_with_attention(true, 20));
        let ws = Workspace::new("g".into(), tile, None);
        assert_eq!(ws.attention_at(), Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(20)));
    }

    #[test]
    fn attention_at_returns_most_recent_stamp_once_everything_is_read() {
        let mut tile = Tile::new(7, crate::term::Session::placeholder());
        tile.tabs.push(tab_with_attention(false, 5));
        tile.tabs.push(tab_with_attention(false, 20));
        let ws = Workspace::new("g".into(), tile, None);
        assert_eq!(ws.attention_at(), Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(20)));
    }

    #[test]
    fn attention_at_is_none_when_unread_tabs_have_no_stamp_and_nothing_else_does() {
        let mut tile = Tile::new(7, crate::term::Session::placeholder());
        let mut tab = Tab::new(crate::term::Session::placeholder());
        tab.unread = true;
        tile.tabs.push(tab);
        let ws = Workspace::new("g".into(), tile, None);
        assert_eq!(ws.attention_at(), None);
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
    fn snoozing_hides_the_unread_dot_but_keeps_the_signal() {
        use crate::term::Session;
        let mut tile = Tile::new(42, Session::placeholder());
        tile.tabs[0].unread = true;
        let mut ws = Workspace::new("g".into(), tile, None);
        assert!(ws.shows_unread_dot(), "a live unread tab dots the row");
        ws.snoozed = true;
        assert!(ws.any_unread(), "snoozing must not clear the unread signal");
        assert!(!ws.shows_unread_dot(), "a snoozed row paints no dot");
    }

    /// The open "Snoozed" section: one snoozed group, not folded.
    fn snoozed_open() -> SnoozedSection {
        SnoozedSection { total: 1, collapsed: false }
    }

    #[test]
    fn snoozed_rows_sort_below_the_snoozed_caption() {
        let mut later = ws("later", None);
        later.snoozed = true;
        let workspaces = vec![ws("a", None), later, ws("b", None)];

        // Ordering: pinned first (none here), then workspace order, then the
        // snoozed run at the bottom.
        let rows = sidebar_rows_filtered(&workspaces, &[], None, false, false);
        assert_eq!(
            rows,
            vec![
                SidebarRow { ws_idx: 0 },
                SidebarRow { ws_idx: 2 },
                SidebarRow { ws_idx: 1 },
            ]
        );
        assert_eq!(snoozed_run(&rows, &workspaces), 1);

        // Geometry: the snoozed row hangs off the caption band, which itself
        // sits a section gap below the last ordinary row.
        let list = LayoutRect { x: 0.0, y: 0.0, w: 260.0, h: 600.0 };
        let scale = 1.0;
        let a = sidebar_row_rect(&rows, 0, &workspaces, false, scale, &list);
        let b = sidebar_row_rect(&rows, 1, &workspaces, false, scale, &list);
        let s = sidebar_row_rect(&rows, 2, &workspaces, false, scale, &list);
        let cap =
            snoozed_caption_rect(&rows, &workspaces, false, snoozed_open(), scale, &list);
        assert_eq!(b.y, a.y + a.h);
        assert!(cap.y >= b.y + b.h, "the caption never overlaps the last row");
        assert_eq!(s.y, cap.y + cap.h);
        assert!(s.y > b.y + b.h);

        // Nothing snoozed: no band at all.
        let plain = sidebar_rows_filtered(&workspaces[..1], &[], None, false, false);
        assert_eq!(snoozed_run(&plain, &workspaces), 0);
        assert_eq!(
            snoozed_caption_rect(&plain, &workspaces, false, SnoozedSection::default(), scale, &list).h,
            0.0
        )
    }

    /// The "Snoozed" section folds on a click, exactly as the "Pinned" run
    /// does: the rows leave the list, the caption band stays put as the
    /// toggle, and the stack's extent still covers it.
    #[test]
    fn snoozed_section_folds_away_like_the_pinned_run() {
        let mut later = ws("later", None);
        later.snoozed = true;
        let workspaces = vec![ws("a", None), later, ws("b", None)];
        let list = LayoutRect { x: 0.0, y: 0.0, w: 260.0, h: 600.0 };
        let scale = 1.0;
        let folded = SnoozedSection { total: 1, collapsed: true };

        // Folded: the snoozed row leaves the list.
        let rows = sidebar_rows_filtered(&workspaces, &[], None, false, true);
        assert_eq!(rows.iter().map(|r| r.ws_idx).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(snoozed_run(&rows, &workspaces), 0);

        // The caption survives — it is the toggle — right under the last
        // ordinary row, with the rail in the gap above it.
        let cap = snoozed_caption_rect(&rows, &workspaces, false, folded, scale, &list);
        let last = sidebar_row_rect(&rows, 1, &workspaces, false, scale, &list);
        assert_eq!(cap.h, PINNED_CAPTION_H * scale);
        assert!(cap.y >= last.y + last.h);
        let rail = snoozed_divider_rect(&rows, &workspaces, false, folded, scale, &list);
        assert!(rail.y < cap.y && rail.y >= last.y + last.h - PINNED_SECTION_GAP * scale);

        // Extent: the row band is gone, the caption's bottom still counts.
        let open_rows = sidebar_rows_filtered(&workspaces, &[], None, false, false);
        let open_ext =
            sidebar_rows_extent(&open_rows, &workspaces, false, snoozed_open(), scale, &list);
        let folded_ext = sidebar_rows_extent(&rows, &workspaces, false, folded, scale, &list);
        assert!(folded_ext < open_ext);
        assert_eq!(folded_ext, cap.y + cap.h - (list.y + SESSIONS_HEADER_H * scale));

        // Nothing snoozed: no band at all, folded or not.
        let none = sidebar_rows_filtered(&workspaces[..1], &[], None, false, true);
        assert_eq!(
            snoozed_caption_rect(&none, &workspaces, false, SnoozedSection::default(), scale, &list).h,
            0.0
        )
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
    fn empty_state_cta_centered_in_terminal_area() {
        let (w, h, scale, sidebar_w) = (1600, 1000, 2.0, SIDEBAR_DEFAULT_W);
        let area = terminal_area(w, h, scale, sidebar_w, 0.0);
        let cta = empty_state_cta(w, h, scale, sidebar_w);
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
        let inset = (COLLAPSED_STRIP_INSET * scale).round();

        // Top-left tile: strip starts right of the traffic lights and the
        // floating "Show sessions" button, same span otherwise (right edge,
        // y band unchanged).
        let top_left = LayoutRect { x: area.x, y: area.y, w: 800.0, h: 400.0 };
        let strip = tab_strip_rect(area, &top_left, scale, 0.0);
        assert_eq!(strip.x, inset);
        let btn = show_sessions_button(scale);
        assert!(btn.x + btn.w <= strip.x);
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
        assert_eq!(resize_hover_at(&node, area, 2.0, 0.0, None, 12.0, false, 4.0, 500.0), None);
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
            anchor: None,
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
            anchor: None,
        };
        apply_section_rename(&mut s, "✨");
        assert_eq!(s.emoji, "✨");
        assert_eq!(s.name, "keep");
    }

    // --- (b) row derivation ---

    #[test]
    fn row_heights_follow_the_app_text_size() {
        // Identity at the default size: this is what every other geometry
        // assertion in this module implicitly relies on.
        assert_eq!(sidebar_row_h(1.0), CARD_H);
        // Doubling the text size doubles the row that has to hold it.
        assert_eq!(sidebar_row_h(2.0), CARD_H * 2.0);
        // A two-line session row stays taller than a one-line Settings row
        // at every size.
        for f in [0.6_f32, 1.0, 1.7, 2.7] {
            assert!(sidebar_row_h(f) > TAB_H * f);
        }
    }

    /// The rect helpers, not just the height formulas, have to carry the text
    /// factor through — pinned at a non-default size so dropping the wiring
    /// fails here rather than passing on the ambient default of 1.0.
    #[test]
    fn row_rects_carry_the_text_factor_through() {
        let scale = 2.0;
        let list = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true, 1000, scale);
        let rows = [SidebarRow { ws_idx: 0 }, SidebarRow { ws_idx: 1 }];
        let workspaces = [ws("a", None), ws("b", None)];
        let pitch = |f: f32| {
            let a = sidebar_row_rect_at(&rows, 0, &workspaces, true, scale, f, &list);
            let b = sidebar_row_rect_at(&rows, 1, &workspaces, true, scale, f, &list);
            (a.h, b.y - a.y)
        };
        let (h1, pitch1) = pitch(1.0);
        let (h2, pitch2) = pitch(2.0);
        assert_eq!(h1, (CARD_H * scale).round());
        assert_eq!(h2, (CARD_H * 2.0 * scale).round());
        // Rows touch: the mock separates them with a hairline, not a gap.
        assert_eq!(pitch1, h1);
        assert_eq!(pitch2, h2);

        // Same for the Settings ladder; both start below the list header.
        assert_eq!(tab_rect_at(0, scale, 1.0, &list).h, (TAB_H * scale).round());
        assert_eq!(tab_rect_at(0, scale, 2.0, &list).h, (TAB_H * 2.0 * scale).round());
        let header_bottom = list.y + (SESSIONS_HEADER_H * scale).round();
        assert_eq!(
            tab_rect_at(0, scale, 2.0, &list).y,
            header_bottom + 2.0 * (TAB_GAP * scale).round()
        );
        // Session rows start under the (always present) "Pinned" caption and
        // its section gap.
        assert_eq!(
            sidebar_row_rect_at(&rows, 0, &workspaces, true, scale, 2.0, &list).y,
            header_bottom + (PINNED_CAPTION_H * scale).round() + (PINNED_SECTION_GAP * scale).round()
        );

        // The live factor is capped, so a large text size can never hide more
        // of the un-scrollable row stack than the cap allows.
        assert!(row_font_scale() <= MAX_ROW_FONT_SCALE);
        // Below the cap the setting passes through untouched; above it, it
        // stops. `appearance.font_size` reaches 40px (≈2.7×), which without
        // this would leave about five rows in a 900pt window.
        assert_eq!(cap_row_font_scale(1.0), 1.0);
        assert_eq!(cap_row_font_scale(1.2), 1.2);
        assert_eq!(cap_row_font_scale(MAX_ROW_FONT_SCALE), MAX_ROW_FONT_SCALE);
        assert_eq!(cap_row_font_scale(2.7), MAX_ROW_FONT_SCALE);
        assert_eq!(cap_row_font_scale(40.0 / 15.0), MAX_ROW_FONT_SCALE);
    }

    // --- (gantry) two-part sidebar region ---

    /// The region's width is the sessions width plus the folders column
    /// while the card is open, plus only a slim inset while it is closed;
    /// collapsed (sessions width zero) always reports zero so
    /// `terminal_area` keeps keying off the same zero.
    #[test]
    fn sidebar_region_width_adds_the_folders_column() {
        assert_eq!(sidebar_region_w(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true), SIDEBAR_DEFAULT_W + 226.0);
        assert_eq!(sidebar_region_w(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, false), SIDEBAR_DEFAULT_W + 16.0);
        assert_eq!(sidebar_region_w(0.0, FOLDERS_CARD_W, true), 0.0);
        assert_eq!(sidebar_region_w(0.0, FOLDERS_CARD_W, false), 0.0);
        assert_eq!(sidebar_region_w(-1.0, FOLDERS_CARD_W, true), 0.0);
    }

    /// The drag clamp bounds the sessions-list width; the GANTRY redesign
    /// retunes the three constants, so pin them at their new values.
    #[test]
    fn sessions_width_constants_match_the_gantry_mock() {
        assert_eq!(SIDEBAR_MIN_W, 180.0);
        assert_eq!(SIDEBAR_DEFAULT_W, 260.0);
        assert_eq!(SIDEBAR_MAX_W, 520.0);
    }

    /// Dragging the grab band: the pointer is the region's right edge, so
    /// the stored sessions width is that minus the folders column (open) or
    /// the closed inset, clamped — and a round trip through
    /// `sidebar_region_w` lands back on the pointer.
    #[test]
    fn resize_drag_takes_the_folders_column_back_off() {
        let x = sidebar_region_w(300.0, FOLDERS_CARD_W, true);
        assert_eq!(sessions_w_for_pointer(x, FOLDERS_CARD_W, true), 300.0);
        assert_eq!(sidebar_region_w(sessions_w_for_pointer(x, FOLDERS_CARD_W, true), FOLDERS_CARD_W, true), x);
        let x = sidebar_region_w(300.0, FOLDERS_CARD_W, false);
        assert_eq!(sessions_w_for_pointer(x, FOLDERS_CARD_W, false), 300.0);
        // Clamped at both ends.
        assert_eq!(sessions_w_for_pointer(0.0, FOLDERS_CARD_W, true), SIDEBAR_MIN_W);
        assert_eq!(sessions_w_for_pointer(5000.0, FOLDERS_CARD_W, false), SIDEBAR_MAX_W);
    }

    /// Card and list rects: the card is the left 200px column inset by the
    /// region padding and spans the full region height; the list starts
    /// right of card + gutter, keeps the right grab-band inset, and shares
    /// the card's vertical inset (its header band holds the title/chips). Every coordinate is scaled and
    /// rounded the same way painting and hit-testing will read it.
    #[test]
    fn gantry_card_and_list_rects_follow_the_region() {
        let (h, s) = (1000_u32, 2.0_f32);
        let card = folders_card_rect(h, FOLDERS_CARD_W, s);
        assert_eq!(card.x, (REGION_PAD * s).round());
        assert_eq!(card.y, card.x);
        assert_eq!(card.w, (FOLDERS_CARD_W * s).round());
        assert_eq!(card.h, h as f32 - 2.0 * card.y);

        let list = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true, h, s);
        assert_eq!(list.x, card.x + card.w + (REGION_GAP * s).round());
        assert_eq!(list.y, (REGION_PAD * s).round());
        let region_w = sidebar_region_w(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true) * s;
        assert_eq!(list.x + list.w, region_w - (REGION_RIGHT_PAD * s).round());
        assert!(list.h > 0.0);

        // Card closed: the list takes over the slim inset and the gutter.
        let closed = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, false, h, s);
        assert_eq!(closed.x, (FOLDERS_CLOSED_INSET * s).round());
        let closed_region = sidebar_region_w(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, false) * s;
        assert_eq!(closed.x + closed.w, closed_region - (REGION_RIGHT_PAD * s).round());

        // Collapsed (sessions width zero): no list at all.
        assert_eq!(sessions_list_rect(0.0, FOLDERS_CARD_W, true, h, s).w, 0.0);

        // Degenerate window height: the rect never goes negative.
        assert!(sessions_list_rect(SIDEBAR_MIN_W, FOLDERS_CARD_W, true, 10, s).h >= 0.0);
    }

    /// The flat list is Group-only rows in workspace order: no section
    /// headers, the folder's pinned groups first (the "Pinned" section), a
    /// section filter keeps just its members, and an id nothing owns
    /// filters to nothing.
    #[test]
    fn sidebar_rows_filtered_keeps_flat_group_rows() {
        let mut pinned_a = ws("pinned", Some(7));
        pinned_a.pinned = true;
        let workspaces = vec![
            ws("a", None),
            pinned_a,
            ws("b", Some(7)),
            ws("c", None),
            ws("d", Some(8)),
        ];
        let sections = vec![sec(7, false), sec(8, false)];

        // No filter: every group, pins first, then workspace order.
        let rows = sidebar_rows_filtered(&workspaces, &sections, None, false, false);
        assert_eq!(
            rows,
            vec![
                SidebarRow { ws_idx: 1 },
                SidebarRow { ws_idx: 0 },
                SidebarRow { ws_idx: 2 },
                SidebarRow { ws_idx: 3 },
                SidebarRow { ws_idx: 4 },
            ]
        );
        assert_eq!(pinned_run(&rows, &workspaces), 1);

        // A section filter keeps only that section's members, its pin first.
        assert_eq!(
            sidebar_rows_filtered(&workspaces, &sections, Some(7), false, false),
            vec![SidebarRow { ws_idx: 1 }, SidebarRow { ws_idx: 2 }]
        );

        // Unknown or dangling ids filter to nothing.
        assert!(sidebar_rows_filtered(&workspaces, &sections, Some(99), false, false).is_empty());
    }

    #[test]
    fn normalize_sets_anchor_to_following_group() {
        let mut member = ws("m", Some(1));
        member.primary_tile = 10;
        let mut g = ws("g", None);
        g.primary_tile = 11;
        let mut workspaces = vec![member, g];
        let mut sections = vec![sec(1, false)];
        normalize_section_anchors(&workspaces, &mut sections);
        assert_eq!(sections[0].anchor, Some(11));

        // Remove the member; the section empties but keeps its anchor.
        workspaces.remove(0);
        normalize_section_anchors(&workspaces, &mut sections);
        assert_eq!(sections[0].anchor, Some(11));
    }

    #[test]
    fn normalize_repoints_interior_anchor_to_first_member() {
        // Empty section E anchored at tile 20. Tile 20 starts as section S's
        // first member, then a new group (tile 19) is inserted ahead of it in
        // S, making tile 20 an interior member. E's anchor must re-point to the
        // new first member (tile 19) so its header stays before S rather than
        // silently dropping to the bottom.
        let mut e = sec(1, false);
        e.anchor = Some(20);
        let mut s_first = ws("s0", Some(2));
        s_first.primary_tile = 19;
        let mut s_second = ws("s1", Some(2));
        s_second.primary_tile = 20;
        let workspaces = vec![s_first, s_second];
        let mut sections = vec![e, sec(2, false)];
        normalize_section_anchors(&workspaces, &mut sections);
        assert_eq!(sections[0].anchor, Some(19));
    }

    #[test]
    fn normalize_clears_dangling_anchor() {
        let mut g = ws("g", None);
        g.primary_tile = 11;
        let workspaces = vec![g];
        let mut s = sec(1, false);
        s.anchor = Some(999);
        let mut sections = vec![s];
        normalize_section_anchors(&workspaces, &mut sections);
        assert_eq!(sections[0].anchor, None);
    }

    #[test]
    fn active_row_index_finds_the_group_row() {
        let rows = [SidebarRow { ws_idx: 2 }, SidebarRow { ws_idx: 5 }];
        assert_eq!(active_row_index(&rows, 2), Some(0));
        assert_eq!(active_row_index(&rows, 5), Some(1));
        // Pinned or filtered-out groups have no row to highlight.
        assert_eq!(active_row_index(&rows, 3), None);
    }

    // --- (c) geometry ---

    /// The "Pinned" caption always heads the list (it is the drop zone that
    /// pins); the section gap — with the rail centred in it — sits after
    /// the pinned run, or straight under the caption when nothing is
    /// pinned. Rows keep their touching pitch either side of the gap.
    #[test]
    fn sidebar_row_rect_carves_out_the_pinned_section() {
        let mut workspaces = vec![ws("a", None), ws("b", None), ws("c", None)];
        let scale = 1.0;
        let list = sessions_list_rect(300.0, FOLDERS_CARD_W, false, 1000, scale);
        let plain = [SidebarRow { ws_idx: 0 }, SidebarRow { ws_idx: 1 }];
        // Nothing pinned and no drag: no section, rows start under the header.
        let bare = sidebar_row_rect(&plain, 0, &workspaces, false, scale, &list);
        assert_eq!(bare.y, list.y + SESSIONS_HEADER_H * scale);
        assert_eq!(pinned_drop_zone(&plain, &workspaces, false, scale, &list).h, 0.0);
        assert_eq!(pinned_divider_rect(&plain, &workspaces, false, scale, &list).h, 0.0);
        assert_eq!(
            sidebar_rows_extent(&[], &workspaces, false, SnoozedSection::default(), scale, &list),
            0.0
        );
        // With the section shown (a drag is live), the caption heads the list.
        let r0 = sidebar_row_rect(&plain, 0, &workspaces, true, scale, &list);
        let r1 = sidebar_row_rect(&plain, 1, &workspaces, true, scale, &list);
        let caption = pinned_caption_rect(scale, &list);
        assert_eq!(caption.y, list.y + SESSIONS_HEADER_H * scale);
        assert_eq!(r0.y, caption.y + caption.h + PINNED_SECTION_GAP);
        assert_eq!(r0.x, list.x);
        // No pins: the drop zone is the caption plus the gap, and the rail
        // sits in the gap.
        let zone = pinned_drop_zone(&plain, &workspaces, true, scale, &list);
        assert_eq!(zone.y, caption.y);
        assert_eq!(zone.y + zone.h, r0.y);
        let rail = pinned_divider_rect(&plain, &workspaces, true, scale, &list);
        assert!(rail.y >= caption.y + caption.h && rail.y + rail.h <= r0.y);
        assert_eq!(
            sidebar_rows_extent(&plain, &workspaces, true, SnoozedSection::default(), scale, &list),
            r1.y + r1.h - (list.y + SESSIONS_HEADER_H * scale)
        );
        assert_eq!(r0.w, list.w);
        assert_eq!(r1.y, r0.y + r0.h);

        workspaces[2].pinned = true;
        let pinned = [SidebarRow { ws_idx: 2 }, SidebarRow { ws_idx: 0 }, SidebarRow { ws_idx: 1 }];
        assert_eq!(pinned_run(&pinned, &workspaces), 1);
        let caption = pinned_caption_rect(scale, &list);
        let p0 = sidebar_row_rect(&pinned, 0, &workspaces, true, scale, &list);
        let p1 = sidebar_row_rect(&pinned, 1, &workspaces, true, scale, &list);
        let p2 = sidebar_row_rect(&pinned, 2, &workspaces, true, scale, &list);
        assert_eq!(caption.y, list.y + SESSIONS_HEADER_H * scale);
        assert_eq!(p0.y, caption.y + caption.h);
        assert_eq!(p1.y, p0.y + p0.h + PINNED_SECTION_GAP);
        assert_eq!(p2.y, p1.y + p1.h);
        // With pins the caption alone is the drop zone; the rail closes the run.
        let zone = pinned_drop_zone(&pinned, &workspaces, true, scale, &list);
        assert_eq!(zone.h, caption.h);
        let rail = pinned_divider_rect(&pinned, &workspaces, true, scale, &list);
        assert!(rail.y >= p0.y + p0.h && rail.y + rail.h <= p1.y);
        // Folding the run drops the pinned rows from the list; the gap then
        // sits straight under the caption again.
        let folded = sidebar_rows_filtered(&workspaces, &[], None, true, false);
        assert_eq!(folded.iter().map(|r| r.ws_idx).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(sidebar_row_rect(&folded, 0, &workspaces, true, scale, &list).y, r0.y);
        assert_eq!(max_scroll(100.0, 60.0), 40.0);
        assert_eq!(max_scroll(50.0, 60.0), 0.0);
    }

    #[test]
    fn sidebar_drag_collapses_past_the_slack() {
        let narrowest = sidebar_region_w(SIDEBAR_MIN_W, FOLDERS_CARD_W, true);
        assert!(!sidebar_drag_collapses(narrowest, FOLDERS_CARD_W, true));
        assert!(!sidebar_drag_collapses(narrowest - SIDEBAR_COLLAPSE_SLACK, FOLDERS_CARD_W, true));
        assert!(sidebar_drag_collapses(narrowest - SIDEBAR_COLLAPSE_SLACK - 1.0, FOLDERS_CARD_W, true));
        // Without the folders column the threshold moves in with the region.
        let closed = sidebar_region_w(SIDEBAR_MIN_W, FOLDERS_CARD_W, false);
        assert!(closed < narrowest);
        assert!(sidebar_drag_collapses(closed - SIDEBAR_COLLAPSE_SLACK - 1.0, FOLDERS_CARD_W, false));
        assert!(!sidebar_drag_collapses(closed, FOLDERS_CARD_W, false));
    }

    #[test]
    fn folders_drag_collapses_past_the_slack() {
        // The band's x at the card's minimum width.
        let narrowest = folders_edge_x(FOLDERS_MIN_W, true).unwrap();
        assert_eq!(folders_w_for_pointer(narrowest), FOLDERS_MIN_W);
        assert!(!folders_drag_collapses(narrowest));
        assert!(!folders_drag_collapses(narrowest - FOLDERS_COLLAPSE_SLACK));
        assert!(folders_drag_collapses(narrowest - FOLDERS_COLLAPSE_SLACK - 1.0));
    }

    #[test]
    fn reorder_section_moves_a_folder_and_returns_its_slot() {
        let mut sections = vec![sec(1, false), sec(2, false), sec(3, false)];
        // Drag the first folder below the last: the gap after index 2.
        let at = reorder_section(&mut sections, 0, 3);
        assert_eq!(at, 2);
        assert_eq!(sections.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2, 3, 1]);
        // Dropping into its own gap is a no-op.
        let at = reorder_section(&mut sections, 1, 1);
        assert_eq!(at, 1);
        assert_eq!(sections.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2, 3, 1]);
        // Folders card rows fold the tools run away.
        assert_eq!(folder_rows(2, true, 1), vec![FolderRow::PinnedHeader, FolderRow::AllSessions, FolderRow::Section(0)]);
        assert_eq!(folder_rows(2, false, 1).len(), 5);
    }

    #[test]
    fn terminal_area_reserves_the_bottom_inset() {
        let (w, h, scale) = (1600u32, 1000u32, 2.0f32);
        let full = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, 0.0);
        let inset = terminal_area(w, h, scale, SIDEBAR_DEFAULT_W, 68.0);
        assert_eq!(full.h - inset.h, (68.0 * scale).round());
        assert_eq!(full.y, inset.y, "the inset only trims the bottom edge");
    }

    #[test]
    fn terminal_area_agrees_with_the_wider_sidebar() {
        let (w, h, scale) = (1600u32, 1000u32, 2.0f32);
        let region = sidebar_region_w(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true);
        let area = terminal_area(w, h, scale, region, 0.0);
        // The split tree starts exactly at the region's right edge.
        assert_eq!(area.x, (region * scale).round());

        // ...and every session row stays inside the list, clear of it.
        let workspaces = vec![ws("a", None), ws("b", None)];
        let rows = sidebar_rows_filtered(&workspaces, &[], None, false, false);
        let list = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true, h, scale);
        for i in 0..rows.len() {
            let r = sidebar_row_rect(&rows, i, &workspaces, true, scale, &list);
            assert!(r.x + r.w <= area.x);
            assert!(r.x >= list.x);
        }
    }

    /// The sessions-list header carries the chips: focus / ＋ / gear
    /// right-clustered inside the header band, plus a "Show folders" chip
    /// clear of the traffic lights only while the card is hidden.
    #[test]
    fn sessions_header_chips_cluster_right_and_show_folders_only_when_closed() {
        let scale = 2.0;
        let list = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, true, 1000, scale);
        let chips = sessions_header_chips(&list, true, scale);
        assert!(chips.show_folders.is_none());
        let side = (HEADER_CHIP * scale).round();
        let gap = (HEADER_CHIP_GAP * scale).round();
        for c in [chips.focus, chips.plus, chips.gear] {
            assert_eq!(c.w, side);
            assert_eq!(c.h, side);
            assert_eq!(c.y, chips.gear.y);
        }
        assert_eq!(chips.gear.x + side, list.x + list.w - (HEADER_CHIP_INSET * scale).round());
        assert_eq!(chips.plus.x + side + gap, chips.gear.x);
        assert_eq!(chips.focus.x + side + gap, chips.plus.x);
        assert!(chips.gear.y >= list.y);
        assert!(chips.gear.y + side <= list.y + (SESSIONS_HEADER_H * scale).round());

        let closed = sessions_list_rect(SIDEBAR_DEFAULT_W, FOLDERS_CARD_W, false, 1000, scale);
        let chips = sessions_header_chips(&closed, false, scale);
        let show = chips.show_folders.expect("show-folders chip while the card is hidden");
        assert_eq!(show.x, ((TRAFFIC_LIGHT_END + SHOW_FOLDERS_GAP) * scale).round());
        assert!(show.x >= (TRAFFIC_LIGHT_END * scale).round());
        assert_eq!(show.y, chips.gear.y);
        assert!(show.x + show.w < chips.focus.x);

        // Collapsed (no list): the chips shrink to nothing and hit nothing.
        let none = sessions_list_rect(0.0, FOLDERS_CARD_W, true, 1000, scale);
        let chips = sessions_header_chips(&none, true, scale);
        assert_eq!(chips.plus.w, 0.0);
        assert!(!chips.plus.contains(chips.plus.x, chips.plus.y));
    }

    /// Folder rows: the pinned-tools caption and its tool rows come first
    /// (only when there are tools), then "All sessions", then one row per
    /// section in `sections` order.
    #[test]
    fn folder_rows_order_tools_then_all_sessions_then_folders() {
        assert_eq!(
            folder_rows(2, false, 1),
            vec![
                FolderRow::PinnedHeader,
                FolderRow::Tool(0),
                FolderRow::Tool(1),
                FolderRow::AllSessions,
                FolderRow::Section(0),
            ]
        );
        assert_eq!(
            folder_rows(0, false, 2),
            vec![FolderRow::AllSessions, FolderRow::Section(0), FolderRow::Section(1)]
        );
        // No tools: nothing to separate "All sessions" from.
        let card = folders_card_rect(1000, FOLDERS_CARD_W, 1.0);
        assert!(folder_separator_rect(&card, &folder_rows(0, false, 2), 0.0, 1.0).is_none());
    }

    /// Folder row rects stack inside the card's body inset, start below the
    /// header band, use the slimmer tool height, and leave the separator's
    /// space between the tools and "All sessions".
    #[test]
    fn folder_row_rects_stack_inside_the_card() {
        let scale = 2.0;
        let card = folders_card_rect(1000, FOLDERS_CARD_W, scale);
        let rows = folder_rows(1, false, 2);
        let rects: Vec<_> = (0..rows.len())
            .map(|i| folder_row_rect(&card, &rows, i, 0.0, scale))
            .collect();
        let pad = (FOLDER_BODY_PAD * scale).round();
        for r in &rects {
            assert_eq!(r.x, card.x + pad);
            assert_eq!(r.w, card.w - 2.0 * pad);
        }
        assert_eq!(rects[0].y, card.y + (FOLDERS_HEADER_H * scale).round());
        assert_eq!(rects[0].h, (FOLDER_ROW_H * scale).round());
        assert_eq!(rects[1].h, (TOOL_ROW_H * scale).round());
        assert_eq!(rects[2].h, (FOLDER_ROW_H * scale).round());
        let gap = (FOLDER_ROW_GAP * scale).round();
        assert_eq!(rects[1].y, rects[0].y + rects[0].h + gap);
        assert_eq!(
            rects[2].y,
            rects[1].y + rects[1].h + gap + (FOLDER_SEPARATOR_H * scale).round()
        );
        assert_eq!(rects[3].y, rects[2].y + rects[2].h + gap);
        let sep = folder_separator_rect(&card, &rows, 0.0, scale).unwrap();
        assert!(sep.y > rects[1].y + rects[1].h);
        assert!(sep.y + sep.h <= rects[2].y);
        assert!(sep.x > card.x && sep.x + sep.w < card.x + card.w);

        // Footer hugs the card's bottom; the header chips ride the header
        // band with "new" outermost, inset like the region.
        let footer = folders_footer_rect(&card, scale);
        assert_eq!(footer.y + footer.h, card.y + card.h);
        let (hide, new) = folders_header_chips(&card, scale);
        assert_eq!(hide.y, new.y);
        assert!(hide.x + hide.w < new.x);
        assert_eq!(new.x + new.w, card.x + card.w - (REGION_PAD * scale).round());
        assert!(new.y + new.h <= card.y + (FOLDERS_HEADER_H * scale).round());

        // Out of range: an empty rect at the stack's end.
        assert_eq!(folder_row_rect(&card, &rows, 9, 0.0, scale).h, 0.0);
    }

    /// The floating "Show sessions" button sits right of the folded traffic
    /// lights and inside the widened tab-strip inset, so the first tile's
    /// tabs never slide under it.
    #[test]
    fn show_sessions_button_sits_beside_the_folded_traffic_lights() {
        let scale = 2.0;
        let btn = show_sessions_button(scale);
        assert_eq!(btn.w, (SHOW_SESSIONS_BTN * scale).round());
        assert_eq!(btn.h, btn.w);
        assert_eq!(btn.x, ((TRAFFIC_LIGHT_END + SHOW_SESSIONS_GAP) * scale).round());
        assert!(btn.x + btn.w <= (COLLAPSED_STRIP_INSET * scale).round());
        // The strip keeps the same breathing room after the button as the
        // lights keep before it.
        assert_eq!((COLLAPSED_STRIP_INSET * scale).round() - (btn.x + btn.w), (SHOW_SESSIONS_GAP * scale).round());
        assert!(btn.y >= (AREA_PAD * scale).round());
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
    fn append_to_section_then_delete_removes_it() {
        let mut workspaces = vec![ws("a", None), ws("b", Some(2)), ws("c", Some(2))];
        let mut sections = vec![sec(2, false)];
        let idx = append_to_section(&mut workspaces, 0, 2);
        assert_eq!(workspaces[idx].section, Some(2));
        assert_eq!(section_member_range(&workspaces, 2), Some((0, 3)));

        // delete_section ungroups every member and drops the section; the
        // groups themselves survive.
        assert!(delete_section(&mut sections, &mut workspaces, 2));
        assert!(sections.is_empty());
        assert_eq!(workspaces.len(), 3);
        assert!(workspaces.iter().all(|w| w.section.is_none()));
    }

    #[test]
    fn delete_section_keeps_groups_ungrouped() {
        let mut workspaces = vec![ws("a", Some(1)), ws("b", Some(1)), ws("c", None)];
        let mut sections = vec![sec(1, false)];
        assert!(delete_section(&mut sections, &mut workspaces, 1));
        assert!(sections.is_empty());
        // Groups are kept, in order, now ungrouped.
        assert_eq!(workspaces.len(), 3);
        assert_eq!(workspaces[0].name, "a");
        assert_eq!(workspaces[1].name, "b");
        assert!(workspaces.iter().all(|w| w.section.is_none()));
        // Deleting a section that does not exist is a no-op.
        assert!(!delete_section(&mut sections, &mut workspaces, 99));
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
    fn empty_section_survives_until_deleted() {
        let mut workspaces = vec![ws("a", None)];
        let mut sections = vec![sec(3, false)];
        // Empty section still gets a folder row.
        assert!(folder_rows(0, false, sections.len()).contains(&FolderRow::Section(0)));
        // Nothing auto-removes it just for being empty.
        assert!(!workspaces.iter().any(|w| w.section == Some(3)));
        // Only the explicit delete removes it.
        assert!(delete_section(&mut sections, &mut workspaces, 3));
        assert!(sections.is_empty());
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
            resize_hover_at(&node, area, scale, sidebar_edge_x, None, grab, true, dx, dy),
            Some(ResizeHover::Divider { path: vec![], dir: Dir::Row })
        );

        // Point at the sidebar edge → Sidebar.
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, None, grab, true, sidebar_edge_x, 200.0),
            Some(ResizeHover::Sidebar)
        );

        // Point in a tile interior → None.
        let (tiles, _) = layout_tiles(&node, area, scale);
        let t = &tiles[0].1;
        let ix = t.x + t.w / 2.0;
        let iy = t.y + t.h / 2.0;
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, None, grab, true, ix, iy),
            None
        );

        // dividers_active=false suppresses divider hits but not the sidebar.
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, None, grab, false, dx, dy),
            None
        );
        assert_eq!(
            resize_hover_at(&node, area, scale, sidebar_edge_x, None, grab, false, sidebar_edge_x, 200.0),
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
            resize_hover_at(&node, area, scale, -100.0, None, grab, true, dx, dy),
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

}
