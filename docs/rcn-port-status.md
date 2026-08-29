# rcn port — status after the first stack

Companion to [`rcn-port-audit.md`](rcn-port-audit.md): what the stack of
`rcn/*` PRs actually moved off the canvas, what is still painted or resolved
by hand, and what the next steps look like. Step numbers refer to the audit's
§d port plan.

## What moved (one PR per line, in stack order)

| PR | Branch | Audit step | Surface | Now |
|----|--------|-----------|---------|-----|
| 01 | `rcn/01-audit` | — | audit + plan | docs only |
| 02 | `rcn/02-sidebar-chrome` | 1–3 | empty-state CTA + hint, page-dot strip; flyover paint gate | elements with `on_click`; canvas registers no hot rects; the empty-flyover card behind a picker (screenshot #2) no longer paints |
| 03 | `rcn/03-settings-sidebar` | 4 | Settings search box + section rows | real rcn `Input` entity (`App::settings_search`) + element click targets |
| 04 | `rcn/04-modals` | 5–6a | confirm dialog, message panel | rcn `AlertDialog` (`modal_ui`), mounted topmost; `ChromeState::element_modal` keeps the canvas inert underneath |
| 05 | `rcn/07-launch-panel` | 9 | Launch tool panel | shared `tool_panel_overlay` (`launch_ui`); the overlay occludes, so floating panels own their clicks (screenshot #1) |
| 06 | `rcn/08-save-modal` | 6b | save-as-workspace modal | rcn `Input` fields + element rows (`save_ui`), per-render focus reconcile |
| 07 | `rcn/09-tile-tabs` | 10a | tile tab strips (pixels) | clipped element strips (`tile_ui`); hit rects stay canvas |
| 08 | `rcn/10-header-chips` | — | sidebar ⇤ / ＋ chips | element click targets |
| 09 | `rcn/05-palette` | 8 | command palette | shared rcn `Input` (`App::modal_search`) + element rows (`palette_ui`) |
| 10 | `rcn/06-pickers` | 7 | directory / fork / profile pickers | `SearchModal` element trees (`picker_ui`); the canvas picker layer is deleted |
| 11 | `rcn/11-flyover-strip` | 11a | flyover tab strip (pixels) | shared `tab_strip` element (`flyover_ui`); popout window unchanged |
| 12 | `rcn/12-window-drag` | — | titlebar / folded-corner window drag | one element whose press calls `start_window_move` |
| 13 | `rcn/13-resize-handles` | 12 | sidebar edge, dividers, tool-panel edge, flyover top edge | element handles own cursor, grip and drag start (`resize_ui`) |
| 14 | `rcn/14-strip-clicks` | 10b/11b | tile tab / × presses, flyover tab / × / min / max presses | strip elements arm the same `Drag::TabPress` etc. |
| 15 | `rcn/15-sidebar-presses` | — | sidebar rows (Notes, Cleanup, pinned bubbles, section headers + delete chip, group cards) | row elements arm the same presses / drags |
| 16 | `rcn/16-chrome-presses` | — | tile collapse carets, side-strip expand, ribbon slots | invisible element press targets |

Every modal is an element tree now (`Frame::picker_quads` / `picker_labels`
and `overlay_click` are gone), and every chrome *press* except terminal
content is element-owned. The canvas `on_mouse_down` still handles: terminal
content clicks / selection / link opening / mouse-report forwarding, the
floating tool panel's blur-dismiss fallthrough, and the "empty run" of a tab
bar past its last tab. The canvas `on_mouse_move` / `on_mouse_up` still drive
every drag (tab, group, section, resize, selection) — the element handlers
only *arm* them, by design, so the drag state machine did not have to move.

## Still canvas-painted

Window gradient; tile card divider hairline, collapse caret (a rotating
chevron drawn as line segments) and side-strip hover fill; drag-and-drop
hints; terminal grids (text, cursor, selection, links, block glyphs); the
flyover card / border / divider / terminal content, and the popout window's
whole strip; ribbon slot pills and vector glyphs; the frame-stats debug
overlay. These are either the terminal itself or geometry fonts cannot carry.

## Conventions the ports settled on

- **Element clicks that must not reach the canvas** call `app.stop_propagation()`
  in their `on_mouse_down` (or `occlude()` the container when nothing under it
  should ever be hit, e.g. modal scrims and header chips). Handles whose drag
  the canvas continues (resize edges, tab presses) must *not* occlude, or the
  canvas `on_mouse_move` / `on_mouse_up` stop hearing the pointer.
- **`App::note_pointer(ev)`** records an element event's position in the
  physical-px form the canvas drag machine reads before arming a drag.
- **`ChromeState::element_modal`** (`confirm | message | save_ws | any search modal`)
  is how the renderer learns an element modal owns the window.
- **`App::modal_search`** is the one rcn `Input` every search-list modal shares;
  `modal_search_reset = Some(placeholder)` on open, consumed in `App::render`.
- **Per-render reconcile** (`sync_save_focus`, the settings/modal search blocks
  at the top of `App::render`) is where element focus meets `App` state,
  because opening happens in handlers without a `Window`.
- Local rcn additions are documented in each `src/ui/*.rs` header and in
  `src/ui/mod.rs` (`Input::set_text_size`, `AlertDialog::{scrim, top,
  on_backdrop_click}`).

## Next steps

1. **Hand-verify in the app** — nothing in this stack has been exercised by a
   human yet (each branch was smoke-launched for 10 s from a worktree; `cargo
   test` is green throughout). Highest-value checks: typing in the Settings
   search and the palette / pickers; Tab / Enter in the save modal; dragging
   every resize edge; dragging a tab between tiles; the window drag from the
   titlebar strip.
2. **Audit step 13 (host the terminal grid in an rcn container)** — the
   monolithic `build_frame` / `paint_terminal` would need to paint per tile
   (one canvas child per `Card`) for the tile cards themselves to become
   elements. Deliberately not attempted overnight.
3. **Step 14 cleanup** — `App::hot_rects` now only feeds the pointing-hand
   cursor and hover repaints; it can shrink to the terminal's needs once the
   ribbon and caret pixels move too.
4. The ribbon glyphs and the collapse caret could move to `svg()` icons (rcn's
   theme carries lucide) if a small design change is acceptable.
