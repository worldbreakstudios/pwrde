//! Terminal session: PTY + VT emulation, built on wezterm's core crates.
//!
//! - `portable-pty` spawns the shell on a kernel PTY.
//! - `wezterm-term` is the VT state machine (same one WezTerm ships): grid,
//!   scrollback, escape handling, hyperlinks, image protocols.
//! - Unlike alacritty_terminal, wezterm-term brings no I/O event loop, so the
//!   reader thread + output coalescing (iTerm2 trick #2) live here: PTY output
//!   advances the grid at I/O speed, but a redraw is only *requested* if one
//!   isn't already pending — `cat huge.txt` produces a handful of wakeups, not
//!   thousands.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use wezterm_term::color::ColorPalette;
use wezterm_term::{
    Alert, AlertHandler, KeyModifiers, MouseButton, MouseEvent, MouseEventKind, StableRowIndex,
    Terminal, TerminalConfiguration, TerminalSize, VisibleRowIndex,
};
/// wezterm-term's stock window title, i.e. "this pane has never set one".
/// Panes showing this are the ones that get a process-derived name instead.
pub const STOCK_TITLE: &str = "wezterm";

/// User events forwarded to the gpui app over an mpsc channel.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// Grid changed; a redraw is needed (coalesced). Tagged with the session id.
    Wakeup(u64),
    /// Shell exited. Tagged with the session id.
    Exit(u64),
    /// A background `ps` sweep came back. `titles` is `(session id, short
    /// name)` for the panes that resolved; `asked` is every id the sweep looked
    /// at, so the ones that resolved to nothing can be counted as misses and
    /// backed off instead of re-swept every tick.
    ProcTitlesReady { asked: Vec<u64>, titles: Vec<(u64, String)> },
    /// The program requested attention (OSC 9 toast notification). Tagged with the session id.
    Attention(u64),
    /// A `drop` worktree finished provisioning: create its group at `cwd`.
    GroupReady { name: String, cwd: std::path::PathBuf },
    /// A directory arrived from outside the app (Finder "Open With", the
    /// `open -a` CLI, a `pwrde://` deep link, or an argv path): open it as a
    /// new group.
    OpenDir { cwd: std::path::PathBuf },
    /// `drop` failed; show `message` in the picker overlay.
    GroupFailed { message: String },
    /// A Wry top-level navigation committed; folded into the owning tab on
    /// the main thread so its address, title, and persisted URL stay current.
    WebviewNavigated { id: u64, url: String },
    /// Pointer focus entered a native child view; keep the owning tile as the
    /// workspace focus target for tab and address-bar actions.
    WebviewFocused { id: u64 },
    /// The branch-scoped PR list (`--head <current branch>`) finished loading
    /// (or failed).
    PrListLoaded { result: Result<Vec<crate::gh::PrSummary>, String> },
    /// A PR's detail finished loading, tagged with the PR number so a stale
    /// result for a PR the user already navigated away from can be dropped.
    PrDetailLoaded { number: u32, result: Result<crate::gh::PrDetail, String> },
    /// A PR's diff finished loading (parsed + syntax-highlighted off-thread),
    /// tagged with the PR number.
    PrDiffLoaded {
        number: u32,
        result: Result<std::sync::Arc<crate::pr_ui::DiffRender>, String>,
    },
    /// A PR write action (approve/comment/merge/ready) completed.
    PrActionDone(Result<String, String>),
    /// The local diff tool finished gathering + highlighting a diff.
    LocalDiffLoaded(Result<crate::pr_ui::LocalDiffRender, String>),
    /// `lfg` reported a cache entry refreshed; the open PR view should re-fetch
    /// to pick up the fresh data. `number` is the PR number when the event is
    /// PR-scoped.
    PrCacheUpdated { kind: String, number: Option<u32> },
    /// A background `git_context::fetch` finished for `cwd`. The blocking git
    /// and `gh` calls must never run on the main thread, so the aggregate
    /// comes back here and is folded into `App::git_contexts`.
    GitContextReady {
        cwd: std::path::PathBuf,
        ctx: crate::git_context::GitContext,
    },
    /// A bare "please repaint" nudge from a background job whose result is read
    /// from a shared cache rather than carried in the event (e.g. a finished
    /// mermaid render). Carries no state — just marks the frame dirty.
    Redraw,
    /// A request arrived on the command bus socket; the drain loop executes it on
    /// the main thread and sends exactly one Reply back.
    Bus {
        cmd: crate::bus::Command,
        reply: std::sync::mpsc::Sender<crate::bus::Reply>,
    },
    /// Flow agent progress (assistant text, tool cards, turn lifecycle) from
    /// one of the per-chat agent backends; see `flow.rs`. Arrives on the shared event
    /// channel so the backend's reader thread never touches the foreground.
    Flow { chat: u64, ev: crate::flow::FlowEvent },
}

/// Forwards `Alert::ToastNotification` from wezterm-term to the UI event channel
/// as a `TermEvent::Attention`. All other alert variants are ignored.
struct AttentionHandler {
    id: u64,
    sender: Sender<TermEvent>,
}

impl AlertHandler for AttentionHandler {
    fn alert(&mut self, alert: Alert) {
        if let Alert::ToastNotification { .. } = alert {
            let _ = self.sender.send(TermEvent::Attention(self.id));
        }
    }
}

#[derive(Debug)]
struct TermConfig;

impl TerminalConfiguration for TermConfig {
    /// The palette apps see through OSC color queries (10/11/4…). Resolved
    /// from the live settings each call so wezterm-term's lazy palette always
    /// reports the scheme currently on screen, not the one at spawn time.
    fn color_palette(&self) -> ColorPalette {
        crate::term_theme::palette(crate::theme::current())
    }
}

/// The PTY input handle, shared between user keystrokes and the terminal's
/// own answerback responses (cursor position reports, device attributes, …).
#[derive(Clone)]
struct PtyWriter(Arc<Mutex<Box<dyn Write + Send>>>);

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

/// Which lifecycle phase a forwarded mouse event is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MousePhase {
    /// A button went down.
    Press,
    /// The pointer moved with a button held (a drag).
    Move,
    /// A button came up.
    Release,
}

/// Which button a forwarded mouse event is for. The wheel keeps its own path
/// ([`Session::forward_wheel`]); this covers the three physical buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
}

/// Build the wezterm-term [`MouseEvent`] for a forwarded button report. Pure
/// (no terminal lock, no I/O) so the button/phase/modifier mapping is unit
/// testable; [`Session::forward_mouse`] hands the result to `mouse_event`.
fn mouse_report_event(
    phase: MousePhase,
    button: MouseBtn,
    col: usize,
    row: usize,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> MouseEvent {
    let kind = match phase {
        MousePhase::Press => MouseEventKind::Press,
        MousePhase::Move => MouseEventKind::Move,
        MousePhase::Release => MouseEventKind::Release,
    };
    let button = match button {
        MouseBtn::Left => MouseButton::Left,
        MouseBtn::Middle => MouseButton::Middle,
        MouseBtn::Right => MouseButton::Right,
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::SHIFT, shift);
    modifiers.set(KeyModifiers::ALT, alt);
    modifiers.set(KeyModifiers::CTRL, ctrl);
    MouseEvent {
        kind,
        x: col,
        y: row as VisibleRowIndex,
        x_pixel_offset: 0,
        y_pixel_offset: 0,
        button,
        modifiers,
    }
}

/// A text selection, anchored in scrollback-*stable* row coordinates so it
/// stays pinned to its content as the viewport scrolls. `anchor` is where the
/// drag began; `head` is the cell under the cursor now. Both cols are cell
/// indices; the cell under `head` is included.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sel {
    anchor: (usize, StableRowIndex),
    head: (usize, StableRowIndex),
}

impl Sel {
    /// Reading-order (start, end): the earlier point first, by (row, col).
    fn ordered(self) -> ((usize, StableRowIndex), (usize, StableRowIndex)) {
        let a = (self.anchor.1, self.anchor.0);
        let h = (self.head.1, self.head.0);
        if a <= h { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }
}

/// Locate the shpool binary in the usual install locations. GUI apps don't
/// inherit a login-shell PATH, so probe the well-known dirs directly.
pub fn shpool_binary() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cargo/bin"));
    }
    candidates.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(Into::into));
    candidates.into_iter().map(|d| d.join("shpool")).find(|p| p.exists())
}

/// Contents of the pwrde-managed shpool config (see [`shpool_quiet_config`]).
///
/// `prompt_prefix = ""` silences the `shpool:$SHPOOL_SESSION_NAME` line the
/// daemon otherwise stamps above every prompt. `forward_env` matters because
/// the daemon spawns the session's shell with a scrubbed environment (only
/// `TERM`, `DISPLAY`, `LANG`, `SSH_AUTH_SOCK` and the `forward_env` list cross
/// over from the attach client) — without it the `COLORTERM`/`PWRDE`/
/// `PWRDE_SOCKET` vars set on `shpool attach` in [`Session::new`] never reach
/// the persisted shell, so apps inside it fall back to 256 colors and
/// `pwrde-cli` can't find the bus socket. The list is read by the attach
/// *client* on every attach, so it takes effect for newly created sessions
/// without a daemon restart (reattached sessions keep the env they started
/// with).
const SHPOOL_CONFIG: &str =
    "prompt_prefix = \"\"\nforward_env = [\"COLORTERM\", \"PWRDE\", \"PWRDE_SOCKET\"]\n";

/// Exact contents pwrde wrote to `shpool.toml` in earlier versions. A file
/// matching one of these was written by pwrde, not hand-edited, and is safe to
/// upgrade in place to [`SHPOOL_CONFIG`].
const SHPOOL_CONFIG_STALE: &[&str] = &["prompt_prefix = \"\"\n"];

/// Config file to pass to `shpool attach` so the prompt prefix is silenced and
/// the truecolor/bus env vars are forwarded into the session shell (see
/// [`SHPOOL_CONFIG`]).
///
/// Defers to the user: if they keep their own shpool config (either the
/// macOS `~/Library/Application Support/shpool/config.toml` or the XDG
/// `~/.config/shpool/config.toml`), returns None so shpool loads it normally.
/// Otherwise lazily writes `~/.pwrde/shpool.toml` and returns its path,
/// upgrading a file left by an older pwrde but never touching a hand-edited
/// one. Any failure returns None — persistence must keep working even if the
/// config can't be written.
pub fn shpool_quiet_config() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let user_configs = [
        home.join("Library/Application Support/shpool/config.toml"),
        home.join(".config/shpool/config.toml"),
    ];
    shpool_quiet_config_in(&user_configs, &crate::settings::config_dir())
}

/// Path logic for [`shpool_quiet_config`] over explicit paths so tests can use
/// temp dirs.
pub fn shpool_quiet_config_in(
    user_configs: &[std::path::PathBuf],
    pwrde_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if user_configs.iter().any(|p| p.exists()) {
        return None;
    }
    let path = pwrde_dir.join("shpool.toml");
    match std::fs::read_to_string(&path) {
        Err(_) => {
            std::fs::create_dir_all(pwrde_dir).ok()?;
            std::fs::write(&path, SHPOOL_CONFIG).ok()?;
        }
        Ok(current) if SHPOOL_CONFIG_STALE.contains(&current.as_str()) => {
            std::fs::write(&path, SHPOOL_CONFIG).ok()?;
        }
        Ok(_) => {} // hand-edited: leave it alone
    }
    Some(path)
}

/// Fire-and-forget `shpool kill <name>`, used when a persisted tab is closed
/// explicitly so the daemon doesn't accumulate orphaned sessions.
pub fn shpool_kill(name: &str) {
    if let Some(bin) = shpool_binary() {
        let _ = std::process::Command::new(bin)
            .args(["kill", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Walk a process table (pid, ppid, command) and return the deepest first-child
/// descendant of `root_pid`. If `root_pid` has no children in the table,
/// returns `None`. Pure function — no shelling out — so it is unit-testable.
///
/// "First child" means the child with the lowest pid among the direct children
/// of each node, chosen greedily at each level (depth-first, first-child chain).
pub fn deepest_descendant(
    table: &[(u32, u32, String)],
    root_pid: u32,
) -> Option<String> {
    command_chain(table, root_pid).into_iter().skip(1).next_back()
}

/// The first-child chain from `root_pid` as commands, the root's own first (when
/// the table knows it) and the deepest last. Pure, for tests.
pub fn command_chain(table: &[(u32, u32, String)], root_pid: u32) -> Vec<String> {
    // Build parent → children map (sorted by pid for determinism).
    let mut children: std::collections::HashMap<u32, Vec<(u32, &str)>> =
        std::collections::HashMap::new();
    for (pid, ppid, cmd) in table {
        children.entry(*ppid).or_default().push((*pid, cmd.as_str()));
    }
    // Sort each child list by pid so the walk is deterministic.
    for v in children.values_mut() {
        v.sort_by_key(|(pid, _)| *pid);
    }

    let mut chain = Vec::new();
    if let Some(root) = command_for_pid(table, root_pid) {
        chain.push(root);
    }
    let mut current = root_pid;
    while let Some(&(child_pid, child_cmd)) = children.get(&current).and_then(|v| v.first()) {
        chain.push(child_cmd.to_owned());
        current = child_pid;
    }
    chain
}

/// The deepest command in `root_pid`'s first-child chain that yields a usable
/// tab name, walking back up when it doesn't — a zombie caught mid-reap at the
/// tip must not cost the pane its name — and naming the shell itself when it has
/// no children at all (an idle pane). Pure, for tests.
pub fn deepest_name(table: &[(u32, u32, String)], root_pid: u32) -> Option<String> {
    command_chain(table, root_pid).iter().rev().find_map(|cmd| title_from_command(cmd))
}

/// The command field of a single row, for the case where a shell has no
/// children at all — an idle pane. [`deepest_descendant`] only ever returns a
/// *descendant*, so without this an idle shell resolves to nothing and the
/// pane keeps asking to be named. Pure, for tests.
pub fn command_for_pid(table: &[(u32, u32, String)], pid: u32) -> Option<String> {
    table.iter().find(|(p, _, _)| *p == pid).map(|(_, _, cmd)| cmd.clone())
}

/// Run `ps` with `args` (which must select `pid=,ppid=,command=` columns) and
/// parse each line into `(pid, ppid, command)`. `split_whitespace` tolerates
/// ps's right-aligned column padding; the command keeps its remaining tokens
/// joined by single spaces. Returns `None` on any `ps` failure.
fn ps_table(args: &[&str]) -> Option<Vec<(u32, u32, String)>> {
    let output = std::process::Command::new("ps").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let pid: u32 = parts.next()?.parse().ok()?;
                let ppid: u32 = parts.next()?.parse().ok()?;
                let cmd = parts.collect::<Vec<_>>().join(" ");
                if cmd.is_empty() { None } else { Some((pid, ppid, cmd)) }
            })
            .collect(),
    )
}

/// Reduce a command line to the short name a tab shows: the program itself,
/// with a login shell's leading `-` and any directory prefix stripped
/// (`/usr/bin/vim file.txt` → `vim`, `-zsh` → `zsh`). Plain shell names are
/// kept deliberately — "zsh" beats the emulator's stock "wezterm". Pure, for
/// tests.
pub fn title_from_command(cmd: &str) -> Option<String> {
    let token = cmd.split_whitespace().next()?;
    // `ps` renders a process it can no longer read as `(name)`, and a zombie's
    // command as `<defunct>`. Catching one mid-walk is easy (the walk takes the
    // *deepest* descendant, and short-lived children come and go), and either
    // is a worse tab name than keeping the last one — both were seen live while
    // verifying this against real shpool sessions.
    if token.starts_with('(') || token.starts_with('<') {
        return None;
    }
    let token = token.strip_prefix('-').unwrap_or(token);
    let name = token.rsplit('/').next().unwrap_or(token);
    if name.is_empty() { None } else { Some(name.to_owned()) }
}

/// Process-derived titles for a whole batch of sessions: each spec is
/// `(session id, shpool session name, direct child pid)`, walked the same way
/// [`foreground_command`] and [`shpool_foreground_command`] walk one — with one
/// deliberate difference: an idle pane, whose shell has no descendant at all,
/// resolves to the shell itself here rather than to `None`, because a tab still
/// needs a name, and an unusable name at the tip of the walk (a zombie caught
/// mid-reap) falls back up the chain instead of dropping the pane.
///
/// The point of the batch is the process table: one `ps` sweep (plus the
/// environment-tagged sweep, and only when some spec is shpool-backed) serves
/// every session, instead of a pair per pane. Any failure yields an empty
/// `Vec` rather than a panic, and callers keep whatever title they had.
pub fn foreground_titles(specs: &[(u64, Option<String>, Option<u32>)]) -> Vec<(u64, String)> {
    if specs.is_empty() {
        return Vec::new();
    }
    let Some(clean) = ps_table(&["-axo", "pid=,ppid=,command="]) else {
        return Vec::new();
    };
    // `ps -axE` dumps every process's whole environment, so skip it unless
    // some pane in this batch is shpool-backed and actually needs it.
    let env_table = if specs.iter().any(|(_, name, _)| name.is_some()) {
        ps_table(&["-axE", "-o", "pid=,ppid=,command="])
    } else {
        None
    };
    titles_from_tables(specs, &clean, env_table.as_deref())
}

/// The resolution half of [`foreground_titles`], over process tables already
/// gathered. Specs that don't resolve — no pid, no matching shpool subtree, no
/// descendant, no usable name — are simply absent from the result, so a failed
/// `ps -axE` costs only the shpool-backed specs and never the rest. Pure, for
/// tests.
pub fn titles_from_tables(
    specs: &[(u64, Option<String>, Option<u32>)],
    clean: &[(u32, u32, String)],
    env: Option<&[(u32, u32, String)]>,
) -> Vec<(u64, String)> {
    specs
        .iter()
        .filter_map(|(id, shpool_name, child_pid)| {
            let root = match shpool_name {
                Some(name) => {
                    let needle = format!("SHPOOL_SESSION_NAME={name}");
                    env_subtree_root(env?, &needle)?
                },
                None => (*child_pid)?,
            };
            // The chain includes the subtree root, which on the shpool arm is
            // meant to be the daemon-side shell. Should the env tag ever land on
            // the client instead, "shpool" is a worse tab name than no name at
            // all — leave the pane to the next sweep.
            let name = deepest_name(clean, root).filter(|name| name != "shpool")?;
            Some((*id, name))
        })
        .collect()
}

/// Return the command line of the foreground process running inside the shell
/// identified by `shell_pid`. Shells out to `ps -axo pid=,ppid=,command=`,
/// builds a parent→children map, and returns the deepest first-child descendant
/// of `shell_pid`. Returns `None` if the shell has no descendants or on any
/// `ps` failure. Best-effort: never panics.
pub fn foreground_command(shell_pid: u32) -> Option<String> {
    let table = ps_table(&["-axo", "pid=,ppid=,command="])?;
    deepest_descendant(&table, shell_pid)
}

/// The root pid of the env-tagged subtree: among rows whose command field
/// contains `needle`, the one whose parent does *not* — i.e. the process the
/// environment variable was first set on. Pure, for tests.
pub fn env_subtree_root(table: &[(u32, u32, String)], needle: &str) -> Option<u32> {
    let matching: std::collections::HashSet<u32> =
        table.iter().filter(|(_, _, cmd)| cmd.contains(needle)).map(|(pid, _, _)| *pid).collect();
    table
        .iter()
        .find(|(pid, ppid, _)| matching.contains(pid) && !matching.contains(ppid))
        .map(|(pid, _, _)| *pid)
}

/// Best-effort foreground command for a shpool-backed pane. The pane's own
/// child is just `shpool attach`; the real shell lives under the daemon, so
/// find it by its `SHPOOL_SESSION_NAME=<name>` environment (`ps -E` appends
/// the environment to the command field for same-user processes), then walk
/// its descendants in a clean (env-free) table. Returns `None` whenever any
/// step fails — callers save the tab as a bare shell.
pub fn shpool_foreground_command(session_name: &str) -> Option<String> {
    let env_table = ps_table(&["-axE", "-o", "pid=,ppid=,command="])?;
    let needle = format!("SHPOOL_SESSION_NAME={session_name}");
    let root = env_subtree_root(&env_table, &needle)?;
    let clean = ps_table(&["-axo", "pid=,ppid=,command="])?;
    deepest_descendant(&clean, root)
}

pub struct Session {
    pub id: u64,
    pub term: Arc<Mutex<Terminal>>,
    writer: PtyWriter,
    master: Box<dyn MasterPty + Send>,
    redraw_pending: Arc<AtomicBool>,
    /// Lines scrolled up from the live bottom (0 = following new output).
    scroll_offset: AtomicUsize,
    /// Active mouse selection, if any (in stable-row coordinates).
    selection: Mutex<Option<Sel>>,
    /// Consecutive sweeps that failed to resolve a name for this pane. A pane
    /// that can never resolve — a detached shpool session whose shell is gone —
    /// would otherwise sit in the fast sweep forever, so past a few misses it
    /// drops back to the slow renew tick.
    proc_title_misses: AtomicUsize,
    /// Foreground-process name from the throttled `ps` sweep, shown while the
    /// emulator has no title of its own. Restoring a persisted shpool pane
    /// gives it a fresh grid whose title is the stock "wezterm" until the
    /// program inside happens to emit OSC 0/2 — this is what fills that gap.
    proc_title: Mutex<Option<String>>,
    /// The shpool session name if this session is backed by shpool.
    pub shpool_session: Option<String>,
    /// PID of the direct child process (shell or shpool client). None for
    /// placeholder sessions and when spawn fails. Used by `foreground_command`
    /// to find the deepest foreground descendant.
    pub child_pid: Option<u32>,
}

impl Session {
    /// Spawn the user's shell on a fresh PTY. `cwd` sets the shell's working
    /// directory; `None` inherits pwrde's own working directory.
    ///
    /// When `command` is `Some(cmd)`, the login shell is spawned with
    /// `["-lc", cmd]` instead of an interactive default program. Tool sessions
    /// are never persisted: a non-`None` `command` takes precedence over
    /// `shpool_session` (shpool is ignored in that case).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        dpi: u32,
        cwd: Option<&std::path::Path>,
        command: Option<&str>,
        events: Sender<TermEvent>,
        shpool_session: Option<String>,
    ) -> Self {
        let pty_size = PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: cols as u16 * cell_width,
            pixel_height: rows as u16 * cell_height,
        };
        let pair = native_pty_system().openpty(pty_size).expect("openpty");

        // Tool sessions are never persisted — a command overrides shpool.
        let shpool_name = if command.is_some() {
            None
        } else {
            shpool_session.clone()
        };

        let (cmd_opt, spawn_error) = if let Some(ref name) = shpool_name {
            if let Some(shpool_path) = shpool_binary() {
                let mut cmd = CommandBuilder::new(shpool_path);
                // Global flag, so it must precede the subcommand (after
                // `attach`, `-c` means `--cmd`). The attach client reads
                // `forward_env` from it on every attach; `prompt_prefix` only
                // matters when this attach auto-starts the daemon — an
                // already-running daemon keeps the config it was launched with.
                if let Some(cfg) = shpool_quiet_config() {
                    cmd.arg("--config-file");
                    cmd.arg(cfg);
                }
                cmd.arg("attach");
                // The daemon spawns the session's shell, so the client's cwd
                // doesn't reach it — pass the start dir explicitly (only used
                // when the session is first created; ignored on reattach).
                // Only honor a cwd that still exists — a pinned/recent dir may
                // have been deleted since it was saved.
                if let Some(dir) = cwd.filter(|d| d.is_dir()) {
                    cmd.arg("--dir");
                    cmd.arg(dir);
                    cmd.cwd(dir);
                }
                cmd.arg(name);
                cmd.env("TERM", "xterm-256color");
                // Advertise 24-bit color: wezterm-term parses truecolor SGR and
                // the renderer paints full RGB per cell, so apps should emit it.
                // These land on the attach *client*; they only reach the
                // daemon-spawned session shell because the pwrde shpool config
                // lists them in `forward_env` (see SHPOOL_CONFIG).
                cmd.env("COLORTERM", "truecolor");
                cmd.env("PWRDE", "1");
                if let Ok(sock) = std::env::var("PWRDE_SOCKET") {
                    cmd.env("PWRDE_SOCKET", sock);
                }
                (Some(cmd), None)
            } else {
                // Fail the pane loudly rather than silently losing persistence.
                (None, Some("shpool not found — install it (brew install shell-pool/shpool/shpool) or disable Persist sessions\r\n"))
            }
        } else if let Some(run) = command {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut cmd = CommandBuilder::new(shell);
            cmd.arg("-lc");
            cmd.arg(run);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("PWRDE", "1");
            // Only honor a cwd that still exists — a pinned/recent dir may have
            // been deleted since it was saved, and spawning a shell in a missing
            // directory would fail. Fall back to inheriting our own cwd.
            if let Some(dir) = cwd.filter(|d| d.is_dir()) {
                cmd.cwd(dir);
            }
            (Some(cmd), None)
        } else {
            let mut cmd = CommandBuilder::new_default_prog(); // user's shell
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("PWRDE", "1");
            if let Ok(sock) = std::env::var("PWRDE_SOCKET") {
                cmd.env("PWRDE_SOCKET", sock);
            }
            // Only honor a cwd that still exists — a pinned/recent dir may have
            // been deleted since it was saved, and spawning a shell in a missing
            // directory would fail. Fall back to inheriting our own cwd.
            if let Some(dir) = cwd.filter(|d| d.is_dir()) {
                cmd.cwd(dir);
            }
            (Some(cmd), None)
        };

        let child_opt = if let Some(cmd) = cmd_opt {
            Some(pair.slave.spawn_command(cmd).expect("spawn shell"))
        } else {
            None
        };
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().expect("pty reader");
        let writer =
            PtyWriter(Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer"))));

        let term_size = TerminalSize {
            rows,
            cols,
            pixel_width: pty_size.pixel_width as usize,
            pixel_height: pty_size.pixel_height as usize,
            dpi,
        };
        let mut term = Terminal::new(
            term_size,
            Arc::new(TermConfig),
            "pwrde",
            env!("CARGO_PKG_VERSION"),
            Box::new(writer.clone()),
        );
        // Register attention handler before sharing — OSC 9 toasts signal unread.
        term.set_notification_handler(Box::new(AttentionHandler {
            id,
            sender: events.clone(),
        }));
        let term = Arc::new(Mutex::new(term));

        let redraw_pending = Arc::new(AtomicBool::new(false));

        // PTY reader thread: large-chunk reads, coalesced wakeups.
        {
            let term = Arc::clone(&term);
            let redraw_pending = Arc::clone(&redraw_pending);
            let events = events.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            term.lock().unwrap().advance_bytes(&buf[..n]);
                            // Only signal the UI if it hasn't been signaled
                            // since it last drew.
                            if !redraw_pending.swap(true, Ordering::AcqRel) {
                                let _ = events.send(TermEvent::Wakeup(id));
                            }
                        },
                    }
                }
            });
        }

        // If there's an error message, feed it to the terminal
        if let Some(err_msg) = spawn_error {
            term.lock().unwrap().advance_bytes(err_msg.as_bytes());
            redraw_pending.store(true, Ordering::Release);
            let _ = events.send(TermEvent::Wakeup(id));
        }

        // Capture PID before moving child into the watcher thread.
        let child_pid = child_opt.as_ref().and_then(|c| c.process_id());

        // Child watcher: shell exit closes the window.
        if let Some(mut child) = child_opt {
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = events.send(TermEvent::Exit(id));
            });
        }

        Self {
            id,
            term,
            writer,
            master: pair.master,
            redraw_pending,
            scroll_offset: AtomicUsize::new(0),
            selection: Mutex::new(None),
            proc_title: Mutex::new(None),
            proc_title_misses: AtomicUsize::new(0),
            shpool_session: shpool_name,
            child_pid,
        }
    }

    /// Minimal session for unit tests: `/bin/cat` on a fresh PTY, no reader
    /// thread and no event channel — just enough structure to build a `Tab`.
    #[cfg(test)]
    pub fn placeholder() -> Self {
        let pty_size = PtySize { rows: 24, cols: 80, pixel_width: 640, pixel_height: 384 };
        let pair = native_pty_system().openpty(pty_size).expect("openpty");
        let _child = pair.slave.spawn_command(CommandBuilder::new("/bin/cat")).expect("spawn cat");
        drop(pair.slave);
        let writer =
            PtyWriter(Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer"))));
        let term_size =
            TerminalSize { rows: 24, cols: 80, pixel_width: 640, pixel_height: 384, dpi: 96 };
        let term =
            Terminal::new(term_size, Arc::new(TermConfig), "pwrde-test", "0", Box::new(writer.clone()));
        Self {
            id: 0,
            term: Arc::new(Mutex::new(term)),
            writer,
            master: pair.master,
            redraw_pending: Arc::new(AtomicBool::new(false)),
            scroll_offset: AtomicUsize::new(0),
            selection: Mutex::new(None),
            proc_title: Mutex::new(None),
            proc_title_misses: AtomicUsize::new(0),
            shpool_session: None,
            child_pid: None,
        }
    }

    /// Write user input to the PTY.
    pub fn write(&self, bytes: impl AsRef<[u8]>) {
        let mut writer = self.writer.clone();
        let _ = writer.write_all(bytes.as_ref());
        let _ = writer.flush();
    }

    /// Mark the pending redraw as consumed; the next PTY chunk after this
    /// will emit a fresh wakeup. Call at the start of every frame.
    pub fn begin_frame(&self) {
        self.redraw_pending.store(false, Ordering::Release);
    }

    /// Current window title exactly as set by escape sequences — no fallback.
    /// Callers deciding whether a pane still needs a process-derived name use
    /// this; everything that displays a name uses [`Session::title`].
    pub fn emulator_title(&self) -> String {
        self.term.lock().unwrap().get_title().to_string()
    }

    /// Record (or clear) the process-derived fallback title. Returns whether
    /// the stored value actually changed, so a caller only repaints when a name
    /// really moved — the poll otherwise re-resolves the same name every tick
    /// for the life of a title-less pane.
    pub fn set_proc_title(&self, title: Option<String>) -> bool {
        self.proc_title_misses.store(0, Ordering::Relaxed);
        let mut slot = self.proc_title.lock().unwrap();
        if *slot == title {
            return false;
        }
        *slot = title;
        true
    }

    /// Record a sweep that came back with no name for this pane, and return
    /// the number of consecutive misses.
    pub fn note_proc_title_miss(&self) -> usize {
        self.proc_title_misses.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Consecutive sweeps that failed to name this pane.
    pub fn proc_title_misses(&self) -> usize {
        self.proc_title_misses.load(Ordering::Relaxed)
    }

    /// Whether a process-derived name is already recorded.
    pub fn has_proc_title(&self) -> bool {
        self.proc_title.lock().unwrap().is_some()
    }

    /// Whether this pane wants a process-derived name: the emulator has set no
    /// title of its own, so [`Session::title`] would otherwise show
    /// [`STOCK_TITLE`].
    pub fn needs_proc_title(&self) -> bool {
        let title = self.emulator_title();
        title.is_empty() || title == STOCK_TITLE
    }

    /// Current window title as set by escape sequences, falling back to the
    /// foreground process name while the emulator has none of its own. A real
    /// OSC title always wins.
    pub fn title(&self) -> String {
        // Reads the term lock once: this runs per visible tab per frame, and
        // that lock is contended with the PTY reader thread.
        let title = self.emulator_title();
        if (title.is_empty() || title == STOCK_TITLE)
            && let Some(proc_title) = self.proc_title.lock().unwrap().clone()
        {
            return proc_title;
        }
        title
    }

    /// Read-only snapshot of the pane's text: every line of the whole buffer
    /// (scrollback + visible screen) as right-trimmed strings, top to bottom.
    /// With `tail = Some(n)` only the last n lines are kept. Never touches the
    /// viewport scroll or the selection — this backs the bus `read_pane`
    /// command and is safe to call while the UI is scrolled elsewhere.
    pub fn read_lines(&self, tail: Option<usize>) -> Vec<String> {
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let cols = screen.physical_cols;
        let total = screen.scrollback_rows();
        // Only materialize the rows asked for — the buffer can be thousands
        // of lines deep and the default read is one screen's worth.
        let first = tail.map_or(0, |n| total.saturating_sub(n));
        screen
            .lines_in_phys_range(first..total)
            .iter()
            .map(|l| l.columns_as_str(0..cols).trim_end().to_owned())
            .collect()
    }

    /// Read-only pane metadata, in one lock: `(cols, rows, scrollback_rows,
    /// (cursor col, cursor row), alt_screen)`. Never mutates state.
    pub fn read_info(&self) -> (usize, usize, usize, (usize, usize), bool) {
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let cursor = term.cursor_pos();
        (
            screen.physical_cols,
            screen.physical_rows,
            screen.scrollback_rows(),
            (cursor.x, cursor.y as usize),
            term.is_alt_screen_active(),
        )
    }

    /// Paste text, honoring bracketed-paste mode (the terminal wraps it in
    /// ESC[200~ / ESC[201~ when the app has requested that).
    pub fn paste(&self, text: &str) {
        let _ = self.term.lock().unwrap().send_paste(text);
    }

    // ── Scrollback ──────────────────────────────────────────────────────

    /// Lines the viewport is scrolled up from the live bottom (0 = following).
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset.load(Ordering::Relaxed)
    }

    /// Scroll the viewport by `lines` (positive = back into history), clamped
    /// to the available scrollback. The maximum offset is the number of rows
    /// *above* the visible top — `scrollback_rows()` counts the whole buffer
    /// (visible screen included), so subtract the viewport, or the viewport
    /// slides off the top of the buffer and renders blank.
    pub fn scroll_by(&self, lines: isize) {
        let max = {
            let term = self.term.lock().unwrap();
            let screen = term.screen();
            screen.scrollback_rows().saturating_sub(screen.physical_rows) as isize
        };
        let cur = self.scroll_offset.load(Ordering::Relaxed) as isize;
        let next = (cur + lines).clamp(0, max);
        self.scroll_offset.store(next as usize, Ordering::Relaxed);
    }

    /// Snap back to the live bottom (called on keystroke, like every terminal).
    pub fn scroll_to_bottom(&self) {
        self.scroll_offset.store(0, Ordering::Relaxed);
    }

    /// True when wheel events belong to the *app*, not our scrollback: either
    /// the app enabled mouse tracking (it handles the wheel itself) or it's on
    /// the alternate screen (a full-screen TUI like vim/less/Claude Code, which
    /// has no scrollback — the wheel should scroll *its* content instead).
    pub fn app_consumes_wheel(&self) -> bool {
        let term = self.term.lock().unwrap();
        term.is_mouse_grabbed() || term.is_alt_screen_active()
    }

    /// Forward one wheel step to the app at cell (`col`, `row`). wezterm-term
    /// encodes it as a mouse report when the app enabled tracking, or (on the
    /// alternate screen) translates it into arrow keys — xterm alternate-scroll,
    /// which is how the wheel scrolls a full-screen TUI.
    pub fn forward_wheel(&self, up: bool, col: usize, row: usize) {
        let button = if up { MouseButton::WheelUp(1) } else { MouseButton::WheelDown(1) };
        let event = MouseEvent {
            kind: MouseEventKind::Press,
            x: col,
            y: row as VisibleRowIndex,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
            button,
            modifiers: KeyModifiers::NONE,
        };
        let _ = self.term.lock().unwrap().mouse_event(event);
    }

    /// True when the app has enabled mouse *button* tracking (any of xterm's
    /// 1000/1002/1003 modes), so clicks and drags belong to the app rather
    /// than pwrde's text selection. Unlike [`app_consumes_wheel`](Self::app_consumes_wheel)
    /// this is *not* set by the alternate screen alone — a full-screen TUI that
    /// never asked for the mouse still lets us select text.
    pub fn app_grabs_mouse(&self) -> bool {
        self.term.lock().unwrap().is_mouse_grabbed()
    }

    /// Forward one mouse button event at cell (`col`, `row`) to the app as a
    /// mouse report. `phase` picks press / drag-motion / release and `button`
    /// picks left/middle/right; `shift`/`alt`/`ctrl` carry the keyboard
    /// modifiers that TUIs use to distinguish clicks. wezterm-term encodes the
    /// report per the app's active tracking + encoding mode and writes it to
    /// the PTY; if the app hasn't enabled the matching mode this is a no-op, so
    /// callers can forward unconditionally once [`app_grabs_mouse`](Self::app_grabs_mouse)
    /// is true.
    pub fn forward_mouse(
        &self,
        phase: MousePhase,
        button: MouseBtn,
        col: usize,
        row: usize,
        shift: bool,
        alt: bool,
        ctrl: bool,
    ) {
        let event = mouse_report_event(phase, button, col, row, shift, alt, ctrl);
        let _ = self.term.lock().unwrap().mouse_event(event);
    }

    /// The stable row index at the top of the currently displayed viewport.
    /// Holds the terminal lock only for the lookup.
    fn viewport_top_stable(&self) -> StableRowIndex {
        let offset = self.scroll_offset.load(Ordering::Relaxed) as i32;
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let phys = screen.scrollback_or_visible_row(-offset);
        screen.phys_to_stable_row_index(phys)
    }

    // ── Selection ─────────────────────────────────────────────────────────

    /// Begin a selection at viewport cell (`col`, `row`) — `row` counts from
    /// the top of the currently displayed viewport.
    pub fn begin_selection(&self, col: usize, row: usize) {
        let stable = self.viewport_top_stable() + row as StableRowIndex;
        let point = (col, stable);
        *self.selection.lock().unwrap() = Some(Sel { anchor: point, head: point });
    }

    /// Extend the active selection's head to viewport cell (`col`, `row`).
    pub fn update_selection(&self, col: usize, row: usize) {
        let stable = self.viewport_top_stable() + row as StableRowIndex;
        if let Some(sel) = self.selection.lock().unwrap().as_mut() {
            sel.head = (col, stable);
        }
    }

    /// Drop any active selection.
    pub fn clear_selection(&self) {
        *self.selection.lock().unwrap() = None;
    }

    /// The active selection as an ordered (start, end) pair in stable-row
    /// coordinates, or `None` if there is no selection. `end`'s column is the
    /// last selected cell (inclusive).
    pub fn selection_span(
        &self,
    ) -> Option<((usize, StableRowIndex), (usize, StableRowIndex))> {
        self.selection.lock().unwrap().map(|s| s.ordered())
    }

    /// The selected text, joined with newlines and with trailing blanks on
    /// each row trimmed (the usual terminal copy behavior). `None` when the
    /// selection is empty (a click with no drag).
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection.lock().unwrap().map(|s| s.ordered())?;
        if start == end {
            return None;
        }
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let cols = screen.physical_cols;
        let phys = screen.stable_range(&(start.1..end.1 + 1));
        let lines = screen.lines_in_phys_range(phys);
        let n = lines.len();
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate() {
            let start_col = if i == 0 { start.0 } else { 0 };
            let end_col = if i == n - 1 { (end.0 + 1).min(cols) } else { cols };
            if end_col > start_col {
                out.push_str(line.columns_as_str(start_col..end_col).trim_end());
            }
            if i != n - 1 {
                out.push('\n');
            }
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// The URL under the given visible cell, if any (OSC 8 or plain text).
    /// Wrap-aware: clicking any row of a wrapped URL yields the whole URL.
    pub fn link_at(&self, col: usize, row: usize) -> Option<String> {
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let rows = screen.physical_rows;
        if row >= rows {
            return None;
        }
        // Use the same viewport slice as renderer.rs snapshot_pane so that
        // hit-testing agrees with what is actually visible when scrolled.
        let offset = self.scroll_offset() as i32;
        let lines = screen.lines_in_phys_range(
            screen.scrollback_or_visible_range(&(-offset..rows as i32 - offset)),
        );
        crate::links::links_in_lines(&lines, screen.physical_cols)
            .into_iter()
            .find(|h| h.contains(row, col))
            .map(|h| h.url)
    }

    /// Propagate a window resize to both the grid and the PTY (SIGWINCH).
    pub fn resize(&self, cols: usize, rows: usize, cell_width: u16, cell_height: u16, dpi: u32) {
        if cols == 0 || rows == 0 {
            return;
        }
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: cols as u16 * cell_width,
            pixel_height: rows as u16 * cell_height,
        });
        self.term.lock().unwrap().resize(TerminalSize {
            rows,
            cols,
            pixel_width: cols * cell_width as usize,
            pixel_height: rows * cell_height as usize,
            dpi,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_term(cols: usize, rows: usize) -> Terminal {
        let size =
            TerminalSize { rows, cols, pixel_width: cols * 8, pixel_height: rows * 16, dpi: 96 };
        Terminal::new(size, Arc::new(TermConfig), "pwrde-test", "0", Box::new(std::io::sink()))
    }

    /// The scrollback viewport must never slide off the top of the buffer and
    /// render blank. `scroll_by` clamps the offset to `scrollback_rows() -
    /// physical_rows`; this checks that every offset in that range yields a
    /// full `physical_rows`-tall viewport, and that going past it (the old
    /// bug, clamping to `scrollback_rows()` itself) is what blanks the screen.
    #[test]
    fn scrollback_offset_never_blanks_the_viewport() {
        let p = 5;
        let mut term = make_term(20, p);
        for i in 0..40 {
            term.advance_bytes(format!("line{i}\r\n").as_bytes());
        }

        let max_offset = {
            let s = term.screen();
            s.scrollback_rows().saturating_sub(s.physical_rows)
        };
        assert!(max_offset > 0, "expected scrollback to accumulate");

        for offset in 0..=max_offset {
            let o = offset as i32;
            let screen = term.screen();
            let range = screen.scrollback_or_visible_range(&(-o..p as i32 - o));
            let lines = screen.lines_in_phys_range(range);
            assert_eq!(lines.len(), p, "offset {offset} rendered {} rows, not {p}", lines.len());
        }

        // The regression: clamping to the whole buffer (`scrollback_rows()`)
        // let the offset reach here, where the viewport is starved of rows.
        let bad = term.screen().scrollback_rows() as i32;
        let screen = term.screen();
        let range = screen.scrollback_or_visible_range(&(-bad..p as i32 - bad));
        assert!(
            screen.lines_in_phys_range(range).len() < p,
            "over-scroll must be clamped away by scroll_by"
        );
    }

    /// The wheel belongs to the app (not our scrollback) when it's on the
    /// alternate screen or has grabbed the mouse — this is exactly the gate
    /// `app_consumes_wheel` uses so full-screen TUIs (vim/less/Claude Code)
    /// scroll their own content.
    #[test]
    fn full_screen_apps_claim_the_wheel() {
        let mut term = make_term(20, 5);
        assert!(
            !term.is_mouse_grabbed() && !term.is_alt_screen_active(),
            "a fresh primary screen claims no wheel"
        );

        // Enter/leave the alternate screen (DECSET 1049), as full-screen TUIs do.
        term.advance_bytes(b"\x1b[?1049h");
        assert!(term.is_alt_screen_active(), "alt screen should claim the wheel");
        term.advance_bytes(b"\x1b[?1049l");
        assert!(!term.is_alt_screen_active(), "leaving alt screen releases it");

        // Mouse tracking (DECSET 1000) claims the wheel on the primary screen.
        term.advance_bytes(b"\x1b[?1000h");
        assert!(term.is_mouse_grabbed(), "mouse-tracking apps claim the wheel");
    }

    /// A terminal that enabled mouse tracking turns a forwarded button event
    /// into an SGR mouse report on its writer — press ends in `M`, release in
    /// `m`, coords are 1-based, and modifiers shift the button code. Before the
    /// app opts in, the same call emits nothing so pwrde keeps text selection.
    #[test]
    fn grabbed_terminal_emits_sgr_mouse_report() {
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let sink = Arc::new(Mutex::new(Vec::new()));
        let size =
            TerminalSize { rows: 24, cols: 80, pixel_width: 640, pixel_height: 384, dpi: 96 };
        let mut term = Terminal::new(
            size,
            Arc::new(TermConfig),
            "pwrde-test",
            "0",
            Box::new(Capture(sink.clone())),
        );

        // wezterm-term hands writes to a background thread, so reports arrive
        // asynchronously — poll the sink until the expected bytes show up.
        let dump = || String::from_utf8_lossy(&sink.lock().unwrap()).into_owned();
        let wait_for = |needle: &str| {
            for _ in 0..400 {
                if dump().contains(needle) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        };

        // Ungrabbed: a forwarded press emits nothing (pwrde does selection).
        term.mouse_event(mouse_report_event(
            MousePhase::Press,
            MouseBtn::Left,
            4,
            2,
            false,
            false,
            false,
        ))
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(dump().is_empty(), "ungrabbed terminal must stay silent, got {:?}", dump());

        // Opt in to button tracking (1002) with SGR encoding (1006).
        term.advance_bytes(b"\x1b[?1002h\x1b[?1006h");
        assert!(term.is_mouse_grabbed());

        // Left press at cell (4,2): button 0, coords 1-based → ESC [<0;5;3M.
        term.mouse_event(mouse_report_event(
            MousePhase::Press,
            MouseBtn::Left,
            4,
            2,
            false,
            false,
            false,
        ))
        .unwrap();
        // Shift + right release at (4,2): button 2 + 4 (shift) = 6, release → m.
        term.mouse_event(mouse_report_event(
            MousePhase::Release,
            MouseBtn::Right,
            4,
            2,
            true,
            false,
            false,
        ))
        .unwrap();

        assert!(wait_for("\x1b[<0;5;3M"), "expected left-press report, got {:?}", dump());
        assert!(wait_for("\x1b[<6;5;3m"), "expected shift+right-release report, got {:?}", dump());
    }

    /// OSC 9 toasts must fire `TermEvent::Attention`; plain output must not.
    #[test]
    fn osc9_fires_attention() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<TermEvent>();
        let mut term = make_term(80, 24);
        term.set_notification_handler(Box::new(AttentionHandler { id: 42, sender: tx }));

        // OSC 9 toast notification — should signal attention.
        term.advance_bytes(b"\x1b]9;ping\x07");
        assert!(
            matches!(rx.try_recv(), Ok(TermEvent::Attention(42))),
            "OSC 9 should produce Attention event"
        );

        // Plain output — must NOT signal attention.
        term.advance_bytes(b"hello world\r\n");
        assert!(
            rx.try_recv().is_err(),
            "plain output must not produce Attention event"
        );
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("pwrde-term-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// With no user shpool config, the quiet config is written under the pwrde
    /// dir with the prefix disabled.
    #[test]
    fn quiet_config_written_when_user_has_none() {
        let dir = temp_dir("quiet-fresh");
        let missing = [dir.join("nope/config.toml")];
        let path = shpool_quiet_config_in(&missing, &dir.join("pwrde"))
            .expect("should produce a config path");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHPOOL_CONFIG);
    }

    /// A config written by an older pwrde (no `forward_env`) is upgraded in
    /// place so persisted sessions get COLORTERM and the bus vars forwarded.
    #[test]
    fn quiet_config_upgrades_stale_pwrde_default() {
        let dir = temp_dir("quiet-upgrade");
        let pwrde = dir.join("pwrde");
        std::fs::create_dir_all(&pwrde).unwrap();
        std::fs::write(pwrde.join("shpool.toml"), "prompt_prefix = \"\"\n").unwrap();
        let missing = [dir.join("nope/config.toml")];
        let path = shpool_quiet_config_in(&missing, &pwrde).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SHPOOL_CONFIG);
    }

    /// A user-managed shpool config wins: no pwrde config is offered or written.
    #[test]
    fn quiet_config_defers_to_user_config() {
        let dir = temp_dir("quiet-defer");
        let user = dir.join("config.toml");
        std::fs::write(&user, "prompt_prefix = \"mine\"\n").unwrap();
        let pwrde = dir.join("pwrde");
        assert!(shpool_quiet_config_in(&[user], &pwrde).is_none());
        assert!(!pwrde.join("shpool.toml").exists(), "must not write a rival config");
    }

    /// An existing pwrde shpool config (possibly hand-edited) is not clobbered.
    #[test]
    fn quiet_config_does_not_overwrite_existing() {
        let dir = temp_dir("quiet-keep");
        let pwrde = dir.join("pwrde");
        std::fs::create_dir_all(&pwrde).unwrap();
        std::fs::write(pwrde.join("shpool.toml"), "prompt_prefix = \"custom\"\n").unwrap();
        let missing = [dir.join("nope/config.toml")];
        let path = shpool_quiet_config_in(&missing, &pwrde).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "prompt_prefix = \"custom\"\n"
        );
    }

    // ── title_from_command ──────────────────────────────────────────────────

    /// A bare program name is already the title.
    #[test]
    fn title_from_plain_command() {
        assert_eq!(title_from_command("zsh"), Some("zsh".to_owned()));
    }

    /// `ps` reports a login shell as `-zsh`; the dash is not part of the name.
    #[test]
    fn title_from_login_shell() {
        assert_eq!(title_from_command("-zsh"), Some("zsh".to_owned()));
    }

    /// An absolute path keeps only its final component, and arguments are
    /// dropped.
    #[test]
    fn title_from_absolute_command_with_args() {
        assert_eq!(title_from_command("/usr/bin/vim file.txt"), Some("vim".to_owned()));
    }

    /// Arguments never reach the title, even for a relative program name.
    #[test]
    fn title_from_command_ignores_arguments() {
        assert_eq!(title_from_command("git status --short"), Some("git".to_owned()));
    }

    /// Nothing to name: empty and whitespace-only command lines yield `None`,
    /// so the caller leaves the existing title alone.
    #[test]
    fn title_from_empty_command() {
        assert_eq!(title_from_command(""), None);
        assert_eq!(title_from_command("   "), None);
        assert_eq!(title_from_command("/"), None);
    }

    // ── titles_from_tables / Session::title precedence ──────────────────────

    /// A shpool spec resolves through the env-tagged table, a plain spec
    /// through its own pid, and both land in one batch from one pair of tables.
    #[test]
    fn titles_from_tables_resolves_both_spec_kinds() {
        let needle = "SHPOOL_SESSION_NAME=pwrde-1-abc";
        let env = vec![
            pt(50, 1, "shpool daemon"),
            pt(60, 50, format!("-zsh {needle}").as_str()),
            pt(61, 60, format!("/usr/bin/claude --foo {needle}").as_str()),
        ];
        let clean = vec![
            pt(50, 1, "shpool daemon"),
            pt(60, 50, "-zsh"),
            pt(61, 60, "/usr/bin/claude --foo"),
            pt(200, 1, "-zsh"),
            pt(201, 200, "vim notes.md"),
        ];
        let specs =
            vec![(1u64, Some("pwrde-1-abc".to_owned()), None), (2u64, None, Some(200u32))];
        assert_eq!(
            titles_from_tables(&specs, &clean, Some(&env)),
            vec![(1, "claude".to_owned()), (2, "vim".to_owned())],
        );
    }

    /// An idle pane — a shell with no children — resolves to the shell itself
    /// rather than to nothing, on both the shpool and the plain-pid arm.
    #[test]
    fn titles_from_tables_falls_back_to_an_idle_shell() {
        let needle = "SHPOOL_SESSION_NAME=pwrde-1-abc";
        let env = vec![pt(50, 1, "shpool daemon"), pt(60, 50, format!("-zsh {needle}").as_str())];
        let clean = vec![pt(50, 1, "shpool daemon"), pt(60, 50, "-zsh"), pt(200, 1, "-bash")];
        let specs =
            vec![(1u64, Some("pwrde-1-abc".to_owned()), None), (2u64, None, Some(200u32))];
        assert_eq!(
            titles_from_tables(&specs, &clean, Some(&env)),
            vec![(1, "zsh".to_owned()), (2, "bash".to_owned())],
        );
    }

    /// A zombie at the tip of the walk costs the pane nothing: the name comes
    /// from the deepest command that is usable at all.
    #[test]
    fn titles_from_tables_walks_up_past_an_unusable_tip() {
        let clean =
            vec![pt(200, 1, "-zsh"), pt(201, 200, "vim notes.md"), pt(202, 201, "<defunct>")];
        assert_eq!(deepest_descendant(&clean, 200).as_deref(), Some("<defunct>"));
        assert_eq!(
            titles_from_tables(&[(1u64, None, Some(200u32))], &clean, None),
            vec![(1, "vim".to_owned())],
        );
    }

    /// If the env tag ever lands on the `shpool attach` client, the pane is
    /// left unnamed rather than titled after shpool itself.
    #[test]
    fn titles_from_tables_never_names_a_pane_shpool() {
        let needle = "SHPOOL_SESSION_NAME=pwrde-1-abc";
        let env = vec![pt(80, 1, format!("shpool attach pwrde-1-abc {needle}").as_str())];
        let clean = vec![pt(80, 1, "shpool attach pwrde-1-abc")];
        let specs = vec![(1u64, Some("pwrde-1-abc".to_owned()), None)];
        assert!(titles_from_tables(&specs, &clean, Some(&env)).is_empty());
    }

    /// Without the env table only the shpool-backed specs are lost: the
    /// pid-backed ones still resolve, so one failed `ps -axE` cannot blank the
    /// whole batch. Specs with nothing to walk are absent, never defaulted.
    #[test]
    fn titles_from_tables_drops_only_unresolvable_specs() {
        let clean = vec![pt(200, 1, "-zsh"), pt(201, 200, "vim notes.md")];
        let specs = vec![
            (1u64, Some("pwrde-1-abc".to_owned()), None),
            (2u64, None, Some(200u32)),
            (3u64, None, None),
            (4u64, None, Some(999u32)),
        ];
        assert_eq!(titles_from_tables(&specs, &clean, None), vec![(2, "vim".to_owned())]);
    }

    /// A miss counter that backs a hopeless pane off the fast sweep, and is
    /// reset by any name that lands.
    #[test]
    fn proc_title_misses_reset_on_a_name() {
        let session = Session::placeholder();
        assert_eq!(session.proc_title_misses(), 0);
        assert_eq!(session.note_proc_title_miss(), 1);
        assert_eq!(session.note_proc_title_miss(), 2);
        assert_eq!(session.proc_title_misses(), 2);
        session.set_proc_title(Some("vim".to_owned()));
        assert_eq!(session.proc_title_misses(), 0);
    }

    /// `ps` parenthesizes a process it can no longer read; that is never a tab
    /// name, so the pane keeps whatever it had.
    #[test]
    fn title_from_a_reaped_process_is_rejected() {
        assert_eq!(title_from_command("(ps)"), None);
        assert_eq!(title_from_command("(zsh)"), None);
        assert_eq!(title_from_command("<defunct>"), None);
    }

    /// A fresh pane carries the stock emulator title, so it wants a
    /// process-derived name and shows it once given — and re-resolving the same
    /// name is not a change, so no repaint is owed.
    #[test]
    fn stock_title_takes_the_process_fallback() {
        let session = Session::placeholder();
        assert_eq!(session.emulator_title(), STOCK_TITLE);
        assert!(session.needs_proc_title());
        assert!(session.set_proc_title(Some("vim".to_owned())));
        assert_eq!(session.title(), "vim");
        assert!(session.has_proc_title());
        assert!(!session.set_proc_title(Some("vim".to_owned())));
    }

    /// A real OSC title always wins over the fallback, and a pane that has one
    /// is never asked for a process name in the first place.
    #[test]
    fn real_osc_title_beats_the_process_fallback() {
        let session = Session::placeholder();
        session.set_proc_title(Some("vim".to_owned()));
        session.term.lock().unwrap().advance_bytes(b"\x1b]0;real\x07");
        assert_eq!(session.emulator_title(), "real");
        assert!(!session.needs_proc_title());
        assert_eq!(session.title(), "real");
    }

    /// The read-only pane snapshot: `None` returns the whole buffer
    /// (scrollback + visible screen), `Some(n)` the last n lines — here the
    /// final two, both non-empty — and the cursor/size info is read without
    /// disturbing anything.
    #[test]
    fn read_lines_returns_full_buffer_and_tail() {
        let session = Session::placeholder();
        let mut feed = String::new();
        for i in 1..24 {
            feed.push_str(&format!("r{i}\r\n"));
        }
        feed.push_str("r24"); // no trailing newline: r24 must stay the last line
        session.term.lock().unwrap().advance_bytes(feed.as_bytes());

        let lines = session.read_lines(None);
        let nonempty: Vec<&str> = lines.iter().map(String::as_str).filter(|l| !l.is_empty()).collect();
        let expected: Vec<String> = (1..=24).map(|i| format!("r{i}")).collect();
        assert_eq!(nonempty, expected.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(lines.last().map(String::as_str), Some("r24"));

        let tail = session.read_lines(Some(2));
        assert_eq!(tail, vec!["r23".to_owned(), "r24".to_owned()]);

        let (cols, rows, scrollback, cursor, alt) = session.read_info();
        assert_eq!((cols, rows), (80, 24));
        assert!(scrollback >= rows);
        assert_eq!(cursor, (3, 23));
        assert!(!alt);
    }

    // ── foreground_command / deepest_descendant ──────────────────────────────

    fn pt(pid: u32, ppid: u32, cmd: &str) -> (u32, u32, String) {
        (pid, ppid, cmd.to_owned())
    }

    /// Shell with no children → None.
    #[test]
    fn deepest_descendant_no_children() {
        let table = vec![pt(100, 1, "bash")];
        assert_eq!(deepest_descendant(&table, 100), None);
    }

    /// Shell → one child → grandchild → returns grandchild.
    #[test]
    fn deepest_descendant_chain() {
        let table = vec![
            pt(100, 1, "bash"),
            pt(101, 100, "vim"),
            pt(102, 101, "git"),
        ];
        assert_eq!(deepest_descendant(&table, 100), Some("git".to_owned()));
    }

    /// Among multiple children the one with the lowest pid wins (first in
    /// sorted order), so the walk is deterministic.
    #[test]
    fn deepest_descendant_picks_lowest_pid_child() {
        let table = vec![
            pt(100, 1, "bash"),
            pt(105, 100, "zsh"),
            pt(102, 100, "vim"),  // lower pid → chosen
        ];
        // First child (pid 102 "vim") has no children — result is "vim".
        assert_eq!(deepest_descendant(&table, 100), Some("vim".to_owned()));
    }

    /// Root pid absent from table (no children) → None.
    #[test]
    fn deepest_descendant_unknown_root() {
        let table = vec![pt(200, 1, "sh")];
        assert_eq!(deepest_descendant(&table, 999), None);
    }

    /// The env-subtree root is the matching process whose parent doesn't
    /// match — the daemon-side session shell, not its descendants (which
    /// inherit the variable) and not the daemon itself.
    #[test]
    fn env_subtree_root_finds_session_shell() {
        let needle = "SHPOOL_SESSION_NAME=pwrde-1-abc";
        let table = vec![
            pt(50, 1, "shpool daemon"),
            pt(60, 50, format!("-zsh {needle} TERM=xterm").as_str()),
            pt(61, 60, format!("claude {needle}").as_str()),
            pt(70, 50, "-zsh SHPOOL_SESSION_NAME=other"),
        ];
        assert_eq!(env_subtree_root(&table, needle), Some(60));
        assert_eq!(env_subtree_root(&table, "SHPOOL_SESSION_NAME=missing"), None);
    }

    /// A tool session (`command` set) runs that command through the login
    /// shell from `cwd`, paints its output, and reports `Exit` when it ends —
    /// the contract the CLI tool pages build on.
    #[test]
    fn command_session_runs_in_cwd_and_exits() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = temp_dir("tool-cwd");
        let (tx, rx) = mpsc::channel::<TermEvent>();
        let session = Session::new(
            7,
            80,
            24,
            8,
            16,
            96,
            Some(&dir),
            Some("basename \"$PWD\""),
            tx,
            Some("ignored-when-command-is-set".into()),
        );
        let want = dir.file_name().unwrap().to_string_lossy().into_owned();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            if let Ok(TermEvent::Exit(7)) = rx.recv_timeout(Duration::from_millis(100)) {
                exited = true;
            }
        }
        assert!(exited, "command session should report Exit");
        let text = {
            let term = session.term.lock().unwrap();
            let screen = term.screen();
            let rows = screen.physical_rows;
            screen
                .lines_in_phys_range(0..rows)
                .iter()
                .map(|l| l.as_str().into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text.contains(&want), "expected {want:?} in grid, got {text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
