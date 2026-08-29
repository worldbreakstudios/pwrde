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
| `web/` trunk workspace, smoke page, `scripts/web-screenshot.sh` | done |
| Boot the real `App` on the web (PTY-less fixture sessions, fonts, clocks) | **next** |
| Deep-link a page/state for screenshots (`?page=settings`, fixture layouts) | later |

## Layout

```
web/                    standalone cargo workspace (nightly + build-std must not
  Cargo.toml            leak into the native build); depends on `pwrde` by path
  main.rs               wasm entry point: gpui_web boot + the smoke page
  index.html            trunk template — a full-page <canvas>
  trunk.toml            dev server on :8090 with the COOP/COEP headers
                        SharedArrayBuffer (wasm threads) needs
  .cargo/config.toml    atomics/shared-memory rustflags, build-std, getrandom cfg;
                        disables the parent's sccache wrapper
  rust-toolchain.toml   nightly + wasm32-unknown-unknown + rust-src
  patches/wezterm-wasm.patch  the four wezterm cfg fixes (see below)
scripts/web-wezterm-fork.sh   builds the patched wezterm branch (target/wezterm-fork)
scripts/web-screenshot.sh     trunk serve + headless Chromium → PNG
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
cd web && trunk serve            # http://127.0.0.1:8090  (?backend=webgl to force WebGL)
scripts/web-screenshot.sh        # → target/web-screenshot.png
scripts/web-screenshot.sh out.png '?backend=webgl'
PWRDE_WEZTERM_LOCAL=1 scripts/web-screenshot.sh   # against target/wezterm-fork, no published branch needed
```

`PWRDE_WEZTERM_LOCAL=1` appends the local patch table to
`web/.cargo/config.toml` for the run and restores the file afterwards (trunk
cannot pass cargo `--config`). In this mode cargo may print "patch … was not
used in the crate graph" for each wezterm crate — noise from the lock having
recorded them as path deps; the compiled sources are the checkout's. This is how the smoke page was verified: a dev
build, WebGPU via SwiftShader (`selected=BrowserWebGpu` in the console log),
1200×720 capture showing the card, badges, and buttons in the chrome theme.

The screenshot script runs Chromium with `--use-angle=swiftshader` so it has a
software WebGL/WebGPU device, and `--virtual-time-budget` so the wasm module
boots and paints before the capture. Raise `PWRDE_WEB_BUDGET_MS` if a page
comes out blank.

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

Things that **compile** on wasm32 but must not run there — the web entry
point has to route around them when it boots the real `App`:

- `std::process::Command` (git, gh/lfg, drop, ps, shpool, mmdc) returns an
  error on wasm; the sidebar/PR/cleanup refreshers must be skipped or fed
  fixtures.
- `std::thread::spawn` panics on wasm32-unknown-unknown; gpui's
  `background_spawn` (backed by `wasm_thread`) is the replacement.
- `std::time::Instant::now()` panics on wasm — `App` stamps several poll
  timers with it. Use `web_time::Instant` (a drop-in that maps to
  `performance.now()`), as rcn does.
- `dirs::home_dir()` is `None`; `settings::init` tolerates that (empty
  store, defaults everywhere) but writes are lost.

## Next: booting the real `App`

1. Extract the `App { … }` construction in `app::run_native`'s window callback
   into `App::new(events, renderer, window, cx)` so both entry points share it.
2. `term::Session::from_bytes(id, cols, rows, bytes)` — a wezterm-term grid
   with an in-memory writer and no PTY; fixtures live under `web/fixtures/`
   (recorded terminal output: a shell prompt, an `ls`, a Claude Code session).
3. Swap `std::time::Instant` for `web_time::Instant` in `app.rs` (identical
   API on native).
4. Fonts: `gpui_web` embeds only IBM Plex Sans and Lilex. Embed the terminal
   and chrome faces the settings default to, or map them to Lilex/Plex on
   wasm in `renderer::measure_cell_width`.
5. `web/main.rs`: read `?page=…&layout=…` from the URL to seed a page and a
   fixture workspace, then `scripts/web-screenshot.sh out.png '?page=settings'`
   gives an agent a deterministic view of any screen, and the gstack `/browse`
   skill can click through it.
