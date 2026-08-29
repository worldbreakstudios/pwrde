//! pwrde — a GPU-accelerated terminal workspace for macOS, rendered with gpui.
//!
//! The crate is a library plus a thin native binary (`src/main.rs`) so the
//! same modules can be booted by more than one entry point: the macOS app
//! today, and the wasm32 build under `web/` (gpui_web + gpui_wgpu) that lets
//! browser automation screenshot the UI. See `docs/web-build.md`.
//!
//! `app` holds the gpui `App` entity, the terminal `Element`, and all input
//! handling; everything else is the module map described in `CLAUDE.md`.

pub mod app;
pub mod claude_hooks;
pub mod clipboard;
pub mod cleanup;
pub mod cleanup_ui;
pub mod diff;
pub mod features;
pub mod file_tree;
pub mod flyover_ui;
pub mod gh;
pub mod git;
pub mod git_context;
pub mod highlight;
pub mod lfg;
pub mod launch_ui;
pub mod local_diff_ui;
pub mod markdown;
pub mod mermaid;
pub mod modal_ui;
pub mod notes;
pub mod notes_ui;
pub mod pr_ui;
pub mod settings_ui;
pub mod links;
pub mod pages;
pub mod palette_ui;
pub mod palette;
pub mod persist;
pub mod picker;
pub mod picker_ui;
pub mod pwrspace;
pub mod rect;
pub mod renderer;
pub mod resize_ui;
pub mod ribbon_ui;
pub mod save_ui;
pub mod settings;
pub mod sidebar_card;
pub mod sidebar_ui;
pub mod term;
pub mod tile_ui;
pub mod term_theme;
pub mod theme;
// Vendored shadcn-style component copies (see ui/mod.rs). Kept faithful to
// their rcn source rather than pruned to current usage.
pub mod ui;
pub mod workspace;

// Root-level names the modules reach as `crate::…`; they lived at the crate
// root when `app.rs` was `main.rs`.
pub(crate) use app::{App, ConfirmAction, ConfirmClose, Drag, DropTarget, GRAB};
pub(crate) use pages::Page;
