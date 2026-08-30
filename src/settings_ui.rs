//! Settings page as a gpui element tree over the canvas.
//!
//! The rest of the app stays canvas-painted; this module builds a real
//! component tree (vendored rcn Card/Input/Switch/…) positioned over
//! [`workspace::terminal_area`]. Confirm dialogs stay on the canvas path, so
//! the overlay is skipped while one is open. The Settings sidebar (search +
//! section tabs) remains canvas-painted; the Appearance section's body is
//! built here as a gpui tree (segmented controls, Selects, WYSIWYG previews).

use gpui::{
    div, px, AnyElement, App as GpuiApp, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window,
};

use crate::pages::{self, Action, AppearanceDropdown, Section};
use crate::settings;
use crate::ui::select::Select;
use crate::ui::theme::{alpha, Theme};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, CardContent, CardHeader,
    CardTitle, Kbd, Switch, Table, TableBody, TableCell, TableRow,
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
                Section::Sessions => render_sessions(self, &theme, entity.clone()),
                Section::Keyboard => render_keyboard(self, &theme, entity.clone()),
                Section::Terminal => render_terminal(&theme, entity.clone()),
                Section::Accessibility => render_accessibility(&theme, entity.clone()),
                Section::Debug => render_debug(self, &theme, entity.clone()),
                Section::FeatureFlags => {
                    render_feature_flags(self, &theme, entity.clone())
                }
                Section::Appearance => render_appearance(self, &theme, entity.clone()),
                Section::Tools => render_tools(self, &theme, entity.clone()),
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
            .on_mouse_down(gpui::MouseButton::Left, move |_ev, win, gpui_app| {
                if let Some(entity) = bg_entity.upgrade() {
                    entity.update(gpui_app, |this, cx| {
                        this.recording = None;
                        this.blur_settings_search(win, cx);
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
                    .glass()
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
                                .mx_auto()
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

/// macOS System Settings-style grouped box: related rows in an inset rounded
/// container — a faint lifted wash with a hairline border, rows separated by
/// inset hairline dividers. This is what gives the glass panel its depth.
fn settings_group(theme: &Theme, rows: Vec<AnyElement>) -> gpui::Div {
    let mut boxed = div()
        .flex()
        .flex_col()
        // Never shrink: `overflow_hidden` zeroes this box's automatic
        // minimum size, so as a direct child of a scrolling column it would
        // be squeezed to a sliver once the page overflows.
        .flex_none()
        .rounded(theme.radius_lg())
        .bg(alpha(theme.foreground, 0.04))
        .border_1()
        .border_color(alpha(theme.foreground, 0.08))
        .overflow_hidden();
    for (ix, row) in rows.into_iter().enumerate() {
        if ix > 0 {
            boxed = boxed.child(
                div().ml_3().h(px(1.)).bg(alpha(theme.foreground, 0.06)),
            );
        }
        boxed = boxed.child(row);
    }
    boxed
}

/// A row's left cell: 13px title with an optional 12px muted description
/// under it — the shape every settings row shares.
fn row_text(theme: &Theme, title: &'static str, desc: Option<&'static str>) -> gpui::Div {
    let mut cell = div()
        .flex()
        .flex_col()
        .child(div().text_size(px(13.)).child(title));
    if let Some(desc) = desc {
        cell = cell.child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child(desc),
        );
    }
    cell
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

    let hover_bg = alpha(theme.foreground, 0.08);
    let mut rows: Vec<AnyElement> = Vec::new();
    for (ix, entry) in results.into_iter().enumerate() {
        let section = entry.section;
        let row_entity = entity.clone();
        rows.push(
            settings_row()
                .id(("settings-search", ix))
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg))
                .on_click(move |_ev: &ClickEvent, win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(entity) = row_entity.upgrade() {
                        entity.update(gpui_app, move |this, cx| {
                            this.section = section;
                            this.clear_settings_search(cx);
                            this.blur_settings_search(win, cx);
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
                )
                .into_any_element(),
        );
    }
    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .child(settings_group(theme, rows))
        .into_any_element()
}

// ── Sessions ────────────────────────────────────────────────────────────

fn render_sessions(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let input_el = div()
        .flex_1()
        .min_w(px(0.))
        .max_w(px(360.))
        .child(app.command_input.clone());

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_group(
            theme,
            vec![
                settings_row()
                    .child(row_text(
                        theme,
                        "Primary command",
                        Some("runs in the primary pane when a group opens (enter saves, esc cancels)"),
                    ))
                    .child(input_el)
                    .into_any_element(),
                git_cli_row(theme, entity.clone()),
                git_async_row(entity),
            ],
        ))
        .into_any_element()
}

/// The PR-data CLI selector (a small lfg | gh segmented control). Any other
/// value can still be set directly in the settings file's `git.cli` key.
fn git_cli_row(theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let current = crate::gh::cli();
    let mk = |id: &'static str, name: &'static str| {
        let active = current == name;
        let e = entity.clone();
        Button::new(id)
            .variant(if active { ButtonVariant::Default } else { ButtonVariant::Outline })
            .size(ButtonSize::Sm)
            .child(name)
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |_this, cx| {
                        settings::set("git.cli", name.into());
                        cx.notify();
                    });
                }
            })
    };
    settings_row()
        .child(row_text(
            theme,
            "Pull request CLI",
            Some("tool used to fetch PR data (lfg is the fast, cached path)"),
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(mk("git-cli-lfg", "lfg"))
                .child(mk("git-cli-gh", "gh")),
        )
        .into_any_element()
}

fn git_async_row(entity: gpui::WeakEntity<App>) -> AnyElement {
    let on = crate::gh::async_enabled();
    let toggle = Switch::new("git-async")
        .checked(on)
        .on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            let enabled = *checked;
            if let Some(e) = entity.upgrade() {
                e.update(gpui_app, move |_this, cx| {
                    settings::set("git.async", enabled.into());
                    cx.notify();
                });
            }
        });
    settings_row()
        .child(div().text_size(px(13.)).child("Async streaming (lfg -A)"))
        .child(toggle)
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
        .child(settings_group(
            theme,
            vec![Table::new().child(body).into_any_element()],
        ))
        .into_any_element()
}

// ── Tools ───────────────────────────────────────────────────────────────

/// Registered CLI tool pages: a table of what's registered (name, icon,
/// command, directory, Remove) over an add form. Only the command is
/// required — see `App::add_tool_from_form` for the defaults.
fn render_tools(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let tools = &app.tools;
    let mut body = TableBody::new();
    let count = tools.len();
    for (ix, tool) in tools.iter().enumerate() {
        let remove_entity = entity.clone();
        let remove = Button::new(("tool-remove", ix))
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Sm)
            .child("Remove")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(entity) = remove_entity.upgrade() {
                    entity.update(gpui_app, move |this, cx| {
                        this.remove_tool(ix);
                        cx.notify();
                    });
                }
            });
        body = body.child(
            TableRow::new()
                .id(("tool-row", ix))
                .last(ix + 1 == count)
                .child(TableCell::new().w(px(44.)).child(tool.icon.clone()))
                .child(TableCell::new().flex(1.).child(tool.name.clone()))
                .child(
                    TableCell::new()
                        .flex(1.5)
                        .child(div().font_family("monospace").child(tool.command.clone())),
                )
                .child(
                    TableCell::new()
                        .flex(1.)
                        .child(div().text_color(theme.muted_foreground).child(tool.cwd.clone())),
                )
                .child(
                    TableCell::new()
                        .w(px(96.))
                        .child(div().flex().w_full().justify_end().child(remove)),
                ),
        );
    }
    let table: AnyElement = if count == 0 {
        div()
            .text_size(px(12.))
            .text_color(theme.muted_foreground)
            .child("No tools registered — add one below.")
            .into_any_element()
    } else {
        Table::new().child(body).into_any_element()
    };

    let field = |label: &'static str, input: gpui::Entity<crate::ui::Input>, flex: f32| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_basis(px(0.))
            .flex_grow(flex)
            .min_w(px(0.))
            .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(label))
            .child(input)
            .into_any_element()
    };
    let add_entity = entity;
    let add = Button::new("tool-add")
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Sm)
        .child("Add tool")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            if let Some(entity) = add_entity.upgrade() {
                entity.update(gpui_app, |this, cx| {
                    this.add_tool_from_form(cx);
                    cx.notify();
                });
            }
        });
    let form = div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .w_full()
                .child(field("Name", app.tool_form.name.clone(), 1.))
                .child(field("Command", app.tool_form.command.clone(), 2.))
                .child(field("Directory", app.tool_form.cwd.clone(), 1.))
                .child(field("Icon", app.tool_form.icon.clone(), 0.5)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div().flex_1().min_w(px(0.)).text_size(px(11.)).text_color(theme.muted_foreground).child(
                        "Each tool gets a sidebar page running its command in a fresh terminal (not persisted). \
                         Directory accepts ~; icon is any text — Nerd Font glyphs render like the built-ins.",
                    ),
                )
                .child(add),
        );

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .gap_4()
        .child(settings_group(theme, vec![table]))
        .child(div().child(group_label(theme, "Add a tool")).child(settings_group(theme, vec![form.into_any_element()])))
        .into_any_element()
}

// ── Terminal ────────────────────────────────────────────────────────────

fn render_terminal(theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
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
        .child(settings_group(
            theme,
            vec![
                settings_row()
                    .child(row_text(theme, "Persist sessions", None))
                    .child(toggle)
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

// ── Accessibility ─────────────────────────────────────────────────────────

/// Font-size controls. Two independent sizes — the terminal grid text and the
/// app/chrome text — each with −/+ steppers and a reset. Mirrors the ⌘= / ⌘-
/// zoom hotkeys, which nudge whichever size matches the focused surface.
fn render_accessibility(theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_group(
            theme,
            vec![
                font_size_row(
                    theme,
                    entity.clone(),
                    "term-font",
                    "Terminal text size",
                    "font size of the terminal grid (⌘= / ⌘- while a terminal is focused)",
                    "terminal.font_size",
                ),
                font_size_row(
                    theme,
                    entity,
                    "app-font",
                    "App text size",
                    "font size of tabs, sidebar, and other app chrome (⌘= / ⌘- elsewhere)",
                    "appearance.font_size",
                ),
            ],
        ))
        .into_any_element()
}

/// One font-size stepper row: label + description on the left, a `−  N px  +`
/// control plus Reset on the right. Each control writes `key` and notifies, so
/// the next canvas paint re-measures and reflows.
fn font_size_row(
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    key: &'static str,
) -> AnyElement {
    let cur = settings::get_f32(key, crate::renderer::FONT_SIZE);

    let step_btn = |idx: usize, glyph: &'static str, delta: f32| {
        let e = entity.clone();
        Button::new((id, idx))
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .child(glyph)
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |_this, cx| {
                        crate::renderer::bump_font(key, delta);
                        cx.notify();
                    });
                }
            })
    };

    let reset_e = entity.clone();
    let reset_btn = Button::new((id, 2usize))
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::Sm)
        .child("Reset")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(e) = reset_e.upgrade() {
                e.update(gpui_app, move |_this, cx| {
                    settings::set(key, f64::from(crate::renderer::FONT_SIZE).into());
                    cx.notify();
                });
            }
        });

    settings_row()
        .child(row_text(theme, title, Some(desc)))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(step_btn(0, "−", -crate::renderer::FONT_SIZE_STEP))
                .child(
                    div()
                        .min_w(px(52.))
                        .text_size(px(13.))
                        .text_color(theme.muted_foreground)
                        .child(div().flex().w_full().justify_center().child(format!("{} px", cur as i32))),
                )
                .child(step_btn(1, "+", crate::renderer::FONT_SIZE_STEP))
                .child(reset_btn),
        )
        .into_any_element()
}

// ── Feature Flags ───────────────────────────────────────────────────────

/// Experimental feature toggles, one row per [`crate::features::ALL`] entry.
/// Each flag is persisted as a `features.<key>` bool (default off).
fn render_feature_flags(
    _app: &App,
    theme: &Theme,
    entity: gpui::WeakEntity<App>,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    for flag in crate::features::ALL {
        let key = flag.key;
        let switch_entity = entity.clone();
        let toggle = Switch::new(key)
            .checked(crate::features::enabled(key))
            .on_change(
                move |checked: &bool,
                      _win: &mut Window,
                      gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    let on = *checked;
                    if let Some(entity) = switch_entity.upgrade() {
                        entity.update(gpui_app, move |_this, cx| {
                            settings::set(&format!("features.{key}"), on.into());
                            cx.notify();
                        });
                    }
                },
            );

        rows.push(
            settings_row()
                .child(row_text(theme, flag.label, Some(flag.description)))
                .child(toggle)
                .into_any_element(),
        );
    }

    if rows.is_empty() {
        return div()
            .px_3()
            .py_2()
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .child("No experimental flags right now.")
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .child(settings_group(theme, rows))
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

    let mut diag_rows: Vec<AnyElement> = Vec::new();
    for (key, value) in diags {
        diag_rows.push(
            settings_row()
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.muted_foreground)
                        .child(key),
                )
                .child(div().text_size(px(13.)).child(value))
                .into_any_element(),
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

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_group(theme, diag_rows))
        .child(settings_group(
            theme,
            vec![
                settings_row()
                    .child(row_text(theme, "Show frame stats", None))
                    .child(toggle)
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

// ── Appearance ─────────────────────────────────────────────────────────

fn group_label(theme: &Theme, title: &'static str) -> gpui::Div {
    div()
        .px_1()
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .child(title)
}

fn render_appearance(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    use crate::renderer::color;
    use crate::theme::Mode;

    let preview_dark = app.preview_dark;
    let current_mode = crate::theme::mode();

    // ── Mode + Preview segmented controls ──────────────────────────────
    let mode_seg = {
        let mut row = div().flex().flex_row().gap_1();
        for m in Mode::ALL {
            let active = current_mode == m;
            let e = entity.clone();
            let name = m.name();
            row = row.child(
                Button::new(format!("appearance-mode-{}", name))
                    .variant(if active {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::Sm)
                    .child(m.label())
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                        gpui_app.stop_propagation();
                        if let Some(e) = e.upgrade() {
                            e.update(gpui_app, move |_this, cx| {
                                settings::set("appearance.mode", name.into());
                                cx.notify();
                            });
                        }
                    }),
            );
        }
        settings_row()
            .child(row_text(theme, "Mode", None))
            .child(row)
            .into_any_element()
    };

    let preview_seg = {
        let mk = |id: &'static str, label: &'static str, dark: bool| {
            let active = preview_dark == dark;
            let e = entity.clone();
            Button::new(id)
                .variant(if active {
                    ButtonVariant::Default
                } else {
                    ButtonVariant::Outline
                })
                .size(ButtonSize::Sm)
                .child(label)
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                    gpui_app.stop_propagation();
                    if let Some(e) = e.upgrade() {
                        e.update(gpui_app, move |this, cx| {
                            this.preview_dark = dark;
                            cx.notify();
                        });
                    }
                })
        };
        settings_row()
            .child(row_text(theme, "Preview", None))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(mk("appearance-preview-light", "Light", false))
                    .child(mk("appearance-preview-dark", "Dark", true)),
            )
            .into_any_element()
    };

    // ── Accent swatches ────────────────────────────────────────────────
    // macOS-style dot row: System (follows the OS accent) first, then the
    // mock's eight presets. The selected dot wears an ink ring.
    let accent_row = {
        use crate::theme::Accent;
        let current = crate::theme::accent_setting();
        let system_rgb =
            crate::theme::resolve_accent(Accent::System, crate::theme::system_accent());
        let mut swatches = div().flex().flex_row().items_center().gap(px(6.));
        for (ix, a) in Accent::ALL.into_iter().enumerate() {
            let active = current == a;
            let e = entity.clone();
            let name = a.name();
            let fill = color(a.rgb().unwrap_or(system_rgb), 1.0);
            let ring = if active { theme.foreground } else { gpui::transparent_black() };
            let mut dot = div().size_full().rounded_full().bg(fill);
            if a == Accent::System {
                // The follow-the-OS swatch: a hollow center so it reads as
                // "auto" rather than as one more fixed color.
                dot = dot.flex().items_center().justify_center().child(
                    div().size(px(5.)).rounded_full().bg(gpui::white()),
                );
            }
            swatches = swatches.child(
                div()
                    .id(("appearance-accent", ix))
                    .size(px(22.))
                    .p(px(2.))
                    .rounded_full()
                    .border_2()
                    .border_color(ring)
                    .cursor_pointer()
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                        gpui_app.stop_propagation();
                        if let Some(e) = e.upgrade() {
                            e.update(gpui_app, move |_this, cx| {
                                settings::set("accent", name.into());
                                cx.notify();
                            });
                        }
                    })
                    .child(dot),
            );
        }
        let desc: &'static str = match current {
            Accent::System => "System — follows macOS",
            other => other.label(),
        };
        settings_row()
            .child(row_text(theme, "Accent", Some(desc)))
            .child(swatches)
            .into_any_element()
    };

    // ── App theme selects ──────────────────────────────────────────────
    let theme_select = |which: AppearanceDropdown| -> AnyElement {
        let dark = which.dark();
        let opts = pages::theme_options(dark);
        let labels: Vec<String> = opts.iter().map(|t| t.label.to_string()).collect();
        let selected_name = crate::theme::selected(dark).name;
        let value = opts.iter().position(|t| t.name == selected_name);
        let open = app.appearance_menu == Some(which);
        let e_open = entity.clone();
        let e_change = entity.clone();
        let opts_for_change: Vec<&'static str> = opts.iter().map(|t| t.name).collect();
        let id = match which {
            AppearanceDropdown::ThemeLight => "appearance-theme-light",
            AppearanceDropdown::ThemeDark => "appearance-theme-dark",
            _ => "appearance-theme",
        };
        let label = if dark { "Dark theme" } else { "Light theme" };
        settings_row()
            .child(row_text(theme, label, None))
            .child(
                div().w(px(220.)).child(
                    Select::new(id)
                        .options(labels)
                        .value(value)
                        .open(open)
                        .on_open_change(move |is_open: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
                            let open = *is_open;
                            if let Some(e) = e_open.upgrade() {
                                e.update(gpui_app, move |this, cx| {
                                    this.appearance_menu = if open { Some(which) } else { None };
                                    cx.notify();
                                });
                            }
                        })
                        .on_change(move |ix: &usize, _win: &mut Window, gpui_app: &mut GpuiApp| {
                            let name = opts_for_change.get(*ix).copied().unwrap_or("");
                            if name.is_empty() {
                                return;
                            }
                            if let Some(e) = e_change.upgrade() {
                                e.update(gpui_app, move |_this, cx| {
                                    settings::set(crate::theme::setting_key(dark), name.into());
                                    cx.notify();
                                });
                            }
                        }),
                ),
            )
            .into_any_element()
    };

    // ── Terminal color selects ─────────────────────────────────────────
    let term_select = |which: AppearanceDropdown| -> AnyElement {
        let dark = which.dark();
        let opts = pages::term_options(dark);
        let labels: Vec<String> = opts
            .iter()
            .map(|t| t.map_or("Default".to_string(), |t| t.label.to_string()))
            .collect();
        let selected = crate::term_theme::selected(dark);
        let value = opts.iter().position(|t| match (t, selected) {
            (None, None) => true,
            (Some(a), Some(b)) => a.name == b.name,
            _ => false,
        });
        let open = app.appearance_menu == Some(which);
        let e_open = entity.clone();
        let e_change = entity.clone();
        let opts_names: Vec<Option<&'static str>> =
            opts.iter().map(|t| t.map(|t| t.name)).collect();
        let id = match which {
            AppearanceDropdown::TermLight => "appearance-term-light",
            AppearanceDropdown::TermDark => "appearance-term-dark",
            _ => "appearance-term",
        };
        let label = if dark { "Dark theme" } else { "Light theme" };
        settings_row()
            .child(row_text(theme, label, None))
            .child(
                div().w(px(220.)).child(
                    Select::new(id)
                        .options(labels)
                        .value(value)
                        .open(open)
                        .on_open_change(move |is_open: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
                            let open = *is_open;
                            if let Some(e) = e_open.upgrade() {
                                e.update(gpui_app, move |this, cx| {
                                    this.appearance_menu = if open { Some(which) } else { None };
                                    cx.notify();
                                });
                            }
                        })
                        .on_change(move |ix: &usize, _win: &mut Window, gpui_app: &mut GpuiApp| {
                            let name = opts_names
                                .get(*ix)
                                .copied()
                                .flatten()
                                .unwrap_or("default");
                            if let Some(e) = e_change.upgrade() {
                                e.update(gpui_app, move |_this, cx| {
                                    settings::set(
                                        crate::term_theme::setting_key(dark),
                                        name.into(),
                                    );
                                    cx.notify();
                                });
                            }
                        }),
                ),
            )
            .into_any_element()
    };

    // ── WYSIWYG preview cards ──────────────────────────────────────────
    let pt = crate::theme::selected(preview_dark);
    let app_preview = {
        let (pane_bg, pane_ink, pane_dim, pane_divider) =
            match crate::term_theme::selected(preview_dark) {
                Some(t) => (
                    color(t.bg, 1.0),
                    color(t.fg, 1.0),
                    color(t.fg, 0.55),
                    color(t.fg, 0.15),
                ),
                None => (
                    color(pt.term_bg, 1.0),
                    color(pt.text_bright, 1.0),
                    color(pt.text_dim, 1.0),
                    color(pt.card_divider, 1.0),
                ),
            };
        let tokens = [pt.gradient_from, pt.card, pt.term_bg, pt.accent, pt.ink];
        let mut chips = div().flex().flex_row().items_center().gap(px(6.));
        for c in tokens {
            chips = chips.child(
                div()
                    .w(px(14.))
                    .h(px(14.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(color(pt.ink, 0.25))
                    .bg(color(c, 1.0)),
            );
        }
        chips = chips.child(
            div()
                .text_size(px(11.))
                .text_color(color(pt.ink_dim, 1.0))
                .child("bg · surface · pane · accent · ink"),
        );

        let mut sidebar = div().flex().flex_col().gap(px(4.)).w(px(110.));
        for (i, name) in ["flaky tests", "stripe v4", "docs pass"].iter().enumerate() {
            let mut row = div()
                .px(px(7.))
                .py(px(4.))
                .rounded(px(6.))
                .text_size(px(11.))
                .text_color(color(if i == 0 { pt.ink } else { pt.ink_dim }, 1.0))
                .child(*name);
            if i == 0 {
                row = row.bg(color(pt.card, 0.9));
            }
            sidebar = sidebar.child(row);
        }

        let mini_term = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(70.))
            .rounded(px(8.))
            .bg(pane_bg)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(16.))
                    .px(px(10.))
                    .py(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(pane_ink)
                            .child("zsh"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(pane_dim)
                            .child("cargo"),
                    ),
            )
            .child(div().h(px(1.)).bg(pane_divider))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(6.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(pane_ink)
                            .child("$ cargo run"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(pane_dim)
                            .child("   Compiling pwrde"),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(220.))
            .rounded(theme.radius_lg())
            .bg(color(pt.gradient_from, 1.0))
            .p(px(12.))
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color(pt.ink, 0.25)))
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color(pt.ink, 0.25)))
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color(pt.ink, 0.25)))
                    .child(
                        div()
                            .ml(px(4.))
                            .text_size(px(11.))
                            .text_color(color(pt.ink_dim, 1.0))
                            .child(format!("{} · Preview", pt.label)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .flex_1()
                    .child(sidebar)
                    .child(mini_term),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .rounded(px(7.))
                    .bg(color(pt.card, 1.0))
                    .px(px(8.))
                    .py(px(6.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(color(pt.ink, 1.0))
                            .child("Surface card"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .text_size(px(11.))
                            .child(
                                div()
                                    .text_color(color(pt.ink_dim, 1.0))
                                    .child("Secondary text on surface · "),
                            )
                            .child(
                                div()
                                    .text_color(color(pt.accent, 1.0))
                                    .child("a link"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(color(pt.accent, 1.0))
                            .text_size(px(11.))
                            .text_color(color((255, 255, 255), 1.0))
                            .child("Primary"),
                    )
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(color(pt.ink, 0.08))
                            .text_size(px(11.))
                            .text_color(color(pt.ink, 1.0))
                            .child("Secondary"),
                    ),
            )
            .child(chips)
            .into_any_element()
    };

    let term_preview = {
        let sel = crate::term_theme::selected(preview_dark);
        let (tfg, tbg, ansi) = crate::term_theme::preview_colors(sel, pt.term_bg);
        let fgc = color(tfg, 1.0);
        let dimc = color(tfg, 0.55);
        let red = color(ansi[1], 1.0);
        let green = color(ansi[2], 1.0);
        let yellow = color(ansi[3], 1.0);
        let magenta = color(ansi[5], 1.0);
        let cyan = color(ansi[6], 1.0);

        let mut ansi_chips = div().flex().flex_row().items_center().gap(px(3.));
        for c in ansi {
            ansi_chips = ansi_chips.child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .rounded(px(2.))
                    .bg(color(c, 1.0)),
            );
        }

        let line = |spans: Vec<AnyElement>| {
            let mut row = div().flex().flex_row().text_size(px(11.));
            for s in spans {
                row = row.child(s);
            }
            row
        };
        let span = |text: &'static str, c: gpui::Hsla| {
            div().text_color(c).child(text).into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(220.))
            .rounded(theme.radius_lg())
            .bg(color(tbg, 1.0))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(12.))
                    .py(px(6.))
                    .bg(color((0, 0, 0), 0.18))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(color(tfg, 0.7))
                            .child(format!(
                                "{} · Preview",
                                sel.map_or("Default", |t| t.label)
                            )),
                    )
                    .child(ansi_chips),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(12.))
                    .py(px(8.))
                    .child(line(vec![
                        span("you@dev", green),
                        span(":~/checkout$", dimc),
                        span(" git status", fgc),
                    ]))
                    .child(line(vec![
                        span("On branch ", dimc),
                        span("feature/flaky-capture", cyan),
                    ]))
                    .child(line(vec![
                        span("  modified:  ", red),
                        span("tests/conftest.py", fgc),
                    ]))
                    .child(line(vec![
                        span("  new file:  ", green),
                        span("tests/test_clock.py", fgc),
                    ]))
                    .child(line(vec![
                        span("you@dev", green),
                        span(":~$", dimc),
                        span(" pytest -q", fgc),
                    ]))
                    .child(line(vec![
                        span("warning: ", yellow),
                        span("2 deprecation warnings", fgc),
                    ]))
                    .child(line(vec![
                        span("400 passed ", green),
                        span("0 failed", red),
                        span(" in ", fgc),
                        span("41.2s", magenta),
                    ]))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .text_size(px(11.))
                            .child(span("❯ ", magenta))
                            .child(span("agent watching e2e ", fgc))
                            .child(
                                div()
                                    .w(px(7.))
                                    .h(px(12.))
                                    .bg(color(tfg, 0.9)),
                            ),
                    ),
            )
            .into_any_element()
    };

    // ── Footer actions ─────────────────────────────────────────────────
    let import_e = entity.clone();
    let import_btn = Button::new("appearance-import")
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Sm)
        .child("Import from clipboard")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                if let Some(tokens) = clipboard
                    .get_text()
                    .ok()
                    .as_deref()
                    .and_then(crate::theme::parse_tokens)
                {
                    let dark = crate::theme::is_dark_color(tokens[0]);
                    if let Some(e) = import_e.upgrade() {
                        e.update(gpui_app, move |_this, cx| {
                            settings::set(
                                crate::theme::custom_key(dark),
                                crate::theme::serialize_tokens(&tokens).into(),
                            );
                            settings::set(
                                crate::theme::setting_key(dark),
                                crate::theme::custom_name(dark).into(),
                            );
                            cx.notify();
                        });
                    }
                }
            }
        });

    let copy_e = entity.clone();
    let copy_btn = Button::new("appearance-copy")
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Sm)
        .child("Copy theme tokens")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(crate::theme::export_current());
            }
            if let Some(e) = copy_e.upgrade() {
                e.update(gpui_app, |_this, cx| {
                    cx.notify();
                });
            }
        });

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .gap_4()
        .child(settings_group(theme, vec![mode_seg, accent_row, preview_seg]))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(group_label(theme, "App theme"))
                .child(settings_group(
                    theme,
                    vec![
                        theme_select(AppearanceDropdown::ThemeLight),
                        theme_select(AppearanceDropdown::ThemeDark),
                    ],
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(group_label(theme, "Terminal colors"))
                .child(settings_group(
                    theme,
                    vec![
                        term_select(AppearanceDropdown::TermLight),
                        term_select(AppearanceDropdown::TermDark),
                    ],
                )),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_4()
                .child(div().flex_1().min_w(px(0.)).child(app_preview))
                .child(div().flex_1().min_w(px(0.)).child(term_preview)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(import_btn)
                .child(copy_btn),
        )
        .into_any_element()
}
