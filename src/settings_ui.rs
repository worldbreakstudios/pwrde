//! Component-based Settings page (gpui-component port).
//!
//! Renders the Settings page with gpui-component's `Settings` element — a
//! searchable page sidebar plus grouped switch/input/dropdown fields — and
//! fully replaces the painted settings chrome that used to live in
//! renderer.rs. Reads and writes go straight to the settings store and theme
//! slots. main.rs overlays this view over the content area while
//! `Page::Settings` is active and pushes per-frame host state into it
//! (`sync_from_host`): the active sidebar section (the painted section tabs
//! stay clickable and force the matching page here) and the Debug page's
//! diagnostics, which only the host can compute.
//!
//! The five pages mirror `pages::Section::ALL` in order — Sessions, Keyboard,
//! Terminal, Appearance, Debug — so section tab index == page index.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    Keystroke, Modifiers, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Window,
};
use gpui_component::{
    ActiveTheme as _,
    button::Button,
    kbd::Kbd,
    label::Label,
    setting::{
        NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
        Settings,
    },
};
use serde_json::Value;

use crate::pages::{Action, Binding};
use crate::{settings, term_theme, theme};

/// A host callback: mutations made here need the painted chrome repainted
/// (and sometimes more), and only main.rs can reach the `App` entity.
type HostHook = Rc<dyn Fn(&mut App)>;

/// Callbacks the host installs at construction.
pub struct HostHooks {
    /// Repaint the painted chrome (theme edits restyle the terminal area on
    /// the very next frame).
    pub on_change: HostHook,
    /// Snapshot groups immediately — the persist toggle must behave like the
    /// painted handler did, so enabling then restarting (with no further
    /// mutations) still restores the current groups.
    pub persist_snapshot: HostHook,
}

pub struct SettingsUi {
    hooks: Rc<HostHooks>,
    /// Keyboard-page row currently capturing a new binding, if any.
    recording: Option<Action>,
    /// Focused while recording so the captured chord lands on this view's
    /// `on_key_down` instead of bubbling to the terminal element.
    focus_handle: FocusHandle,
    /// Debug-page diagnostics, pushed by the host each frame (they read
    /// renderer/workspace state this view cannot see).
    diags: Vec<(String, String)>,
    /// Page index forced by the painted section tabs. The `Settings` element
    /// keeps its selection in window-keyed element state that is private to
    /// gpui-component, so forcing a page means minting a fresh element id
    /// (`generation`) with a new `default_selected_index`. Side effect: the
    /// search box resets on a forced jump.
    forced_page: usize,
    generation: usize,
}

impl SettingsUi {
    pub fn new(hooks: HostHooks, cx: &mut Context<Self>) -> Self {
        Self {
            hooks: Rc::new(hooks),
            recording: None,
            focus_handle: cx.focus_handle(),
            diags: Vec::new(),
            forced_page: 0,
            generation: 0,
        }
    }

    /// Per-frame push from the host. Notifies only on an actual change so the
    /// host can call this from its own render without looping.
    pub fn sync_from_host(
        &mut self,
        section_ix: usize,
        diags: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        if section_ix != self.forced_page {
            self.forced_page = section_ix;
            self.generation += 1;
            changed = true;
        }
        if diags != self.diags {
            self.diags = diags;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
}

/// Dropdown options `(value, label)` for one chrome-theme polarity slot.
fn theme_options(dark: bool) -> Vec<(SharedString, SharedString)> {
    theme::ALL
        .iter()
        .filter(|t| t.dark == dark)
        .map(|t| (SharedString::from(t.name), SharedString::from(t.label)))
        .chain(
            theme::custom(dark)
                .map(|t| (SharedString::from(t.name), SharedString::from(t.label))),
        )
        .collect()
}

/// Dropdown options for one terminal-scheme polarity slot; the empty value is
/// the adaptive default (scheme follows the chrome theme — `resolve_slot`
/// yields `None` for any unknown/absent value, which `Value::Null` is).
fn term_options(dark: bool) -> Vec<(SharedString, SharedString)> {
    std::iter::once(("".into(), "Adaptive (match chrome)".into()))
        .chain(
            term_theme::ALL
                .iter()
                .filter(|t| t.dark == dark)
                .map(|t| (SharedString::from(t.name), SharedString::from(t.label))),
        )
        .collect()
}

/// A switch field over a boolean settings key.
fn bool_field(key: &'static str, default: bool, hooks: &Rc<HostHooks>) -> SettingField<bool> {
    let hooks = hooks.clone();
    SettingField::switch(
        move |_: &App| settings::get_bool(key, default),
        move |val: bool, cx: &mut App| {
            settings::set(key, Value::Bool(val));
            (hooks.on_change)(cx);
        },
    )
    .default_value(default)
}

/// A dropdown field over a string settings key. `get`/`set` translate between
/// the stored value and the dropdown's value strings.
fn dropdown_field(
    options: Vec<(SharedString, SharedString)>,
    get: impl Fn() -> SharedString + 'static,
    set: impl Fn(SharedString) + 'static,
    hooks: &Rc<HostHooks>,
) -> SettingField<SharedString> {
    let hooks = hooks.clone();
    SettingField::dropdown(options, move |_: &App| get(), move |val, cx: &mut App| {
        set(val);
        (hooks.on_change)(cx);
    })
}

fn sessions_page(hooks: &Rc<HostHooks>) -> SettingPage {
    SettingPage::new("Sessions").default_open(true).groups(vec![
        SettingGroup::new().title("Sessions").item(
            SettingItem::new(
                "Primary command",
                SettingField::input(
                    |_: &App| SharedString::from(settings::primary_command()),
                    {
                        let hooks = hooks.clone();
                        move |val: SharedString, cx: &mut App| {
                            settings::set(
                                "session.primary_command",
                                Value::String(val.to_string()),
                            );
                            (hooks.on_change)(cx);
                        }
                    },
                )
                .default_value(SharedString::from("claude")),
            )
            .description(
                "Auto-run in a group's primary pane when the group is created. \
                 Empty disables the auto-run.",
            ),
        ),
        SettingGroup::new().title("Flyover").item(
            SettingItem::new(
                "Panel height",
                SettingField::number_input(
                    NumberFieldOptions { min: 0.2, max: 0.9, step: 0.05 },
                    |_: &App| {
                        settings::get_str("flyover.height")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(crate::workspace::FLYOVER_DEFAULT_FRAC as f64)
                    },
                    {
                        let hooks = hooks.clone();
                        move |val: f64, cx: &mut App| {
                            settings::set("flyover.height", Value::String(val.to_string()));
                            (hooks.on_change)(cx);
                        }
                    },
                )
                .default_value(crate::workspace::FLYOVER_DEFAULT_FRAC as f64),
            )
            .description("Fraction of the window the flyover terminal covers."),
        ),
    ])
}

/// Rebindable ⌘ shortcuts: click a row to arm recording, then the next ⌘
/// chord becomes the binding (Esc cancels) — same flow the painted page had.
/// The key capture itself lives on the view root (`Render` below).
fn keyboard_page(ui: &gpui::Entity<SettingsUi>) -> SettingPage {
    SettingPage::new("Keyboard").group(
        SettingGroup::new().title("Shortcuts").items(
            Action::ALL
                .iter()
                .map(|&action| {
                    let ui = ui.clone();
                    SettingItem::render(move |_options, _window, cx| {
                        let recording = ui.read(cx).recording == Some(action);
                        let value: AnyElement = if recording {
                            Label::new("press keys… (esc cancels)")
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element()
                        } else {
                            let b = action.binding();
                            Kbd::new(Keystroke {
                                modifiers: Modifiers {
                                    platform: true,
                                    shift: b.shift,
                                    alt: b.alt,
                                    control: b.ctrl,
                                    function: false,
                                },
                                key: b.key.clone(),
                                key_char: None,
                            })
                            .into_any_element()
                        };
                        let ui = ui.clone();
                        gpui_component::h_flex()
                            .id(SharedString::from(action.name()))
                            .w_full()
                            .justify_between()
                            .cursor_pointer()
                            .on_click(move |_, window, cx| {
                                let handle = ui.read(cx).focus_handle.clone();
                                ui.update(cx, |ui, cx| {
                                    ui.recording = Some(action);
                                    cx.notify();
                                });
                                window.focus(&handle, cx);
                            })
                            .child(Label::new(action.label()))
                            .child(value)
                            .into_any_element()
                    })
                    .keywords([action.label()])
                })
                .collect::<Vec<_>>(),
        ),
    )
}

fn terminal_page(hooks: &Rc<HostHooks>) -> SettingPage {
    let persist = {
        let hooks = hooks.clone();
        SettingField::switch(
            move |_: &App| settings::get_bool("terminal.persist", false),
            move |val: bool, cx: &mut App| {
                settings::set("terminal.persist", Value::Bool(val));
                // Snapshot right away so enabling then restarting (with no
                // further mutations) still restores the current groups.
                (hooks.persist_snapshot)(cx);
                (hooks.on_change)(cx);
            },
        )
        .default_value(false)
    };
    SettingPage::new("Terminal").group(
        SettingGroup::new().title("Terminal").item(
            SettingItem::new("Persist sessions", persist)
                .description("Keep sessions alive across app restarts (via shpool)."),
        ),
    )
}

fn appearance_page(hooks: &Rc<HostHooks>) -> SettingPage {
    // The clipboard's token string becomes its polarity's custom theme and is
    // selected right away; anything unparseable changes nothing.
    let import_export = {
        let hooks = hooks.clone();
        SettingItem::render(move |_options, _window, _cx| {
            let hooks = hooks.clone();
            gpui_component::h_flex()
                .gap_2()
                .child(Button::new("import-theme").label("Import from Clipboard").on_click(
                    move |_, _, cx| {
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
                                (hooks.on_change)(cx);
                            }
                        }
                    },
                ))
                .child(Button::new("export-theme").label("Copy Theme String").on_click(
                    |_, _, _| {
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            let _ = clipboard.set_text(theme::export_current());
                        }
                    },
                ))
                .into_any_element()
        })
        .keywords(["import", "export", "clipboard", "theme"])
    };
    SettingPage::new("Appearance").groups(vec![
        SettingGroup::new().title("Chrome").items(vec![
            SettingItem::new(
                "Mode",
                dropdown_field(
                    theme::Mode::ALL
                        .iter()
                        .map(|m| (SharedString::from(m.name()), SharedString::from(m.label())))
                        .collect(),
                    || SharedString::from(theme::mode().name()),
                    |val| settings::set("appearance.mode", Value::String(val.to_string())),
                    hooks,
                )
                .default_value(SharedString::from("system")),
            )
            .description("Follow the OS appearance, or pin dark/light."),
            SettingItem::new(
                "Light theme",
                dropdown_field(
                    theme_options(false),
                    || SharedString::from(theme::selected(false).name),
                    |val| settings::set(theme::setting_key(false), Value::String(val.to_string())),
                    hooks,
                ),
            ),
            SettingItem::new(
                "Dark theme",
                dropdown_field(
                    theme_options(true),
                    || SharedString::from(theme::selected(true).name),
                    |val| settings::set(theme::setting_key(true), Value::String(val.to_string())),
                    hooks,
                ),
            ),
            import_export,
        ]),
        SettingGroup::new().title("Terminal colors").items(vec![
            SettingItem::new(
                "Light scheme",
                dropdown_field(
                    term_options(false),
                    || {
                        term_theme::selected(false)
                            .map(|t| SharedString::from(t.name))
                            .unwrap_or_default()
                    },
                    |val| {
                        let stored = if val.is_empty() {
                            Value::Null
                        } else {
                            Value::String(val.to_string())
                        };
                        settings::set(term_theme::setting_key(false), stored);
                    },
                    hooks,
                ),
            ),
            SettingItem::new(
                "Dark scheme",
                dropdown_field(
                    term_options(true),
                    || {
                        term_theme::selected(true)
                            .map(|t| SharedString::from(t.name))
                            .unwrap_or_default()
                    },
                    |val| {
                        let stored = if val.is_empty() {
                            Value::Null
                        } else {
                            Value::String(val.to_string())
                        };
                        settings::set(term_theme::setting_key(true), stored);
                    },
                    hooks,
                ),
            ),
        ]),
    ])
}

fn debug_page(hooks: &Rc<HostHooks>, diags: &[(String, String)]) -> SettingPage {
    SettingPage::new("Debug").groups(vec![
        SettingGroup::new().title("Debug").item(
            SettingItem::new(
                "Show frame stats",
                bool_field("debug.overlay", false, hooks),
            )
            .description("Overlay frame timings in the terminal area."),
        ),
        SettingGroup::new().title("Diagnostics").items(
            diags
                .iter()
                .map(|(key, value)| {
                    let (key, value) =
                        (SharedString::from(key.clone()), SharedString::from(value.clone()));
                    SettingItem::render({
                        let (key, value) = (key.clone(), value.clone());
                        move |_options, _window, cx| {
                            gpui_component::h_flex()
                                .w_full()
                                .justify_between()
                                .child(
                                    Label::new(key.clone())
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(Label::new(value.clone()))
                                .into_any_element()
                        }
                    })
                    .keywords([key])
                })
                .collect::<Vec<_>>(),
        ),
    ])
}

impl Render for SettingsUi {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = cx.entity();
        gpui::div()
            .size_full()
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|ui, ev: &KeyDownEvent, _window, cx| {
                // Swallow every key while recording, exactly like the painted
                // flow: Esc cancels, a ⌘ chord binds, anything else is eaten.
                if let Some(action) = ui.recording {
                    if ev.keystroke.key == "escape" {
                        ui.recording = None;
                    } else if let Some(binding) = Binding::from_keystroke(&ev.keystroke) {
                        settings::set(&action.setting_key(), binding.serialize().into());
                        ui.recording = None;
                        (ui.hooks.on_change)(cx);
                    }
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(
                // A fresh id per forced jump re-creates the element state with
                // the forced page selected (see `forced_page` field docs).
                Settings::new(SharedString::from(format!("settings-{}", self.generation)))
                    .default_selected_index(SelectIndex {
                        page_ix: self.forced_page,
                        group_ix: None,
                    })
                    .pages(vec![
                        sessions_page(&self.hooks),
                        keyboard_page(&ui),
                        terminal_page(&self.hooks),
                        appearance_page(&self.hooks),
                        debug_page(&self.hooks, &self.diags),
                    ]),
            )
    }
}
