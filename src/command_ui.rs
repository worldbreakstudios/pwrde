//! The unified command palette as a gpui element tree, built from rcn atoms.
//!
//! One surface for everything: opened at the root it lists every `Action`
//! grouped and fuzzy-filterable; `New session…` is the multi-step command
//! whose picks (repository › base › layout › folder) collapse into token chips in the
//! field, with a step rail as the receipt. The sidebar ＋ and ⇧⌘T open the
//! same surface with the command token already committed. The state machine
//! is [`crate::command::CommandPalette`] — pure, unit-tested — and this
//! file only renders it and forwards clicks; the keyboard (↑ ↓ ↩ ⌫ ⎋) stays
//! in `main.rs` because the root key listener still fires while the rcn
//! `Input` (`App::modal_search`) owns editing.
//!
//! Visual spec is the "Unified command palette" mock: a 560px glass panel
//! (14px radius, deep shadow, hairline rim, top highlight), a token field
//! with a `›` prefix and an `esc` chip, a step rail with numbered dots, 24px
//! glyph tiles on command rows, section captions, right-aligned key hints,
//! a footer with the key legend. Every color comes from the chrome theme
//! through `Theme::from_chrome`, so it follows light/dark.

use std::rc::Rc;

use gpui::{
    AnyElement, App as GpuiApp, BoxShadow, ClickEvent, Context, Hsla,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, Window, div, point, px, prelude::FluentBuilder as _,
};

use crate::ui::icon;
use crate::App;
use crate::command::{action_icon, RootRow, Stage, StepState, Token};
use crate::picker::{FolderKind, ForkScope, PickerRow};
use crate::pwrspace::ProfileNode;
use crate::ui::assets::{
    ICON_ARROW_UP, ICON_CHEVRON_RIGHT, ICON_GIT_BRANCH, ICON_GIT_FORK, ICON_HOUSE, ICON_MINUS,
    ICON_PIN, ICON_PLUS,
};
use crate::ui::theme::Theme;
use crate::ui::{Badge, Button, ButtonSize, ButtonVariant, Kbd};

/// Panel width.
const PANEL_W: f32 = 560.0;
/// Panel corner radius.
const PANEL_RADIUS: f32 = 14.0;
/// Where the panel sits: its top edge at this fraction of the window height,
/// so it reads as centered-high like the mock rather than pinned to the top.
const PANEL_TOP_FRAC: f32 = 0.2;
/// Never closer to the top edge than this, on short windows.
const PANEL_TOP_MIN: f32 = 40.0;
/// The list's cap before it scrolls.
const LIST_MAX_H: f32 = 350.0;
/// Deferred-draw priority: above every other deferred element (Select
/// dropdowns, the confirm dialog) so nothing paints over the palette.
const LAYER_PRIORITY: usize = 4;

/// A rounded tile for a glyph, the mock's 24px command glyph well.
fn icon_tile(size: f32, radius: f32, bg: Hsla, fg: Hsla, path: &str) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .rounded(px(radius))
        .bg(bg)
        .text_color(fg)
        .child(icon(path, px(size * 0.5), fg))
}

fn glyph_tile(theme: &Theme, size: f32, radius: f32, bg: Hsla, fg: Hsla, glyph: impl Into<SharedString>) -> gpui::Div {
    let _ = theme;
    div()
        .flex_none()
        .w(px(size))
        .h(px(size))
        .rounded(px(radius))
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(fg)
        .child(glyph.into())
}

/// A section caption: 10px, bold, uppercase, tracked, muted.
fn caption(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .pt(px(8.0))
        .pb(px(4.0))
        .px(px(10.0))
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme.muted_foreground)
        .child(text.into().to_uppercase())
}

/// A list row shell: 7px/10px padding, 8px radius, accent tint when
/// selected or hovered, click → `on_pick`.
fn row_shell(theme: &Theme, id: usize, selected: bool, on_pick: Rc<dyn Fn(usize, &mut GpuiApp)>) -> gpui::Stateful<gpui::Div> {
    let tint = theme.primary.opacity(0.10);
    div()
        .id(("cmd-row", id))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .when(selected, |d| d.bg(tint))
        .when(!selected, |d| d.hover(move |s| s.bg(tint)))
        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| on_pick(id, app))
}

/// A stable hue for a repo's initials tile, from its name.
fn initials_color(name: &str) -> Hsla {
    let hash = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    gpui::hsla((hash % 360) as f32 / 360.0, 0.55, 0.52, 1.0)
}

fn initials(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).take(2).collect::<String>().to_lowercase()
}

/// The mini pane preview of a profile layout: a 44px box split the way the
/// profile splits, first leaf tinted accent, the rest muted.
fn layout_preview(theme: &Theme, node: Option<&ProfileNode>) -> gpui::Div {
    fn cell(theme: &Theme, first: &mut bool) -> gpui::Div {
        let bg = if *first { theme.primary.opacity(0.16) } else { theme.foreground.opacity(0.10) };
        *first = false;
        div().flex_1().rounded(px(4.0)).bg(bg).border_1().border_color(theme.border.opacity(0.6))
    }
    fn build(theme: &Theme, node: &ProfileNode, first: &mut bool) -> gpui::Div {
        match node {
            ProfileNode::Leaf(_) => cell(theme, first),
            ProfileNode::Split(s) => {
                let row = matches!(s.split, crate::pwrspace::SplitDir::Row);
                let a = build(theme, &s.a, first).flex_basis(px(0.0)).flex_grow(1.0);
                let b = build(theme, &s.b, first).flex_basis(px(0.0)).flex_grow(1.0);
                div()
                    .flex_1()
                    .flex()
                    .when(!row, |d| d.flex_col())
                    .gap(px(3.0))
                    .child(a)
                    .child(b)
            }
        }
    }
    let mut first = true;
    let inner = match node {
        Some(n) => build(theme, n, &mut first),
        None => cell(theme, &mut first),
    };
    div().h(px(44.0)).w_full().flex().mb(px(8.0)).child(inner)
}

/// The commands a profile launches, for the card's subtitle.
fn layout_procs(node: Option<&ProfileNode>) -> String {
    fn walk(node: &ProfileNode, out: &mut Vec<String>) {
        match node {
            ProfileNode::Leaf(leaf) => {
                for tab in &leaf.tabs {
                    out.push(tab.command.clone().unwrap_or_else(|| "shell".into()));
                }
            }
            ProfileNode::Split(s) => {
                walk(&s.a, out);
                walk(&s.b, out);
            }
        }
    }
    let mut out = Vec::new();
    match node {
        Some(n) => walk(n, &mut out),
        None => out.push("shell".into()),
    }
    out.join(" · ")
}

impl App {
    /// Placeholder the shared search field shows for the palette's stage.
    pub(crate) fn command_placeholder(&self) -> &'static str {
        self.command.as_ref().map_or("", |c| c.placeholder())
    }

    /// The palette panel alone, laid out for the window it is rendered in.
    ///
    /// The palette lives in its own window (`crate::palette_window`), so the
    /// panel is the whole surface: no scrim, no click-to-dismiss backdrop,
    /// and nothing painted over the main window's webviews.
    pub(crate) fn render_command_panel(
        &mut self,
        win_w: f32,
        win_h: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Keyboard navigation keeps its row in view; the wheel is free
        // otherwise (the target is consumed here, not re-applied per frame).
        if let Some(ix) = self.command_scroll_to.take() {
            self.command_scroll.scroll_to_item(ix);
        }
        let scroll = self.command_scroll.clone();
        let Some(pal) = self.command.as_ref() else {
            return div().into_any_element();
        };
        cx.set_global(Theme::from_chrome(crate::theme::current()));
        let theme = Theme::of(cx).clone();
        let chrome = crate::theme::current();
        let entity = cx.entity().downgrade();
        let panel_w = PANEL_W.min(win_w - 32.0).max(280.0);

        let chip_bg = theme.foreground.opacity(0.08);
        let hairline = theme.border;

        // ── Field: › prefix, token chips, the shared Input, esc chip ──
        self.modal_search.update(cx, |i, _| i.set_text_size(Some(px(13.5))));
        let mut field = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .py(px(12.0))
            .border_b_1()
            .border_color(hairline)
            .child(
                div().flex_none().child(icon(
                    theme.icons.chevron_right(),
                    px(13.0),
                    theme.muted_foreground,
                )),
            );
        for tok in pal.tokens() {
            let (bg, fg, kind, label) = match tok {
                Token::Command(label) => (theme.foreground.opacity(0.9), theme.background, None, label.to_string()),
                Token::Arg { kind, label } => (theme.primary.opacity(0.13), theme.primary, Some(kind), label),
            };
            field = field.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .rounded(px(6.0))
                    .px(px(8.0))
                    .py(px(3.0))
                    .bg(bg)
                    .text_size(px(12.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .when_some(kind, |d, kind| {
                        d.child(div().opacity(0.55).font_weight(gpui::FontWeight::MEDIUM).child(kind))
                    })
                    .child(label),
            );
        }
        field = field
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.0))
                    .text_color(theme.foreground)
                    .child(self.modal_search.clone()),
            )
            .child(Kbd::new().child("esc"));

        // ── Step rail (only inside the New session flow) ──
        let rail = pal.in_flow().then(|| {
            let mut rail = div()
                .flex()
                .items_center()
                .px(px(14.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(hairline.opacity(0.7));
            let steps = pal.steps();
            let n = steps.len();
            for (i, (label, state)) in steps.into_iter().enumerate() {
                let (dot_bg, dot_fg, done, label_color) = match state {
                    StepState::Done => (theme.primary, theme.primary_foreground, true, theme.primary),
                    StepState::Active => (theme.foreground, theme.background, false, theme.foreground),
                    StepState::Pending => (theme.foreground.opacity(0.12), theme.muted_foreground, false, theme.muted_foreground),
                };
                rail = rail.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(17.0))
                                .h(px(17.0))
                                .rounded(px(9.0))
                                .bg(dot_bg)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(9.5))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(dot_fg)
                                .when(done, |d| d.child(icon(theme.icons.check(), px(9.5), dot_fg)))
                                .when(!done, |d| d.child((i + 1).to_string())),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(label_color)
                                .child(label),
                        )
                        .when(i + 1 < n, |d| {
                            d.child(
                                div()
                                    .mx(px(10.0))
                                    .text_color(theme.muted_foreground.opacity(0.6))
                                    .child(icon(
                                        theme.icons.chevron_right(),
                                        px(11.0),
                                        theme.muted_foreground.opacity(0.6),
                                    )),
                            )
                        }),
                );
            }
            let back_entity = entity.clone();
            rail.child(div().flex_1()).child(
                div()
                    .id("cmd-back")
                    .cursor_pointer()
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.primary)
                    .child("⌫ back")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(entity) = back_entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.command_back();
                                cx.notify();
                            });
                        }
                    }),
            )
        });

        // ── List ──
        let pick_entity = entity.clone();
        let on_pick: Rc<dyn Fn(usize, &mut GpuiApp)> = Rc::new(move |i, app| {
            if let Some(entity) = pick_entity.upgrade() {
                entity.update(app, |this, cx| {
                    if let Some(p) = this.command.as_mut() {
                        p.select(i);
                    }
                    this.command_enter();
                    cx.notify();
                });
            }
        });
        let selected = pal.selected();
        let mut list = div()
            .id("cmd-list")
            .flex()
            .flex_col()
            .p(px(6.0))
            .max_h(px(LIST_MAX_H))
            .overflow_y_scroll()
            .track_scroll(&scroll);
        match pal.stage {
            Stage::Root => {
                for (i, row) in pal.root_rows.iter().enumerate() {
                    list = list.child(match row {
                        RootRow::Header(h) => caption(&theme, *h).into_any_element(),
                        RootRow::Action(action) => {
                            let multi = crate::command::is_multi_step(*action);
                            row_shell(&theme, i, i == selected, on_pick.clone())
                                .child(icon_tile(24.0, 6.0, chip_bg, theme.muted_foreground, action_icon(*action)))
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(action.label()),
                                )
                                .when(multi, |d| {
                                    d.child(Badge::new().variant(crate::ui::BadgeVariant::Secondary).child("4 steps ›"))
                                })
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.muted_foreground)
                                        .child(action.binding().display()),
                                )
                                .into_any_element()
                        }
                    });
                }
            }
            Stage::Repo => {
                if let Some(repo) = pal.repo.as_ref() {
                    for (i, row) in repo.rows.iter().enumerate() {
                        list = list.child(match row {
                            PickerRow::Header(h) => caption(&theme, *h).into_any_element(),
                            PickerRow::Entry(entry) => {
                                let pinned = repo.is_pinned(&entry.path);
                                let path = entry.path.to_string_lossy().to_string();
                                let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or_default();
                                let path = if !home.is_empty() && path.starts_with(&home) { format!("~{}", &path[home.len()..]) } else { path };
                                row_shell(&theme, i, i == selected, on_pick.clone())
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(26.0))
                                            .h(px(26.0))
                                            .rounded(px(7.0))
                                            .bg(initials_color(&entry.label))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(11.0))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(gpui::white())
                                            .child(initials(&entry.label)),
                                    )
                                    .child(
                                        div()
                                            .font_family(crate::renderer::FONT_FAMILY)
                                            .text_size(px(13.0))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(entry.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(theme.muted_foreground)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(path),
                                    )
                                    .child(div().flex_1())
                                    .child({
                                        // The pin toggle: a gold pin icon on favorites, a
                                        // faint one otherwise; its press never selects the row.
                                        let star_entity = entity.clone();
                                        let path = entry.path.clone();
                                        div()
                                            .id(("cmd-star", i))
                                            .flex_none()
                                            .w(px(18.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .child(icon(
                                                ICON_PIN,
                                                px(12.0),
                                                if pinned { gpui::hsla(0.12, 0.85, 0.52, 1.0) } else { theme.muted_foreground.opacity(0.35) },
                                            ))
                                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                                app.stop_propagation();
                                                if let Some(entity) = star_entity.upgrade() {
                                                    entity.update(app, |this, cx| {
                                                        if let Some(p) = this.command.as_mut().and_then(|c| c.repo.as_mut()) {
                                                            p.toggle_pin(&path);
                                                        }
                                                        this.request_redraw();
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                    })
                                    .when(entry.is_git, |d| {
                                        d.child(
                                            div()
                                                .font_family(crate::renderer::FONT_FAMILY)
                                                .text_size(px(11.0))
                                                .text_color(theme.muted_foreground)
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(3.0))
                                                        .child(icon(ICON_GIT_BRANCH, px(11.0), theme.muted_foreground))
                                                        .child("git"),
                                                ),
                                        )
                                    })
                                    .into_any_element()
                            }
                        });
                    }
                }
            }
            Stage::Base => {
                if let Some(base) = pal.base.as_ref() {
                    let mut last_group: Option<&'static str> = None;
                    for (i, entry) in base.rows.iter().enumerate() {
                        let (group, icon, meta) = match entry.scope {
                            ForkScope::Default => ("Start fresh", ICON_GIT_FORK, entry.from.clone().unwrap_or_else(|| "default branch".into())),
                            ForkScope::RepoRoot => ("Start fresh", ICON_HOUSE, "no worktree".into()),
                            ForkScope::Worktree => ("Existing worktrees", ICON_GIT_BRANCH, entry.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
                            ForkScope::Local => ("Local branches", ICON_MINUS, "local".into()),
                            ForkScope::Remote => ("Remote branches", ICON_ARROW_UP, "remote".into()),
                        };
                        if last_group != Some(group) {
                            list = list.child(caption(&theme, group));
                            last_group = Some(group);
                        }
                        list = list.child(
                            row_shell(&theme, i, i == selected, on_pick.clone())
                                .child(icon_tile(26.0, 7.0, chip_bg, theme.muted_foreground, icon))
                                .child(
                                    div()
                                        .font_family(crate::renderer::FONT_FAMILY)
                                        .text_size(px(13.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(entry.label.clone()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .font_family(crate::renderer::FONT_FAMILY)
                                        .text_size(px(11.0))
                                        .text_color(theme.muted_foreground)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(meta),
                                ),
                        );
                    }
                }
            }
            Stage::Layout => {
                if let Some(layout) = pal.layout.as_ref() {
                    // Two cards per row.
                    let mut grid = div().flex().flex_col().gap(px(8.0)).p(px(6.0));
                    let mut row = div().flex().gap(px(8.0));
                    let mut in_row = 0;
                    for (i, entry) in layout.rows.iter().enumerate() {
                        let node = entry.profile.as_ref().map(|p| &p.layout);
                        let is_selected = i == selected;
                        let on_pick = on_pick.clone();
                        let card = div()
                            .id(("cmd-layout", i))
                            .flex_1()
                            .min_w(px(0.0))
                            .rounded(px(10.0))
                            .p(px(10.0))
                            .bg(theme.card)
                            .border(px(1.5))
                            .border_color(if is_selected { theme.primary } else { hairline })
                            .when(is_selected, |d| {
                                d.shadow(vec![BoxShadow {
                                    color: theme.primary.opacity(0.14),
                                    offset: point(px(0.0), px(0.0)),
                                    blur_radius: px(0.0),
                                    spread_radius: px(3.0),
                                    inset: false,
                                }])
                            })
                            .cursor_pointer()
                            .hover(move |s| s.border_color(theme.primary))
                            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| on_pick(i, app))
                            .child(layout_preview(&theme, node))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(if entry.profile.is_some() { entry.label.clone() } else { "Default".to_string() }),
                            )
                            .child(
                                div()
                                    .font_family(crate::renderer::FONT_FAMILY)
                                    .text_size(px(10.5))
                                    .text_color(theme.muted_foreground)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(layout_procs(node)),
                            );
                        row = row.child(card);
                        in_row += 1;
                        if in_row == 2 {
                            grid = grid.child(row);
                            row = div().flex().gap(px(8.0));
                            in_row = 0;
                        }
                    }
                    if in_row > 0 {
                        grid = grid.child(row.child(div().flex_1()));
                    }
                    list = list.child(grid);
                }
            }
            Stage::Folder => {
                if let Some(folder) = pal.folder.as_ref() {
                    let mut last_group: Option<&'static str> = None;
                    for (i, entry) in folder.rows.iter().enumerate() {
                        let (group, icon, tinted) = match &entry.kind {
                            FolderKind::TopLevel => ("Session folder", Some(ICON_MINUS), false),
                            FolderKind::Existing { .. } => (
                                "Existing folders",
                                if entry.emoji.is_empty() { Some(ICON_CHEVRON_RIGHT) } else { None },
                                true,
                            ),
                            FolderKind::New { .. } => ("Existing folders", Some(ICON_PLUS), false),
                        };
                        if last_group != Some(group) {
                            list = list.child(caption(&theme, group));
                            last_group = Some(group);
                        }
                        let (tile_bg, tile_fg) = if tinted {
                            (theme.primary.opacity(0.13), theme.primary)
                        } else {
                            (chip_bg, theme.muted_foreground)
                        };
                        list = list.child(
                            row_shell(&theme, i, i == selected, on_pick.clone())
                                .child(match icon {
                                    Some(icon) => icon_tile(26.0, 7.0, tile_bg, tile_fg, icon),
                                    None => glyph_tile(&theme, 26.0, 7.0, tile_bg, tile_fg, entry.emoji.clone()),
                                })
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(entry.label.clone()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .font_family(crate::renderer::FONT_FAMILY)
                                        .text_size(px(11.0))
                                        .text_color(theme.muted_foreground)
                                        .child(entry.meta.clone()),
                                ),
                        );
                    }
                }
            }
            Stage::Done => {
                let (line, sub) = pal.summary();
                let launch_entity = entity.clone();
                let reset_entity = entity.clone();
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .px(px(14.0))
                        .py(px(18.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .rounded(px(10.0))
                                .p(px(12.0))
                                .bg(theme.primary.opacity(0.07))
                                .border_1()
                                .border_color(theme.primary.opacity(0.2))
                                .child(icon_tile(38.0, 19.0, theme.primary, theme.primary_foreground, ICON_GIT_FORK))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(
                                            div()
                                                .font_family(crate::renderer::FONT_FAMILY)
                                                .text_size(px(13.0))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(theme.foreground)
                                                .child(line),
                                        )
                                        .child(div().text_size(px(11.5)).text_color(theme.muted_foreground).child(sub)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(8.0))
                                .justify_end()
                                .child(
                                    Button::new("cmd-start-over")
                                        .variant(ButtonVariant::Secondary)
                                        .size(ButtonSize::Sm)
                                        .child("Start over")
                                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                            if let Some(entity) = reset_entity.upgrade() {
                                                entity.update(app, |this, cx| {
                                                    this.command_start_over();
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("cmd-create")
                                        .variant(ButtonVariant::Default)
                                        .size(ButtonSize::Sm)
                                        .child("Create Session ↩")
                                        .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                                            if let Some(entity) = launch_entity.upgrade() {
                                                entity.update(app, |this, cx| {
                                                    this.command_enter();
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        ),
                );
            }
        }

        // ── Footer ──
        let legend = |k: &'static str, what: &'static str| {
            div()
                .flex()
                .gap(px(3.0))
                .child(div().font_weight(gpui::FontWeight::BOLD).text_color(theme.muted_foreground).child(k))
                .child(what)
        };
        let footer = div()
            .flex()
            .gap(px(14.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_t_1()
            .border_color(hairline.opacity(0.8))
            .text_size(px(10.5))
            .text_color(theme.muted_foreground.opacity(0.85))
            .child(legend("↑↓", "navigate"))
            .child(legend("↩", "select"))
            .child(legend("⌫", if pal.stage == Stage::Root { "close" } else { "pop token" }))
            .child(div().flex_1())
            .child(pal.footer());

        // ── Panel + scrim, on its own top-most deferred layer ──
        let panel = div()
            .id("command-palette")
            .occlude()
            .w(px(panel_w))
            .flex()
            .flex_col()
            .rounded(px(PANEL_RADIUS))
            .overflow_hidden()
            // Liquid glass over the blurred canvas impression (`backdrop.rs`)
            // when one is live; the legible-opaque recipe otherwise.
            .map(|d| {
                let glass = if self.glass_backdrop.is_some() {
                    crate::ui::Glass::panel_blurred(theme.dark)
                } else {
                    crate::ui::Glass::panel(theme.dark)
                };
                d.bg(glass.fill).border_1().border_color(glass.rim)
            })
            .children(self.glass_backdrop_el(gpui::Corners::all(px(PANEL_RADIUS))))
            .shadow(vec![
                BoxShadow {
                    color: crate::renderer::color(chrome.shadow, 0.35),
                    offset: point(px(0.0), px(18.0)),
                    blur_radius: px(48.0),
                    spread_radius: px(0.0),
                    inset: false,
                },
                BoxShadow {
                    color: gpui::white().opacity(if theme.dark { 0.10 } else { 0.8 }),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(0.0),
                    inset: true,
                },
            ])
            .text_size(px(13.0))
            .text_color(theme.foreground)
            .child(field)
            .when_some(rail, |d, rail| d.child(rail))
            .child(list)
            .child(footer);

        // Centered-high in its own window, exactly as it sat in the mock:
        // `PANEL_TOP_FRAC` of the window height, never closer than
        // `PANEL_TOP_MIN` to the top edge.
        let top = (win_h * PANEL_TOP_FRAC).max(PANEL_TOP_MIN);
        div()
            .absolute()
            .left(px(((win_w - panel_w) / 2.0).max(0.0)))
            .top(px(top))
            .child(panel)
            .into_any_element()
    }
}
