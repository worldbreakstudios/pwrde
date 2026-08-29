# Web build (wasm32)

pwrde is a native macOS app, which makes visual testing awkward: a coding agent
can't screenshot or drive an AppKit window without screen-recording
permissions. gpui — the UI framework pwrde is built on — also has a web
platform (`gpui_web` + `gpui_wgpu`, the same stack rcn's storybook runs on),
so the plan is to boot the same Rust modules inside a browser canvas and let
headless Chromium screenshot and drive them.

This document is the map: what is in place, what each native-only piece is
replaced with on wasm32, and what is left before the full app renders in a
browser.

## Status

| Step | State |
| --- | --- |
| Library + thin binary split (`src/lib.rs`, `src/app.rs`, `src/main.rs`) | done |
| Native-only deps behind `cfg(not(target_family = "wasm"))` | done |
| wezterm crates compile for wasm32 (`web/patches/wezterm-wasm.patch`) | done |
| `web/` trunk workspace, `scripts/web-screenshot.sh` (DevTools capture) | done |
| Boot the real `App` on the web (PTY-less fixture sessions, fonts, clocks) | done |
| Deep-link a page/state for screenshots (`?page=…`, `?dark=`, `?fixture=`) | done |
| Drive it: keyboard/mouse through the gstack `/browse` skill, more fixtures | **next** |

## Layout

```
web/                    standalone cargo workspace (nightly + build-std must not
  Cargo.toml            leak into the native build); depends on `pwrde` by path
  main.rs               wasm entry point: gpui_web boot, embedded fonts, the
                        real `App` seeded from `pwrde::fixture`, `?page=` etc.
  fonts/                JetBrainsMono Nerd Font Mono (see fonts/README.md)
  index.html            trunk template — a full-page <canvas>
  trunk.toml            dev server on :8090 with the COOP/COEP headers
                        SharedArrayBuffer (wasm threads) needs
  .cargo/config.toml    atomics/shared-memory rustflags, build-std, getrandom cfg;
                        disables the parent's sccache wrapper
  rust-toolchain.toml   nightly + wasm32-unknown-unknown + rust-src
  patches/wezterm-wasm.patch  the four wezterm cfg fixes (see below)
src/fixture.rs          demo workspace + `src/fixtures/*.vt` recorded transcripts
src/bg.rs               off-thread work seam (std thread natively, dropped on wasm)
scripts/web-wezterm-fork.sh   builds the patched wezterm branch (target/wezterm-fork)
scripts/web-screenshot.sh     trunk serve + headless Chromium → PNG
scripts/web-screenshot.mjs    the DevTools-protocol capture the .sh drives (node 22+)
```

## Prerequisites

- `trunk` (`brew install trunk`) and rustup's nightly with the wasm target
  (`web/rust-toolchain.toml` pulls it in on first use). Run cargo through
  rustup — Homebrew's `cargo` does not understand `+nightly`.
- A headless Chromium. `scripts/web-screenshot.sh` defaults to Playwright's
  `chrome-headless-shell` (`npx playwright install chromium`); set
  `PWRDE_CHROME` to use another binary.
- The patched wezterm branch. `web/Cargo.toml` redirects every wezterm crate to
  `a1re1/wezterm` branch `pwrde-wasm-patches`; publish it once with
  `scripts/web-wezterm-fork.sh --push` (it is upstream at the rev pwrde's
  `Cargo.lock` pins, plus `web/patches/wezterm-wasm.patch`). Re-run after
  bumping the wezterm rev.

  To build without the remote branch (or to test a patch change), the script
  also writes `target/wezterm-fork-patch.toml`, a cargo config that redirects
  the wezterm crates to its local checkout — this is how the wasm check in
  this document was verified:

  ```sh
  scripts/web-wezterm-fork.sh
  cd web && cargo +nightly check --target wasm32-unknown-unknown \
      --config ../target/wezterm-fork-patch.toml
  ```

  (`trunk` has no way to pass `--config`; for `trunk serve` and the screenshot
  script use `PWRDE_WEZTERM_LOCAL=1`, below, until the branch is published.)

  `web/Cargo.lock` was last resolved against that local checkout, so its
  wezterm entries carry no `source`; the first build against the published
  branch rewrites them (commit the result).

## Running

```sh
cd web && trunk serve            # http://127.0.0.1:8090
scripts/web-screenshot.sh        # → target/web-screenshot.png
scripts/web-screenshot.sh out.png '?page=settings&dark=1'
PWRDE_WEZTERM_LOCAL=1 scripts/web-screenshot.sh   # against target/wezterm-fork, no published branch needed
```

The page boots the real `App` — sidebar, groups, split tiles, tab strips,
every page — with the workspace seeded from recorded transcripts
(`pwrde::fixture::DEMO`: a `pwrde` group split shell | claude with a build
tab, and an `rcn` group). Query parameters select what to look at:

| Parameter | Effect |
| --- | --- |
| `?page=sessions` / `settings` / `cleanup` / … | open on that page (`pages::Page` variant names) |
| `?dark=1` / `?dark=0` | force the appearance polarity (sets `appearance.mode`) |
| `?fixture=none` | skip the demo workspace — the empty-state CTA |
| `?backend=webgpu` / `?backend=webgl` | force a renderer (default auto-detects) |

`PWRDE_WEZTERM_LOCAL=1` appends the local patch table to
`web/.cargo/config.toml` for the run and restores the file afterwards (trunk
cannot pass cargo `--config`). In this mode cargo may print "patch … was not
used in the crate graph" for each wezterm crate — noise from the lock having
recorded them as path deps; the compiled sources are the checkout's. This is how the smoke page was verified: a dev
build, WebGPU via SwiftShader (`selected=BrowserWebGpu` in the console log),
1200×720 capture showing the card, badges, and buttons in the chrome theme.

The screenshot script runs Chromium with `--use-angle=swiftshader` so it has a
software WebGL/WebGPU device, and drives it over the DevTools protocol
(`scripts/web-screenshot.mjs`, dependency-free node): navigate, wait
`PWRDE_WEB_SETTLE_MS` (6000) of *real* time for the wasm to boot and paint,
capture. Chromium's own `--screenshot --virtual-time-budget` mode is not
usable here: it freezes timers after the load event, which stalls gpui's frame
loop before the app has laid itself out. Page console output (including Rust
panics, with `Error.stackTraceLimit` raised in `index.html` so the Rust frames
survive) is appended to `target/web-serve.log`.

Known gaps in the picture: glyphs outside JetBrainsMono Nerd Font Mono and
IBM Plex Sans — emoji, `⏺`/`✻`, rounded box-drawing corners — draw as boxes;
sidebar cards show the pane title (`zsh`) rather than the fixture group name,
exactly as native does for a pane whose shell has not set a title.

## The wezterm patch

`wezterm-term` (VT emulation and the grid) is pure Rust, but three crates
under it assumed unix-or-windows and failed to compile for wasm32:

- `filedescriptor` — its whole body referenced `RawFileDescriptor` and
  friends that only the unix/windows modules define. The real implementation
  moves to `native.rs` behind `cfg(any(unix, windows))`; other targets get
  only `Error`, which is all `termwiz` reaches outside its own unix/windows
  terminal modules.
- `termwiz` — `Terminal::waker`, `new_terminal`, and
  `line_editor_terminal` are gated the same way.
- `wezterm-escape-parser` — the kitty shared-memory image transfer got a
  fallback `read_shared_memory_data` that returns `io::ErrorKind::Unsupported`.

Native builds are unaffected (checked with `cargo check` in the fork). The
patch is small enough to carry; upstreaming it is worth a try.

On top of the patch, wasm32 needs a randomness backend spelled out:
`getrandom` with `wasm_js` (feature in `Cargo.toml`, `--cfg` in
`web/.cargo/config.toml`) and `uuid` with `js`, both reached through
`wezterm-blob-leases`.

## What replaces each native-only piece

| Native | Where | On wasm32 |
| --- | --- | --- |
| `gpui_platform::current_platform` (macOS window) | `app::run_native` | `gpui_platform::application_with_web_backend` in `web/main.rs` |
| `portable-pty` — the PTY behind `term::Session` | `term.rs` | `Session::new` and the PTY master are compiled out; a `Session` fed recorded VT bytes is the next step |
| `rusqlite` — session persistence | `persist.rs` | `save_snapshot_default` / `load_snapshot_default` are no-ops; a page starts empty |
| `arboard` — system clipboard | `clipboard.rs` | no-ops (`set_text`, `contents`) until routed through the browser clipboard API |
| `libc::localtime_r` — sidebar day buckets | `sidebar_card.rs` | UTC (`local_offset` returns 0) |
| `objc` / `raw-window-handle` — titlebar hairline hack | `app.rs` | already `cfg(target_os = "macos")` |
| `std::thread::spawn` — git/PR/cleanup/ps workers | `bg.rs` | the job is dropped; callers already tolerate a worker that never answers |
| `std::time::Instant` / `SystemTime` | everywhere | `web_time` (std re-exported natively, `performance.now()` on wasm) |
| system fonts | `web/main.rs` | `web/fonts/` registered via `text_system().add_fonts` |
| `wezterm-term`'s writer thread | patch | writes synchronously on targets without threads |

Things that **compile** on wasm32 and simply fail at runtime, which the app
tolerates: `std::process::Command` (git, gh/lfg, drop, ps, shpool, mmdc)
returns an error, so the git-context cards stay bare and the PR/cleanup panels
stay on their loading state; `dirs::home_dir()` is `None`, so `settings::init`
loads the empty store (defaults everywhere) and writes are lost — which is how
`?dark=` works without a settings file.

One rendering detail the web surfaced: gpui_web's window reports a 1×1
viewport until the canvas is laid out, so the first paint resizes the
renderer's surface after the sidebar's row and page-dot layers already
measured against the old one. `paint_terminal` now leaves `dirty` set when
the surface changed, so the next drain tick re-renders — on native the window
has its size from the start and this never fired.

## How the real `App` boots on the web

`App::open_main_window(cx, options, events)` and `App::new(...)` are the shared
boot (`run_native` calls them too; `app::init_globals` seeds the theme and
component globals). `web/main.rs` registers the embedded fonts, opens the
window, then seeds `fixture::DEMO` and applies `?page=` inside a
`window.update`. Sessions come from the wasm `Session::new` (no PTY, sink
writer) and are filled with `Session::feed(bytes)`.

## Next

- Interaction: click a group card, switch tabs, open the command palette from
  the gstack `/browse` skill against `trunk serve`, and grow
  `scripts/web-screenshot.sh` into a small flow runner (navigate, click,
  capture) for review threads.
- More fixtures: a PR panel fed a recorded `gh pr view --json`, a Cleanup scan
  from a recorded `drop -d --json`, so those pages render populated instead of
  loading.
- Group names in cards: the fixture could set an OSC 0 title per pane so
  cards read `pwrde` / `rcn` rather than `zsh`.
- A symbols fallback font (Nerd Font Symbols or Noto Symbols) for the glyphs
  the terminal transcripts use.
