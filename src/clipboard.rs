//! System clipboard behind one seam. Native goes through `arboard`; wasm32
//! writes to the browser's async clipboard and keeps the last copied text in
//! the page, since the read side of that API is asynchronous and gated on a
//! permission prompt — an in-page clipboard is what a browser test can rely
//! on, and ⌘C/⌘V still round-trip inside the app.

/// What a paste can hand to a terminal.
pub enum Contents {
    /// Non-empty text.
    Text(String),
    /// An image and no text — the terminal gets ^V so the program inside can
    /// pull the image itself.
    Image,
    Empty,
}

/// Put `text` on the system clipboard. Failures are silent, as before.
pub fn set_text(text: impl Into<String>) {
    let text = text.into();
    #[cfg(not(target_family = "wasm"))]
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
    #[cfg(target_family = "wasm")]
    {
        // Fire and forget: the promise resolves (or is refused) on its own.
        if let Some(window) = web_sys::window() {
            let _ = window.navigator().clipboard().write_text(&text);
        }
        LAST.with(|last| *last.borrow_mut() = Some(text));
    }
}

#[cfg(target_family = "wasm")]
thread_local! {
    /// The page's own copy of the last text copied, for the synchronous read.
    static LAST: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Read the clipboard: text wins over an image; anything else is `Empty`.
pub fn contents() -> Contents {
    #[cfg(not(target_family = "wasm"))]
    {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return Contents::Empty };
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => Contents::Text(text),
            _ if clipboard.get_image().is_ok() => Contents::Image,
            _ => Contents::Empty,
        }
    }
    #[cfg(target_family = "wasm")]
    LAST.with(|last| match last.borrow().as_deref() {
        Some(text) if !text.is_empty() => Contents::Text(text.to_string()),
        _ => Contents::Empty,
    })
}
