//! Cleanup page as a gpui element tree over the canvas.
//!
//! The rest of the app stays canvas-painted; this module builds a real
//! component tree (vendored rcn Card/Table/…) positioned over
//! [`workspace::terminal_area`]. Confirm dialogs stay on the canvas path, so
//! the overlay is skipped while one is open.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    div, px, AnyElement, App as GpuiApp, ClickEvent, Context, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
    prelude::FluentBuilder as _,
};

use crate::cleanup::{self, DirtyFile, Row, ScanState, WorktreeInfo};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, CardAction, CardContent,
    CardDescription, CardFooter, CardHeader, CardTitle, Checkbox, HoverCard, Skeleton, Table,
    TableBody, TableCell, TableHead, TableHeader, TableRow,
};
use crate::{App, ConfirmAction, ConfirmClose};

impl App {
    /// Build the Cleanup page overlay (logical px, absolutely positioned over
    /// the content area). Call only when `page == Cleanup` and no confirm is up.
    pub fn render_cleanup(&self, cx: &mut Context<Self>) -> AnyElement {
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

        let entity = cx.entity().downgrade();
        let theme = crate::ui::theme::Theme::of(cx).clone();
        let n_visible = self.cleanup.visible().len();
        let n_selected = self.cleanup.selected.len();
        let has_filter = self.cleanup.repo_filter.is_some();
        let ready = matches!(&self.cleanup.scan, Some(ScanState::Ready(_)));

        let refresh = {
            let entity = entity.clone();
            Button::new("cleanup-refresh")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child("Refresh")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.spawn_cleanup_scan();
                            cx.notify();
                        });
                    }
                })
        };

        let mut actions = CardAction::new().child(refresh);
        if has_filter {
            let entity = entity.clone();
            actions = actions.child(
                Button::new("cleanup-all-repos")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .child("All repos")
                    .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(app, |this, cx| {
                                this.cleanup.repo_filter = None;
                                cx.notify();
                            });
                        }
                    }),
            );
        }

        let mut header = CardHeader::new().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap_2()
                .child(CardTitle::new().child("Cleanup"))
                .child(actions),
        );
        // The counts only mean something once a scan has landed.
        if ready {
            header = header.child(
                CardDescription::new().child(format!(
                    "{n_visible} worktrees · {n_selected} selected"
                )),
            );
        }

        let body = match &self.cleanup.scan {
            None | Some(ScanState::Scanning) => skeleton_body(),
            Some(ScanState::Failed(err)) => failed_body(err, &theme),
            Some(ScanState::Ready(_)) if n_visible == 0 => empty_body(&theme),
            Some(ScanState::Ready(_)) => table_body(self, cx),
        };

        let n_sel = n_selected;
        let delete = {
            let entity = entity.clone();
            Button::new("cleanup-delete")
                .variant(ButtonVariant::Destructive)
                .size(ButtonSize::Sm)
                .disabled(n_sel == 0)
                .child(if n_sel == 0 {
                    "Delete selected".into()
                } else {
                    format!("Delete {n_sel} selected")
                })
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            if this.cleanup.selected.is_empty() {
                                return;
                            }
                            let targets = this.cleanup.selected_by_repo();
                            let count: usize =
                                targets.iter().map(|(_, ids)| ids.len()).sum();
                            let dirty = this.cleanup.selected_dirty_count();
                            let plural = if count == 1 { "" } else { "s" };
                            let text = if dirty > 0 {
                                format!(
                                    "Delete {count} worktree{plural}? {dirty} ha{} uncommitted changes — those are discarded.",
                                    if dirty == 1 { "s" } else { "ve" }
                                )
                            } else {
                                format!(
                                    "Delete {count} worktree{plural}? Unmerged branches are kept."
                                )
                            };
                            this.confirm = Some(ConfirmClose {
                                text,
                                action: ConfirmAction::CleanupDelete { targets },
                            });
                            this.request_redraw();
                            cx.notify();
                        });
                    }
                })
        };

        let footer = CardFooter::new().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child("click to select · a all · m merged · esc clear · r refresh"),
                )
                .child(delete),
        );

        // Card/CardContent are not Styled — wrap for flex fill/height.
        div()
            .absolute()
            .left(px(left))
            .top(px(pad))
            .right(px(right))
            .bottom(px(pad))
            .p(px(8.))
            .child(
                div()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        Card::new()
                            .child(header)
                            .child(
                                CardContent::new().child(
                                    div()
                                        .flex_1()
                                        .min_h(px(0.))
                                        .overflow_hidden()
                                        .w_full()
                                        .child(body),
                                ),
                            )
                            .child(footer),
                    ),
            )
            .into_any_element()
    }
}

fn skeleton_body() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .p(px(8.))
        .children((0..5).map(|i| {
            Skeleton::new()
                .w(px(if i % 2 == 0 { 280. } else { 200. }))
                .h(px(16.))
                .into_any_element()
        }))
        .into_any_element()
}

fn failed_body(err: &str, theme: &crate::ui::theme::Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .w_full()
        .h_full()
        .child(
            div()
                .text_color(theme.destructive)
                .child(format!("drop -d failed: {err}")),
        )
        .into_any_element()
}

fn empty_body(theme: &crate::ui::theme::Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .w_full()
        .h_full()
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child("No drop worktrees found."),
        )
        .into_any_element()
}

fn table_body(app: &App, cx: &mut Context<App>) -> AnyElement {
    let entity = cx.entity().downgrade();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let theme = crate::ui::theme::Theme::of(cx).clone();
    let rows = app.cleanup.rows();
    let n_rows = rows.len();

    // Branch/id only need enough for a name; the pr column carries the
    // title, so it takes the lion's share of the flexible space.
    let header = TableHeader::new().child(
        TableRow::new()
            .child(TableHead::new().w(px(28.)).child(""))
            .child(TableHead::new().flex(1.).child("branch"))
            .child(TableHead::new().flex(0.5).child("id"))
            .child(TableHead::new().w(px(72.)).child("dirty"))
            .child(TableHead::new().w(px(90.)).child("parity"))
            .child(TableHead::new().flex(2.5).child("pr"))
            .child(TableHead::new().w(px(56.)).child("age")),
    );

    let mut body = TableBody::new();
    for (ix, row) in rows.into_iter().enumerate() {
        let last = ix + 1 == n_rows;
        match row {
            Row::Header {
                root,
                display,
                count,
            } => {
                let root = root.to_string();
                let label = format!("{display} · {count}");
                let entity = entity.clone();
                body = body.child(
                    TableRow::new()
                        .id(("cleanup-hdr", ix))
                        .last(last)
                        .on_click(move |_ev, _win, app| {
                            if let Some(entity) = entity.upgrade() {
                                let root = root.clone();
                                entity.update(app, move |this, cx| {
                                    this.cleanup.repo_filter = Some(root);
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            TableCell::new().child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(label),
                            ),
                        ),
                );
            }
            Row::Entry(w) => {
                body = body.child(entry_row(w, ix, last, now_ms, &theme, entity.clone(), app));
            }
        }
    }

    Table::new()
        .child(header)
        .child(
            div()
                .id("cleanup-rows")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(body),
        )
        .into_any_element()
}

fn entry_row(
    w: &WorktreeInfo,
    ix: usize,
    last: bool,
    now_ms: u64,
    theme: &crate::ui::theme::Theme,
    entity: gpui::WeakEntity<App>,
    app: &App,
) -> TableRow {
    let selected = app.cleanup.selected.contains(&w.id);
    let id = w.id.clone();
    let is_current = w.is_current;

    let toggle_entity = entity.clone();
    let toggle_id = id.clone();
    let on_row_click = move |_ev: &ClickEvent, _win: &mut Window, gpui_app: &mut GpuiApp| {
        if is_current {
            return;
        }
        if let Some(entity) = toggle_entity.upgrade() {
            let id = toggle_id.clone();
            entity.update(gpui_app, move |this, cx| {
                this.cleanup.toggle(&id);
                cx.notify();
            });
        }
    };

    let cb_entity = entity.clone();
    let cb_id = id.clone();
    let checkbox = Checkbox::new(("cleanup-cb", ix))
        .checked(selected)
        .disabled(is_current)
        .on_change(move |_checked: &bool, _win: &mut Window, gpui_app: &mut GpuiApp| {
            // The row also toggles on click; stop the bubble so a checkbox
            // click doesn't toggle twice (a visual no-op).
            gpui_app.stop_propagation();
            if let Some(entity) = cb_entity.upgrade() {
                let id = cb_id.clone();
                entity.update(gpui_app, move |this, cx| {
                    this.cleanup.toggle(&id);
                    cx.notify();
                });
            }
        });

    let mut branch = cleanup::format_branch(w);
    if is_current {
        branch.push_str(" (current)");
    }
    let branch_el = div()
        .truncate()
        .when(is_current, |el| el.text_color(theme.muted_foreground))
        .child(branch);

    let id_el = div()
        .truncate()
        .text_color(theme.muted_foreground)
        .child(w.id.clone());

    let dirty_label = cleanup::format_dirty(w.dirty_count);
    let dirty_badge = if w.dirty_count == 0 {
        Badge::new()
            .variant(BadgeVariant::Outline)
            .child(dirty_label)
            .into_any_element()
    } else {
        Badge::new()
            .variant(BadgeVariant::Destructive)
            .child(dirty_label)
            .into_any_element()
    };
    let dirty_el: AnyElement = if !w.dirty_files.is_empty() {
        let files = w.dirty_files.clone();
        HoverCard::new(("cleanup-dirty", ix))
            .content(move |cx| dirty_hover_content(&files, cx))
            .child(dirty_badge)
            .into_any_element()
    } else {
        dirty_badge
    };

    let parity = cleanup::format_parity(w.ahead, w.behind);
    let parity_el = div()
        .when(parity == "—", |el| el.text_color(theme.muted_foreground))
        .child(parity);

    let pr_el = pr_cell(w.pr.as_ref(), theme);

    let age = cleanup::format_age(now_ms, w.last_activity_ms as u64);
    let age_el = div()
        .text_color(theme.muted_foreground)
        .child(age);

    let mut row = TableRow::new()
        .id(("wt", ix))
        .selected(selected)
        .last(last)
        .child(TableCell::new().w(px(28.)).child(checkbox))
        .child(TableCell::new().flex(1.).child(branch_el))
        .child(TableCell::new().flex(0.5).child(id_el))
        .child(TableCell::new().w(px(72.)).child(dirty_el))
        .child(TableCell::new().w(px(90.)).child(parity_el))
        .child(TableCell::new().flex(2.5).child(pr_el))
        .child(TableCell::new().w(px(56.)).child(age_el));

    if !is_current {
        row = row.on_click(on_row_click);
    }
    row
}

fn pr_cell(pr: Option<&cleanup::PrInfo>, theme: &crate::ui::theme::Theme) -> AnyElement {
    // PR status tints (same palette the canvas table used), flipped per
    // polarity so they read on both white and dark cards.
    let (merged, open) = if theme.dark {
        (gpui::rgb(0xc882dc), gpui::rgb(0x78be8c))
    } else {
        (gpui::rgb(0x8e44ad), gpui::rgb(0x228b54))
    };
    match pr {
        None => div()
            .text_color(theme.muted_foreground)
            .child("—")
            .into_any_element(),
        Some(p) => {
            let label = cleanup::format_pr(Some(p));
            let badge = match p.state.as_str() {
                "merged" => Badge::new().color(merged.into()),
                "open" | "draft" => Badge::new().color(open.into()),
                _ => Badge::new().variant(BadgeVariant::Destructive),
            };
            // No pre-truncation: the cell's ellipsis clips to whatever room
            // the pr column actually has.
            let title = p.title.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .overflow_hidden()
                .child(badge.child(label))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .into_any_element()
        }
    }
}

fn dirty_hover_content(files: &[DirtyFile], cx: &mut GpuiApp) -> AnyElement {
    let theme = crate::ui::theme::Theme::of(cx).clone();
    let (added, removed) = cleanup::dirty_totals(files);
    let n = files.len();
    let header = format!(
        "{n} change{} · +{added} −{removed}",
        if n == 1 { "" } else { "s" }
    );

    let mut lines: Vec<AnyElement> = Vec::new();
    lines.push(
        div()
            .mb(px(6.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(header)
            .into_any_element(),
    );

    let show = files.len().min(10);
    for f in files.iter().take(show) {
        let detail = if f.untracked {
            "new".to_string()
        } else {
            match (f.added, f.removed) {
                (Some(a), Some(r)) => format!("+{a} −{r}"),
                _ => "binary".to_string(),
            }
        };
        lines.push(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap_2()
                .child(div().child(SharedString::from(f.path.clone())))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(detail),
                )
                .into_any_element(),
        );
    }
    if files.len() > 10 {
        let more = files.len() - 10;
        lines.push(
            div()
                .mt(px(4.))
                .text_color(theme.muted_foreground)
                .child(format!("+{more} more"))
                .into_any_element(),
        );
    }

    div().flex().flex_col().gap_1().children(lines).into_any_element()
}
