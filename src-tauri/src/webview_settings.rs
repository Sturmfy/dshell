//! Host-side WebView2 policy for webviews that are not DSH content tabs.
//!
//! Content tabs keep the browser context menu (with a few items filtered in
//! `bridge`); dshell's own chrome and page tabs have no use for it, so their
//! default menu is disabled. The dshell tab menu is a Tauri native menu and is
//! not affected by this setting.

use tauri::Webview;

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
    });
}
