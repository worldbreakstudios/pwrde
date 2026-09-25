# Overlay redesign — toasts in the sidebar, a real command-palette window, a fullscreen global terminal

Status: plan landed; section 4.1 (the sidebar toast stack) is implemented —
`src/toast_ui.rs` plus every producer/reader rewired, no `App.message` left. Scope: the plan the goal asks for **and**
the PRs that implement it. Every file:line below was read on this branch
(`tw-omura-fa2q4`, base `bbfb1e7`).

## 1. Goal (verbatim)

> i have some modal popups / overlays in pwrde like when i spawn a session or open the cmd
> pallete / etc. but the issue is that i have some embedded webviews in the app and since a
> web view is essentially a headless chrome window painted over the app layering isnt always
> possilbe and we often end up just hiding chrome when we paint the modals and overlays. can
> you help me redesign to avoid the overlays. the modals that are notifications should just be
> toast messages that go in the sidebar. maybe we have a place at the bottom of the sidebar to
> show a layered list of toasts and then let them auto expire after ~3 seconds. that way we can
> see the notification messages when they're needed, but they arent big full screen overlays
> and dont mess with webviews. i also imagine we might want some toasts that are more
> persistent when there are true notifications we want to show vs just a brief status message
> directly after i take an action. then the other class of overlay is the cmd pallete. this one
> is a bit different because i want to keep the popup panel. its really useful, but i'd love if
> instead of an overlay in the same window painted over the app it was truly a separate window
> that just is hidden or brought to the front when you hit the hotkey to trigger the cmd
> pallete. bonus points with this approach is that i'd love if we set a system wide hotkey for
> the cmd pallete so i could control and launch pwrde from anywhere on my computer and open the
> pallete even if pwrde wasnt my primary open window. this would allow me to use it similar to
> spotlight or raycast and add more functionality over time to improve my productivity flow.
> help me plan out these features and a pr to add it to pwrde

Operator follow-up (this session): *"the global terminal overlay also fits the pattern of an
overlay. you could probably remove the overlay version and make it always be in full screen
mode when you open it"*.

## 2. Why overlays are broken here

Embedded web tabs are **native child NSViews** (`wry`, `WebViewBuilder::build_as_child`,
`src/webview.rs`), z-ordered against the window's content view — not gpui elements. gpui cannot
paint over them, so every overlay path has to know about them:

- `src/webview_ui.rs:70-90` is the only place that manipulates sub-windows, and
  `src/main.rs:1145-1148` hides/suppresses chrome whenever the webview sync fails, printing the
  failure into the `message` overlay.
- The same trade-off is why the flyover panel is clipped around sub-windows
  (`src/sidebar_ui.rs:112-130`, `flyover_ceiling` at `:357`): the chrome simply stops where a
  webview starts, rather than layering.

So the fix is **not** "layer better", it is **stop putting chrome in the same window as the
webviews**: notifications move *inside the sidebar region* (which no webview ever covers) and
the two big panels become their own OS windows.

## 3. Overlay inventory (verified anchors)

| Overlay | State | Render | Notes |
|---|---|---|---|
| Confirm dialog | `App.confirm: Option<ConfirmClose>` | `modal_ui::render_confirm` (`src/modal_ui.rs:32+`) | **Stays** — a true modal decision, not a notification. |
| Message panel | `App.message: Option<(String, bool)>` (`src/main.rs:450-452`) | `modal_ui::render_message` (`:118+`) | **Converted to toasts.** `bool` = dismissable. |
| Toast note | `App.toast_note: Option<(String, Instant)>` (`src/main.rs:453-456`) | rcn `Toast` (`src/ui/toast.rs`) | Already a toast; moves from bottom-right viewport into the sidebar stack. Expiry logic + tests already exist (`src/bus_exec.rs:700-710`, test at `:1181`). |
| Command palette | `App.command: Option<CommandPalette>` | `command_ui::render_command` (`src/command_ui.rs:189`) | **Becomes a real window.** |
| Global terminal (flyover) | `flyover_open` / `flyover_windowed` | canvas paint + `flyover_ui` strip | **Always the fullscreen window** (operator follow-up). |
| Save-workspace / webview prompt | `App.save_ws`, `App.webview_prompt` | inline panels | Small; unchanged for now. |

Producers of the `message` overlay that must be re-pointed: `src/bus_exec.rs:263` (screenshot
failure), `:544` (bus `state_json` `"message"`), `src/main.rs:1147` (webview open failure),
`:5619` (`open URL failed`), plus the `drop` provisioning note (`main.rs` ~`2112`).
Readers: `modal_overlay_open` (`src/main.rs:4120-4123`), overlay gating at `:2130`, `:2538`,
`:4728`, `:2740`, the any-key-dismiss at `:4841`.

## 4. Design

Status note: §4.1–§4.3 are the design as planned before it landed; where the shipped shape
differs the paragraph carries a **Landed** note (§4.1's row struct, §4.4's reconciliation).
The code is the reference for the shipped contract.

### 4.1 Toasts live in the sidebar — two kinds

`App.toasts: Vec<ToastNote>` (keep the name `Toast` only inside `sidebar_ui`/`ui`, call the row
kind `ToastKind`) at the bottom of the sessions list, above the folders card's tail, painted
**inside the sidebar region rect** (`App::sidebar_w()`, `sidebar_ui.rs:104-130`). Because the
region is a later element sibling of the canvas and never overlaps a webview child view, this
is the one place chrome can be stacked freely.

```
// Landed (`src/toast_ui.rs`): one text line per row, no title/body split.
struct ToastNote { id: u64, kind: ToastKind, text: String, shown: Instant }
enum ToastKind { Status, Notification }
```

(The sketch this plan started from said `title`/`body`/`created`; the landed row keeps a single
`text` and an `Instant`, and a click dismisses a `Notification` only — a `Status` has no × and
expires on its own timer.)

- **`Status`** — direct feedback for an action just taken (screenshot copied, session spawned,
  webview error). Auto-expires after `TOAST_TTL = 3s` (`toast_ui::status_expired`, pinned by
  `toast_note_expires_after_lifetime` in `src/bus_exec.rs`).
- **`Notification`** — a true notification (drop provisioning failure, `open URL failed`,
  webview could not open). No timer; it stays until dismissed with its × button, and is
  capped (`MAX_TOASTS = 4`, oldest `Status` evicted first).
- Rendering: newest at the bottom (closest to where the eye lands after an action), stacked
  with a small gap; each card is a slim rcn-`Toast`-style row (icon, one line, optional ×),
  width = sidebar width − padding, mounted as the sidebar region's own element layer (an
  element sibling of the canvas, not `paint_terminal` pixels) so a native webview NSView can
  never cover it — that is the whole point of moving off the overlay.
- Clicking a `Notification` dismisses it; a `Status` has no × and expires on its own. Clicks
  pass through otherwise (mirrors `src/ui/toast.rs:139` `occlude()`).
- Bus: `state_json` (`src/bus_exec.rs:528-543`) reports `"toasts": [{kind, text, persistent}]`
  and keeps `"message"` as the newest toast's text for one release, so a CLI consumer that only
  knows `message` never sees the field go missing; note it in the PR.
- After the conversion `App.message` and `modal_ui::render_message` are deleted, and `message`
  drops out of `modal_overlay_open` — that is the actual "no full-screen overlay" win.

### 4.2 Command palette — a separate window

pwrde already does exactly this for the flyover (`open_flyover_window`, `src/main.rs:7495-7545`,
`FlyoverPopout` at `:7059`): a lazily created `cx.open_window` view that **reuses `App` as the
single source of truth** and pulls `App.window` only for native operations. The palette copies
that shape:

- `PaletteWindow` view in `src/palette.rs` (or a new `src/palette_window.rs`), holding the
  same `App` entity plus a `Renderer`-light context for text measurement.
- `App.command` stays the state; a new `App.palette_window: Option<WindowHandle<PaletteWindow>>`
  plus `palette_window_visible: bool` mirror `flyover_window*` (`:533-538`).
- Hotkey: `show → open or activate_window + focus the search input`; pressed again while the
  palette is up → `hide` (window kept alive, like the flyover's `on_window_should_close` hide
  hook). Window is `WindowBounds::Windowed`, centered, ~560×420, `titlebar` transparent so it
  reads as a Spotlight-style floating panel.
- Keyboard/focus: the palette's `Input` is owned by the window view, so typing filters there
  and the in-window palette branches (`main.rs:2538`, `:2740`, `handle_picker_key`) are
  removed for the palette case.
- Enter still routes through `App` (`submit_command` at `main.rs:2793+`): the window calls
  into the app entity, so "open session / create group" keeps working with the main window
  behind it.

### 4.3 System-wide hotkey

gpui at this pin exposes **no** global-hotkey registration (grep of the vendored crate found
none), and `global-hotkey` is not in `Cargo.lock`. Two viable routes, in preference order:

1. **`global-hotkey` crate** (tao/tauri's, macOS backend `RegisterEventHotKey`) on its own
   thread, forwarding presses over the existing `events_tx` channel as a new
   `TermEvent::GlobalHotkey(u32)` — the drain in `App::drain_events` (`src/main.rs:5477`) then
   flips `palette_window_visible` on the main thread. No new unsafe glue, rebindable later via
   `settings.rs`.
2. **Carbon directly** through the already-present `objc 0.2` / `libc` deps
   (`InstallEventHandler` + `RegisterEventHotKey`) — no new dependency, but ~100 lines of
   unsafe AppKit/Carbon glue.

Default binding: **⌥⌘P** (⌃⌘Space as the alternative if ⌥⌘P collides with a system/app
shortcut on the machine). Registered while pwrde runs, so the palette can be summoned with
pwrde in the background — that is the "spotlight / raycast" flow, and the same hotkey table is
where future global commands go.

### 4.4 Global terminal: always the fullscreen window

Per the operator follow-up, the in-window flyover paint path goes away:
`flyover_toggle` (`main.rs:2991-3008`) always opens `FlyoverPopout`, sized to the active
screen instead of `880×480`; the canvas flyover paint, `flyover_ceiling` + the sidebar clip
(`sidebar_ui.rs:112-130`, `:357`), `flyover_windowed`, and `flyover_anim` are deleted. That is
another overlay gone, and the sidebar regains the full column height.

**Landed on this branch:** `toggle_flyover` forces `flyover_windowed = true` at the single
toggle entry point and `open_flyover_window` sizes the popout from the display's
`visible_bounds()`, so the flyover is always its own fullscreen window.

**Reconciled after review:** the pump's predicate and the `FlyoverPopout` action disagreed —
the dock branch flipped `flyover_windowed` back off, which both blanked the terminal on that
press and left the in-window paint path reachable. The dock branch is gone: the action now only
shows or hides the window and never unsets `windowed` (its label reads "Flyover: show/hide
window"), and the pump asks one pure predicate, `App::flyover_window_wanted(windowed, visible)`,
which the ⌘` toggle also flips against (unit-tested, `src/main.rs`). `flyover_open` is the
retired in-window flag and now only mirrors `flyover_window_visible`.

**Deferred to a follow-up (not done):** deleting the remaining dead in-window machinery — the
canvas paint block (`main.rs:6528-6555`), the `paint_flyover_layer` call in `paint_terminal`,
`flyover_ceiling` + the sidebar clip (`sidebar_ui.rs:112-130`, `:357`), the flyover resize band
(`resize_ui.rs:229-240`), the flyover mouse hit-tests (`main.rs:4018`, `:4095`, `:4198-4217`,
`:4504-4512`, `:4565`), the `flyover_anim` tick (`main.rs:5705-5712`), and `flyover_maximized`
(`main.rs:2340-2344`). Each is unreachable while `flyover_windowed` stays true, but it is still
code that reads like an overlay path, so the deletion is worth a scoped commit of its own
rather than being folded into the shipping change.

## 5. PR plan

One branch (`tw-omura-fa2q4`), four scoped commits, each independently reviewable, then a
draft PR against `main`:

1. Toast stack in the sidebar + `App.message` removal (webviews untouched, no new deps).
2. Palette as its own window (hotkey-bound via the existing in-app binding first, so it is
   usable and testable before any global hook exists).
3. Global hotkey registration (new dep or Carbon glue) + `TermEvent::GlobalHotkey`.
4. Flyover always-windowed + deletion of the canvas flyover path.

Verification per commit: `cargo build` + `cargo test` (the project's suite, ~445 tests), plus
the existing `toast_note_*` tests re-pointed at the new stack. The webview-specific claim
("no chrome paints over a webview") is checked by construction — toasts render inside the
sidebar region rect — and by hand: spawn a session with a webview tab and fire a screenshot
status toast.

Risks: (a) sub-window z-order — a palette window must be activated explicitly
(`window.activate_window()` + `focus`, as the flyover does) or it can open behind Slack;
(b) the global hotkey needs Accessibility/Input Monitoring permission on first use (the same
permission the input helper in repo memory already documents); (c) `message` disappearing from
`state_json` is an API change for CLI consumers — keep `message` (the newest toast's text) for one release.
