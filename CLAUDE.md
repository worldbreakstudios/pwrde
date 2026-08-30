# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

pwrde — a GPU-accelerated terminal workspace for macOS, written in Rust. Groups (vertical sidebar tabs) each hold a binary split tree of tiles; each tile has a horizontal tab strip of terminals (cmux-style). macOS-only.

The top-level pages (see `pages.rs`) are **Sessions** (the terminal workspace), **Pull Requests**, one page per user-registered **CLI tool** (`cli_tools.rs` — a full-page, non-persisted terminal running e.g. `drop -d` from `~/src`; the built-in default replaces the old Cleanup page), and **Settings**.

## Commands

- `cargo run --release` — build and run the app. Debug builds also work (deps are compiled at `-O2` even in dev, so it stays usable).
- `cargo run --bin pwrde-cli -- <subcommand>` — drive a running app over the command bus (`pwrde-cli --help`; e.g. `pwrde-cli new-session ~/src/pwrde`, `pwrde-cli send-text 'ls' --enter`, `pwrde-cli screenshot /tmp/app.png`, `pwrde-cli state`).
- `cargo test` — run all tests. Tests are inline `#[cfg(test)]` modules in `src/*.rs`; there is no `tests/` directory.
- `cargo test <name>` — run a single test or filter by substring.
- `scripts/make-app.sh` — assemble `target/release/Pwrde.app` (requires `cargo build --release` first).
- `scripts/deploy.sh` — pull main, build, bundle, install to `/Applications`, and install `pwrde-cli` onto PATH (`$PWRDE_CLI_DIR`, else `~/.cargo/bin`, else `/usr/local/bin`). Refuses to run off the `main` branch.

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

**Exception: the Settings page** (and the Pull Requests / Notes pages) are real gpui element trees (`settings_ui.rs` etc., built from the vendored rcn components below) absolutely positioned over the canvas content area — not canvas-painted. Confirm dialogs and the Settings sidebar (search box + section tabs) stay on the canvas path. The Settings → Appearance section (segmented mode/preview controls, theme/terminal Selects, WYSIWYG preview mockups, token import/export) lives in the overlay too — its previews are element-tree divs painted with the exact theme colors.

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
- `pages.rs` — top-level pages (`Page`: Sessions, PullRequests, `Tool(i)` per registered CLI tool, Notes, Settings — `Page::all(n_tools)` gives the dot-strip order) and rebindable keyboard `Action`s; every ⌘ shortcut resolves through a bindings table from settings keys `keyboard.<action>`.
- `command.rs` / `command_ui.rs` — the unified command palette (⌘P): a pure, unit-tested state machine (root commands grouped + fuzzy-filtered; `New session…` is the multi-step Repo › Base › Layout flow whose picks become token chips, ⌫ pops one) and its rcn element tree on a top-priority deferred layer. ⇧⌘T and the sidebar ＋ open the same palette with the command already committed; the flyover's first-open picker goes through it too. `palette.rs` keeps only `fuzzy_match`.
- `picker.rs` — the palette's step data: the directory scan (`~`, `~/src`, subdirs; git detection; pins/recents in `groups.json`), fork-source choices, and workspace-profile choices.
- `git.rs` — shells out to git for the fork-source picker (default branch, branch lists), mirroring what the `drop` worktree tool runs. Also `worktree_scope`, which settings/persist use to give each linked git worktree its own config/DB.
- `cli_tools.rs` — the CLI tool page registry: `{name, command, cwd, icon}` entries stored as a JSON string under the `tools.cli` setting (absent → the built-in `drop -d` default; `[]` → the user removed it). `main.rs` holds one lazily-spawned `ToolSession` per tool (`ensure_tool_session`, outside the workspace tree so it is never persisted), `renderer.rs::tool_page` paints it as a full-area card, and Settings → Tools (in `settings_ui.rs`) adds/removes entries. The command runs as `$SHELL -lc <command>` (`Session::new`'s `command` arg) so login-shell PATH applies.
- `settings_ui.rs` — Settings page content as an rcn element tree (all sections including Appearance, plus search-results mode), absolutely positioned over the content area. Keyboard capture (search typing, primary-command and Tools-form Input save/cancel, ⌘-chord recording) stays in `main.rs`'s `handle_settings_key`.
- `persist.rs` — session persistence to SQLite (`<data_dir>/pwrde/state.db`, worktree-scoped like settings): group layouts, sidebar sections, shpool sessions. Additive schema migrations in `open_db`.
- `pwrspace.rs` — workspace profiles: saved group layouts in `.pwrspace.json` files, offered when creating a new group.
- `settings.rs` — flat key-value store at `~/.pwrde/settings.json` (worktree-scoped variant under `~/.pwrde/worktrees/<slug>/`); read once at startup, `set` rewrites the file. Load/save are pure functions over an explicit path so tests use temp dirs. A missing/corrupt file must never prevent launch.
- `theme.rs` — chrome color presets, chosen per appearance polarity (`"theme.light"` / `"theme.dark"` + `"appearance.mode"`), applied live; themes also compress to seven shareable hex tokens for import/export.
- `term_theme.rs` — terminal ANSI palette presets (`"terminal.light"` / `"terminal.dark"`), separate from chrome themes; `"default"` means wezterm's stock ANSI table on the chrome theme's `term_bg`.
- `bus.rs` / `bus_exec.rs` / `bin/pwrde-cli.rs` — the command bus. `bus.rs` is the crate-independent wire protocol (JSON-tagged `Command`/`Reply`, NDJSON over a Unix socket at `~/.pwrde/bus.sock` — worktree-scoped like settings — plus `serve`/`request`); it must stay free of `crate::` imports because `pwrde-cli` includes it via `#[path]`. `bus_exec.rs` is `App::execute`, the socket listener's dispatcher; its `Command::Action` arm calls the same `App::run_action` the ⌘P palette and keyboard bindings use (which returns whether the action applied, so the bus can report no-ops), and the other arms cover what the palette can't parameterize: new-session, send-text, focus/move groups, sections, page navigation, window resize, `state` (JSON snapshot for agents) and `screenshot` (in-process `CGWindowListCreateImage` of our own window — no Screen Recording grant needed; `screencapture` is only a fallback). Child shells get `PWRDE_SOCKET` so `pwrde-cli` inside a tab targets its own app. Every parameterless UI command is an `Action` (so it is palette-listed and rebindable under `keyboard.<name>`); parameterized mutations plus the `state`/`ping`/`list_commands` introspection commands are bus/CLI-only.
- `claude_hooks.rs` — auto-installs Claude Code hooks (`~/.pwrde/claude-hook.sh`, merged additively into `~/.claude/settings.json`) that emit OSC 9 toasts back onto the pane's PTY so tabs get unread-attention dots.
- `links.rs` — OSC 8 hyperlinks + implicit URL detection over visible lines (hard-wrapped lines are joined before matching).
- `rect.rs` — pure char→geometry mapping for block elements / box-drawing chars and the cursor; painted as gpui quads because fonts can't be trusted to fill the cell.
- `gh.rs` — pull-request data + write actions (approve / comment / ready / merge / close / resolve thread, plus inline line comments, thread replies and a batched review submission via `api` REST with a stdin JSON body), shelled through a settings-configurable CLI: `git.cli` (default `lfg`, swappable to `gh` or any drop-in with the same `pr view/list/diff --json` contract) and `git.async` (the `lfg -A` fast path). Returns normalized, gpui-free structs; pure JSON-parsing tested inline.
- `diff.rs` — pure unified-diff parser (status/rename/binary/counts/hunks with old/new gutters) shared by the PR files view and the local diff tool. `git.rs` grows `local_diff` (branch-vs-base and working-vs-HEAD modes, with untracked files) on top of it.
- `highlight.rs` — `syntect` wrapper mapping code lines to gpui-colored spans by file extension and chrome polarity; used by both diff viewers.
- `lfg.rs` — background tail of `lfg events` (the daemon's cache-updated SSE stream); forwards each event as a `TermEvent` so the open PR panel re-fetches when GitHub returns fresh data. Active only on the async `lfg` path.
- `pr_ui.rs` / `local_diff_ui.rs` — the Pull Request and Local-diff tools as gpui element-tree overlays (same pattern as `settings_ui`), positioned over the right-edge tool panel; both gated to git-backed Sessions groups (`active_cwd_is_git`). The PR review is one continuous stream (description → conversation → per-file diff cards, unified or split) beside a contact-card sidebar on the Pull Requests page (identity, Readiness, file tree, contextual primary action); the narrow tool surface folds identity into the header. Inline line comments, thread replies and a batched pending review ("Start review" → Submit) use `gpui_component` `Textarea` composers. `pr_ui` owns the shared syntax-highlighted diff renderer (`render_diff_files`, `split_rows`).
- `ui/` — vendored rcn components (see "UI components" above).
