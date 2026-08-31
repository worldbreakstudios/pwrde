//! Vendored shadcn-style gpui components from [rcn](https://github.com/a1re1/rcn).
//!
//! These are faithful copies (not a Cargo dependency), pinned to the same gpui
//! git rev as this app so they compile unchanged. Vendoring is managed by the
//! rcn CLI via `rcn.toml` at the repo root, which maps the registry's default
//! layout onto this directory (components, theme, motion, and assets all live
//! here as `crate::ui::*` instead of top-level modules). After `rcn add`,
//! rewrite the new file's `crate::theme` / `crate::motion` imports to
//! `crate::ui::theme` / `crate::ui::motion`. Tokens live in [`theme`]; bridge
//! them from pwrde chrome via [`theme::Theme::from_chrome`] (added locally).
//! Deliberate local extensions: [`table`] (`TableRow::on_click`; shrinkable
//! flex cells with optional grow weights, so columns stay aligned at narrow
//! widths and can take unequal shares; `Table::h_full`; `Table::w_auto` for a
//! content-width table inside a horizontal scroll area), [`badge`]
//! (`Badge::color` status tints), and [`card`] (`Card::h_full` /
//! `CardContent::flex_1` for page-filling cards, `Card::floating` for a
//! raised shadow when floating over other content, `Card::glass` for a
//! translucent glass panel over the blurred vibrancy ground,
//! `Card::liquid_glass` for the full [`Glass::panel`] liquid-glass recipe),
//! [`input`]
//! (`Input::set_text_size` so a bare field can match its host row's type),
//! and [`alert_dialog`] (`AlertDialog::scrim` for the chrome's own scrim
//! color, `AlertDialog::on_backdrop_click` for clicks on the scrim outside
//! the panel, `AlertDialog::top` to pin the panel below the viewport's top
//! edge, and the panel width yielding to `Styled` refinements).
//!
//! Keep the `pub use` re-exports ABOVE the `pub mod` lines: `rcn add`
//! regenerates everything after the first `pub mod` and would drop them.

pub use badge::{Badge, BadgeVariant};
pub use alert_dialog::{AlertDialog, AlertDialogFooter};
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use alert_dialog::{AlertDialogDescription, AlertDialogHeader, AlertDialogTitle};
pub use button::{Button, ButtonSize, ButtonVariant};
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use card::{
    Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardSize, CardTitle,
};
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use checkbox::Checkbox;
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use dialog::{Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle};
pub use glass::Glass;
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use hover_card::HoverCard;
pub use input::Input;
pub use kbd::Kbd;
#[allow(unused_imports)] // vendored rcn surface: kept exported while no caller uses it
pub use label::Label;
pub use skeleton::Skeleton;
pub use switch::Switch;
pub use table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};

pub mod alert_dialog;
pub mod assets;
pub mod badge;
pub mod button;
pub mod button_group;
pub mod card;
pub mod checkbox;
pub mod dialog;
pub mod glass;
pub mod hover_card;
pub mod input;
pub mod kbd;
pub mod label;
pub mod motion;
pub mod select;
pub mod separator;
pub mod skeleton;
pub mod switch;
pub mod table;
pub mod theme;
