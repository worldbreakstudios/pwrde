//! Link detection over terminal lines, shared by the renderer (accent color +
//! underline) and ⌘-click handling (open in browser).
//!
//! Two sources:
//! - Explicit OSC 8 hyperlinks (wezterm-term stores them per cell).
//! - Implicit `http(s)://` URLs found in the visible text. Physical lines
//!   that hard-wrapped (`last_cell_was_wrapped`) are joined into one logical
//!   line first, so a URL broken across rows is detected — and opened — as
//!   one combined URL.
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
pub fn links_in_lines(lines: &[Line]) -> Vec<LinkHit> {
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

    // ── Implicit URLs over *logical* lines (wrap-joined) ───────────────
    let mut row = 0;
    while row < lines.len() {
        // Collect the wrapped run starting at `row`.
        let mut end_row = row;
        while end_row + 1 < lines.len() && lines[end_row].last_cell_was_wrapped() {
            end_row += 1;
        }

        // Char stream with (row, col) provenance across the logical line.
        let mut chars: Vec<(usize, usize, char)> = Vec::new();
        for (r, line) in lines.iter().enumerate().take(end_row + 1).skip(row) {
            for cell in line.visible_cells() {
                if let Some(ch) = cell.str().chars().next() {
                    chars.push((r, cell.cell_index(), ch));
                }
            }
        }

        let mut i = 0;
        while i < chars.len() {
            let starts_scheme = {
                let s: String =
                    chars[i..chars.len().min(i + 8)].iter().map(|(_, _, c)| *c).collect();
                s.starts_with("https://") || s.starts_with("http://")
            };
            if !starts_scheme {
                i += 1;
                continue;
            }
            // Extend over URL charset; row transitions are fine (that's the
            // wrap), within a row require near-adjacent columns.
            let mut j = i + 1;
            while j < chars.len() && is_url_char(chars[j].2) {
                let (pr, pc, _) = chars[j - 1];
                let (cr, cc, _) = chars[j];
                if cr == pr && cc > pc + 2 {
                    break;
                }
                j += 1;
            }
            let mut url: String = chars[i..j].iter().map(|(_, _, c)| *c).collect();
            let mut end = j - 1;
            // Trailing punctuation is almost never part of the URL.
            while let Some(last) = url.chars().last() {
                if ".,;:!?'\")]".contains(last) && url.len() > 8 && end > i {
                    url.pop();
                    end -= 1;
                } else {
                    break;
                }
            }
            // Skip if OSC 8 already covers the start.
            let (r0, c0, _) = chars[i];
            if !hits.iter().any(|h| h.contains(r0, c0)) {
                // Emit one hit per row the URL touches.
                let mut k = i;
                while k <= end {
                    let seg_row = chars[k].0;
                    let start_col = chars[k].1;
                    let mut m = k;
                    while m + 1 <= end && chars[m + 1].0 == seg_row {
                        m += 1;
                    }
                    hits.push(LinkHit {
                        row: seg_row,
                        start_col,
                        end_col: chars[m].1,
                        url: url.clone(),
                    });
                    k = m + 1;
                }
            }
            i = j;
        }

        row = end_row + 1;
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
        let hits = links_in_lines(&lines);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/x");
        assert_eq!((hits[0].start_col, hits[0].end_col), (4, 24));
    }

    #[test]
    fn wrapped_url_is_combined() {
        // URL broken across a hard wrap: "https://example.com/aaa" + "bbb/ccc"
        let lines = [line("go https://example.com/aaa", true), line("bbb/ccc now", false)];
        let hits = links_in_lines(&lines);
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
        let hits = links_in_lines(&lines);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/aaa");
    }

    #[test]
    fn trailing_punctuation_trimmed() {
        let lines = [line("(https://example.com/x).", false)];
        let hits = links_in_lines(&lines);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/x");
    }

    #[test]
    fn hovered_url_hit_and_miss() {
        // "see https://example.com/x for info"
        //  col  4..24
        let lines = [line("see https://example.com/x for info", false)];
        let hits = links_in_lines(&lines);

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
