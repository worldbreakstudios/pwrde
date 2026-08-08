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
/// User events forwarded to the gpui app over an mpsc channel.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// Grid changed; a redraw is needed (coalesced). Tagged with the session id.
    Wakeup(u64),
    /// Shell exited. Tagged with the session id.
    Exit(u64),
    /// The program requested attention (OSC 9 toast notification). Tagged with the session id.
    Attention(u64),
    /// A `drop` worktree finished provisioning: create its group at `cwd`.
    GroupReady { name: String, cwd: std::path::PathBuf },
    /// `drop` failed; show `message` in the picker overlay.
    GroupFailed { message: String },
    /// `drop -d --json` completed: full list of managed worktrees.
    CleanupScanned(Vec<crate::cleanup::WorktreeInfo>),
    /// `drop -d --json` failed with an error message.
    CleanupScanFailed(String),
    /// `drop rm … --json` completed: counts of removed and failed worktrees,
    /// plus the first failure's reason when there is one.
    CleanupRemoved { removed: usize, failed: usize, error: Option<String> },
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

/// Config file to pass to `shpool attach` so the daemon it auto-starts skips
/// the `shpool:$SHPOOL_SESSION_NAME` prompt prefix, which otherwise stamps an
/// extra line above every prompt in persisted panes.
///
/// Defers to the user: if they keep their own shpool config (either the
/// macOS `~/Library/Application Support/shpool/config.toml` or the XDG
/// `~/.config/shpool/config.toml`), returns None so shpool loads it normally.
/// Otherwise lazily writes `~/.pwrde/shpool.toml` with `prompt_prefix = ""`
/// and returns its path. Any failure returns None — persistence must keep
/// working even if the prefix can't be silenced.
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
    if !path.exists() {
        std::fs::create_dir_all(pwrde_dir).ok()?;
        std::fs::write(&path, "prompt_prefix = \"\"\n").ok()?;
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
    /// The shpool session name if this session is backed by shpool.
    pub shpool_session: Option<String>,
}

impl Session {
    /// Spawn the user's shell on a fresh PTY. `cwd` sets the shell's working
    /// directory; `None` inherits pwrde's own working directory.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        dpi: u32,
        cwd: Option<&std::path::Path>,
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

        let shpool_name = shpool_session.clone();

        let (cmd_opt, spawn_error) = if let Some(ref name) = shpool_name {
            if let Some(shpool_path) = shpool_binary() {
                let mut cmd = CommandBuilder::new(shpool_path);
                // Global flag, so it must precede the subcommand (after
                // `attach`, `-c` means `--cmd`). Only matters when this attach
                // auto-starts the daemon; an already-running daemon keeps the
                // config it was launched with.
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
                cmd.env("COLORTERM", "truecolor");
                cmd.env("PWRDE", "1");
                (Some(cmd), None)
            } else {
                // Fail the pane loudly rather than silently losing persistence.
                (None, Some("shpool not found — install it (brew install shell-pool/shpool/shpool) or disable Persist sessions\r\n"))
            }
        } else {
            let mut cmd = CommandBuilder::new_default_prog(); // user's shell
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
            shpool_session: shpool_name,
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
            shpool_session: None,
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

    /// Current window title as set by escape sequences.
    pub fn title(&self) -> String {
        self.term.lock().unwrap().get_title().to_string()
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
        crate::links::links_in_lines(&lines)
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
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "prompt_prefix = \"\"\n"
        );
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
}
