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
//!
//! Appearance's Terminal colors block is the editable end of
//! `term_theme.rs`: a caption row (caption + Light | Dark preview toggle), one
//! card whose first row is the previewed polarity's theme Select with its
//! hint, `CUSTOM` badge and — only while colours differ from the base —
//! Reset / Save as theme, then three hairline-topped swatch groups (Core,
//! Normal, Bright) of 30px swatches, each with a 10.5px label and the hex
//! field that edits it, and one terminal preview card painted from the
//! resolved palette. Chrome colors use the rcn tokens above; the swatch and
//! preview colours are the literal palette values, which is the whole point of
//! a preview.

use std::time::{Duration, Instant};

use gpui::{
    div, linear_color_stop, linear_gradient, px, AnyElement, App as GpuiApp, ClickEvent,
    ClipboardItem, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};

use crate::pages::{self, Action, Section};
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
                toggle_row(
                    theme,
                    "Open in webview tab",
                    "⇧⌘G opens the pull request in a webview tab instead of the browser.",
                    "git-pr-webview",
                    "git.open_pr_in_webview",
                    settings::open_pr_in_webview(),
                    false,
                    false,
                    &entity,
                ),
            ],
        ))
        .into_any_element()
}

// ── Appearance ──────────────────────────────────────────────────────────

/// Re-seed the Terminal-colors hex fields from the palette that is actually
/// painted for `app.preview_dark`. Called from every action that moves the
/// effective colours (polarity toggle, Reset, Save as theme, base-theme
/// select); typing never goes through it, so a field mid-edit is untouched.
fn sync_term_hex_fields(app: &App, cx: &mut GpuiApp) {
    let r = crate::term_theme::resolved(app.preview_dark);
    for (ix, input) in app.term_hex_inputs.iter().enumerate() {
        let shown = r.hex(crate::term_theme::KEYS[ix]).unwrap_or_default();
        input.update(cx, |i, cx| {
            if i.text() != shown.as_str() {
                i.set_text(shown, cx);
            }
        });
    }
}

fn render_appearance(app: &App, theme: &Theme, entity: gpui::WeakEntity<App>) -> AnyElement {
    use crate::renderer::color;
    use crate::theme::{Accent, Mode};
    use gpui::Focusable as _;

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


    // ── Terminal colors (editable 20-colour palette) ───────────────────
    // One card owns the previewed polarity's theme row and the three swatch
    // groups (Core / Normal / Bright); a second card below it previews the
    // resolved palette with the mock's sample shell output.
    let dark = preview_dark;
    let r = crate::term_theme::resolved(dark);
    let accent = theme.primary;
    let modified = r.is_modified();
    let overridden = |k: &str| r.overrides.contains_key(k);
    let (tc_names, tc_labels): (Vec<String>, Vec<String>) =
        crate::term_theme::slot_options(dark).into_iter().unzip();
    let value_ix = tc_names.iter().position(|n| *n == r.base_key);

    let tc_card = {
        // Theme row: title (+ CUSTOM badge), hint, Reset / Save as theme (only
        // when modified) and the 180px theme select.
        let mut title = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .text_size(px(13.))
            .text_color(theme.foreground)
            .child(if dark { "Dark theme" } else { "Light theme" });
        if modified {
            title = title.child(
                div()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(alpha(accent, 0.5))
                    .text_size(px(9.5))
                    .font_weight(FontWeight::BOLD)
                    .text_color(accent)
                    .child("CUSTOM"),
            );
        }
        let mut header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .px(px(12.))
            .py(px(11.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(title)
                    .child(
                        div()
                            .mt(px(2.))
                            .text_size(px(11.5))
                            .text_color(theme.muted_foreground)
                            .child(if modified {
                                format!(
                                    "{} of {} colors changed from {}.",
                                    r.modified(),
                                    crate::term_theme::KEYS.len(),
                                    r.base_label
                                )
                            } else {
                                "Click a swatch to pick a color, or type a hex value.".to_string()
                            }),
                    ),
            );
        if modified {
            let e_reset = entity.clone();
            let reset = div()
                .id("appearance-term-reset")
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.secondary_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.foreground))
                .child("Reset")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, cx: &mut GpuiApp| {
                    cx.stop_propagation();
                    crate::term_theme::clear_overrides(dark);
                    if let Some(e) = e_reset.upgrade() {
                        e.update(cx, |this, cx| {
                            sync_term_hex_fields(this, cx);
                            cx.notify();
                        });
                    }
                });
            let e_save = entity.clone();
            let save = div()
                .id("appearance-term-save")
                .px(px(10.))
                .py(px(4.))
                .rounded_full()
                .border_1()
                .border_color(overlay(theme, 0.12))
                .bg(overlay(theme, 0.05))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(accent)
                .cursor_pointer()
                .child("Save as theme")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, cx: &mut GpuiApp| {
                    cx.stop_propagation();
                    crate::term_theme::save_as_theme(dark);
                    if let Some(e) = e_save.upgrade() {
                        e.update(cx, |this, cx| {
                            sync_term_hex_fields(this, cx);
                            cx.notify();
                        });
                    }
                });
            header = header.child(reset).child(save);
        }
        let e_open = entity.clone();
        let e_change = entity.clone();
        let names = tc_names.clone();
        let select = div().w(px(180.)).child(
            Select::new("appearance-term-theme")
                .options(tc_labels.clone())
                .value(value_ix)
                .open(app.appearance_term_menu)
                .on_open_change(move |is_open: &bool, _win: &mut Window, cx: &mut GpuiApp| {
                    let open = *is_open;
                    if let Some(e) = e_open.upgrade() {
                        e.update(cx, move |this, cx| {
                            this.appearance_term_menu = open;
                            cx.notify();
                        });
                    }
                })
                .on_change(move |ix: &usize, _win: &mut Window, cx: &mut GpuiApp| {
                    let Some(name) = names.get(*ix) else { return };
                    crate::term_theme::select_base(dark, name);
                    if let Some(e) = e_change.upgrade() {
                        e.update(cx, |this, cx| {
                            sync_term_hex_fields(this, cx);
                            cx.notify();
                        });
                    }
                }),
        );
        header = header.child(select);

        // One swatch group: a hairline-topped section with a name/note line
        // and a colour grid of `cols` columns.
        let group = |name: &'static str, note: &'static str, cols: u16, from: usize| -> AnyElement {
            let mut grid = div().grid().grid_cols(cols).gap(px(8.));
            for i in from..from + cols as usize {
                let key = crate::term_theme::KEYS[i];
                let rgb = r.colors.get(key).unwrap_or((0, 0, 0));
                let mut swatch = div()
                    .id(("term-swatch", i as u64))
                    .relative()
                    .h(px(30.))
                    .w_full()
                    .rounded(px(7.))
                    .bg(color(rgb, 1.0))
                    .cursor_pointer()
                    .shadow(vec![gpui::BoxShadow {
                        color: color((255, 255, 255), if theme.dark { 0.12 } else { 0.18 }),
                        offset: gpui::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                        inset: true,
                    }]);
                if overridden(key) {
                    swatch = swatch.child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(4.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(gpui::white())
                            .shadow(vec![gpui::BoxShadow {
                                color: alpha(gpui::black(), 0.45),
                                offset: gpui::point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(1.5),
                                inset: false,
                            }]),
                    );
                }
                let field = app.term_hex_inputs[i].clone();
                let input_ent = field.clone();
                let e_swatch = entity.clone();
                let pickable = swatch.on_click(
                    move |_ev: &ClickEvent, window: &mut Window, cx: &mut GpuiApp| {
                        cx.stop_propagation();
                        window.focus(&input_ent.read(cx).focus_handle(cx), cx);
                        if let Some(e) = e_swatch.upgrade() {
                            e.update(cx, |_this, cx| cx.notify());
                        }
                    },
                );
                grid = grid.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .min_w(px(0.))
                        .child(pickable)
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.secondary_foreground)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(crate::term_theme::LABELS[i]),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_size(px(10.5))
                                .font_family(crate::renderer::FONT_FAMILY)
                                .child(field),
                        ),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(12.))
                .pt(px(10.))
                .pb(px(12.))
                .border_t_1()
                .border_color(overlay(theme, 0.06))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.muted_foreground)
                                .child(note),
                        ),
                )
                .child(grid)
                .into_any_element()
        };
        let core = group("Core", "Canvas, text, cursor and selection", 4, 0);
        let normal = group("Normal", "ANSI 0-7", 8, 4);
        let bright = group("Bright", "ANSI 8-15 · bold text", 8, 12);

        div()
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(12.))
            .border_1()
            .border_color(card_border(theme))
            .bg(card_fill(theme))
            .overflow_hidden()
            .child(header)
            .child(core)
            .child(normal)
            .child(bright)
            .into_any_element()
    };

    let tc_preview = {
        let c = |i: usize| color(r.colors.ansi[i], 1.0);
        let bg = color(r.colors.bg, 1.0);
        let fg = color(r.colors.fg, 1.0);
        let muted = c(8);
        let bar = color(crate::theme::mix(r.colors.bg, r.colors.fg, 0.12), 1.0);
        let mut chips = div().flex().flex_row().gap(px(2.));
        for i in 0..16 {
            chips = chips.child(div().size(px(7.)).bg(c(i)));
        }
        let line = |spans: Vec<AnyElement>| {
            let mut row = div()
                .flex()
                .flex_row()
                .text_size(px(11.))
                .font_family(crate::renderer::FONT_FAMILY)
                .text_color(fg)
                .line_height(gpui::relative(1.75));
            for s in spans {
                row = row.child(s);
            }
            row
        };
        let span = |text: &'static str, col: gpui::Hsla| div().text_color(col).child(text).into_any_element();
        let prompt = |cmd: &'static str| {
            line(vec![
                span("you@dev", c(2)),
                span(":", muted),
                span("~/checkout", c(4)),
                span("$", muted),
                span(cmd, fg),
            ])
        };
        let dirs = div()
            .flex()
            .flex_row()
            .gap(px(14.))
            .text_size(px(11.))
            .font_family(crate::renderer::FONT_FAMILY)
            .line_height(gpui::relative(1.75))
            .child(span("src", c(12)))
            .child(span("tests", c(12)))
            .child(span("target", c(12)))
            .child(span("Cargo.toml", fg))
            .child(span("run.sh", c(10)))
            .child(span("docs -> ../docs", c(14)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(12.))
            .border_1()
            .border_color(card_border(theme))
            .bg(bg)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(12.))
                    .py(px(7.))
                    .bg(bar)
                    .child(
                        div().text_size(px(11.)).text_color(muted).child(format!(
                            "{}{} · Preview",
                            r.base_label,
                            if modified { " · Custom" } else { "" }
                        )),
                    )
                    .child(chips),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(12.))
                    .pt(px(10.))
                    .pb(px(12.))
                    .child(prompt(" ls"))
                    .child(dirs)
                    .child(prompt(" git status"))
                    .child(line(vec![
                        span("On branch ", fg),
                        span("feature/flaky-capture", c(5)),
                    ]))
                    .child(line(vec![
                        span("  ", fg),
                        span("modified:", c(1)),
                        span("   tests/conftest.py", fg),
                    ]))
                    .child(line(vec![
                        span("  ", fg),
                        span("deleted:", c(1)),
                        span("    ", fg),
                        div()
                            .bg(color(
                                crate::term_theme::blend(r.colors.bg, r.colors.sel, 0.30),
                                1.0,
                            ))
                            .text_color(fg)
                            .child("tests/fixtures/old_capture.json")
                            .into_any_element(),
                    ]))
                    .child(prompt(" cargo test"))
                    .child(line(vec![
                        span("   ", fg),
                        span("Compiling", c(10)),
                        span(" pwrde v0.9.4", fg),
                    ]))
                    .child(line(vec![
                        span("test capture::flaky ... ", fg),
                        span("ok", c(2)),
                        span("  test capture::retry ... ", fg),
                        span("ignored", c(3)),
                        span("  test io::pipe ... ", fg),
                        span("FAILED", c(9)),
                    ]))
                    .child(line(vec![
                        span("warning", c(3)),
                        span(":", c(15)),
                        span(" unused variable ", fg),
                        span("`buf`", c(6)),
                        span(" ", fg),
                        span("--> src/io.rs:42:9", c(8)),
                    ]))
                    .child(line(vec![
                        span("[DEBUG]", c(13)),
                        span(" ", fg),
                        span("shpool attach", c(7)),
                        span(" ", fg),
                        span("gantry-2", c(11)),
                        span(" ", fg),
                        div()
                            .bg(color(r.colors.ansi[15], 1.0))
                            .text_color(color(r.colors.ansi[0], 1.0))
                            .child(" 0.9.4 ")
                            .into_any_element(),
                    ]))
                    .child(line(vec![
                        span("you@dev", c(2)),
                        span(":", muted),
                        span("~/checkout", c(4)),
                        span("$", muted),
                        span(" ", fg),
                        div()
                            .w(px(7.))
                            .h(px(13.))
                            .bg(color(r.colors.cursor, 1.0))
                            .into_any_element(),
                    ])),
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
                        sync_term_hex_fields(this, cx);
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
                .child(tc_card)
                .child(div().mt(px(4.)).child(tc_preview)),
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
                .child(field("Directory", &app.tool_form.cwd, 1.0)),
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
                        .child("Directory accepts ~."),
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
