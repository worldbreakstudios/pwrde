//! Embedded asset source serving the chevron icons the components need, in
//! each supported icon library's own drawing style (mirroring shadcn's icon
//! library choice). All four sets are permissively licensed (Lucide ISC,
//! Tabler MIT, Phosphor MIT, Remix Apache-2.0).

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The icon set components draw from — shadcn create's "Icon Library" pick.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IconLibrary {
    #[default]
    Lucide,
    Tabler,
    Phosphor,
    Remix,
}

impl IconLibrary {
    pub const ALL: [IconLibrary; 4] = [
        IconLibrary::Lucide,
        IconLibrary::Tabler,
        IconLibrary::Phosphor,
        IconLibrary::Remix,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IconLibrary::Lucide => "lucide",
            IconLibrary::Tabler => "tabler",
            IconLibrary::Phosphor => "phosphor",
            IconLibrary::Remix => "remix",
        }
    }

    fn dir(self) -> &'static str {
        self.label()
    }

    pub fn chevron_down(self) -> String {
        format!("icons/{}/chevron-down.svg", self.dir())
    }

    pub fn chevron_up(self) -> String {
        format!("icons/{}/chevron-up.svg", self.dir())
    }

    pub fn chevron_right(self) -> String {
        format!("icons/{}/chevron-right.svg", self.dir())
    }

    pub fn chevron_left(self) -> String {
        format!("icons/{}/chevron-left.svg", self.dir())
    }

    pub fn x(self) -> String {
        format!("icons/{}/x.svg", self.dir())
    }

    pub fn check(self) -> String {
        format!("icons/{}/check.svg", self.dir())
    }
}

/// A stroked 24px icon in the Lucide/Tabler style.
macro_rules! stroked {
    ($path:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d=""#,
            $path,
            r#""/></svg>"#
        )
        .as_bytes()
    };
}

/// A filled icon (Phosphor's 256 grid, Remix's 24 grid).
macro_rules! filled {
    ($viewbox:literal, $path:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 "#,
            $viewbox,
            r#"" fill="currentColor"><path d=""#,
            $path,
            r#""/></svg>"#
        )
        .as_bytes()
    };
}

const ICONS: &[(&str, &[u8])] = &[
    ("icons/lucide/chevron-down.svg", stroked!("m6 9 6 6 6-6")),
    ("icons/lucide/chevron-up.svg", stroked!("m18 15-6-6-6 6")),
    ("icons/lucide/chevron-right.svg", stroked!("m9 18 6-6-6-6")),
    ("icons/tabler/chevron-down.svg", stroked!("M6 9l6 6l6 -6")),
    ("icons/tabler/chevron-up.svg", stroked!("M6 15l6 -6l6 6")),
    ("icons/tabler/chevron-right.svg", stroked!("M9 6l6 6l-6 6")),
    (
        "icons/phosphor/chevron-down.svg",
        filled!(
            "256 256",
            "M213.66,101.66l-80,80a8,8,0,0,1-11.32,0l-80-80A8,8,0,0,1,53.66,90.34L128,164.69l74.34-74.35a8,8,0,0,1,11.32,11.32Z"
        ),
    ),
    (
        "icons/phosphor/chevron-up.svg",
        filled!(
            "256 256",
            "M213.66,165.66a8,8,0,0,1-11.32,0L128,91.31,53.66,165.66a8,8,0,0,1-11.32-11.32l80-80a8,8,0,0,1,11.32,0l80,80A8,8,0,0,1,213.66,165.66Z"
        ),
    ),
    (
        "icons/phosphor/chevron-right.svg",
        filled!(
            "256 256",
            "M181.66,133.66l-80,80a8,8,0,0,1-11.32-11.32L164.69,128,90.34,53.66a8,8,0,0,1,11.32-11.32l80,80A8,8,0,0,1,181.66,133.66Z"
        ),
    ),
    (
        "icons/remix/chevron-down.svg",
        filled!(
            "24 24",
            "M12 13.1717L16.9497 8.22192L18.364 9.63614L12 16.0001L5.63604 9.63614L7.05025 8.22192L12 13.1717Z"
        ),
    ),
    (
        "icons/remix/chevron-up.svg",
        filled!(
            "24 24",
            "M12 10.8284L7.05025 15.7782L5.63604 14.364L12 8L18.364 14.364L16.9497 15.7782L12 10.8284Z"
        ),
    ),
    (
        "icons/remix/chevron-right.svg",
        filled!(
            "24 24",
            "M13.1717 12L8.22192 7.05025L9.63614 5.63604L16.0001 12L9.63614 18.364L8.22192 16.9497L13.1717 12Z"
        ),
    ),
    ("icons/lucide/chevron-left.svg", stroked!("m15 18-6-6 6-6")),
    ("icons/tabler/chevron-left.svg", stroked!("M15 6l-6 6l6 6")),
    (
        "icons/phosphor/chevron-left.svg",
        filled!(
            "256 256",
            "M165.66,202.34a8,8,0,0,1-11.32,11.32l-80-80a8,8,0,0,1,0-11.32l80-80a8,8,0,0,1,11.32,11.32L91.31,128Z"
        ),
    ),
    (
        "icons/remix/chevron-left.svg",
        filled!(
            "24 24",
            "M10.8284 12L15.7782 16.9497L14.364 18.364L8 12L14.364 5.63604L15.7782 7.05025L10.8284 12Z"
        ),
    ),
    ("icons/lucide/x.svg", stroked!("M18 6 6 18M6 6l12 12")),
    ("icons/tabler/x.svg", stroked!("M18 6l-12 12M6 6l12 12")),
    (
        "icons/phosphor/x.svg",
        filled!(
            "256 256",
            "M205.66,194.34a8,8,0,0,1-11.32,11.32L128,139.31,61.66,205.66a8,8,0,0,1-11.32-11.32L116.69,128,50.34,61.66A8,8,0,0,1,61.66,50.34L128,116.69l66.34-66.35a8,8,0,0,1,11.32,11.32L139.31,128Z"
        ),
    ),
    (
        "icons/remix/x.svg",
        filled!(
            "24 24",
            "M12 10.5858L16.2426 6.34315L17.6569 7.75736L13.4142 12L17.6569 16.2426L16.2426 17.6569L12 13.4142L7.75736 17.6569L6.34315 16.2426L10.5858 12L6.34315 7.75736L7.75736 6.34315L12 10.5858Z"
        ),
    ),
    ("icons/lucide/check.svg", stroked!("M20 6 9 17l-5-5")),
    ("icons/tabler/check.svg", stroked!("M5 12l5 5l10 -10")),
    (
        "icons/phosphor/check.svg",
        filled!(
            "256 256",
            "M229.66,77.66l-128,128a8,8,0,0,1-11.32,0l-56-56a8,8,0,0,1,11.32-11.32L96,188.69,218.34,66.34a8,8,0,0,1,11.32,11.32Z"
        ),
    ),
    (
        "icons/remix/check.svg",
        filled!(
            "24 24",
            "M10 15.1716L19.1924 5.97919L20.6066 7.3934L10 18L3.63604 11.636L5.05025 10.2218L10 15.1716Z"
        ),
    ),
    // Status icons (lucide drawings shared across libraries for now —
    // TODO(rcn): per-library variants).
    (
        "icons/circle-alert.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" x2="12" y1="8" y2="12"/><line x1="12" x2="12.01" y1="16" y2="16"/></svg>"##,
    ),
    (
        "icons/loader.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-6.219-8.56"/></svg>"##,
    ),
    (
        "icons/ellipsis.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></svg>"##,
    ),
    (
        "icons/circle-check.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/></svg>"##,
    ),
    // Webview toolbar icons (lucide drawings shared across libraries for
    // now, like the status icons above): reload, https lock, plain-http
    // info, and the vertical ⋮ overflow. `currentColor` so `svg()` tints.
    (
        "icons/refresh.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/></svg>"##,
    ),
    (
        "icons/lock.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>"##,
    ),
    (
        "icons/info.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>"##,
    ),
    (
        "icons/ellipsis-vertical.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="1"/><circle cx="12" cy="19" r="1"/><circle cx="12" cy="5" r="1"/></svg>"##,
    ),
    // ── Sidebar chrome icons (Lucide, ISC) ───────────────────────────
    // The folders card and the sessions-list header: folder rows, the pin
    // caption, the new-folder / hide-folders / show-folders / focus chips
    // and the Settings gear. `currentColor` so `svg()` tints them.
    (
        "icons/folder.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>"##,
    ),
    (
        "icons/folder-plus.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 10v6"/><path d="M9 13h6"/><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>"##,
    ),
    (
        "icons/pin.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/></svg>"##,
    ),
    // Traced from the GANTRY sidebar mock (stroke 1.8 like its other chrome
    // glyphs): the plain panel-left frame is every folders/sessions toggle
    // ("Show folders", "Hide folders"); the outward corner brackets are
    // "Focus terminals" and their inward twin (lucide minimize) is the
    // collapsed-region "Show sessions" chip.
    (
        "icons/panel-left.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/></svg>"##,
    ),
    (
        "icons/maximize.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M21 8V5a2 2 0 0 0-2-2h-3"/><path d="M3 16v3a2 2 0 0 0 2 2h3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/></svg>"##,
    ),
    (
        "icons/minimize.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3v3a2 2 0 0 1-2 2H3"/><path d="M21 8h-3a2 2 0 0 1-2-2V3"/><path d="M3 16h3a2 2 0 0 1 2 2v3"/><path d="M16 21v-3a2 2 0 0 1 2-2h3"/></svg>"##,
    ),
    (
        "icons/settings.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/></svg>"##,
    ),
    // Fold chevrons for the "Pinned" / "Pinned tools" captions (lucide
    // chevron-down / chevron-right).
    // lucide wrench — the folders card's "Tools" caption.
    (
        "icons/wrench.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/></svg>"##,
    ),
    (
        "icons/chevron-down.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"##,
    ),
    (
        "icons/chevron-right.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>"##,
    ),
    (
        "icons/plus.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>"##,
    ),
    // ── Sidebar PR-state icons ─────────────────────────────────────────
    // Traced from the GANTRY sidebar mock, which draws each pull-request
    // state as a little commit graph rather than a generic symbol: two nodes
    // on a trunk, plus whatever the third node is doing. `currentColor` so
    // gpui's monochrome `svg()` can tint them per state.
    (
        "icons/git/pr-none.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><circle cx="4" cy="3.5" r="1.7"/><circle cx="4" cy="12.5" r="1.7"/><circle cx="12" cy="3.5" r="1.7"/><path d="M4 5.2v5.6"/><path d="M12 5.2c0 3-3.2 3.3-6.3 3.3"/></svg>"##,
    ),
    (
        "icons/git/pr-draft.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><circle cx="4" cy="3.5" r="1.7"/><circle cx="4" cy="12.5" r="1.7"/><path d="M4 5.2v5.6"/><circle cx="12" cy="12.5" r="1.7"/><circle cx="12" cy="3.5" r="1" fill="currentColor" stroke="none"/><circle cx="12" cy="7.5" r="1" fill="currentColor" stroke="none"/></svg>"##,
    ),
    (
        "icons/git/pr-open.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><circle cx="4" cy="3.5" r="1.7"/><circle cx="4" cy="12.5" r="1.7"/><circle cx="12" cy="12.5" r="1.7"/><path d="M4 5.2v5.6"/><path d="M8.7 3.5h1.55A1.75 1.75 0 0 1 12 5.25v5.4"/><path d="M7.4 2.2l1.3 1.3-1.3 1.3"/></svg>"##,
    ),
    (
        "icons/git/pr-merged.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><circle cx="4" cy="3.5" r="1.7"/><circle cx="4" cy="12.5" r="1.7"/><circle cx="12" cy="8" r="1.7"/><path d="M4 5.2v5.6"/><path d="M4 5.5c.4 2.3 2.6 2.5 6.1 2.5"/></svg>"##,
    ),
];

/// Sidebar PR-state icons (see the table above).
pub const ICON_PR_NONE: &str = "icons/git/pr-none.svg";
pub const ICON_PR_DRAFT: &str = "icons/git/pr-draft.svg";
pub const ICON_PR_OPEN: &str = "icons/git/pr-open.svg";
pub const ICON_PR_MERGED: &str = "icons/git/pr-merged.svg";

pub const ICON_CIRCLE_ALERT: &str = "icons/circle-alert.svg";
pub const ICON_LOADER: &str = "icons/loader.svg";
pub const ICON_ELLIPSIS: &str = "icons/ellipsis.svg";
pub const ICON_CIRCLE_CHECK: &str = "icons/circle-check.svg";

/// Webview toolbar icons (see the table above).
pub const ICON_REFRESH: &str = "icons/refresh.svg";
pub const ICON_LOCK: &str = "icons/lock.svg";
pub const ICON_INFO: &str = "icons/info.svg";
pub const ICON_ELLIPSIS_VERTICAL: &str = "icons/ellipsis-vertical.svg";

/// Sidebar chrome icons (see the table above).
pub const ICON_FOLDER: &str = "icons/folder.svg";
pub const ICON_FOLDER_PLUS: &str = "icons/folder-plus.svg";
pub const ICON_PIN: &str = "icons/pin.svg";
pub const ICON_PANEL_LEFT: &str = "icons/panel-left.svg";
pub const ICON_MAXIMIZE: &str = "icons/maximize.svg";
pub const ICON_MINIMIZE: &str = "icons/minimize.svg";
pub const ICON_SETTINGS: &str = "icons/settings.svg";
pub const ICON_PLUS: &str = "icons/plus.svg";
pub const ICON_WRENCH: &str = "icons/wrench.svg";
pub const ICON_CHEVRON_DOWN: &str = "icons/chevron-down.svg";
pub const ICON_CHEVRON_RIGHT: &str = "icons/chevron-right.svg";

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
