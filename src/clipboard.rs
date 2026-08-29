//! System clipboard behind one seam. Native goes through `arboard`; the wasm32
//! build (which has no `arboard`) compiles to no-ops until the web entry point
//! routes these through the browser's async clipboard API.

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
    #[cfg(not(target_family = "wasm"))]
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.into());
    }
    #[cfg(target_family = "wasm")]
    let _ = text;
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
    Contents::Empty
}
