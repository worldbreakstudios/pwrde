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
pwrde-cli state                                  # JSON: page, groups → tiles → tabs (title/active/unread/cols/rows), sections, palette open, status message
pwrde-cli commands                               # every bus command + every rebindable Action with its current key binding
pwrde-cli new-session ~/src/pwrde                # open a group at a dir; --layout <profile> applies a .pwrspace profile; --base <ref|default> forks a worktree via drop
pwrde-cli send-text 'cargo test' --enter         # raw keystrokes into the focused pane; --enter appends \r; --group <name|index>; text `-` reads stdin
pwrde-cli send-text $'\x03'                      # control bytes pass through (^C, escape sequences)
pwrde-cli action split_right                     # any Action by name (split_right, new_tab, close_tab, focus_left, toggle_sidebar, screenshot_to_file, …)
pwrde-cli page settings                          # sessions | pull_requests | cleanup | notes | settings
pwrde-cli focus pwrde                            # by group name, sidebar title, or 0-based index
pwrde-cli new-section Work && pwrde-cli move pwrde Work
pwrde-cli resize 1100 700                        # window content size in points
pwrde-cli screenshot /tmp/app.png                # PNG of the app window (no Screen Recording prompt); no path = temp file printed on stdout; --clipboard
pwrde-cli raw '{"cmd":"send_text","text":"ls\r","group":"pwrde"}'   # anything the protocol accepts
```

`send-text` is raw input, not a bracketed paste: multi-line text runs line by
line in a shell. Actions that don't apply (e.g. `split_right` on the Settings
page, or with no session open) return exit `1` with a reason rather than a
silent no-op — treat that as a real signal.

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
