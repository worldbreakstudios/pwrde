//! Vendored shadcn-style gpui components from [rcn](https://github.com/a1re1/rcn).
//!
//! These are faithful copies (not a Cargo dependency), pinned to the same gpui
//! git rev as this app so they compile unchanged. Tokens live in [`theme`];
//! bridge them from pwrde chrome via [`theme::Theme::from_chrome`] (added
//! locally). Deliberate local extensions, both in [`table`]: `TableRow::on_click`,
//! and shrinkable flex cells so columns stay aligned at narrow widths.

pub mod assets;
pub mod badge;
pub mod button;
pub mod card;
pub mod checkbox;
pub mod hover_card;
pub mod motion;
pub mod separator;
pub mod skeleton;
pub mod table;
pub mod theme;

pub use badge::{Badge, BadgeVariant};
pub use button::{Button, ButtonSize, ButtonVariant};
pub use card::{
    Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle,
};
pub use checkbox::Checkbox;
pub use hover_card::HoverCard;
pub use skeleton::Skeleton;
pub use table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
