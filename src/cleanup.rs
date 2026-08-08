//! Cleanup page: lists and deletes drop-managed git worktrees.
//!
//! This module owns all cleanup-specific data structures, state, helper
//! methods, and pure geometry functions.  Nothing here touches gpui or
//! spawns threads; side-effects live in `main.rs`.
//!
//! # External tool
//! `drop -d --json` (run from any non-repo directory, e.g. the home dir)
//! returns a JSON array of [`WorktreeInfo`] objects.  Deletion is done via
//! `drop rm <id>... --json`.

use std::collections::HashSet;

use serde::Deserialize;

use crate::workspace::LayoutRect;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// One git worktree managed by the `drop` CLI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    pub repo_root: String,
    pub id: String,
    /// Absent for detached worktrees.
    #[serde(default)]
    pub branch: Option<String>,
    pub head: String,
    pub dirty_count: u32,
    pub merged: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Epoch ms; drop derives this from file mtimes, so it can be fractional.
    pub last_activity_ms: f64,
    pub is_current: bool,
    pub pr: Option<PrInfo>,
}

/// Pull-request summary attached to a worktree.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrInfo {
    pub number: u32,
    pub state: String, // "open" | "draft" | "merged" | "closed"
    pub title: String,
}

// ---------------------------------------------------------------------------
// Cleanup state
// ---------------------------------------------------------------------------

/// Scan lifecycle.
#[derive(Debug, Clone)]
pub enum ScanState {
    /// A scan is in progress (thread is running).
    Scanning,
    /// Scan completed successfully.
    Ready(Vec<WorktreeInfo>),
    /// Scan failed; the string is the error message.
    Failed(String),
}

/// All mutable state for the Cleanup page.
#[derive(Debug, Default)]
pub struct Cleanup {
    /// Lifecycle of the background scan.
    pub scan: Option<ScanState>,
    /// Currently selected worktree ids.
    pub selected: HashSet<String>,
    /// `None` = show all repos; `Some(repo_root)` = filter to that repo.
    pub repo_filter: Option<String>,
    /// Vertical scroll position in visible-row units.
    pub scroll: usize,
}

impl Default for ScanState {
    fn default() -> Self {
        ScanState::Scanning
    }
}

/// Summary entry for one repo shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// The `repoRoot` value from the worktree data.
    pub root: String,
    /// Last path component of `root` (display name).
    pub display: String,
    /// Number of worktrees in this repo.
    pub count: usize,
}

impl Cleanup {
    /// Transition to Scanning state (clears selection/scroll).
    pub fn set_scanning(&mut self) {
        self.scan = Some(ScanState::Scanning);
        self.selected.clear();
        self.scroll = 0;
    }

    /// Transition to Ready state with the scanned worktrees.
    pub fn set_ready(&mut self, worktrees: Vec<WorktreeInfo>) {
        self.selected.retain(|id| worktrees.iter().any(|w| &w.id == id));
        self.scan = Some(ScanState::Ready(worktrees));
    }

    /// Transition to Failed state with an error message.
    pub fn set_failed(&mut self, message: String) {
        self.scan = Some(ScanState::Failed(message));
    }

    /// Sorted list of unique repos from the ready worktree list.
    /// Returns an empty vec when not in the `Ready` state.
    pub fn repos(&self) -> Vec<RepoEntry> {
        let worktrees = match &self.scan {
            Some(ScanState::Ready(v)) => v,
            _ => return Vec::new(),
        };
        let mut map: std::collections::BTreeMap<&str, usize> = Default::default();
        for w in worktrees {
            *map.entry(w.repo_root.as_str()).or_insert(0) += 1;
        }
        map.into_iter()
            .map(|(root, count)| {
                let display = root
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .last()
                    .unwrap_or(root)
                    .to_string();
                RepoEntry { root: root.to_string(), display, count }
            })
            .collect()
    }

    /// Worktrees visible under the current repo filter.
    pub fn visible(&self) -> Vec<&WorktreeInfo> {
        let worktrees = match &self.scan {
            Some(ScanState::Ready(v)) => v,
            _ => return Vec::new(),
        };
        worktrees
            .iter()
            .filter(|w| match &self.repo_filter {
                None => true,
                Some(root) => &w.repo_root == root,
            })
            .collect()
    }

    /// Toggle selection for `id` (no-op if `isCurrent`).
    pub fn toggle(&mut self, id: &str) {
        // Guard: never select isCurrent rows.
        if let Some(ScanState::Ready(v)) = &self.scan {
            if v.iter().any(|w| w.id == id && w.is_current) {
                return;
            }
        }
        if self.selected.contains(id) {
            self.selected.remove(id);
        } else {
            self.selected.insert(id.to_string());
        }
    }

    /// Select every visible row that is not `isCurrent`.
    pub fn select_all_visible(&mut self) {
        let ids: Vec<String> = self
            .visible()
            .into_iter()
            .filter(|w| !w.is_current)
            .map(|w| w.id.clone())
            .collect();
        for id in ids {
            self.selected.insert(id);
        }
    }

    /// Select every visible row that is merged, clean (`dirtyCount == 0`),
    /// and not the currently checked-out worktree.
    pub fn select_merged_visible(&mut self) {
        let ids: Vec<String> = self
            .visible()
            .into_iter()
            .filter(|w| w.merged && w.dirty_count == 0 && !w.is_current)
            .map(|w| w.id.clone())
            .collect();
        for id in ids {
            self.selected.insert(id);
        }
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Every selected worktree grouped by repo, for `drop rm` — which resolves
    /// ids only within one repo, so deletion runs once per repo root.
    pub fn selected_by_repo(&self) -> Vec<(String, Vec<String>)> {
        let worktrees = match &self.scan {
            Some(ScanState::Ready(v)) => v,
            _ => return Vec::new(),
        };
        let mut map: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
        for w in worktrees {
            if self.selected.contains(&w.id) {
                map.entry(w.repo_root.as_str()).or_default().push(w.id.clone());
            }
        }
        map.into_iter().map(|(root, ids)| (root.to_string(), ids)).collect()
    }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

/// Format a millisecond age into a compact human-readable string.
///
/// | age       | display   |
/// |-----------|-----------|
/// | < 60 s    | "now"     |
/// | < 60 min  | "5m"      |
/// | < 24 h    | "3h"      |
/// | ≥ 24 h    | "2d"      |
pub fn format_age(now_ms: u64, last_activity_ms: u64) -> String {
    let delta = now_ms.saturating_sub(last_activity_ms);
    let secs = delta / 1000;
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Dirty count display: "clean" when 0, else "3±".
pub fn format_dirty(dirty_count: u32) -> String {
    if dirty_count == 0 {
        "clean".into()
    } else {
        format!("{}±", dirty_count)
    }
}

/// Parity display (ahead/behind): "—" when both zero, else "↓B ↑A".
pub fn format_parity(ahead: u32, behind: u32) -> String {
    if ahead == 0 && behind == 0 {
        "—".into()
    } else {
        format!("↓{} ↑{}", behind, ahead)
    }
}

/// PR display: "—" when absent, "#12 merged" / "#12 open" / etc.
pub fn format_pr(pr: Option<&PrInfo>) -> String {
    match pr {
        None => "—".into(),
        Some(p) => format!("#{} {}", p.number, p.state),
    }
}

/// Branch display: the branch name, or the short head for detached worktrees.
pub fn format_branch(w: &WorktreeInfo) -> String {
    match &w.branch {
        Some(b) => b.clone(),
        None => format!("@{}", &w.head[..w.head.len().min(8)]),
    }
}

// ---------------------------------------------------------------------------
// Table geometry (pure, shared by renderer and hit-testing)
// ---------------------------------------------------------------------------

/// Vertical rhythm constants (logical px, multiplied by scale at call sites).
pub const CLEANUP_HEADER_H: f32 = 52.0;
pub const CLEANUP_COL_HEADER_H: f32 = 28.0;
pub const CLEANUP_ROW_H: f32 = 32.0;
pub const CLEANUP_FOOTER_H: f32 = 44.0;
pub const CLEANUP_CARD_PAD: f32 = 14.0;

/// Column x-offsets inside the card (logical px fractions of card width).
/// Caller must multiply by scale after resolving card width.
/// Layout: [checkbox | branch | id | dirty | parity | pr | age]
pub struct ColumnOffsets {
    pub branch: f32,
    pub id: f32,
    pub dirty: f32,
    pub parity: f32,
    pub pr: f32,
    pub age: f32,
}

/// Compute column x-offsets in physical px given the card rect and scale.
pub fn column_offsets(card: &LayoutRect, scale: f32) -> ColumnOffsets {
    let pad = (CLEANUP_CARD_PAD * scale).round();
    let checkbox_w = (20.0 * scale).round();
    let x = card.x + pad;
    // Proportional column widths relative to usable width.
    let usable = (card.w - 2.0 * pad).max(0.0);
    // branch 28%, id 18%, dirty 10%, parity 14%, pr 18%, age 12%
    let branch_w = (usable * 0.28).round();
    let id_w = (usable * 0.18).round();
    let dirty_w = (usable * 0.10).round();
    let parity_w = (usable * 0.14).round();
    let pr_w = (usable * 0.18).round();
    ColumnOffsets {
        branch: x + checkbox_w + (6.0 * scale).round(),
        id: x + checkbox_w + (6.0 * scale).round() + branch_w,
        dirty: x + checkbox_w + (6.0 * scale).round() + branch_w + id_w,
        parity: x + checkbox_w + (6.0 * scale).round() + branch_w + id_w + dirty_w,
        pr: x + checkbox_w + (6.0 * scale).round() + branch_w + id_w + dirty_w + parity_w,
        age: x + checkbox_w + (6.0 * scale).round() + branch_w + id_w + dirty_w + parity_w + pr_w,
    }
}

/// The header row of the cleanup card (title + refresh button).
pub fn header_rect(card: &LayoutRect, scale: f32) -> LayoutRect {
    let h = (CLEANUP_HEADER_H * scale).round();
    LayoutRect { x: card.x, y: card.y, w: card.w, h }
}

/// The column-header (dim label) row below the card header.
pub fn col_header_rect(card: &LayoutRect, scale: f32) -> LayoutRect {
    let header_h = (CLEANUP_HEADER_H * scale).round();
    let h = (CLEANUP_COL_HEADER_H * scale).round();
    LayoutRect { x: card.x, y: card.y + header_h, w: card.w, h }
}

/// The first y-coordinate of data rows (physical px).
fn rows_origin_y(card: &LayoutRect, scale: f32) -> f32 {
    let header_h = (CLEANUP_HEADER_H * scale).round();
    let col_h = (CLEANUP_COL_HEADER_H * scale).round();
    card.y + header_h + col_h
}

/// The y-coordinate of the footer.
fn footer_origin_y(card: &LayoutRect, scale: f32) -> f32 {
    let footer_h = (CLEANUP_FOOTER_H * scale).round();
    card.y + card.h - footer_h
}

/// How many data rows fit between the column-header band and the footer.
pub fn rows_that_fit(card: &LayoutRect, scale: f32) -> usize {
    let available = (footer_origin_y(card, scale) - rows_origin_y(card, scale)).max(0.0);
    let row_h = (CLEANUP_ROW_H * scale).round();
    if row_h <= 0.0 {
        return 0;
    }
    (available / row_h).floor() as usize
}

/// Row rect for the visible row at index `vis_idx` (0 = first on screen),
/// honoring `scroll_offset`.  Returns `None` if the row would overflow the
/// card.
pub fn row_rect(card: &LayoutRect, vis_idx: usize, scale: f32) -> Option<LayoutRect> {
    let pad = (CLEANUP_CARD_PAD * scale).round();
    let row_h = (CLEANUP_ROW_H * scale).round();
    let origin_y = rows_origin_y(card, scale);
    let y = origin_y + vis_idx as f32 * row_h;
    if y + row_h > footer_origin_y(card, scale) {
        return None;
    }
    Some(LayoutRect { x: card.x + pad, y, w: (card.w - 2.0 * pad).max(0.0), h: row_h })
}

/// The checkbox sub-rect inside the given data row.
pub fn checkbox_rect(row: &LayoutRect, scale: f32) -> LayoutRect {
    let size = (16.0 * scale).round();
    let mid_y = (row.y + (row.h - size) / 2.0).round();
    LayoutRect { x: row.x, y: mid_y, w: size, h: size }
}

/// The "Delete N selected" button rect, right-aligned in the footer.
pub fn delete_button_rect(card: &LayoutRect, scale: f32, cell_width: f32) -> LayoutRect {
    let pad = (CLEANUP_CARD_PAD * scale).round();
    let footer_y = footer_origin_y(card, scale);
    let footer_h = (CLEANUP_FOOTER_H * scale).round();
    let btn_w = (22.0 * cell_width).round(); // fits "Delete 99 selected"
    let btn_h = (28.0 * scale).round();
    let x = card.x + card.w - pad - btn_w;
    let y = (footer_y + (footer_h - btn_h) / 2.0).round();
    LayoutRect { x, y, w: btn_w, h: btn_h }
}

/// The "Refresh" button rect, right-aligned in the card header.
pub fn refresh_button_rect(card: &LayoutRect, scale: f32, cell_width: f32) -> LayoutRect {
    let pad = (CLEANUP_CARD_PAD * scale).round();
    let header_h = (CLEANUP_HEADER_H * scale).round();
    let btn_w = (9.0 * cell_width).round(); // fits "Refresh"
    let btn_h = (28.0 * scale).round();
    let x = card.x + card.w - pad - btn_w;
    let y = (card.y + (header_h - btn_h) / 2.0).round();
    LayoutRect { x, y, w: btn_w, h: btn_h }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // JSON parsing
    // -----------------------------------------------------------------------

    const SAMPLE_JSON: &str = r#"[
        {
            "repoRoot": "/home/user/src/alpha",
            "path": "/home/user/src/alpha/.worktrees/feat-login",
            "id": "wt-1",
            "branch": "feat/login",
            "head": "abc1234",
            "detached": false,
            "dirtyCount": 0,
            "merged": true,
            "ahead": 0,
            "behind": 0,
            "baseRef": "main",
            "lastActivity": "2024-01-10T12:00:00Z",
            "lastActivityMs": 1704888000000,
            "isCurrent": false,
            "pr": {
                "number": 42,
                "state": "merged",
                "url": "https://github.com/example/alpha/pull/42",
                "title": "Add login"
            }
        },
        {
            "repoRoot": "/home/user/src/alpha",
            "path": "/home/user/src/alpha/.worktrees/feat-logout",
            "id": "wt-2",
            "branch": "feat/logout",
            "head": "def5678",
            "detached": false,
            "dirtyCount": 3,
            "merged": false,
            "ahead": 1,
            "behind": 2,
            "baseRef": "main",
            "lastActivity": "2024-01-11T09:00:00Z",
            "lastActivityMs": 1704963600000,
            "isCurrent": true,
            "pr": null
        },
        {
            "repoRoot": "/home/user/src/beta",
            "path": "/home/user/src/beta/.worktrees/fix-bug",
            "id": "wt-3",
            "head": "aabbccddeeff",
            "detached": true,
            "dirtyCount": 0,
            "merged": true,
            "ahead": 0,
            "behind": 0,
            "lastActivity": "2024-01-09T06:00:00Z",
            "lastActivityMs": 1704780000123.4567,
            "isCurrent": false
        }
    ]"#;

    fn sample_worktrees() -> Vec<WorktreeInfo> {
        serde_json::from_str(SAMPLE_JSON).expect("valid JSON")
    }

    #[test]
    fn test_json_parsing_with_pr() {
        let wts = sample_worktrees();
        assert_eq!(wts.len(), 3);
        let wt = &wts[0];
        assert_eq!(wt.id, "wt-1");
        assert_eq!(wt.branch.as_deref(), Some("feat/login"));
        assert!(wt.merged);
        assert_eq!(wt.dirty_count, 0);
        assert!(!wt.is_current);
        let pr = wt.pr.as_ref().expect("should have pr");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.state, "merged");
        assert_eq!(pr.title, "Add login");
    }

    #[test]
    fn test_json_parsing_without_pr() {
        let wts = sample_worktrees();
        // wt-2 has pr: null, wt-3 has no pr field at all
        assert!(wts[1].pr.is_none());
        assert!(wts[2].pr.is_none());
    }

    #[test]
    fn test_json_parsing_is_current() {
        let wts = sample_worktrees();
        assert!(wts[1].is_current);
        assert!(!wts[0].is_current);
    }

    #[test]
    fn test_json_parsing_detached_and_fractional_ms() {
        // wt-3: no branch/baseRef fields, detached, fractional lastActivityMs
        // (drop derives activity from file mtimes, which are sub-millisecond).
        let wts = sample_worktrees();
        let wt = &wts[2];
        assert!(wt.branch.is_none());
        assert!((wt.last_activity_ms - 1704780000123.4567).abs() < 1e-3);
        assert_eq!(format_branch(wt), "@aabbccdd");
    }

    // -----------------------------------------------------------------------
    // repos() grouping
    // -----------------------------------------------------------------------

    fn make_cleanup(wts: Vec<WorktreeInfo>) -> Cleanup {
        Cleanup {
            scan: Some(ScanState::Ready(wts)),
            selected: HashSet::new(),
            repo_filter: None,
            scroll: 0,
        }
    }

    #[test]
    fn test_repos_grouping() {
        let c = make_cleanup(sample_worktrees());
        let repos = c.repos();
        assert_eq!(repos.len(), 2);
        // BTreeMap => sorted by root: alpha before beta
        assert_eq!(repos[0].root, "/home/user/src/alpha");
        assert_eq!(repos[0].display, "alpha");
        assert_eq!(repos[0].count, 2);
        assert_eq!(repos[1].root, "/home/user/src/beta");
        assert_eq!(repos[1].display, "beta");
        assert_eq!(repos[1].count, 1);
    }

    #[test]
    fn test_repos_empty_when_scanning() {
        let c = Cleanup { scan: Some(ScanState::Scanning), ..Default::default() };
        assert!(c.repos().is_empty());
    }

    // -----------------------------------------------------------------------
    // visible() filtering
    // -----------------------------------------------------------------------

    #[test]
    fn test_visible_all() {
        let c = make_cleanup(sample_worktrees());
        assert_eq!(c.visible().len(), 3);
    }

    #[test]
    fn test_visible_filter_by_repo() {
        let mut c = make_cleanup(sample_worktrees());
        c.repo_filter = Some("/home/user/src/alpha".to_string());
        let vis = c.visible();
        assert_eq!(vis.len(), 2);
        assert!(vis.iter().all(|w| w.repo_root == "/home/user/src/alpha"));
    }

    #[test]
    fn test_visible_filter_other_repo() {
        let mut c = make_cleanup(sample_worktrees());
        c.repo_filter = Some("/home/user/src/beta".to_string());
        assert_eq!(c.visible().len(), 1);
        assert_eq!(c.visible()[0].id, "wt-3");
    }

    // -----------------------------------------------------------------------
    // Selection rules
    // -----------------------------------------------------------------------

    #[test]
    fn test_toggle_selects_and_deselects() {
        let mut c = make_cleanup(sample_worktrees());
        c.toggle("wt-1");
        assert!(c.selected.contains("wt-1"));
        c.toggle("wt-1");
        assert!(!c.selected.contains("wt-1"));
    }

    #[test]
    fn test_toggle_ignores_is_current() {
        let mut c = make_cleanup(sample_worktrees());
        // wt-2 is isCurrent
        c.toggle("wt-2");
        assert!(!c.selected.contains("wt-2"));
    }

    #[test]
    fn test_select_all_visible_excludes_current() {
        let mut c = make_cleanup(sample_worktrees());
        c.select_all_visible();
        assert!(c.selected.contains("wt-1"));
        assert!(!c.selected.contains("wt-2")); // isCurrent
        assert!(c.selected.contains("wt-3"));
    }

    #[test]
    fn test_select_merged_visible_criteria() {
        let mut c = make_cleanup(sample_worktrees());
        c.select_merged_visible();
        // wt-1: merged=true, dirty=0, not current => selected
        assert!(c.selected.contains("wt-1"));
        // wt-2: is_current => excluded
        assert!(!c.selected.contains("wt-2"));
        // wt-3: merged=true, dirty=0, not current => selected
        assert!(c.selected.contains("wt-3"));
    }

    #[test]
    fn test_selected_by_repo_groups() {
        let mut c = make_cleanup(sample_worktrees());
        c.toggle("wt-1");
        c.toggle("wt-3");
        let groups = c.selected_by_repo();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], ("/home/user/src/alpha".to_string(), vec!["wt-1".to_string()]));
        assert_eq!(groups[1], ("/home/user/src/beta".to_string(), vec!["wt-3".to_string()]));
    }

    #[test]
    fn test_select_merged_excludes_dirty() {
        // Build a worktree that is merged but dirty
        let mut wt = sample_worktrees().remove(0); // merged, clean
        wt.id = "wt-dirty".to_string();
        wt.dirty_count = 1;
        let c = make_cleanup(vec![wt]);
        let mut c = c;
        c.select_merged_visible();
        assert!(!c.selected.contains("wt-dirty"));
    }

    // -----------------------------------------------------------------------
    // Age formatting
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_age_now() {
        assert_eq!(format_age(1000, 1000), "now");
        assert_eq!(format_age(1000 + 59_000, 1000), "now"); // 59 seconds
    }

    #[test]
    fn test_format_age_minutes() {
        let base: u64 = 0;
        assert_eq!(format_age(base + 5 * 60 * 1000, base), "5m");
        assert_eq!(format_age(base + 59 * 60 * 1000, base), "59m");
    }

    #[test]
    fn test_format_age_hours() {
        let base: u64 = 0;
        assert_eq!(format_age(base + 3 * 3600 * 1000, base), "3h");
        assert_eq!(format_age(base + 23 * 3600 * 1000, base), "23h");
    }

    #[test]
    fn test_format_age_days() {
        let base: u64 = 0;
        assert_eq!(format_age(base + 2 * 86400 * 1000, base), "2d");
        assert_eq!(format_age(base + 30 * 86400 * 1000, base), "30d");
    }

    #[test]
    fn test_format_age_boundary_exact_minute() {
        // exactly 60 seconds = 1m, not "now"
        assert_eq!(format_age(60_000, 0), "1m");
    }

    #[test]
    fn test_format_age_future_is_now() {
        // last_activity_ms > now_ms => saturating_sub => 0 => "now"
        assert_eq!(format_age(0, 9999), "now");
    }

    // -----------------------------------------------------------------------
    // Geometry
    // -----------------------------------------------------------------------

    fn test_card() -> LayoutRect {
        LayoutRect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 }
    }

    #[test]
    fn test_header_rect_height() {
        let card = test_card();
        let h = header_rect(&card, 1.0);
        assert_eq!(h.h, CLEANUP_HEADER_H);
        assert_eq!(h.x, card.x);
        assert_eq!(h.y, card.y);
    }

    #[test]
    fn test_col_header_below_header() {
        let card = test_card();
        let ch = col_header_rect(&card, 1.0);
        let h = header_rect(&card, 1.0);
        assert_eq!(ch.y, h.y + h.h);
        assert_eq!(ch.h, CLEANUP_COL_HEADER_H);
    }

    #[test]
    fn test_rows_do_not_overlap() {
        let card = test_card();
        let scale = 1.0;
        let n = rows_that_fit(&card, scale);
        assert!(n > 0, "should fit at least one row");
        for i in 0..n {
            let r0 = row_rect(&card, i, scale).expect("should be Some for rows that fit");
            if i + 1 < n {
                let r1 = row_rect(&card, i + 1, scale).expect("row i+1 should fit");
                // rows touch but do not overlap: r0.y + r0.h == r1.y
                assert_eq!(r0.y + r0.h, r1.y, "rows {i} and {} overlap", i + 1);
            }
        }
    }

    #[test]
    fn test_row_rect_none_beyond_fit() {
        let card = test_card();
        let n = rows_that_fit(&card, 1.0);
        // One past the last fitting row should be None
        assert!(row_rect(&card, n, 1.0).is_none());
    }

    #[test]
    fn test_row_rect_scroll_offset_shifts_y() {
        // The scroll offset is applied by the renderer (it passes vis_idx =
        // logical_idx - scroll), so row_rect(card, 0) always produces the
        // top-most visible position.  Verify two consecutive calls differ by
        // exactly CLEANUP_ROW_H * scale.
        let card = test_card();
        let r0 = row_rect(&card, 0, 1.0).unwrap();
        let r1 = row_rect(&card, 1, 1.0).unwrap();
        assert_eq!(r1.y - r0.y, CLEANUP_ROW_H);
    }

    #[test]
    fn test_footer_inside_card() {
        let card = test_card();
        let footer_y = footer_origin_y(&card, 1.0);
        assert!(footer_y >= card.y);
        assert!(footer_y + CLEANUP_FOOTER_H <= card.y + card.h + 0.5); // within card
    }

    #[test]
    fn test_delete_button_inside_card() {
        let card = test_card();
        let btn = delete_button_rect(&card, 1.0, 8.0);
        assert!(btn.x >= card.x);
        assert!(btn.x + btn.w <= card.x + card.w + 0.5);
    }

    #[test]
    fn test_checkbox_rect_inside_row() {
        let card = test_card();
        let row = row_rect(&card, 0, 1.0).unwrap();
        let cb = checkbox_rect(&row, 1.0);
        assert!(cb.y >= row.y);
        assert!(cb.y + cb.h <= row.y + row.h + 0.5);
        assert!(cb.x >= row.x);
    }
}
