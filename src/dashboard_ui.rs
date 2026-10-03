//! The Dashboard page (`Page::Dashboard`, `Action::ToggleDashboard` ⌘G, the
//! sessions header's grid chip): the **primary pane** of every group in the
//! selected folder as a live card in one scrollable, count-adaptive grid, so
//! several agents can be watched — and typed into — at once. Secondary panes
//! never appear here.
//!
//! A card's body is the real terminal: the same `Session` the Sessions page
//! shows, its PTY resized to the card (`App::sync_dashboard_layout`) and
//! painted on the canvas by `Renderer::dashboard`. This module is the chrome
//! over that, as a gpui element tree to the GANTRY mock, plus the page's
//! model and input handling:
//!
//! - the **header bar** (a tile tab bar high, ceding the traffic-light corner
//!   while the sidebar is collapsed): "Dashboard", "N primary panes · M
//!   unread" (the selected folder's groups, whatever the filter), a segmented **All / Unread**
//!   filter and a × that closes the page;
//! - one **card** per showing group: a 34px header (the unread dot — the
//!   sidebar row's accent dot, with a halo ring, while unread, a dim dot with
//!   none once read — the title the sidebar row shows, `repo/branch` on cards
//!   300px or wider, the "unread" / "read" chip), the body, and a 30px footer
//!   (elapsed time since the group last asked for attention, the PR, the
//!   branch diff, "Open →"). The border is the chrome accent at 2px on the
//!   focused card, the same accent thin and faint on an unread one, brighter
//!   under the pointer; an unread card's ground is tinted with it too
//!   (`Renderer::dashboard`). A primary
//!   tab with no terminal (a webview) shows its title, dim and centred,
//!   instead;
//! - the **scroll hints**: while showing cards extend below the viewport, a
//!   "↓ N more" pill floats over its bottom edge (opaque, so it reads over
//!   terminal text); a click scrolls one row down. While some of those cards
//!   are unread it takes the unread style — the cards' unread dot at its
//!   left, "↓ N more · M unread" with the count in the unread ink, the
//!   unread border — and widens to fit. Its mirror, "↑ N more", floats under
//!   the top edge while showing cards are scrolled out above it; a click
//!   scrolls one row up.
//!
//! Every size is the figure at the default chrome text size and scales by
//! [`crate::workspace::chrome_ui_scale`]. The geometry is pure and lives in
//! `workspace` (`dashboard_bar` / `dashboard_viewport` / `dashboard_grid` /
//! `dashboard_card_rect` and the card's header / body / footer); [`Layout`]
//! bundles it with the live state, and the painter, this tree and the mouse
//! path all read that one value, so they cannot disagree.
//!
//! **Focus** is the active group: there is no dashboard focus of its own. A
//! press on a card (or ⇧⌘H/J/K/L, ⌘[ / ⌘], ⌘⇧↑/↓, a sidebar row) goes through
//! `App::switch_workspace`, which on this page neither changes page nor
//! resizes a primary PTY to its workspace tile; the card is scrolled into
//! view. Keys and paste reach the focused card's session
//! (`App::keyboard_session`). "Open →" and the right-click menu's **Go to
//! session** leave for the Sessions page with that group's primary focused.
//!
//! **Status** ([`status`]) is the primary tab's `unread` flag and nothing
//! else: *unread* or *read*. On this page a primary tab is read only by the
//! user picking it with the mouse — a left press on its card, anywhere on it,
//! the focused card included, or a click on its row in the sessions list
//! (`Workspace::mark_primary_read`). Opening the dashboard, being on
//! screen, being the focused card, keyboard focus moves and typing read
//! nothing, and no pane counts as watched here ([`attention_watched`]): an
//! attention signal (OSC 9) dots and stamps every primary, the focused
//! card's too. Going to the session reads it the way arriving on the
//! Sessions page always has.
//!
//! **Which cards** ([`cards`]): the dashboard follows the folders card. Its
//! cards are the **folder set** — the groups the sessions list shows for the
//! selected folder (`App::sidebar_rows`, i.e. `workspace::dashboard_groups`),
//! same membership, same order — and the All / Unread filter then hides
//! cards within it ([`shows`], live; the focused card always shows, so it
//! never vanishes under the user). Everything the page counts or sizes is
//! over the folder set: the subtitle, the grid's shape and the card size
//! (so the filter moves cards between slots but never resizes a PTY, while
//! picking another folder re-lays the grid and re-fits the set's PTYs —
//! `App::dashboard_set_changed`), focus moves, the scroll hint. Groups
//! outside the set keep whatever PTY size they had. When the active group
//! is outside the set no card is focused and keys reach no session
//! (`App::dashboard_focused`) until a click or a focus move — which enters
//! at the first card — focuses one.
//!
//! Nothing here is persisted, and the data in the footer is
//! the `App::git_contexts` snapshot the sidebar rows read: no fetches.

use std::time::SystemTime;

use gpui::{
    AnyElement, BoxShadow, Context, FontWeight, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Styled, Window, div, point, prelude::FluentBuilder as _, px,
};

use crate::App;
use crate::git_context::derive_rollup;
use crate::infobar_ui::{pr_state_word, repo_counts, repo_label};
use crate::pages::Page;
use crate::renderer::color;
use crate::sidebar_card::{CardAvatar, avatar_for, relative_time};
use crate::sidebar_ui::press;
use crate::term::{MouseBtn, Session};
use crate::tile_ui::StripStyle;
use crate::ui::assets::{ICON_ARROW_RIGHT, ICON_CLOCK, ICON_X};
use crate::ui::icon;
use crate::ui::theme::Theme;
use crate::workspace::{self, DashGrid, LayoutRect, NavDir, Tab, Workspace};

/// The header bar: the tab strip's paddings (6px above the 30px row, 4px
/// below, 10px at the sides), 8px between its children.
const BAR_PAD_TOP: f32 = 6.0;
const BAR_PAD_BOTTOM: f32 = 4.0;
const BAR_PAD_X: f32 = 10.0;
const BAR_GAP: f32 = 8.0;
/// "Dashboard" and the count beside it.
const BAR_TITLE_SIZE: f32 = 12.5;
const BAR_SUBTITLE_SIZE: f32 = 12.0;
/// The segmented filter: a 26px pill holding 20px segments.
const SEG_H: f32 = 26.0;
const SEG_PAD: f32 = 3.0;
const SEG_GAP: f32 = 2.0;
const SEG_ITEM_H: f32 = 20.0;
const SEG_ITEM_PAD_X: f32 = 9.0;
const SEG_TEXT_SIZE: f32 = 11.5;
/// The × that closes the page.
const CLOSE_GLYPH: f32 = 15.0;
/// A card's header: 10px side padding, 8px between its children.
const HEADER_PAD_X: f32 = 10.0;
const HEADER_GAP: f32 = 8.0;
/// The status dot and the halo ring around it.
const DOT: f32 = 7.0;
const DOT_RING: f32 = 3.0;
const TITLE_SIZE: f32 = 12.5;
/// `repo/branch`: its type, the share of the card it may take, and the card
/// width under which it is dropped.
const BRANCH_SIZE: f32 = 11.0;
const BRANCH_MAX_SHARE: f32 = 0.4;
const BRANCH_MIN_CARD_W: f32 = 300.0;
/// The status chip.
const CHIP_H: f32 = 20.0;
const CHIP_PAD_X: f32 = 8.0;
const CHIP_TEXT_SIZE: f32 = 11.0;
/// A card's footer: 10px side padding and gaps, 11px type and glyphs.
const FOOTER_PAD_X: f32 = 10.0;
const FOOTER_GAP: f32 = 10.0;
const FOOTER_TEXT_SIZE: f32 = 11.0;
const FOOTER_GLYPH: f32 = 11.0;
/// The mock's whites as alphas over the scheme ink (`StripStyle::pill_rgb`):
/// a card's resting and hovered border, the hairlines under the header and
/// over the footer, the filter's ground and its selected segment, the "read"
/// chip, and how far the read dot's dim ink is faded.
const BORDER: f32 = 0.08;
const BORDER_HOVER: f32 = 0.22;
const HAIRLINE: f32 = 0.06;
const SEG_GROUND: f32 = 0.06;
const SEG_SELECTED: f32 = 0.1;
const READ_CHIP: f32 = 0.06;
const READ_DOT: f32 = 0.6;
/// The unread colour's alphas: the halo ring around the dot, the chip's
/// ground, an unread card's border (its ground tint is the renderer's
/// `DASH_UNREAD_TINT`).
const UNREAD_HALO: f32 = 0.18;
const UNREAD_CHIP: f32 = 0.12;
const UNREAD_BORDER: f32 = 0.35;
/// The "↓ N more" pill's type (its rect is `workspace::dashboard_more_pill`).
const MORE_TEXT_SIZE: f32 = 11.5;

/// A card's status: whether its primary tab is unread. Nothing else feeds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// The primary tab asked for attention (OSC 9) and has not been read
    /// since.
    Unread,
    Read,
}

impl Status {
    /// The `state` spelling, and the status chip's text.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Status::Unread => "unread",
            Status::Read => "read",
        }
    }
}

/// A card's status from its primary tab's `unread` flag.
pub(crate) fn status(unread: bool) -> Status {
    if unread { Status::Unread } else { Status::Read }
}

/// The header's segmented filter. Session-only; every open of the dashboard
/// starts at [`Filter::All`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    All,
    Unread,
}

impl Filter {
    /// Segment order.
    pub(crate) const ALL: [Filter; 2] = [Filter::All, Filter::Unread];

    /// The `state` / `dashboard-filter` spelling.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Unread => "unread",
        }
    }

    /// Inverse of [`Filter::name`], forgiving about case and padding.
    pub(crate) fn from_name(name: &str) -> Option<Filter> {
        let name = name.trim().to_ascii_lowercase();
        Filter::ALL.into_iter().find(|f| f.name() == name)
    }

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Unread => "Unread",
        }
    }
}

/// Whether a card shows under `filter`: its status matches, or it is the
/// focused card — which always shows, so the card the user is typing into
/// never vanishes when a click reads it.
pub(crate) fn shows(filter: Filter, status: Status, focused: bool) -> bool {
    focused
        || match filter {
            Filter::All => true,
            Filter::Unread => status == Status::Unread,
        }
}

/// Whether a pane that just signalled for attention (OSC 9) is being watched
/// — in which case it is only stamped, not dotted. `on_screen` is "a visible
/// tab of the active group" (`App::is_visible`), `in_flyover` "the open
/// flyover's active tab".
///
/// Only the Sessions page watches its panes. On the dashboard none is — not
/// even the focused card's primary, however visible — so a signal there
/// always dots the tab, and only a click on the card reads it again.
pub(crate) fn attention_watched(page: Page, on_screen: bool, in_flyover: bool) -> bool {
    in_flyover || (page == Page::Sessions && on_screen)
}

/// One group of the folder set as the dashboard sees it this instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Card {
    /// Index into `App::workspaces`.
    pub group: usize,
    pub status: Status,
    /// The active group's card.
    pub focused: bool,
    /// Whether the filter shows it ([`shows`]).
    pub visible: bool,
}

/// The dashboard's cards: one per group of the **folder set**, in its order,
/// then the All / Unread filter over them.
///
/// `folder_set` is `workspace::dashboard_groups` — the groups the sessions
/// list shows for the folder selected in the folders card. A group outside
/// it gets no card whatever its state. Within it, `filter` hides cards
/// ([`shows`]) but the focused one — `active`'s card — is always kept. When
/// `active` is not in the folder set no card is focused: the caller then has
/// no keyboard target (`App::dashboard_focused`).
pub(crate) fn cards(
    workspaces: &[Workspace],
    folder_set: &[usize],
    active: usize,
    filter: Filter,
) -> Vec<Card> {
    folder_set
        .iter()
        .filter_map(|&group| {
            let ws = workspaces.get(group)?;
            let status = status(primary_tab(ws).is_some_and(|tab| tab.unread));
            let focused = group == active;
            Some(Card { group, status, focused, visible: shows(filter, status, focused) })
        })
        .collect()
}

/// The dashboard's geometry for the current frame (physical px): the pure
/// `workspace::dashboard_*` rects bundled with the cards and the scroll they
/// are laid out for.
pub(crate) struct Layout {
    pub bar: LayoutRect,
    pub viewport: LayoutRect,
    pub grid: DashGrid,
    /// The folder set's cards, in its (sessions list) order — showing or not.
    pub cards: Vec<Card>,
    /// The group in each occupied slot: the showing cards, in order.
    pub shown: Vec<usize>,
    /// The grid's scroll and its limit.
    pub scroll: f32,
    pub max_scroll: f32,
    pub scale: f32,
}

impl Layout {
    /// The (scrolled) card in `slot`.
    pub(crate) fn card(&self, slot: usize) -> LayoutRect {
        workspace::dashboard_card_rect(&self.viewport, &self.grid, slot, self.scroll, self.scale)
    }

    /// The slot `group`'s card occupies, if it is showing.
    pub(crate) fn slot_of(&self, group: usize) -> Option<usize> {
        self.shown.iter().position(|&g| g == group)
    }

    /// The showing card under a point: `(slot, group)`.
    pub(crate) fn hit(&self, px: f32, py: f32) -> Option<(usize, usize)> {
        workspace::dashboard_slot_at(
            &self.viewport,
            &self.grid,
            self.shown.len(),
            self.scroll,
            self.scale,
            px,
            py,
        )
        .map(|slot| (slot, self.shown[slot]))
    }

    /// The on-screen part of the terminal body of the card in `slot`.
    fn body_in_view(&self, slot: usize) -> LayoutRect {
        workspace::dashboard_card_body(&self.card(slot), self.scale).intersect(&self.viewport)
    }

    /// The showing cards, in slot order (`shown[slot]` is each one's group).
    pub(crate) fn shown_cards(&self) -> impl Iterator<Item = &Card> {
        self.cards.iter().filter(|card| card.visible)
    }

    /// The top scroll hint's counts: showing cards whose top edge lies above
    /// the viewport, and how many of those are unread.
    pub(crate) fn above(&self) -> (usize, usize) {
        let unread: Vec<bool> =
            self.shown_cards().map(|card| card.status == Status::Unread).collect();
        (
            workspace::dashboard_more_above(
                &self.viewport,
                &self.grid,
                unread.len(),
                self.scroll,
                self.scale,
            ),
            workspace::dashboard_unread_above(
                &self.viewport,
                &self.grid,
                &unread,
                self.scroll,
                self.scale,
            ),
        )
    }

    /// The bottom scroll hint's counts: showing cards whose bottom edge lies
    /// below the viewport, and how many of those are unread.
    pub(crate) fn below(&self) -> (usize, usize) {
        let unread: Vec<bool> =
            self.shown_cards().map(|card| card.status == Status::Unread).collect();
        (
            workspace::dashboard_more_below(
                &self.viewport,
                &self.grid,
                unread.len(),
                self.scroll,
                self.scale,
            ),
            workspace::dashboard_unread_below(
                &self.viewport,
                &self.grid,
                &unread,
                self.scroll,
                self.scale,
            ),
        )
    }
}

/// A group's primary pane tab — the one its card shows.
pub(crate) fn primary_tab(ws: &Workspace) -> Option<&Tab> {
    ws.root.find_tile(ws.primary_tile).and_then(|t| t.active_tab())
}

/// The inks one frame of the dashboard paints with: `StripStyle`'s scheme
/// inks (so a light terminal palette stays legible) and the colours the
/// sidebar and the info bar already define.
struct Inks {
    ink: Hsla,
    ink_dim: Hsla,
    /// The mock's white, as the scheme's pill colour.
    wash_rgb: (u8, u8, u8),
    /// The unread colour — the chrome accent, as the sidebar row's unread dot.
    unread: Hsla,
    red: Hsla,
    accent: Hsla,
    dark: bool,
}

impl Inks {
    fn wash(&self, alpha: f32) -> Hsla {
        color(self.wash_rgb, alpha)
    }
}

impl App {
    /// The dashboard's folder set: the groups the sessions list shows for the
    /// folder selected in the folders card — the very rows the sidebar paints
    /// (`App::sidebar_rows`), so the two cannot disagree — in that order.
    /// Empty in the empty state.
    pub(crate) fn dashboard_groups(&self) -> Vec<usize> {
        if self.is_empty_state() {
            return Vec::new();
        }
        workspace::dashboard_groups(
            &self.workspaces,
            &self.sections,
            self.folder_filter,
            self.pinned_collapsed,
            self.snoozed_collapsed,
        )
    }

    /// The folder set's cards, in its order ([`cards`]).
    pub(crate) fn dashboard_cards(&self) -> Vec<Card> {
        cards(&self.workspaces, &self.dashboard_groups(), self.active, self.dashboard_filter)
    }

    /// The group whose card is focused: the active group, while it is in the
    /// folder set. `None` otherwise — then no card is focused and the
    /// keyboard has no session to reach (`App::keyboard_session`), least of
    /// all the hidden active group's.
    pub(crate) fn dashboard_focused(&self) -> Option<usize> {
        self.dashboard_groups().contains(&self.active).then_some(self.active)
    }

    /// The folder set changed under the open dashboard — another folder was
    /// picked, or the list's "Pinned" / "Snoozed" run folded: re-fit the new
    /// set's primary PTYs to the re-shaped grid, clamp the scroll to the new
    /// content, and keep the focused card (if the set still has it) in view.
    /// A no-op off the Dashboard page.
    pub(crate) fn dashboard_set_changed(&mut self) {
        if self.page != Page::Dashboard {
            return;
        }
        self.sync_dashboard_layout(false);
        let layout = self.dashboard_layout();
        self.dashboard_scroll = layout.scroll / layout.scale;
        self.dashboard_reveal();
        self.request_redraw();
    }

    /// This frame's dashboard geometry. The grid is shaped by the folder
    /// set's count; the filter only picks which cards fill its slots.
    pub(crate) fn dashboard_layout(&self) -> Layout {
        let scale = self.scale();
        let area = self.area();
        let bar = workspace::dashboard_bar(&area, scale, self.sidebar_w());
        let viewport = workspace::dashboard_viewport(&area, scale);
        let cards = self.dashboard_cards();
        let grid = workspace::dashboard_grid(&viewport, cards.len(), scale);
        let shown: Vec<usize> = cards.iter().filter(|c| c.visible).map(|c| c.group).collect();
        let max_scroll = workspace::max_scroll(
            workspace::dashboard_content_h(&grid, shown.len(), scale),
            viewport.h,
        );
        // Stored in logical px and clamped on read, like the sidebar scrolls.
        let scroll = (self.dashboard_scroll * scale).clamp(0.0, max_scroll);
        Layout { bar, viewport, grid, cards, shown, scroll, max_scroll, scale }
    }

    /// `Action::ToggleDashboard`, the sidebar's grid chip and the bar's ×:
    /// Sessions ⇄ Dashboard (a tool page opens the dashboard).
    pub(crate) fn toggle_dashboard(&mut self) {
        let page = if self.page == Page::Dashboard { Page::Sessions } else { Page::Dashboard };
        self.set_page(page);
    }

    /// The page just became the dashboard (`App::set_page`): start from the
    /// unfiltered grid, fit every primary PTY to its card, and bring the
    /// active group's card — the focused one — into view. Opening reads
    /// nothing: an unread primary stays unread until its card is clicked.
    pub(crate) fn dashboard_opened(&mut self) {
        self.dashboard_filter = Filter::All;
        self.sync_dashboard_layout(false);
        self.dashboard_reveal();
    }

    /// Pick a filter segment (or `dashboard-filter` over the bus).
    pub(crate) fn set_dashboard_filter(&mut self, filter: Filter) {
        if self.dashboard_filter != filter {
            self.dashboard_filter = filter;
            // The slots just re-dealt: keep the focused card on screen.
            self.dashboard_reveal();
        }
        self.request_redraw();
    }

    /// The user picked `group` with the mouse — a left press on its card, or
    /// a click on its row in the sessions list: read its primary tab. The
    /// only ways a primary is read on this page (see the module docs).
    pub(crate) fn dashboard_mark_read(&mut self, group: usize) {
        if self.workspaces.get_mut(group).is_some_and(Workspace::mark_primary_read) {
            self.persist_snapshot();
            self.request_redraw();
        }
    }

    /// A click on the "↓ N more" pill: scroll the grid one row down.
    pub(crate) fn dashboard_scroll_more(&mut self) {
        let layout = self.dashboard_layout();
        let next = workspace::dashboard_more_scroll(
            &layout.grid,
            layout.scroll,
            layout.max_scroll,
            layout.scale,
        );
        self.dashboard_scroll = next / layout.scale;
        self.request_redraw();
    }

    /// A click on the "↑ N more" pill: scroll the grid one row up.
    pub(crate) fn dashboard_scroll_less(&mut self) {
        let layout = self.dashboard_layout();
        let next = workspace::dashboard_above_scroll(&layout.grid, layout.scroll, layout.scale);
        self.dashboard_scroll = next / layout.scale;
        self.request_redraw();
    }

    /// Scroll the grid the least it takes to show the focused card whole.
    pub(crate) fn dashboard_reveal(&mut self) {
        self.dashboard_revealed =
            (!self.is_empty_state()).then(|| self.workspaces[self.active].primary_tile);
        let layout = self.dashboard_layout();
        let Some(slot) = layout.slot_of(self.active) else { return };
        let scroll = workspace::dashboard_reveal_scroll(
            &layout.viewport,
            &layout.grid,
            slot,
            layout.scroll,
            layout.scale,
        );
        self.dashboard_scroll = scroll.clamp(0.0, layout.max_scroll) / layout.scale;
    }

    /// Follow the active group: when it is not the one the grid last
    /// revealed — a card press, a sidebar row, ⌘⇧↑/↓, a new or closed group —
    /// bring its card into view. Run from `App::sync_layout`, so a wheel
    /// scroll is not undone by unrelated layout passes.
    pub(crate) fn dashboard_follow_active(&mut self) {
        let active = (!self.is_empty_state()).then(|| self.workspaces[self.active].primary_tile);
        if active != self.dashboard_revealed {
            self.dashboard_reveal();
        }
    }

    /// ⇧⌘H/J/K/L on the dashboard: focus the neighbouring showing card.
    pub(crate) fn dashboard_step(&mut self, dir: NavDir) {
        let layout = self.dashboard_layout();
        if let Some(group) =
            workspace::dashboard_focus_dir(&layout.shown, self.active, layout.grid.cols, dir)
        {
            self.switch_workspace(group);
        }
    }

    /// ⌘[ / ⌘] on the dashboard: focus the previous / next showing card.
    pub(crate) fn dashboard_cycle(&mut self, delta: isize) {
        let layout = self.dashboard_layout();
        if let Some(group) = workspace::dashboard_focus_cycle(&layout.shown, self.active, delta) {
            self.switch_workspace(group);
        }
    }

    /// "Open →" and the context menu's "Go to session": make `group` active
    /// and show it on the Sessions page with its primary pane focused.
    pub(crate) fn go_to_session(&mut self, group: usize) {
        if self.is_empty_state() || group >= self.workspaces.len() {
            return;
        }
        // Switched while still on the dashboard, so the group being left is
        // not re-fitted to a workspace it is not going to show.
        self.switch_workspace(group);
        let ws = &mut self.workspaces[group];
        ws.focused_tile = ws.primary_tile;
        self.set_page(Page::Sessions);
    }

    /// The session of the card whose primary tile is `tile`.
    pub(crate) fn dashboard_session(&self, tile: u64) -> Option<&Session> {
        self.workspaces
            .iter()
            .find(|ws| ws.primary_tile == tile)
            .and_then(primary_tab)
            .and_then(Tab::session)
    }

    /// The terminal body rect of the card whose primary tile is `tile`, or
    /// `None` while that card is not showing.
    pub(crate) fn dashboard_body(&self, tile: u64) -> Option<LayoutRect> {
        let group = self.workspaces.iter().position(|ws| ws.primary_tile == tile)?;
        let layout = self.dashboard_layout();
        let slot = layout.slot_of(group)?;
        Some(workspace::dashboard_card_body(&layout.card(slot), layout.scale))
    }

    /// The card terminal under a point, for forwarded mouse reports.
    pub(crate) fn dashboard_pane_at(&self, px: f32, py: f32) -> Option<crate::MouseLoc> {
        let layout = self.dashboard_layout();
        let (slot, group) = layout.hit(px, py)?;
        layout
            .body_in_view(slot)
            .contains(px, py)
            .then(|| crate::MouseLoc::Card(self.workspaces[group].primary_tile))
    }

    /// A left press on the dashboard's canvas (the bar's controls, "Open" and
    /// the scroll hint are element targets that stop the press first): read
    /// and focus the card under it — reading it even when it already was the
    /// focused one — and inside its body hand the click to the terminal: a
    /// mouse report for a TUI that tracks the mouse, else the start of a
    /// selection.
    pub(crate) fn dashboard_mouse_down(&mut self, px: f32, py: f32) {
        let layout = self.dashboard_layout();
        let Some((slot, group)) = layout.hit(px, py) else { return };
        let before = layout.card(slot);
        self.dashboard_mark_read(group);
        self.switch_workspace(group);
        self.request_redraw();
        // Focusing can move the card — a reveal scroll, or a filtered grid
        // re-dealing its slots. The press then only focused it: the point no
        // longer names a cell of that terminal.
        let layout = self.dashboard_layout();
        let Some(slot) = layout.slot_of(group).filter(|&slot| layout.card(slot) == before) else {
            return;
        };
        if !layout.body_in_view(slot).contains(px, py) {
            return;
        }
        let tile = self.workspaces[group].primary_tile;
        let loc = crate::MouseLoc::Card(tile);
        if self.try_forward_press(loc, MouseBtn::Left, px, py) {
            return;
        }
        let body = workspace::dashboard_card_body(&before, layout.scale);
        if let Some((col, row)) = self.renderer.cell_at(&body, px, py)
            && let Some(session) = self.dashboard_session(tile)
        {
            session.begin_selection(col, row);
            self.drag = crate::Drag::CardSelect { tile };
        }
    }

    /// A right press anywhere on a card opens its native context menu, whose
    /// one item goes to that session. Like every other menu here, showing it
    /// changes no focus.
    pub(crate) fn dashboard_right_mouse_down(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        // The flyover slides over the grid: a press on it is not on a card.
        if self.flyover_open
            && !self.flyover_tabs.is_empty()
            && self.flyover_rect_now().contains(px, py)
        {
            return;
        }
        let Some(view) = crate::context_menu::ns_view(window) else { return };
        let layout = self.dashboard_layout();
        let Some((_, group)) = layout.hit(px, py) else { return };
        let items = vec![crate::context_menu::MenuItem {
            title: "Go to session".into(),
            enabled: true,
            separator_after: false,
            shortcut: None,
        }];
        let target = crate::MenuTarget::Card { tile: self.workspaces[group].primary_tile };
        let at = (px / layout.scale, py / layout.scale);
        self.show_context_menu(view, at, target, items, window, cx);
    }

    /// The wheel on the dashboard. Over the focused card's body — or any
    /// card's while the grid has nothing to scroll — it is that terminal's
    /// (scrollback, or the app's own wheel handling); anywhere else in the
    /// grid it scrolls the grid.
    pub(crate) fn dashboard_wheel(&mut self, delta: gpui::ScrollDelta, cell_height: f32) {
        let layout = self.dashboard_layout();
        let (px, py) = (self.cursor.0 as f32, self.cursor.1 as f32);
        if !layout.viewport.contains(px, py) {
            return;
        }
        let over = layout
            .hit(px, py)
            .filter(|&(slot, _)| layout.body_in_view(slot).contains(px, py))
            .filter(|&(_, group)| group == self.active || layout.max_scroll <= 0.0);
        let Some((slot, group)) = over else {
            let scroll = layout.scroll / layout.scale - Self::wheel_px(delta);
            let next = scroll.clamp(0.0, layout.max_scroll / layout.scale);
            if next != self.dashboard_scroll {
                self.dashboard_scroll = next;
                self.request_redraw();
            }
            return;
        };
        // Whole wheel steps, exactly as a tile's terminal takes them.
        let notches = match delta {
            gpui::ScrollDelta::Lines(p) => p.y as f64,
            gpui::ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / (cell_height as f64 * 3.0),
        };
        let steps = crate::scroll_steps(&mut self.scroll_accum, notches);
        if steps == 0 {
            return;
        }
        let body = workspace::dashboard_card_body(&layout.card(slot), layout.scale);
        if let Some(session) = primary_tab(&self.workspaces[group]).and_then(Tab::session) {
            if session.app_consumes_wheel() {
                let (col, row) = self.renderer.cell_at(&body, px, py).unwrap_or((0, 0));
                for _ in 0..steps.unsigned_abs() {
                    session.forward_wheel(steps > 0, col, row);
                }
            } else {
                session.scroll_by(steps * 3);
            }
        }
        self.request_redraw();
    }

    /// The dashboard's chrome over the canvas — the header bar and every
    /// showing card's header, footer and border — or an empty element off the
    /// Dashboard page.
    pub(crate) fn render_dashboard(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.page != Page::Dashboard {
            return div().into_any_element();
        }
        // The flyover slides over the bottom of the page; stop this layer
        // above it exactly as the tab strips do.
        let ceiling = self.flyover_ceiling();
        if ceiling == Some(0.0) {
            return div().into_any_element();
        }
        let layout = self.dashboard_layout();
        let inv = 1.0 / layout.scale;
        let ui = workspace::chrome_ui_scale();
        let dark = crate::theme::dark_active();
        let strip = StripStyle::from_scheme(crate::theme::current());
        let inks = Inks {
            ink: strip.ink,
            ink_dim: strip.ink_dim,
            wash_rgb: strip.pill_rgb,
            // The accent, as the sidebar row's unread dot paints it. The
            // focused card's border is the accent too, at full strength and
            // twice the width.
            unread: crate::sidebar_ui::accent(),
            red: crate::sidebar_ui::diff_removed(dark),
            accent: crate::sidebar_ui::accent(),
            dark,
        };
        // Hover in physical px, like the tab strips: none mid-drag or under
        // a modal.
        let cur = (matches!(self.drag, crate::Drag::None) && !self.modal_overlay_open())
            .then_some((self.cursor.0 as f32, self.cursor.1 as f32));
        let entity = cx.entity().downgrade();

        // ── Header bar ──────────────────────────────────────────────────
        let unread = layout.cards.iter().filter(|c| c.status == Status::Unread).count();
        let total = layout.cards.len();
        let subtitle = format!(
            "{total} primary {} · {unread} unread",
            if total == 1 { "pane" } else { "panes" }
        );
        let mut segments = div()
            .flex_shrink_0()
            .h(px(SEG_H * ui))
            .rounded(px(SEG_H * ui / 2.0))
            .bg(inks.wash(SEG_GROUND))
            .px(px(SEG_PAD * ui))
            .flex()
            .items_center()
            .gap(px(SEG_GAP * ui))
            .text_size(px(SEG_TEXT_SIZE * ui))
            .whitespace_nowrap();
        for filter in Filter::ALL {
            let selected = filter == self.dashboard_filter;
            segments = segments.child(
                div()
                    .h(px(SEG_ITEM_H * ui))
                    .px(px(SEG_ITEM_PAD_X * ui))
                    .rounded(px(SEG_ITEM_H * ui / 2.0))
                    .flex()
                    .items_center()
                    .text_color(if selected { inks.ink } else { inks.ink_dim })
                    .when(selected, |d| d.bg(inks.wash(SEG_SELECTED)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        press(entity.clone(), move |this, _ev, _cx| {
                            this.set_dashboard_filter(filter)
                        }),
                    )
                    .child(filter.label()),
            );
        }
        let close = div()
            .flex_shrink_0()
            .ml(px(4.0 * ui))
            .p(px(2.0 * ui))
            .rounded(px(6.0 * ui))
            .hover(|d| d.bg(color(strip.pill_rgb, SEG_SELECTED)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                press(entity.clone(), |this, _ev, _cx| this.toggle_dashboard()),
            )
            .child(icon(ICON_X, px(CLOSE_GLYPH * ui), inks.ink_dim));
        let bar = layout.bar;
        let bar_el = div()
            .absolute()
            .left(px(bar.x * inv))
            .top(px(bar.y * inv))
            .w(px(bar.w * inv))
            .h(px(bar.h * inv))
            .overflow_hidden()
            .pt(px(BAR_PAD_TOP * ui))
            .pb(px(BAR_PAD_BOTTOM * ui))
            .px(px(BAR_PAD_X * ui))
            .flex()
            .items_center()
            .gap(px(BAR_GAP * ui))
            .child(
                div()
                    .flex_shrink_0()
                    .pl(px(2.0 * ui))
                    .pr(px(4.0 * ui))
                    .text_size(px(BAR_TITLE_SIZE * ui))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(inks.ink)
                    .child("Dashboard"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(BAR_SUBTITLE_SIZE * ui))
                    .text_color(inks.ink_dim)
                    .child(subtitle),
            )
            .child(segments)
            .child(close);

        // ── Cards ───────────────────────────────────────────────────────
        let vp = layout.viewport;
        let mut grid_el = div()
            .absolute()
            .left(px(vp.x * inv))
            .top(px(vp.y * inv))
            .w(px(vp.w * inv))
            .h(px(vp.h * inv))
            .overflow_hidden();
        for (slot, card) in layout.shown_cards().enumerate() {
            let rect = layout.card(slot);
            // A card scrolled wholly out of the viewport costs nothing.
            if rect.intersect(&vp).h <= 0.0 {
                continue;
            }
            let hovered =
                cur.is_some_and(|(x, y)| vp.contains(x, y) && rect.contains(x, y));
            grid_el = grid_el.child(self.dashboard_card(
                card,
                &rect,
                &vp,
                layout.scale,
                &inks,
                hovered,
                entity.clone(),
            ));
        }
        if layout.shown.is_empty() {
            // `total` is the folder set's count: an empty folder, an app with
            // no sessions, or a filter nothing in the set passes.
            let hint = match (total, self.folder_filter) {
                (0, Some(_)) => "No sessions in this folder",
                (0, None) => "No sessions",
                _ => "No sessions match this filter",
            };
            grid_el = grid_el.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(BAR_TITLE_SIZE * ui))
                    .text_color(inks.ink_dim)
                    .child(hint),
            );
        }

        // ── Scroll hints ────────────────────────────────────────────────
        // Last in the viewport layer, so they float above the cards; opaque,
        // because the terminals under them are canvas paint they must cover.
        // One at the bottom while cards extend below the viewport, its mirror
        // at the top while cards are scrolled out above it. While some of the
        // cards a hint counts are unread it says so, in the unread style: the
        // cards' unread dot, "M unread" in the unread ink, the unread border.
        let (above, unread_above) = layout.above();
        let (below, unread_below) = layout.below();
        let hints = [
            (
                above,
                unread_above,
                workspace::dashboard_above_pill(&vp, above, unread_above, layout.scale),
                workspace::dashboard_above_parts(above, unread_above),
                true,
            ),
            (
                below,
                unread_below,
                workspace::dashboard_more_pill(&vp, below, unread_below, layout.scale),
                workspace::dashboard_more_parts(below, unread_below),
                false,
            ),
        ];
        let chrome = crate::theme::current();
        let theme = Theme::from_chrome(chrome);
        let ring = (DOT + 2.0 * DOT_RING) * ui;
        let shadow = |blur: f32, y: f32, alpha: f32| BoxShadow {
            color: color(chrome.shadow, alpha),
            offset: point(px(0.0), px(y * ui)),
            blur_radius: px(blur * ui),
            spread_radius: px(0.0),
            inset: false,
        };
        for (n, unread, pill, (more_text, unread_text), up) in hints {
            if n == 0 {
                continue;
            }
            let dot = unread_text.is_some().then(|| {
                div()
                    .flex_shrink_0()
                    .w(px(ring))
                    .h(px(ring))
                    .mr(px(workspace::DASH_MORE_DOT_SLOT * ui - ring))
                    .rounded_full()
                    .bg(inks.unread.opacity(UNREAD_HALO))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(DOT * ui)).h(px(DOT * ui)).rounded_full().bg(inks.unread))
            });
            let border = if unread > 0 { inks.unread.opacity(UNREAD_BORDER) } else { theme.border };
            grid_el = grid_el.child(
                div()
                    .absolute()
                    .left(px((pill.x - vp.x) * inv))
                    .top(px((pill.y - vp.y) * inv))
                    .w(px(pill.w * inv))
                    .h(px(pill.h * inv))
                    .rounded(px(pill.h * inv / 2.0))
                    // The theme's popover ground, forced opaque.
                    .bg(Hsla { a: 1.0, ..theme.popover })
                    .border_1()
                    .border_color(border)
                    .shadow(vec![shadow(10.0, 3.0, 0.28), shadow(2.0, 1.0, 0.18)])
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(MORE_TEXT_SIZE * ui))
                    .text_color(theme.popover_foreground)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        press(entity.clone(), move |this, _ev, _cx| {
                            if up {
                                this.dashboard_scroll_less()
                            } else {
                                this.dashboard_scroll_more()
                            }
                        }),
                    )
                    .children(dot)
                    .child(more_text)
                    // The label's " · M unread": the separator as a margined
                    // dot (a text run's edge spaces are not dependable), the
                    // count in the unread ink.
                    .children(unread_text.map(|text| {
                        div()
                            .flex()
                            .items_center()
                            .child(div().mx(px(5.0 * ui)).child("·"))
                            .child(div().text_color(inks.unread).child(text))
                    })),
            );
        }

        div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w_full()
            .overflow_hidden()
            .map(|d| match ceiling {
                Some(limit) => d.h(px(limit)),
                None => d.h_full(),
            })
            .font_family(crate::renderer::FONT_FAMILY)
            .child(bar_el)
            .child(grid_el)
            .into_any_element()
    }

    /// One card's chrome at `rect` (physical px, scrolled), positioned inside
    /// the viewport layer: the header, the footer, a placeholder when there
    /// is no terminal to paint, and the border over all of it. The ground and
    /// the terminal are on the canvas underneath (`Renderer::dashboard`).
    #[allow(clippy::too_many_arguments)]
    fn dashboard_card(
        &self,
        card: &Card,
        rect: &LayoutRect,
        viewport: &LayoutRect,
        scale: f32,
        inks: &Inks,
        hovered: bool,
        entity: gpui::WeakEntity<Self>,
    ) -> gpui::Div {
        let inv = 1.0 / scale;
        let ui = workspace::chrome_ui_scale();
        let ws = &self.workspaces[card.group];
        let tab = primary_tab(ws);
        // The same snapshot the sidebar row and the info bar read.
        let git = ws
            .cwd
            .as_deref()
            .and_then(|cwd| self.git_contexts.get(cwd))
            .filter(|c| c.is_git);
        let hairline = inks.wash(HAIRLINE);

        // ── Header ──────────────────────────────────────────────────────
        // The dot is the unread dot: lit with its halo while unread, dim and
        // bare once read. The chip says the same in words.
        let (dot, halo, chip_bg, chip_ink) = match card.status {
            Status::Unread => (
                inks.unread,
                Some(inks.unread.opacity(UNREAD_HALO)),
                inks.unread.opacity(UNREAD_CHIP),
                inks.unread,
            ),
            Status::Read => {
                (inks.ink_dim.opacity(READ_DOT), None, inks.wash(READ_CHIP), inks.ink_dim)
            },
        };
        let title = ws.title();
        let title = if title.is_empty() { "shell".to_string() } else { title };
        let card_w = rect.w * inv;
        let branch = (card_w >= BRANCH_MIN_CARD_W * ui)
            .then(|| git.and_then(repo_label))
            .flatten()
            .map(|(repo, head)| {
                let slash = repo.is_some() && head.is_some();
                div()
                    .flex_shrink(1.0)
                    .min_w(px(0.0))
                    .max_w(px(card_w * BRANCH_MAX_SHARE))
                    .ml(px(HEADER_GAP * ui))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .flex()
                    .items_center()
                    .text_size(px(BRANCH_SIZE * ui))
                    .children(
                        repo.map(|repo| div().flex_shrink_0().text_color(inks.ink_dim).child(repo)),
                    )
                    .when(slash, |d| {
                        d.child(div().flex_shrink_0().text_color(inks.ink_dim.opacity(0.6)).child("/"))
                    })
                    .children(head.map(|head| {
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .text_color(crate::sidebar_ui::pr_merged(inks.dark))
                            .child(head)
                    }))
            });
        let header = workspace::dashboard_card_header(rect, scale);
        let ring = (DOT + 2.0 * DOT_RING) * ui;
        let header_el = div()
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w_full()
            .h(px(header.h * inv))
            .border_b_1()
            .border_color(hairline)
            // The halo ring reaches into the side padding and the gap.
            .pl(px((HEADER_PAD_X - DOT_RING) * ui))
            .pr(px(HEADER_PAD_X * ui))
            .flex()
            .items_center()
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(ring))
                    .h(px(ring))
                    .rounded_full()
                    .when_some(halo, |d, halo| d.bg(halo))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(DOT * ui)).h(px(DOT * ui)).rounded_full().bg(dot)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .ml(px((HEADER_GAP - DOT_RING) * ui))
                    .truncate()
                    .text_size(px(TITLE_SIZE * ui))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(inks.ink)
                    .child(title),
            )
            .children(branch)
            .child(
                div()
                    .flex_shrink_0()
                    .ml(px(HEADER_GAP * ui))
                    .h(px(CHIP_H * ui))
                    .px(px(CHIP_PAD_X * ui))
                    .rounded(px(CHIP_H * ui / 2.0))
                    .bg(chip_bg)
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .text_size(px(CHIP_TEXT_SIZE * ui))
                    .text_color(chip_ink)
                    .child(card.status.name()),
            );

        // ── Body placeholder ────────────────────────────────────────────
        // A primary tab without a terminal (a webview — its native view
        // stays hidden on this page) has nothing for the canvas to paint.
        let placeholder = tab.and_then(Tab::session).is_none().then(|| {
            let body = workspace::dashboard_card_body(rect, scale);
            let label = tab.map(Tab::title).filter(|t| !t.is_empty());
            div()
                .absolute()
                .left(px(0.0))
                .top(px((body.y - rect.y) * inv))
                .w_full()
                .h(px(body.h * inv))
                .px(px(HEADER_PAD_X * ui))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(TITLE_SIZE * ui))
                        .text_color(inks.ink_dim)
                        .child(label.unwrap_or_else(|| "No terminal".to_string())),
                )
        });

        // ── Footer ──────────────────────────────────────────────────────
        let glyph = px(FOOTER_GLYPH * ui);
        let run = || div().flex_shrink_0().flex().items_center().gap(px(5.0 * ui));
        let stamp = ws.attention_at().map(|t| relative_time(t, SystemTime::now()));
        let pr = git.and_then(|c| c.pr.as_ref()).map(|pr| {
            let kind = avatar_for(derive_rollup(Some(pr)), pr.is_draft);
            let tint = match kind {
                CardAvatar::NoPr => crate::sidebar_ui::pr_none_ink(inks.dark),
                CardAvatar::Draft => inks.accent,
                CardAvatar::Open => crate::sidebar_ui::pr_open(inks.dark),
                CardAvatar::Merged => crate::sidebar_ui::pr_merged(inks.dark),
            };
            run()
                .child(icon(crate::sidebar_ui::avatar_icon(kind), glyph, tint))
                .child(format!("#{} {}", pr.number, pr_state_word(pr)))
        });
        let diff = git.and_then(|c| repo_counts(c).0).map(|(added, removed)| {
            run()
                .child(div().text_color(crate::sidebar_ui::diff_added(inks.dark)).child(added))
                .child(div().text_color(inks.red).child(removed))
        });
        let open_ink = inks.ink.opacity(0.8);
        let group = card.group;
        let open = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(4.0 * ui))
            .text_color(open_ink)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                press(entity, move |this, _ev, _cx| this.go_to_session(group)),
            )
            .child("Open")
            .child(icon(ICON_ARROW_RIGHT, glyph, open_ink));
        let footer = workspace::dashboard_card_footer(rect, scale);
        let footer_el = div()
            .absolute()
            .left(px(0.0))
            .top(px((footer.y - rect.y) * inv))
            .w_full()
            .h(px(footer.h * inv))
            .border_t_1()
            .border_color(hairline)
            .px(px(FOOTER_PAD_X * ui))
            .flex()
            .items_center()
            .gap(px(FOOTER_GAP * ui))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(px(FOOTER_TEXT_SIZE * ui))
            .text_color(inks.ink_dim)
            .children(
                stamp.map(|stamp| run().child(icon(ICON_CLOCK, glyph, inks.ink_dim)).child(stamp)),
            )
            .children(pr)
            .children(diff)
            .child(div().flex_1().min_w(px(0.0)))
            .child(open);

        // ── Border ──────────────────────────────────────────────────────
        // Its own layer over the card, so its width never shifts the header
        // or footer off the rects the canvas and the mouse path read.
        let radius = px(workspace::DASH_CARD_RADIUS * ui);
        let frame = div().absolute().left(px(0.0)).top(px(0.0)).size_full().rounded(radius);
        let frame = if card.focused {
            frame.border_2().border_color(inks.accent)
        } else {
            frame.border_1().border_color(match card.status {
                Status::Unread => inks.unread.opacity(UNREAD_BORDER),
                Status::Read if hovered => inks.wash(BORDER_HOVER),
                Status::Read => inks.wash(BORDER),
            })
        };

        div()
            .absolute()
            .left(px((rect.x - viewport.x) * inv))
            .top(px((rect.y - viewport.y) * inv))
            .w(px(rect.w * inv))
            .h(px(rect.h * inv))
            .rounded(radius)
            .overflow_hidden()
            .child(header_el)
            .children(placeholder)
            .child(footer_el)
            .child(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status is the unread flag, spelled "unread" / "read" in `state`
    /// and on the chip.
    #[test]
    fn status_is_the_unread_flag() {
        assert_eq!(status(true), Status::Unread);
        assert_eq!(status(false), Status::Read);
        assert_eq!([Status::Unread, Status::Read].map(Status::name), ["unread", "read"]);
    }

    /// All shows everything; Unread shows unread cards only — except the
    /// focused card, which every filter keeps.
    #[test]
    fn filter_membership_follows_status_and_keeps_the_focused_card() {
        for status in [Status::Unread, Status::Read] {
            assert!(shows(Filter::All, status, false));
            assert_eq!(shows(Filter::Unread, status, false), status == Status::Unread);
            for filter in Filter::ALL {
                assert!(shows(filter, status, true), "{filter:?} hid the focused {status:?} card");
            }
        }
    }

    /// The bus spellings round-trip, forgivingly; the removed filters are
    /// gone; a dashboard opens on All.
    #[test]
    fn filter_names_round_trip() {
        for filter in Filter::ALL {
            assert_eq!(Filter::from_name(filter.name()), Some(filter));
        }
        assert_eq!(Filter::from_name(" Unread "), Some(Filter::Unread));
        assert_eq!(Filter::from_name("ALL"), Some(Filter::All));
        for gone in ["working", "needs-you", "needs_you", "read", "done", ""] {
            assert_eq!(Filter::from_name(gone), None, "{gone:?}");
        }
        assert_eq!(Filter::default(), Filter::All);
        assert_eq!(Filter::ALL.map(Filter::name), ["all", "unread"]);
        assert_eq!(Filter::ALL.map(Filter::label), ["All", "Unread"]);
    }

    /// Seven groups: 0, 2, 3 and 5 in folder 1 (2 pinned, 5 snoozed), 1 in
    /// folder 2, 4 and 6 in none. The primaries of 0, 1, 4 and 5 are unread.
    fn folders() -> (Vec<Workspace>, Vec<workspace::Section>) {
        let section = |id: u64| workspace::Section {
            id,
            name: format!("folder{id}"),
            emoji: String::new(),
            collapsed: false,
            anchor: None,
        };
        let mut workspaces: Vec<Workspace> = [Some(1), Some(2), Some(1), Some(1), None, Some(1), None]
            .into_iter()
            .enumerate()
            .map(|(i, folder)| {
                let mut tile = workspace::Tile::new(i as u64 + 1, Session::placeholder());
                tile.tabs[0].unread = matches!(i, 0 | 1 | 4 | 5);
                let mut ws = Workspace::new(format!("g{i}"), tile, None);
                ws.section = folder;
                ws
            })
            .collect();
        workspaces[2].pinned = true;
        workspaces[5].snoozed = true;
        (workspaces, vec![section(1), section(2)])
    }

    /// Card membership: the folder set first — the sessions list's members
    /// in its order, nothing from outside it — then the unread filter within
    /// it, with the focused card kept. An active group outside the folder
    /// set focuses no card.
    #[test]
    fn cards_are_the_folder_set_then_the_unread_filter() {
        let (workspaces, sections) = folders();
        let set = |folder| workspace::dashboard_groups(&workspaces, &sections, folder, false, false);
        let groups = |cards: &[Card]| cards.iter().map(|c| c.group).collect::<Vec<_>>();
        let shown = |cards: &[Card]| {
            cards.iter().filter(|c| c.visible).map(|c| c.group).collect::<Vec<_>>()
        };
        let focused = |cards: &[Card]| {
            cards.iter().filter(|c| c.focused).map(|c| c.group).collect::<Vec<_>>()
        };

        // Folder 1 with its member 3 active: pinned first, snoozed last.
        let folder = set(Some(1));
        let all = cards(&workspaces, &folder, 3, Filter::All);
        assert_eq!(groups(&all), [2, 0, 3, 5]);
        assert_eq!(shown(&all), [2, 0, 3, 5]);
        assert_eq!(focused(&all), [3]);
        assert_eq!(
            all.iter().map(|c| c.status).collect::<Vec<_>>(),
            [Status::Read, Status::Unread, Status::Read, Status::Unread]
        );

        // Unread hides the read 2 but keeps the read, focused 3. The card
        // list — what sizes the grid — is still the whole folder set.
        let unread = cards(&workspaces, &folder, 3, Filter::Unread);
        assert_eq!(groups(&unread), [2, 0, 3, 5]);
        assert_eq!(shown(&unread), [0, 3, 5]);
        assert_eq!(focused(&unread), [3]);

        // The active group 1 is in another folder: it gets no card here,
        // unread though it is, and no card is focused.
        let outside = cards(&workspaces, &folder, 1, Filter::Unread);
        assert_eq!(groups(&outside), [2, 0, 3, 5]);
        assert_eq!(shown(&outside), [0, 5]);
        assert!(focused(&outside).is_empty());
        assert!(focused(&cards(&workspaces, &folder, 1, Filter::All)).is_empty());

        // All sessions: every group, in the list's order.
        let every = cards(&workspaces, &set(None), 1, Filter::Unread);
        assert_eq!(groups(&every), [2, 0, 1, 3, 4, 6, 5]);
        assert_eq!(shown(&every), [0, 1, 4, 5]);
        assert_eq!(focused(&every), [1]);

        // A folded "Pinned" run leaves the list, and so the dashboard.
        let folded = workspace::dashboard_groups(&workspaces, &sections, Some(1), true, false);
        assert_eq!(groups(&cards(&workspaces, &folded, 2, Filter::All)), [0, 3, 5]);
        assert!(focused(&cards(&workspaces, &folded, 2, Filter::All)).is_empty());

        // A folder nothing is in, and an index no group has: no cards.
        assert!(cards(&workspaces, &set(Some(9)), 0, Filter::All).is_empty());
        assert!(cards(&workspaces, &[99], 0, Filter::All).is_empty());
    }

    /// An attention signal finds no pane watched on the dashboard — not even
    /// one that is on screen as the focused card — so it always dots the tab.
    /// The Sessions page and the flyover keep their rule.
    #[test]
    fn no_pane_is_watched_on_the_dashboard() {
        for on_screen in [false, true] {
            assert!(!attention_watched(Page::Dashboard, on_screen, false));
            assert!(!attention_watched(Page::Tool(0), on_screen, false));
            assert_eq!(attention_watched(Page::Sessions, on_screen, false), on_screen);
            // The flyover's own tab is on screen whatever the page.
            for page in [Page::Sessions, Page::Dashboard, Page::Tool(0)] {
                assert!(attention_watched(page, on_screen, true));
            }
        }
    }
}
