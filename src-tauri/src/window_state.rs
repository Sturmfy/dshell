//! Window geometry: remember where the window was, and put it back on the same
//! monitor next launch.
//!
//! Three decisions carry this module.
//!
//! **1. Geometry is state, not preference, so it lives in `window.json`.** A close
//! must be able to record it synchronously: `CloseRequested` runs on the main
//! thread, while `config.json` is only written through an `async` command holding
//! a tokio mutex. Waiting for that lock during a close could block the event loop
//! (or deadlock against a task that is itself waiting on the main thread), and a
//! mistake in a 300-byte rectangle must never be able to take the user's real
//! settings down with it. `tabs.json` already sets this precedent: state next to
//! the config, not inside it.
//!
//! **2. The record is physical pixels plus the identity of their monitor.**
//! Logical coordinates mean nothing without a scale factor, and dshell runs on
//! machines where two monitors disagree about DPI. The saved frame is the outer
//! (frame) top-left plus the client size, both physical — the window is
//! frameless, so the client area is what the user actually sees.
//!
//! **3. Only the restored frame is saved.** A maximized window reports the
//! maximized rectangle; storing that as the restore rect would make the next
//! "unmaximize" leave a full-screen window behind. [`Tracker`] keeps the last
//! normal frame and the maximized flag apart, and ignores minimized windows
//! altogether (Windows parks those at `-32000`).
//!
//! Restoring is a pure function ([`restore`]) over the saved record and the
//! monitors that exist *now*: match the monitor by device name (the Windows
//! `\\.\DISPLAYn` name survives re-arranging, reconnecting and resolution
//! changes), fall back to geometry, then to the primary; place the window at the
//! same offset from that monitor's origin; and clamp it into the work area when
//! the monitor's own rectangle changed — which is exactly what happens when it
//! was unplugged, rotated or resized — or when the record would put the window's
//! drag strip off the desktop.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Monitor, PhysicalPosition, PhysicalSize, Window};

/// Bumped only when a change cannot be expressed as a new optional field.
pub const SCHEMA_VERSION: u64 = 1;

/// Larger than any real display; anything beyond this is a corrupt file, not a
/// window, and must not be turned into one.
const MAX_DIMENSION: u32 = 16_384;
/// Same idea for coordinates: virtual desktops are big, but not this big.
const MAX_COORDINATE: u32 = 1_000_000;

/// A rectangle in virtual-desktop physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The window's frame: top-left plus client size.
    fn of_window(window: &Window) -> Option<Self> {
        let position = window.outer_position().ok()?;
        let size = window.inner_size().ok()?;
        if size.width == 0 || size.height == 0 {
            // A zero-sized window is a transient state, never something to
            // remember.
            return None;
        }
        Some(Self::new(position.x, position.y, size.width, size.height))
    }

    fn right(&self) -> i64 {
        self.x as i64 + self.width as i64
    }

    fn bottom(&self) -> i64 {
        self.y as i64 + self.height as i64
    }

    fn center(&self) -> (i64, i64) {
        (
            self.x as i64 + self.width as i64 / 2,
            self.y as i64 + self.height as i64 / 2,
        )
    }

    fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x as i64 && x < self.right() && y >= self.y as i64 && y < self.bottom()
    }

    /// Shared area with `other`, in square pixels. Zero for rectangles that do
    /// not overlap at all (touching edges do not count).
    fn overlap(&self, other: &Frame) -> u64 {
        let left = self.x.max(other.x) as i64;
        let top = self.y.max(other.y) as i64;
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            0
        } else {
            (right - left) as u64 * (bottom - top) as u64
        }
    }
}

/// One monitor as it exists at this moment. The work area is the part not
/// covered by the taskbar — the only region a window may be clamped into.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: Option<String>,
    pub frame: Frame,
    pub work: Frame,
    pub scale_factor: f64,
}

impl MonitorInfo {
    fn from_monitor(monitor: &Monitor) -> Self {
        let size = monitor.size();
        let position = monitor.position();
        let work = monitor.work_area();
        let frame = Frame::new(position.x, position.y, size.width, size.height);
        let work = Frame::new(
            work.position.x,
            work.position.y,
            work.size.width,
            work.size.height,
        );
        Self {
            name: monitor.name().cloned(),
            frame,
            // Some drivers report an empty work area; the monitor rectangle is
            // then the best available answer.
            work: if work.width == 0 || work.height == 0 {
                frame
            } else {
                work
            },
            scale_factor: monitor.scale_factor(),
        }
    }

    /// The part of a monitor that is worth persisting.
    fn reference(&self) -> MonitorRef {
        MonitorRef {
            name: self.name.clone(),
            frame: self.frame,
            scale_factor: self.scale_factor,
        }
    }
}

/// A monitor as it was when the window was saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorRef {
    /// The OS device name (`\\.\DISPLAY1`). `None` when the platform (or a
    /// display that has since disappeared) does not provide one.
    #[serde(default)]
    pub name: Option<String>,
    pub frame: Frame,
    /// Informational: kept so a log can explain an old record. Restoring works in
    /// physical pixels and does not need it.
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f64,
}

/// Everything `window.json` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedWindow {
    #[serde(default = "default_schema_version")]
    pub schema_version: u64,
    /// The window's *restored* frame — never the maximized one.
    pub frame: Frame,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub monitor: Option<MonitorRef>,
}

impl SavedWindow {
    /// Whether this record can be trusted to describe a window. A file is
    /// hand-editable, so a bad one must be ignored rather than turned into a
    /// window the size of the desktop.
    fn is_usable(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
            && plausible(&self.frame)
            && self
                .monitor
                .as_ref()
                .is_none_or(|monitor| plausible(&monitor.frame))
    }
}

fn plausible(frame: &Frame) -> bool {
    frame.width > 0
        && frame.width <= MAX_DIMENSION
        && frame.height > 0
        && frame.height <= MAX_DIMENSION
        && frame.x.unsigned_abs() <= MAX_COORDINATE
        && frame.y.unsigned_abs() <= MAX_COORDINATE
}

/// Where the window should be put when it is created.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub frame: Frame,
    pub maximized: bool,
    /// The monitor the frame was placed on; becomes the tracker's starting point
    /// so a close before the first move still records the right screen.
    pub monitor: Option<MonitorRef>,
}

impl Placement {
    /// The initial memory of where the window is.
    pub fn record(&self) -> SavedWindow {
        SavedWindow {
            schema_version: SCHEMA_VERSION,
            frame: self.frame,
            maximized: self.maximized,
            monitor: self.monitor.clone(),
        }
    }
}

// ------------------------------------------------------------------ monitors

/// Every monitor Windows currently reports, in physical pixels.
pub fn monitors(app: &AppHandle) -> Vec<MonitorInfo> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(MonitorInfo::from_monitor)
        .collect()
}

pub fn primary_monitor(app: &AppHandle) -> Option<MonitorInfo> {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| MonitorInfo::from_monitor(&monitor))
}

// ----------------------------------------------------------------- placement

/// Put the window back where the record says it was.
///
/// The monitor search runs from most to least specific: the device name, the
/// saved monitor rectangle, the window's centre, the window rectangle, and
/// finally the primary monitor (the saved screen is gone).
pub fn restore(
    saved: &SavedWindow,
    monitors: &[MonitorInfo],
    primary: Option<&MonitorInfo>,
) -> Placement {
    let Some(target) = choose_monitor(saved, monitors, primary) else {
        // No monitor information at all. The record is still the only truth.
        return Placement {
            frame: saved.frame,
            maximized: saved.maximized,
            monitor: saved.monitor.clone(),
        };
    };

    let frame = match &saved.monitor {
        Some(saved_monitor) => {
            // The offset from the old monitor's origin is what the user chose;
            // the monitor's position itself is the OS's business.
            let desired = Frame::new(
                target.frame.x + (saved.frame.x - saved_monitor.frame.x),
                target.frame.y + (saved.frame.y - saved_monitor.frame.y),
                saved.frame.width,
                saved.frame.height,
            );
            if target.frame == saved_monitor.frame
                && grip_on_screen(&desired, &target.work, target.scale_factor)
            {
                // Same monitor, same geometry: reproduce the frame bit-for-bit.
                // A window deliberately hanging past an edge stays there, as long
                // as its top strip — the frameless window's only drag region — is
                // still reachable.
                desired
            } else {
                clamp_into(target.work, desired)
            }
        }
        // Without a monitor reference there is no sensible offset; the centre of
        // the primary screen is the neutral answer.
        None => centered(target.work, saved.frame.width, saved.frame.height),
    };

    Placement {
        frame,
        maximized: saved.maximized,
        monitor: Some(target.reference()),
    }
}

/// The placement for a launch that has no record yet: the configured size,
/// centred on the primary monitor. Logical pixels are converted with the
/// primary monitor's scale, which is what the window will be created under.
pub fn first_launch(
    width: f64,
    height: f64,
    maximized: bool,
    monitors: &[MonitorInfo],
    primary: Option<&MonitorInfo>,
) -> Placement {
    let Some(target) = primary.or_else(|| monitors.first()) else {
        return Placement {
            frame: Frame::new(
                0,
                0,
                logical_to_physical(width, 1.0),
                logical_to_physical(height, 1.0),
            ),
            maximized,
            monitor: None,
        };
    };
    let scale = if target.scale_factor.is_finite() && target.scale_factor > 0.0 {
        target.scale_factor
    } else {
        1.0
    };
    Placement {
        frame: centered(
            target.work,
            logical_to_physical(width, scale),
            logical_to_physical(height, scale),
        ),
        maximized,
        monitor: Some(target.reference()),
    }
}

fn logical_to_physical(value: f64, scale: f64) -> u32 {
    let pixels = (value * scale).round();
    if !pixels.is_finite() || pixels < 1.0 {
        1
    } else {
        pixels.min(MAX_DIMENSION as f64) as u32
    }
}

fn choose_monitor<'a>(
    saved: &SavedWindow,
    monitors: &'a [MonitorInfo],
    primary: Option<&'a MonitorInfo>,
) -> Option<&'a MonitorInfo> {
    if let Some(saved_monitor) = &saved.monitor {
        if let Some(name) = saved_monitor
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let wanted = name.to_lowercase();
            if let Some(found) = monitors.iter().find(|monitor| {
                monitor
                    .name
                    .as_deref()
                    .is_some_and(|candidate| candidate.trim().to_lowercase() == wanted)
            }) {
                return Some(found);
            }
        }
        // The name is missing or no longer exists (driver reinstall, a
        // platform without device names): the screen's own rectangle is the
        // next best identity.
        if let Some(found) = best_overlap(monitors, &saved_monitor.frame) {
            return Some(found);
        }
    }

    let (center_x, center_y) = saved.frame.center();
    if let Some(found) = monitors
        .iter()
        .find(|monitor| monitor.frame.contains(center_x, center_y))
    {
        return Some(found);
    }
    if let Some(found) = best_overlap(monitors, &saved.frame) {
        return Some(found);
    }
    primary.or_else(|| monitors.first())
}

fn best_overlap<'a>(monitors: &'a [MonitorInfo], rect: &Frame) -> Option<&'a MonitorInfo> {
    monitors
        .iter()
        .map(|monitor| (monitor, rect.overlap(&monitor.frame)))
        .filter(|(_, area)| *area > 0)
        .max_by_key(|(_, area)| *area)
        .map(|(monitor, _)| monitor)
}

/// Whether the window's top strip is still on screen.
///
/// The window is frameless, so the strip at its top is the only place the user
/// can grab it. A record that puts that strip below, above or beside the work
/// area describes a window that cannot be moved back by hand — `window.json` is
/// hand-editable, so that is worth checking rather than trusting.
fn grip_on_screen(frame: &Frame, work: &Frame, scale_factor: f64) -> bool {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let strip_height = (crate::tabs::layout::TAB_BAR_HEIGHT * scale)
        .round()
        .clamp(1.0, 256.0) as u32;
    let strip = Frame::new(
        frame.x,
        frame.y,
        frame.width,
        frame.height.min(strip_height),
    );
    work.overlap(&strip) > 0
}

/// Keep a frame inside a work area. A window larger than the area is pinned to
/// its origin — the tab strip has to stay reachable, and the overflow belongs on
/// the side that still has a screen.
fn clamp_into(work: Frame, frame: Frame) -> Frame {
    let width = frame.width.min(work.width);
    let height = frame.height.min(work.height);
    let max_x = work.x as i64 + (work.width - width) as i64;
    let max_y = work.y as i64 + (work.height - height) as i64;
    Frame::new(
        (frame.x as i64).clamp(work.x as i64, max_x) as i32,
        (frame.y as i64).clamp(work.y as i64, max_y) as i32,
        width,
        height,
    )
}

fn centered(work: Frame, width: u32, height: u32) -> Frame {
    let width = width.min(work.width);
    let height = height.min(work.height);
    Frame::new(
        work.x + ((work.width - width) / 2) as i32,
        work.y + ((work.height - height) / 2) as i32,
        width,
        height,
    )
}

// -------------------------------------------------------------------- record

/// Read the saved geometry. A missing, unreadable or implausible file is simply
/// "no record": the cost is one launch at the default position.
pub fn load(path: &Path) -> Option<SavedWindow> {
    let text = std::fs::read_to_string(path).ok()?;
    // Notepad's "UTF-8 with BOM" is a realistic way to edit this file.
    let saved = serde_json::from_str::<SavedWindow>(text.trim_start_matches('\u{feff}')).ok()?;
    saved.is_usable().then_some(saved)
}

pub fn save(path: &Path, saved: &SavedWindow) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(saved).unwrap_or_default();
    std::fs::write(path, text)
}

/// Move a freshly created (still hidden) window onto the recorded frame.
///
/// Physical values are passed on purpose: `Position`/`Size` are scaled by the
/// window's current factor, and a physical value is not scaled at all.
///
/// `set_size` runs twice. tao turns an inner size into an outer one using the
/// frame it can measure *right now*, and a window that has never been shown still
/// reports its not-yet-reduced frame — on Windows 11 that is a full caption
/// taller, so a single pass lands 30px too tall. The first pass settles the
/// frame and the second one lands exactly on the requested client area; when the
/// first pass was already right, the second is a no-op.
pub fn apply(window: &Window, placement: &Placement) {
    let _ = window.set_position(PhysicalPosition::new(placement.frame.x, placement.frame.y));
    for _ in 0..2 {
        let _ = window.set_size(PhysicalSize::new(
            placement.frame.width,
            placement.frame.height,
        ));
    }
}

// ------------------------------------------------------------------- tracker

/// The window's position, kept current for as long as the process lives.
///
/// The close paths cannot afford to inspect a window that may already be gone,
/// and the exiting process cannot start asynchronous work; they only need the
/// latest value, which every move, resize and DPI change refreshes here.
pub struct Tracker {
    state: Mutex<SavedWindow>,
    path: PathBuf,
    root: PathBuf,
}

impl Tracker {
    pub fn new(path: PathBuf, root: PathBuf, seed: SavedWindow) -> Self {
        Self {
            state: Mutex::new(seed),
            path,
            root,
        }
    }

    fn state(&self) -> MutexGuard<'_, SavedWindow> {
        // A panicked writer left a plain value behind; geometry is not worth a
        // process-wide abort on every later close.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Refresh the remembered frame from the live window.
    pub fn remember(&self, window: &Window) {
        let (minimized, maximized) = window_state(window);
        // A minimized window reports its icon position (-32000, -32000 on
        // Windows). The last real frame is the only useful answer.
        if minimized {
            return;
        }
        // Read the window before taking the lock: these are calls into the
        // windowing library, and no close should ever wait on them while holding
        // the only copy of the geometry.
        let frame = if maximized {
            None
        } else {
            Frame::of_window(window)
        };
        let monitor = if frame.is_some() {
            window
                .current_monitor()
                .ok()
                .flatten()
                .map(|monitor| MonitorInfo::from_monitor(&monitor).reference())
        } else {
            None
        };

        let mut state = self.state();
        state.maximized = maximized;
        // A maximized window keeps its restore frame: overwriting it would
        // destroy the size the user returns to when unmaximizing.
        if let Some(frame) = frame {
            state.frame = frame;
            if let Some(monitor) = monitor {
                state.monitor = Some(monitor);
            }
        }
    }

    /// Write the remembered frame to disk. Called synchronously from the close
    /// and exit paths, where an `async` config write is not available.
    pub fn persist(&self) {
        let saved = self.state().clone();
        if let Err(err) = save(&self.path, &saved) {
            crate::dsh::process::debug_log(
                &self.root,
                &format!("window: could not write {}: {err}", self.path.display()),
            );
        }
    }
}

fn default_schema_version() -> u64 {
    SCHEMA_VERSION
}

fn default_scale_factor() -> f64 {
    1.0
}

/// Whether the window is minimized or maximized.
///
/// On Windows this asks the OS, not the windowing library: the library only
/// learns that a window was maximized while processing `WM_SIZE`, which arrives
/// *after* the move event that already reports the maximized rectangle. Trusting
/// the library there records that rectangle as the restore frame. `IsZoomed` /
/// `IsIconic` read the window style, which is correct by the time any event is
/// delivered.
fn window_state(window: &Window) -> (bool, bool) {
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        let hwnd = hwnd.0 as isize;
        return (crate::win::is_iconic(hwnd), crate::win::is_zoomed(hwnd));
    }
    (
        window.is_minimized().unwrap_or(false),
        window.is_maximized().unwrap_or(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: &str = r"\\.\DISPLAY11";
    const PRIMARY: &str = r"\\.\DISPLAY12";

    fn left_monitor() -> MonitorInfo {
        MonitorInfo {
            name: Some(LEFT.into()),
            frame: Frame::new(-2560, 0, 2560, 1440),
            work: Frame::new(-2560, 0, 2560, 1392),
            scale_factor: 1.0,
        }
    }

    fn primary_monitor() -> MonitorInfo {
        MonitorInfo {
            name: Some(PRIMARY.into()),
            frame: Frame::new(0, 0, 2560, 1440),
            work: Frame::new(0, 0, 2560, 1392),
            scale_factor: 1.0,
        }
    }

    fn saved_on(monitor: &MonitorInfo, frame: Frame) -> SavedWindow {
        SavedWindow {
            schema_version: SCHEMA_VERSION,
            frame,
            maximized: false,
            monitor: Some(monitor.reference()),
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dshell-window-state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        dir
    }

    #[test]
    fn the_same_monitor_restores_the_exact_frame() {
        let left = left_monitor();
        let primary = primary_monitor();
        let frame = Frame::new(-2000, 100, 1280, 820);
        let saved = saved_on(&left, frame);

        let placement = restore(&saved, &[left, primary.clone()], Some(&primary));

        assert_eq!(placement.frame, frame);
        assert_eq!(
            placement.monitor.and_then(|monitor| monitor.name),
            Some(LEFT.to_string())
        );
    }

    /// The left screen moved to the right of the primary: the device name is the
    /// identity, not the coordinates.
    #[test]
    fn a_monitor_moved_to_the_other_side_takes_its_window_along() {
        let left = left_monitor();
        let primary = primary_monitor();
        let saved = saved_on(&left, Frame::new(-2000, 100, 1280, 820));
        let moved = MonitorInfo {
            frame: Frame::new(2560, 0, 2560, 1440),
            work: Frame::new(2560, 0, 2560, 1392),
            ..left
        };

        let placement = restore(&saved, &[primary.clone(), moved], Some(&primary));

        // Same 560px offset from the monitor's left edge, on the other side.
        assert_eq!(placement.frame, Frame::new(3120, 100, 1280, 820));
    }

    /// The monitor the window was on is gone. The window must come back on a
    /// screen that exists, fully visible, rather than at its remembered (now
    /// off-desktop) coordinates.
    #[test]
    fn an_unplugged_monitor_returns_the_window_to_the_primary() {
        let left = left_monitor();
        let primary = MonitorInfo {
            frame: Frame::new(0, 0, 1920, 1080),
            work: Frame::new(0, 0, 1920, 1040),
            ..primary_monitor()
        };
        let saved = saved_on(&left, Frame::new(-2200, 200, 1280, 820));

        let frame = restore(&saved, std::slice::from_ref(&primary), Some(&primary)).frame;

        assert_eq!((frame.width, frame.height), (1280, 820));
        assert!(
            frame.x >= 0 && frame.y >= 0,
            "frame {frame:?} left the screen"
        );
        assert!(frame.right() <= 1920 && frame.bottom() <= 1040);
    }

    /// Same screen, new resolution: the offset is kept, then pulled back inside.
    #[test]
    fn a_resolution_change_keeps_the_window_inside_the_monitor() {
        let left = left_monitor();
        let shrunk = MonitorInfo {
            frame: Frame::new(-2560, 0, 1920, 1080),
            work: Frame::new(-2560, 0, 1920, 1040),
            ..left_monitor()
        };
        // 1960px from the monitor's left edge — past the new right edge.
        let saved = saved_on(&left, Frame::new(-600, 100, 1280, 820));

        let frame = restore(&saved, &[shrunk], None).frame;

        assert_eq!(frame.x, -2560 + (1920 - 1280));
        assert_eq!(frame.right(), -640);
        assert_eq!(frame.y, 100);
    }

    #[test]
    fn a_window_larger_than_its_new_monitor_is_shrunk_to_fit() {
        let left = left_monitor();
        let small = MonitorInfo {
            frame: Frame::new(-2560, 0, 1920, 1080),
            work: Frame::new(-2560, 0, 1920, 1040),
            ..left_monitor()
        };
        let saved = saved_on(&left, Frame::new(-2400, 40, 2400, 1300));

        let frame = restore(&saved, &[small], None).frame;

        assert_eq!((frame.width, frame.height), (1920, 1040));
        assert_eq!((frame.x, frame.y), (-2560, 0));
    }

    /// When nothing about the screen changed, the frame is not "helpfully"
    /// adjusted: a window hanging over an edge was a choice.
    #[test]
    fn a_window_hanging_past_the_edge_is_restored_where_it_was() {
        let primary = primary_monitor();
        let frame = Frame::new(2480, 20, 1280, 820);

        let placement = restore(
            &saved_on(&primary, frame),
            std::slice::from_ref(&primary),
            Some(&primary),
        );

        assert_eq!(placement.frame, frame);
    }

    /// A record that would put the window's only drag region off the desktop is
    /// not taken literally — otherwise a hand-edited `window.json` strands the
    /// window with no way to move it back.
    #[test]
    fn a_record_whose_drag_strip_is_offscreen_is_pulled_back() {
        let left = left_monitor();
        // Same monitor rectangle, but the window (and its strip) is far above and
        // to the left of it.
        let saved = saved_on(&left, Frame::new(-5200, -3000, 1600, 900));

        let frame = restore(&saved, std::slice::from_ref(&left), None).frame;

        assert_eq!(frame, Frame::new(-2560, 0, 1600, 900));
    }

    /// A reinstall can change the device name while the screen stays put; the
    /// saved rectangle still identifies it.
    #[test]
    fn a_renamed_monitor_is_matched_by_its_rectangle() {
        let left = left_monitor();
        let primary = primary_monitor();
        let mut saved = saved_on(&left, Frame::new(-2000, 100, 1280, 820));
        saved.monitor.as_mut().expect("monitor").name = None;
        let renamed = MonitorInfo {
            name: Some("DELL U2720Q".into()),
            ..left
        };

        let placement = restore(&saved, &[primary.clone(), renamed], Some(&primary));

        assert_eq!(placement.frame, Frame::new(-2000, 100, 1280, 820));
        assert_eq!(
            placement.monitor.and_then(|monitor| monitor.name),
            Some("DELL U2720Q".to_string())
        );
    }

    /// A window straddling two screens belongs to the one it covers most — and
    /// with no name to go by, the saved monitor rectangle finds it again.
    #[test]
    fn a_straddling_window_returns_to_the_screen_it_mostly_covers() {
        let a = MonitorInfo {
            name: Some("A".into()),
            frame: Frame::new(0, 0, 1000, 1000),
            work: Frame::new(0, 0, 1000, 1000),
            scale_factor: 1.0,
        };
        let b = MonitorInfo {
            name: Some("B".into()),
            frame: Frame::new(1000, 0, 1000, 1000),
            work: Frame::new(1000, 0, 1000, 1000),
            scale_factor: 1.0,
        };
        let mut saved = saved_on(&b, Frame::new(800, 100, 600, 400));
        saved.monitor.as_mut().expect("monitor").name = None;

        let placement = restore(&saved, &[a.clone(), b], Some(&a));

        assert_eq!(placement.frame, Frame::new(800, 100, 600, 400));
    }

    #[test]
    fn a_record_without_a_monitor_is_centered_on_the_primary() {
        let primary = primary_monitor();
        let mut saved = saved_on(&primary, Frame::new(100, 100, 1200, 800));
        saved.monitor = None;

        let frame = restore(&saved, std::slice::from_ref(&primary), Some(&primary)).frame;

        assert_eq!((frame.x, frame.y), ((2560 - 1200) / 2, (1392 - 800) / 2));
    }

    /// Maximization is a flag, not a bigger frame: the restore rect survives.
    #[test]
    fn a_maximized_window_comes_back_maximized_at_its_restore_frame() {
        let primary = primary_monitor();
        let mut saved = saved_on(&primary, Frame::new(100, 80, 1280, 820));
        saved.maximized = true;

        let placement = restore(&saved, std::slice::from_ref(&primary), Some(&primary));

        assert!(placement.maximized);
        assert_eq!(placement.frame, Frame::new(100, 80, 1280, 820));
    }

    #[test]
    fn the_first_launch_centers_the_configured_size_on_the_primary() {
        let primary = MonitorInfo {
            scale_factor: 1.5,
            ..primary_monitor()
        };

        let placement = first_launch(
            1280.0,
            820.0,
            false,
            std::slice::from_ref(&primary),
            Some(&primary),
        );

        assert_eq!(
            (placement.frame.width, placement.frame.height),
            (1920, 1230)
        );
        assert_eq!(
            (placement.frame.x, placement.frame.y),
            ((2560 - 1920) / 2, (1392 - 1230) / 2)
        );
    }

    #[test]
    fn the_record_survives_a_write_and_a_read() {
        let dir = test_dir("round-trip");
        let path = dir.join("window.json");
        let saved = SavedWindow {
            schema_version: SCHEMA_VERSION,
            frame: Frame::new(-2000, 100, 1280, 820),
            maximized: true,
            monitor: Some(left_monitor().reference()),
        };

        save(&path, &saved).expect("write the record");

        assert_eq!(load(&path), Some(saved));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_byte_order_mark_does_not_discard_the_record() {
        let dir = test_dir("bom");
        let path = dir.join("window.json");
        std::fs::write(
            &path,
            "\u{feff}{\"schemaVersion\":1,\"frame\":{\"x\":1,\"y\":2,\"width\":900,\"height\":600},\"maximized\":false}",
        )
        .unwrap();

        let saved = load(&path).expect("the BOM must be ignored");

        assert_eq!(saved.frame, Frame::new(1, 2, 900, 600));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_future_or_impossible_record_is_ignored() {
        let dir = test_dir("rejected");
        let path = dir.join("window.json");

        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path).is_none());

        // A newer build's layout: guessing its fields would be worse than
        // starting at the default position.
        std::fs::write(
            &path,
            r#"{"schemaVersion":99,"frame":{"x":0,"y":0,"width":900,"height":600}}"#,
        )
        .unwrap();
        assert!(load(&path).is_none());

        // Zero-sized or absurd frames must never become a window.
        std::fs::write(
            &path,
            r#"{"schemaVersion":1,"frame":{"x":0,"y":0,"width":0,"height":600}}"#,
        )
        .unwrap();
        assert!(load(&path).is_none());
        std::fs::write(
            &path,
            r#"{"schemaVersion":1,"frame":{"x":0,"y":0,"width":90000,"height":600}}"#,
        )
        .unwrap();
        assert!(load(&path).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tracker_writes_its_seed_without_needing_a_window() {
        let dir = test_dir("tracker");
        let path = dir.join("window.json");
        let seed = SavedWindow {
            schema_version: SCHEMA_VERSION,
            frame: Frame::new(-2000, 100, 1280, 820),
            maximized: false,
            monitor: Some(left_monitor().reference()),
        };
        let tracker = Tracker::new(path.clone(), dir.clone(), seed.clone());

        tracker.persist();

        assert_eq!(load(&path), Some(seed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
