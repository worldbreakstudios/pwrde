//! GPUI-owned browser chrome for the native Chromium (CEF) child views.
//!
//! The toolbar follows the GANTRY Workspace mock: a 48px bar on the pane
//! ground (8px above and below 32px controls, 10px at the sides, 8px between
//! them) over a hairline — a nav capsule (back, forward, reload as bare 26px
//! cells), a centred address pill capped at 420px, and a round More button
//! that opens the Tools popover. All three are the foreground at .07, with the
//! tab strip's inks. The address pill carries the lock / info glyph that
//! toggles the Site popover at its left edge, and shows the bare host at rest;
//! a press on the focused pane's pill swaps in the editable full URL. The
//! mock's Annotate and Share-with-agent controls are deliberately not built.
//!
//! Those sizes are the figures at the default chrome text size. The bar's
//! height ([`crate::webview::toolbar_h`]) and everything painted in it scale
//! with `appearance.font_size` through [`crate::workspace::chrome_ui_scale`]
//! — the factor the tab strips and the sidebar use. The Site and Tools popovers
//! hang under the scaled bar; their contents keep their own sizes.
//!
//! The two popovers are floating cards, also to the mock: Site (connection
//! header, cookies / permissions / certificate rows, clear data) centred under
//! the address pill, and Tools (find field, zoom stepper, open in browser,
//! copy link, print, developer tools, send to agent, clear data) hanging from
//! the More button's right edge. A native child view paints over everything
//! this window draws, so the cards live in their own chrome-less window
//! (`webview_popover_window`) and never reflow the page; this file owns the
//! model (`App::webview_panel`), the cards' element trees
//! (`App::webview_popover_card`), their fixed sizes and the pure anchor
//! geometry ([`anchor_rect`]), which shares the toolbar's layout constants.
//! Rows without a backend yet (cookies detail, permissions, certificate, send
//! to agent) are drawn and answer with a "not available yet" toast. The
//! shortcut hints are the live bindings of the matching `pages::Action`s,
//! which [`App::run_webview_action`] runs for the focused tile's webview tab.

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::App;
use crate::pages::Action;
use crate::renderer::color;
use crate::tile_ui::StripStyle;
use crate::ui::assets::{
    ICON_CHEVRON_RIGHT, ICON_CIRCLE_ELLIPSIS, ICON_CODE, ICON_COPY, ICON_ELLIPSIS,
    ICON_EXTERNAL_LINK, ICON_FILE, ICON_INFO, ICON_LOCK, ICON_PEN_LINE, ICON_PRINTER,
    ICON_REFRESH, ICON_SEARCH, ICON_SHIELD, ICON_TRASH, ICON_ZOOM_IN,
};
use crate::ui::icon;
use crate::ui::theme::Theme;
use crate::ui::{AlertDialog, AlertDialogFooter, Button, ButtonSize, ButtonVariant};
use crate::workspace::LayoutRect;

#[derive(Clone, Debug)]
pub(crate) enum Panel {
    Site {
        id: u64,
        cookies: Result<usize, String>,
    },
    Tools {
        id: u64,
    },
}

/// Which popover a [`Panel`] is, without its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelKind {
    Site,
    Tools,
}

impl Panel {
    pub(crate) fn id(&self) -> u64 {
        match self {
            Self::Site { id, .. } | Self::Tools { id } => *id,
        }
    }

    pub(crate) fn kind(&self) -> PanelKind {
        match self {
            Self::Site { .. } => PanelKind::Site,
            Self::Tools { .. } => PanelKind::Tools,
        }
    }
}

#[derive(Clone)]
struct ChromePlacement {
    id: u64,
    url: String,
    rect: crate::workspace::LayoutRect,
    focused: bool,
    can_go_back: bool,
    can_go_forward: bool,
    toolbar_hidden: bool,
}

/// The toolbar's controls — nav capsule, address pill, More — share one
/// height and are full pills of the foreground at the mock's .07. Sizes here
/// are at the default chrome text size; the painter multiplies by the chrome
/// factor.
const PILL_H: f32 = 32.0;
const PILL_FILL: f32 = 0.07;
/// A nav capsule cell, and the lock / info cell inside the address pill.
const NAV_CELL: f32 = 26.0;
const SITE_CELL: f32 = 22.0;
/// The bar's side padding and the gap between its three controls; the nav
/// capsule's own side padding and cell gap; and the address pill's cap and
/// floor. The bar is laid out from these and [`anchor_rect`] re-derives the
/// pill and More rects from them, so a popover cannot drift off its control.
const BAR_PAD_X: f32 = 10.0;
const BAR_GAP: f32 = 8.0;
const NAV_PAD_X: f32 = 6.0;
const NAV_GAP: f32 = 2.0;
const ADDRESS_MAX_W: f32 = 420.0;
const ADDRESS_MIN_W: f32 = 40.0;
/// The mock's control ink (`#c9c9ce`) and its dim while there is no history
/// to move through (`#5c5c62`), as shares of the strip ink.
const NAV_ENABLED: f32 = 0.8;
const NAV_DISABLED: f32 = 0.33;

/// The popover cards, to the mock: 6px of padding inside a 1px border, 32px
/// rows (34px on the Site card), a 52px Site header, 1px separators with 4px
/// above and below, the 32px find field (4px over, 6px under) and the zoom
/// row around its 26px stepper. Every block has a fixed height so the card's
/// size — and with it the popover window's — is known without measuring.
const CARD_PAD: f32 = 6.0;
const CARD_BORDER: f32 = 1.0;
const CARD_RADIUS: f32 = 12.0;
const SITE_CARD_W: f32 = 300.0;
const TOOLS_CARD_W: f32 = 272.0;
const ROW_H: f32 = 32.0;
const SITE_ROW_H: f32 = 34.0;
const SITE_HEADER_H: f32 = 52.0;
const SEPARATOR_H: f32 = 1.0;
const SEPARATOR_GAP: f32 = 4.0;
const FIND_H: f32 = 32.0;
const FIND_TOP: f32 = 4.0;
const FIND_BOTTOM: f32 = 6.0;
const ZOOM_ROW_H: f32 = 40.0;
const STEPPER_H: f32 = 26.0;
/// The mock's dimmest ink (`#5c5c62`: chevrons and shortcut hints), the row
/// hover wash and the separator, as shares of the popover foreground.
const HINT_INK: f32 = 0.36;
const ROW_HOVER: f32 = 0.06;
const SEPARATOR_INK: f32 = 0.08;
/// The mock's semantic colours: the secure green (disc at .15) and, for a
/// plain-http page, the system amber in the same treatment.
const SECURE_RGB: u32 = 0x30d158;
const INSECURE_RGB: u32 = 0xff9f0a;
const STATUS_DISC: f32 = 0.15;
/// A press on a popover's own toggle first takes key status from the popover
/// window, which dismisses it; the same press must then not reopen it.
const TOGGLE_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// The popover card's outer size in logical px, `(width, height)`.
pub(crate) fn card_size(kind: PanelKind) -> (f32, f32) {
    let frame = 2.0 * (CARD_PAD + CARD_BORDER);
    let separator = SEPARATOR_H + 2.0 * SEPARATOR_GAP;
    match kind {
        // Header, a flush hairline, three rows, a separator, the clear row.
        PanelKind::Site => (
            SITE_CARD_W,
            frame + SITE_HEADER_H + SEPARATOR_H + 3.0 * SITE_ROW_H + separator + SITE_ROW_H,
        ),
        // Find, zoom, then 3 + 2 + 1 rows in three separated groups.
        PanelKind::Tools => (
            TOOLS_CARD_W,
            frame
                + (FIND_TOP + FIND_H + FIND_BOTTOM)
                + ZOOM_ROW_H
                + 3.0 * separator
                + 6.0 * ROW_H,
        ),
    }
}

/// The control a popover hangs from — the address pill for Site, the More
/// button for Tools — in the same logical window coordinates as `content`
/// (a webview tile's content rect). With the title bar hidden there is no
/// control, so the anchor collapses to a zero-height rect on the content's
/// top edge and the card hangs from there. `ui` is the chrome factor the bar
/// is painted at.
pub(crate) fn anchor_rect(
    kind: PanelKind,
    content: LayoutRect,
    toolbar_hidden: bool,
    ui: f32,
) -> LayoutRect {
    let pill_h = PILL_H * ui;
    let more_x = content.x + content.w - BAR_PAD_X * ui - pill_h;
    let (x, w) = match kind {
        PanelKind::Tools => (more_x, pill_h),
        PanelKind::Site => {
            let nav_w = (2.0 * NAV_PAD_X + 3.0 * NAV_CELL + 2.0 * NAV_GAP) * ui;
            let left = content.x + (BAR_PAD_X + BAR_GAP) * ui + nav_w;
            let slot = (more_x - BAR_GAP * ui - left).max(ADDRESS_MIN_W * ui);
            let pill = slot.min(ADDRESS_MAX_W * ui);
            (left + (slot - pill) / 2.0, pill)
        },
    };
    if toolbar_hidden {
        LayoutRect { x, y: content.y, w, h: 0.0 }
    } else {
        let y = content.y + (crate::webview::TOOLBAR_H * ui - pill_h) / 2.0;
        LayoutRect { x, y, w, h: pill_h }
    }
}

/// What the address pill shows at rest: the URL's authority, without scheme,
/// path, query or fragment.
fn host(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(url)
        .to_string()
}

/// The Site card's header for `url`: whether the connection is secure, its
/// title, and the "<host> · <scheme>" subtitle. Only the scheme is known, so
/// no TLS version is claimed.
fn site_summary(url: &str) -> (bool, &'static str, String) {
    let secure = url.starts_with("https://");
    let scheme = url
        .split_once("://")
        .map_or_else(String::new, |(scheme, _)| scheme.to_uppercase());
    let subtitle = if scheme.is_empty() {
        host(url)
    } else {
        format!("{} · {scheme}", host(url))
    };
    let title = if secure { "Connection is secure" } else { "Connection is not secure" };
    (secure, title, subtitle)
}

/// The Site card's cookie value: the live count, or a dash when it could not
/// be read.
fn cookies_label(cookies: &Result<usize, String>) -> String {
    match cookies {
        Ok(count) => format!("{count} in use"),
        Err(_) => "—".to_string(),
    }
}

/// The toast a row without a backend answers with.
fn unavailable(feature: &str) -> String {
    format!("{feature} is not available yet")
}
/// One bare cell of the nav capsule: a centred icon in the control ink,
/// dimmed while disabled. `size` is the icon's and `ui` the chrome factor the
/// cell and icon scale by. The caller attaches the click.
fn nav_cell(
    id: String,
    path: impl Into<gpui::SharedString>,
    size: f32,
    ui: f32,
    enabled: bool,
    strip: &StripStyle,
) -> gpui::Stateful<gpui::Div> {
    let ink = strip.ink.opacity(if enabled { NAV_ENABLED } else { NAV_DISABLED });
    let hover = strip.ink.opacity(PILL_FILL);
    div()
        .id(gpui::SharedString::from(id))
        .flex_shrink_0()
        .w(px(NAV_CELL * ui))
        .h(px(NAV_CELL * ui))
        .rounded(px(NAV_CELL * ui / 2.0))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |cell| cell.cursor_pointer().hover(move |cell| cell.bg(hover)))
        .child(icon(path, px(size * ui), ink))
}

impl App {
    fn webview_tab_url(&self, id: u64) -> Option<String> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.root.tiles())
            .flat_map(|tile| tile.tabs.iter())
            .find(|tab| tab.webview_id() == Some(id))
            .and_then(|tab| tab.url().map(str::to_string))
    }

    /// Report a webview failure as a persistent sidebar notification.
    /// Returns whether it failed, so a caller can skip a focus it would
    /// only take back from the page it just refused to open.
    fn webview_error(&mut self, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => false,
            Err(error) => {
                self.toast_notification(error);
                true
            },
        }
    }

    fn focus_webview_tab(&mut self, id: u64) {
        let tile = self.workspaces[self.active]
            .root
            .tiles()
            .iter()
            .find_map(|tile| {
                tile.active_tab()
                    .is_some_and(|tab| tab.webview_id() == Some(id))
                    .then_some(tile.id)
            });
        if let Some(tile) = tile
            && self.workspaces[self.active].focused_tile != tile
        {
            self.workspaces[self.active].focused_tile = tile;
            self.mark_visible_read();
            self.request_redraw();
        }
    }

    fn navigate_webview(&mut self, id: u64, raw: &str) -> Result<(), String> {
        let url = crate::webview::normalize_input(raw)?;
        self.webviews.navigate(id, &url)?;
        if self.set_webview_url(id, url) {
            self.persist_snapshot();
        }
        Ok(())
    }

    /// The focused tile's active tab, when it is a webview.
    pub(crate) fn focused_webview_id(&self) -> Option<u64> {
        self.workspaces
            .get(self.active)?
            .focused()?
            .active_tab()?
            .webview_id()
    }

    /// Close whichever popover is up; the pump takes its window down.
    pub(crate) fn close_webview_panel(&mut self) {
        self.webview_panel = None;
        self.webview_find_for = None;
        self.request_redraw();
    }

    /// The popover window lost key status: dismiss the popover, remembering
    /// which one so the press that took the focus — if it landed on that
    /// popover's own toggle — closes it instead of reopening it.
    pub(crate) fn dismiss_webview_panel_on_blur(&mut self) {
        if let Some(panel) = self.webview_panel.take() {
            self.webview_panel_dismissed =
                Some((panel.kind(), panel.id(), std::time::Instant::now()));
            self.webview_find_for = None;
            self.request_redraw();
        }
    }

    fn toggle_webview_panel(&mut self, kind: PanelKind, id: u64) {
        let open = self
            .webview_panel
            .as_ref()
            .is_some_and(|panel| panel.kind() == kind && panel.id() == id);
        let just_dismissed = self
            .webview_panel_dismissed
            .take()
            .is_some_and(|(was, of, at)| was == kind && of == id && at.elapsed() < TOGGLE_GRACE);
        if open || just_dismissed {
            self.close_webview_panel();
            return;
        }
        self.webview_find_for = None;
        self.webview_panel = Some(match kind {
            PanelKind::Site => {
                let url = self.webview_tab_url(id).unwrap_or_default();
                Panel::Site { id, cookies: self.webviews.cookie_count(id, &url) }
            },
            PanelKind::Tools => Panel::Tools { id },
        });
        self.request_redraw();
    }

    /// The open popover's kind, webview and anchor rect (logical window
    /// coordinates), or `None` once it has nothing to hang from: its tab is
    /// no longer the focused tile's visible webview, or the chrome is covered
    /// (another page, a modal, the flyover, a drag). The pump closes the
    /// popover on `None`.
    pub(crate) fn webview_popover_anchor(&self) -> Option<(PanelKind, u64, LayoutRect)> {
        let panel = self.webview_panel.as_ref()?;
        let scale = self.scale();
        let placement = self
            .chrome_placements()
            .into_iter()
            .find(|placement| placement.id == panel.id() && placement.focused)?;
        let content = LayoutRect {
            x: placement.rect.x / scale,
            y: placement.rect.y / scale,
            w: placement.rect.w / scale,
            h: placement.rect.h / scale,
        };
        let kind = panel.kind();
        Some((kind, panel.id(), anchor_rect(
                kind,
                content,
                placement.toolbar_hidden,
                crate::workspace::chrome_ui_scale(),
            )))
    }

    /// The webview actions (`Action::FindInPage` … `Action::DeveloperTools`)
    /// for the focused tile's active tab. Returns false — a reported no-op —
    /// when that tab is a terminal, so terminal chords are left alone, or
    /// when its page is covered (another page, a modal, the flyover, Flow):
    /// a chord must not print or inspect a page the user cannot see.
    pub(crate) fn run_webview_action(&mut self, action: Action) -> bool {
        let Some(id) = self.focused_webview_id() else { return false };
        let visible = self
            .chrome_placements()
            .iter()
            .any(|placement| placement.id == id && placement.focused);
        visible && self.webview_action(id, action)
    }

    fn webview_action(&mut self, id: u64, action: Action) -> bool {
        match action {
            // Open (or keep) the Tools popover with its find field focused.
            Action::FindInPage => {
                if !matches!(self.webview_panel, Some(Panel::Tools { id: open }) if open == id) {
                    self.webview_panel = Some(Panel::Tools { id });
                }
                self.webview_find_for = Some(id);
                self.request_redraw();
                return true;
            },
            // Toggle the Site popover, as the address pill's lock glyph does.
            Action::SiteInfo => {
                self.toggle_webview_panel(PanelKind::Site, id);
                return true;
            },
            Action::OpenInBrowser => {
                if let Some(url) = self.webview_tab_url(id)
                    && let Err(error) = std::process::Command::new("open").arg(url).spawn()
                {
                    self.toast_notification(format!("open browser: {error}"));
                }
            },
            Action::CopyLink => {
                if let Some(url) = self.webview_tab_url(id)
                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                {
                    let _ = clipboard.set_text(url);
                }
            },
            Action::PrintPage => {
                let result = self.webviews.print(id);
                self.webview_error(result);
            },
            Action::DeveloperTools => {
                let result = self.webviews.open_devtools(id);
                self.webview_error(result);
            },
            _ => return false,
        }
        self.close_webview_panel();
        true
    }

    /// Keys pressed while the popover window is key. ⎋ closes it and hands
    /// the keyboard back to the page; ↩ in the find field searches; of the ⌘
    /// chords only the webview actions resolve — like a menu, the popover
    /// swallows the rest rather than leaking them to the tab underneath.
    pub(crate) fn webview_popover_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.webview_panel.as_ref().map(Panel::id) else { return };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform {
            if let Some(
                action @ (Action::FindInPage
                | Action::OpenInBrowser
                | Action::CopyLink
                | Action::PrintPage
                | Action::DeveloperTools
                | Action::SiteInfo),
            ) = crate::pages::match_action(keystroke)
            {
                self.webview_action(id, action);
            }
        } else if keystroke.key == "escape" {
            self.close_webview_panel();
            self.webviews.focus(id);
        } else {
            self.handle_webview_input_key(event, window, cx);
        }
        self.request_redraw();
    }

    fn confirm_clear_webview_data(&mut self, id: u64) {
        self.confirm = Some(crate::ConfirmClose {
            text: "Clear cookies, cache, local storage, and other website data for all webviews?"
                .into(),
            action: crate::ConfirmAction::ClearWebviewData { id },
        });
        self.request_redraw();
    }

    pub(crate) fn webview_input_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.webview_address
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
            || self
                .webview_find
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    /// Main-window focus upkeep: hand the keyboard back once the address
    /// field's pane is no longer showing, and drop the find claim once its
    /// Tools popover (where the find field lives and is focused) is gone.
    pub(crate) fn sync_webview_input_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placements = self.chrome_placements();
        let address_visible = self.webview_address_for.is_some_and(|id| {
            placements.iter().any(|placement| {
                placement.id == id && placement.focused && !placement.toolbar_hidden
            })
        });
        let find_visible = self.webview_find_for.is_some_and(|id| {
            matches!(self.webview_panel, Some(Panel::Tools { id: open }) if open == id)
        });
        let address_focused = self
            .webview_address
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if address_focused && !address_visible {
            window.focus(&self.focus_handle, cx);
        }
        if !find_visible {
            self.webview_find_for = None;
        }
    }

    pub(crate) fn handle_webview_input_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address_focused = self
            .webview_address
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let find_focused = self
            .webview_find
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        match event.keystroke.key.as_str() {
            "enter" if address_focused => {
                let id = self.webview_address_for;
                let value = self.webview_address.read(cx).text().to_string();
                if let Some(id) = id {
                    let result = self.navigate_webview(id, &value);
                    if !self.webview_error(result) {
                        window.focus(&self.focus_handle, cx);
                        self.webviews.focus(id);
                    }
                }
            }
            "escape" if address_focused => {
                if let Some(id) = self.webview_address_for {
                    if let Some(url) = self.webview_tab_url(id) {
                        self.webview_address
                            .update(cx, |input, cx| input.set_text(url, cx));
                    }
                    window.focus(&self.focus_handle, cx);
                    self.webviews.focus(id);
                }
            }
            "enter" if find_focused => {
                if let Some(id) = self.webview_find_for {
                    let query = self.webview_find.read(cx).text().to_string();
                    let result = self.webviews.find(id, &query);
                    let _ = self.webview_error(result);
                }
            }
            _ => {}
        }
    }

    fn chrome_placements(&self) -> Vec<ChromePlacement> {
        if self.page != crate::pages::Page::Sessions
            || self.modal_overlay_open()
            || self.flyover_anim > 0.0
            || self.flow.open
            || matches!(
                self.drag,
                crate::Drag::Tab { .. } | crate::Drag::Group { .. }
            )
        {
            return Vec::new();
        }
        let scale = self.scale();
        let workspace = &self.workspaces[self.active];
        let (tiles, _) = crate::workspace::layout_tiles(&workspace.root, self.area(), scale);
        tiles
            .into_iter()
            .filter_map(|(tile_id, rect)| {
                let tile = workspace.root.find_tile(tile_id)?;
                if tile.collapsed || tile.collapse_anim > 0.0 {
                    return None;
                }
                let tab = tile.active_tab()?;
                let id = tab.webview_id()?;
                let url = tab.url()?.to_string();
                let toolbar_hidden = tab.toolbar_hidden();
                let state = self.webviews.state(id);
                Some(ChromePlacement {
                    id,
                    url,
                    rect: crate::workspace::tile_content(&rect, scale),
                    focused: tile_id == workspace.focused_tile,
                    can_go_back: state.as_ref().is_some_and(|state| state.can_go_back),
                    can_go_forward: state.as_ref().is_some_and(|state| state.can_go_forward),
                    toolbar_hidden,
                })
            })
            .collect()
    }

    pub(crate) fn render_webview_chrome(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let placements = self.chrome_placements();
        if placements.is_empty() {
            return div().into_any_element();
        }
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();
        let scale = self.scale();
        // The tab strip's inks and the pane ground it sits on, so the bar
        // reads as one surface with the strip above it on any palette.
        let strip = StripStyle::from_scheme(crate::theme::current());
        let ground =
            color(crate::term_theme::resolved(crate::theme::dark_active()).colors.bg, 1.0);
        // Fills and the address text take the strip's ink too: the chrome
        // foreground can be the wrong polarity for a chosen terminal scheme.
        let pill = strip.ink.opacity(PILL_FILL);
        // One factor for the bar's height and everything in it, so the
        // controls stay centred in the scaled bar.
        let ui = crate::workspace::chrome_ui_scale();
        let pill_h = PILL_H * ui;
        let site_cell = SITE_CELL * ui;
        let text_size = 12.5 * ui;
        // Size set here rather than with the text, so a live text-size
        // change reaches a field that is already mounted.
        self.webview_address.update(cx, |input, _cx| {
            input.set_text_color(Some(strip.ink));
            input.set_text_size(Some(px(text_size)));
        });
        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();

        for placement in placements {
            let id = placement.id;
            let x = placement.rect.x / scale;
            let y = placement.rect.y / scale;
            let width = placement.rect.w / scale;
            let height = placement.rect.h / scale;
            let toolbar_height = if placement.toolbar_hidden {
                0.0
            } else {
                crate::webview::toolbar_h().min(height.max(0.0))
            };
            let address_focused = self
                .webview_address
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            if placement.focused
                && !placement.toolbar_hidden
                && (self.webview_address_for != Some(id)
                    || (!address_focused && self.webview_address.read(cx).text() != placement.url))
            {
                self.webview_address_for = Some(id);
                let url = placement.url.clone();
                self.webview_address.update(cx, |input, cx| input.set_text(url, cx));
            }

            // Only the focused pane's field can be mid-edit; every other pane,
            // and the focused one at rest, shows the bare host.
            let editing =
                placement.focused && address_focused && self.webview_address_for == Some(id);

            if !placement.toolbar_hidden {
                let back_entity = entity.clone();
                let back = nav_cell(
                    format!("webview-back-{id}"),
                    theme.icons.chevron_left(),
                    15.0,
                    ui,
                    placement.can_go_back,
                    &strip,
                )
                .when(placement.can_go_back, |cell| {
                    cell.on_click(move |_event: &ClickEvent, _window, app| {
                        if let Some(entity) = back_entity.upgrade() {
                            entity.update(app, |this, _cx| {
                                let result = this.webviews.go_back(id);
                                this.webview_error(result);
                            });
                        }
                    })
                });
                let forward_entity = entity.clone();
                let forward = nav_cell(
                    format!("webview-forward-{id}"),
                    theme.icons.chevron_right(),
                    15.0,
                    ui,
                    placement.can_go_forward,
                    &strip,
                )
                .when(placement.can_go_forward, |cell| {
                    cell.on_click(move |_event: &ClickEvent, _window, app| {
                        if let Some(entity) = forward_entity.upgrade() {
                            entity.update(app, |this, _cx| {
                                let result = this.webviews.go_forward(id);
                                this.webview_error(result);
                            });
                        }
                    })
                });
                let reload_entity = entity.clone();
                let reload =
                    nav_cell(format!("webview-reload-{id}"), ICON_REFRESH, 14.0, ui, true, &strip)
                        .on_click(move |_event: &ClickEvent, _window, app| {
                            if let Some(entity) = reload_entity.upgrade() {
                                entity.update(app, |this, _cx| {
                                    let result = this.webviews.reload(id);
                                    this.webview_error(result);
                                });
                            }
                        });
                // The nav capsule: one pill around three bare 26px cells.
                let nav = div()
                    .flex_shrink_0()
                    .h(px(pill_h))
                    .rounded(px(pill_h / 2.0))
                    .bg(pill)
                    .px(px(NAV_PAD_X * ui))
                    .flex()
                    .items_center()
                    .gap(px(NAV_GAP * ui))
                    .child(back)
                    .child(forward)
                    .child(reload);

                // The lock / info glyph at the pill's left edge toggles the
                // Site popover; the rest of the pill starts an edit.
                let site_entity = entity.clone();
                let site = div()
                    .id(gpui::SharedString::from(format!("webview-site-{id}")))
                    .flex_shrink_0()
                    .w(px(site_cell))
                    .h(px(site_cell))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon(
                        if placement.url.starts_with("https://") { ICON_LOCK } else { ICON_INFO },
                        px(13.0 * ui),
                        strip.ink_dim,
                    ))
                    // On the press, not the click: the press is what takes
                    // key status from an open popover (see `TOGGLE_GRACE`).
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        move |_event, _window, app: &mut GpuiApp| {
                            if let Some(entity) = site_entity.upgrade() {
                                entity.update(app, |this, _cx| {
                                    this.toggle_webview_panel(PanelKind::Site, id)
                                });
                            }
                        },
                    );
                let field = if editing {
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(self.webview_address.clone())
                } else {
                    // The trailing pad mirrors the glyph cell, so the host
                    // stays centred in the pill rather than in what is left.
                    let edit_entity = entity.clone();
                    let focused = placement.focused;
                    let url = placement.url.clone();
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .h_full()
                        .pr(px(site_cell))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor(gpui::CursorStyle::IBeam)
                        .child(div().max_w_full().truncate().child(host(&placement.url)))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            move |_event, window, app: &mut GpuiApp| {
                                // A press on an unfocused pane only focuses
                                // it (the bar's own handler): the native view
                                // takes key focus on that switch, so an edit
                                // begun in the same press would type into the
                                // page.
                                if !focused {
                                    return;
                                }
                                if let Some(entity) = edit_entity.upgrade() {
                                    entity.update(app, |this, cx| {
                                        this.webview_address_for = Some(id);
                                        let url = url.clone();
                                        this.webview_address
                                            .update(cx, |input, cx| input.set_text(url, cx));
                                        let handle = this.webview_address.read(cx).focus_handle(cx);
                                        window.focus(&handle, cx);
                                        this.request_redraw();
                                    });
                                }
                            },
                        )
                };
                let address = div()
                    .flex_1()
                    .min_w(px(ADDRESS_MIN_W * ui))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(ADDRESS_MAX_W * ui))
                            .h(px(pill_h))
                            .rounded(px(pill_h / 2.0))
                            .bg(pill)
                            .px(px(5.0 * ui))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .text_size(px(text_size))
                            .text_color(strip.ink)
                            .child(site)
                            .child(field),
                    );

                let tools_entity = entity.clone();
                let tools_open =
                    matches!(self.webview_panel, Some(Panel::Tools { id: open }) if open == id);
                let tools = div()
                    .id(gpui::SharedString::from(format!("webview-tools-{id}")))
                    .flex_shrink_0()
                    .w(px(pill_h))
                    .h(px(pill_h))
                    .rounded(px(pill_h / 2.0))
                    .bg(if tools_open { strip.ink.opacity(2.0 * PILL_FILL) } else { pill })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |cell| cell.bg(strip.ink.opacity(2.0 * PILL_FILL)))
                    .child(icon(ICON_ELLIPSIS, px(15.0 * ui), strip.ink.opacity(NAV_ENABLED)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        move |_event, _window, app: &mut GpuiApp| {
                            if let Some(entity) = tools_entity.upgrade() {
                                entity.update(app, |this, _cx| {
                                    this.toggle_webview_panel(PanelKind::Tools, id)
                                });
                            }
                        },
                    );

                // The bar sits straight on the pane ground, a hairline under it.
                let toolbar_entity = entity.clone();
                let toolbar = div()
                    .absolute()
                    .occlude()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .h(px(toolbar_height))
                    .overflow_hidden()
                    .px(px(BAR_PAD_X * ui))
                    .flex()
                    .items_center()
                    .gap(px(BAR_GAP * ui))
                    .bg(ground)
                    .border_b_1()
                    .border_color(strip.ink.opacity(0.06))
                    .font_family(crate::renderer::FONT_FAMILY)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        move |_event, _window, app: &mut GpuiApp| {
                            if let Some(entity) = toolbar_entity.upgrade() {
                                entity.update(app, |this, _cx| this.focus_webview_tab(id));
                            }
                        },
                    )
                    .child(nav)
                    .child(address)
                    .child(tools);
                layer = layer.child(toolbar);
            }
        }
        layer.into_any_element()
    }

    /// The open popover's card, for the popover window to render: the whole
    /// window is this one element, [`card_size`] big.
    pub(crate) fn webview_popover_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(panel) = self.webview_panel.clone() else {
            return div().into_any_element();
        };
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();
        let id = panel.id();
        let (width, height) = card_size(panel.kind());
        let body = match panel {
            Panel::Site { cookies, .. } => {
                // Chromium counts asynchronously: prefer a count that landed
                // since the popover opened.
                let cookies = self.webviews.cookies_seen(id).map(Ok).unwrap_or(cookies);
                self.site_card(id, &cookies, &theme, &entity)
            },
            Panel::Tools { .. } => self.tools_card(id, &theme, &entity, window, cx),
        };
        body.w(px(width))
            .h(px(height))
            .p(px(CARD_PAD))
            .rounded(px(CARD_RADIUS))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .flex()
            .flex_col()
            // No family: the window's default is the system UI font, the
            // mock's (and the Settings window's) rather than the chrome mono.
            .text_size(px(12.5))
            .text_color(theme.popover_foreground)
            .into_any_element()
    }

    fn site_card(
        &self,
        id: u64,
        cookies: &Result<usize, String>,
        theme: &Theme,
        entity: &gpui::WeakEntity<App>,
    ) -> gpui::Div {
        let url = self.webview_tab_url(id).unwrap_or_default();
        let (secure, title, subtitle) = site_summary(&url);
        let status: gpui::Hsla =
            gpui::rgb(if secure { SECURE_RGB } else { INSECURE_RGB }).into();
        let header = div()
            .flex_shrink_0()
            .h(px(SITE_HEADER_H))
            .px(px(10.0))
            .pt(px(10.0))
            .pb(px(12.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(30.0))
                    .h(px(30.0))
                    .rounded(px(15.0))
                    .bg(status.opacity(STATUS_DISC))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(if secure { ICON_LOCK } else { ICON_INFO }, px(14.0), status)),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(subtitle),
                    ),
            );
        // The three detail rows have no backend yet: drawn to the mock, they
        // answer with a toast. Only the cookie count is live.
        let detail = |key: &str, glyph: &'static str, label: &'static str, value: String| {
            popover_row(format!("webview-site-{key}-{id}"), glyph, label, SITE_ROW_H, false, theme)
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground)
                        .child(value),
                )
                .child(icon(
                    ICON_CHEVRON_RIGHT,
                    px(12.0),
                    theme.popover_foreground.opacity(HINT_INK),
                ))
                .on_click(on_app(entity, move |this| {
                    this.toast_notification(unavailable(label));
                    this.close_webview_panel();
                }))
        };
        div()
            .child(header)
            .child(separator(theme, 0.0))
            .child(detail(
                "cookies",
                ICON_CIRCLE_ELLIPSIS,
                "Cookies and site data",
                cookies_label(cookies),
            ))
            .child(detail("permissions", ICON_SHIELD, "Permissions", "Default".into()))
            .child(detail(
                "certificate",
                ICON_FILE,
                "Certificate",
                if secure { "Valid" } else { "None" }.into(),
            ))
            .child(separator(theme, SEPARATOR_GAP))
            .child(
                popover_row(
                    format!("webview-site-clear-{id}"),
                    ICON_TRASH,
                    "Clear data for this site…",
                    SITE_ROW_H,
                    true,
                    theme,
                )
                .on_click(on_app(entity, move |this| {
                    this.close_webview_panel();
                    this.confirm_clear_webview_data(id);
                })),
            )
    }

    fn tools_card(
        &mut self,
        id: u64,
        theme: &Theme,
        entity: &gpui::WeakEntity<App>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let ink = theme.popover_foreground;
        let wash = ink.opacity(ROW_HOVER);
        self.webview_find.update(cx, |input, _cx| {
            input.set_text_size(Some(px(12.5)));
            input.set_text_color(Some(ink));
        });
        // ⌘F (or a press on the field) claims the find field for this
        // webview; the claim holds the keyboard there while the card is up.
        let find_focus = self.webview_find.read(cx).focus_handle(cx);
        if self.webview_find_for == Some(id) && !find_focus.is_focused(window) {
            window.focus(&find_focus, cx);
        }
        let find_entity = entity.clone();
        let find = div()
            .flex_shrink_0()
            .h(px(FIND_H))
            .mx(px(4.0))
            .mt(px(FIND_TOP))
            .mb(px(FIND_BOTTOM))
            .px(px(10.0))
            .rounded(px(8.0))
            .bg(wash)
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(icon(ICON_SEARCH, px(13.0), theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .child(self.webview_find.clone()),
            )
            .child(hint(Action::FindInPage, theme))
            .on_mouse_down(
                gpui::MouseButton::Left,
                move |_event, window, app: &mut GpuiApp| {
                    if let Some(entity) = find_entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.webview_find_for = Some(id);
                            window.focus(&this.webview_find.read(cx).focus_handle(cx), cx);
                        });
                    }
                },
            );

        // The stepper keeps the card open: − and + step by 10%, and a press
        // on the percentage resets to 100%.
        let zoom = self.webviews.state(id).map_or(100, |state| state.zoom_percent);
        let step = |key: &str, glyph: &'static str, delta: f64| {
            div()
                .id(gpui::SharedString::from(format!("webview-zoom-{key}-{id}")))
                .flex_shrink_0()
                .w(px(STEPPER_H))
                .h(px(STEPPER_H))
                .flex()
                .items_center()
                .justify_center()
                .text_color(ink.opacity(NAV_ENABLED))
                .cursor_pointer()
                .hover(move |cell| cell.bg(ink.opacity(SEPARATOR_INK)))
                .child(glyph)
                .on_click(on_app(entity, move |this| {
                    let result = this.webviews.zoom(id, delta).map(|_| ());
                    this.webview_error(result);
                }))
        };
        let stepper = div()
            .flex_shrink_0()
            .h(px(STEPPER_H))
            .rounded(px(8.0))
            .bg(wash)
            .overflow_hidden()
            .flex()
            .items_center()
            .child(step("out", "−", -0.1))
            .child(
                div()
                    .id(gpui::SharedString::from(format!("webview-zoom-reset-{id}")))
                    .min_w(px(44.0))
                    .h(px(STEPPER_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .child(format!("{zoom}%"))
                    .on_click(on_app(entity, move |this| {
                        let result = this.webviews.set_zoom(id, 1.0).map(|_| ());
                        this.webview_error(result);
                    })),
            )
            .child(step("in", "+", 0.1));
        let zoom_row = div()
            .flex_shrink_0()
            .h(px(ZOOM_ROW_H))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(icon(ICON_ZOOM_IN, px(14.0), theme.muted_foreground))
            .child(div().flex_1().child("Zoom"))
            .child(stepper);

        // A row that runs one of the webview actions (and so closes the
        // card), with that action's live chord as its hint.
        let action_row = |glyph: &'static str, action: Action| {
            popover_row(
                format!("webview-{}-{id}", action.name()),
                glyph,
                action.label(),
                ROW_H,
                false,
                theme,
            )
            .child(hint(action, theme))
            .on_click(on_app(entity, move |this| {
                this.webview_action(id, action);
            }))
        };
        div()
            .child(find)
            .child(zoom_row)
            .child(separator(theme, SEPARATOR_GAP))
            .child(action_row(ICON_EXTERNAL_LINK, Action::OpenInBrowser))
            .child(action_row(ICON_COPY, Action::CopyLink))
            .child(action_row(ICON_PRINTER, Action::PrintPage))
            .child(separator(theme, SEPARATOR_GAP))
            .child(action_row(ICON_CODE, Action::DeveloperTools))
            .child(
                popover_row(
                    format!("webview-agent-{id}"),
                    ICON_PEN_LINE,
                    "Send page to agent",
                    ROW_H,
                    false,
                    theme,
                )
                .on_click(on_app(entity, move |this| {
                    this.toast_notification(unavailable("Send page to agent"));
                    this.close_webview_panel();
                })),
            )
            .child(separator(theme, SEPARATOR_GAP))
            .child(
                popover_row(
                    format!("webview-clear-{id}"),
                    ICON_TRASH,
                    "Clear browsing data…",
                    ROW_H,
                    true,
                    theme,
                )
                .on_click(on_app(entity, move |this| {
                    this.close_webview_panel();
                    this.confirm_clear_webview_data(id);
                })),
            )
    }

    pub(crate) fn render_new_webview_prompt(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(prompt) = self.webview_prompt.as_ref() else {
            return div().into_any_element();
        };
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let entity = cx.entity().downgrade();
        let cancel_entity = entity.clone();
        let open_entity = entity.clone();
        let backdrop_entity = entity.clone();
        crate::modal_ui::panel_style(AlertDialog::new("new-webview").open(true), &theme, chrome)
            .w(px(520.0))
            .scrim(crate::renderer::color(chrome.scrim, 0.30))
            .on_backdrop_click(move |_event, _window, app| {
                if let Some(entity) = backdrop_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.webview_prompt = None;
                        this.request_redraw();
                        cx.notify();
                    });
                }
            })
            .font_family(crate::renderer::FONT_FAMILY)
            .text_color(theme.foreground)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(match prompt.mode {
                        crate::WebviewPromptMode::Url => "New webview",
                        crate::WebviewPromptMode::Command => "New webview from command",
                    }),
            )
            .child(
                div()
                    .h(px(38.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.foreground.opacity(0.05))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .child(self.modal_search.clone()),
            )
            .when_some(prompt.error.clone(), |dialog, error| {
                dialog.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.destructive)
                        .child(error),
                )
            })
            .child(
                AlertDialogFooter::new()
                    .gap(px(8.0))
                    .child(
                        Button::new("new-webview-cancel")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .child("Cancel")
                            .on_click(
                                move |_event: &ClickEvent,
                                      _window: &mut Window,
                                      app: &mut GpuiApp| {
                                    if let Some(entity) = cancel_entity.upgrade() {
                                        entity.update(app, |this, _cx| {
                                            this.webview_prompt = None;
                                            this.request_redraw();
                                        });
                                    }
                                },
                            ),
                    )
                    .child(
                        Button::new("new-webview-open")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Sm)
                            .child("Open")
                            .on_click(
                                move |_event: &ClickEvent,
                                      _window: &mut Window,
                                      app: &mut GpuiApp| {
                                    if let Some(entity) = open_entity.upgrade() {
                                        entity.update(app, |this, _cx| {
                                            this.submit_new_webview_prompt()
                                        });
                                    }
                                },
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// One popover row: a 14px glyph and a label that takes the slack, so the
/// caller's trailing value / chevron / hint sits at the right edge. A
/// destructive row is the theme's destructive ink over a wash of it.
fn popover_row(
    id: String,
    glyph: &'static str,
    label: &'static str,
    height: f32,
    destructive: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (ink, glyph_ink, hover) = if destructive {
        (theme.destructive, theme.destructive, theme.destructive.opacity(SEPARATOR_INK))
    } else {
        (
            theme.popover_foreground,
            theme.muted_foreground,
            theme.popover_foreground.opacity(ROW_HOVER),
        )
    };
    div()
        .id(gpui::SharedString::from(id))
        .flex_shrink_0()
        .h(px(height))
        .px(px(10.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .text_color(ink)
        .cursor_pointer()
        .hover(move |row| row.bg(hover))
        .child(icon(glyph, px(14.0), glyph_ink))
        .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
}

/// A row's shortcut hint: the action's live binding, formatted as Settings →
/// Keyboard shows it.
fn hint(action: Action, theme: &Theme) -> gpui::Div {
    div()
        .flex_shrink_0()
        .text_size(px(11.0))
        .text_color(theme.popover_foreground.opacity(HINT_INK))
        .child(action.binding().display())
}

/// The hairline between row groups, `gap` px clear above and below.
fn separator(theme: &Theme, gap: f32) -> gpui::Div {
    div()
        .flex_shrink_0()
        .h(px(SEPARATOR_H))
        .mx(px(4.0))
        .my(px(gap))
        .bg(theme.popover_foreground.opacity(SEPARATOR_INK))
}

/// A popover click handler: run `f` on the app and repaint both windows.
fn on_app(
    entity: &gpui::WeakEntity<App>,
    f: impl Fn(&mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut GpuiApp) + 'static {
    let entity = entity.clone();
    move |_event, _window, app| {
        if let Some(entity) = entity.upgrade() {
            entity.update(app, |this, cx| {
                f(this);
                this.request_redraw();
                cx.notify();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> LayoutRect {
        LayoutRect { x: 100.0, y: 50.0, w: 1000.0, h: 600.0 }
    }

    #[test]
    fn host_strips_scheme_and_path() {
        assert_eq!(host("https://example.com/docs"), "example.com");
        assert_eq!(host("https://github.com"), "github.com");
        assert_eq!(host("http://localhost:3000/a?b=c"), "localhost:3000");
        assert_eq!(host("https://example.com?q=1"), "example.com");
        assert_eq!(host("https://example.com#top"), "example.com");
        assert_eq!(host("example.com/docs"), "example.com");
    }

    #[test]
    fn card_sizes_are_the_mocks_widths_over_the_summed_blocks() {
        // Site: 14 frame + 52 header + 1 hairline + 3×34 + 9 separator + 34.
        assert_eq!(card_size(PanelKind::Site), (300.0, 212.0));
        // Tools: 14 frame + 42 find + 40 zoom + 3×9 separators + 6×32 rows.
        assert_eq!(card_size(PanelKind::Tools), (272.0, 315.0));
    }

    #[test]
    fn tools_anchor_is_the_more_button() {
        // 10px in from the content's right edge, centred in the 48px bar.
        assert_eq!(
            anchor_rect(PanelKind::Tools, content(), false, 1.0),
            LayoutRect { x: 1058.0, y: 58.0, w: 32.0, h: 32.0 }
        );
    }

    #[test]
    fn site_anchor_is_the_capped_pill_centred_in_its_slot() {
        // The slot runs from the nav capsule (10 + 94 + 8) to 8px short of
        // More: 212..1050. The pill caps at 420 and centres in it.
        assert_eq!(
            anchor_rect(PanelKind::Site, content(), false, 1.0),
            LayoutRect { x: 421.0, y: 58.0, w: 420.0, h: 32.0 }
        );
        // A narrow tile: the pill fills its 138px slot.
        let narrow = LayoutRect { w: 300.0, ..content() };
        assert_eq!(
            anchor_rect(PanelKind::Site, narrow, false, 1.0),
            LayoutRect { x: 212.0, y: 58.0, w: 138.0, h: 32.0 }
        );
    }

    #[test]
    fn a_hidden_title_bar_anchors_on_the_contents_top_edge() {
        let anchor = anchor_rect(PanelKind::Tools, content(), true, 1.0);
        assert_eq!(anchor, LayoutRect { x: 1058.0, y: 50.0, w: 32.0, h: 0.0 });
    }

    #[test]
    fn site_summary_names_the_scheme_and_never_a_tls_version() {
        assert_eq!(
            site_summary("https://github.com/zed"),
            (true, "Connection is secure", "github.com · HTTPS".to_string())
        );
        assert_eq!(
            site_summary("http://localhost:3000/a"),
            (false, "Connection is not secure", "localhost:3000 · HTTP".to_string())
        );
        // No scheme at all: still not secure, and nothing invented after it.
        assert_eq!(
            site_summary("example.com"),
            (false, "Connection is not secure", "example.com".to_string())
        );
    }

    #[test]
    fn cookies_label_reports_the_live_count() {
        assert_eq!(cookies_label(&Ok(5)), "5 in use");
        assert_eq!(cookies_label(&Ok(0)), "0 in use");
        assert_eq!(cookies_label(&Err("no cookie store".into())), "—");
    }

    #[test]
    fn stub_rows_say_the_feature_is_not_available_yet() {
        assert_eq!(unavailable("Permissions"), "Permissions is not available yet");
    }
}
