//! Link detection over terminal lines, shared by the renderer (accent color +
//! underline) and ⌘-click handling (open in browser).
//!
//! Two sources:
//! - Explicit OSC 8 hyperlinks (wezterm-term stores them per cell).
//! - Implicit `http(s)://` URLs found in the visible text. A URL broken
//!   across rows is detected — and opened — as one combined URL in two
//!   cases: the grid hard-wrapped the line (`last_cell_was_wrapped`), or
//!   the program wrapped its own output with a real newline — recognized
//!   when the fragment runs into the last column and the next row picks it
//!   up after an optional continuation indent.
//!
//! A link spanning a wrap yields one `LinkHit` per row, all carrying the
//! full URL.

use termwiz::surface::Line;

pub struct LinkHit {
    pub row: usize,
    /// Inclusive column range within `row`.
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}

impl LinkHit {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        row == self.row && col >= self.start_col && col <= self.end_col
    }
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// Find every link across the visible lines, joining wrapped rows.
///
/// `cols` is the terminal width: a URL whose fragment runs into the last
/// column of a row is treated as continuing onto the next row even without
/// the grid's wrap flag, since programs that wrap their own output emit real
/// newlines (often with a continuation indent, which is skipped).
pub fn links_in_lines(lines: &[Line], cols: usize) -> Vec<LinkHit> {
    let mut hits: Vec<LinkHit> = Vec::new();

    // ── OSC 8 explicit hyperlinks: merge consecutive cells per row ─────
    for (row, line) in lines.iter().enumerate() {
        for cell in line.visible_cells() {
            let col = cell.cell_index();
            if let Some(link) = cell.attrs().hyperlink() {
                let uri = link.uri();
                match hits.last_mut() {
                    Some(last)
                        if last.row == row && last.url == uri && col <= last.end_col + 2 =>
                    {
                        last.end_col = col.max(last.end_col)
                    },
                    _ => hits.push(LinkHit {
                        row,
                        start_col: col,
                        end_col: col,
                        url: uri.to_string(),
                    }),
                }
            }
        }
    }

    // ── Implicit URLs across rows ──────────────────────────────────────
    // One char stream over every visible cell, with (row, col) provenance.
    let mut chars: Vec<(usize, usize, char)> = Vec::new();
    for (r, line) in lines.iter().enumerate() {
        for cell in line.visible_cells() {
            if let Some(ch) = cell.str().chars().next() {
                chars.push((r, cell.cell_index(), ch));
            }
        }
    }
    let grid_wrapped: Vec<bool> = lines.iter().map(|l| l.last_cell_was_wrapped()).collect();

    let mut i = 0;
    while i < chars.len() {
        if chars[i].2 != 'h' {
            i += 1;
            continue;
        }
        // Walk forward over the URL charset, recording the indices taken.
        // Row transitions are allowed when the grid wrapped the line, or —
        // the soft-wrap case — when the fragment ran into the right edge
        // and the next row continues it after an optional indent (programs
        // that wrap their own output print real newlines, so the grid's
        // wrap flag never fires). Within a row, require near-adjacency.
        let mut taken: Vec<usize> = vec![i];
        let mut j = i + 1;
        while j < chars.len() {
            let (pr, pc, _) = chars[*taken.last().unwrap()];
            let (cr, cc, c) = chars[j];
            if cr == pr {
                if !is_url_char(c) || cc > pc + 2 {
                    break;
                }
                taken.push(j);
            } else if cr == pr + 1 && grid_wrapped[pr] {
                if !is_url_char(c) {
                    break;
                }
                taken.push(j);
            } else if cr == pr + 1 && cols > 0 && pc + 2 >= cols {
                // Soft wrap: skip the continuation indent, keep the URL.
                while j < chars.len() && chars[j].0 == cr && chars[j].2 == ' ' {
                    j += 1;
                }
                if j < chars.len() && chars[j].0 == cr && is_url_char(chars[j].2) {
                    taken.push(j);
                } else {
                    break;
                }
            } else {
                break;
            }
            j += 1;
        }
        let mut url: String = taken.iter().map(|&k| chars[k].2).collect();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            i += 1;
            continue;
        }
        // Trailing punctuation is almost never part of the URL.
        while let Some(last) = url.chars().last() {
            if ".,;:!?'\")]".contains(last) && url.len() > 8 && taken.len() > 1 {
                url.pop();
                taken.pop();
            } else {
                break;
            }
        }
        // Skip if OSC 8 already covers the start.
        let (r0, c0, _) = chars[taken[0]];
        if !hits.iter().any(|h| h.contains(r0, c0)) {
            // Emit one hit per row the URL touches. Skipped indent is not
            // in `taken`, so it stays outside the highlight/hit ranges.
            let mut k = 0;
            while k < taken.len() {
                let (seg_row, start_col, _) = chars[taken[k]];
                let mut end_col = start_col;
                while k + 1 < taken.len() && chars[taken[k + 1]].0 == seg_row {
                    k += 1;
                    end_col = chars[taken[k]].1;
                }
                hits.push(LinkHit { row: seg_row, start_col, end_col, url: url.clone() });
                k += 1;
            }
        }
        i = j;
    }

    hits.sort_by_key(|h| (h.row, h.start_col));
    hits
}

/// Return the URL of the first `LinkHit` that contains `(row, col)`, or
/// `None` if no hit covers that cell.
pub fn hovered_url<'a>(hits: &'a [LinkHit], row: usize, col: usize) -> Option<&'a str> {
    hits.iter().find(|h| h.contains(row, col)).map(|h| h.url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::cell::CellAttributes;

    fn line(s: &str, wrapped: bool) -> Line {
        let mut l = Line::from_text(s, &CellAttributes::default(), 1, None);
        l.set_last_cell_was_wrapped(wrapped, 1);
        l
    }

    #[test]
    fn plain_url_on_one_line() {
        let lines = [line("see https://example.com/x for info", false)];
        let hits = links_in_lines(&lines, 80);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/x");
        assert_eq!((hits[0].start_col, hits[0].end_col), (4, 24));
    }

    #[test]
    fn wrapped_url_is_combined() {
        // URL broken across a hard wrap: "https://example.com/aaa" + "bbb/ccc"
        let lines = [line("go https://example.com/aaa", true), line("bbb/ccc now", false)];
        let hits = links_in_lines(&lines, 80);
        // One hit per row, both carrying the combined URL.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/aaabbb/ccc");
        assert_eq!(hits[1].url, "https://example.com/aaabbb/ccc");
        assert_eq!(hits[0].row, 0);
        assert_eq!(hits[1].row, 1);
        assert_eq!((hits[1].start_col, hits[1].end_col), (0, 6));
    }

    #[test]
    fn unwrapped_lines_stay_separate() {
        // Same text but *no* wrap flag: the second row is a new logical line.
        let lines = [line("go https://example.com/aaa", false), line("bbb/ccc now", false)];
        let hits = links_in_lines(&lines, 80);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/aaa");
    }

    #[test]
    fn soft_wrapped_url_joined_across_hard_newline() {
        // The program wrapped its own output: the fragment runs into the
        // right edge (cols == line length) and the next row continues it
        // after a two-space continuation indent (the Claude Code pattern).
        let a = "go https://example.co";
        let lines = [line(a, false), line("  m/path and more", false)];
        let hits = links_in_lines(&lines, a.chars().count());
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/path");
        assert_eq!(hits[1].url, "https://example.com/path");
        // The indent is not part of the hit range on the continuation row.
        assert_eq!((hits[1].start_col, hits[1].end_col), (2, 7));
    }

    #[test]
    fn url_short_of_the_edge_is_not_joined() {
        // Hard newline and the URL stops well before the right edge: the
        // next row is prose, not a continuation.
        let lines = [line("see https://example.com", false), line("and more text", false)];
        let hits = links_in_lines(&lines, 80);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com");
    }

    #[test]
    fn soft_wrap_chains_across_multiple_rows() {
        // 12-col terminal: the URL fills rows 0 and 1 completely and ends
        // mid-row 2, with no wrap flags anywhere.
        let lines = [
            line("https://ab.c", false),
            line("d/efghijklmn", false),
            line("op end", false),
        ];
        let hits = links_in_lines(&lines, 12);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|h| h.url == "https://ab.cd/efghijklmnop"));
        assert_eq!((hits[2].start_col, hits[2].end_col), (0, 1));
    }

    #[test]
    fn trailing_punctuation_trimmed() {
        let lines = [line("(https://example.com/x).", false)];
        let hits = links_in_lines(&lines, 80);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/x");
    }

    #[test]
    fn hovered_url_hit_and_miss() {
        // "see https://example.com/x for info"
        //  col  4..24
        let lines = [line("see https://example.com/x for info", false)];
        let hits = links_in_lines(&lines, 80);

        // Cell inside the URL.
        assert_eq!(hovered_url(&hits, 0, 4), Some("https://example.com/x"));
        assert_eq!(hovered_url(&hits, 0, 14), Some("https://example.com/x"));
        assert_eq!(hovered_url(&hits, 0, 24), Some("https://example.com/x"));

        // Cell outside the URL.
        assert_eq!(hovered_url(&hits, 0, 3), None);
        assert_eq!(hovered_url(&hits, 0, 25), None);

        // Wrong row.
        assert_eq!(hovered_url(&hits, 1, 10), None);
    }
}

/// Open `url` the way the platform does: `open` on macOS, a new tab on wasm32.
pub fn open(url: &str) {
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_family = "wasm")]
    if let Some(window) = web_sys::window() {
        let _ = window.open_with_url_and_target(url, "_blank");
    }
}
