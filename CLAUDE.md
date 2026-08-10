# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

pwrde — a GPU-accelerated terminal workspace for macOS, written in Rust. Groups (vertical sidebar tabs) each hold a binary split tree of tiles; each tile has a horizontal tab strip of terminals (cmux-style). macOS-only.

There are three top-level pages (see `pages.rs`): **Sessions** (the terminal workspace), **Cleanup** (git-worktree hygiene, driven by the external `drop` CLI), and **Settings**.

## Commands

- `cargo run --release` — build and run the app. Debug builds also work (deps are compiled at `-O2` even in dev, so it stays usable).
- `cargo test` — run all tests. Tests are inline `#[cfg(test)]` modules in `src/*.rs`; there is no `tests/` directory.
- `cargo test <name>` — run a single test or filter by substring.
- `scripts/make-app.sh` — assemble `target/release/Pwrde.app` (requires `cargo build --release` first).
- `scripts/deploy.sh` — pull main, build, bundle, install to `/Applications`. Refuses to run off the `main` branch.

Builds compile through **sccache** (`.cargo/config.toml` sets `rustc-wrapper`), so a fresh worktree's first build pulls the gpui dependency tree from cache instead of recompiling it. sccache must be installed (`brew install sccache`) or cargo fails with "could not execute process `sccache`".

## Architecture

**The README's "Architecture" section is stale.** It describes the original winit + wgpu + glyphon design. The code has since been ported to **gpui** (Zed's UI framework), which is now the sole windowing + rendering layer. `winit`/`wgpu`/`glyphon` still appear in `Cargo.toml` and in port-note comments but are not used by any code. Trust the module doc comments (`//!` headers in each `src/*.rs` file) over the README diagram.

### Dependency pinning constraints

- `wezterm-term` (VT emulation/grid) and `termwiz` (cell/color model) are git dependencies on the wezterm repo and **must be at the same rev** so cell types line up.
- `gpui` and `gpui_platform` are pinned to the same zed repo rev. `gpui_platform`'s `font-kit` feature is **required** — without it gpui falls back to `NoopTextSystem` and no glyphs render (quads still do), which is a confusing failure mode.

### Threading model (per terminal session)

Same shape as iTerm2: a PTY reader thread per session (`term.rs`) reads output in chunks, advances the wezterm-term grid (shared behind `Arc<Mutex>`), and sends a coalesced wakeup — only requested if one isn't already pending — over an mpsc channel drained on gpui's foreground executor. The main thread briefly locks the grid to snapshot visible cells, then paints unlocked. wezterm-term brings no I/O loop, so this reader/coalescing logic is hand-built here.

### Rendering split

Painting happens inside a single custom gpui `Element`'s `paint()` in `main.rs`. `renderer.rs` is deliberately **stateless and GPU-free**: `build_frame` walks the workspace tree + terminal grids and produces a `Frame` of plain data (quads, text runs, labels) that the Element then paints via `window.paint_quad` / `shape_line`. Keep geometry/color logic in `renderer.rs` and actual painting in `main.rs`.

**Exception: the Cleanup and Settings pages** are real gpui element trees (`cleanup_ui.rs` / `settings_ui.rs`, built from the vendored rcn components below) absolutely positioned over the canvas content area — not canvas-painted. Confirm dialogs and the Settings sidebar (search box + section tabs) stay on the canvas path. The Appearance section's WYSIWYG preview cards are plain gpui divs with explicit colors from the previewed theme, deliberately not rcn theme tokens.

### UI components (rcn, vendored in `src/ui/`)

`src/ui/` holds shadcn-style gpui components vendored from [rcn](https://github.com/a1re1/rcn) — "shadcn, but for gpui". Conventions:

- Components are **copied source, not a Cargo dependency** (that's rcn's model). They're pinned to the same gpui rev as the app so they compile unchanged.
- Each file is a port of a shadcn base-vega `ui/*.tsx` component; its `//!` header lists omissions vs the source and any **local additions** (e.g. `Table::h_full`, `TableRow::on_click`, `Badge::color`, `Card::h_full`). When extending a component locally, document the addition in that header and in `src/ui/mod.rs` — this is what keeps future `rcn diff` runs readable.
- Design tokens live in `src/ui/theme.rs` (shadcn's CSS variables as a gpui `Global`). pwrde chrome colors bridge into it via `Theme::from_chrome` — callers `cx.set_global(...)` before rendering rcn components so they track the live chrome theme.
- The **rcn CLI** manages vendoring: `cargo install --git https://github.com/a1re1/rcn rcn-cli`, then `rcn add <component>` to vendor a component (plus registry deps), `rcn list` to see what's installed, `rcn diff <component>` to compare local copies against the registry (useful given the local additions above).
- `rcn.toml` at the repo root maps the registry's default layout onto `src/ui/` (registry components import `crate::theme` / `crate::motion`; ours are `crate::ui::*`). Two consequences: after `rcn add`, rewrite the new file's imports to `crate::ui::…`, and in `src/ui/mod.rs` keep the `pub use` re-exports **above** the `pub mod` lines (`rcn add` regenerates everything after the first `pub mod` and would drop them). **Never run `rcn init` here** — it would overwrite `src/theme.rs` (pwrde's chrome themes) and replace `src/main.rs`.

### Module map

- `main.rs` — gpui app, window, the terminal `Element`, keyboard/mouse handling, tab drag & drop (`DropTarget`/`Drag`).
- `workspace.rs` — group/split-tree/tile/tab model plus pure layout math over the window size, so drawing and hit-testing/PTY-resize always agree.
- `term.rs` — `Session`: PTY (portable-pty) + VT emulation (wezterm-term) + reader thread.
- `renderer.rs` — stateless frame building (see above).
- `pages.rs` — top-level pages (Sessions, Cleanup, Settings) and rebindable keyboard `Action`s; every ⌘ shortcut resolves through a bindings table from settings keys `keyboard.<action>`.
- `palette.rs` — command palette model (⌘P): fuzzy subsequence search over action labels.
- `picker.rs` — directory picker for new groups (scans `~`, `~/src`, and its subdirs; git detection; pins/recents persisted to `groups.json` in the data dir) and the fork-source picker.
- `git.rs` — shells out to git for the fork-source picker (default branch, branch lists), mirroring what the `drop` worktree tool runs. Also `worktree_scope`, which settings/persist use to give each linked git worktree its own config/DB.
- `cleanup.rs` / `cleanup_ui.rs` — Cleanup page. `cleanup.rs` is the pure model (data structures, state, format helpers) over the external `drop` CLI (`drop -d --json` to list, `drop rm <id>... --json` to delete); `cleanup_ui.rs` builds the gpui element tree from the rcn components; side-effects live in `main.rs`.
- `settings_ui.rs` — Settings page content as an rcn element tree (all five sections + search-results mode), same overlay pattern as `cleanup_ui.rs`. Keyboard capture (search typing, primary-command Input save/cancel, ⌘-chord recording) stays in `main.rs`'s `handle_settings_key`.
- `persist.rs` — session persistence to SQLite (`<data_dir>/pwrde/state.db`, worktree-scoped like settings): group layouts, sidebar sections, shpool sessions. Additive schema migrations in `open_db`.
- `pwrspace.rs` — workspace profiles: saved group layouts in `.pwrspace.json` files, offered when creating a new group.
- `settings.rs` — flat key-value store at `~/.pwrde/settings.json` (worktree-scoped variant under `~/.pwrde/worktrees/<slug>/`); read once at startup, `set` rewrites the file. Load/save are pure functions over an explicit path so tests use temp dirs. A missing/corrupt file must never prevent launch.
- `theme.rs` — chrome color presets, chosen per appearance polarity (`"theme.light"` / `"theme.dark"` + `"appearance.mode"`), applied live; themes also compress to seven shareable hex tokens for import/export.
- `term_theme.rs` — terminal ANSI palette presets (`"terminal.light"` / `"terminal.dark"`), separate from chrome themes; `"default"` means wezterm's stock ANSI table on the chrome theme's `term_bg`.
- `claude_hooks.rs` — auto-installs Claude Code hooks (`~/.pwrde/claude-hook.sh`, merged additively into `~/.claude/settings.json`) that emit OSC 9 toasts back onto the pane's PTY so tabs get unread-attention dots.
- `links.rs` — OSC 8 hyperlinks + implicit URL detection over visible lines (hard-wrapped lines are joined before matching).
- `rect.rs` — pure char→geometry mapping for block elements / box-drawing chars and the cursor; painted as gpui quads because fonts can't be trusted to fill the cell.
- `ui/` — vendored rcn components (see "UI components" above).
