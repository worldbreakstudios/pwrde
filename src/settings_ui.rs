//! Settings content as a gpui element tree.
//!
//! [`App::settings_content`] builds a plain flex column that fills whatever
//! parent it is placed in — today the Settings window's right-hand pane (the
//! window's own nav sidebar lives in `settings_window.rs`).
//!
//! Layout: a vertically scrollable column (max-width 720, horizontally
//! centered, padding 44/32/80, gap 26) holding a page header and one block
//! per group. A block is a small uppercase caption plus a rounded card of
//! rows; each row is a label/description cell on the left and a control on
//! the right, separated by hairline dividers. The five sections — General,
//! Appearance, Tools, Keyboard, Advanced — share the shell plus the control
//! helpers below (segmented control, toggle, stepper, key caps, accent
//! swatches, selects). Search results render on the same shell as a single
//! flat card. Colors derive from the rcn [`Theme`] via white/black alpha
//! overlays chosen by polarity; no hex ground colors are hard-coded.

use std::time::{Duration, Instant};

use gpui::{
    div, linear_color_stop, linear_gradient, px, AnyElement, App as GpuiApp, ClickEvent,
    ClipboardItem, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};

use crate::pages::{self, Action, AppearanceDropdown, Section};
use crate::settings;
use crate::ui::select::Select;
use crate::ui::theme::{alpha, Theme};
use crate::ui::{Badge, BadgeVariant, Switch};
use crate::App;

/// Settings rows read badly stretched across a fullscreen window; the content
/// column caps out at this width and centers in the overlay.
const CONTENT_MAX_W: f32 = 720.0;

/// How long the Diagnostics "Copied" label stays up after a copy.
const COPIED_FOR: Duration = Duration::from_millis(1500);

static COPIED_AT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

// ── Page shell ──────────────────────────────────────────────────────────

impl App {
    /// The Settings content column: a flex column that fills its parent and
    /// hosts the 720px centred scroll column. The Settings window renders it as
    /// its right-hand pane.
    pub fn settings_content(&self, cx: &mut Context<Self>) -> AnyElement {
        // Keep the rcn Theme global in sync with live chrome tokens.
        cx.set_global(crate::ui::theme::Theme::from_chrome(crate::theme::current()));

        let theme = Theme::of(cx).clone();
        let entity = cx.entity().downgrade();

        // Style the vendored Inputs down to the rows' type size; the vendored
        // field keeps its own border + focus ring.
        self.command_input.update(cx, |i, _| {
            i.set_text_size(Some(px(12.5)));
        });
        for input in self.tool_form.inputs() {
            input.update(cx, |i, _| i.set_text_size(Some(px(12.5))));
        }

        let searching = !self.settings_query.is_empty();
        let title: String = if searching {
            "Search results".into()
        } else {
            self.section.label().into()
        };
        let subtitle: Option<&'static str> = if searching {
            None
        } else {
            Some(match self.section {
                Section::General => "How sessions start, persist and talk to your forge.",
                Section::Appearance => "Theme, accent, text size and terminal colors.",
                Section::Tools => {
                    "Each tool gets a sidebar page running its command in a fresh terminal. \
                     Not persisted."
                }
                Section::Keyboard => "Click a shortcut to rebind it. Press Esc to cancel, ⌫ to clear.",
                Section::Advanced => {
                    "Experiments and diagnostics. Things here may change or disappear."
                }
            })
        };

        let body = if searching {
            render_search_results(self, &theme, entity.clone())
        } else {
            match self.section {
                Section::General => render_general(self, &theme, entity.clone()),
                Section::Appearance => render_appearance(self, &theme, entity.clone()),
                Section::Tools => render_tools(self, &theme, entity.clone()),
                Section::Keyboard => render_keyboard(self, &theme, entity.clone()),
                Section::Advanced => render_advanced(self, &theme, entity.clone()),
            }
        };

        let mut header = div().flex().flex_col();
        header = header.child(
            div()
                .text_size(px(22.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(title),
        );
        if let Some(sub) = subtitle {
            if !sub.is_empty() {
                header = header.child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.5))
                        .text_color(theme.muted_foreground)
                        .child(sub),
                );
            }
        }
        // The Keyboard page header carries the "Restore defaults" pill on the
        // right; other sections keep the header alone.
        let header = if !searching && self.section == Section::Keyboard {
            div()
                .flex()
                .flex_row()
                .items_end()
                .justify_between()
                .child(header)
                .child(restore_defaults_pill(&theme, &entity))
                .into_any_element()
        } else {
            header.into_any_element()
        };

        let bg_entity = entity.clone();
        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h(px(0.))
            .bg(theme.background)
            // Any press in the page first drops transient input state — an
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
                div()
                    .id("settings-scroll")
                    .flex()
                    .flex_col()
                    .h_full()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(26.))
                            .w_full()
                            .max_w(px(CONTENT_MAX_W))
                            .mx_auto()
                            .pt(px(44.))
                            .px(px(32.))
                            .pb(px(80.))
                            .child(header)
                            .child(body),
                    ),
            )
            .into_any_element()
    }

}

// ── Shared shell helpers ────────────────────────────────────────────────

/// A white overlay in dark polarity, a black overlay in light polarity —
/// fills and borders derive from this, never from hard-coded hex colors.
fn overlay(theme: &Theme, a: f32) -> gpui::Hsla {
    if theme.dark {
        alpha(gpui::white(), a)
    } else {
        alpha(gpui::black(), a)
    }
}

/// Inset surfaces (segment wells, icon chips) read darker: black alpha.
fn inset(_theme: &Theme, a: f32) -> gpui::Hsla {
    alpha(gpui::black(), a)
}

/// The card's fill/border alphas by polarity.
fn card_fill(theme: &Theme) -> gpui::Hsla {
    overlay(theme, if theme.dark { 0.035 } else { 0.03 })
}

fn card_border(theme: &Theme) -> gpui::Hsla {
    overlay(theme, if theme.dark { 0.07 } else { 0.08 })
}

/// An 11px uppercase section caption above a card.
fn settings_caption(theme: &Theme, text: &'static str) -> gpui::Div {
    div()
        .pl(px(12.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .child(text.to_uppercase())
}

/// A caption plus an optional right-aligned control on one line.
fn caption_row(theme: &Theme, text: &'static str, right: Option<AnyElement>) -> gpui::Div {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(settings_caption(theme, text));
    if let Some(r) = right {
        row = row.child(r);
    }
    row
}

/// A rounded card of settings rows with hairline dividers between them
/// (never above the first).
fn settings_card(theme: &Theme, rows: Vec<AnyElement>) -> gpui::Div {
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(12.))
        .border_1()
        .border_color(card_border(theme))
        .bg(card_fill(theme))
        .overflow_hidden();
    for (ix, row) in rows.into_iter().enumerate() {
        if ix > 0 {
            card = card.child(div().h(px(1.)).flex_shrink_0().bg(overlay(theme, 0.06)));
        }
        card = card.child(row);
    }
    card
}

/// One block: a caption and its card.
fn settings_block(theme: &Theme, caption: &'static str, rows: Vec<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(caption_row(theme, caption, None))
        .child(settings_card(theme, rows))
}

/// A settings row: label/description on the left, control on the right.
fn settings_row() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(16.))
        .px(px(12.))
        .py(px(11.))
        .w_full()
}

/// A row's left cell: a 13px title with an optional 11.5px muted description
/// under it — the shape every settings row shares.
fn row_text(theme: &Theme, title: impl Into<gpui::SharedString>, desc: Option<&str>) -> gpui::Div {
    let mut cell = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .text_size(px(13.))
        .text_color(theme.foreground)
        .child(title.into());
    if let Some(desc) = desc {
        cell = cell.child(
            div()
                .mt(px(2.))
                .text_size(px(11.5))
                .text_color(theme.muted_foreground)
                .child(desc.to_string()),
        );
    }
    cell
}

// ── Controls ────────────────────────────────────────────────────────────

/// A segmented control writing one settings key: a dark well holding pill
/// segments; the active one wears the accent. `mono` renders the labels in
/// the monospace face (the lfg | gh CLI picker).
fn segmented(
    theme: &Theme,
    id: &'static str,
    key: &'static str,
    options: &[(&'static str, &'static str)],
    active_ix: usize,
    mono: bool,
    entity: &gpui::WeakEntity<App>,
) -> gpui::Div {
    let hover = overlay(theme, 0.04);
    let mut out = div()
        .flex()
        .flex_row()
        .p(px(2.))
        .gap(px(3.))
        .rounded(px(8.))
        .bg(inset(theme, if theme.dark { 0.25 } else { 0.05 }))
        .border_1()
        .border_color(alpha(gpui::black(), 0.08));
    for (ix, (label, value)) in options.iter().enumerate() {
        let value: &'static str = *value;
        let active = ix == active_ix;
        let e = entity.clone();
        let mut seg = div()
            .id((id, ix as u64))
            .px(px(11.))
            .py(px(4.))
            .rounded(px(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(*label);
        if mono {
            seg = seg.font_family("monospace");
        }
        if active {
            seg = seg.bg(theme.primary).text_color(theme.primary_foreground);
        } else {
            seg = seg.text_color(theme.secondary_foreground).hover(move |s| s.bg(hover));
        }
        seg = seg.on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(e) = e.upgrade() {
                e.update(gpui_app, move |_this, cx| {
                    settings::set(key, (*value).into());
                    cx.notify();
                });
            }
        });
        out = out.child(seg);
    }
    out
}

/// A toggle row bound to one settings key; `snapshot` additionally snapshots
/// the workspace tree after the write (the shpool persistence knob);
/// `disabled` dims the whole row to 45% and swallows the toggle.
fn toggle_row(
    theme: &Theme,
    title: &'static str,
    desc: &'static str,
    id: &'static str,
    key: &'static str,
    checked: bool,
    snapshot: bool,
    disabled: bool,
    entity: &gpui::WeakEntity<App>,
) -> AnyElement {
    let e = entity.clone();
    let mut toggle = Switch::new(id).checked(checked);
    if disabled {
        toggle = toggle.disabled(true);
    }
    toggle = toggle.on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
        gpui_app.stop_propagation();
        if disabled {
            return;
        }
        let on = *checked;
        if let Some(e) = e.upgrade() {
            e.update(gpui_app, move |this, cx| {
                settings::set(key, on.into());
                if snapshot {
                    this.persist_snapshot();
                }
                cx.notify();
            });
        }
    });
    let mut row = settings_row()
        .child(row_text(theme, title, Some(desc)))
        .child(toggle);
    if disabled {
        row = row.opacity(0.45);
    }
    row.into_any_element()
}

/// A small square step button for the font-size steppers.
fn step_button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    glyph: &'static str,
    key: &'static str,
    delta: f32,
    entity: &gpui::WeakEntity<App>,
) -> gpui::Stateful<gpui::Div> {
    let hover = overlay(theme, 0.04);
    let e = entity.clone();
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .text_size(px(13.))
        .text_color(theme.secondary_foreground)
        .border_1()
        .border_color(overlay(theme, 0.08))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
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
}

/// A font-size stepper row: `−  N px  +  Reset` bound to one settings key.
fn stepper_row(
    theme: &Theme,
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    key: &'static str,
    entity: &gpui::WeakEntity<App>,
) -> AnyElement {
    let cur = settings::get_f32(key, crate::renderer::FONT_SIZE);
    let off_default = (cur - crate::renderer::FONT_SIZE).abs() > f32::EPSILON;
    let e = entity.clone();
    let reset = div()
        .id(gpui::SharedString::from(format!("{id}-reset")))
        .text_size(px(11.5))
        .cursor_pointer()
        .child("Reset")
        .text_color(if off_default {
            theme.primary
        } else {
            alpha(theme.muted_foreground, 0.25)
        })
        .hover(move |s| s.text_color(theme.primary))
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(e) = e.upgrade() {
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
                .gap(px(6.))
                .child(step_button(
                    theme,
                    (id, 0u64),
                    "−",
                    key,
                    -crate::renderer::FONT_SIZE_STEP,
                    entity,
                ))
                .child(
                    div()
                        .w(px(46.))
                        .flex()
                        .justify_center()
                        .text_size(px(12.5))
                        .font_family("monospace")
                        .text_color(theme.foreground)
                        .child(format!("{} px", cur as i32)),
                )
                .child(step_button(
                    theme,
                    (id, 1u64),
                    "+",
                    key,
                    crate::renderer::FONT_SIZE_STEP,
                    entity,
                ))
                .child(reset),
        )
        .into_any_element()
}

/// One key cap: a small bordered chip for a modifier glyph or key name.
fn key_cap(theme: &Theme, label: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .text_size(px(11.))
        .text_color(theme.secondary_foreground)
        .rounded(px(5.))
        .border_1()
        .border_color(overlay(theme, 0.14))
        .border_b_2()
        .bg(overlay(theme, 0.06))
        .child(label.into())
}

/// The key caps for a chord: one per modifier glyph (⌃ ⌥ ⇧ ⌘) plus the key.
fn chord_caps(b: &pages::Binding) -> Vec<String> {
    let mut caps = Vec::new();
    if b.ctrl {
        caps.push("⌃".to_string());
    }
    if b.alt {
        caps.push("⌥".to_string());
    }
    if b.shift {
        caps.push("⇧".to_string());
    }
    caps.push("⌘".to_string());
    caps.push(key_glyph(&b.key));
    caps
}

fn key_glyph(key: &str) -> String {
    match key {
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "minus" => "-".into(),
        k => k.to_uppercase(),
    }
}

/// The Beta badge shown next to an experiment's name.
fn beta_badge(theme: &Theme) -> gpui::Div {
    div()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .text_size(px(9.5))
        .font_weight(FontWeight::BOLD)
        .text_color(theme.primary)
        .border_1()
        .border_color(alpha(theme.primary, 0.5))
        .child("BETA")
}

// ── General ─────────────────────────────────────────────────────────────

fn render_general(_app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(26.))
        .child(settings_block(
            theme,
            "Sessions",
            vec![
                settings_row()
                    .child(row_text(
                        theme,
                        "Primary command",
                        Some("Runs in the primary pane when a group opens."),
                    ))
                    .child(
                        div()
                            .w(px(220.))
                            .font_family("monospace")
                            .child(_app.command_input.clone()),
                    )
                    .into_any_element(),
                toggle_row(
                    theme,
                    "Persist sessions",
                    "Panes survive restarts via shpool. Sessions, folders and layouts always \
                     persist.",
                    "persist-sessions",
                    "terminal.persist",
                    settings::persist_sessions(),
                    true,
                    false,
                    &entity,
                ),
            ],
        ))
        .child(settings_block(
            theme,
            "Pull requests",
            vec![
                settings_row()
                    .child(row_text(
                        theme,
                        "Pull request CLI",
                        Some("Tool used to fetch PR data. lfg is the fast, cached path."),
                    ))
                    .child({
                        let current = crate::gh::cli();
                        let options = [("lfg", "lfg"), ("gh", "gh")];
                        let active = options
                            .iter()
                            .position(|(value, _)| *value == current)
                            .unwrap_or(0);
                        segmented(theme, "git-cli", "git.cli", &options, active, true, &entity)
                    })
                    .into_any_element(),
                toggle_row(
                    theme,
                    "Async streaming",
                    "Stream results as they arrive (lfg -A). Requires lfg.",
                    "git-async",
                    "git.async",
                    crate::gh::async_enabled(),
                    false,
                    crate::gh::cli() != "lfg",
                    &entity,
                ),
            ],
        ))
        .into_any_element()
}

// ── Appearance ──────────────────────────────────────────────────────────

fn render_appearance(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    use crate::renderer::color;
    use crate::theme::{Accent, Mode};

    let preview_dark = app.preview_dark;
    let current_mode = crate::theme::mode();

    // ── Mode segmented control ─────────────────────────────────────────
    let mode_seg = {
        let options: Vec<(&'static str, &'static str)> =
            Mode::ALL.iter().map(|m| (m.label(), m.name())).collect();
        let active = Mode::ALL
            .iter()
            .position(|m| *m == current_mode)
            .unwrap_or(0);
        settings_row()
            .child(row_text(theme, "Mode", None))
            .child(segmented(
                theme,
                "appearance-mode",
                "appearance.mode",
                &options,
                active,
                false,
                &entity,
            ))
            .into_any_element()
    };

    // ── Accent swatches ────────────────────────────────────────────────
    // System (follows the OS accent) first as a rainbow swatch, then the
    // eight presets. The selected dot wears a 2px swatch-colored ring
    // separated from the dot by a 2px ground-colored gap.
    let accent_row = {
        let current = crate::theme::accent_setting();
        let system_rgb =
            crate::theme::resolve_accent(Accent::System, crate::theme::system_accent());
        let ground = theme.background;
        let mut swatches = div().flex().flex_row().items_center().gap(px(10.));
        for (ix, a) in Accent::ALL.into_iter().enumerate() {
            let active = current == a;
            let e = entity.clone();
            let name = a.name();
            let swatch = color(a.rgb().unwrap_or(system_rgb), 1.0);
            let dot: gpui::AnyElement = if a == Accent::System {
                // gpui has no conic gradient; approximate the rainbow with a
                // diagonal two-stop linear gradient.
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(linear_gradient(
                        135.,
                        linear_color_stop(color((255, 59, 48), 1.0), 0.),
                        linear_color_stop(color((0, 122, 255), 1.0), 1.),
                    ))
                    .into_any_element()
            } else {
                div().size(px(16.)).rounded_full().bg(swatch).into_any_element()
            };
            let mut ring = div()
                .id(("appearance-accent", ix as u64))
                .size(px(24.))
                .rounded_full()
                .p(px(2.))
                .bg(ground)
                .border_2()
                .border_color(if active {
                    swatch
                } else {
                    gpui::transparent_black()
                })
                .cursor_pointer()
                .child(dot);
            ring = ring.on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |_this, cx| {
                        settings::set("accent", name.into());
                        cx.notify();
                    });
                }
            });
            swatches = swatches.child(ring);
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

    // ── Text size steppers ─────────────────────────────────────────────
    let term_size = stepper_row(
        theme,
        "term-font",
        "Terminal",
        "Font size of the terminal grid. ⌘= / ⌘− while a terminal is focused.",
        "terminal.font_size",
        &entity,
    );
    let app_size = stepper_row(
        theme,
        "app-font",
        "App chrome",
        "Tabs, sidebar and other UI. ⌘= / ⌘− elsewhere.",
        "appearance.font_size",
        &entity,
    );

    // ── Terminal theme selects ─────────────────────────────────────────
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
        };
        let label = if dark { "Dark theme" } else { "Light theme" };
        settings_row()
            .child(row_text(theme, label, None))
            .child(
                div().w(px(180.)).child(
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
    let polarity = if preview_dark { "Dark" } else { "Light" };

    let app_preview = {
        let ink = color(pt.ink, 1.0);
        let ink_dim = color(pt.ink_dim, 1.0);
        let surface = color(pt.card, 1.0);
        let accent = color(pt.accent, 1.0);
        let (pane_bg, pane_ink, pane_dim) = match crate::term_theme::selected(preview_dark) {
            Some(t) => (color(t.bg, 1.0), color(t.fg, 1.0), color(t.fg, 0.55)),
            None => (
                color(pt.term_bg, 1.0),
                color(pt.text_bright, 1.0),
                color(pt.text_dim, 1.0),
            ),
        };

        let mut sidebar = div().flex().flex_col().gap(px(4.)).w(px(82.));
        for (i, name) in ["flaky tests", "stripe v4", "docs pass"].iter().enumerate() {
            let mut chip = div()
                .px(px(7.))
                .py(px(4.))
                .rounded(px(6.))
                .text_size(px(10.5))
                .truncate()
                .text_color(if i == 0 { ink } else { ink_dim })
                .child(*name);
            if i == 0 {
                chip = chip.bg(surface);
            }
            sidebar = sidebar.child(chip);
        }

        let mini_term = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(70.))
            .rounded(px(8.))
            .bg(pane_bg)
            .border_1()
            .border_color(overlay(theme, 0.1))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(6.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(pane_dim)
                            .child("$ cargo run"),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(pane_ink)
                            .child("Compiling pwrde"),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(200.))
            .rounded(px(12.))
            .border_1()
            .border_color(card_border(theme))
            .bg(color(pt.gradient_from, 1.0))
            .p(px(12.))
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(div().size(px(7.)).rounded_full().bg(ink_dim))
                    .child(div().size(px(7.)).rounded_full().bg(ink_dim))
                    .child(div().size(px(7.)).rounded_full().bg(ink_dim))
                    .child(
                        div()
                            .ml(px(4.))
                            .text_size(px(10.5))
                            .text_color(ink_dim)
                            .child(format!("{polarity} · App")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .flex_1()
                    .child(sidebar)
                    .child(mini_term),
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
                            .bg(accent)
                            .text_size(px(10.5))
                            .text_color(gpui::white())
                            .child("Primary"),
                    )
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(surface)
                            .text_size(px(10.5))
                            .text_color(ink)
                            .child("Secondary"),
                    ),
            )
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
        let blue = color(ansi[4], 1.0);

        let mut ansi_squares = div().flex().flex_row().items_center().gap(px(3.));
        for c in ansi.iter().take(6) {
            ansi_squares = ansi_squares.child(
                div().size(px(7.)).rounded(px(2.)).bg(color(*c, 1.0)),
            );
        }

        let line = |spans: Vec<AnyElement>| {
            let mut row = div()
                .flex()
                .flex_row()
                .text_size(px(10.5))
                .font_family("monospace")
                .line_height(gpui::relative(1.7));
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
            .min_h(px(200.))
            .rounded(px(12.))
            .border_1()
            .border_color(card_border(theme))
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
                    .bg(alpha(gpui::black(), 0.18))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(color(tfg, 0.7))
                            .child(format!(
                                "{} · Terminal",
                                sel.map_or("Default", |t| t.label)
                            )),
                    )
                    .child(ansi_squares),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(12.))
                    .py(px(8.))
                    .child(line(vec![span("$ ", dimc), span("cargo test", fgc)]))
                    .child(line(vec![
                        span("Compiling ", green),
                        span("pwrde v0.1.0", fgc),
                    ]))
                    .child(line(vec![
                        span("warning: ", yellow),
                        span("unused import", fgc),
                    ]))
                    .child(line(vec![
                        span("error[E0425]: ", red),
                        span("cannot find value", fgc),
                    ]))
                    .child(line(vec![span("note: ", blue), span("see the docs", dimc)])),
            )
            .into_any_element()
    };

    // The Light | Dark preview toggle rides the "Terminal colors" caption.
    let preview_seg = {
        let well_hover = overlay(theme, 0.04);
        let mk = |id: &'static str, label: &'static str, dark: bool| {
            let active = preview_dark == dark;
            let e = entity.clone();
            let mut seg = div()
                .id(id)
                .px(px(9.))
                .py(px(2.))
                .rounded(px(6.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .child(label);
            if active {
                seg = seg.bg(theme.primary).text_color(theme.primary_foreground);
            } else {
                seg = seg.text_color(theme.secondary_foreground).hover(move |s| s.bg(well_hover));
            }
            seg = seg.on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |this, cx| {
                        this.preview_dark = dark;
                        cx.notify();
                    });
                }
            });
            seg
        };
        div()
            .flex()
            .flex_row()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(8.))
            .bg(inset(theme, if theme.dark { 0.25 } else { 0.05 }))
            .border_1()
            .border_color(alpha(gpui::black(), 0.08))
            .child(mk("appearance-preview-light", "Light", false))
            .child(mk("appearance-preview-dark", "Dark", true))
            .into_any_element()
    };

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .gap(px(26.))
        .child(settings_block(theme, "Theme", vec![mode_seg, accent_row]))
        .child(settings_block(theme, "Text size", vec![term_size, app_size]))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(caption_row(theme, "Terminal colors", Some(preview_seg)))
                .child(settings_card(
                    theme,
                    vec![
                        term_select(AppearanceDropdown::TermLight),
                        term_select(AppearanceDropdown::TermDark),
                    ],
                )),
        )
        .child(
            div()
                .mt(px(4.))
                .flex()
                .flex_row()
                .gap(px(12.))
                .child(app_preview)
                .child(term_preview),
        )
        .into_any_element()
}

// ── Tools ───────────────────────────────────────────────────────────────

fn render_tools(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let remove_hover = theme.destructive;
    let mut rows: Vec<AnyElement> = Vec::new();
    for (ix, tool) in app.tools.iter().enumerate() {
        let remove_entity = entity.clone();
        let remove = div()
            .id(("tool-remove", ix as u64))
            .size(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.))
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(move |s| s.text_color(remove_hover))
            .child("✕")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                if let Some(entity) = remove_entity.upgrade() {
                    entity.update(gpui_app, move |this, cx| {
                        this.remove_tool(ix);
                        cx.notify();
                    });
                }
            });
        rows.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .py(px(10.))
                .child(
                    // 26×26 icon chip
                    div()
                        .size(px(26.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .bg(inset(theme, 0.05))
                        .text_size(px(12.))
                        .font_family("monospace")
                        .child(tool.icon.clone()),
                )
                .child(
                    div()
                        .flex_basis(px(0.))
                        .flex_grow(1.1)
                        .min_w(px(0.))
                        .text_size(px(13.))
                        .text_color(theme.foreground)
                        .truncate()
                        .child(tool.name.clone()),
                )
                .child(
                    div()
                        .flex_basis(px(0.))
                        .flex_grow(1.4)
                        .min_w(px(0.))
                        .text_size(px(12.))
                        .font_family("monospace")
                        .text_color(theme.secondary_foreground)
                        .truncate()
                        .child(tool.command.clone()),
                )
                .child(
                    div()
                        .flex_basis(px(0.))
                        .flex_grow(1.0)
                        .min_w(px(0.))
                        .text_size(px(12.))
                        .font_family("monospace")
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(tool.cwd.clone()),
                )
                .child(remove)
                .into_any_element(),
        );
    }
    if rows.is_empty() {
        rows.push(
            div()
                .px(px(12.))
                .py(px(18.))
                .text_size(px(12.5))
                .text_color(theme.muted_foreground)
                .child("No tools yet.")
                .into_any_element(),
        );
    }

    let field =
        |label: &'static str, input: &gpui::Entity<crate::ui::Input>, grow: f32| {
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .flex_basis(px(0.))
                .flex_grow(grow)
                .min_w(px(0.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(input.clone())
                .into_any_element()
        };

    let add_entity = entity;
    let add = div()
        .id("tool-add")
        .px(px(12.))
        .py(px(6.))
        .rounded(px(7.))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .bg(theme.primary)
        .cursor_pointer()
        .child("Add tool")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(e) = add_entity.upgrade() {
                e.update(gpui_app, move |this, cx| {
                    let name = this.tool_form.name.read(cx).text().trim().to_string();
                    let command = this.tool_form.command.read(cx).text().trim().to_string();
                    if name.is_empty() || command.is_empty() {
                        return;
                    }
                    this.add_tool_from_form(cx);
                });
            }
        });

    let form = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(12.))
                .w_full()
                .child(field("Name", &app.tool_form.name, 1.1))
                .child(field("Command", &app.tool_form.command, 1.6))
                .child(field("Directory", &app.tool_form.cwd, 1.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .w(px(64.))
                        .flex_shrink_0()
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted_foreground)
                                .child("Icon"),
                        )
                        .child(app.tool_form.icon.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(11.5))
                        .text_color(theme.muted_foreground)
                        .child(
                            "Directory accepts ~. Icon is any text — Nerd Font glyphs render \
                             like the built-ins.",
                        ),
                )
                .child(add),
        );

    div()
        .flex()
        .flex_col()
        .gap(px(26.))
        .child(settings_block(theme, "Installed", rows))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(settings_caption(theme, "Add a tool"))
                .child(
                    div()
                        .w_full()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(card_border(theme))
                        .bg(card_fill(theme))
                        .p(px(12.))
                        .overflow_hidden()
                        .child(form),
                ),
        )
        .into_any_element()
}

// ── Keyboard ────────────────────────────────────────────────────────────

/// The Keyboard section's groups, in display order.
const KEYBOARD_GROUPS: [&str; 5] = ["Layout", "Tabs & sessions", "Focus", "Editing", "App"];

fn render_keyboard(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let query = app.settings_query.to_lowercase();
    let mut blocks: Vec<AnyElement> = Vec::new();

    for group in KEYBOARD_GROUPS {
        let actions: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| a.keyboard_group() == group)
            .filter(|a| {
                query.is_empty()
                    || a.label().to_lowercase().contains(&query)
                    || group.to_lowercase().contains(&query)
            })
            .collect();
        if actions.is_empty() {
            continue;
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, action) in actions.into_iter().enumerate() {
            rows.push(keyboard_row(
                theme,
                ix,
                action,
                app.recording == Some(action),
                &entity,
            ));
        }
        blocks.push(settings_block(theme, group, rows).into_any_element());
    }

    let body: AnyElement = if blocks.is_empty() {
        div()
            .pl(px(12.))
            .text_size(px(12.5))
            .text_color(theme.muted_foreground)
            .child(format!("No shortcuts match “{}”.", app.settings_query))
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(26.))
            .children(blocks)
            .into_any_element()
    };

    div()
        .id("settings-rows")
        .flex()
        .flex_col()
        .child(body)
        .into_any_element()
}

/// One shortcut row: the label on the left, an accent dot when the binding is
/// customized, and the chord as key caps (or a "Press keys…" pill while
/// recording). Clicking the row arms or disarms the recorder.
fn keyboard_row(
    theme: &Theme,
    ix: usize,
    action: Action,
    recording: bool,
    entity: &gpui::WeakEntity<App>,
) -> AnyElement {
    let row_entity = entity.clone();
    let right: AnyElement = if recording {
        div()
            .px(px(9.))
            .py(px(3.))
            .rounded(px(6.))
            .text_size(px(11.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(gpui::white())
            .bg(theme.primary)
            .child("Press keys…")
            .into_any_element()
    } else {
        let binding = action.binding();
        if binding.key.is_empty() {
            key_cap(theme, "—").into_any_element()
        } else {
            let mut caps = div().flex().flex_row().items_center().gap(px(3.));
            for cap in chord_caps(&binding) {
                caps = caps.child(key_cap(theme, cap));
            }
            caps.into_any_element()
        }
    };

    let customized = action.binding() != action.default_binding();
    let mut row = settings_row()
        .id(("kbd-row", ix as u64))
        .cursor_pointer()
        .py(px(0.))
        .h(px(38.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(13.))
                .text_color(theme.foreground)
                .child(action.label()),
        )
        .child(
            div()
                .size(px(6.))
                .flex_shrink_0()
                .rounded_full()
                .bg(if customized {
                    theme.primary
                } else {
                    gpui::transparent_black()
                }),
        )
        .child(right);
    if recording {
        row = row.bg(overlay(theme, 0.05));
    }
    row = row.on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
        gpui_app.stop_propagation();
        if let Some(entity) = row_entity.upgrade() {
            entity.update(gpui_app, move |this, cx| {
                this.recording = if this.recording == Some(action) {
                    None
                } else {
                    Some(action)
                };
                cx.notify();
            });
        }
    });
    row.into_any_element()
}

/// The "Restore defaults" pill: writes every action's default binding back
/// (settings.rs has no unset API; re-serializing the default keeps the value
/// shape identical).
fn restore_defaults_pill(
    theme: &Theme,
    entity: &gpui::WeakEntity<App>,
) -> gpui::Stateful<gpui::Div> {
    let hover = overlay(theme, 0.09);
    let e = entity.clone();
    div()
        .id("restore-defaults")
        .px(px(10.))
        .py(px(5.))
        .rounded_full()
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.primary)
        .border_1()
        .border_color(overlay(theme, 0.12))
        .bg(overlay(theme, 0.05))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child("Restore defaults")
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            if let Some(e) = e.upgrade() {
                e.update(gpui_app, move |_this, cx| {
                    for action in Action::ALL {
                        settings::set(
                            &action.setting_key(),
                            action.default_binding().serialize().into(),
                        );
                    }
                    cx.notify();
                });
            }
        })
}

// ── Advanced ────────────────────────────────────────────────────────────

fn render_advanced(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    // Flow's in-app description (the features.rs text names a source file the
    // settings page shouldn't; the brief's string wins).
    const FLOW_DESC: &str =
        "Bottom command bar that drives this workspace through an embedded agent. ⌘J";

    let mut rows: Vec<AnyElement> = Vec::new();
    for flag in crate::features::ALL {
        let desc: &str = if flag.key == crate::features::FLOW {
            FLOW_DESC
        } else {
            flag.description
        };
        let label_cell = || {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(13.))
                        .text_color(theme.foreground)
                        .child(flag.label)
                        .child(beta_badge(theme)),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(11.5))
                        .text_color(theme.muted_foreground)
                        .child(desc),
                )
        };
        let key = flag.key;
        let e = entity.clone();
        let toggle = Switch::new(gpui::SharedString::from(format!("flag-{key}")))
            .checked(crate::features::enabled(key))
            .on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                let on = *checked;
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |_this, cx| {
                        settings::set(&format!("features.{key}"), on.into());
                        cx.notify();
                    });
                }
            });
        rows.push(settings_row().child(label_cell()).child(toggle).into_any_element());
    }

    // Show frame stats (debug.overlay).
    {
        let e = entity.clone();
        let toggle = Switch::new("debug-overlay")
            .checked(settings::get_bool("debug.overlay", false))
            .on_change(move |checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
                gpui_app.stop_propagation();
                let on = *checked;
                if let Some(e) = e.upgrade() {
                    e.update(gpui_app, move |_this, cx| {
                        settings::set("debug.overlay", on.into());
                        cx.notify();
                    });
                }
            });
        rows.push(
            settings_row()
                .child(row_text(
                    theme,
                    "Show frame stats",
                    Some("Overlay render timing in the corner of each terminal."),
                ))
                .child(toggle)
                .into_any_element(),
        );
    }

    // ── Diagnostics ────────────────────────────────────────────────────
    let th = crate::theme::current();
    let (sw, sh) = app.renderer.surface_size();
    let diags: [(&str, String); 7] = [
        (
            "Settings file",
            settings::path().to_string_lossy().into_owned(),
        ),
        (
            "Chrome",
            format!(
                "{} · accent #{:02x}{:02x}{:02x}",
                th.label, th.accent.0, th.accent.1, th.accent.2
            ),
        ),
        ("Scale", format!("{:.2}", app.renderer.scale)),
        ("Surface", format!("{}×{} px", sw, sh)),
        (
            "Cell",
            format!("{}×{} px", app.renderer.cell_width, app.renderer.cell_height),
        ),
        ("Workspaces", app.workspaces.len().to_string()),
        (
            "Tiles (active group)",
            app.workspaces
                .get(app.active)
                .map(|ws| ws.root.tiles().len().to_string())
                .unwrap_or_else(|| "—".into()),
        ),
    ];
    let diag_rows: Vec<AnyElement> = diags
        .iter()
        .map(|(key, value)| {
            settings_row()
                .py(px(9.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(12.5))
                        .text_color(theme.muted_foreground)
                        .child(*key),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .max_w(px(420.))
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_family("monospace")
                                .text_color(theme.foreground)
                                .truncate()
                                .child(value.clone()),
                        ),
                )
                .into_any_element()
        })
        .collect();

    let copy_text = diags
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let copied = COPIED_AT
        .lock()
        .ok()
        .and_then(|g| *g)
        .is_some_and(|t| t.elapsed() < COPIED_FOR);
    let e = entity.clone();
    let copy_all = div()
        .id("copy-all")
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.primary)
        .cursor_pointer()
        .child(if copied { "Copied" } else { "Copy all" })
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
            gpui_app.stop_propagation();
            gpui_app.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
            if let Ok(mut at) = COPIED_AT.lock() {
                *at = Some(Instant::now());
            }
            if let Some(e) = e.upgrade() {
                let eid = e.entity_id();
                e.update(gpui_app, move |_this, cx| {
                    cx.notify();
                    cx.spawn_in(_win, async move |_this, cx| {
                        cx.background_executor().timer(COPIED_FOR).await;
                        let _ = cx.update(|_win, cx| cx.notify(eid));
                    })
                    .detach();
                });
            }
        });

    let reveal = div()
        .id("reveal-settings")
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.primary)
        .cursor_pointer()
        .child("Reveal settings.json")
        .hover(|s| s.opacity(0.8))
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, _gpui_app: &mut GpuiApp| {
            let _ = std::process::Command::new("open")
                .arg("-R")
                .arg(settings::path())
                .spawn();
        });

    div()
        .flex()
        .flex_col()
        .gap(px(26.))
        .child(settings_block(theme, "Experiments", rows))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(caption_row(theme, "Diagnostics", Some(copy_all.into_any_element())))
                .child(settings_card(theme, diag_rows))
                .child(div().pl(px(12.)).mt(px(2.)).child(reveal)),
        )
        .into_any_element()
}

// ── Search results ──────────────────────────────────────────────────────

fn render_search_results(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    let results = pages::search_settings(&app.settings_query);
    if results.is_empty() {
        return div()
            .pl(px(12.))
            .text_size(px(12.5))
            .text_color(theme.muted_foreground)
            .child("no settings match")
            .into_any_element();
    }

    let hover_bg = overlay(theme, 0.04);
    let mut rows: Vec<AnyElement> = Vec::new();
    for (ix, entry) in results.into_iter().enumerate() {
        let section = entry.section;
        let row_entity = entity.clone();
        rows.push(
            settings_row()
                .id(("settings-search", ix as u64))
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
    settings_card(theme, rows).into_any_element()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_caps_orders_modifiers_then_key() {
        let b = pages::Binding::parse("cmd-shift-left").unwrap();
        assert_eq!(chord_caps(&b), vec!["⇧", "⌘", "←"]);
        let b = pages::Binding::parse("cmd-ctrl-alt-t").unwrap();
        assert_eq!(chord_caps(&b), vec!["⌃", "⌥", "⌘", "T"]);
        let b = pages::Binding::parse("cmd-w").unwrap();
        assert_eq!(chord_caps(&b), vec!["⌘", "W"]);
    }

    #[test]
    fn key_glyph_maps_arrows_and_minus() {
        assert_eq!(key_glyph("left"), "←");
        assert_eq!(key_glyph("right"), "→");
        assert_eq!(key_glyph("up"), "↑");
        assert_eq!(key_glyph("down"), "↓");
        assert_eq!(key_glyph("minus"), "-");
        assert_eq!(key_glyph("w"), "W");
    }

    #[test]
    fn overlay_follows_polarity() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert!(
            overlay(&dark, 0.5).l > dark.background.l,
            "dark polarity overlays white"
        );
        assert!(
            overlay(&light, 0.5).l < light.background.l,
            "light polarity overlays black"
        );
    }

    #[test]
    fn keyboard_groups_are_all_covered() {
        for action in Action::ALL {
            assert!(
                KEYBOARD_GROUPS.contains(&action.keyboard_group()),
                "{} has unknown group {}",
                action.name(),
                action.keyboard_group()
            );
        }
    }

    #[test]
    fn every_keyboard_group_is_non_empty() {
        for group in KEYBOARD_GROUPS {
            assert!(
                Action::ALL.iter().any(|a| a.keyboard_group() == group),
                "group {group} has no actions"
            );
        }
    }
}
