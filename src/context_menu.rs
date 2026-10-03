//! Native macOS context menus shown over the terminal grid.
//!
//! `pop_up` builds an `NSMenu` by hand through `objc::msg_send!`, presents it
//! at a logical window-space point via `popUpMenuPositioningItem:` (which runs
//! its own synchronous tracking loop), and returns the index of the item the
//! user chose, or `None` when the menu is dismissed. The chosen item is
//! recorded through a tiny Objective-C target class registered once through
//! a `std::sync::OnceLock`; each menu item carries its index in its `tag` and calls
//! `menuItemChosen:`, which stashes the tag into an `AtomicIsize`.
//!
//! An item may carry a [`Shortcut`] — a key equivalent plus modifier mask,
//! derived from an action's live `pages::Binding` by the pure
//! [`shortcut_for`] — which AppKit draws as the right-aligned hint column.
//! It is a hint only: the menu is a context menu, never the main menu, so
//! the chord itself still resolves through the app's bindings table.
//!
//! Because the tracking loop is nested, `pop_up` must NOT run inside a gpui
//! callback: gpui's `App` is mutably borrowed there, and any foreground task
//! the loop services (a PTY wakeup, a timer) would re-enter it and panic.
//! Callers grab the [`NsView`] while they still have the `Window`, then call
//! `pop_up` from a spawned foreground future (`cx.spawn_in`), which polls with
//! the borrow released, and re-enter gpui with `update_in` once it returns.

#![cfg(target_os = "macos")]

use objc::declare::ClassDecl;
use objc::runtime::{BOOL, NO, Object, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

/// One row of the context menu. `separator_after` appends a separator below
/// the item; disabled items stay visible but grey and unclickable. `shortcut`
/// is the hint drawn at the row's right edge (`None` → no hint).
pub struct MenuItem {
    pub title: String,
    pub enabled: bool,
    pub separator_after: bool,
    pub shortcut: Option<Shortcut>,
}

/// An `NSMenuItem` key equivalent: the (lowercase) character AppKit shows
/// and the `NSEventModifierFlags` mask beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub key: String,
    pub mask: usize,
}

/// `NSEventModifierFlags` bits.
const MOD_SHIFT: usize = 1 << 17;
const MOD_CONTROL: usize = 1 << 18;
const MOD_OPTION: usize = 1 << 19;
const MOD_COMMAND: usize = 1 << 20;

/// The menu hint for a binding. ⌘ is always in the mask (every `Binding` is
/// a ⌘ chord); shift goes in the mask too, with the key left lowercase, so
/// AppKit draws `⇧⌘F` rather than inferring shift from an uppercase letter.
/// Named keys map to the characters AppKit knows as key equivalents (the
/// `NS…FunctionKey` code points for arrows, navigation and F-keys); a key
/// with no such character returns `None` and the item shows no hint.
pub fn shortcut_for(binding: &crate::pages::Binding) -> Option<Shortcut> {
    let function_key = |code: u32| char::from_u32(code).map(String::from);
    let key = match binding.key.as_str() {
        "up" => function_key(0xF700)?,
        "down" => function_key(0xF701)?,
        "left" => function_key(0xF702)?,
        "right" => function_key(0xF703)?,
        "home" => function_key(0xF729)?,
        "end" => function_key(0xF72B)?,
        "pageup" => function_key(0xF72C)?,
        "pagedown" => function_key(0xF72D)?,
        "minus" => "-".into(),
        "space" => " ".into(),
        "enter" => "\r".into(),
        "tab" => "\t".into(),
        "escape" => "\u{1b}".into(),
        "backspace" => "\u{8}".into(),
        "delete" => "\u{7f}".into(),
        k => {
            let mut chars = k.chars();
            match (chars.next(), chars.next()) {
                // A letter, digit or punctuation key is its own equivalent.
                (Some(c), None) if !c.is_control() => c.to_lowercase().collect(),
                // f1…f35 are contiguous from NSF1FunctionKey.
                (Some('f'), Some(_)) => match k[1..].parse::<u32>() {
                    Ok(n @ 1..=35) => function_key(0xF704 + n - 1)?,
                    _ => return None,
                },
                _ => return None,
            }
        },
    };
    let mut mask = MOD_COMMAND;
    if binding.shift {
        mask |= MOD_SHIFT;
    }
    if binding.ctrl {
        mask |= MOD_CONTROL;
    }
    if binding.alt {
        mask |= MOD_OPTION;
    }
    Some(Shortcut { key, mask })
}

/// The index of the item the user picked, or `None` if the menu was dismissed.
/// Reset to -1 before each run of the synchronous tracking loop.
static CHOSEN: AtomicIsize = AtomicIsize::new(-1);

/// The `PwrdeContextMenuTarget` class, registered on first use and kept as a
/// raw pointer (classes are never freed).
static TARGET_CLASS: OnceLock<usize> = OnceLock::new();

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

/// Autoreleased `NSString` from a UTF-8 slice, for titles and key equivalents.
unsafe fn ns_string(s: &str) -> *mut objc::runtime::Object {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    objc::msg_send![objc::class!(NSString), stringWithUTF8String: c.as_ptr()]
}

/// The window's content `NSView`, captured while a `Window` is at hand so
/// the menu can be popped later from outside gpui's borrow. Held as an
/// address (not a pointer) so the value is plain data for a spawned future;
/// the view outlives every menu shown over it. (If the window were torn
/// down before the spawned future first polled, the initial `msg_send!`
/// would touch a freed view; the menu is requested from a live click and
/// polled on the next foreground turn, so that window is not reachable in
/// practice.)
#[derive(Clone, Copy, Debug)]
pub struct NsView(usize);

/// The content view of `window`, or `None` off AppKit.
pub fn ns_view(window: &gpui::Window) -> Option<NsView> {
    unsafe { ns_view_from_window(window).map(|view| NsView(view as usize)) }
}

/// `at` is logical px in window coordinates, y down from the top of the
/// window; the returned index refers to the position in `items`. Blocks in
/// AppKit's tracking loop until the menu closes — see the module header for
/// where it is safe to call from.
pub fn pop_up(view: NsView, at: (f32, f32), items: &[MenuItem]) -> Option<usize> {
    unsafe {
        let class_ptr = *TARGET_CLASS.get_or_init(|| {
            let mut decl = ClassDecl::new("PwrdeContextMenuTarget", objc::class!(NSObject))
                .expect("could not declare PwrdeContextMenuTarget");
            decl.add_method(
                sel!(menuItemChosen:),
                menu_item_chosen as extern "C" fn(&Object, objc::runtime::Sel, *mut Object),
            );
            decl.register() as *const objc::runtime::Class as usize
        });
        let target: *mut Object = msg_send![class_ptr as *const objc::runtime::Class, new];
        if target.is_null() {
            return None;
        }

        let ns_view = view.0 as *mut Object;

        let menu: *mut Object = msg_send![class!(NSMenu), alloc];
        let menu: *mut Object = msg_send![menu, initWithTitle: ns_string("Context menu")];
        let _: () = msg_send![menu, setAutoenablesItems: NO];
        CHOSEN.store(-1, Ordering::SeqCst);

        for (index, item) in items.iter().enumerate() {
            let entry: *mut Object = msg_send![class!(NSMenuItem), alloc];
            let entry: *mut Object = msg_send![entry,
                initWithTitle: ns_string(&item.title)
                action: sel!(menuItemChosen:)
                keyEquivalent: ns_string(item.shortcut.as_ref().map_or("", |s| s.key.as_str()))
            ];
            if entry.is_null() {
                let _: () = msg_send![menu, release];
                let _: () = msg_send![target, release];
                return None;
            }
            if let Some(shortcut) = &item.shortcut {
                let _: () = msg_send![entry, setKeyEquivalentModifierMask: shortcut.mask];
            }
            let _: () = msg_send![entry, setTag: index as isize];
            let _: () = msg_send![entry, setEnabled: if item.enabled { YES } else { NO }];
            let _: () = msg_send![entry, setTarget: target];
            let _: () = msg_send![menu, addItem: entry];
            let _: () = msg_send![entry, release];
            if item.separator_after {
                let separator: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
                let _: () = msg_send![menu, addItem: separator];
            }
        }

        // AppKit views are bottom-left origin unless flipped; window coords
        // are top-down, so an unflipped view needs the height subtracted.
        let bounds: CGRect = msg_send![ns_view, bounds];
        let flipped: BOOL = msg_send![ns_view, isFlipped];
        let y = window_y_to_view_y(bounds.size.height as f32, at.1, flipped == YES);
        let location = CGPoint {
            x: at.0 as f64,
            y: y as f64,
        };

        // Synchronous: runs its own tracking loop; the choice is readable
        // from CHOSEN once this returns.
        let _: () = msg_send![menu,
            popUpMenuPositioningItem: std::ptr::null::<Object>()
            atLocation: location
            inView: ns_view
        ];

        let _: () = msg_send![menu, release];
        let _: () = msg_send![target, release];

        let chosen = CHOSEN.load(Ordering::SeqCst);
        if chosen < 0 {
            None
        } else {
            Some(chosen as usize)
        }
    }
}

extern "C" fn menu_item_chosen(
    _this: &objc::runtime::Object,
    _cmd: objc::runtime::Sel,
    sender: *mut objc::runtime::Object,
) {
    unsafe {
        let tag: isize = msg_send![sender, tag];
        CHOSEN.store(tag, Ordering::SeqCst);
    }
}

/// The content `NSView` behind a gpui window, via `HasWindowHandle`.
unsafe fn ns_view_from_window(window: &gpui::Window) -> Option<*mut Object> {
    use raw_window_handle::HasWindowHandle;
    // The trait must be called fully-qualified: Window's inherent
    // `window_handle()` (returning AnyWindowHandle) otherwise shadows it.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        raw_window_handle::RawWindowHandle::AppKit(appkit) => {
            let view = appkit.ns_view.as_ptr() as *mut Object;
            if view.is_null() {
                None
            } else {
                Some(view)
            }
        }
        _ => None,
    }
}

/// The one pure, testable piece: window-space (top-down) y to view-space y.
fn window_y_to_view_y(view_h: f32, at_y: f32, flipped: bool) -> f32 {
    if flipped {
        at_y
    } else {
        view_h - at_y
    }
}

#[cfg(test)]
mod tests {
    use super::{shortcut_for, window_y_to_view_y, Shortcut};
    use crate::pages::{Action, Binding};

    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;

    fn binding(s: &str) -> Binding {
        Binding::parse(s).expect("test binding parses")
    }

    #[test]
    fn default_split_right_is_command_d() {
        assert_eq!(
            shortcut_for(&Action::SplitRight.default_binding()),
            Some(Shortcut { key: "d".into(), mask: COMMAND })
        );
    }

    #[test]
    fn shift_goes_in_the_mask_with_a_lowercase_key() {
        assert_eq!(
            shortcut_for(&binding("cmd-shift-f")),
            Some(Shortcut { key: "f".into(), mask: COMMAND | SHIFT })
        );
    }

    #[test]
    fn ctrl_and_alt_set_their_mask_bits() {
        assert_eq!(
            shortcut_for(&binding("cmd-ctrl-alt-p")),
            Some(Shortcut { key: "p".into(), mask: COMMAND | CONTROL | OPTION })
        );
    }

    #[test]
    fn named_keys_map_to_appkit_characters() {
        let key = |s: &str| shortcut_for(&binding(s)).map(|s| s.key);
        assert_eq!(key("cmd-up").as_deref(), Some("\u{F700}"));
        assert_eq!(key("cmd-down").as_deref(), Some("\u{F701}"));
        assert_eq!(key("cmd-shift-left").as_deref(), Some("\u{F702}"));
        assert_eq!(key("cmd-right").as_deref(), Some("\u{F703}"));
        assert_eq!(key("cmd-minus").as_deref(), Some("-"));
        assert_eq!(key("cmd-f5").as_deref(), Some("\u{F708}"));
        assert_eq!(key("cmd-]").as_deref(), Some("]"));
    }

    #[test]
    fn unrepresentable_key_has_no_hint() {
        assert_eq!(shortcut_for(&binding("cmd-capslock")), None);
        assert_eq!(shortcut_for(&binding("cmd-f99")), None);
    }

    #[test]
    fn unflipped_view_converts_top_down_to_bottom_up() {
        assert_eq!(window_y_to_view_y(800.0, 0.0, false), 800.0);
        assert_eq!(window_y_to_view_y(800.0, 800.0, false), 0.0);
        assert_eq!(window_y_to_view_y(800.0, 200.0, false), 600.0);
    }

    #[test]
    fn flipped_view_keeps_top_down() {
        assert_eq!(window_y_to_view_y(800.0, 200.0, true), 200.0);
        assert_eq!(window_y_to_view_y(800.0, 0.0, true), 0.0);
    }
}
