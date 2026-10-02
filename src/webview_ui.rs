//! GPUI-owned browser chrome for native Wry child views.
//!
//! The toolbar follows the GANTRY Workspace mock: a 48px bar on the pane
//! ground (8px above and below 32px controls, 10px at the sides, 8px between
//! them) over a hairline — a nav capsule (back, forward, reload as bare 26px
//! cells), a centred address pill capped at 420px, and a round More button
//! that opens the tools panel. All three are the foreground at .07, with the
//! tab strip's inks. The address pill carries the lock / info glyph that
//! toggles the site panel at its left edge, and shows the bare host at rest;
//! a press on the focused pane's pill swaps in the editable full URL. The
//! mock's Annotate and Share-with-agent controls are deliberately not built.
//!
//! Those sizes are the figures at the default chrome text size. The bar's
//! height ([`crate::webview::toolbar_h`]) and everything painted in it scale
//! with `appearance.font_size` through [`crate::workspace::chrome_ui_scale`]
//! — the factor the tab strips and the sidebar use. The Site and Tools panels
//! hang under the scaled bar; their contents keep their own sizes.

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::App;
use crate::renderer::color;
use crate::tile_ui::StripStyle;
use crate::ui::theme::Theme;
use crate::ui::assets::{ICON_ELLIPSIS, ICON_INFO, ICON_LOCK, ICON_REFRESH};
use crate::ui::icon;
use crate::ui::{AlertDialog, AlertDialogFooter, Button, ButtonSize, ButtonVariant};

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

impl Panel {
    pub(crate) fn id(&self) -> u64 {
        match self {
            Self::Site { id, .. } | Self::Tools { id } => *id,
        }
    }

    fn height(&self) -> f32 {
        match self {
            Self::Site { .. } => crate::webview::SITE_PANEL_H,
            Self::Tools { .. } => crate::webview::TOOLS_PANEL_H,
        }
    }
}

pub(crate) fn panel_height(panel: Option<&Panel>, id: u64) -> f32 {
    panel
        .filter(|panel| panel.id() == id)
        .map_or(0.0, Panel::height)
}

#[derive(Clone)]
struct ChromePlacement {
    id: u64,
    url: String,
    rect: crate::workspace::LayoutRect,
    focused: bool,
    can_go_back: bool,
    can_go_forward: bool,
    zoom_percent: u16,
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
/// The mock's control ink (`#c9c9ce`) and its dim while there is no history
/// to move through (`#5c5c62`), as shares of the strip ink.
const NAV_ENABLED: f32 = 0.8;
const NAV_DISABLED: f32 = 0.33;

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

    fn toggle_webview_site_panel(&mut self, id: u64) {
        if matches!(self.webview_panel.as_ref(), Some(Panel::Site { id: open, .. }) if *open == id)
        {
            self.webview_panel = None;
        } else {
            let url = self.webview_tab_url(id).unwrap_or_default();
            let cookies = self.webviews.cookie_count(id, &url);
            self.webview_panel = Some(Panel::Site { id, cookies });
        }
        self.request_redraw();
    }

    fn toggle_webview_tools_panel(&mut self, id: u64) {
        if matches!(self.webview_panel.as_ref(), Some(Panel::Tools { id: open }) if *open == id) {
            self.webview_panel = None;
            self.webview_find_for = None;
        } else {
            self.webview_panel = Some(Panel::Tools { id });
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

    pub(crate) fn sync_webview_input_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placements = self.chrome_placements();
        let address_visible = self.webview_address_for.is_some_and(|id| {
            placements.iter().any(|placement| {
                placement.id == id && placement.focused && !placement.toolbar_hidden
            })
        });
        let find_visible = self.webview_find_for.is_some_and(|id| {
            placements.iter().any(|placement| placement.id == id)
                && matches!(self.webview_panel, Some(Panel::Tools { id: open }) if open == id)
        });
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
        if (address_focused && !address_visible) || (find_focused && !find_visible) {
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
            "escape" if find_focused => {
                if let Some(id) = self.webview_find_for.take() {
                    window.focus(&self.focus_handle, cx);
                    self.webviews.focus(id);
                }
                self.request_redraw();
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
                    zoom_percent: state.map_or(100, |state| state.zoom_percent),
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
                    .px(px(6.0 * ui))
                    .flex()
                    .items_center()
                    .gap(px(2.0 * ui))
                    .child(back)
                    .child(forward)
                    .child(reload);

                // The lock / info glyph at the pill's left edge toggles the
                // site panel; the rest of the pill starts an edit.
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
                    .on_click(move |_event: &ClickEvent, _window, app| {
                        if let Some(entity) = site_entity.upgrade() {
                            entity.update(app, |this, _cx| this.toggle_webview_site_panel(id));
                        }
                    });
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
                    .min_w(px(40.0 * ui))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(420.0 * ui))
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
                let tools = div()
                    .id(gpui::SharedString::from(format!("webview-tools-{id}")))
                    .flex_shrink_0()
                    .w(px(pill_h))
                    .h(px(pill_h))
                    .rounded(px(pill_h / 2.0))
                    .bg(pill)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |cell| cell.bg(strip.ink.opacity(2.0 * PILL_FILL)))
                    .child(icon(ICON_ELLIPSIS, px(15.0 * ui), strip.ink.opacity(NAV_ENABLED)))
                    .on_click(move |_event: &ClickEvent, _window, app| {
                        if let Some(entity) = tools_entity.upgrade() {
                            entity.update(app, |this, _cx| this.toggle_webview_tools_panel(id));
                        }
                    });

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
                    .px(px(10.0 * ui))
                    .flex()
                    .items_center()
                    .gap(px(8.0 * ui))
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

            if let Some(panel) = self.webview_panel.clone().filter(|panel| panel.id() == id) {
                let panel_height = panel
                    .height()
                    .min((height - toolbar_height - 1.0 / scale).max(0.0));
                let panel_el = match panel {
                    Panel::Site { cookies, .. } => {
                        let secure = placement.url.starts_with("https://");
                        let cookies = cookies
                            .map(|count| {
                                format!("{count} cookie{}", if count == 1 { "" } else { "s" })
                            })
                            .unwrap_or_else(|error| error);
                        let clear_entity = entity.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(7.0))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(host(&placement.url)),
                            )
                            .child(if secure {
                                "Secure connection (HTTPS)".to_string()
                            } else {
                                "Connection is not secure".to_string()
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(cookies)
                                    .child(
                                        Button::new(format!("webview-site-clear-{id}"))
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::Xs)
                                            .child("Clear data…")
                                            .on_click(move |_event, _window, app| {
                                                if let Some(entity) = clear_entity.upgrade() {
                                                    entity.update(app, |this, _cx| {
                                                        this.confirm_clear_webview_data(id)
                                                    });
                                                }
                                            }),
                                    ),
                            )
                            .into_any_element()
                    }
                    Panel::Tools { .. } => self.render_webview_tools(
                        id,
                        placement.zoom_percent,
                        &theme,
                        &entity,
                        window,
                        cx,
                    ),
                };
                layer = layer.child(
                    div()
                        .absolute()
                        .occlude()
                        .left(px(x))
                        .top(px(y + toolbar_height))
                        .w(px(width))
                        .h(px(panel_height))
                        .overflow_hidden()
                        .px(px(12.0))
                        .py(px(9.0))
                        .bg(theme.popover)
                        .border_b_1()
                        .border_color(theme.border)
                        .font_family(crate::renderer::FONT_FAMILY)
                        .text_size(px(12.0))
                        .text_color(theme.foreground)
                        .child(panel_el),
                );
            }
        }
        layer.into_any_element()
    }

    fn render_webview_tools(
        &mut self,
        id: u64,
        zoom: u16,
        theme: &Theme,
        entity: &gpui::WeakEntity<App>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.webview_find
            .update(cx, |input, _cx| input.set_text_size(Some(px(12.5))));
        let find_entity = entity.clone();
        let find = div()
            .h(px(30.0))
            .rounded(px(7.0))
            .bg(theme.foreground.opacity(0.06))
            .border_1()
            .border_color(theme.border)
            .px(px(8.0))
            .flex()
            .items_center()
            .child(self.webview_find.clone())
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
        if self.webview_find_for == Some(id)
            && !self
                .webview_find
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            window.focus(&self.webview_find.read(cx).focus_handle(cx), cx);
        }

        let minus_entity = entity.clone();
        let reset_entity = entity.clone();
        let plus_entity = entity.clone();
        let print_entity = entity.clone();
        let devtools_entity = entity.clone();
        let external_entity = entity.clone();
        let clear_entity = entity.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(find)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child("Zoom")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .child(
                                Button::new(format!("webview-zoom-out-{id}"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::IconXs)
                                    .child("−")
                                    .on_click(move |_event, _window, app| {
                                        if let Some(entity) = minus_entity.upgrade() {
                                            entity.update(app, |this, _cx| {
                                                let result =
                                                    this.webviews.zoom(id, -0.1).map(|_| ());
                                                this.webview_error(result);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new(format!("webview-zoom-reset-{id}"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Xs)
                                    .child(format!("{zoom}%"))
                                    .on_click(move |_event, _window, app| {
                                        if let Some(entity) = reset_entity.upgrade() {
                                            entity.update(app, |this, _cx| {
                                                let result =
                                                    this.webviews.set_zoom(id, 1.0).map(|_| ());
                                                this.webview_error(result);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new(format!("webview-zoom-in-{id}"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::IconXs)
                                    .child("+")
                                    .on_click(move |_event, _window, app| {
                                        if let Some(entity) = plus_entity.upgrade() {
                                            entity.update(app, |this, _cx| {
                                                let result =
                                                    this.webviews.zoom(id, 0.1).map(|_| ());
                                                this.webview_error(result);
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        Button::new(format!("webview-print-{id}"))
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .child("Print")
                            .on_click(move |_event, _window, app| {
                                if let Some(entity) = print_entity.upgrade() {
                                    entity.update(app, |this, _cx| {
                                        let result = this.webviews.print(id);
                                        this.webview_error(result);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new(format!("webview-devtools-{id}"))
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .child("Inspector")
                            .on_click(move |_event, _window, app| {
                                if let Some(entity) = devtools_entity.upgrade() {
                                    entity.update(app, |this, _cx| {
                                        let result = this.webviews.open_devtools(id);
                                        this.webview_error(result);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new(format!("webview-external-{id}"))
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .child("Open external")
                            .on_click(move |_event, _window, app| {
                                if let Some(entity) = external_entity.upgrade() {
                                    entity.update(app, |this, _cx| {
                                        if let Some(url) = this.webview_tab_url(id)
                                            && let Err(error) =
                                                std::process::Command::new("open").arg(url).spawn()
                                        {
                                            this.toast_notification(format!(
                                                "open browser: {error}"
                                            ));
                                        }
                                        this.request_redraw();
                                    });
                                }
                            }),
                    ),
            )
            .child(
                Button::new(format!("webview-clear-{id}"))
                    .variant(ButtonVariant::Destructive)
                    .size(ButtonSize::Sm)
                    .child("Clear browsing data…")
                    .on_click(move |_event, _window, app| {
                        if let Some(entity) = clear_entity.upgrade() {
                            entity.update(app, |this, _cx| this.confirm_clear_webview_data(id));
                        }
                    }),
            )
            .into_any_element()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_height_only_applies_to_own_webview() {
        let panel = Panel::Tools { id: 4 };
        assert_eq!(panel_height(Some(&panel), 4), crate::webview::TOOLS_PANEL_H);
        assert_eq!(panel_height(Some(&panel), 5), 0.0);
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
}
