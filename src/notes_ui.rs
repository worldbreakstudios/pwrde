//! Notes page as a gpui element tree over the canvas (experimental).
//!
//! Mirrors the `cleanup_ui` / `settings_ui` overlay pattern: the rest of the
//! app stays canvas-painted while this module builds a real component tree
//! positioned over the content area. The left column browses the active
//! vault's markdown docs (the vault tabs themselves are the canvas sidebar,
//! painted in `renderer.rs`); the right column renders the selected doc with
//! gpui-component's `TextView::markdown`, or — in edit mode — a gpui-component
//! `Textarea` that writes back to disk on save.
//!
//! Reachable only while the `features.notes` flag is on; the `.when(...)` mount
//! in `main.rs` and `set_page` both gate on it, so this never renders otherwise.

use gpui::{
    div, px, AnyElement, App as GpuiApp, AppContext as _, ClickEvent, Context, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
    prelude::FluentBuilder as _,
};

use gpui_component::input::{Editor, EditorState};
use gpui_component::text::TextView;
use gpui_component::ActiveTheme as _;

use crate::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardContent, CardHeader, CardTitle,
};
use crate::App;

/// Expand a leading `~` to the home directory; otherwise a plain path.
fn expand_path(raw: &str) -> std::path::PathBuf {
    if let Some(rest) = raw.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            return if rest.is_empty() { home } else { home.join(rest) };
        }
    }
    std::path::PathBuf::from(raw)
}

impl App {
    /// Build the Notes page overlay (logical px, absolutely positioned over the
    /// content area). Call only when `page == Notes`, the flag is on, and no
    /// confirm is up.
    pub fn render_notes(&self, cx: &mut Context<Self>) -> AnyElement {
        // Keep the rcn Theme global in sync with live chrome tokens.
        cx.set_global(crate::ui::theme::Theme::from_chrome(crate::theme::current()));

        // Same live-resizing edge insets as the other overlays.
        let pad = crate::workspace::AREA_PAD;
        let sidebar = self.sidebar_w();
        let left = if sidebar == 0.0 { pad } else { sidebar };
        let right = self.right_w() + pad;

        let theme = crate::ui::theme::Theme::of(cx).clone();
        let entity = cx.entity().downgrade();
        let vaults = crate::notes::vaults();

        let title = CardTitle::new().child("Notes");
        let header = CardHeader::new().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap_2()
                .child(title)
                .when_some(self.notes_status.clone(), |el, status| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                }),
        );

        // The vault + file list now lives in the single left (canvas) sidebar;
        // the overlay is just the reader/editor, with the "add vault" path
        // field appearing on top only while the sidebar's add row is toggled.
        let body = if vaults.is_empty() {
            self.notes_empty_state(&theme, entity.clone())
        } else {
            div()
                .flex()
                .flex_col()
                .w_full()
                .h_full()
                .gap_2()
                .when(self.notes_adding_vault, |el| {
                    el.child(self.notes_add_row(entity.clone()))
                })
                .child(self.notes_reader_column(&theme, entity.clone(), cx))
                .into_any_element()
        };

        div()
            .absolute()
            .left(px(left))
            .top(px(pad))
            .right(px(right))
            .bottom(px(pad))
            .child(
                Card::new()
                    .h_full()
                    .child(header)
                    .child(CardContent::new().flex_1().child(body)),
            )
            .into_any_element()
    }

    /// Path input + "Add vault" button, shared by the empty state and the list
    /// column. Reads the rcn Input, registers the directory if it exists.
    fn notes_add_row(&self, entity: gpui::WeakEntity<App>) -> AnyElement {
        let btn_input = self.notes_add_input.clone();
        let add_entity = entity.clone();
        let button = Button::new("notes-add-vault")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .child("Add vault")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                let raw = btn_input.read(app).text().trim().to_string();
                if raw.is_empty() {
                    return;
                }
                if let Some(entity) = add_entity.upgrade() {
                    entity.update(app, move |this, cx| {
                        let path = expand_path(&raw);
                        if path.is_dir() {
                            // add_vault canonicalizes and returns the stored
                            // path, so select the vault by what was persisted.
                            let canon = crate::notes::add_vault(path);
                            let vaults = crate::notes::vaults();
                            this.notes_active_vault =
                                vaults.iter().position(|p| *p == canon).unwrap_or(0);
                            this.notes_reset_selection();
                            this.notes_adding_vault = false;
                            this.notes_rescan();
                            this.notes_add_input.update(cx, |i, cx| i.set_text("", cx));
                        } else {
                            this.notes_status =
                                Some(format!("Not a directory: {}", path.display()));
                        }
                        cx.notify();
                    });
                }
            });

        let cancel_entity = entity;
        let cancel = Button::new("notes-add-cancel")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Sm)
            .child("Cancel")
            .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                if let Some(entity) = cancel_entity.upgrade() {
                    entity.update(app, |this, cx| {
                        this.notes_adding_vault = false;
                        this.notes_add_input.update(cx, |i, cx| i.set_text("", cx));
                        cx.notify();
                    });
                }
            });

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .child(div().flex_1().child(self.notes_add_input.clone()))
            .child(button)
            .child(cancel)
            .into_any_element()
    }

    fn notes_empty_state(
        &self,
        theme: &crate::ui::theme::Theme,
        entity: gpui::WeakEntity<App>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .w_full()
            .h_full()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("No notes vaults yet — register a folder of markdown docs."),
            )
            .child(div().w(px(420.)).child(self.notes_add_row(entity)))
            .into_any_element()
    }

    fn notes_reader_column(
        &self,
        theme: &crate::ui::theme::Theme,
        entity: gpui::WeakEntity<App>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(path) = self.notes_selected.clone() else {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .h_full()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Select a note to read."),
                )
                .into_any_element();
        };

        let doc_title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        // Edit / View toggle.
        let toggle_entity = entity.clone();
        let toggle_path = path.clone();
        let editing = self.notes_edit_mode;
        let toggle = Button::new("notes-mode-toggle")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .child(if editing { "View" } else { "Edit" })
            .on_click(move |_ev: &ClickEvent, window: &mut Window, app: &mut GpuiApp| {
                if let Some(entity) = toggle_entity.upgrade() {
                    let toggle_path = toggle_path.clone();
                    entity.update(app, move |this, cx| {
                        if this.notes_edit_mode {
                            // Flipping back to View persists the edits first.
                            this.notes_autosave(cx);
                            this.notes_edit_mode = false;
                            this.notes_editor = None;
                        } else {
                            let content =
                                crate::notes::read_doc(&toggle_path).unwrap_or_default();
                            this.notes_editor = Some(cx.new(|cx| {
                                EditorState::new("markdown", window, cx)
                                    .default_value(content)
                                    .line_number(true)
                            }));
                            this.notes_edit_mode = true;
                        }
                        this.notes_status = None;
                        cx.notify();
                    });
                }
            });

        let mut header_actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(toggle);

        if editing {
            let save_entity = entity.clone();
            let save_path = path.clone();
            let save = Button::new("notes-save")
                .variant(ButtonVariant::Default)
                .size(ButtonSize::Sm)
                .child("Save")
                .on_click(move |_ev: &ClickEvent, _win: &mut Window, app: &mut GpuiApp| {
                    if let Some(entity) = save_entity.upgrade() {
                        let save_path = save_path.clone();
                        entity.update(app, move |this, cx| {
                            if let Some(editor) = this.notes_editor.as_ref() {
                                let content = editor.read(cx).value().to_string();
                                this.notes_status = match crate::notes::write_doc(
                                    &save_path, &content,
                                ) {
                                    Ok(()) => Some("Saved".into()),
                                    Err(e) => Some(format!("Save failed: {e}")),
                                };
                                this.notes_rescan();
                            }
                            cx.notify();
                        });
                    }
                });
            header_actions = header_actions.child(save);
        }

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .truncate()
                    .child(doc_title),
            )
            .child(header_actions);

        let body: AnyElement = if editing {
            match self.notes_editor.as_ref() {
                // A full source-code editor (gutter + line numbers, monospace)
                // that fills the pane — the unstyled EditorState stretches to
                // whatever height its flex container gives it.
                Some(state) => div()
                    .id("notes-editor")
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(
                        Editor::new(state)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(cx.theme().mono_font_size)
                            .size_full(),
                    )
                    .into_any_element(),
                // Defensive: edit mode without a state (shouldn't happen).
                None => div().into_any_element(),
            }
        } else {
            let content = crate::notes::read_doc(&path).unwrap_or_default();
            // Per-doc element ids so scroll offset and text selection reset
            // when a different note is opened instead of leaking across docs.
            let path_key = path.to_string_lossy();
            let scroll_id = SharedString::from(format!("notes-view:{path_key}"));
            let md_id = SharedString::from(format!("note-md:{path_key}"));
            div()
                .id(scroll_id)
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .overflow_y_scroll()
                .child(TextView::markdown(md_id, content).selectable(true))
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .gap_2()
            .child(header)
            .child(body)
            .into_any_element()
    }
}
