//! Settings page as a gpui element tree over the canvas.
//!
//! The rest of the app stays canvas-painted; this module builds a real
//! component tree (vendored rcn Card/Input/Switch/…) positioned over
//! [`workspace::terminal_area`]. Confirm dialogs stay on the canvas path, so
//! the overlay is skipped while one is open. The Settings sidebar (search +
//! section tabs) remains canvas-painted — and so does the whole Appearance
//! section (`Renderer::appearance_page`), whose WYSIWYG preview cards are
//! deliberately quad-painted; this overlay never mounts for it.

use gpui::{
    div, px, AnyElement, App as GpuiApp, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window,
};

use crate::pages::{self, Action, Section};
use crate::settings;
use crate::ui::theme::Theme;
use crate::ui::{
    Badge, BadgeVariant, Card, CardContent, CardHeader, CardTitle, Kbd, Label, Switch, Table,
    TableBody, TableCell, TableRow,
};
use crate::App;

/// Settings rows read badly stretched across a fullscreen window; the card
/// stays page-wide (matching Cleanup and the canvas Appearance section) but
/// its content column caps out at this width.
const CONTENT_MAX_W: f32 = 760.0;

impl App {
    /// Build the Settings page overlay (logical px, absolutely positioned over
    /// the content area). Call only when `page == Settings` and no confirm is up.
    pub fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        // Keep the rcn Theme global in sync with live chrome tokens.
        cx.set_global(crate::ui::theme::Theme::from_chrome(crate::theme::current()));

        // Mirror workspace::terminal_area as logical edge insets rather than
        // computing w/h from the renderer's surface size: insets re-resolve
        // in gpui layout every frame, so the overlay tracks a live window
        // resize instead of waiting for the next entity notify.
        let pad = crate::workspace::AREA_PAD;
        let sidebar = self.sidebar_w();
        let left = if sidebar == 0.0 { pad } else { sidebar };
        let right = self.right_w() + pad;

        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        let searching = !self.settings_query.is_empty();
        let title: String = if searching {
            "Search results".into()
        } else {
            self.section.label().into()
        };

        let body = if searching {
            render_search_results(self, &theme, entity.clone())
        } else {
            match self.section {
                Section::Sessions => render_sessions(self, &theme),
                Section::Keyboard => render_keyboard(self, &theme, entity.clone()),
                Section::Terminal => render_terminal(entity.clone()),
                Section::Debug => render_debug(self, &theme, entity.clone()),
                // Canvas-painted (Renderer::appearance_page); the overlay is
                // never mounted for it. Defensive empty body.
                Section::Appearance => div().into_any_element(),
            }
        };

        let header = CardHeader::new().child(CardTitle::new().child(title));

        let bg_entity = entity.clone();
        div()
            .absolute()
            .left(px(left))
            .top(px(pad))
            .right(px(right))
            .bottom(px(pad))
            // Any press in the card first drops transient input state — an
            // armed recording and the sidebar search box's focus — exactly
            // like the old canvas settings_click. Row/control handlers run
            // at mouse-up (on_click), so they re-arm on top of this.
            .on_mouse_down(gpui::MouseButton::Left, move |_ev, _win, gpui_app| {
                if let Some(entity) = bg_entity.upgrade() {
                    entity.update(gpui_app, |this, cx| {
                        this.recording = None;
                        this.settings_search_focus = false;
                        cx.notify();
                    });
                }
            })
            .child(
                // h_full/flex_1 are local additions: the card fills the page
                // and the content band absorbs the height the header leaves
                // over, keeping any row scroller bounded.
                Card::new()
                    .h_full()
                    .child(header)
                    .child(
                        CardContent::new().flex_1().child(
                            div()
                                .flex()
                                .flex_col()
                                .h_full()
                                .min_h(px(0.))
                                .w_full()
                                .max_w(px(CONTENT_MAX_W))
                                .child(body),
                        ),
                    ),
            )
            .into_any_element()
    }
}

// ── Shared row chrome ───────────────────────────────────────────────────

fn settings_row() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
}

// ── Search results ──────────────────────────────────────────────────────

fn render_search_results(
    app: &App,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let results = pages::search_settings(&app.settings_query);
    if results.is_empty() {
        return div()
            .px_3()
            .py_2()
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .child("no settings match")
            .into_any_element();
    }

    let mut list = div().id("settings-rows").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll();
    for (ix, entry) in results.into_iter().enumerate() {
        let section = entry.section;
        let row_entity = entity.clone();
        list = list.child(
            settings_row()
                .id(("settings-search", ix))
                .cursor_pointer()
                .hover(|s| s.bg(theme.accent))
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = row_entity.upgrade() {
                        entity.update(gpui_app, move |this, cx| {
                            this.section = section;
                            this.settings_query.clear();
                            this.settings_search_focus = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(13.))
                        .child(entry.label),
                )
                .child(
                    Badge::new()
                        .variant(BadgeVariant::Outline)
                        .child(entry.section.label()),
                ),
        );
    }
    list.into_any_element()
}

// ── Sessions ────────────────────────────────────────────────────────────

fn render_sessions(app: &App, theme: &Theme) -> AnyElement {
    let input_el = div()
        .flex_1()
        .min_w(px(0.))
        .max_w(px(360.))
        .child(app.command_input.clone());

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            settings_row()
                .child(Label::new().child("Primary command"))
                .child(input_el),
        )
        .child(
            div()
                .px_3()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child("runs in the primary pane when a group opens (enter saves, esc cancels)"),
        )
        .into_any_element()
}

// ── Keyboard ────────────────────────────────────────────────────────────

fn render_keyboard(
    app: &App,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let mut body = TableBody::new();
    let count = Action::ALL.len();
    for (ix, action) in Action::ALL.iter().copied().enumerate() {
        let recording = app.recording == Some(action);
        let row_entity = entity.clone();
        let right: AnyElement = if recording {
            div()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child("press keys… (esc cancels)")
                .into_any_element()
        } else {
            Kbd::new()
                .child(action.binding().display())
                .into_any_element()
        };

        body = body.child(
            TableRow::new()
                .id(("kbd-row", ix))
                .selected(recording)
                .last(ix + 1 == count)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = row_entity.upgrade() {
                        entity.update(gpui_app, move |this, cx| {
                            this.recording = Some(action);
                            cx.notify();
                        });
                    }
                })
                .child(TableCell::new().flex(1.).child(action.label()))
                .child(
                    TableCell::new()
                        .w(px(200.))
                        .child(div().flex().w_full().justify_end().child(right)),
                ),
        );
    }

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .child(Table::new().child(body))
        .into_any_element()
}

// ── Terminal ────────────────────────────────────────────────────────────

fn render_terminal(entity: gpui::WeakEntity<App>) -> AnyElement {
    let persist = settings::get_bool("terminal.persist", false);
    let switch_entity = entity.clone();
    let toggle = Switch::new("terminal-persist")
        .checked(persist)
        .on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            let on = *checked;
            if let Some(entity) = switch_entity.upgrade() {
                entity.update(gpui_app, move |this, cx| {
                    settings::set("terminal.persist", on.into());
                    this.persist_snapshot();
                    cx.notify();
                });
            }
        });

    div()
        .flex()
        .flex_col()
        .child(
            settings_row()
                .child(
                    div()
                        .text_size(px(13.))
                        .child("Persist sessions"),
                )
                .child(toggle),
        )
        .into_any_element()
}

// ── Debug ───────────────────────────────────────────────────────────────

fn render_debug(
    app: &App,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let th = crate::theme::current();
    let (sw, sh) = app.renderer.surface_size();
    let diags: [(&str, String); 7] = [
        (
            "settings file",
            settings::path().to_string_lossy().into_owned(),
        ),
        ("theme", th.label.into()),
        ("scale", format!("{:.2}", app.renderer.scale)),
        ("surface", format!("{}×{} px", sw, sh)),
        (
            "cell",
            format!(
                "{}×{} px",
                app.renderer.cell_width, app.renderer.cell_height
            ),
        ),
        ("workspaces", app.workspaces.len().to_string()),
        (
            "tiles (active group)",
            app.workspaces
                .get(app.active)
                .map(|ws| ws.root.tiles().len().to_string())
                .unwrap_or_else(|| "0".into()),
        ),
    ];

    let mut col = div().flex().flex_col().gap_1();
    for (key, value) in diags {
        col = col.child(
            settings_row()
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.muted_foreground)
                        .child(key),
                )
                .child(div().text_size(px(13.)).child(value)),
        );
    }

    let overlay_on = settings::get_bool("debug.overlay", false);
    let switch_entity = entity.clone();
    let toggle = Switch::new("debug-overlay")
        .checked(overlay_on)
        .on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            let on = *checked;
            if let Some(entity) = switch_entity.upgrade() {
                entity.update(gpui_app, move |_this, cx| {
                    settings::set("debug.overlay", on.into());
                    cx.notify();
                });
            }
        });

    col.child(
        settings_row()
            .child(div().text_size(px(13.)).child("Show frame stats"))
            .child(toggle),
    )
    .into_any_element()
}
