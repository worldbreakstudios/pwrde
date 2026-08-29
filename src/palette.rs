//! Fuzzy subsequence matching for the command palette's root filter (see
//! `command.rs`, which owns the palette model itself).

/// Returns `true` when every character of `query` appears in `label` in order
/// (case-insensitive). An empty query always matches.
pub(crate) fn fuzzy_match(label: &str, query: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fuzzy_match_empty_query_always_matches() {
        assert!(fuzzy_match("anything", ""));
        assert!(fuzzy_match("", ""));
    }
}
