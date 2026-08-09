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
];

pub const ICON_CIRCLE_ALERT: &str = "icons/circle-alert.svg";
pub const ICON_LOADER: &str = "icons/loader.svg";
pub const ICON_ELLIPSIS: &str = "icons/ellipsis.svg";
pub const ICON_CIRCLE_CHECK: &str = "icons/circle-check.svg";

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
