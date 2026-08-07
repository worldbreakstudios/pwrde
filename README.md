# pwrde

A GPU-accelerated terminal for macOS, written in Rust — the foundation for a
multi-terminal, agent-driven dev workspace. Built on WezTerm's core crates with
iTerm2's performance architecture.

```sh
cargo run --release
```

## UI model

Borderless window with custom traffic lights. The left sidebar (resizable —
drag its edge) holds vertical *group* tabs; its top strip is the window drag
handle. Each group is a binary split tree of *tiles*; every tile has a
horizontal tab strip (cmux-style) holding one or more terminals.

## Shortcuts

| Keys | Action |
|---|---|
| `⌘D` / `⇧⌘D` | Split side-by-side / stacked |
| `⌘T` / `⇧⌘T` | New tab in focused tile / new group |
| `⌘W` | Close active tab (last tab closes the tile, last tile the group) |
| `⌘1`–`⌘9` | Switch group |
| `⌘]` / `⌘[` | Cycle tile focus |
| `⇧⌘]` / `⇧⌘[` | Next / previous tab in the tile |
| `⌘Q` | Quit |

Mouse: drag split dividers to resize; drag the sidebar edge to resize it;
drag a tile tab to reorder it, drop it on another tile's tab strip or center
to group it there, on a tile edge to split it out, or on a sidebar group tab
to send it to that group.

## Architecture

Two threads per terminal session, same shape as iTerm2:

```
┌─────────────────────────────┐      ┌──────────────────────────────┐
│ PTY thread (term.rs)        │      │ Main thread                  │
│                             │      │ (winit event loop)           │
│  portable-pty ↔ shell       │      │                              │
│  read pty in 64K chunks ────┼──┐   │  Wakeup ──► request_redraw   │
│  wezterm-term VT parser     │  │   │  (coalesced to vsync)        │
│  update grid                │  ├──►│                              │
│  coalesced Wakeup           │  │   │  RedrawRequested:            │
│  (skipped if one pending)   │  │   │    lock grid, snapshot cells │
│                             │  │   │    unlock                    │
│  keyboard bytes ◄───────────┼──┘   │    shape + draw via wgpu     │
└──────────────┬──────────────┘      └──────────────┬───────────────┘
               │                                    │
     Terminal grid  ◄────── Arc<Mutex> ──────────►  │
          (shared state, briefly locked per frame)
```

| Layer | Crate | Notes |
|---|---|---|
| VT emulation + grid | `wezterm-term` (git) | The exact state machine WezTerm ships: scrollback, hyperlinks, image protocols, semantic zones |
| Cell/color model | `termwiz` (same git rev) | Must match wezterm-term's version — both come from the wezterm repo |
| PTY | `portable-pty` | Spawns the login shell on a kernel pty |
| Window + event loop | `winit` | Native NSWindow on macOS |
| GPU (Metal on macOS) | `wgpu` | sRGB surface, vsync Fifo present |
| Glyph atlas + text draw | `glyphon` (cosmic-text) | Rasterize once, instanced quads per frame |

`wezterm-term` isn't published to crates.io, so it's a git dependency on the
wezterm repo; `termwiz` is pinned to the same rev so the cell types line up.

## The iTerm2 performance tricks, and where they live here

1. **GPU rendering with a glyph atlas** — glyphs are rasterized once into a GPU
   texture atlas; frames are instanced textured quads. (`renderer.rs`)
2. **PTY I/O off the main thread with output coalescing** — the reader thread
   advances the grid at I/O speed; a redraw is requested only if one isn't
   already pending (atomic flag, cleared per frame). `cat huge.txt` yields a
   handful of wakeups, not thousands. (`term.rs` — hand-built here since
   wezterm-term brings no I/O loop)
3. **Vsync-capped presentation** — `PresentMode::Fifo` + winit redraw
   coalescing ≈ iTerm2's CVDisplayLink-driven frame pacing.
4. **Short critical sections** — the grid mutex is held only to snapshot
   visible cells; GPU work happens unlocked.
5. **Gamma-correct sRGB pipeline** — sRGB surface + linearized clear color +
   glyphon `ColorMode::Accurate` (colors were washed out before this).
6. **Release profile** — fat LTO, `codegen-units = 1`; deps at `-O2` in dev.

## Why wezterm-term (over alacritty_terminal)

The end goal is an environment that runs and manages *many* terminals with
agentic workflows on top. wezterm-term's richer model buys features that matter
for that and would be painful to retrofit:

- **Image protocols** (iTerm2/kitty/sixel) — inline plots, screenshots from agents
- **OSC 8 hyperlinks** — clickable file paths and URLs
- **Semantic zones** (shell integration) — prompt/command/output regions, which
  is exactly what an agent needs to read "the output of the last command"
- One `Session` per pane is already the multi-terminal unit; `main.rs` currently
  wires up one, but nothing in `term.rs` assumes a singleton.

Cost: we maintain our own reader thread + coalescing (done), and the grid
snapshot walks wezterm's `Line`/`CellRef` API.

## Roadmap

- [x] **Multiple sessions**: workspaces ("groups") as vertical sidebar tabs,
      each with a cmux-style auto-tiling pane grid (`workspace.rs`).
- [ ] **Damage tracking**: wezterm-term stamps lines with sequence numbers
      (`line.current_seqno()`); skip reshaping rows unchanged since last frame.
      More urgent now that several panes shape text every frame.
- [ ] Background-color cell quads (vim themes, selections, reverse video).
- [ ] Scrollback scrolling, selection + copy.
- [ ] Semantic-zone API surface for agents (read last command output).
- [ ] Cursor styles, blink, IME/dead keys, bold/italic.
- [ ] Config (font, theme), native macOS menus/tabs.
