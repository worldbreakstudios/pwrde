//! Experimental feature flags, persisted in settings as `features.<key>` bools
//! (default false). The Settings → Feature Flags section toggles them.

use crate::settings;

/// A single experimental feature toggle.
pub struct FeatureFlag {
    /// Settings sub-key; the persisted key is `features.<key>`.
    pub key: &'static str,
    /// Human-readable name shown in Settings.
    pub label: &'static str,
    /// One-line explanation shown under the label.
    pub description: &'static str,
}

/// Every experimental flag, in display order.
pub const ALL: &[FeatureFlag] = &[FeatureFlag {
    key: "notes",
    label: "Notes",
    description: "Obsidian-style markdown vaults page (experimental).",
}];

/// True when the flag `key` is switched on (defaults to off).
pub fn enabled(key: &str) -> bool {
    settings::get_bool(&format!("features.{key}"), false)
}

/// Convenience accessor for the Notes page flag.
pub fn notes_enabled() -> bool {
    enabled("notes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_non_empty() {
        assert!(!ALL.is_empty());
    }

    #[test]
    fn keys_are_unique() {
        let mut keys: Vec<&str> = ALL.iter().map(|f| f.key).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate feature flag key");
    }

    #[test]
    fn notes_flag_is_present() {
        assert!(ALL.iter().any(|f| f.key == "notes"));
    }
}
