//! Command palette model: fuzzy action search and selection.
//!
//! Opening the command palette (cmd-p) lists every [`Action`] except
//! [`Action::CommandPalette`] itself. The list is filtered by a case-insensitive
//! fuzzy subsequence match on each action's [`Action::label()`].

use crate::pages::Action;

/// Returns `true` when every character of `query` appears in `label` in order
/// (case-insensitive). An empty query always matches.
fn fuzzy_match(label: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let label_lower = label.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut label_chars = label_lower.chars();
    'outer: for qc in query_lower.chars() {
        for lc in label_chars.by_ref() {
            if lc == qc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// The open command palette: query text, selection, and the visible action rows.
///
/// [`rows`] is recomputed on every mutation. With an empty query every action
/// is listed; with a query only those whose label passes [`fuzzy_match`] survive.
/// [`Action::CommandPalette`] is never included.
#[derive(Clone, Debug)]
pub struct Palette {
    /// The current search text.
    pub query: String,
    /// Index into [`Palette::rows`] of the highlighted row.
    pub selected: usize,
    /// The filtered list currently on screen.
    pub rows: Vec<Action>,
}

impl Palette {
    /// Creates a new palette with an empty query (shows all actions).
    pub fn new() -> Self {
        let mut palette = Self { query: String::new(), selected: 0, rows: Vec::new() };
        palette.rebuild();
        palette
    }

    /// Replaces the whole query (the search field owns editing) and
    /// refilters from the top of the list.
    pub fn set_query(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query = query.to_string();
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

    /// The highlighted action, if the list is not empty.
    pub fn selected_action(&self) -> Option<Action> {
        self.rows.get(self.selected).copied()
    }

    /// Recomputes [`Palette::rows`] from the query (fuzzy subsequence match on
    /// the label; empty query shows all actions except [`Action::CommandPalette`]).
    fn rebuild(&mut self) {
        let query = self.query.trim().to_string();
        self.rows = Action::ALL
            .iter()
            .copied()
            .filter(|a| !matches!(a, Action::CommandPalette))
            .filter(|a| fuzzy_match(a.label(), &query))
            .collect();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty query shows every action except CommandPalette itself.
    #[test]
    fn empty_query_lists_all_actions_except_command_palette() {
        let palette = Palette::new();
        // ALL has 18 entries; palette excludes CommandPalette → 17
        assert_eq!(palette.rows.len(), Action::ALL.len() - 1);
        assert!(!palette.rows.contains(&Action::CommandPalette));
    }

    /// Replacing the query refilters and resets the highlight to the top.
    #[test]
    fn set_query_refilters_and_resets_selection() {
        let mut palette = Palette::new();
        palette.move_selection(3);
        palette.set_query("split");
        assert_eq!(palette.selected, 0);
        assert!(!palette.rows.is_empty());
        assert!(palette.rows.iter().all(|a| fuzzy_match(a.label(), "split")));
        palette.set_query("");
        assert_eq!(palette.rows.len(), Action::ALL.len() - 1);
    }

    /// Fuzzy subsequence: query chars must appear in order in the label.
    #[test]
    fn fuzzy_match_subsequence() {
        // 's', 'p', 'l' all appear in order in "Split side-by-side"
        assert!(fuzzy_match("Split side-by-side", "spl"));
        // 'n', 't' appear in order in "New tab"
        assert!(fuzzy_match("New tab", "nt"));
        // 'xyz' does not appear as a subsequence in any label
        assert!(!fuzzy_match("Split side-by-side", "xyz"));
        assert!(!fuzzy_match("Command palette", "xyz"));
    }

    /// Empty query always matches.
    #[test]
    fn fuzzy_match_empty_query_always_matches() {
        assert!(fuzzy_match("anything", ""));
        assert!(fuzzy_match("", ""));
    }

    /// Filtering: only actions whose labels fuzzy-match the query appear.
    #[test]
    fn palette_filters_by_query() {
        let mut palette = Palette::new();
        // "spl" should match "Split side-by-side" and "Split top/bottom"
        palette.set_query("spl");
        assert!(!palette.rows.is_empty());
        for action in &palette.rows {
            assert!(
                fuzzy_match(action.label(), "spl"),
                "action {:?} label '{}' should match 'spl'",
                action,
                action.label()
            );
        }
        // 'splxyz' matches nothing
        palette.set_query("splxyz");
        assert!(palette.rows.is_empty());
    }

    /// Selection clamps when the filtered list shrinks.
    #[test]
    fn selection_clamps_when_list_shrinks() {
        let mut palette = Palette::new();
        // Move to the last row
        palette.move_selection(isize::MAX);
        let last = palette.selected;
        assert_eq!(last, palette.rows.len() - 1);

        // Now filter to a shorter list
        palette.set_query("spl");
        // selected must be within the new (shorter) rows
        assert!(palette.selected < palette.rows.len().max(1));
    }

    /// Clearing the query restores the full list.
    #[test]
    fn clearing_the_query_restores_rows() {
        let mut palette = Palette::new();
        let full_count = palette.rows.len();

        palette.set_query("spl");
        let filtered_count = palette.rows.len();
        assert!(filtered_count < full_count);

        palette.set_query("");
        assert_eq!(palette.rows.len(), full_count, "an empty query restores the full list");
    }

    /// selected_action returns the highlighted action.
    #[test]
    fn selected_action_returns_highlighted() {
        let mut palette = Palette::new();
        palette.move_selection(1);
        let action = palette.selected_action().expect("should have a selected action");
        assert_eq!(action, palette.rows[palette.selected]);
    }
}
