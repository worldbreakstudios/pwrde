# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

pwrde — a GPU-accelerated terminal workspace for macOS, written in Rust. Groups (vertical sidebar tabs) each hold a binary split tree of tiles; each tile has a horizontal tab strip of terminals (cmux-style). macOS-only.

## Commands

- `cargo run --release` — build and run the app. Debug builds also work (deps are compiled at `-O2` even in dev, so it stays usable).
- `cargo test` — run all tests. Tests are inline `#[cfg(test)]` modules in `src/*.rs`; there is no `tests/` directory.
- `cargo test <name>` — run a single test or filter by substring.
- `scripts/make-app.sh` — assemble `target/release/Pwrde.app` (requires `cargo build --release` first).
- `scripts/deploy.sh` — pull main, build, bundle, install to `/Applications`. Refuses to run off the `main` branch.

## Architecture

**The README's "Architecture" section is stale.** It describes the original winit + wgpu + glyphon design. The code has since been ported to **gpui** (Zed's UI framework), which is now the sole windowing + rendering layer. `winit`/`wgpu`/`glyphon` still appear in `Cargo.toml` and in port-note comments but are not used by any code. Trust the module doc comments (`//!` headers in each `src/*.rs` file) over the README diagram.

### Dependency pinning constraints

- `wezterm-term` (VT emulation/grid) and `termwiz` (cell/color model) are git dependencies on the wezterm repo and **must be at the same rev** so cell types line up.
- `gpui` and `gpui_platform` are pinned to the same zed repo rev. `gpui_platform`'s `font-kit` feature is **required** — without it gpui falls back to `NoopTextSystem` and no glyphs render (quads still do), which is a confusing failure mode.

### Threading model (per terminal session)

Same shape as iTerm2: a PTY reader thread per session (`term.rs`) reads output in chunks, advances the wezterm-term grid (shared behind `Arc<Mutex>`), and sends a coalesced wakeup — only requested if one isn't already pending — over an mpsc channel drained on gpui's foreground executor. The main thread briefly locks the grid to snapshot visible cells, then paints unlocked. wezterm-term brings no I/O loop, so this reader/coalescing logic is hand-built here.

### Rendering split

Painting happens inside a single custom gpui `Element`'s `paint()` in `main.rs`. `renderer.rs` is deliberately **stateless and GPU-free**: `build_frame` walks the workspace tree + terminal grids and produces a `Frame` of plain data (quads, text runs, labels) that the Element then paints via `window.paint_quad` / `shape_line`. Keep geometry/color logic in `renderer.rs` and actual painting in `main.rs`.

### Module map

- `main.rs` — gpui app, window, the terminal `Element`, keyboard/mouse handling, tab drag & drop (`DropTarget`/`Drag`).
- `workspace.rs` — group/split-tree/tile/tab model plus pure layout math over the window size, so drawing and hit-testing/PTY-resize always agree.
- `term.rs` — `Session`: PTY (portable-pty) + VT emulation (wezterm-term) + reader thread.
- `renderer.rs` — stateless frame building (see above).
- `pages.rs` — top-level pages (Sessions, Settings) and rebindable keyboard `Action`s; every ⌘ shortcut resolves through a bindings table from settings keys `keyboard.<action>`.
- `picker.rs` — directory picker for new groups (scans `~`, `~/src`, and its subdirs; git detection; pins/recents persisted to `groups.json` in the data dir) and the fork-source picker.
- `git.rs` — shells out to git for the fork-source picker (default branch, branch lists), mirroring what the `drop` worktree tool runs.
- `settings.rs` — flat key-value store at `~/.pwrde/settings.json`; read once at startup, `set` rewrites the file. Load/save are pure functions over an explicit path so tests use temp dirs. A missing/corrupt file must never prevent launch.
- `theme.rs` — chrome color presets, persisted under the `"theme"` settings key, applied live. Terminal ANSI colors come from the wezterm palette and are not themed here.
- `links.rs` — OSC 8 hyperlinks + implicit URL detection over visible lines (hard-wrapped lines are joined before matching).
- `rect.rs` — pure char→geometry mapping for block elements / box-drawing chars and the cursor; painted as gpui quads because fonts can't be trusted to fill the cell.
