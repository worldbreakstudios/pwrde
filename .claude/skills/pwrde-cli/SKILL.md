---
name: pwrde-cli
description: Drive and inspect the running pwrde app over its command bus with `pwrde-cli` — open sessions, type into panes, run palette actions, navigate pages, dump state as JSON and take screenshots — to verify a change against the real app. Use when the user runs /pwrde-cli, asks to "test this in the app", "check it in the running app", "take a screenshot of the app", or when a navis/verify step needs an observable behavior confirmed. $ARGUMENTS names what to verify (a feature, page, or command).
---

# pwrde-cli — drive the app for verification

pwrde exposes everything the ⌘P palette can do over a Unix-socket command bus
(`src/bus.rs`, dispatched by `src/bus_exec.rs`). `pwrde-cli` is the client, so
you can exercise and inspect the real app without a browser or a web port.
Use it for every manual-verification step instead of asking the user to click.

Goal for this run: `$ARGUMENTS` (if empty, ask what behavior to verify).

## 1. Pick the app instance

Socket resolution order (override with `--socket <path>`):

1. `$PWRDE_SOCKET` — exported into every pane's shell, so from inside a pwrde
   tab the CLI always targets *that* app.
2. `~/.pwrde/bus.sock` — the installed `/Applications/Pwrde.app`.
3. The most recent `~/.pwrde/worktrees/<slug>/bus.sock` — an app launched from
   a linked git worktree (scoped like its settings/DB).

`pwrde-cli ping` first. "Connection refused" on a worktree socket means that
build was killed and left a stale socket — relaunch it or pass the right
`--socket`. Exit codes: `0` ok · `1` the app refused the command (reason on
stderr) · `2` could not connect · `64` usage error.

## 2. Verifying a code change: use a dev build

The installed app does not have your uncommitted change. From the worktree:

```sh
cargo build --bins
nohup target/debug/pwrde >/tmp/pwrde-dev.log 2>&1 &     # background, never foreground
sleep 3 && pwrde-cli ping                                # resolves the worktree socket
```

The CLI on PATH (installed by `scripts/deploy.sh`) speaks the same protocol as
the dev build; use `target/debug/pwrde-cli` only if you changed `src/bus.rs`.
When finished: `pkill -f target/debug/pwrde`.

## 3. Commands

```sh
pwrde-cli state                                  # JSON: page, groups → tiles → tabs (title/active/unread/cols/rows), sections, sidebar {collapsed, folders_open, folder, sessions_w, region_w}, dashboard {open, filter, panes, unread, cols, rows, above, unread_above, below, unread_below, cards[group/title/status/focused/visible/cols/rows/side_tabs/side_unread]}, palette open, status message
pwrde-cli read panes [query]                     # read-only: one line per pane in every group (session id, group, title, foreground; focused *), filtered by substring; --json for rows
pwrde-cli read pane <id> [--lines N|--all] [--json]   # read-only: a pane's text (screen, or scrollback tail/all) by session id; --json adds title/size/cursor/group/foreground. Never moves focus/scroll
pwrde-cli commands                               # every bus command + every rebindable Action with its current key binding
pwrde-cli new-session ~/src/pwrde                # open a group at a dir; --layout <profile> applies a .pwrspace profile; --base <ref|default> forks a worktree via drop
pwrde-cli new-webview-command 'echo google.com'  # webview tab whose URL is the first line the command prints (run via $SHELL -lc); --group <name|index>
pwrde-cli send-text 'cargo test' --enter         # raw keystrokes into the focused pane; --enter appends \r; --group <name|index>; text `-` reads stdin
pwrde-cli send-text $'\x03'                      # control bytes pass through (^C, escape sequences)
pwrde-cli key cmd-p escape                       # press chords through the app's key handler (tests bindings/overlays; gpui syntax: cmd-shift-t, ctrl-c, enter)
pwrde-cli action split_right                     # any Action by name (split_right, new_tab, close_tab, focus_left, toggle_sidebar, screenshot_to_file, …)
pwrde-cli page settings                          # sessions | dashboard | tool:<n>; `settings` opens the Settings window, `settings:keyboard` jumps to a section
pwrde-cli dashboard-filter unread                # all | unread; exit 1 unless the dashboard is open (`page dashboard` or `action toggle_dashboard`)
pwrde-cli focus pwrde                            # by group name, sidebar title, or 0-based index
pwrde-cli new-section Work && pwrde-cli move pwrde Work
pwrde-cli resize 1100 700                        # window content size in points
pwrde-cli screenshot /tmp/app.png                # PNG of the app window (no Screen Recording prompt); --window settings captures the Settings window, --window popover the open webview Site/Tools popover, --window palette the open ⌘P palette; no path = temp file printed on stdout; --clipboard
pwrde-cli raw '{"cmd":"send_text","text":"ls\r","group":"pwrde"}'   # anything the protocol accepts
```

`key` enters at the app's key handler (bindings, palette/overlay and flyover
routing, PTY typing), not gpui's focus tree — a focused child view such as a
Settings Input or the PR composer won't receive it; use `send-text` for text.
`send-text` is raw input, not a bracketed paste: multi-line text runs line by
line in a shell. Actions that don't apply (e.g. `split_right` on the Settings
page, or with no session open) return exit `1` with a reason rather than a
silent no-op — treat that as a real signal. On the dashboard `key` types into
the focused card's primary pane, `prev_page` / `next_page` / `prev_sidebar_tab` /
`next_sidebar_tab` (⌘⇧←/→/↑/↓) move the focused card by grid position, wrapping
within its row or column, `focus_*` / `next_tile` / `prev_tile` move the
focused card, `new_group` opens the session picker, `close_tab` / `close_group`
(⌘W / ⇧⌘W) both open the close-session confirm for the focused card (Enter
closes that group; exit `1` with no card focused), and the other group and
tile actions (`split_right`, `new_tab`, `toggle_pin`, …) are refused the same way, while `open_pr_in_github` opens the focused card's pull request in the browser (exit `1` with no card focused or no PR); `focus <group>`
leaves the dashboard for the Sessions page. Each card's `status` in `state` is
`read` or `unread` — the primary tab's unread flag: an attention signal (OSC 9)
turns any card `unread`, the focused one included, and only a left click on
the card or on its sessions-list row (neither drivable over the bus), a
keyboard focus move (`focus_*`, `next_tile` / `prev_tile`, the ⌘⇧+arrow
actions, ⌘1–9) that then rests on the card for 1s, or going to the session
reads it. `send-text` is not keyboard input: on the dashboard it
still writes to the target group's focused pane, as on every page, which may
be a pane no card shows.

The dashboard shows only the folder selected in the folders card
(`state.sidebar.folder`; `null` = All sessions): `dashboard.panes` / `unread`
count that folder's sessions, the grid (`cols` × `rows`) and every card's PTY
size come from that count, and `dashboard.cards` lists just the cards showing —
the folder's sessions the All / Unread filter keeps, plus the focused card
(which `unread` keeps even once read), in slot order. `below` /
`unread_below` are the "↓ N more · M unread" hint's counts, `above` /
`unread_above` those of its "↑ N more" mirror at the top. Groups outside the
folder have no card and keep their PTY size; if the active group is one of
them no card is `focused` and `key` types into nothing until a focus action
(`focus_right`, `next_tile`, …) lands on the first card. There is no bus
command to pick a folder: `move <group> <section>` changes a folder's members,
and `"sidebar.folder": "<section id>"` in the settings file selects one at
launch.

## 4. The verification loop

1. Reach the state under test with `page` / `action` / `new-session` /
   `send-text`. Give the PTY a moment (`sleep 1`) before inspecting output.
2. `pwrde-cli screenshot /tmp/<name>.png`, then **Read the PNG** and describe
   what you see once — do not re-read the same screenshot.
3. `pwrde-cli state` and assert structure: group/section membership, tab
   titles, which tile is focused, the current page.
4. Repeat for the negative path (the action on the wrong page, a bad name).
5. Report: what you drove, what the screenshot showed, what `state`
   confirmed, and anything you could not verify. Kill the dev app.

## Adding a command

New palette-visible, parameterless commands are new `Action` variants in
`src/pages.rs` (name/label/default binding) plus `action_group`/`action_glyph`
in `src/command.rs` — they become bus-callable and rebindable for free.
Parameterized commands are `Command` variants in `src/bus.rs` (keep that file
free of `crate::` imports; the CLI includes it via `#[path]`), a `command_specs`
row, a `parse_command` arm in `src/bin/pwrde-cli.rs`, and an `execute` arm in
`src/bus_exec.rs`. `cargo test bus` covers the protocol round-trips.
