//! Window/webview geometry. Kept apart from the manager so the layout can be
//! reasoned about (and tested) without touching any Tauri API.
//!
//! There is exactly one webview above the content: the *chrome* webview. It is
//! usually just the 38px tab strip, but dshell's own panels (new tab, settings,
//! onboarding) grow it into the window. Growing one region instead of overlaying
//! a second window is what keeps this app free of z-order trouble: child
//! webviews have no z-order API, so "what is on top" must never be a guess.

/// Height of the tab strip, in logical pixels.
pub const TAB_BAR_HEIGHT: f64 = 38.0;
/// Chrome height while a panel is open. Modest on purpose: it must read as a
/// dropdown anchored under the tab strip, not as a separate window.
pub const PANEL_BAR_HEIGHT: f64 = 336.0;
/// Extra logical pixels kept to the left of the window when a webview is parked
/// out of sight. Generous so no multi-monitor geometry can slide it back in.
pub const OFFSCREEN_FACTOR: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// What the chrome webview currently occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromeMode {
    /// Just the tab strip; content fills everything below.
    #[default]
    Strip,
    /// Tab strip plus a panel; content is parked.
    Panel,
    /// The whole window (onboarding).
    Full,
}

pub fn chrome_bounds(window_width: f64, window_height: f64, mode: ChromeMode) -> Bounds {
    let height = match mode {
        ChromeMode::Strip => TAB_BAR_HEIGHT,
        ChromeMode::Panel => PANEL_BAR_HEIGHT.min(window_height),
        ChromeMode::Full => window_height,
    };
    Bounds::new(0.0, 0.0, window_width, height)
}

/// Where the active content webview goes. The content webview must **never**
/// overlap the tab strip: the strip carries the drag region, and a child webview
/// on top of it would steal both the drag and the window-control hit testing.
pub fn content_bounds(window_width: f64, window_height: f64, chrome_height: f64) -> Bounds {
    Bounds::new(
        0.0,
        chrome_height,
        window_width,
        (window_height - chrome_height).max(0.0),
    )
}

/// Where parked (inactive or panel-covered) content webviews go.
///
/// Not `hide()` and not a zero size: a hidden webview is a candidate for
/// background throttling, and DSH is a long-lived SSE session where a stalled
/// stream means a stuck approval prompt. Moving it out of the visible area keeps
/// the page alive and makes switching instant.
pub fn parked_bounds(window_width: f64) -> Bounds {
    Bounds::new(-OFFSCREEN_FACTOR * window_width.max(800.0), 0.0, 1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_never_overlaps_the_tab_strip() {
        let chrome = chrome_bounds(1280.0, 820.0, ChromeMode::Strip);
        assert_eq!(chrome.height, TAB_BAR_HEIGHT);
        let content = content_bounds(1280.0, 820.0, chrome.height);
        assert_eq!(content.y, TAB_BAR_HEIGHT);
        assert_eq!(content.height, 820.0 - TAB_BAR_HEIGHT);
    }

    #[test]
    fn panels_take_room_from_the_content_not_from_the_strip() {
        let chrome = chrome_bounds(1280.0, 820.0, ChromeMode::Panel);
        assert_eq!(chrome.height, PANEL_BAR_HEIGHT);
        let content = content_bounds(1280.0, 820.0, chrome.height);
        assert_eq!(content.y, PANEL_BAR_HEIGHT);
        assert!(content.height > 0.0);
    }

    #[test]
    fn tiny_windows_do_not_produce_negative_heights() {
        assert_eq!(content_bounds(100.0, 10.0, TAB_BAR_HEIGHT).height, 0.0);
        assert_eq!(chrome_bounds(100.0, 10.0, ChromeMode::Panel).height, 10.0);
    }

    #[test]
    fn parked_views_are_far_out_of_sight() {
        let parked = parked_bounds(1280.0);
        assert!(parked.x <= -2400.0);
        assert!(parked.x + parked.width < 0.0);
    }
}
