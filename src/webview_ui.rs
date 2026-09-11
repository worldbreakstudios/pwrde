//! GPUI-owned browser chrome for native Wry child views.

use gpui::{
    AnyElement, App as GpuiApp, ClickEvent, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::App;
use crate::ui::theme::Theme;
use crate::ui::assets::{ICON_ELLIPSIS_VERTICAL, ICON_INFO, ICON_LOCK, ICON_REFRESH};
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
}

fn host(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
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

    fn webview_error(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.message = Some((error, true));
        }
        self.request_redraw();
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
            placements
                .iter()
                .any(|placement| placement.id == id && placement.focused)
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
                    self.webview_error(result);
                    if self.message.is_none() {
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
                    self.webview_error(result);
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
            || (self.flyover_anim > 0.0 && !self.flyover_windowed)
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
                let state = self.webviews.state(id);
                Some(ChromePlacement {
                    id,
                    url,
                    rect: crate::workspace::tile_content(&rect, scale),
                    focused: tile_id == workspace.focused_tile,
                    can_go_back: state.as_ref().is_some_and(|state| state.can_go_back),
                    can_go_forward: state.as_ref().is_some_and(|state| state.can_go_forward),
                    zoom_percent: state.map_or(100, |state| state.zoom_percent),
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
        let mut layer = div().absolute().left(px(0.0)).top(px(0.0)).size_full();

        for placement in placements {
            let id = placement.id;
            let x = placement.rect.x / scale;
            let y = placement.rect.y / scale;
            let width = placement.rect.w / scale;
            let height = placement.rect.h / scale;
            let toolbar_height = crate::webview::TOOLBAR_H.min(height.max(0.0));
            let address_focused = self
                .webview_address
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            if placement.focused
                && (self.webview_address_for != Some(id)
                    || (!address_focused && self.webview_address.read(cx).text() != placement.url))
            {
                self.webview_address_for = Some(id);
                let url = placement.url.clone();
                self.webview_address.update(cx, |input, cx| {
                    input.set_text_size(Some(px(12.5)));
                    input.set_text(url, cx);
                });
            }

            let back_entity = entity.clone();
            let back = Button::new(format!("webview-back-{id}"))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .disabled(!placement.can_go_back)
                .child(
                    gpui::svg()
                        .path(theme.icons.chevron_left())
                        .size(px(14.0)),
                )
                .on_click(move |_event, _window, app| {
                    if let Some(entity) = back_entity.upgrade() {
                        entity.update(app, |this, _cx| {
                            let result = this.webviews.go_back(id);
                            this.webview_error(result);
                        });
                    }
                });
            let forward_entity = entity.clone();
            let forward = Button::new(format!("webview-forward-{id}"))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .disabled(!placement.can_go_forward)
                .child(
                    gpui::svg()
                        .path(theme.icons.chevron_right())
                        .size(px(14.0)),
                )
                .on_click(move |_event, _window, app| {
                    if let Some(entity) = forward_entity.upgrade() {
                        entity.update(app, |this, _cx| {
                            let result = this.webviews.go_forward(id);
                            this.webview_error(result);
                        });
                    }
                });
            let reload_entity = entity.clone();
            let reload = Button::new(format!("webview-reload-{id}"))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .child(gpui::svg().path(ICON_REFRESH).size(px(14.0)))
                .on_click(move |_event, _window, app| {
                    if let Some(entity) = reload_entity.upgrade() {
                        entity.update(app, |this, _cx| {
                            let result = this.webviews.reload(id);
                            this.webview_error(result);
                        });
                    }
                });
            let site_entity = entity.clone();
            let site = Button::new(format!("webview-site-{id}"))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .child(if placement.url.starts_with("https://") {
                    gpui::svg().path(ICON_LOCK).size(px(14.0))
                } else {
                    gpui::svg().path(ICON_INFO).size(px(14.0))
                })
                .on_click(move |_event, _window, app| {
                    if let Some(entity) = site_entity.upgrade() {
                        entity.update(app, |this, _cx| this.toggle_webview_site_panel(id));
                    }
                });
            let tools_entity = entity.clone();
            let tools = Button::new(format!("webview-tools-{id}"))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .child(gpui::svg().path(ICON_ELLIPSIS_VERTICAL).size(px(14.0)))
                .on_click(move |_event, _window, app| {
                    if let Some(entity) = tools_entity.upgrade() {
                        entity.update(app, |this, _cx| this.toggle_webview_tools_panel(id));
                    }
                });

            let address = div()
                .flex_1()
                .min_w(px(40.0))
                .h(px(30.0))
                .rounded(px(8.0))
                .bg(theme.foreground.opacity(0.06))
                .border_1()
                .border_color(theme.border)
                .px(px(9.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .text_size(px(12.5))
                .text_color(theme.foreground)
                .child(if placement.focused {
                    self.webview_address.clone().into_any_element()
                } else {
                    div()
                        .whitespace_nowrap()
                        .child(placement.url.clone())
                        .into_any_element()
                });

            let toolbar_entity = entity.clone();
            let toolbar = div()
                .absolute()
                .occlude()
                .left(px(x))
                .top(px(y))
                .w(px(width))
                .h(px(toolbar_height))
                .overflow_hidden()
                .px(px(5.0))
                .flex()
                .items_center()
                .gap(px(2.0))
                .bg(theme.card)
                .border_b_1()
                .border_color(theme.border)
                .font_family(crate::renderer::FONT_FAMILY)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    move |_event, _window, app: &mut GpuiApp| {
                        if let Some(entity) = toolbar_entity.upgrade() {
                            entity.update(app, |this, _cx| this.focus_webview_tab(id));
                        }
                    },
                )
                .child(back)
                .child(forward)
                .child(reload)
                .child(site)
                .child(address)
                .child(tools);
            layer = layer.child(toolbar);

            if let Some(panel) = self.webview_panel.clone().filter(|panel| panel.id() == id) {
                let panel_height = panel
                    .height()
                    .min((height - crate::webview::TOOLBAR_H - 1.0 / scale).max(0.0));
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
                        .top(px(y + crate::webview::TOOLBAR_H))
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
                                            this.message =
                                                Some((format!("open browser: {error}"), true));
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
                    .child("New webview"),
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
    }
}
