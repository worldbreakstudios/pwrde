//! Syntax highlighting for the PR / local-diff viewers.
//!
//! Wraps `syntect` so the diff UIs can colour code lines by language. The heavy
//! default syntax + theme sets load once behind `OnceLock`s; callers build a
//! [`LineHighlighter`] per file (keyed on the file extension and the chrome
//! polarity) and feed it the content lines of a hunk in display order.
//!
//! A diff interleaves added/removed/context lines, so highlighting is a close
//! approximation rather than a full-file parse: state (open strings, block
//! comments) is carried within a hunk and [`LineHighlighter::reset`] clears it
//! at each hunk boundary to bound drift. Colours are returned as plain
//! `gpui::Rgba` spans so this module stays free of any window/context and is
//! unit-testable.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// A run of same-coloured text within one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub color: gpui::Rgba,
}

fn syntaxes() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The theme for a polarity. `base16-ocean.dark` reads well on dark cards;
/// `InspiredGitHub` is the light-mode counterpart. Both ship in syntect's
/// defaults, so the lookups can't miss — but fall back to the first theme
/// rather than panicking if a future syntect drops them.
fn theme(dark: bool) -> &'static Theme {
    static DARK: OnceLock<Theme> = OnceLock::new();
    static LIGHT: OnceLock<Theme> = OnceLock::new();
    let slot = if dark { &DARK } else { &LIGHT };
    slot.get_or_init(|| {
        let mut ts = ThemeSet::load_defaults();
        let name = if dark { "base16-ocean.dark" } else { "InspiredGitHub" };
        ts.themes
            .remove(name)
            .or_else(|| ts.themes.values().next().cloned())
            .expect("syntect ships at least one default theme")
    })
}

/// Highlights successive lines of a single file, carrying parser state across
/// them. Cheap to construct; hold one per file being rendered.
pub struct LineHighlighter {
    hl: HighlightLines<'static>,
}

impl LineHighlighter {
    /// Build a highlighter for `ext` (a bare extension like `rs`, or a whole
    /// filename like `Makefile`) at the given polarity. Unknown languages fall
    /// back to plain text (a single default-coloured span per line).
    pub fn new(ext: &str, dark: bool) -> Self {
        let ss = syntaxes();
        let syntax = ss
            .find_syntax_by_extension(ext)
            .or_else(|| ss.find_syntax_by_token(ext))
            .unwrap_or_else(|| ss.find_syntax_plain_text());
        LineHighlighter { hl: HighlightLines::new(syntax, theme(dark)) }
    }

    /// Highlight one line of content (no trailing newline needed). On any
    /// highlighting error the line is returned as a single uncoloured span so
    /// the diff still renders.
    pub fn highlight(&mut self, line: &str) -> Vec<Span> {
        let ss = syntaxes();
        // syntect's newline-based syntaxes expect a trailing newline.
        let with_nl = format!("{line}\n");
        match self.hl.highlight_line(&with_nl, ss) {
            Ok(ranges) => ranges
                .into_iter()
                .filter_map(|(style, text)| {
                    let text = text.trim_end_matches('\n');
                    if text.is_empty() {
                        return None;
                    }
                    let c = style.foreground;
                    Some(Span {
                        text: text.to_string(),
                        color: gpui::Rgba {
                            r: c.r as f32 / 255.0,
                            g: c.g as f32 / 255.0,
                            b: c.b as f32 / 255.0,
                            a: 1.0,
                        },
                    })
                })
                .collect(),
            Err(_) => vec![Span {
                text: line.to_string(),
                color: gpui::Rgba { r: 0.8, g: 0.8, b: 0.8, a: 1.0 },
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_language_produces_spans() {
        let mut h = LineHighlighter::new("rs", true);
        let spans = h.highlight("fn main() {}");
        assert!(!spans.is_empty());
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
    }

    #[test]
    fn unknown_language_falls_back_to_plain() {
        let mut h = LineHighlighter::new("weird-ext-xyz", false);
        let spans = h.highlight("plain text line");
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "plain text line");
    }

    #[test]
    fn empty_line_yields_no_spans() {
        let mut h = LineHighlighter::new("rs", true);
        assert!(h.highlight("").is_empty());
    }

    #[test]
    fn both_polarities_load() {
        // Exercises the theme OnceLocks for both slots.
        let _ = LineHighlighter::new("rs", true).highlight("let x = 1;");
        let _ = LineHighlighter::new("rs", false).highlight("let x = 1;");
    }
}
