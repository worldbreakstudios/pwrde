//! Deterministic workspaces for the web build, screenshots, and tests: groups,
//! splits, and tabs whose sessions are fed recorded terminal output
//! (`src/fixtures/*.vt`, raw VT byte streams) instead of a shell. On wasm32
//! `Session::new` has no PTY, so this is the only way content reaches a grid
//! there; on native the same seeding works on top of real shells.

use crate::app::App;
use crate::workspace::Dir;

/// A recorded transcript: what a pane shows, plus the tab title the program
/// inside would have set.
pub struct Transcript {
    pub title: &'static str,
    pub bytes: &'static [u8],
}

/// An interactive shell: `ls`, `git status`, a test run.
pub const SHELL: Transcript =
    Transcript { title: "zsh", bytes: include_bytes!("fixtures/shell.vt") };
/// A Claude Code session mid-task.
pub const CLAUDE: Transcript =
    Transcript { title: "claude", bytes: include_bytes!("fixtures/claude.vt") };
/// A release build with one warning.
pub const BUILD: Transcript =
    Transcript { title: "cargo", bytes: include_bytes!("fixtures/build.vt") };

/// One group of the demo layout: its sidebar name and the transcripts of its
/// tiles, in split order (the first tile is the founding, primary one).
pub struct Group {
    pub name: &'static str,
    pub tiles: &'static [&'static [Transcript]],
}

/// The demo workspace: a pwrde group split side-by-side (shell | claude, with
/// a build tab behind the shell) and a second group holding one shell.
pub const DEMO: &[Group] = &[
    Group { name: "pwrde", tiles: &[&[SHELL, BUILD], &[CLAUDE]] },
    Group { name: "rcn", tiles: &[&[SHELL]] },
];

/// Replace the empty state with `groups`, feeding every tab its transcript.
/// The first group ends up active. Call once the window exists, so the grids
/// get their real size from `sync_layout` before the bytes land.
pub fn seed(app: &mut App, groups: &[Group]) {
    for group in groups {
        app.add_group(group.name.to_string(), None);
        // `add_group` queues the primary command (`claude` by default) for the
        // founding pane's first wakeup. A transcript already shows a program
        // running, so nothing should be typed over it.
        app.pending_primary_cmd.clear();
        // `add_group` founds the tile with one tab; every further tile is a
        // side-by-side split of the focused one, every further transcript in
        // a tile is another tab. Splits and tabs both spawn sessions.
        for (ti, tile) in group.tiles.iter().enumerate() {
            if ti > 0 {
                app.split(Dir::Row);
            }
            for _ in 1..tile.len() {
                app.new_tab();
            }
        }
        app.sync_layout();
        let ws = &mut app.workspaces[app.active];
        for (tile, transcripts) in ws.root.tiles_mut().into_iter().zip(group.tiles.iter()) {
            for (tab, transcript) in tile.tabs.iter_mut().zip(transcripts.iter()) {
                tab.session.feed(transcript.bytes);
                // Programs name their tab via OSC 0/2; the transcripts carry
                // that where the program would have sent it, and this is the
                // process-derived fallback the `ps` sweep would have found.
                tab.session.set_proc_title(Some(transcript.title.to_string()));
            }
            // Show the first tab, as a user would after opening the pane.
            tile.active = 0;
        }
    }
    app.switch_workspace(0);
    app.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Session;

    fn visible_text(session: &Session) -> String {
        let term = session.term.lock().unwrap();
        let screen = term.screen();
        screen
            .lines_in_phys_range(0..screen.physical_rows)
            .iter()
            .map(|l| l.as_str().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn transcripts_land_on_the_grid() {
        for t in [SHELL, CLAUDE, BUILD] {
            let session = Session::placeholder();
            session.feed(t.bytes);
            assert!(!visible_text(&session).trim().is_empty(), "{} rendered nothing", t.title);
        }
        let session = Session::placeholder();
        session.feed(SHELL.bytes);
        let text = visible_text(&session);
        assert!(text.contains("git status --short"), "{text}");
        assert!(text.contains("402 passed"), "{text}");
    }

    #[test]
    fn demo_layout_is_well_formed() {
        assert_eq!(DEMO[0].tiles.len(), 2);
        assert!(DEMO.iter().all(|g| g.tiles.iter().all(|t| !t.is_empty())));
    }
}
