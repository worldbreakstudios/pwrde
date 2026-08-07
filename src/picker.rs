//! Directory picker model: candidate scan, git detection, and persistence.
//!
//! Creating a group in pwrde asks "where?" — every terminal in a group spawns
//! in that group's cwd. This module supplies the candidate directories for
//! that choice: the home dir, `~/src`, and each immediate subdirectory of
//! `~/src` (the usual home for checkouts), flagging the ones that are git
//! repos so the UI can mark them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::workspace::LayoutRect;

/// How many recent directories are remembered.
const MAX_RECENTS: usize = 8;

/// One candidate directory offered by the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerEntry {
    /// Absolute path the group's shells will spawn in.
    pub path: PathBuf,
    /// Short display label: `~`, `~/src`, or the basename for `~/src/*`.
    pub label: String,
    /// Whether `<path>/.git` exists (repo checkout).
    pub is_git: bool,
}

impl PickerEntry {
    /// Builds an entry for `path`, probing `<path>/.git` for the git flag.
    pub fn new(path: PathBuf, label: String) -> Self {
        let is_git = path.join(".git").exists();
        Self {
            path,
            label,
            is_git,
        }
    }
}

/// Scans the candidate directories: `~`, `~/src`, then every immediate
/// subdirectory of `~/src` (directories only, hidden entries skipped).
///
/// A missing `~/src` is not an error — it just yields fewer entries.
pub fn scan_entries() -> Vec<PickerEntry> {
    let mut out = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return out;
    };
    out.push(PickerEntry::new(home.clone(), "~".to_string()));

    let src = home.join("src");
    if src.is_dir() {
        out.push(PickerEntry::new(src.clone(), "~/src".to_string()));
        out.extend(scan_children(&src));
    }
    out
}

/// User state that outlives a run: pinned favourites and recently used dirs.
///
/// Persisted as JSON at `<data_dir>/pwrde/groups.json`. Every IO failure is
/// swallowed — a missing or corrupt file simply means "no pins, no recents".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerStore {
    /// Directories the user pinned, in the order they were pinned.
    #[serde(default)]
    pub pinned: Vec<PathBuf>,
    /// Directories most recently chosen, most recent first, capped at 8.
    #[serde(default)]
    pub recents: Vec<PathBuf>,
}

impl PickerStore {
    /// Reads the store from disk, returning the default when unavailable.
    pub fn load() -> Self {
        let Some(path) = store_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the store to disk, creating parent dirs; IO errors are ignored.
    pub fn save(&self) {
        let Some(path) = store_path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        let _ = std::fs::write(path, text);
    }

    /// Records `dir` as the most recent choice (deduped, capped) and persists.
    pub fn add_recent(&mut self, dir: &Path) {
        self.recents.retain(|p| p != dir);
        self.recents.insert(0, dir.to_path_buf());
        self.recents.truncate(MAX_RECENTS);
        self.save();
    }

    /// Whether `dir` is currently pinned.
    pub fn is_pinned(&self, dir: &Path) -> bool {
        self.pinned.iter().any(|p| p == dir)
    }

    /// Adds or removes `dir` from the favourites and persists.
    pub fn toggle_pin(&mut self, dir: &Path) {
        if self.is_pinned(dir) {
            self.pinned.retain(|p| p != dir);
        } else {
            self.pinned.push(dir.to_path_buf());
        }
        self.save();
    }
}

/// One line of the picker's visible list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerRow {
    /// A dim section caption such as `FAVORITES`; not selectable.
    Header(&'static str),
    /// A selectable candidate directory.
    Entry(PickerEntry),
}

/// Which group a fork-source row belongs to — drives both its label tag color
/// and what confirming it does (fork via `drop`, or open a group directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkScope {
    /// The default "new branch off the remote default" row (forks via `drop`).
    Default,
    /// "Use the repo root" — open the group in the repo itself, no worktree
    /// and no `drop` invocation.
    RepoRoot,
    /// Attach the group to an existing worktree (opens at [`ForkEntry::path`],
    /// no `drop`).
    Worktree,
    /// A local branch to fork a new `drop` worktree off of.
    Local,
    /// A remote branch to fork a new `drop` worktree off of.
    Remote,
}

/// One fork-source choice in the second picker step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkEntry {
    /// Display label, e.g. `local   main  (current)`.
    pub label: String,
    /// The `--from` ref passed to `drop` for [`ForkScope::Local`]/[`Remote`].
    /// `None` for every other scope.
    pub from: Option<String>,
    /// The directory a group opens in directly (no `drop`) for
    /// [`ForkScope::RepoRoot`] (the repo) and [`ForkScope::Worktree`] (the
    /// worktree). `None` for the forking scopes.
    pub path: Option<PathBuf>,
    pub scope: ForkScope,
}

/// The fork-source picker: a flat, filterable list of [`ForkEntry`]s over the
/// git repo `drop` will spawn a worktree in. Mirrors drop's own `fork from>`
/// prompt — the default (new branch off the remote default) sits first, then
/// local branches, then remote branches.
pub struct ForkPicker {
    /// The git repo whose worktree we'll create.
    pub repo: PathBuf,
    /// The group name to use once the worktree exists (the repo's basename).
    pub name: String,
    /// Every fork choice, in display order (default, locals, remotes).
    pub entries: Vec<ForkEntry>,
    pub query: String,
    pub selected: usize,
    /// The filtered view currently on screen.
    pub rows: Vec<ForkEntry>,
}

impl ForkPicker {
    /// Open the fork picker over `repo`, naming the eventual group `name`.
    pub fn new(repo: PathBuf, name: String, entries: Vec<ForkEntry>) -> Self {
        let mut picker =
            Self { repo, name, entries, query: String::new(), selected: 0, rows: Vec::new() };
        picker.rebuild();
        picker
    }

    /// Appends a typed character to the query and refilters.
    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.rebuild();
    }

    /// Removes the last query character and refilters.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.rebuild();
    }

    /// Moves the highlight by `delta` rows, clamped to the list.
    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// Selects the row at `index` when it is in range.
    pub fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
        }
    }

    /// The highlighted fork source, if the list is not empty.
    pub fn selected_entry(&self) -> Option<&ForkEntry> {
        self.rows.get(self.selected)
    }

    /// Recomputes [`ForkPicker::rows`] from the query (case-insensitive
    /// substring match on the label; empty query shows everything).
    fn rebuild(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.rows = if query.is_empty() {
            self.entries.clone()
        } else {
            self.entries.iter().filter(|e| e.label.to_lowercase().contains(&query)).cloned().collect()
        };
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }
}

/// The open directory picker: candidates, query, selection and visible rows.
///
/// The visible list is recomputed on every mutation. With an empty query it is
/// grouped `FAVORITES` / `RECENTS` / `ALL`; with a query it collapses to one
/// flat list of case-insensitive substring matches on the label or path.
#[derive(Clone, Debug)]
pub struct Picker {
    /// Every candidate directory: the scan plus any pinned/recent extras.
    pub entries: Vec<PickerEntry>,
    /// Persisted pins and recents, shared with the picker's actions.
    pub store: PickerStore,
    /// The current search text.
    pub query: String,
    /// Index into [`Picker::rows`] of the highlighted row.
    pub selected: usize,
    /// The filtered, ordered list currently on screen.
    pub rows: Vec<PickerRow>,
}

impl Picker {
    /// Opens a picker: scans candidates, loads the store, builds the list.
    pub fn new() -> Self {
        let store = PickerStore::load();
        let mut entries = scan_entries();
        let home = dirs::home_dir();
        for path in store.pinned.iter().chain(store.recents.iter()) {
            if !entries.iter().any(|e| &e.path == path) {
                let label = label_for(path, home.as_deref());
                entries.push(PickerEntry::new(path.clone(), label));
            }
        }
        let mut picker = Self {
            entries,
            store,
            query: String::new(),
            selected: 0,
            rows: Vec::new(),
        };
        picker.rebuild();
        picker
    }

    /// Appends a typed character to the query and refilters.
    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.rebuild();
    }

    /// Removes the last query character and refilters.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.rebuild();
    }

    /// Moves the highlight by `delta` rows, skipping headers and clamping.
    pub fn move_selection(&mut self, delta: isize) {
        let step = if delta < 0 { -1 } else { 1 };
        let mut cursor = self.selected as isize;
        for _ in 0..delta.unsigned_abs().max(1) {
            let mut next = cursor + step;
            while let Some(row) = usize::try_from(next).ok().and_then(|i| self.rows.get(i)) {
                if matches!(row, PickerRow::Entry(_)) {
                    break;
                }
                next += step;
            }
            if usize::try_from(next).ok().and_then(|i| self.rows.get(i)).is_some() {
                cursor = next;
            }
        }
        self.selected = cursor.max(0) as usize;
    }

    /// Selects the row at `index` when it is a selectable entry.
    pub fn select(&mut self, index: usize) {
        if matches!(self.rows.get(index), Some(PickerRow::Entry(_))) {
            self.selected = index;
        }
    }

    /// The highlighted entry, if the list is not empty.
    pub fn selected_entry(&self) -> Option<&PickerEntry> {
        match self.rows.get(self.selected) {
            Some(PickerRow::Entry(entry)) => Some(entry),
            _ => None,
        }
    }

    /// Whether `dir` is pinned (drives the star glyph).
    pub fn is_pinned(&self, dir: &Path) -> bool {
        self.store.is_pinned(dir)
    }

    /// Toggles the pin on `dir`, persists, and reorders the list.
    pub fn toggle_pin(&mut self, dir: &Path) {
        self.store.toggle_pin(dir);
        self.rebuild();
    }

    /// Records `dir` as the newest recent choice and persists.
    pub fn record_recent(&mut self, dir: &Path) {
        self.store.add_recent(dir);
    }

    /// Recomputes [`Picker::rows`] from the query, pins and recents.
    fn rebuild(&mut self) {
        let query = self.query.trim().to_lowercase();
        let mut rows = Vec::new();
        if query.is_empty() {
            let pinned: Vec<PickerEntry> = self
                .store
                .pinned
                .iter()
                .filter_map(|p| self.entry_for(p))
                .collect();
            let recents: Vec<PickerEntry> = self
                .store
                .recents
                .iter()
                .filter(|p| !self.store.is_pinned(p))
                .filter_map(|p| self.entry_for(p))
                .collect();
            push_section(&mut rows, "FAVORITES", pinned);
            let recent_paths: Vec<PathBuf> = recents.iter().map(|e| e.path.clone()).collect();
            push_section(&mut rows, "RECENTS", recents);
            let rest: Vec<PickerEntry> = self
                .entries
                .iter()
                .filter(|e| !self.store.is_pinned(&e.path) && !recent_paths.contains(&e.path))
                .cloned()
                .collect();
            push_section(&mut rows, "ALL", rest);
        } else {
            for entry in &self.entries {
                let path = entry.path.to_string_lossy().to_lowercase();
                if entry.label.to_lowercase().contains(&query) || path.contains(&query) {
                    rows.push(PickerRow::Entry(entry.clone()));
                }
            }
        }
        self.rows = rows;
        self.clamp_selection();
    }

    /// Pulls the scanned entry for `path`, synthesising one when unknown.
    fn entry_for(&self, path: &Path) -> Option<PickerEntry> {
        self.entries.iter().find(|e| e.path == path).cloned()
    }

    /// Keeps [`Picker::selected`] on a selectable row inside the list.
    fn clamp_selection(&mut self) {
        let first = self.rows.iter().position(|r| matches!(r, PickerRow::Entry(_)));
        let Some(first) = first else {
            self.selected = 0;
            return;
        };
        if !matches!(self.rows.get(self.selected), Some(PickerRow::Entry(_))) {
            let below = self
                .rows
                .iter()
                .enumerate()
                .skip(self.selected)
                .find(|(_, r)| matches!(r, PickerRow::Entry(_)))
                .map(|(i, _)| i);
            self.selected = below.unwrap_or(first);
        }
    }
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

/// Appends `header` plus `entries` to `rows` when the section is non-empty.
fn push_section(rows: &mut Vec<PickerRow>, header: &'static str, entries: Vec<PickerEntry>) {
    if entries.is_empty() {
        return;
    }
    rows.push(PickerRow::Header(header));
    rows.extend(entries.into_iter().map(PickerRow::Entry));
}

/// Display label for a path outside the scan: `~`, `~/src`, or the basename.
fn label_for(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home {
            return "~".to_string();
        }
        if path == home.join("src") {
            return "~/src".to_string();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Location of the persisted store: `<data_dir>/pwrde/groups.json`.
fn store_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("pwrde").join("groups.json"))
}

/// Collects the visible subdirectories of `dir`, sorted by label.
fn scan_children(dir: &Path) -> Vec<PickerEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut kids: Vec<PickerEntry> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            Some(PickerEntry::new(e.path(), name))
        })
        .collect();
    kids.sort_by_key(|e| e.label.to_lowercase());
    kids
}

/// Popover width, logical px.
const PANEL_W: f32 = 420.0;
/// Height of one list row, logical px.
const ROW_H: f32 = 28.0;
/// Height of the search box, logical px.
const SEARCH_H: f32 = 38.0;
/// Padding inside the popover panel, logical px.
const PANEL_PAD: f32 = 10.0;
/// How many rows the list shows before it scrolls.
const MAX_VISIBLE_ROWS: usize = 12;

/// Pixel geometry of the open popover, shared by rendering and hit-testing.
///
/// Recomputed from the window size on every frame and on every click, so the
/// picker itself stores no geometry.
#[derive(Clone, Debug)]
pub struct PickerLayout {
    /// The popover panel, centered in the window.
    pub panel: LayoutRect,
    /// The search box at the top of the panel.
    pub search: LayoutRect,
    /// Height of one list row, physical px.
    pub row_h: f32,
    /// Top of the first visible list row, physical px.
    pub list_top: f32,
    /// Index into [`Picker::rows`] of the first row on screen.
    pub first_visible: usize,
    /// How many rows fit on screen.
    pub visible: usize,
}

impl PickerLayout {
    /// Lays the popover out for a `width`×`height` window, scrolled so the
    /// selected row (of `rows_len` total) is on screen. Shared by both the
    /// directory and fork-source pickers.
    pub fn compute(width: u32, height: u32, scale: f32, rows_len: usize, selected: usize) -> Self {
        let (win_w, win_h) = (width as f32, height as f32);
        let pad = (PANEL_PAD * scale).round();
        let row_h = (ROW_H * scale).round();
        let search_h = (SEARCH_H * scale).round();
        let panel_w = (PANEL_W * scale).round().min(win_w - 2.0 * pad).max(row_h);

        // Rows are capped by both the list length and the window height.
        let room = (((win_h - 4.0 * pad - search_h) / row_h).floor()).max(1.0) as usize;
        let visible = rows_len.min(MAX_VISIBLE_ROWS).min(room);
        let first_visible = (selected + 1).saturating_sub(visible);

        let panel_h = search_h + visible as f32 * row_h + 2.0 * pad;
        let panel = LayoutRect {
            x: ((win_w - panel_w) / 2.0).round().max(0.0),
            y: ((win_h - panel_h) / 2.0).round().max(0.0),
            w: panel_w,
            h: panel_h,
        };
        let search = LayoutRect {
            x: panel.x + pad,
            y: panel.y + pad,
            w: panel.w - 2.0 * pad,
            h: search_h,
        };
        let list_top = search.y + search_h;
        Self { panel, search, row_h, list_top, first_visible, visible }
    }

    /// The rect of row `index` (an index into [`Picker::rows`]), or `None`
    /// when that row is scrolled out of view.
    pub fn row_rect(&self, index: usize) -> Option<LayoutRect> {
        let slot = index.checked_sub(self.first_visible)?;
        if slot >= self.visible {
            return None;
        }
        Some(LayoutRect {
            x: self.panel.x,
            y: self.list_top + slot as f32 * self.row_h,
            w: self.panel.w,
            h: self.row_h,
        })
    }

    /// The star (pin toggle) hit area at the right end of `row`.
    pub fn star_rect(&self, row: &LayoutRect) -> LayoutRect {
        LayoutRect { x: row.x + row.w - self.row_h, y: row.y, w: self.row_h, h: row.h }
    }

    /// The [`Picker::rows`] index under a click, if any row is there.
    pub fn row_at(&self, px: f32, py: f32) -> Option<usize> {
        (self.first_visible..self.first_visible + self.visible)
            .find(|i| self.row_rect(*i).is_some_and(|r| r.contains(px, py)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork_entry(label: &str, from: Option<&str>, scope: ForkScope) -> ForkEntry {
        ForkEntry { label: label.to_string(), from: from.map(str::to_string), path: None, scope }
    }

    fn sample_fork_picker() -> ForkPicker {
        let entries = vec![
            fork_entry("↪ new branch off default (origin/main)", None, ForkScope::Default),
            fork_entry("⌂ repo root (no worktree)", None, ForkScope::RepoRoot),
            ForkEntry {
                label: "worktree  bordeaux  (.worktrees/36647115)".into(),
                from: None,
                path: Some(PathBuf::from("/repo/.worktrees/36647115")),
                scope: ForkScope::Worktree,
            },
            fork_entry("local   main  (current, default)", Some("main"), ForkScope::Local),
            fork_entry("local   tw-term-features", Some("tw-term-features"), ForkScope::Local),
            fork_entry("remote  origin/tw-term-features", Some("origin/tw-term-features"), ForkScope::Remote),
        ];
        ForkPicker::new(PathBuf::from("/repo"), "repo".into(), entries)
    }

    /// The default fork source is the first row and starts selected, so hitting
    /// Enter immediately forks a worktree off the remote default (no `--from`).
    #[test]
    fn fork_picker_defaults_to_the_remote_default() {
        let picker = sample_fork_picker();
        assert_eq!(picker.selected, 0);
        let entry = picker.selected_entry().expect("a selected entry");
        assert_eq!(entry.scope, ForkScope::Default);
        assert_eq!(entry.from, None);
    }

    /// Typing filters the list by case-insensitive substring on the label and
    /// resets the highlight to the first surviving row.
    #[test]
    fn fork_picker_filters_by_query() {
        let mut picker = sample_fork_picker();
        for ch in "tw".chars() {
            picker.push_char(ch);
        }
        assert_eq!(picker.rows.len(), 2, "only the two tw-* branches match");
        assert_eq!(picker.selected, 0);
        assert!(picker.rows.iter().all(|e| e.label.contains("tw-term-features")));

        // Clearing the query restores every choice.
        picker.backspace();
        picker.backspace();
        assert_eq!(picker.rows.len(), 6);
    }

    /// The "repo root" row sits second (just below the default), carries no
    /// `--from`, and is tagged so `confirm_fork` opens the repo directly
    /// instead of running drop.
    #[test]
    fn fork_picker_offers_repo_root_below_default() {
        let mut picker = sample_fork_picker();
        picker.move_selection(1);
        let entry = picker.selected_entry().expect("a selected entry");
        assert_eq!(entry.scope, ForkScope::RepoRoot);
        assert_eq!(entry.from, None);
    }

    /// An existing worktree is an attach target: no `--from`, and a `path` for
    /// `confirm_fork` to open the group in directly (no drop).
    #[test]
    fn fork_picker_attaches_to_existing_worktree() {
        let mut picker = sample_fork_picker();
        picker.move_selection(2);
        let entry = picker.selected_entry().expect("a selected entry");
        assert_eq!(entry.scope, ForkScope::Worktree);
        assert_eq!(entry.from, None);
        assert_eq!(entry.path.as_deref(), Some(std::path::Path::new("/repo/.worktrees/36647115")));
    }

    /// Selection moves within the filtered list and clamps at both ends.
    #[test]
    fn fork_picker_selection_clamps() {
        let mut picker = sample_fork_picker();
        picker.move_selection(-1);
        assert_eq!(picker.selected, 0, "cannot move above the first row");
        picker.move_selection(100);
        assert_eq!(picker.selected, picker.rows.len() - 1, "clamps at the last row");
        let entry = picker.selected_entry().expect("a selected entry");
        assert_eq!(entry.from.as_deref(), Some("origin/tw-term-features"));
    }
}
