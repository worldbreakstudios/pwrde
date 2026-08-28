//! Unified-diff model + parser, shared by the PR files view and the local
//! diff tool.
//!
//! Both `git diff` and `lfg pr diff <n>` emit the same unified-diff text, so a
//! single parser drives both tools. Everything a viewer needs — per-file
//! status (add/delete/rename/copy/modify), rename source, binary flag,
//! add/delete counts, and the hunk lines with old/new line-number gutters — is
//! recovered from that one stream, so callers never need a second `--numstat`
//! / `--name-status` pass.
//!
//! The parser is intentionally forgiving: unknown header lines are skipped, and
//! a malformed hunk header just resets the line counters rather than aborting
//! the file. Rendering (colors, syntax highlighting) lives in the UI modules;
//! this module is pure data so it can be unit-tested without gpui.

/// How a file changed between the two sides of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
    Renamed,
    Copied,
}

impl FileStatus {
    /// Single-letter tag shown in the file list (matches `git` porcelain).
    pub fn tag(self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Removed => "D",
            FileStatus::Modified => "M",
            FileStatus::Renamed => "R",
            FileStatus::Copied => "C",
        }
    }
}

/// The role of a single physical line inside a hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged line present on both sides.
    Context,
    /// Line added on the new side (`+`).
    Add,
    /// Line removed from the old side (`-`).
    Remove,
}

/// One rendered line of a hunk, with its content stripped of the leading
/// `+`/`-`/space marker and its old/new line numbers resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number on the old side, or `None` for added lines.
    pub old_no: Option<u32>,
    /// Line number on the new side, or `None` for removed lines.
    pub new_no: Option<u32>,
    /// Line content without the marker (and without the trailing newline).
    pub text: String,
}

/// A single `@@ … @@` hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    /// The raw `@@ -a,b +c,d @@ section` header line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// The function/scope heading git appends after the closing `@@`, if any
    /// (e.g. `impl App {` in `@@ -a,b +c,d @@ impl App {`). Empty when absent.
    // The hunk row currently renders the whole header line verbatim; this
    // accessor is used by the context-expansion UI.
    pub fn section(&self) -> &str {
        match self.header.rfind("@@") {
            Some(i) => self.header[i + 2..].trim(),
            None => "",
        }
    }
}

/// A single file's worth of diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    /// New-side path (or the surviving path for a delete).
    pub path: String,
    /// Old-side path for renames/copies.
    pub previous_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    /// New-side full file content (post-change), one entry per line, when it
    /// could be fetched. Used to expand elided context. `None` disables
    /// expansion for this file. Not part of the parsed diff — the producers
    /// fill it in.
    pub new_lines: Option<std::sync::Arc<Vec<String>>>,
}

impl DiffFile {
    /// The `old → new` label for renames, else just the path.
    pub fn display_path(&self) -> String {
        match &self.previous_path {
            Some(prev) if prev != &self.path => format!("{prev} → {}", self.path),
            _ => self.path.clone(),
        }
    }

    /// Language hint for syntax highlighting: the file extension (lowercased),
    /// falling back to the file name for extensionless files like `Makefile`.
    pub fn extension(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        match name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => ext.to_lowercase(),
            _ => name.to_lowercase(),
        }
    }
}

/// Strip a quoted/`a/`,`b/` prefixed path from a `diff --git` / `---`/`+++`
/// token into a plain repo-relative path. `/dev/null` becomes `None`.
fn clean_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw == "/dev/null" {
        return None;
    }
    // git quotes paths with unusual bytes: "a/weird name".
    let unquoted = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    let stripped = unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(unquoted);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// Parse the two paths out of a `diff --git a/x b/y` line. Returns
/// `(old, new)`. Paths with spaces are ambiguous in this line, so callers
/// should prefer the `---`/`+++` lines when present; this is the fallback and
/// the source of truth for pure renames (which carry no `+++`/`---`).
fn parse_diff_git_line(line: &str) -> (Option<String>, Option<String>) {
    let rest = match line.strip_prefix("diff --git ") {
        Some(r) => r,
        None => return (None, None),
    };
    // Split on " b/" — the only unambiguous separator when there are no spaces.
    if let Some(idx) = rest.find(" b/") {
        let old = clean_path(&rest[..idx]);
        let new = clean_path(&rest[idx + 1..]);
        (old, new)
    } else {
        // Fall back to whitespace split (no-space paths).
        let mut it = rest.split_whitespace();
        (it.next().and_then(clean_path), it.next().and_then(clean_path))
    }
}

/// Parse the old/new starting line numbers from a hunk header
/// `@@ -a,b +c,d @@`. Returns `(old_start, new_start)`, defaulting a missing
/// side to 1 (as git does for single-line ranges).
fn parse_hunk_header(header: &str) -> (u32, u32) {
    let mut old_start = 1;
    let mut new_start = 1;
    for tok in header.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('-') {
            old_start = rest.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(1);
        } else if let Some(rest) = tok.strip_prefix('+') {
            new_start = rest.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(1);
        }
    }
    (old_start, new_start)
}

/// Parse a full unified-diff blob into per-file diffs.
pub fn parse(diff: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;

    // Header state pending on the current file until a path resolves.
    let mut explicit_status: Option<FileStatus> = None;

    macro_rules! flush {
        () => {
            if let Some(f) = cur.take() {
                files.push(f);
            }
        };
    }

    for line in diff.lines() {
        if let Some(_rest) = line.strip_prefix("diff --git ") {
            flush!();
            let (old, new) = parse_diff_git_line(line);
            // A rename with no hunks still needs both paths; default status to
            // Modified until a `new file`/`deleted file`/`rename` line refines
            // it, and the path to whichever side we could read.
            let path = new.clone().or_else(|| old.clone()).unwrap_or_default();
            cur = Some(DiffFile {
                path,
                previous_path: old.filter(|o| Some(o) != new.as_ref()),
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
                binary: false,
                hunks: Vec::new(),
                new_lines: None,
            });
            explicit_status = None;
            old_no = 0;
            new_no = 0;
            continue;
        }

        let Some(f) = cur.as_mut() else { continue };

        if let Some(rest) = line.strip_prefix("rename to ") {
            if let Some(p) = clean_path(rest) {
                f.path = p;
            }
            explicit_status = Some(FileStatus::Renamed);
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            f.previous_path = clean_path(rest);
            explicit_status = Some(FileStatus::Renamed);
            continue;
        }
        if let Some(rest) = line.strip_prefix("copy to ") {
            if let Some(p) = clean_path(rest) {
                f.path = p;
            }
            explicit_status = Some(FileStatus::Copied);
            continue;
        }
        if let Some(rest) = line.strip_prefix("copy from ") {
            f.previous_path = clean_path(rest);
            explicit_status = Some(FileStatus::Copied);
            continue;
        }
        if line.starts_with("new file mode") {
            explicit_status = Some(FileStatus::Added);
            f.previous_path = None;
            continue;
        }
        if line.starts_with("deleted file mode") {
            explicit_status = Some(FileStatus::Removed);
            continue;
        }
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            f.binary = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            // `--- /dev/null` marks an add; otherwise the old path.
            if rest.trim() == "/dev/null" {
                if explicit_status.is_none() {
                    explicit_status = Some(FileStatus::Added);
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest.trim() == "/dev/null" {
                if explicit_status.is_none() {
                    explicit_status = Some(FileStatus::Removed);
                }
            } else if let Some(p) = clean_path(rest) {
                // Prefer the +++ path (handles spaces the `diff --git` line
                // can't), but don't clobber a rename's resolved target.
                if !matches!(explicit_status, Some(FileStatus::Renamed | FileStatus::Copied)) {
                    f.path = p;
                }
            }
            continue;
        }
        if line.starts_with("@@") {
            // Parse the numbers from only the `@@ … @@` prefix so a section
            // heading containing `-`/`+` tokens can't corrupt them, but keep
            // the whole line as the displayed header.
            let num_end = line[2..].find("@@").map(|i| i + 4).unwrap_or(line.len());
            let (o, n) = parse_hunk_header(&line[..num_end]);
            let header = line.to_string();
            old_no = o;
            new_no = n;
            if let Some(st) = explicit_status {
                f.status = st;
            }
            f.hunks.push(DiffHunk { header, lines: Vec::new() });
            continue;
        }

        // Body lines belong to the last hunk.
        let Some(hunk) = f.hunks.last_mut() else {
            // `index …`, `old mode`, `new mode`, and other headers we don't
            // model land here before the first hunk; ignore them.
            continue;
        };
        if let Some(text) = line.strip_prefix('+') {
            hunk.lines.push(DiffLine { kind: LineKind::Add, old_no: None, new_no: Some(new_no), text: text.to_string() });
            f.additions += 1;
            new_no += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            hunk.lines.push(DiffLine { kind: LineKind::Remove, old_no: Some(old_no), new_no: None, text: text.to_string() });
            f.deletions += 1;
            old_no += 1;
        } else if line == "\\ No newline at end of file" {
            // Marker; not a content line.
            continue;
        } else {
            let text = line.strip_prefix(' ').unwrap_or(line);
            hunk.lines.push(DiffLine {
                kind: LineKind::Context,
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: text.to_string(),
            });
            old_no += 1;
            new_no += 1;
        }
    }

    if let Some(mut f) = cur.take() {
        if let Some(st) = explicit_status {
            f.status = st;
        }
        files.push(f);
    }
    // Apply explicit status to files whose status was resolved only after the
    // last hunk started (e.g. binary add/delete with no `@@`).
    for f in &mut files {
        if f.binary && f.hunks.is_empty() {
            // status already set from headers; nothing to do.
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modified_file_counts_and_gutters() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
index 111..222 100644\n\
--- a/src/main.rs\n\
+++ b/src/main.rs\n\
@@ -1,3 +1,4 @@\n\
 fn main() {\n\
-    old();\n\
+    new();\n\
+    extra();\n\
 }\n";
        let files = parse(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "src/main.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.additions, 2);
        assert_eq!(f.deletions, 1);
        assert!(!f.binary);
        assert_eq!(f.hunks.len(), 1);
        let lines = &f.hunks[0].lines;
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!(lines[0].old_no, Some(1));
        assert_eq!(lines[0].new_no, Some(1));
        assert_eq!(lines[1].kind, LineKind::Remove);
        assert_eq!(lines[1].old_no, Some(2));
        assert_eq!(lines[1].new_no, None);
        assert_eq!(lines[2].kind, LineKind::Add);
        assert_eq!(lines[2].new_no, Some(2));
        assert_eq!(lines[3].new_no, Some(3));
    }

    #[test]
    fn detects_added_file() {
        let diff = "diff --git a/new.txt b/new.txt\n\
new file mode 100644\n\
index 000..abc\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1,2 @@\n\
+hello\n\
+world\n";
        let files = parse(diff);
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[0].deletions, 0);
        assert!(files[0].previous_path.is_none());
    }

    #[test]
    fn detects_deleted_file() {
        let diff = "diff --git a/gone.txt b/gone.txt\n\
deleted file mode 100644\n\
index abc..000\n\
--- a/gone.txt\n\
+++ /dev/null\n\
@@ -1,1 +0,0 @@\n\
-bye\n";
        let files = parse(diff);
        assert_eq!(files[0].status, FileStatus::Removed);
        assert_eq!(files[0].path, "gone.txt");
        assert_eq!(files[0].deletions, 1);
    }

    #[test]
    fn detects_pure_rename_with_no_hunks() {
        let diff = "diff --git a/old/name.rs b/new/name.rs\n\
similarity index 100%\n\
rename from old/name.rs\n\
rename to new/name.rs\n";
        let files = parse(diff);
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(files[0].path, "new/name.rs");
        assert_eq!(files[0].previous_path.as_deref(), Some("old/name.rs"));
        assert_eq!(files[0].display_path(), "old/name.rs → new/name.rs");
    }

    #[test]
    fn detects_binary_file() {
        let diff = "diff --git a/img.png b/img.png\n\
index 111..222 100644\n\
Binary files a/img.png and b/img.png differ\n";
        let files = parse(diff);
        assert!(files[0].binary);
        assert_eq!(files[0].path, "img.png");
    }

    #[test]
    fn parses_multiple_files() {
        let diff = "diff --git a/a.rs b/a.rs\n\
--- a/a.rs\n\
+++ b/a.rs\n\
@@ -1 +1 @@\n\
-a\n\
+b\n\
diff --git a/b.rs b/b.rs\n\
--- a/b.rs\n\
+++ b/b.rs\n\
@@ -1 +1 @@\n\
-c\n\
+d\n";
        let files = parse(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.rs");
        assert_eq!(files[1].path, "b.rs");
    }

    #[test]
    fn extension_falls_back_to_filename() {
        let mk = |p: &str| DiffFile {
            path: p.into(),
            previous_path: None,
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            binary: false,
            hunks: Vec::new(),
            new_lines: None,
        };
        assert_eq!(mk("src/main.rs").extension(), "rs");
        assert_eq!(mk("Makefile").extension(), "makefile");
        assert_eq!(mk("a/b/CHANGELOG.md").extension(), "md");
    }

    #[test]
    fn empty_diff_is_empty() {
        assert!(parse("").is_empty());
    }
}
