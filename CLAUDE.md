# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

pwrde — a GPU-accelerated terminal workspace for macOS, written in Rust. Groups (vertical sidebar tabs) each hold a binary split tree of tiles; each tile has a horizontal tab strip of terminals (cmux-style). macOS-only as an app; the crate is a library plus a thin binary so the same modules can also be built for wasm32 (see "Web build" below).

There are three top-level pages (see `pages.rs`): **Sessions** (the terminal workspace), **Cleanup** (git-worktree hygiene, driven by the external `drop` CLI), and **Settings**.

## Commands

- `cargo run --release` — build and run the app. Debug builds also work (deps are compiled at `-O2` even in dev, so it stays usable).
- `cargo test` — run all tests. Tests are inline `#[cfg(test)]` modules in `src/*.rs`; there is no `tests/` directory.
- `cargo test <name>` — run a single test or filter by substring.
- `scripts/make-app.sh` — assemble `target/release/Pwrde.app` (requires `cargo build --release` first).
- `scripts/deploy.sh` — pull main, build, bundle, install to `/Applications`. Refuses to run off the `main` branch.
- `scripts/web-screenshot.sh [out.png] [query]` — build the wasm32 page under `web/`, serve it, and screenshot it with headless Chromium (driven over DevTools by `scripts/web-screenshot.mjs`, node 22+). The page is the real app seeded from `src/fixture.rs`; `?page=settings&dark=1` picks what to capture. `cd web && trunk serve` for a live page on :8090. Needs the patched wezterm branch (`scripts/web-wezterm-fork.sh --push`, once) or `PWRDE_WEZTERM_LOCAL=1` — see `docs/web-build.md`.

Builds compile through **sccache** (`.cargo/config.toml` sets `rustc-wrapper`), so a fresh worktree's first build pulls the gpui dependency tree from cache instead of recompiling it. sccache must be installed (`brew install sccache`) or cargo fails with "could not execute process `sccache`".

## Architecture

**The README's "Architecture" section is stale.** It describes the original winit + wgpu + glyphon design. The code has since been ported to **gpui** (Zed's UI framework), which is now the sole windowing + rendering layer. `winit`/`wgpu`/`glyphon` still appear in port-note comments but are not used by any code (and are no longer dependencies). Trust the module doc comments (`//!` headers in each `src/*.rs` file) over the README diagram.

### Dependency pinning constraints

- `wezterm-term` (VT emulation/grid) and `termwiz` (cell/color model) are git dependencies on the wezterm repo and **must be at the same rev** so cell types line up.
- `gpui` and `gpui_platform` are pinned to the same zed repo rev. `gpui_platform`'s `font-kit` feature is **required** — without it gpui falls back to `NoopTextSystem` and no glyphs render (quads still do), which is a confusing failure mode.
- `web/Cargo.lock` is the wasm workspace's own lock (seeded from the root one so zed/wezterm resolve to the same revs). When re-pinning gpui or wezterm at the root, run the same `cargo update --precise` inside `web/` too.

### Threading model (per terminal session)

Same shape as iTerm2: a PTY reader thread per session (`term.rs`) reads output in chunks, advances the wezterm-term grid (shared behind `Arc<Mutex>`), and sends a coalesced wakeup — only requested if one isn't already pending — over an mpsc channel drained on gpui's foreground executor. The main thread briefly locks the grid to snapshot visible cells, then paints unlocked. wezterm-term brings no I/O loop, so this reader/coalescing logic is hand-built here.

### Rendering split

Painting happens inside a single custom gpui `Element`'s `paint()` in `app.rs`. `renderer.rs` is deliberately **stateless and GPU-free**: `build_frame` walks the workspace tree + terminal grids and produces a `Frame` of plain data (quads, text runs, labels) that the Element then paints via `window.paint_quad` / `shape_line`. Keep geometry/color logic in `renderer.rs` and actual painting in `app.rs`.

**Exception: the Cleanup and Settings pages** are real gpui element trees (`cleanup_ui.rs` / `settings_ui.rs`, built from the vendored rcn components below) absolutely positioned over the canvas content area — not canvas-painted. Confirm dialogs and the Settings sidebar (search box + section tabs) stay on the canvas path. The Settings → Appearance section (segmented mode/preview controls, theme/terminal Selects, WYSIWYG preview mockups, token import/export) lives in the overlay too — its previews are element-tree divs painted with the exact theme colors.

### UI components (rcn, vendored in `src/ui/`)

`src/ui/` holds shadcn-style gpui components vendored from [rcn](https://github.com/a1re1/rcn) — "shadcn, but for gpui". Conventions:

- Components are **copied source, not a Cargo dependency** (that's rcn's model). They're pinned to the same gpui rev as the app so they compile unchanged.
- Each file is a port of a shadcn base-vega `ui/*.tsx` component; its `//!` header lists omissions vs the source and any **local additions** (e.g. `Table::h_full`, `TableRow::on_click`, `Badge::color`, `Card::h_full`). When extending a component locally, document the addition in that header and in `src/ui/mod.rs` — this is what keeps future `rcn diff` runs readable.
- Design tokens live in `src/ui/theme.rs` (shadcn's CSS variables as a gpui `Global`). pwrde chrome colors bridge into it via `Theme::from_chrome` — callers `cx.set_global(...)` before rendering rcn components so they track the live chrome theme.
- The **rcn CLI** manages vendoring: `cargo install --git https://github.com/a1re1/rcn rcn-cli`, then `rcn add <component>` to vendor a component (plus registry deps), `rcn list` to see what's installed, `rcn diff <component>` to compare local copies against the registry (useful given the local additions above).
- `rcn.toml` at the repo root maps the registry's default layout onto `src/ui/` (registry components import `crate::theme` / `crate::motion`; ours are `crate::ui::*`). Two consequences: after `rcn add`, rewrite the new file's imports to `crate::ui::…`, and in `src/ui/mod.rs` keep the `pub use` re-exports **above** the `pub mod` lines (`rcn add` regenerates everything after the first `pub mod` and would drop them). **Never run `rcn init` here** — it would overwrite `src/theme.rs` (pwrde's chrome themes) and replace `src/main.rs`.

### Web build (wasm32)

`web/` is a standalone cargo workspace (nightly + `build-std`, kept out of the native build) that boots the `pwrde` library through `gpui_web` + `gpui_wgpu` and paints to a browser canvas, so browser automation can screenshot and drive the UI. Native-only dependencies (`portable-pty`, `rusqlite`, `arboard`, `libc`, `gpui_platform`) sit under `[target.'cfg(not(target_family = "wasm"))'.dependencies]` and their call sites are cfg-gated; the wezterm crates need `web/patches/wezterm-wasm.patch` (four cfg fixes, carried on a fork branch). The real `App` boots there (`App::open_main_window` / `App::new` are shared with `run_native`), seeded from `fixture::DEMO`; clocks go through `web_time`, workers through `bg::spawn`. `docs/web-build.md` has the status table, what each native piece is replaced with, and what is next. When adding a native-only dependency or a `std::thread::spawn`, gate it the same way so the wasm check keeps passing.

### Module map

- `lib.rs` — the crate root: declares every module and re-exports the few root names modules reach as `crate::…` (`App`, `Drag`, `DropTarget`, `Page`, …).
- `main.rs` — the native binary: `fn main` calls `pwrde::app::run_native()`, nothing else.
- `app.rs` — the gpui `App` entity: window boot (`run_native`), the terminal `Element`, keyboard/mouse handling, tab drag & drop (`DropTarget`/`Drag`). Items are `pub(crate)` because the UI modules reach into it.
- `clipboard.rs` — the system clipboard behind one seam (`arboard` natively, no-ops on wasm32).
- `bg.rs` — off-thread work behind one seam (`std::thread::spawn` natively; on wasm32, where threads panic, the job is dropped and the fixture's canned `TermEvent`s answer instead). Use it for fire-and-forget workers that report back over `TermEvent`.
- `fixture.rs` — deterministic demo workspace for the web build and tests: groups/splits/tabs whose sessions are fed the recorded transcripts in `src/fixtures/*.vt` via `Session::feed`.
- `workspace.rs` — group/split-tree/tile/tab model plus pure layout math over the window size, so drawing and hit-testing/PTY-resize always agree.
- `term.rs` — `Session`: PTY (portable-pty) + VT emulation (wezterm-term) + reader thread.
- `renderer.rs` — stateless frame building (see above).
- `pages.rs` — top-level pages (Sessions, Cleanup, Settings) and rebindable keyboard `Action`s; every ⌘ shortcut resolves through a bindings table from settings keys `keyboard.<action>`.
- `command.rs` / `command_ui.rs` — the unified command palette (⌘P): a pure, unit-tested state machine (root commands grouped + fuzzy-filtered; `New session…` is the multi-step Repo › Base › Layout flow whose picks become token chips, ⌫ pops one) and its rcn element tree on a top-priority deferred layer. ⇧⌘T and the sidebar ＋ open the same palette with the command already committed; the flyover's first-open picker goes through it too. `palette.rs` keeps only `fuzzy_match`.
- `picker.rs` — the palette's step data: the directory scan (`~`, `~/src`, subdirs; git detection; pins/recents in `groups.json`), fork-source choices, and workspace-profile choices.
- `git.rs` — shells out to git for the fork-source picker (default branch, branch lists), mirroring what the `drop` worktree tool runs. Also `worktree_scope`, which settings/persist use to give each linked git worktree its own config/DB.
- `cleanup.rs` / `cleanup_ui.rs` — Cleanup page. `cleanup.rs` is the pure model (data structures, state, format helpers) over the external `drop` CLI (`drop -d --json` to list, `drop rm <id>... --json` to delete); `cleanup_ui.rs` builds the gpui element tree from the rcn components; side-effects live in `app.rs`.
- `settings_ui.rs` — Settings page content as an rcn element tree (all sections including Appearance, plus search-results mode), same overlay pattern as `cleanup_ui.rs`. Keyboard capture (search typing, primary-command Input save/cancel, ⌘-chord recording) stays in `app.rs`'s `handle_settings_key`.
- `persist.rs` — session persistence to SQLite (`<data_dir>/pwrde/state.db`, worktree-scoped like settings): group layouts, sidebar sections, shpool sessions. Additive schema migrations in `open_db`.
- `pwrspace.rs` — workspace profiles: saved group layouts in `.pwrspace.json` files, offered when creating a new group.
- `settings.rs` — flat key-value store at `~/.pwrde/settings.json` (worktree-scoped variant under `~/.pwrde/worktrees/<slug>/`); read once at startup, `set` rewrites the file. Load/save are pure functions over an explicit path so tests use temp dirs. A missing/corrupt file must never prevent launch.
- `theme.rs` — chrome color presets, chosen per appearance polarity (`"theme.light"` / `"theme.dark"` + `"appearance.mode"`), applied live; themes also compress to seven shareable hex tokens for import/export.
- `term_theme.rs` — terminal ANSI palette presets (`"terminal.light"` / `"terminal.dark"`), separate from chrome themes; `"default"` means wezterm's stock ANSI table on the chrome theme's `term_bg`.
- `claude_hooks.rs` — auto-installs Claude Code hooks (`~/.pwrde/claude-hook.sh`, merged additively into `~/.claude/settings.json`) that emit OSC 9 toasts back onto the pane's PTY so tabs get unread-attention dots.
- `links.rs` — OSC 8 hyperlinks + implicit URL detection over visible lines (hard-wrapped lines are joined before matching).
- `rect.rs` — pure char→geometry mapping for block elements / box-drawing chars and the cursor; painted as gpui quads because fonts can't be trusted to fill the cell.
- `gh.rs` — pull-request data + write actions (approve/comment/merge/ready), shelled through a settings-configurable CLI: `git.cli` (default `lfg`, swappable to `gh` or any drop-in with the same `pr view/list/diff --json` contract) and `git.async` (the `lfg -A` fast path). Returns normalized, gpui-free structs; pure JSON-parsing tested inline.
- `diff.rs` — pure unified-diff parser (status/rename/binary/counts/hunks with old/new gutters) shared by the PR files view and the local diff tool. `git.rs` grows `local_diff` (branch-vs-base and working-vs-HEAD modes, with untracked files) on top of it.
- `highlight.rs` — `syntect` wrapper mapping code lines to gpui-colored spans by file extension and chrome polarity; used by both diff viewers.
- `lfg.rs` — background tail of `lfg events` (the daemon's cache-updated SSE stream); forwards each event as a `TermEvent` so the open PR panel re-fetches when GitHub returns fresh data. Active only on the async `lfg` path.
- `pr_ui.rs` / `local_diff_ui.rs` — the Pull Request and Local-diff tools as gpui element-tree overlays (same pattern as `cleanup_ui`), positioned over the right-edge tool panel; both gated to git-backed Sessions groups (`active_cwd_is_git`). `pr_ui` owns the shared syntax-highlighted diff renderer.
- `ui/` — vendored rcn components (see "UI components" above).
