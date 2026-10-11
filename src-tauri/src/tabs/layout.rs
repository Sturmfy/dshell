//! Window/webview geometry.
//!
//! One window holds three fixed regions, and **no two of them ever overlap**:
//!
//! ```text
//!  y = 0      ┌──────────────────────────────────────────────┐
//!             │ chrome webview — the tab strip (38 px)       │  drag region + window controls
//!  y = 38     ├──────────────────────────────────────────────┤
//!             │ panel webview — new tab / settings /         │  only while a panel is open
//!             │ onboarding (≤ 380 px)                        │
//!  y = 38+h   ├──────────────────────────────────────────────┤
//!             │ content webview — the active DSH page        │
//!             └──────────────────────────────────────────────┘
//! ```
//!
//! Keeping the strip on screen at all times is not cosmetic. The window is
//! frameless, so it can only be moved through a `data-tauri-drag-region`
//! element, and that element lives in the strip. A panel that covered the window
//! would make the window impossible to drag — and child webviews have no z-order
//! API, so "who is on top" must never be a question. Separate regions answer it
//! structurally instead of by stacking order.
//!
//! It also gives dshell's panels a title bar for free: the strip *is* the title
//! bar, so a panel does not need one of its own.

/// Height of the dshell tab strip, in logical pixels.
pub const TAB_BAR_HEIGHT: f64 = 38.0;
/// Height of the panel region while a panel is open. Content-sized on purpose:
/// it should read as a dropdown anchored under the tab strip, not as another
/// window.
pub const PANEL_HEIGHT: f64 = 380.0;
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

    /// True when the two rectangles share any pixel. The tests below use this to
    /// assert the layout invariant rather than trusting the arithmetic.
    pub fn overlaps(&self, other: &Bounds) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// What dshell's own webviews occupy, on top of the content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromeMode {
    /// Just the tab strip; content fills everything below.
    #[default]
    Strip,
    /// Tab strip plus the panel webview; content is parked.
    Panel,
}

impl ChromeMode {
    /// Everything below the tab strip that dshell owns. Content starts after
    /// this, so a content webview can never cover the strip or the panel.
    pub fn reserved_height(self, window_height: f64) -> f64 {
        match self {
            ChromeMode::Strip => TAB_BAR_HEIGHT,
            ChromeMode::Panel => (TAB_BAR_HEIGHT + PANEL_HEIGHT).min(window_height),
        }
    }
}

/// The tab strip. Present in every mode — this is the window's drag region.
pub fn chrome_bounds(window_width: f64) -> Bounds {
    Bounds::new(0.0, 0.0, window_width, TAB_BAR_HEIGHT)
}

/// The panel region, directly under the strip. `None` when no panel is open.
pub fn panel_bounds(window_width: f64, window_height: f64, mode: ChromeMode) -> Option<Bounds> {
    match mode {
        ChromeMode::Strip => None,
        ChromeMode::Panel => {
            let available = (window_height - TAB_BAR_HEIGHT).max(0.0);
            Some(Bounds::new(
                0.0,
                TAB_BAR_HEIGHT,
                window_width,
                PANEL_HEIGHT.min(available),
            ))
        }
    }
}

/// Where the active content webview goes.
pub fn content_bounds(window_width: f64, window_height: f64, mode: ChromeMode) -> Bounds {
    let reserved = mode.reserved_height(window_height);
    Bounds::new(
        0.0,
        reserved,
        window_width,
        (window_height - reserved).max(0.0),
    )
}

/// Where parked content webviews go.
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

    const W: f64 = 1280.0;
    const H: f64 = 820.0;

    #[test]
    fn strip_is_present_in_every_mode() {
        for mode in [ChromeMode::Strip, ChromeMode::Panel] {
            let chrome = chrome_bounds(W);
            assert_eq!(chrome.y, 0.0);
            assert_eq!(chrome.height, TAB_BAR_HEIGHT);
            assert!(!chrome.is_empty(), "{mode:?}");
        }
    }

    /// The invariant that keeps a frameless window draggable: whatever else is on
    /// screen, the strip is never covered.
    #[test]
    fn nothing_ever_overlaps_the_tab_strip() {
        let chrome = chrome_bounds(W);
        for mode in [ChromeMode::Strip, ChromeMode::Panel] {
            assert!(!chrome.overlaps(&content_bounds(W, H, mode)), "{mode:?}");
            if let Some(panel) = panel_bounds(W, H, mode) {
                assert!(!chrome.overlaps(&panel), "{mode:?}");
                assert!(!panel.overlaps(&content_bounds(W, H, mode)), "{mode:?}");
            }
        }
    }

    #[test]
    fn parked_views_are_far_out_of_sight() {
        let parked = parked_bounds(W);
        assert!(parked.x <= -2400.0);
        assert!(!parked.overlaps(&chrome_bounds(W)));
        assert!(!parked.overlaps(&content_bounds(W, H, ChromeMode::Panel)));
    }

    #[test]
    fn panel_takes_room_from_the_content_not_from_the_strip() {
        let panel = panel_bounds(W, H, ChromeMode::Panel).expect("panel mode has a panel");
        assert_eq!(panel.y, TAB_BAR_HEIGHT);
        assert_eq!(panel.height, PANEL_HEIGHT);
        let content = content_bounds(W, H, ChromeMode::Panel);
        assert_eq!(content.y, TAB_BAR_HEIGHT + PANEL_HEIGHT);
        assert_eq!(content.height, H - TAB_BAR_HEIGHT - PANEL_HEIGHT);
    }

    #[test]
    fn no_panel_bounds_when_no_panel_is_open() {
        assert!(panel_bounds(W, H, ChromeMode::Strip).is_none());
        assert_eq!(content_bounds(W, H, ChromeMode::Strip).y, TAB_BAR_HEIGHT);
    }

    #[test]
    fn tiny_windows_do_not_produce_negative_heights() {
        assert_eq!(content_bounds(100.0, 10.0, ChromeMode::Strip).height, 0.0);
        assert_eq!(content_bounds(100.0, 10.0, ChromeMode::Panel).height, 0.0);
        // A window shorter than strip+panel clamps instead of overflowing.
        let panel = panel_bounds(100.0, 100.0, ChromeMode::Panel).unwrap();
        assert_eq!(panel.height, 100.0 - TAB_BAR_HEIGHT);
    }
}
