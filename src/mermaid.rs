//! Mermaid diagram rendering via the external `mmdc` (mermaid-cli), with a
//! content-addressed disk cache.
//!
//! gpui can't paint arbitrary SVG (only monochrome icon `svg()`), and there's no
//! Rust crate that renders mermaid, so we shell out to `mmdc` to produce a PNG
//! and display it with `gpui::img`. Rendering is expensive (it drives headless
//! Chromium), so each diagram is keyed by a hash of its source + polarity and
//! cached on disk at `<data_dir>/pwrde/mermaid/<key>.png` — an unchanged diagram
//! is served straight from cache, and identical diagrams across PRs share one
//! render. Renders run off the UI thread; when one finishes it nudges a repaint
//! through [`TermEvent::Redraw`]. When `mmdc` is absent or a render fails the
//! state is [`Render::Unavailable`] and the caller shows a code-block fallback.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use crate::term::TermEvent;

/// The render state of one mermaid diagram.
#[derive(Clone, Debug)]
pub enum Render {
    /// A render is in flight; show a placeholder.
    Pending,
    /// The rendered PNG is on disk at this path.
    Ready(PathBuf),
    /// `mmdc` is missing or the render failed; show the source as a code block.
    /// Carries a short reason (spawn error or captured stderr) so the fallback
    /// can surface *why* — turning a silent black box into something diagnosable.
    Unavailable(String),
}

static TX: OnceLock<Sender<TermEvent>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<u64, Render>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Render>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Wire up the redraw channel once at startup so finished renders can nudge a
/// repaint. Safe to call once; later calls are ignored.
pub fn init(tx: Sender<TermEvent>) {
    let _ = TX.set(tx);
}

/// Content + polarity hash — the cache key and on-disk filename stem.
pub fn key(source: &str, dark: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    dark.hash(&mut h);
    h.finish()
}

fn cache_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("pwrde").join("mermaid"))
}

fn cache_path(key: u64) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{key:016x}.png")))
}

/// Ensure a render exists for `source` at the given polarity, kicking one off
/// off-thread on a miss, and return its cache key. Cheap and idempotent — a hit
/// (in memory or on disk) does no work — so it's fine to call from `prepare`.
pub fn ensure(source: &str, dark: bool) -> u64 {
    let key = key(source, dark);
    {
        let map = cache().lock().unwrap();
        // A render in flight or done is left alone. A prior `Unavailable` is
        // NOT sticky: retry it on the next `ensure` (i.e. next PR load / SSE
        // refresh), so installing `mmdc` while the app runs heals the diagram
        // without a restart. `ensure` is only called off the render path, so a
        // genuinely-broken `mmdc` re-attempts per load, not per frame.
        match map.get(&key) {
            Some(Render::Pending) | Some(Render::Ready(_)) => return key,
            _ => {}
        }
    }
    // On-disk hit from a previous run (or another PR with the same diagram).
    if let Some(path) = cache_path(key).filter(|p| p.exists()) {
        cache().lock().unwrap().insert(key, Render::Ready(path));
        return key;
    }
    // Miss: mark pending and render off-thread.
    cache().lock().unwrap().insert(key, Render::Pending);
    let source = source.to_string();
    std::thread::spawn(move || {
        let result = render_with_mmdc(&source, dark, key);
        cache().lock().unwrap().insert(key, result);
        if let Some(tx) = TX.get() {
            let _ = tx.send(TermEvent::Redraw);
        }
    });
    key
}

/// The current render state for a key produced by [`ensure`].
pub fn state(key: u64) -> Render {
    cache()
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .unwrap_or_else(|| Render::Unavailable(String::new()))
}

/// Run `mmdc` to render `source` to the cache PNG. Returns `Unavailable` with a
/// short reason if the binary is missing or the render fails, so the caller
/// falls back gracefully and can surface why.
fn render_with_mmdc(source: &str, dark: bool, key: u64) -> Render {
    let unavailable = |reason: String| Render::Unavailable(reason);
    let Some(dir) = cache_dir() else {
        return unavailable("no data dir".into());
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return unavailable(format!("cache dir: {e}"));
    }
    let Some(out) = cache_path(key) else {
        return unavailable("no cache path".into());
    };
    let input = dir.join(format!("{key:016x}.mmd"));
    // Render to a temp file and rename into place only on success, so a crash
    // or kill mid-render never leaves a partial `<key>.png` that the disk-hit
    // path would later trust and render blank with no way to recover. The temp
    // name must still end in `.png` — `mmdc` validates the output extension and
    // rejects anything else (e.g. `.png.tmp`).
    let tmp = dir.join(format!("{key:016x}.tmp.png"));
    if let Err(e) = std::fs::write(&input, source) {
        return unavailable(format!("write input: {e}"));
    }

    // `mmdc` is resolved through the same augmented PATH the rest of the app
    // uses (adds ~/.bun/bin + homebrew), so a bun/npm global install is found.
    let theme = if dark { "dark" } else { "default" };
    let output = crate::git::augmented_command("mmdc")
        .args(["-i", &input.to_string_lossy(), "-o", &tmp.to_string_lossy()])
        .args(["-b", "transparent", "-t", theme])
        .stdin(std::process::Stdio::null())
        .output();
    let _ = std::fs::remove_file(&input);

    match output {
        Ok(o) if o.status.success() && tmp.exists() && std::fs::rename(&tmp, &out).is_ok() => {
            Render::Ready(out)
        }
        Ok(o) => {
            let _ = std::fs::remove_file(&tmp);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let reason = stderr.trim().lines().next_back().unwrap_or("").trim();
            let reason = if reason.is_empty() {
                format!("mmdc exited with {}", o.status)
            } else {
                reason.chars().take(200).collect()
            };
            unavailable(reason)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            unavailable(format!("could not run mmdc: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end pipeline check against a real `mmdc`. Ignored by default
    /// (needs the binary + is slow); run with `cargo test -- --ignored mermaid`.
    #[test]
    #[ignore]
    fn ensure_renders_a_real_diagram() {
        let key = ensure("sequenceDiagram\n  A->>B: hi\n  B-->>A: yo\n", true);
        for _ in 0..150 {
            match state(key) {
                Render::Pending => std::thread::sleep(std::time::Duration::from_millis(100)),
                Render::Ready(path) => {
                    assert!(path.exists(), "Ready but PNG missing");
                    assert!(std::fs::metadata(&path).unwrap().len() > 0, "empty PNG");
                    eprintln!("mermaid Ready: {}", path.display());
                    return;
                }
                Render::Unavailable(reason) => panic!("mermaid Unavailable: {reason}"),
            }
        }
        panic!("mermaid render did not settle within 15s");
    }
}
