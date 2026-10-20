//! Window/webview geometry.
//!
//! One window holds two fixed regions, and they never overlap:
//!
//! ```text
//!  y = 0      ┌──────────────────────────────────────────────┐
//!             │ chrome webview — the tab strip (38 px)       │  drag region + window controls
//!  y = 38     ├──────────────────────────────────────────────┤
//!             │ content webview — the active tab's page      │  a DSH page, or one of
//!             │ (fills the rest of the window)               │  dshell's own pages
//!             └──────────────────────────────────────────────┘
//! ```
//!
//! Keeping the strip on screen at all times is not cosmetic. The window is
//! frameless, so it can only be moved through a `data-tauri-drag-region`
//! element, and that element lives in the strip. Child webviews have no z-order
//! API, so "who is on top" must never be a question — the two regions answer it
//! structurally instead of by stacking order.
//!
//! **There is no third region.** dshell's own pages (first-run wizard, new
//! connection, settings) are tabs like any other and are rendered by the content
//! webview, so focus, keyboard shortcuts, activation and closing follow one set
//! of rules rather than two.

/// Height of the dshell tab strip, in logical pixels.
pub const TAB_BAR_HEIGHT: f64 = 38.0;
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
        Self {
            x,
            y,
            width,
            height,
        }
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

/// The tab strip. Always present — this is the window's drag region.
pub fn chrome_bounds(window_width: f64) -> Bounds {
    Bounds::new(0.0, 0.0, window_width, TAB_BAR_HEIGHT)
}

/// Where the active content webview goes: everything below the strip.
pub fn content_bounds(window_width: f64, window_height: f64) -> Bounds {
    Bounds::new(
        0.0,
        TAB_BAR_HEIGHT,
        window_width,
        (window_height - TAB_BAR_HEIGHT).max(0.0),
    )
}

/// The narrowest, shortest parked viewport that keeps DSH's session rows mounted.
///
/// DSH auto-collapses its sidebar below a 1024px viewport, and the collapsed
/// rail does not render session rows at all. A 1×1 parked webview therefore
/// destroyed exactly the evidence a background tab has to report: a session
/// that finished, or one that started waiting for the user. Width must stay at
/// or above the breakpoint; height is kept at one pixel so an offscreen page
/// barely rasterizes while its React tree — and the rows this app observes —
/// stays mounted.
const PARKED_MIN_WIDTH: f64 = 1024.0;
const PARKED_MIN_HEIGHT: f64 = 1.0;

/// Where parked content webviews go.
///
/// Not `hide()` and not a zero size: a hidden webview is a candidate for
/// background throttling, and DSH is a long-lived SSE session where a stalled
/// stream means a stuck approval prompt. Moving it out of the visible area keeps
/// the page alive and makes switching instant. Width is deliberately at DSH's
/// sidebar breakpoint — see [`PARKED_MIN_WIDTH`].
pub fn parked_bounds(window_width: f64, _window_height: f64) -> Bounds {
    let width = PARKED_MIN_WIDTH;
    Bounds::new(
        -OFFSCREEN_FACTOR * window_width.max(800.0) - width,
        0.0,
        width,
        PARKED_MIN_HEIGHT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 1280.0;
    const H: f64 = 820.0;

    #[test]
    fn strip_sits_at_the_top_and_content_takes_the_rest() {
        let chrome = chrome_bounds(W);
        assert_eq!((chrome.x, chrome.y), (0.0, 0.0));
        assert_eq!(chrome.height, TAB_BAR_HEIGHT);
        assert_eq!(chrome.width, W);

        let content = content_bounds(W, H);
        assert_eq!(content.y, TAB_BAR_HEIGHT);
        assert_eq!(content.height, H - TAB_BAR_HEIGHT);
        assert_eq!(content.width, W);
    }

    /// The invariant that keeps a frameless window draggable: whatever else is on
    /// screen, the strip is never covered.
    #[test]
    fn nothing_ever_overlaps_the_tab_strip() {
        let chrome = chrome_bounds(W);
        assert!(!chrome.overlaps(&content_bounds(W, H)));
        assert!(!chrome.overlaps(&parked_bounds(W, H)));
    }

    #[test]
    fn parked_views_are_far_out_of_sight_and_stay_wide_enough_for_dsh() {
        let parked = parked_bounds(W, H);
        assert!(parked.x <= -2400.0);
        assert!(
            parked.width >= 1024.0,
            "DSH collapses its session rows below 1024px"
        );
        assert!(parked.height >= 1.0);
        assert!(!parked.overlaps(&chrome_bounds(W)));
        assert!(!parked.overlaps(&content_bounds(W, H)));
    }

    /// A page tab and a DSH tab share the content region exactly: there is only
    /// one kind of content area, which is what makes dshell's own pages ordinary
    /// tabs rather than a second layout region.
    #[test]
    fn there_is_exactly_one_content_region() {
        let content = content_bounds(W, H);
        assert_eq!(content.x, 0.0);
        assert_eq!(content.width, W);
        assert_eq!(content.y + content.height, H);
    }

    #[test]
    fn tiny_windows_do_not_produce_negative_heights() {
        assert_eq!(content_bounds(100.0, 10.0).height, 0.0);
        assert_eq!(content_bounds(100.0, TAB_BAR_HEIGHT).height, 0.0);
        assert_eq!(content_bounds(100.0, 0.0).height, 0.0);
    }
}
