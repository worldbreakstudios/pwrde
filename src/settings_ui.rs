//! Settings page as a gpui element tree over the canvas.
//!
//! The rest of the app stays canvas-painted; this module builds a real
//! component tree (vendored rcn Card/Input/Switch/…) positioned over
//! [`workspace::terminal_area`]. Confirm dialogs stay on the canvas path, so
//! the overlay is skipped while one is open. The Settings sidebar (search +
//! section tabs) remains canvas-painted.

use gpui::{
    anchored, deferred, div, px, relative, rgb, svg, AnyElement, App as GpuiApp, ClickEvent,
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window,
    prelude::FluentBuilder as _,
};

use crate::pages::{self, Action, AppearanceDropdown, Section};
use crate::settings;
use crate::term_theme;
use crate::theme;
use crate::ui::theme::{alpha, Theme};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonGroup, ButtonSize, ButtonVariant, Card, CardContent,
    CardHeader, CardTitle, Kbd, Label, Switch,
};
use crate::App;

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

        let searching = !self.settings_query.trim().is_empty();
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
                Section::Appearance => render_appearance(self, &theme, entity.clone()),
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
                    .child(CardContent::new().flex_1().child(body)),
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
    let mut list = div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll();

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

        list = list.child(
            settings_row()
                .id(("kbd-row", ix))
                .cursor_pointer()
                // The armed row gets the chrome-accent tint (rcn `primary`);
                // plain hover keeps the muted `accent` token.
                .when(recording, |el| el.bg(alpha(theme.primary, 0.18)))
                .hover(move |s| {
                    if recording {
                        s.bg(alpha(theme.primary, 0.18))
                    } else {
                        s.bg(theme.accent)
                    }
                })
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = row_entity.upgrade() {
                        entity.update(gpui_app, move |this, cx| {
                            this.recording = Some(action);
                            cx.notify();
                        });
                    }
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(13.))
                        .child(action.label()),
                )
                .child(right),
        );
    }
    list.into_any_element()
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

// ── Appearance ────────────────────────────────────────────────────────────

/// Opaque sRGB triple → gpui Hsla (mirrors ui::theme::from_chrome's helper).
fn srgb((r, g, b): (u8, u8, u8)) -> gpui::Hsla {
    rgb(((r as u32) << 16) | ((g as u32) << 8) | (b as u32)).into()
}

fn render_appearance(
    app: &App,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let preview_dark = app.preview_dark;
    let open_menu = app.appearance_menu;
    let mode = theme::mode();

    // Header: Mode segmented control + Preview Light/Dark toggle.
    let mut mode_group = ButtonGroup::new();
    for (mi, m) in theme::Mode::ALL.into_iter().enumerate() {
        let selected = mode == m;
        let entity_m = entity.clone();
        mode_group = mode_group.item(
            Button::new(("mode-seg", mi))
                .variant(if selected {
                    ButtonVariant::Default
                } else {
                    ButtonVariant::Outline
                })
                .size(ButtonSize::Sm)
                .child(m.label())
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = entity_m.upgrade() {
                        entity.update(gpui_app, move |_this, cx| {
                            settings::set("appearance.mode", m.name().into());
                            cx.notify();
                        });
                    }
                }),
        );
    }

    let mut preview_group = ButtonGroup::new();
    for (i, (label, dark)) in [("Light", false), ("Dark", true)].into_iter().enumerate() {
        let selected = preview_dark == dark;
        let entity_p = entity.clone();
        preview_group = preview_group.item(
            Button::new(("preview-seg", i))
                .variant(if selected {
                    ButtonVariant::Default
                } else {
                    ButtonVariant::Outline
                })
                .size(ButtonSize::Sm)
                .child(label)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = entity_p.upgrade() {
                        entity.update(gpui_app, move |this, cx| {
                            this.preview_dark = dark;
                            cx.notify();
                        });
                    }
                }),
        );
    }

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Label::new().child("Mode"))
                .child(mode_group),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Label::new().child("Preview"))
                .child(preview_group),
        );

    // Two-column dropdowns: App Theme | Terminal Colors.
    let theme_col = dropdown_column(
        "App Theme",
        &[AppearanceDropdown::ThemeLight, AppearanceDropdown::ThemeDark],
        open_menu,
        preview_dark,
        theme,
        entity.clone(),
    );
    let term_col = dropdown_column(
        "Terminal Colors",
        &[AppearanceDropdown::TermLight, AppearanceDropdown::TermDark],
        open_menu,
        preview_dark,
        theme,
        entity.clone(),
    );
    let dropdowns = div()
        .flex()
        .flex_row()
        .gap_4()
        .child(theme_col)
        .child(term_col);

    // WYSIWYG preview cards (explicit colors from the previewed polarity).
    let previews = div()
        .flex()
        .flex_row()
        .gap_3()
        .child(app_preview_card(preview_dark))
        .child(term_preview_card(preview_dark));

    // Footer: Import / Copy + live-apply note.
    let import_entity = entity.clone();
    let copy_entity = entity.clone();
    let footer = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            Button::new("import-theme")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child("Import from Clipboard")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = import_entity.upgrade() {
                        entity.update(gpui_app, |_this, cx| {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                if let Some(tokens) = clipboard
                                    .get_text()
                                    .ok()
                                    .as_deref()
                                    .and_then(theme::parse_tokens)
                                {
                                    let dark = theme::is_dark_color(tokens[0]);
                                    settings::set(
                                        theme::custom_key(dark),
                                        theme::serialize_tokens(&tokens).into(),
                                    );
                                    settings::set(
                                        theme::setting_key(dark),
                                        theme::custom_name(dark).into(),
                                    );
                                }
                            }
                            cx.notify();
                        });
                    }
                }),
        )
        .child(
            Button::new("copy-theme")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child("Copy Theme String")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = copy_entity.upgrade() {
                        entity.update(gpui_app, |_this, cx| {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(theme::export_current());
                            }
                            cx.notify();
                        });
                    }
                }),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .text_right()
                .child("changes apply live"),
        );

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .gap_4()
        .child(header)
        .child(dropdowns)
        .child(previews)
        .child(footer)
        .into_any_element()
}

fn dropdown_column(
    caption: &'static str,
    dropdowns: &[AppearanceDropdown],
    open_menu: Option<AppearanceDropdown>,
    preview_dark: bool,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> gpui::Div {
    let mut col = div()
        .flex()
        .flex_col()
        .flex_1()
        .gap_2()
        .min_w(px(0.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.accent)
                .child(caption),
        );
    for d in dropdowns.iter().copied() {
        col = col.child(swatch_dropdown(d, open_menu, preview_dark, theme, entity.clone()));
    }
    col
}

/// Custom swatch dropdown (rcn Select is text-only; imitate its trigger +
/// anchored/deferred panel so the panel escapes overflow clipping).
fn swatch_dropdown(
    d: AppearanceDropdown,
    open_menu: Option<AppearanceDropdown>,
    preview_dark: bool,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let open = open_menu == Some(d);
    let live = d.dark() == preview_dark;
    let dark = d.dark();
    // Stable numeric id for ElementId tuples ((&str, usize) only).
    let d_ix = AppearanceDropdown::ALL
        .iter()
        .position(|&x| x == d)
        .unwrap_or(0);

    let (swatches, name): ([(u8, u8, u8); 3], String) = match d {
        AppearanceDropdown::ThemeLight | AppearanceDropdown::ThemeDark => {
            let t = theme::selected(dark);
            ([t.gradient_from, t.card, t.accent], t.label.to_string())
        }
        AppearanceDropdown::TermLight | AppearanceDropdown::TermDark => {
            let sel = term_theme::selected(dark);
            let chrome_bg = theme::selected(preview_dark).term_bg;
            let (_, _, ansi) = term_theme::preview_colors(sel, chrome_bg);
            (
                [ansi[1], ansi[2], ansi[4]],
                sel.map_or("Default", |t| t.label).to_string(),
            )
        }
    };

    let trigger_bg = if live {
        alpha(theme.accent, 0.14)
    } else {
        alpha(theme.foreground, 0.06)
    };

    let entity_toggle = entity.clone();
    let entity_close = entity.clone();
    let d_toggle = d;

    let mut swatch_row = div().flex().flex_row().items_center().gap_1();
    for c in swatches.iter() {
        swatch_row = swatch_row.child(
            div()
                .w(px(12.))
                .h(px(12.))
                .rounded(px(3.))
                .bg(srgb(*c)),
        );
    }

    let trigger = div()
        .id(("dd-trigger", d_ix))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(32.))
        .px(px(10.))
        .rounded(px(7.))
        .bg(trigger_bg)
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|s| s.bg(alpha(theme.accent, 0.2)))
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(entity) = entity_toggle.upgrade() {
                entity.update(gpui_app, move |this, cx| {
                    this.appearance_menu = if this.appearance_menu == Some(d_toggle) {
                        None
                    } else {
                        Some(d_toggle)
                    };
                    cx.notify();
                });
            }
        })
        .child(swatch_row)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(13.))
                .child(name),
        )
        .child(
            svg()
                .path(theme.icons.chevron_down())
                .size(px(14.))
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        );

    let field = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child(d.label()),
        )
        .child(trigger);

    // Panel: match select.rs — absolute under trigger + deferred(anchored(...)).
    let panel = if open {
        let options_el = match d {
            AppearanceDropdown::ThemeLight | AppearanceDropdown::ThemeDark => {
                let opts = pages::theme_options(dark);
                let current = theme::selected(dark).name;
                let mut list = div().flex().flex_col().p_1();
                for (i, t) in opts.into_iter().enumerate() {
                    let selected = t.name == current;
                    let entity_item = entity.clone();
                    let set_name: &'static str = t.name;
                    let label = t.label;
                    let item_swatches = [t.gradient_from, t.card, t.accent];
                    let mut chips = div().flex().flex_row().items_center().gap_1();
                    for c in item_swatches.iter() {
                        chips = chips.child(
                            div()
                                .w(px(10.))
                                .h(px(10.))
                                .rounded(px(2.))
                                .bg(srgb(*c)),
                        );
                    }
                    // Encode dropdown + option into one usize key: d_ix*1000 + i
                    let opt_id = d_ix * 1000 + i;
                    list = list.child(
                        div()
                            .id(("dd-opt", opt_id))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.accent).text_color(theme.accent_foreground))
                            .on_click(
                                move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                                    gpui_app.stop_propagation();
                                    if let Some(entity) = entity_item.upgrade() {
                                        entity.update(gpui_app, move |this, cx| {
                                            settings::set(
                                                theme::setting_key(dark),
                                                set_name.into(),
                                            );
                                            this.appearance_menu = None;
                                            cx.notify();
                                        });
                                    }
                                },
                            )
                            .child(chips)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .child(label),
                            )
                            .when(selected, |el| {
                                el.child(
                                    svg()
                                        .path(theme.icons.check())
                                        .size(px(14.))
                                        .text_color(theme.foreground),
                                )
                            }),
                    );
                }
                list
            }
            AppearanceDropdown::TermLight | AppearanceDropdown::TermDark => {
                let opts = pages::term_options(dark);
                let current = term_theme::selected(dark).map(|t| t.name);
                let chrome_bg = theme::selected(preview_dark).term_bg;
                let mut list = div().flex().flex_col().p_1();
                for (i, opt) in opts.into_iter().enumerate() {
                    let selected = opt.map(|t| t.name) == current;
                    let entity_item = entity.clone();
                    let set_val: &'static str = opt.map_or("default", |t| t.name);
                    let label = opt.map_or("Default", |t| t.label);
                    let (_, _, ansi) = term_theme::preview_colors(opt, chrome_bg);
                    let item_swatches = [ansi[1], ansi[2], ansi[4]];
                    let mut chips = div().flex().flex_row().items_center().gap_1();
                    for c in item_swatches.iter() {
                        chips = chips.child(
                            div()
                                .w(px(10.))
                                .h(px(10.))
                                .rounded(px(2.))
                                .bg(srgb(*c)),
                        );
                    }
                    let opt_id = d_ix * 1000 + i;
                    list = list.child(
                        div()
                            .id(("dd-opt", opt_id))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.accent).text_color(theme.accent_foreground))
                            .on_click(
                                move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                                    gpui_app.stop_propagation();
                                    if let Some(entity) = entity_item.upgrade() {
                                        entity.update(gpui_app, move |this, cx| {
                                            settings::set(
                                                term_theme::setting_key(dark),
                                                set_val.into(),
                                            );
                                            this.appearance_menu = None;
                                            cx.notify();
                                        });
                                    }
                                },
                            )
                            .child(chips)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .child(label),
                            )
                            .when(selected, |el| {
                                el.child(
                                    svg()
                                        .path(theme.icons.check())
                                        .size(px(14.))
                                        .text_color(theme.foreground),
                                )
                            }),
                    );
                }
                list
            }
        };

        let panel_body = div()
            .id(("dd-panel", d_ix))
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(220.))
            .w(px(260.))
            .max_h(px(280.))
            .overflow_y_scroll()
            .rounded(theme.radius_md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .p(px(4.))
            .shadow_md()
            .on_mouse_down_out(move |_ev, _win, gpui_app| {
                gpui_app.stop_propagation();
                if let Some(entity) = entity_close.upgrade() {
                    entity.update(gpui_app, |this, cx| {
                        this.appearance_menu = None;
                        cx.notify();
                    });
                }
            })
            .child(options_el);

        Some(
            div()
                .absolute()
                .left_0()
                .top(relative(1.))
                .pt(px(4.))
                .child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.))
                            .child(panel_body),
                    )
                    .with_priority(1),
                ),
        )
    } else {
        None
    };

    div()
        .relative()
        .child(field)
        .when_some(panel, |el, p| el.child(p))
        .into_any_element()
}

/// App chrome WYSIWYG preview — plain divs with explicit colors from the
/// previewed theme (not rcn tokens).
fn app_preview_card(preview_dark: bool) -> AnyElement {
    let pt = theme::selected(preview_dark);
    let bg = srgb(pt.gradient_from);
    let card = srgb(pt.card);
    let accent = srgb(pt.accent);
    let ink = srgb(pt.ink);
    let ink_dim = srgb(pt.ink_dim);
    let term_bg = srgb(pt.term_bg);
    // Dimmed ink for secondary text / inactive tabs.
    let pane_dim = alpha(ink, 0.55);
    let caption = format!("{} · Preview", pt.label);

    // Traffic lights.
    let dots = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(srgb((0xFF, 0x5F, 0x57))))
        .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(srgb((0xFE, 0xBC, 0x2E))))
        .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(srgb((0x28, 0xC8, 0x40))));

    // Mini sidebar rows.
    let mut sidebar = div()
        .flex()
        .flex_col()
        .gap_1()
        .w(px(90.))
        .p_2()
        .rounded(px(6.))
        .bg(alpha(ink, 0.06));
    for (i, name) in ["flaky tests", "stripe v4", "docs pass"].iter().enumerate() {
        let row = div()
            .px(px(6.))
            .py(px(3.))
            .rounded(px(4.))
            .text_size(px(10.))
            .text_color(if i == 0 { ink } else { pane_dim })
            .when(i == 0, |el| el.bg(alpha(accent, 0.25)))
            .child(*name);
        sidebar = sidebar.child(row);
    }

    // Mini terminal tile with tabs.
    let tabs = div()
        .flex()
        .flex_row()
        .gap_1()
        .child(
            div()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(3.))
                .bg(alpha(ink, 0.12))
                .text_size(px(10.))
                .text_color(ink)
                .child("zsh"),
        )
        .child(
            div()
                .px(px(6.))
                .py(px(2.))
                .text_size(px(10.))
                .text_color(pane_dim)
                .child("cargo"),
        );
    let term_tile = div()
        .flex()
        .flex_col()
        .flex_1()
        .gap_1()
        .p_2()
        .rounded(px(6.))
        .bg(term_bg)
        .child(tabs)
        .child(
            div()
                .text_size(px(10.))
                .text_color(ink)
                .font_family("monospace")
                .child("$ cargo run"),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(pane_dim)
                .font_family("monospace")
                .child("   Compiling pwrde"),
        );

    // Surface card + buttons + token swatches.
    let surface = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(px(6.))
        .bg(card)
        .child(
            div()
                .text_size(px(11.))
                .text_color(ink)
                .child("Surface card"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .text_size(px(10.))
                .child(
                    div()
                        .text_color(pane_dim)
                        .child("Secondary text on surface · "),
                )
                .child(div().text_color(accent).child("a link")),
        );

    let buttons = div()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            div()
                .px(px(8.))
                .py(px(3.))
                .rounded(px(4.))
                .bg(accent)
                .text_size(px(10.))
                .text_color(srgb((0xFF, 0xFF, 0xFF)))
                .child("Primary"),
        )
        .child(
            div()
                .px(px(8.))
                .py(px(3.))
                .rounded(px(4.))
                .border_1()
                .border_color(alpha(ink, 0.25))
                .text_size(px(10.))
                .text_color(ink)
                .child("Secondary"),
        );

    let mut swatches = div().flex().flex_row().gap_1();
    for c in [pt.gradient_from, pt.card, pt.term_bg, pt.accent, pt.ink].iter() {
        swatches = swatches.child(
            div()
                .w(px(14.))
                .h(px(14.))
                .rounded(px(3.))
                .bg(srgb(*c)),
        );
    }

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .bg(bg)
        .border_1()
        .border_color(alpha(ink, 0.12))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(dots)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(ink_dim)
                        .child(caption),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(sidebar)
                .child(term_tile),
        )
        .child(surface)
        .child(buttons)
        .child(swatches)
        .child(
            div()
                .text_size(px(9.))
                .text_color(pane_dim)
                .child("bg · surface · pane · accent · ink"),
        )
        .into_any_element()
}

/// Terminal WYSIWYG preview — ANSI chips + fake colored shell session.
fn term_preview_card(preview_dark: bool) -> AnyElement {
    let chrome = theme::selected(preview_dark);
    let sel = term_theme::selected(preview_dark);
    let (tfg, tbg, ansi) = term_theme::preview_colors(sel, chrome.term_bg);
    let bg = srgb(tbg);
    let fg = srgb(tfg);
    let dim = alpha(fg, 0.55);
    let red = srgb(ansi[1]);
    let green = srgb(ansi[2]);
    let yellow = srgb(ansi[3]);
    let magenta = srgb(ansi[5]);
    let cyan = srgb(ansi[6]);
    let caption = format!("{} · Preview", sel.map_or("Default", |t| t.label));

    let mut chips = div().flex().flex_row().gap_1();
    for c in ansi.iter() {
        chips = chips.child(
            div()
                .w(px(10.))
                .h(px(10.))
                .rounded(px(2.))
                .bg(srgb(*c)),
        );
    }

    // Colored span helper for a shell line.
    fn line(spans: Vec<AnyElement>) -> gpui::Div {
        let mut row = div().flex().flex_row().font_family("monospace").text_size(px(10.));
        for s in spans {
            row = row.child(s);
        }
        row
    }
    fn span(text: &'static str, color: gpui::Hsla) -> AnyElement {
        div().text_color(color).child(text).into_any_element()
    }

    let session = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(line(vec![
            span("you@dev", green),
            span(":~/checkout$", dim),
            span(" git status", fg),
        ]))
        .child(line(vec![
            span("On branch ", dim),
            span("fix/flaky-capture", cyan),
        ]))
        .child(line(vec![
            span("  modified:  ", red),
            span("tests/conftest.py", fg),
        ]))
        .child(line(vec![
            span("  new file:  ", green),
            span("tests/test_clock.py", fg),
        ]))
        .child(line(vec![
            span("you@dev", green),
            span(":~$", dim),
            span(" pytest -q", fg),
        ]))
        .child(line(vec![
            span("warning: ", yellow),
            span("2 deprecation warnings", fg),
        ]))
        .child(line(vec![
            span("400 passed ", green),
            span("0 failed", red),
            span(" in ", fg),
            span("41.2s", magenta),
        ]))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .font_family("monospace")
                .text_size(px(10.))
                .child(span("❯ ", magenta))
                .child(span("agent watching e2e ", fg))
                // Trailing block cursor.
                .child(div().w(px(7.)).h(px(12.)).bg(fg)),
        );

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .bg(bg)
        .border_1()
        .border_color(alpha(fg, 0.15))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(dim)
                        .child(caption),
                )
                .child(chips),
        )
        .child(session)
        .into_any_element()
}
