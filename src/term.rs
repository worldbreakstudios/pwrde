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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use wezterm_term::color::ColorPalette;
use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize};
use winit::event_loop::EventLoopProxy;

/// User events forwarded into the winit event loop, tagged with the
/// originating session's id.
#[derive(Debug, Clone, Copy)]
pub enum TermEvent {
    /// Grid changed; a redraw is needed (coalesced).
    Wakeup(u64),
    /// Shell exited.
    Exit(u64),
}

#[derive(Debug)]
struct TermConfig;

impl TerminalConfiguration for TermConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
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

pub struct Session {
    pub id: u64,
    pub term: Arc<Mutex<Terminal>>,
    writer: PtyWriter,
    master: Box<dyn MasterPty + Send>,
    redraw_pending: Arc<AtomicBool>,
}

impl Session {
    pub fn new(
        id: u64,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        dpi: u32,
        proxy: EventLoopProxy<TermEvent>,
    ) -> Self {
        let pty_size = PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: cols as u16 * cell_width,
            pixel_height: rows as u16 * cell_height,
        };
        let pair = native_pty_system().openpty(pty_size).expect("openpty");

        let mut cmd = CommandBuilder::new_default_prog(); // user's shell
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
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
        let term = Terminal::new(
            term_size,
            Arc::new(TermConfig),
            "pwrde",
            env!("CARGO_PKG_VERSION"),
            Box::new(writer.clone()),
        );
        let term = Arc::new(Mutex::new(term));

        let redraw_pending = Arc::new(AtomicBool::new(false));

        // PTY reader thread: large-chunk reads, coalesced wakeups.
        {
            let term = Arc::clone(&term);
            let redraw_pending = Arc::clone(&redraw_pending);
            let proxy = proxy.clone();
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
                                let _ = proxy.send_event(TermEvent::Wakeup(id));
                            }
                        },
                    }
                }
            });
        }

        // Child watcher: shell exit closes the window.
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = proxy.send_event(TermEvent::Exit(id));
        });

        Self { id, term, writer, master: pair.master, redraw_pending }
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

    /// The URL under the given visible cell, if any (OSC 8 or plain text).
    /// Wrap-aware: clicking any row of a wrapped URL yields the whole URL.
    pub fn link_at(&self, col: usize, row: usize) -> Option<String> {
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        if row >= screen.physical_rows {
            return None;
        }
        let lines =
            screen.lines_in_phys_range(screen.phys_range(&(0..screen.physical_rows as i64)));
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
