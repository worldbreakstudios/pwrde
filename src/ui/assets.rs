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
    // Lucide panel-top: the "Show / hide title bar" palette icon (the webview
    // toolbar sits along the tile's top edge the way this frame's bar does).
    (
        "icons/panel-top.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/></svg>"##,
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
    // ── Command palette / new-session flow icons (Lucide, ISC) ────────
    // The palette's fork/worktree/branch stage tiles and summary: the
    // fork glyph, the repo-root house, the worktree branch, the local
    // terminal, the remote push arrow and the plain top-level dash.
    // `currentColor` so `svg()` tints them.
    (
        "icons/git-fork.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="18" r="3"/><circle cx="6" cy="6" r="3"/><circle cx="18" cy="6" r="3"/><path d="M18 9v2c0 .6-.4 1-1 1H7c-.6 0-1-.4-1-1V9"/><path d="M12 12v3"/></svg>"##,
    ),
    (
        "icons/house.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>"##,
    ),
    (
        "icons/git-branch.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/></svg>"##,
    ),
    (
        "icons/terminal.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/></svg>"##,
    ),
    (
        "icons/arrow-up.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 7-7 7 7"/><path d="M12 19V5"/></svg>"##,
    ),
    (
        "icons/minus.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/></svg>"##,
    ),
    // Command palette action icons (lucide, currentColor so `svg()` tints them).
    (
        "icons/x.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>"##,
    ),
    (
        "icons/save.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7"/><path d="M7 3v4a1 1 0 0 0 1 1h7"/></svg>"##,
    ),
    (
        "icons/columns-2.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M12 3v18"/></svg>"##,
    ),
    (
        "icons/rows-2.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 12h18"/></svg>"##,
    ),
    (
        "icons/square-plus.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M8 12h8"/><path d="M12 8v8"/></svg>"##,
    ),
    (
        "icons/globe.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/></svg>"##,
    ),
    (
        "icons/square-terminal.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m7 11 2-2-2-2"/><path d="M11 13h4"/><rect width="18" height="18" x="3" y="3" rx="2"/></svg>"##,
    ),
    (
        "icons/square-x.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/></svg>"##,
    ),
    (
        "icons/chevron-up.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m18 15-6-6-6 6"/></svg>"##,
    ),
    (
        "icons/chevron-left.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>"##,
    ),
    (
        "icons/circle-dot.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="1"/></svg>"##,
    ),
    (
        "icons/arrow-left.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m12 19-7-7 7-7"/><path d="M19 12H5"/></svg>"##,
    ),
    (
        "icons/arrow-down.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14"/><path d="m19 12-7 7-7-7"/></svg>"##,
    ),
    (
        "icons/arrow-right.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>"##,
    ),
    (
        "icons/chevrons-up.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m17 11-5-5-5 5"/><path d="m17 18-5-5-5 5"/></svg>"##,
    ),
    (
        "icons/chevrons-down.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m7 6 5 5 5-5"/><path d="m7 13 5 5 5-5"/></svg>"##,
    ),
    (
        "icons/folder-open.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/></svg>"##,
    ),
    (
        "icons/arrow-down-to-line.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 17V3"/><path d="m6 11 6 6 6-6"/><path d="M19 21H5"/></svg>"##,
    ),
    (
        "icons/picture-in-picture-2.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 9V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h4"/><rect width="10" height="7" x="12" y="13" rx="2"/></svg>"##,
    ),
    (
        "icons/command.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3"/></svg>"##,
    ),
    (
        "icons/power.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/></svg>"##,
    ),
    (
        "icons/copy.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>"##,
    ),
    (
        "icons/clipboard.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="8" height="4" x="8" y="2" rx="1" ry="1"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/></svg>"##,
    ),
    (
        "icons/arrow-up-right.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 7h10v10"/><path d="M7 17 17 7"/></svg>"##,
    ),
    (
        "icons/asterisk.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 6v12"/><path d="M17.196 9 6.804 15"/><path d="m6.804 9 10.392 6"/></svg>"##,
    ),
    (
        "icons/zoom-in.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/><path d="M11 8v6"/><path d="M8 11h8"/></svg>"##,
    ),
    (
        "icons/zoom-out.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/><path d="M8 11h8"/></svg>"##,
    ),
    (
        "icons/camera.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z"/><circle cx="12" cy="13" r="3"/></svg>"##,
    ),
    (
        "icons/download.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/></svg>"##,
    ),
    (
        "icons/history.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></svg>"##,
    ),
    (
        "icons/arrow-left-to-line.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 19V5"/><path d="m13 6-6 6 6 6"/><path d="M17 12H7"/></svg>"##,
    ),
    (
        "icons/arrow-right-to-line.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 19V5"/><path d="m11 6 6 6-6 6"/><path d="M7 12h10"/></svg>"##,
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
pub const ICON_PANEL_TOP: &str = "icons/panel-top.svg";
pub const ICON_MAXIMIZE: &str = "icons/maximize.svg";
pub const ICON_MINIMIZE: &str = "icons/minimize.svg";
pub const ICON_SETTINGS: &str = "icons/settings.svg";
pub const ICON_PLUS: &str = "icons/plus.svg";
pub const ICON_WRENCH: &str = "icons/wrench.svg";
pub const ICON_CHEVRON_DOWN: &str = "icons/chevron-down.svg";
pub const ICON_CHEVRON_RIGHT: &str = "icons/chevron-right.svg";

/// Command palette / new-session flow icons (see the table above).
pub const ICON_GIT_FORK: &str = "icons/git-fork.svg";
pub const ICON_HOUSE: &str = "icons/house.svg";
pub const ICON_GIT_BRANCH: &str = "icons/git-branch.svg";
pub const ICON_TERMINAL: &str = "icons/terminal.svg";
pub const ICON_ARROW_UP: &str = "icons/arrow-up.svg";
pub const ICON_MINUS: &str = "icons/minus.svg";

/// Command palette action icons (see the table above).
pub const ICON_X: &str = "icons/x.svg";
pub const ICON_SAVE: &str = "icons/save.svg";
pub const ICON_COLUMNS_2: &str = "icons/columns-2.svg";
pub const ICON_ROWS_2: &str = "icons/rows-2.svg";
pub const ICON_SQUARE_PLUS: &str = "icons/square-plus.svg";
pub const ICON_GLOBE: &str = "icons/globe.svg";
pub const ICON_SQUARE_TERMINAL: &str = "icons/square-terminal.svg";
pub const ICON_SQUARE_X: &str = "icons/square-x.svg";
pub const ICON_CHEVRON_UP: &str = "icons/chevron-up.svg";
pub const ICON_CHEVRON_LEFT: &str = "icons/chevron-left.svg";
pub const ICON_CIRCLE_DOT: &str = "icons/circle-dot.svg";
pub const ICON_ARROW_LEFT: &str = "icons/arrow-left.svg";
pub const ICON_ARROW_DOWN: &str = "icons/arrow-down.svg";
pub const ICON_ARROW_RIGHT: &str = "icons/arrow-right.svg";
pub const ICON_CHEVRONS_UP: &str = "icons/chevrons-up.svg";
pub const ICON_CHEVRONS_DOWN: &str = "icons/chevrons-down.svg";
pub const ICON_FOLDER_OPEN: &str = "icons/folder-open.svg";
pub const ICON_ARROW_DOWN_TO_LINE: &str = "icons/arrow-down-to-line.svg";
pub const ICON_PICTURE_IN_PICTURE_2: &str = "icons/picture-in-picture-2.svg";
pub const ICON_COMMAND: &str = "icons/command.svg";
pub const ICON_POWER: &str = "icons/power.svg";
pub const ICON_COPY: &str = "icons/copy.svg";
pub const ICON_CLIPBOARD: &str = "icons/clipboard.svg";
pub const ICON_ARROW_UP_RIGHT: &str = "icons/arrow-up-right.svg";
pub const ICON_ASTERISK: &str = "icons/asterisk.svg";
pub const ICON_ZOOM_IN: &str = "icons/zoom-in.svg";
pub const ICON_ZOOM_OUT: &str = "icons/zoom-out.svg";
pub const ICON_CAMERA: &str = "icons/camera.svg";
pub const ICON_DOWNLOAD: &str = "icons/download.svg";
pub const ICON_HISTORY: &str = "icons/history.svg";
pub const ICON_ARROW_LEFT_TO_LINE: &str = "icons/arrow-left-to-line.svg";
pub const ICON_ARROW_RIGHT_TO_LINE: &str = "icons/arrow-right-to-line.svg";

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
