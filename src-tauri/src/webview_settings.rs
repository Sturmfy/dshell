//! Host-side WebView2 policy for webviews that are not DSH content tabs.
//!
//! Content tabs keep the browser context menu (with a few items filtered in
//! `bridge`); dshell's own chrome and page tabs have no use for it, so their
//! default menu is disabled. The dshell tab menu is a Tauri native menu and is
//! not affected by this setting.
//!
//! This module also owns the native background colour. WebView2 paints its
//! default white surface before a document has rendered, and again while an
//! offscreen tab is resized back into view. On a dark theme that is a visible
//! white flash, especially when a page tab is created on demand (settings). The
//! document also gets an inline boot background so its own canvas is correct
//! before `styles.css` arrives; this colour covers the host surface behind it.

use tauri::{webview::Color, Theme, Webview, Window};

/// `--bg` in styles.css, light theme.
const LIGHT_BACKGROUND: Color = Color(255, 255, 255, 255);
/// `--bg` in styles.css, dark theme.
const DARK_BACKGROUND: Color = Color(21, 21, 23, 255);

/// The native surface colour for a theme.
pub fn background_for_theme(theme: Theme) -> Color {
    match theme {
        Theme::Dark => DARK_BACKGROUND,
        _ => LIGHT_BACKGROUND,
    }
}

/// The native surface colour for a resolved dark/light mode.
pub fn background_for_mode(dark: bool) -> Color {
    background_for_theme(if dark { Theme::Dark } else { Theme::Light })
}

/// The native surface colour for the window's current system theme.
pub fn background_for_window(window: &Window) -> Color {
    background_for_theme(window.theme().unwrap_or(Theme::Light))
}

pub fn disable_context_menu(webview: &Webview) {
    let _ = webview.with_webview(|platform| {
        #[cfg(windows)]
        unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            let Ok(settings) = core.Settings() else {
                return;
            };
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
        }
        // WebKit has no equivalent switch; its menu is left alone (documented
        // in `docs/platforms.md`).
        #[cfg(not(windows))]
        let _ = platform;
    });
}
