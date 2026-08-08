//! Component-based Settings page (gpui-component POC).
//!
//! Renders the Settings page with gpui-component's `Settings` element — a
//! searchable page sidebar plus grouped switch/input/dropdown fields —
//! instead of the painted chrome in renderer.rs. Reads and writes go straight
//! to the settings store and theme slots, the same paths the painted page's
//! click handlers use, so the two implementations can coexist during the
//! port. main.rs overlays this view over the content area while
//! `Page::Settings` is active and routes mouse events there to the
//! component tree instead of the painted hit-test cascade.

use std::rc::Rc;

use gpui::{
    App, Context, IntoElement, Keystroke, Modifiers, ParentElement, Render, SharedString, Styled,
    Window,
};
use gpui_component::{
    ActiveTheme as _,
    kbd::Kbd,
    label::Label,
    setting::{
        NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
    },
};
use serde_json::Value;

use crate::pages::Action;
use crate::{settings, term_theme, theme};

/// Hook the host installs so field mutations repaint the painted chrome
/// (theme edits restyle the terminal area on the very next frame).
type OnChange = Rc<dyn Fn(&mut App)>;

pub struct SettingsUi {
    on_change: OnChange,
}

impl SettingsUi {
    pub fn new(on_change: OnChange) -> Self {
        Self { on_change }
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
/// the adaptive default (scheme follows the chrome theme).
fn term_options(dark: bool) -> Vec<(SharedString, SharedString)> {
    std::iter::once(("".into(), "Adaptive (match chrome)".into()))
        .chain(
            term_theme::ALL
                .iter()
                .filter(|t| t.dark == dark)
                .map(|t| (SharedString::from(t.name), SharedString::from(t.name))),
        )
        .collect()
}

/// A switch field over a boolean settings key.
fn bool_field(key: &'static str, default: bool, on_change: &OnChange) -> SettingField<bool> {
    let on_change = on_change.clone();
    SettingField::switch(
        move |_: &App| settings::get_bool(key, default),
        move |val: bool, cx: &mut App| {
            settings::set(key, Value::Bool(val));
            on_change(cx);
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
    on_change: &OnChange,
) -> SettingField<SharedString> {
    let on_change = on_change.clone();
    SettingField::dropdown(options, move |_: &App| get(), move |val, cx: &mut App| {
        set(val);
        on_change(cx);
    })
}

fn general_page(on_change: &OnChange) -> SettingPage {
    let oc = on_change.clone();
    SettingPage::new("General").default_open(true).groups(vec![
        SettingGroup::new().title("Sessions").items(vec![
            SettingItem::new(
                "Primary command",
                SettingField::input(
                    |_: &App| SharedString::from(settings::primary_command()),
                    {
                        let oc = oc.clone();
                        move |val: SharedString, cx: &mut App| {
                            settings::set(
                                "session.primary_command",
                                Value::String(val.to_string()),
                            );
                            oc(cx);
                        }
                    },
                )
                .default_value(SharedString::from("claude")),
            )
            .description(
                "Auto-run in a group's primary pane when the group is created. \
                 Empty disables the auto-run.",
            ),
            SettingItem::new(
                "Persist sessions",
                bool_field("terminal.persist", false, on_change),
            )
            .description("Keep sessions alive across app restarts (via shpool)."),
        ]),
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
                        let oc = on_change.clone();
                        move |val: f64, cx: &mut App| {
                            settings::set("flyover.height", Value::String(val.to_string()));
                            oc(cx);
                        }
                    },
                )
                .default_value(crate::workspace::FLYOVER_DEFAULT_FRAC as f64),
            )
            .description("Fraction of the window the flyover terminal covers."),
        ),
        SettingGroup::new().title("Debug").item(
            SettingItem::new(
                "Show frame stats",
                bool_field("debug.overlay", false, on_change),
            )
            .description("Overlay frame timings in the terminal area."),
        ),
    ])
}

fn appearance_page(on_change: &OnChange) -> SettingPage {
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
                    on_change,
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
                    on_change,
                ),
            ),
            SettingItem::new(
                "Dark theme",
                dropdown_field(
                    theme_options(true),
                    || SharedString::from(theme::selected(true).name),
                    |val| settings::set(theme::setting_key(true), Value::String(val.to_string())),
                    on_change,
                ),
            ),
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
                    on_change,
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
                    on_change,
                ),
            ),
        ]),
    ])
}

/// Read-only listing of the rebindable ⌘ shortcuts. Recording new bindings
/// stays on the painted page for now — this demonstrates the Kbd component
/// and search across a large page.
fn keyboard_page() -> SettingPage {
    SettingPage::new("Keyboard").group(
        SettingGroup::new().title("Shortcuts").items(
            Action::ALL
                .iter()
                .map(|&action| {
                    SettingItem::render(move |_options, _window, _cx| {
                        let b = action.binding();
                        let stroke = Keystroke {
                            modifiers: Modifiers {
                                platform: true,
                                shift: b.shift,
                                alt: b.alt,
                                control: b.ctrl,
                                function: false,
                            },
                            key: b.key.clone(),
                            key_char: None,
                        };
                        gpui_component::h_flex()
                            .w_full()
                            .justify_between()
                            .child(Label::new(action.label()))
                            .child(Kbd::new(stroke))
                            .into_any_element()
                    })
                    .keywords([action.label()])
                })
                .collect::<Vec<_>>(),
        ),
    )
}

impl Render for SettingsUi {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .size_full()
            .bg(cx.theme().background)
            .child(
                Settings::new("settings")
                    .pages(vec![
                        general_page(&self.on_change),
                        appearance_page(&self.on_change),
                        keyboard_page(),
                    ]),
            )
    }
}
