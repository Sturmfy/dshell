//! The notification-area icon, its native menu, and the close-to-tray policy.
//!
//! This is deliberately not another webview surface. The tray menu must open on
//! a right-click even when the main window is hidden, which a child webview in a
//! 38px strip cannot do; a native menu also renders the status dot as a real
//! menu-item icon at the DPI Windows expects.
//!
//! The menu is a live projection of the tab list:
//!
//! ```text
//! [● 连接 A]
//! [● 连接 B]
//! ──────────
//! [退出]
//! ```
//!
//! Only *connection* tabs appear — dshell's own pages (first-run wizard, new
//! connection, settings) are not things a user switches to from the tray. The
//! dot mirrors `web/strip.ts`'s `dotClass`: session attention outranks phase,
//! and a tab that has not connected yet keeps its row so the dot can explain why.
//!
//! One honest limitation: a native menu cannot animate. The dot has the same
//! colour and the same state priority as the strip's, but not its jump.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::image::Image;
use tauri::menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent, TrayIconId};
use tauri::{AppHandle, Listener, Manager, WindowEvent};

use crate::state::AppState;
use crate::tabs::manager::{TABS_CHANGED_EVENT, TAB_CHANGED_EVENT};
use crate::tabs::state::{attention, phase, TabView};
use crate::MAIN_WINDOW;

/// Stable id of the one tray icon. Tauri keeps icons in its resource table, so
/// this is how every later menu refresh finds it again.
pub const TRAY_ID: &str = "dshell";

const QUIT_ITEM_ID: &str = "tray:quit";
const TAB_ITEM_PREFIX: &str = "tray:tab:";

/// A longer title than this makes a tray popup wider than the screen corner it
/// opens in; the strip itself still shows the full name.
const MAX_LABEL_CHARS: usize = 80;

/// Source size of a dot. Windows draws menu icons at 16×16; starting at 32 and
/// letting the menu scale down keeps the circle smooth on high-DPI displays.
const DOT_ICON_SIZE: u32 = 32;

/// How long each half of the unread-attention blink is shown.
///
/// The strip's dot hops every 2.4s; a tray icon cannot hop, so it alternates
/// between the plain icon and a badge. 600ms on / 600ms off reads as a deliberate
/// alert rather than a stutter, and is slow enough not to flood `Shell_NotifyIcon`.
const BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// Refresh tasks are serialised: two tab events arriving together must not race
/// to install two menus, the older one winning by finishing last.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The rows the menu was last built from. A dot only changes a handful of times
/// per session, while `changed_one` fires on every attention poll; comparing the
/// rendered projection avoids rebuilding a native menu for a no-op.
static LAST_SIGNATURE: Mutex<Option<Vec<MenuRow>>> = Mutex::new(None);

/// One unread-attention blink loop, and the state that starts or stops it.
///
/// The mutex is held across `TrayIcon::set_icon` in the loop: that is what makes
/// a stop (restore the plain icon) and an in-flight tick serialise. Releasing it
/// between the state check and the icon write would let a stale tick repaint the
/// badge after attention was cleared, leaving the tray blinking forever.
static BLINK: Mutex<BlinkState> = Mutex::new(BlinkState {
    generation: 0,
    running: false,
    attention: None,
});

#[derive(Debug, Clone, Copy)]
struct BlinkState {
    /// Incremented on every start/stop; a loop only owns the icon while its
    /// captured generation is still the current one.
    generation: u64,
    running: bool,
    /// Strongest unacknowledged session state across connection tabs.
    attention: Option<Dot>,
}

/// Create the tray icon and start projecting tabs into its menu.
///
/// Returns `false` when the icon could not be installed. The caller then leaves
/// the window's old close behaviour untouched — hiding a window into a tray that
/// does not exist would strand it with no way to bring it back.
pub fn install(app: &AppHandle) -> bool {
    if tray_icon(app).is_some() {
        return true;
    }
    let Some(icon) = app.default_window_icon().cloned() else {
        log(app, "no application icon; tray disabled");
        return false;
    };

    // An empty menu is still valid and useful: the first right-click before any
    // tab exists shows only the exit item instead of nothing at all.
    let initial_rows = Vec::new();
    let menu = match build_menu(app, &initial_rows) {
        Ok(menu) => menu,
        Err(err) => {
            log(app, format!("initial menu failed: {err}"));
            return false;
        }
    };

    let built = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("dshell")
        .menu(&menu)
        // Left click restores the window; only the right click opens the menu.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        })
        .build(app);

    if let Err(err) = built {
        log(app, format!("tray icon failed: {err}"));
        return false;
    }
    if let Ok(mut last) = LAST_SIGNATURE.lock() {
        *last = Some(initial_rows);
    }

    install_close_policy(app);
    listen_for_tabs(app.clone(), TABS_CHANGED_EVENT);
    listen_for_tabs(app.clone(), TAB_CHANGED_EVENT);
    true
}

/// Rebuild the native menu from the current tab snapshot.
///
/// Called for `tabs-changed` and `tab-changed`; the signature comparison makes
/// the second kind cheap when the only thing that moved was a running count.
pub async fn refresh(app: &AppHandle) {
    let _guard = REFRESH.lock().await;

    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let views = state.manager.views().await;
    let rows = rows(&views);

    // The icon's blink is independent of the menu projection: an unread state can
    // appear or clear without the rendered rows changing size, and the store may
    // be refreshed while the menu itself is closed.
    sync_attention(app, attention_level(&views));

    {
        let Ok(last) = LAST_SIGNATURE.lock() else {
            return;
        };
        if last.as_ref() == Some(&rows) {
            return;
        }
    }

    if tray_icon(app).is_none() {
        return;
    }
    match build_menu(app, &rows) {
        Ok(menu) => {
            let applied = tray_icon(app)
                .map(|tray| tray.set_menu(Some(menu)).is_ok())
                .unwrap_or(false);
            if applied {
                if let Ok(mut last) = LAST_SIGNATURE.lock() {
                    *last = Some(rows);
                }
            }
        }
        Err(err) => log(app, format!("menu rebuild failed: {err}")),
    }
}

// ------------------------------------------------------------------- window

/// Bring the main window back from the tray (or just focus it).
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_window(MAIN_WINDOW) {
        // A window hidden while minimized otherwise comes back still minimized:
        // `show` does not clear that state on Windows.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Whether a close request should hide the window instead of ending the app.
///
/// Both conditions matter. The preference alone would hide the window into a
/// tray that failed to install; the tray alone would ignore the user's choice.
fn should_hide_on_close(close_to_tray: bool, tray_installed: bool) -> bool {
    close_to_tray && tray_installed
}

fn install_close_policy(app: &AppHandle) {
    let Some(window) = app.get_window(MAIN_WINDOW) else {
        return;
    };
    let handle = app.clone();
    window.on_window_event(move |event| {
        let WindowEvent::CloseRequested { api, .. } = event else {
            return;
        };
        // Record where the window is *before* it can disappear. Hiding keeps the
        // process alive, but the destroy path never gets a second chance to read
        // the window; both paths leave through here, so this is the one reliable
        // moment. It is a synchronous file write: the config's lock cannot be
        // taken from the main thread (see `window_state`).
        let state = handle.try_state::<Arc<AppState>>();
        if let Some(state) = state.as_ref() {
            if let Some(window) = handle.get_window(MAIN_WINDOW) {
                state.window.remember(&window);
            }
            state.window.persist();
        }
        // The config lives behind a tokio mutex; this handler runs on the main
        // thread and must answer synchronously, so it reads the atomic mirror
        // that `AppState::update_config` keeps in step with the file.
        let close_to_tray = state
            .map(|state| state.close_to_tray.load(Ordering::Relaxed))
            .unwrap_or(true);
        if !should_hide_on_close(close_to_tray, tray_icon(&handle).is_some()) {
            // Not our close to intercept: the existing path destroys the window,
            // disposes the menu popup, and lets the event loop exit.
            return;
        }

        api.prevent_close();
        // A right-click menu that happened to be open must not stay on screen
        // over a hidden window, and its pending measurement must not reveal it
        // again after the fact.
        crate::commands::hide_tab_menu_now(&handle);
        if let Some(window) = handle.get_window(MAIN_WINDOW) {
            let _ = window.hide();
        }
    });
}

// --------------------------------------------------------------------- menu

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    if id == QUIT_ITEM_ID {
        // The popup is a hidden window; take it down explicitly so the exit does
        // not leave a visible menu behind for a frame.
        crate::commands::hide_tab_menu_now(app);
        app.exit(0);
        return;
    }
    let Some(tab_id) = tab_id_from_menu_id(id) else {
        return;
    };
    let tab_id = tab_id.to_string();
    show_main_window(app);

    // `activate` takes the tab locks, and the menu event already runs inside the
    // event loop; hand the work to the async runtime like every other webview
    // touch in this crate.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<Arc<AppState>>() else {
            return;
        };
        // Between building the menu and clicking an item, the tab can be gone.
        // Activating an unknown id would leave every content webview parked, so
        // a stale item only restores the window.
        if state.manager.has_tab(&tab_id).await {
            state.manager.activate(Some(&tab_id)).await;
        }
    });
}

/// Index into the current `views`, skipping dshell's own pages.
fn rows(views: &[TabView]) -> Vec<MenuRow> {
    views
        .iter()
        .filter(|view| view.page_kind.is_none())
        .map(|view| MenuRow {
            id: view.id.clone(),
            label: menu_label(&view.title),
            dot: dot_for(view),
        })
        .collect()
}

fn build_menu(app: &AppHandle, rows: &[MenuRow]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for row in rows {
        let item = IconMenuItem::with_id(
            app,
            tab_item_id(&row.id),
            &row.label,
            true,
            Some(dot_image(row.dot)),
            None::<&str>,
        )?;
        menu.append(&item)?;
    }
    if !rows.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    // Always last: the menu above it will grow, the stop stays where the hand
    // expects it.
    menu.append(&MenuItem::with_id(
        app,
        QUIT_ITEM_ID,
        "退出",
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

fn tab_item_id(tab_id: &str) -> String {
    format!("{TAB_ITEM_PREFIX}{tab_id}")
}

fn tab_id_from_menu_id(menu_id: &str) -> Option<&str> {
    menu_id.strip_prefix(TAB_ITEM_PREFIX)
}

/// What Windows should draw in front of one row.
///
/// Deliberately the same projection as `web/strip.ts::dotClass`: session
/// attention first (error > needs input > completed), then the backend phase.
/// The strip's `exited` branch is unreachable in practice because
/// `TabView::attention` already reports `error` for an exited backend; it is
/// kept here so the mapping stays a literal copy rather than a near-copy.
fn dot_for(tab: &TabView) -> Dot {
    match tab.attention.as_deref() {
        Some(attention::ERROR) => return Dot::Error,
        Some(attention::NEEDS_INPUT) => return Dot::NeedsInput,
        Some(attention::COMPLETED) => return Dot::Completed,
        _ => {}
    }
    match tab.phase.as_str() {
        phase::READY => Dot::Ready,
        phase::FAILED => Dot::Failed,
        phase::NEEDS_TOKEN => Dot::NeedsInput,
        phase::PREPARING | phase::STARTING | phase::WAITING_FOR_READY => Dot::Busy,
        phase::EXITED => Dot::Exited,
        _ => Dot::Idle,
    }
}

/// The strongest **unacknowledged** session state across connection tabs.
///
/// Only `TabView::attention` participates: a tab that is merely starting or
/// ready has no message waiting, and must not make the icon flash. Page tabs are
/// excluded for the same reason they are excluded from the menu — the settings
/// page is not a DSH session. Priority matches the strip: error > needs input >
/// completed.
fn attention_level(views: &[TabView]) -> Option<Dot> {
    fn rank(dot: Dot) -> u8 {
        match dot {
            Dot::Error => 0,
            Dot::NeedsInput => 1,
            Dot::Completed => 2,
            _ => 3,
        }
    }

    let mut level: Option<Dot> = None;
    for view in views.iter().filter(|view| view.page_kind.is_none()) {
        let next = match view.attention.as_deref() {
            Some(attention::ERROR) => Dot::Error,
            Some(attention::NEEDS_INPUT) => Dot::NeedsInput,
            Some(attention::COMPLETED) => Dot::Completed,
            _ => continue,
        };
        if level.is_none_or(|current| rank(next) < rank(current)) {
            level = Some(next);
        }
        if level == Some(Dot::Error) {
            break;
        }
    }
    level
}

/// A native menu interprets `&` as a mnemonic marker, and one line break turns
/// one row into two. Both are user data here (a tab title), so both are cleaned
/// rather than trusted.
fn menu_label(title: &str) -> String {
    let collapsed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = if collapsed.is_empty() {
        "未命名".to_string()
    } else {
        collapsed
    };
    let mut label: String = trimmed.chars().take(MAX_LABEL_CHARS).collect();
    if trimmed.chars().count() > MAX_LABEL_CHARS {
        label.push('…');
    }
    label.replace('&', "&&")
}

// --------------------------------------------------------------------- dots

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dot {
    Error,
    NeedsInput,
    Completed,
    Ready,
    Failed,
    Busy,
    Exited,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuRow {
    id: String,
    label: String,
    dot: Dot,
}

/// Colours are copied from `web/styles.css` (`--danger`, `--warn`, `--ok`,
/// `--accent`, `--fg-faint`) so a tray dot, a tray badge and a strip dot mean the
/// same thing, not merely something similar.
fn dot_color(dot: Dot) -> [u8; 3] {
    match dot {
        Dot::Error | Dot::Failed => [0xec, 0x13, 0x13],
        Dot::NeedsInput => [0xf5, 0x9e, 0x0b],
        Dot::Completed | Dot::Ready => [0x22, 0xc5, 0x5e],
        Dot::Busy => [0x41, 0x76, 0xe6],
        Dot::Exited | Dot::Idle => [0x81, 0x85, 0x8c],
    }
}

fn dot_filled(dot: Dot) -> bool {
    !matches!(dot, Dot::Exited | Dot::Idle)
}

fn dot_image(dot: Dot) -> Image<'static> {
    Image::new_owned(
        render_dot(dot_color(dot), dot_filled(dot)),
        DOT_ICON_SIZE,
        DOT_ICON_SIZE,
    )
}

/// Draw one antialiased dot on a transparent square.
///
/// 4×4 supersampling is enough at this size and keeps the crate free of an image
/// dependency: the whole icon is a few hundred bytes of RGBA we own.
fn render_dot(rgb: [u8; 3], filled: bool) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let size = DOT_ICON_SIZE as f32;
    let center = size / 2.0;
    let outer = size * 0.25; // 8px at 32×32, i.e. the strip's 8px dot at 16px.
    let inner = outer - 2.5;
    let mut rgba = vec![0u8; (DOT_ICON_SIZE * DOT_ICON_SIZE * 4) as usize];

    for y in 0..DOT_ICON_SIZE {
        for x in 0..DOT_ICON_SIZE {
            let mut covered = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = x as f32 + (sample_x as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sample_y as f32 + 0.5) / SAMPLES as f32;
                    let distance = ((px - center).powi(2) + (py - center).powi(2)).sqrt();
                    let inside = distance <= outer && (filled || distance >= inner);
                    if inside {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let alpha = (covered as f32 / (SAMPLES * SAMPLES) as f32 * 255.0).round() as u8;
            let index = ((y * DOT_ICON_SIZE + x) * 4) as usize;
            rgba[index] = rgb[0];
            rgba[index + 1] = rgb[1];
            rgba[index + 2] = rgb[2];
            rgba[index + 3] = alpha;
        }
    }
    rgba
}

// ------------------------------------------------------- unread attention

/// Start or stop the tray icon's unread blink for the current attention level.
///
/// `None` restores the plain application icon and invalidates the loop. `Some`
/// starts exactly one loop if none is running; a loop that is already running
/// reads the level on every tick, so a change from "needs input" to "error"
/// recolours the badge without restarting the blink.
fn sync_attention(app: &AppHandle, level: Option<Dot>) {
    if tray_icon(app).is_none() {
        return;
    }
    let Ok(mut state) = BLINK.lock() else {
        return;
    };
    if state.attention == level {
        return;
    }
    let was_running = state.running;
    state.attention = level;

    if level.is_none() {
        state.generation = state.generation.wrapping_add(1);
        state.running = false;
        // Held under the lock so an in-flight tick cannot repaint the badge after
        // this restore; see the `BLINK` doc comment.
        set_plain_tray_icon(app);
        log(app, "unread blink stopped");
        return;
    }

    if was_running {
        log(app, format!("unread blink level changed: {level:?}"));
        return;
    }
    state.running = true;
    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    drop(state);
    log(app, format!("unread blink started: {level:?}"));
    start_blinking(app.clone(), generation);
}

/// One blink loop. It alternates the plain icon with an attention badge while the
/// current generation still owns the icon.
fn start_blinking(app: AppHandle, generation: u64) {
    tauri::async_runtime::spawn(async move {
        let Some(base) = app.default_window_icon().cloned() else {
            if let Ok(mut state) = BLINK.lock() {
                if state.generation == generation {
                    state.running = false;
                }
            }
            return;
        };
        let mut badge_shown = false;
        loop {
            {
                let Ok(state) = BLINK.lock() else {
                    break;
                };
                if state.generation != generation || state.attention.is_none() {
                    break;
                }
                let level = state.attention.expect("checked above");
                badge_shown = !badge_shown;
                let icon = if badge_shown {
                    badge_image(&base, level)
                } else {
                    base.clone()
                };
                if let Some(tray) = tray_icon(&app) {
                    if let Err(err) = tray.set_icon(Some(icon)) {
                        log(&app, format!("blink icon update failed: {err}"));
                    }
                }
            }
            tokio::time::sleep(BLINK_INTERVAL).await;
        }

        // A newer generation (or the stop path) owns the icon now. Only this loop
        // may clear `running`, and it only restores the plain icon when attention
        // cleared rather than being replaced by a still-blinking level.
        if let Ok(mut state) = BLINK.lock() {
            if state.generation == generation {
                state.running = false;
                if state.attention.is_none() {
                    set_plain_tray_icon(&app);
                }
            }
        }
    });
}

fn set_plain_tray_icon(app: &AppHandle) {
    let Some(icon) = app.default_window_icon().cloned() else {
        return;
    };
    if let Some(tray) = tray_icon(app) {
        let _ = tray.set_icon(Some(icon));
    }
}

/// The application icon with an attention badge in the bottom-right corner.
///
/// The white ring keeps a red badge legible on a dark icon and a green one on a
/// light icon; the fill reuses the same colour as the corresponding strip dot.
fn badge_image(base: &Image<'_>, level: Dot) -> Image<'static> {
    let width = base.width();
    let height = base.height();
    let mut rgba = base.rgba().to_vec();
    if width < 8 || height < 8 {
        return Image::new_owned(rgba, width, height);
    }
    let size = width.min(height) as f32;
    let radius = size * 0.30;
    let ring = (size * 0.09).max(1.0);
    let margin = size * 0.02;
    let cx = width as f32 - radius - margin;
    let cy = height as f32 - radius - margin;
    composite_circle(&mut rgba, width, height, cx, cy, radius, [0xfa, 0xfa, 0xfb]);
    composite_circle(
        &mut rgba,
        width,
        height,
        cx,
        cy,
        (radius - ring).max(1.0),
        dot_color(level),
    );
    Image::new_owned(rgba, width, height)
}

/// Alpha-composite one antialiased circle onto a straight-alpha RGBA buffer.
fn composite_circle(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    rgb: [u8; 3],
) {
    const SAMPLES: u32 = 4;
    if radius <= 0.0 || width == 0 || height == 0 {
        return;
    }
    let x0 = (cx - radius).floor().max(0.0) as u32;
    let y0 = (cy - radius).floor().max(0.0) as u32;
    let x1 = (cx + radius).ceil().min((width - 1) as f32).max(0.0) as u32;
    let y1 = (cy + radius).ceil().min((height - 1) as f32).max(0.0) as u32;
    if x0 > x1 || y0 > y1 {
        return;
    }

    let radius_sq = radius * radius;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut covered = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = x as f32 + (sample_x as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sample_y as f32 + 0.5) / SAMPLES as f32;
                    if (px - cx).powi(2) + (py - cy).powi(2) <= radius_sq {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let source_alpha = covered as f32 / (SAMPLES * SAMPLES) as f32;
            let index = ((y * width + x) * 4) as usize;
            let dest_alpha = rgba[index + 3] as f32 / 255.0;
            let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
            if out_alpha <= 0.0 {
                continue;
            }
            let blend = |source: u8, dest: u8| -> u8 {
                let value = (source as f32 * source_alpha
                    + dest as f32 * dest_alpha * (1.0 - source_alpha))
                    / out_alpha;
                value.round().clamp(0.0, 255.0) as u8
            };
            let red = blend(rgb[0], rgba[index]);
            let green = blend(rgb[1], rgba[index + 1]);
            let blue = blend(rgb[2], rgba[index + 2]);
            rgba[index] = red;
            rgba[index + 1] = green;
            rgba[index + 2] = blue;
            rgba[index + 3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

// ------------------------------------------------------------------- helpers

fn tray_icon(app: &AppHandle) -> Option<tauri::tray::TrayIcon<tauri::Wry>> {
    app.tray_by_id(&TrayIconId::new(TRAY_ID))
}

fn listen_for_tabs(app: AppHandle, event: &'static str) {
    let handle = app.clone();
    app.listen_any(event, move |_| {
        let app = handle.clone();
        tauri::async_runtime::spawn(async move { refresh(&app).await });
    });
}

fn log(app: &AppHandle, message: impl AsRef<str>) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        crate::dsh::process::debug_log(
            &state.data_root.path,
            &format!("tray: {}", message.as_ref()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        id: &str,
        title: &str,
        phase_id: &str,
        attention: Option<&str>,
        page: Option<&str>,
    ) -> TabView {
        TabView {
            id: id.into(),
            title: title.into(),
            badge: "local".into(),
            page_kind: page.map(str::to_string),
            closable: true,
            phase: phase_id.into(),
            origin: None,
            workspace: None,
            dsh_home: None,
            dsh_home_effective: None,
            runtime: None,
            pid: None,
            uptime_ms: None,
            failure: None,
            notice: None,
            plaintext: false,
            attention: attention.map(str::to_string),
            attention_counts: None,
            has_session_cookie: false,
            log_path: None,
            exit_code: None,
            ready: phase_id == phase::READY,
            command_line: None,
            instance_key: String::new(),
        }
    }

    #[test]
    fn the_menu_lists_connection_tabs_in_strip_order() {
        let views = vec![
            view("page", "设置", phase::READY, None, Some("settings")),
            view("b", "B", phase::READY, None, None),
            view("a", "A", phase::STARTING, None, None),
        ];
        let rows = rows(&views);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"],
            "page tabs are not connections and must not appear"
        );
        assert_eq!(rows[0].label, "B");
    }

    #[test]
    fn the_dot_uses_the_strip_priority() {
        // Session attention outranks a perfectly ready backend, and error >
        // needs input > completed — the same order as DSH's own session rows.
        assert_eq!(
            dot_for(&view("a", "A", phase::READY, Some(attention::ERROR), None)),
            Dot::Error
        );
        assert_eq!(
            dot_for(&view(
                "a",
                "A",
                phase::READY,
                Some(attention::NEEDS_INPUT),
                None
            )),
            Dot::NeedsInput
        );
        assert_eq!(
            dot_for(&view(
                "a",
                "A",
                phase::READY,
                Some(attention::COMPLETED),
                None
            )),
            Dot::Completed
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::READY, None, None)),
            Dot::Ready
        );
    }

    #[test]
    fn the_dot_falls_back_to_the_phase() {
        assert_eq!(
            dot_for(&view("a", "A", phase::PREPARING, None, None)),
            Dot::Busy
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::STARTING, None, None)),
            Dot::Busy
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::WAITING_FOR_READY, None, None)),
            Dot::Busy
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::FAILED, None, None)),
            Dot::Failed
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::NEEDS_TOKEN, None, None)),
            Dot::NeedsInput
        );
        // `build_view` turns an exited backend into attention=error, so the red
        // branch wins in real payloads; the defensive phase branch is hollow.
        assert_eq!(
            dot_for(&view("a", "A", phase::EXITED, Some(attention::ERROR), None)),
            Dot::Error
        );
        assert_eq!(
            dot_for(&view("a", "A", phase::EXITED, None, None)),
            Dot::Exited
        );
        assert_eq!(dot_for(&view("a", "A", phase::IDLE, None, None)), Dot::Idle);
    }

    /// The flash is about *unviewed* states, not about a backend merely being in
    /// some phase. Page tabs are dshell's own surfaces and cannot have DSH
    /// messages, even if a hand-written payload gave them an attention value.
    #[test]
    fn unread_flash_uses_attention_priority_and_skips_pages() {
        let mut views = vec![
            view(
                "page",
                "设置",
                phase::READY,
                Some(attention::ERROR),
                Some("settings"),
            ),
            view("a", "A", phase::READY, Some(attention::COMPLETED), None),
            view("b", "B", phase::READY, Some(attention::NEEDS_INPUT), None),
        ];
        assert_eq!(attention_level(&views), Some(Dot::NeedsInput));

        views.push(view("c", "C", phase::READY, Some(attention::ERROR), None));
        assert_eq!(attention_level(&views), Some(Dot::Error));

        // A phase alone is not an unread message; only `attention` blinks.
        assert_eq!(
            attention_level(&[view("d", "D", phase::FAILED, None, None)]),
            None
        );
        assert_eq!(
            attention_level(&[view("e", "E", phase::STARTING, None, None)]),
            None
        );
    }

    #[test]
    fn the_flash_badge_keeps_the_icon_size_and_colours_the_corner() {
        let mut rgba = vec![0u8; (DOT_ICON_SIZE * DOT_ICON_SIZE * 4) as usize];
        for pixel in rgba.chunks_mut(4) {
            pixel.copy_from_slice(&[0x20, 0x20, 0x20, 0xff]);
        }
        let base = Image::new_owned(rgba, DOT_ICON_SIZE, DOT_ICON_SIZE);
        let badge = badge_image(&base, Dot::Error);
        assert_eq!(
            (badge.width(), badge.height()),
            (DOT_ICON_SIZE, DOT_ICON_SIZE)
        );
        assert_ne!(
            badge.rgba(),
            base.rgba(),
            "the badge must change at least one pixel"
        );

        // The fill sits inside the white ring near the bottom-right corner; the
        // exact centre is (width - radius - margin, height - radius - margin).
        let centre = ((23 * DOT_ICON_SIZE + 23) * 4) as usize;
        let pixel = &badge.rgba()[centre..centre + 4];
        assert!(
            pixel[0] > pixel[1] && pixel[0] > pixel[2],
            "expected the red attention badge, got {pixel:?}"
        );
        assert_eq!(pixel[3], 255, "the base icon is opaque and stays opaque");
    }

    #[test]
    fn labels_are_safe_for_a_native_menu() {
        assert_eq!(menu_label("  A \n B  "), "A B");
        assert_eq!(menu_label("A&B"), "A&&B");
        assert_eq!(menu_label("   "), "未命名");
        let long = "字".repeat(MAX_LABEL_CHARS + 10);
        let label = menu_label(&long);
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn menu_ids_round_trip_and_quit_is_not_a_tab() {
        assert_eq!(tab_item_id("abc"), "tray:tab:abc");
        assert_eq!(tab_id_from_menu_id("tray:tab:abc"), Some("abc"));
        assert_eq!(tab_id_from_menu_id(QUIT_ITEM_ID), None);
        assert_eq!(tab_id_from_menu_id("something-else"), None);
    }

    #[test]
    fn hiding_on_close_needs_both_the_preference_and_a_tray() {
        assert!(should_hide_on_close(true, true));
        assert!(!should_hide_on_close(true, false));
        assert!(!should_hide_on_close(false, true));
        assert!(!should_hide_on_close(false, false));
    }

    #[test]
    fn a_filled_dot_has_colour_and_an_idle_dot_is_a_ring() {
        let filled = render_dot([0x22, 0xc5, 0x5e], true);
        let ring = render_dot([0x81, 0x85, 0x8c], false);
        assert_eq!(filled.len(), (DOT_ICON_SIZE * DOT_ICON_SIZE * 4) as usize);
        assert_eq!(ring.len(), filled.len());
        assert!(
            filled.iter().skip(3).step_by(4).any(|alpha| *alpha > 0),
            "a filled dot must paint something"
        );
        assert!(
            ring.iter().skip(3).step_by(4).any(|alpha| *alpha > 0),
            "an idle ring must paint something"
        );
        // The centre of a ring is transparent; the centre of a filled dot is not.
        let centre = ((DOT_ICON_SIZE / 2 * DOT_ICON_SIZE + DOT_ICON_SIZE / 2) * 4) as usize;
        assert_eq!(ring[centre + 3], 0);
        assert!(filled[centre + 3] > 0);
    }
}
