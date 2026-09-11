//! Main-thread lifecycle for native Wry child views.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use gpui::Window;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{PageLoadEvent, Rect, WebView, WebViewBuilder};

use crate::term::TermEvent;
use crate::workspace::LayoutRect;

/// Browser chrome is GPUI-owned; the native child starts below it.
pub const TOOLBAR_H: f32 = 42.0;
pub const SITE_PANEL_H: f32 = 116.0;
pub const TOOLS_PANEL_H: f32 = 250.0;

/// Safari's own user agent for the native child views. WKWebView's default
/// carries no `Version/` or `Safari/` product token, so sites like Google
/// treat it as an unknown legacy browser and serve their fallback layouts.
/// It must stay a *Safari* string rather than a Chrome one: the engine really
/// is WebKit, and Google's sign-in refuses ("This browser or app may not be
/// secure") when the advertised browser and the engine's fingerprint disagree.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15";

pub fn normalize_input(value: &str) -> Result<String, String> {
    let value = value.trim();
    let candidate = if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    crate::bus::validate_webview_url(&candidate)
}

/// Convert the tile content rect (physical pixels) into the child-view rect,
/// reserving logical pixels for the always-visible toolbar and optional panel.
pub fn child_bounds(content: LayoutRect, scale: f32, panel_h: f32) -> LayoutRect {
    let reserve = ((TOOLBAR_H + panel_h.max(0.0)) * scale).min((content.h - 1.0).max(0.0));
    LayoutRect {
        x: content.x,
        y: content.y + reserve,
        w: content.w.max(1.0),
        h: (content.h - reserve).max(1.0),
    }
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub id: u64,
    pub url: String,
    pub bounds: LayoutRect,
}

struct Entry {
    view: WebView,
    bounds: LayoutRect,
    visible: bool,
    zoom: f64,
}

#[derive(Clone, Debug)]
pub struct BrowserState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom_percent: u16,
}

#[derive(Default)]
pub struct Manager {
    entries: HashMap<u64, Entry>,
    failed: HashSet<u64>,
    focused: Option<u64>,
}

impl Manager {
    /// Reconcile native views with the tab model. `live` includes webviews in
    /// every group; `placements` contains only views visible in this frame.
    pub fn sync(
        &mut self,
        window: &Window,
        live: &HashSet<u64>,
        placements: &[Placement],
        focus: Option<u64>,
        events: &Sender<TermEvent>,
    ) -> Option<String> {
        let mut first_error = None;
        let visible: HashSet<u64> = placements.iter().map(|p| p.id).collect();
        self.entries.retain(|id, entry| {
            if live.contains(id) {
                true
            } else {
                let _ = entry.view.focus_parent();
                false
            }
        });
        if self
            .focused
            .is_some_and(|id| !self.entries.contains_key(&id))
        {
            self.focused = None;
        }
        // Suppress repeated failures while a tab stays visible, but retry after
        // the user switches away and revisits it.
        self.failed
            .retain(|id| live.contains(id) && visible.contains(id));

        for (id, entry) in &mut self.entries {
            if !visible.contains(id) && entry.visible {
                let _ = entry.view.focus_parent();
                let _ = entry.view.set_visible(false);
                entry.visible = false;
            }
        }

        for placement in placements {
            if let Some(entry) = self.entries.get_mut(&placement.id) {
                if entry.bounds != placement.bounds {
                    let _ = entry.view.set_bounds(wry_rect(&placement.bounds));
                    entry.bounds = placement.bounds;
                }
                if !entry.visible {
                    let _ = entry.view.set_visible(true);
                    entry.visible = true;
                }
            } else if !self.failed.contains(&placement.id) {
                let id = placement.id;
                let load_events = events.clone();
                let focus_events = events.clone();
                let title_events = events.clone();
                match WebViewBuilder::new()
                    .with_url(&placement.url)
                    .with_user_agent(USER_AGENT)
                    .with_bounds(wry_rect(&placement.bounds))
                    .with_visible(true)
                    .with_devtools(true)
                    .with_initialization_script(
                        "addEventListener('pointerdown',()=>window.ipc.postMessage('pwrde:webview-focus'),true)",
                    )
                    .with_ipc_handler(move |request| {
                        if request.body() == "pwrde:webview-focus" {
                            let _ = focus_events.send(TermEvent::WebviewFocused { id });
                        }
                    })
                    .with_on_page_load_handler(move |event, url| {
                        if matches!(event, PageLoadEvent::Finished) {
                            let _ = load_events.send(TermEvent::WebviewNavigated { id, url });
                        }
                    })
                    .with_document_title_changed_handler(move |title| {
                        let _ = title_events.send(TermEvent::WebviewTitleChanged { id, title });
                    })
                    .build_as_child(window)
                {
                    Ok(view) => {
                        self.entries.insert(
                            placement.id,
                            Entry {
                                view,
                                bounds: placement.bounds,
                                visible: true,
                                zoom: 1.0,
                            },
                        );
                    }
                    Err(error) => {
                        let message = format!("{}: {error}", placement.url);
                        eprintln!("webview: failed to create {message}");
                        first_error.get_or_insert(message);
                        self.failed.insert(placement.id);
                    }
                }
            }
        }

        let focus = focus.filter(|id| visible.contains(id));
        if focus != self.focused {
            if let Some(previous) = self.focused.and_then(|id| self.entries.get(&id)) {
                let _ = previous.view.focus_parent();
            }
            if let Some(entry) = focus.and_then(|id| self.entries.get(&id)) {
                let _ = entry.view.focus();
            }
            self.focused = focus;
        }
        first_error
    }

    pub fn state(&self, id: u64) -> Option<BrowserState> {
        let entry = self.entries.get(&id)?;
        Some(BrowserState {
            can_go_back: entry.view.can_go_back().unwrap_or(false),
            can_go_forward: entry.view.can_go_forward().unwrap_or(false),
            zoom_percent: (entry.zoom * 100.0).round() as u16,
        })
    }

    fn entry(&self, id: u64) -> Result<&Entry, String> {
        self.entries
            .get(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    fn entry_mut(&mut self, id: u64) -> Result<&mut Entry, String> {
        self.entries
            .get_mut(&id)
            .ok_or_else(|| "webview is not ready yet".into())
    }

    pub fn navigate(&self, id: u64, url: &str) -> Result<(), String> {
        self.entry(id)?
            .view
            .load_url(url)
            .map_err(|e| format!("navigate: {e}"))
    }

    pub fn reload(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .reload()
            .map_err(|e| format!("reload: {e}"))
    }

    pub fn go_back(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .go_back()
            .map_err(|e| format!("back: {e}"))
    }

    pub fn go_forward(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .go_forward()
            .map_err(|e| format!("forward: {e}"))
    }

    pub fn set_zoom(&mut self, id: u64, zoom: f64) -> Result<u16, String> {
        let entry = self.entry_mut(id)?;
        let zoom = zoom.clamp(0.5, 3.0);
        entry.view.zoom(zoom).map_err(|e| format!("zoom: {e}"))?;
        entry.zoom = zoom;
        Ok((zoom * 100.0).round() as u16)
    }

    pub fn zoom(&mut self, id: u64, delta: f64) -> Result<u16, String> {
        let current = self.entry(id)?.zoom;
        self.set_zoom(id, current + delta)
    }

    pub fn print(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .print()
            .map_err(|e| format!("print: {e}"))
    }

    pub fn open_devtools(&self, id: u64) -> Result<(), String> {
        self.entry(id)?.view.open_devtools();
        Ok(())
    }

    pub fn cookie_count(&self, id: u64, url: &str) -> Result<usize, String> {
        self.entry(id)?
            .view
            .cookies_for_url(url)
            .map(|cookies| cookies.len())
            .map_err(|e| format!("cookies: {e}"))
    }

    pub fn clear_browsing_data(&self, id: u64) -> Result<(), String> {
        self.entry(id)?
            .view
            .clear_all_browsing_data()
            .map_err(|e| format!("clear browsing data: {e}"))
    }

    pub fn find(&self, id: u64, query: &str) -> Result<(), String> {
        let query = serde_json::to_string(query).map_err(|e| e.to_string())?;
        self.entry(id)?
            .view
            .evaluate_script(&format!("window.find({query}, false, false, true)"))
            .map_err(|e| format!("find in page: {e}"))
    }

    pub fn focus(&self, id: u64) {
        if let Some(entry) = self.entries.get(&id) {
            let _ = entry.view.focus();
        }
    }
}

fn wry_rect(rect: &LayoutRect) -> Rect {
    Rect {
        position: PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32).into(),
        size: PhysicalSize::new(
            rect.w.max(1.0).round() as u32,
            rect.h.max(1.0).round() as u32,
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wry_bounds_round_and_never_collapse_to_zero() {
        let rect = wry_rect(&LayoutRect {
            x: 10.6,
            y: 20.4,
            w: 0.0,
            h: 30.6,
        });
        assert_eq!(rect.position, PhysicalPosition::new(11, 20).into());
        assert_eq!(rect.size, PhysicalSize::new(1, 31).into());
    }

    #[test]
    fn child_bounds_reserve_scaled_browser_chrome() {
        let content = LayoutRect {
            x: 10.0,
            y: 20.0,
            w: 500.0,
            h: 400.0,
        };
        assert_eq!(
            child_bounds(content, 2.0, SITE_PANEL_H),
            LayoutRect {
                x: 10.0,
                y: 336.0,
                w: 500.0,
                h: 84.0
            }
        );
        let tiny = child_bounds(content, 2.0, 10_000.0);
        assert_eq!(tiny.y, 419.0);
        assert_eq!(tiny.h, 1.0);
    }

    #[test]
    fn user_agent_is_safari_not_chrome() {
        // Google's sign-in refuses a Chrome UA on a WebKit engine, while the
        // WKWebView default (no Version/ or Safari/ token) gets legacy layouts.
        assert!(USER_AGENT.contains(" Version/"));
        assert!(USER_AGENT.contains(" Safari/"));
        assert!(USER_AGENT.contains("AppleWebKit/605"));
        assert!(!USER_AGENT.contains("Chrome/"));
    }

    #[test]
    fn browser_input_accepts_hosts_and_rejects_other_schemes() {
        assert_eq!(
            normalize_input(" example.com/docs ").unwrap(),
            "https://example.com/docs"
        );
        assert_eq!(
            normalize_input("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert!(normalize_input("file:///tmp/nope").is_err());
    }
}
