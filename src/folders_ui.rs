//! The GANTRY folders card: the floating glass panel at the left of the
//! sidebar region that lists the pinned CLI tools, "All sessions" and one
//! row per section (the mock's "folders"). Picking a row sets the filter the
//! flat sessions list beside it applies ([`App::set_folder_filter`]) or
//! jumps to a tool page.
//!
//! Same discipline as [`crate::sidebar_ui`]: geometry is the pure
//! [`crate::workspace`] helpers (`folders_card_rect`, `folder_rows`,
//! `folder_row_rect`, …) and every element here is absolutely positioned to
//! those rects, because the canvas mouse path in `main.rs` hit-tests the
//! same rects when a group card is dropped onto a folder. Clicks, hover,
//! the inline rename and the delete chip are element-owned and `occlude()`
//! the canvas underneath.
use gpui::{
    App as GpuiApp, ClickEvent, Context, FontWeight, InteractiveElement, MouseButton,
    MouseMoveEvent, ParentElement, ScrollWheelEvent, StatefulInteractiveElement, Styled, Window,
    div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px,
};

use crate::App;
use crate::sidebar_ui::{icon_chip, press, scaled, separator};
use crate::ui::icon;
use crate::ui::theme::Theme;
use crate::workspace::{FolderRow, LayoutRect};

/// Corner radius of the card (mock: 14px).
const CARD_RADIUS: f32 = 14.0;
/// Corner radius of a row's selection / hover fill (mock: 8px).
const ROW_RADIUS: f32 = 8.0;
/// Horizontal padding inside a row (mock: `7px 12px`).
const ROW_PAD_X: f32 = 12.0;
/// Side of a row's leading icon (mock: 15px).
const ROW_ICON: f32 = 15.0;
/// Gap between a row's icon and its label (mock: 9px).
const ROW_GAP: f32 = 9.0;
/// The pinned tool rows' green "running" dot (mock: `#3fb950`, 6px).
const TOOL_DOT: f32 = 6.0;
/// Unread dot on a folder row whose members want attention (matches the
/// session rows' dot).
const UNREAD_DOT: f32 = 6.0;
/// Side of the hover-only delete chip on a folder row.
const DELETE_CHIP: f32 = 16.0;

fn tool_green() -> gpui::Hsla {
    gpui::rgb(0x3fb950).into()
}

impl App {
    /// The folders card in logical px (the element tree's unit).
    pub(crate) fn card_rect(&self) -> LayoutRect {
        crate::workspace::folders_card_rect(self.logical_height(), self.folders_w, 1.0)
    }

    /// Whether `row` is the one the card highlights: the tool page that is
    /// showing, else the active folder filter ("All sessions" when none).
    fn folder_row_selected(&self, row: FolderRow) -> bool {
        match (row, self.page) {
            (FolderRow::Tool(i), crate::Page::Tool(j)) => i == j,
            (FolderRow::Tool(_), _) | (_, crate::Page::Tool(_)) => false,
            (FolderRow::AllSessions, _) => self.folder_filter.is_none(),
            (FolderRow::Section(si), _) => {
                self.sections.get(si).is_some_and(|s| Some(s.id) == self.folder_filter)
            },
            (FolderRow::PinnedHeader, _) => false,
        }
    }

    /// The whole card: glass material over the blurred canvas impression,
    /// the header chips, the rows (clipped above the footer) and the footer
    /// count. Positioned at `workspace::folders_card_rect`.
    pub(crate) fn render_folders_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let card = self.card_rect();
        let entity = cx.entity().downgrade();
        let modal = self.modal_overlay_open();
        let hover = self.sidebar_cursor();
        let hovered = |r: &LayoutRect| hover.is_some_and(|(x, y)| r.contains(x, y));
        let rows = self.folder_rows();
        let scroll = self.folders_scroll().round();
        let footer = crate::workspace::folders_footer_rect(&card, 1.0);
        let (hide, new) = crate::workspace::folders_header_chips(&card, 1.0);
        let handler = |entity: gpui::WeakEntity<Self>, act: fn(&mut Self)| {
            move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(app, |this, cx| {
                        act(this);
                        cx.notify();
                    });
                }
            }
        };

        // The mock's card is the accent-tinted material — `sb1 -> sb2 38%`,
        // the chrome's `gradient_from` / `gradient_to` — painted opaque: the
        // glass recipe's translucent fill over the blurred canvas read as a
        // black slab against the region ground. Keep the recipe's rim and
        // shadow stack; only the fill is the theme's gradient.
        let chrome = crate::theme::current();
        let material = linear_gradient(
            180.,
            linear_color_stop(crate::renderer::color(chrome.gradient_from, 1.0), 0.),
            linear_color_stop(crate::renderer::color(chrome.gradient_to, 1.0), 0.38),
        );
        let mut root = crate::ui::glass::Glass::card(theme.dark)
            .apply(
                div()
                    .absolute()
                    .left(px(card.x))
                    .top(px(card.y))
                    .w(px(card.w))
                    .h(px(card.h))
                    .rounded(px(CARD_RADIUS))
                    .overflow_hidden(),
            )
            .bg(material)
            .id("folders-card")
            .occlude()
            // The card occludes the canvas, so its mouse moves never reach
            // the canvas listener that records `cursor`; record them here or
            // rows only look hot after a click (`note_pointer`).
            .on_mouse_move(cx.listener(|app, ev: &MouseMoveEvent, _win, cx| {
                app.note_cursor(ev.position);
                cx.notify();
            }))
            // Same story for the wheel: the card is its own scroll container.
            .on_scroll_wheel(cx.listener(|app, ev: &ScrollWheelEvent, _win, cx| {
                app.note_cursor(ev.position);
                app.scroll_folders(ev.delta);
                cx.notify();
            }));

        // Header chips (the traffic lights float over the header's left).
        root = root
            .child(
                icon_chip(theme, &rel(&hide, &card), hovered(&hide), false, crate::ui::assets::ICON_PANEL_LEFT)
                    .id("folders-hide")
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity.clone(), |this| this.toggle_folders()))
                    }),
            )
            .child(
                icon_chip(theme, &rel(&new, &card), hovered(&new), false, crate::ui::assets::ICON_FOLDER_PLUS)
                    .id("folders-new")
                    .when(!modal, |c| {
                        c.cursor_pointer()
                            .on_click(handler(entity.clone(), |this| this.new_section_action()))
                    }),
            );

        // Rows, clipped to the band between the header and the footer.
        let body_top = card.y + crate::workspace::FOLDERS_HEADER_H;
        let body = LayoutRect { x: card.x, y: body_top, w: card.w, h: (footer.y - body_top).max(0.0) };
        let mut rows_layer = div()
            .absolute()
            .left(px(0.0))
            .top(px(body.y - card.y))
            .w(px(body.w))
            .h(px(body.h))
            .overflow_hidden();
        for (i, row) in rows.iter().enumerate() {
            let rect = crate::workspace::folder_row_rect(&card, &rows, i, scroll, 1.0);
            let r = rel(&rect, &body);
            let selected = self.folder_row_selected(*row);
            let row_hovered = hovered(&rect);
            let el = match *row {
                FolderRow::PinnedHeader => self
                    .pinned_header_row(theme, &r, row_hovered)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        press(entity.clone(), |this, _ev, _cx| this.toggle_tools_collapsed()),
                    ),
                FolderRow::Tool(ti) => {
                    let Some(tool) = self.tools.get(ti) else { continue };
                    self.tool_row(theme, &r, tool, selected, row_hovered)
                        .on_mouse_down(
                            MouseButton::Left,
                            press(entity.clone(), move |this, _ev, _cx| {
                                this.set_page(crate::Page::Tool(ti))
                            }),
                        )
                },
                FolderRow::AllSessions => {
                    // Every group: pinned ones are still in the list, as
                    // bubbles above the rows.
                    let count = self.workspaces.len();
                    folder_row(theme, &r, None, "All sessions".into(), Some(count), true, false, selected, row_hovered)
                        .on_mouse_down(
                            MouseButton::Left,
                            press(entity.clone(), move |this, _ev, _cx| {
                                this.set_folder_filter(None)
                            }),
                        )
                },
                FolderRow::Section(si) => {
                    let Some(section) = self.sections.get(si) else { continue };
                    self.section_row(theme, &r, &rect, section, selected, row_hovered, hover, entity.clone())
                },
            };
            rows_layer = rows_layer.child(el);
        }
        if let Some(sep) = crate::workspace::folder_separator_rect(&card, &rows, scroll, 1.0) {
            let s = rel(&sep, &body);
            rows_layer = rows_layer.child(
                div()
                    .absolute()
                    .left(px(s.x))
                    .top(px(s.y))
                    .w(px(s.w))
                    .h(px(s.h))
                    .bg(separator(theme)),
            );
        }
        root = root.child(rows_layer);

        // Footer: every group, the same denominator as the "All sessions"
        // badge (pinned groups are in the list too, as bubbles).
        let n = self.workspaces.len();
        root.child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(footer.y - card.y))
                .w(px(footer.w))
                .h(px(footer.h))
                .flex()
                .items_center()
                .pl(px(scaled(14.0)))
                .text_size(px(scaled(10.5)))
                .text_color(theme.muted_foreground)
                .child(format!("{n} session{}", if n == 1 { "" } else { "s" })),
        )
    }

    /// The "Tools" caption row: wrench icon, label, tool count, and a
    /// fold chevron — a click folds the tool rows away
    /// ([`App::toggle_tools_collapsed`]).
    fn pinned_header_row(&self, theme: &Theme, r: &LayoutRect, hovered: bool) -> gpui::Stateful<gpui::Div> {
        let chevron = if self.tools_collapsed {
            crate::ui::assets::ICON_CHEVRON_RIGHT
        } else {
            crate::ui::assets::ICON_CHEVRON_DOWN
        };
        row_shell(r, false, hovered, theme)
            .id("folders-pinned")
            .child(icon(
                crate::ui::assets::ICON_WRENCH,
                px(scaled(ROW_ICON)),
                theme.muted_foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(scaled(12.5)))
                    .text_color(theme.muted_foreground)
                    .child("Tools"),
            )
            .child(count_badge(theme, self.tools.len(), false))
            .child(icon(chevron, px(scaled(ROW_ICON)), theme.muted_foreground))
    }

    /// One pinned CLI-tool row: green dot + the tool's command in monospace.
    fn tool_row(
        &self,
        theme: &Theme,
        r: &LayoutRect,
        tool: &crate::cli_tools::CliTool,
        selected: bool,
        hovered: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let ink = if selected { theme.primary_foreground } else { theme.foreground };
        row_shell(r, selected, hovered, theme)
            .id(("folders-tool", r.y as usize))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .w(px(scaled(TOOL_DOT)))
                    .h(px(scaled(TOOL_DOT)))
                    .rounded_full()
                    .bg(tool_green()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(crate::renderer::FONT_FAMILY)
                    .text_size(px(scaled(11.5)))
                    .text_color(ink)
                    .child(tool.command.clone()),
            )
    }

    /// One folder (section) row: folder icon or emoji, the name — or the
    /// inline rename editor while `editing_section` names this folder — the
    /// member count, and a delete chip on hover. Double-click renames; any
    /// click selects the folder ([`App::press_folder_row`]).
    #[allow(clippy::too_many_arguments)]
    fn section_row(
        &self,
        theme: &Theme,
        r: &LayoutRect,
        abs: &LayoutRect,
        section: &crate::workspace::Section,
        selected: bool,
        hovered: bool,
        hover: Option<(f32, f32)>,
        entity: gpui::WeakEntity<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let section_id = section.id;
        let mut members = 0;
        let mut unread = false;
        for w in self.workspaces.iter().filter(|w| w.section == Some(section_id)) {
            members += 1;
            unread |= w.any_unread();
        }
        let editing = self
            .editing_section
            .as_ref()
            .filter(|(id, _)| *id == section_id)
            .map(|(_, buf)| buf.clone());
        let del = crate::workspace::section_delete_rect(abs, 1.0);
        let del_hovered = hover.is_some_and(|(x, y)| del.contains(x, y));
        let emoji = (!section.emoji.is_empty()).then(|| section.emoji.clone());
        let label = editing.clone().map_or_else(|| section.name.clone(), |buf| format!("{buf}▏"));

        // On hover the delete chip takes the count's slot instead of
        // covering it.
        let show_delete = hovered && editing.is_none();
        let mut row = folder_row(theme, r, emoji, label, Some(members), !show_delete, unread, selected, hovered)
            .on_mouse_down(
                MouseButton::Left,
                press(entity.clone(), move |this, ev, _cx| {
                    this.press_folder_row(section_id, false, ev.click_count);
                    // …and arm the re-order drag (live past the threshold).
                    if let Some(si) = this.sections.iter().position(|s| s.id == section_id) {
                        this.press_folder_drag(si);
                    }
                }),
            );
        if show_delete {
            let d = rel(&del, abs);
            let ink = if selected { theme.primary_foreground } else { theme.muted_foreground };
            row = row.child(
                div()
                    .absolute()
                    .left(px(d.x))
                    .top(px(d.y))
                    .w(px(DELETE_CHIP))
                    .h(px(DELETE_CHIP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .text_color(ink)
                    .when(del_hovered, |c| c.bg(ink.opacity(0.18)))
                    .child(icon(theme.icons.x(), px(11.0), ink))
                    .on_mouse_down(
                        MouseButton::Left,
                        press(entity, move |this, ev, _cx| {
                            this.press_folder_row(section_id, true, ev.click_count)
                        }),
                    ),
            );
        }
        row
    }
}

/// `r` re-based on `parent`'s origin, for children of an absolutely
/// positioned container.
fn rel(r: &LayoutRect, parent: &LayoutRect) -> LayoutRect {
    LayoutRect { x: r.x - parent.x, y: r.y - parent.y, w: r.w, h: r.h }
}

/// The shared row box: absolute at `r`, the mock's selection fill
/// (`Theme.primary` + white text) or a muted hover wash, horizontal flex
/// with the row's icon gap.
fn row_shell(r: &LayoutRect, selected: bool, hovered: bool, theme: &Theme) -> gpui::Div {
    div()
        .absolute()
        .left(px(r.x))
        .top(px(r.y))
        .w(px(r.w))
        .h(px(r.h))
        .rounded(px(ROW_RADIUS))
        .when(selected, |d| d.bg(theme.primary))
        .when(!selected && hovered, |d| {
            d.bg(theme.muted.opacity(if theme.dark { 0.5 } else { 0.7 }))
        })
        .flex()
        .items_center()
        .gap(px(scaled(ROW_GAP)))
        .pl(px(scaled(ROW_PAD_X)))
        .pr(px(scaled(ROW_PAD_X)))
}

/// A folder-style row: folder icon (or the section's emoji), a label, an
/// unread dot when a member wants attention, and the trailing count (`None`
/// leaves its slot empty for the delete chip).
#[allow(clippy::too_many_arguments)]
fn folder_row(
    theme: &Theme,
    r: &LayoutRect,
    emoji: Option<String>,
    label: String,
    count: Option<usize>,
    count_visible: bool,
    unread: bool,
    selected: bool,
    hovered: bool,
) -> gpui::Stateful<gpui::Div> {
    let ink = if selected { theme.primary_foreground } else { theme.foreground };
    let icon_ink = if selected { theme.primary_foreground } else { theme.muted_foreground };
    row_shell(r, selected, hovered, theme)
        .id(("folders-row", r.y as usize))
        .cursor_pointer()
        .map(|d| match emoji {
            Some(e) => d.child(
                div()
                    .flex_none()
                    .w(px(scaled(ROW_ICON)))
                    .text_size(px(scaled(12.0)))
                    .text_color(ink)
                    .child(e),
            ),
            None => d.child(icon(
                crate::ui::assets::ICON_FOLDER,
                px(scaled(ROW_ICON)),
                icon_ink,
            )),
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(scaled(12.5)))
                .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .text_color(ink)
                .child(label),
        )
        .when(unread, |d| {
            d.child(
                div()
                    .flex_none()
                    .w(px(scaled(UNREAD_DOT)))
                    .h(px(scaled(UNREAD_DOT)))
                    .rounded_full()
                    .bg(if selected { theme.primary_foreground } else { theme.primary }),
            )
        })
        // A hidden count still holds its slot, so the unread dot stays put
        // when the delete chip takes the count's place on hover.
        .when_some(count, |d, n| {
            d.child(count_badge(theme, n, selected).when(!count_visible, |b| b.invisible()))
        })
}

/// The trailing count: 11px, muted — 80% white on a selected row.
fn count_badge(theme: &Theme, count: usize, selected: bool) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(scaled(11.0)))
        .text_color(if selected {
            theme.primary_foreground.opacity(0.8)
        } else {
            theme.muted_foreground
        })
        .child(count.to_string())
}
