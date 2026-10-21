//! dshell — a tabbed desktop shell for DeepSeek Harness.
//!
//! ## The one architectural invariant
//!
//! dshell never sits in the middle of a DSH conversation. It starts (or is told
//! about) a `dsh web` server, captures the launch URL from its stdout, and hands
//! that URL to a **top-level** webview. The DSH frontend then talks to its own
//! backend directly, with a session cookie only it can see.
//!
//! That is not a stylistic choice. DSH's session cookie is `SameSite=Strict` and
//! bound to the host authority, and its `/api` trust fence rejects `Origin: null`
//! and any cross-site request — so an `<iframe>`, or a `fetch` from dshell's own
//! origin, can never work. One content webview per tab is the only arrangement
//! that does. Everything else in this crate follows from it.
//!
//! ## Module map
//!
//! | Module | Owns |
//! |---|---|
//! | [`root`] | the data root (`DSHELL_HOME`) and its four-level fallback chain |
//! | [`config`] | `config.json` / `tabs.json` (never a token) |
//! | [`runtime`] | the four backends: npx, npm, managed tree, arbitrary binary |
//! | [`dsh`] | launch-URL parsing, process supervision, credential-free probing |
//! | [`tabs`] | the **only** module allowed to touch multi-webview APIs |
//! | [`bridge`] | the narrow WebView2 message bridge injected into content tabs |
//! | [`picker`] | window-owned native directory/file pickers |
//! | [`webview_settings`] | WebView2 policy for dshell's own chrome and page tabs |
//! | [`tray`] | the notification-area icon, its native menu, and close-to-tray |
//! | [`window_state`] | the saved window frame (`window.json`) and its monitor |
//! | [`commands`] | the `invoke` surface (all `async`) |

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// `Failure` is ~128 bytes, which clippy flags on every `Result<T, Failure>`.
// Tauri's command macro requires the error type by value, and the frontend
// contract *is* that structure (code + message + evidence + actions), so boxing
// it would only move the cost and obscure the API.
#![allow(clippy::result_large_err)]

pub mod bridge;
pub mod commands;
pub mod config;
pub mod dialog;
pub mod dsh;
pub mod env;
pub mod error;
pub mod picker;
pub mod root;
pub mod runtime;
pub mod state;
pub mod tabs;
pub mod tray;
pub mod webview_settings;
pub mod win;
pub mod window_state;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{
    webview::WebviewBuilder, LogicalPosition, LogicalSize, Manager, WebviewUrl, WindowEvent,
};

use crate::config::Config;
use crate::state::AppState;
use crate::tabs::manager::{TabManager, CHROME_LABEL};
use crate::tabs::state::ConnectionKind;

pub const MAIN_WINDOW: &str = "main";

/// The strip handles the action after the styled popup closes.
pub const TAB_MENU_EVENT: &str = "dshell://tab-menu";

/// The reusable popup's own open payload: `{ tabId, page, theme? }`.
pub const TAB_MENU_OPEN_EVENT: &str = "dshell://tab-menu-open";

/// DSH's sidebar surface, reported by the content bridge and consumed by chrome.
pub const TAB_THEME_EVENT: &str = "dshell://tab-theme";

/// A saved preference changed outside the settings page (currently a tab
/// rename updating a history label). Payload is the complete `Config`.
pub const CONFIG_CHANGED_EVENT: &str = "dshell://config-changed";

/// Marker whose presence at startup means the previous run did not reach a clean
/// exit. Written when the app boots, removed by the exit hook in [`main`].
const SESSION_MARKER: &str = "session.lock";

/// Version of the marker's contents.
///
/// A marker without it was written by a build that never removed its marker, so
/// its presence says nothing about how that run ended. Treating one as an
/// unclean exit would produce a single false warning on the first launch after
/// upgrading — which looks exactly like this bug — so only markers this build
/// could have left behind are reported.
const SESSION_MARKER_FORMAT: u64 = 2;

pub fn main() {
    let data_root_override = parse_args(std::env::args().skip(1));
    let (data_root, config) = state::bootstrap(data_root_override);
    // Resolved before `data_root` moves into `setup_app`, because the exit hook
    // below outlives it.
    let session_marker = data_root.path.join(SESSION_MARKER);

    let builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::install_pnpm,
            commands::list_tabs,
            commands::active_tab,
            commands::open_page,
            commands::tab_closable,
            commands::page_ready,
            commands::client_step,
            commands::new_tab,
            commands::close_tab,
            commands::activate_tab,
            commands::rename_tab,
            commands::reorder_tabs,
            commands::restart_tab,
            commands::reload_tab,
            commands::show_tab_menu,
            commands::tab_menu_choose,
            commands::hide_tab_menu,
            commands::tab_menu_state,
            commands::tab_menu_measured,
            commands::clear_tab_data,
            commands::focus_chrome,
            commands::open_in_system_browser,
            commands::shortcut,
            commands::tab_diagnostics,
            commands::tab_failure_detail,
            commands::list_runtimes,
            commands::local_runtime_status,
            commands::prepare_runtime,
            commands::scan_instances,
            commands::probe_connection,
            commands::forget_connection,
            commands::set_extra_binary,
            commands::managed_install_add,
            commands::managed_install_remove,
            commands::get_config,
            commands::set_config,
            commands::complete_onboarding,
            commands::remove_env_var,
            commands::probe_writable_path,
            commands::open_path,
            commands::pick_directory,
            commands::pick_file,
            commands::notify_tabs_changed,
            commands::set_window_title,
            commands::persist_now,
            commands::session_state,
        ])
        .setup(move |app| {
            tabs::manager::install_global(app.handle().clone());
            setup_app(app, data_root, config)?;
            Ok(())
        });

    builder
        .build(tauri::generate_context!())
        .expect("failed to start dshell")
        .run(move |app, event| match event {
            // The first moment a webview can be built: the loop is running, so
            // the creation this spawn waits on lands. It warms the tab-menu
            // popup before the first right-click needs it; a failure there is
            // not fatal, because the click falls back to creating it.
            tauri::RunEvent::Ready => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    commands::prewarm_tab_menu(&app).await;
                });
            }
            // The reused menu popup is a real window even while hidden, and the
            // app only exits once the last window is gone. Take it down with the
            // main window, or closing the app would leave the process running.
            tauri::RunEvent::WindowEvent { label, event, .. } => {
                if label == MAIN_WINDOW && matches!(event, tauri::WindowEvent::Destroyed) {
                    // Deferred: this callback already runs inside a window
                    // message, and destroying another window from here would
                    // re-enter that dispatch.
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        commands::dispose_tab_menu(&app);
                    });
                }
            }
            // The *absence* of the marker is what "the last run exited cleanly"
            // means, so removing it here is the only thing that keeps the
            // warning truthful. `RunEvent::Exit` is emitted by the event loop
            // itself once the last window is gone — which is exactly the path the
            // close button takes — while a crash or a force kill never reaches
            // it. That is the case the warning exists for.
            tauri::RunEvent::Exit => {
                // The window may already be gone, so this reads the copy the
                // tracker has been keeping rather than the window itself. The two
                // close paths already wrote it; this catches the tray's 退出,
                // `app.exit()`, and a build where the tray failed to install.
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    state.window.persist();
                }
                clear_session_marker(&session_marker);
            }
            _ => {}
        });
}

fn setup_app(
    app: &mut tauri::App,
    data_root: root::DataRoot,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();

    // Leftovers from an unclean exit are worth reporting: job objects should have
    // cleaned up, but a user deserves to know the check happened.
    let boot_note = detect_unclean_exit(&data_root.path.join(SESSION_MARKER));

    let window_config = config.window.clone();

    // Where the window opens is decided before it exists: the remembered frame
    // wins, and the config's size is only the first-launch default. Monitors are
    // read through the app handle because there is no window to ask yet.
    let window_file = data_root.window_file();
    let saved = window_state::load(&window_file);
    let monitors = window_state::monitors(app.handle());
    let primary = window_state::primary_monitor(app.handle());
    let placement = match saved.as_ref() {
        Some(saved) => window_state::restore(saved, &monitors, primary.as_ref()),
        None => window_state::first_launch(
            window_config.width,
            window_config.height,
            window_config.maximized,
            &monitors,
            primary.as_ref(),
        ),
    };
    let data_root_path = data_root.path.clone();
    crate::dsh::process::debug_log(
        &data_root_path,
        &format!(
            "window: {} {}x{} at {},{}{}",
            if saved.is_some() {
                "restoring"
            } else {
                "starting"
            },
            placement.frame.width,
            placement.frame.height,
            placement.frame.x,
            placement.frame.y,
            placement
                .monitor
                .as_ref()
                .and_then(|monitor| monitor.name.clone())
                .map(|name| format!(" on {name}"))
                .unwrap_or_default(),
        ),
    );

    // Created hidden and placed while nothing can paint. The window then appears
    // where it belongs, instead of at the OS default spot followed by a jump.
    let window = tauri::window::WindowBuilder::new(app, MAIN_WINDOW)
        .title("dshell")
        .inner_size(window_config.width, window_config.height)
        .min_inner_size(900.0, 560.0)
        .decorations(false)
        .resizable(true)
        .visible(false)
        .build()?;
    window_state::apply(&window, &placement);

    let size = window.inner_size().unwrap_or_default();
    let scale = window.scale_factor().unwrap_or(1.0);
    let width = size.width as f64 / scale;
    let bar = tabs::layout::TAB_BAR_HEIGHT;

    // The chrome webview is dshell's own UI and the only holder of IPC access.
    let chrome = window.add_child(
        WebviewBuilder::new(CHROME_LABEL, WebviewUrl::App("index.html".into()))
            .data_directory(data_root.webview_data_dir().join("chrome"))
            .disable_drag_drop_handler(),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(width, bar),
    )?;
    webview_settings::disable_context_menu(&chrome);

    let manager = Arc::new(TabManager::new(window.clone(), handle.clone()));
    {
        let handle_for_resize = handle.clone();
        window.on_window_event(move |event| match event {
            // Child webviews are fixed-position by default, and `auto_resize` is
            // a proportional scaler rather than a layout manager, so bounds are
            // recomputed here on every geometry change.
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(state) = handle_for_resize.try_state::<Arc<AppState>>() {
                    state.manager.relayout();
                }
            }
            _ => {}
        });
    }

    let tracker =
        window_state::Tracker::new(window_file, data_root_path.clone(), placement.record());
    let state = Arc::new(AppState::new(data_root, manager, config, tracker));
    if let Some(note) = boot_note {
        let mut slot = tauri::async_runtime::block_on(state.boot_note.lock());
        let merged = match slot.take() {
            Some(existing) => format!("{existing}\n{note}"),
            None => note,
        };
        *slot = Some(merged);
    }
    app.manage(state.clone());

    // Keep the remembered frame current on every geometry change, so the close
    // handlers only have to write it out. Registered after `manage`, which is
    // the only way the handler can reach the state; the initial placement was
    // seeded into the tracker above, so nothing before this point is lost.
    {
        let handle = handle.clone();
        window.on_window_event(move |event| {
            if !matches!(
                event,
                WindowEvent::Moved(_)
                    | WindowEvent::Resized(_)
                    | WindowEvent::ScaleFactorChanged { .. }
            ) {
                return;
            }
            let Some(state) = handle.try_state::<Arc<AppState>>() else {
                return;
            };
            if let Some(window) = handle.get_window(MAIN_WINDOW) {
                state.window.remember(&window);
            }
        });
    }

    // The tray must exist before tabs restore: the restore path emits
    // `tabs-changed` for every tab it opens, and those are what keep the native
    // menu in step. A failed install leaves the window's old close behaviour in
    // place, so a close can never hide the window into a tray that is not there.
    let _ = tray::install(app.handle());

    // Shown only now: the frame is applied and the strip exists, so the window
    // never appears at the OS default position and then jumps. Maximizing comes
    // after the show because tao skips the request while the window is hidden.
    if let Err(err) = window.show() {
        crate::dsh::process::debug_log(&data_root_path, &format!("window: show failed: {err}"));
    }
    if placement.maximized {
        let _ = window.maximize();
    }
    // The resize handler may not have run yet for the size applied above.
    state.manager.relayout();

    restore_tabs(state);
    Ok(())
}
/// Boot sequence: restore the user's tabs, then make sure the window shows
/// *something* — always as a tab.
///
/// The order matters. Restoring first means an unfinished wizard appears
/// alongside the user's tabs rather than instead of them.
///
/// Three cases, and every one of them ends with at least one tab:
///
/// | state | what opens |
/// |---|---|
/// | nothing restored, wizard unfinished | the wizard, **active** |
/// | tabs restored, wizard unfinished | the wizard, behind them |
/// | nothing restored, wizard done | a new-connection tab |
///
/// The last row is the point of the change that made dshell's pages tabs: an
/// empty window with a blank content area is not a state this app has any more.
fn restore_tabs(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let (file, note) = state.load_tabs().await;
        if let Some(note) = note {
            let mut slot = state.boot_note.lock().await;
            *slot = Some(note);
        }
        let order = if file.order.is_empty() {
            file.tabs
                .iter()
                .map(|spec| spec.id.clone())
                .collect::<Vec<_>>()
        } else {
            file.order.clone()
        };

        for id in order {
            // A `tabs.json` written by an older build can still contain page
            // entries; they are recreated below from config instead.
            let Some(mut spec) = file
                .tabs
                .iter()
                .find(|spec| spec.id == id && !spec.connection.is_page())
                .cloned()
            else {
                continue;
            };
            match spec.connection.clone() {
                ConnectionKind::LocalSpawn { .. } => {
                    spec.display_origin = None;
                    let instance_home = state.instance_home_for(&spec);
                    if state
                        .manager
                        .open_tab(spec.clone(), instance_home)
                        .await
                        .is_ok()
                    {
                        let state_for_spawn = state.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = state_for_spawn.manager.spawn_connection(spec).await;
                        });
                    }
                }
                ConnectionKind::LocalAttach { .. } | ConnectionKind::Remote { .. } => {
                    if state.manager.open_tab(spec.clone(), None).await.is_err() {
                        continue;
                    }
                    let has_session = state.manager.probe_session(&id).await;
                    let origin = spec.display_origin.clone();
                    if has_session {
                        if let Some(origin) = origin {
                            let state_for_connect = state.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = state_for_connect
                                    .manager
                                    .connect_attach(spec, format!("{origin}/"), false, true)
                                    .await;
                            });
                        }
                    } else {
                        state
                            .manager
                            .fail(
                                &id,
                                error::Failure::new(
                                    error::FailureCode::AttachAuthRequired,
                                    "需要重新提供启动 URL",
                                )
                                .with_detail(
                                    "token 不会被写入磁盘（这是刻意的）。请在运行 `dsh web` \
                                     的终端里复制整行输出，然后重新连接。",
                                )
                                .with_stage("restore")
                                .with_defaults(),
                            )
                            .await
                            .ok();
                    }
                }
                ConnectionKind::Page { .. } => {}
            }
        }

        let restored_any = !state.manager.tab_ids().await.is_empty();
        let onboarding_done = state.onboarding_done().await;
        state.manager.changed().await;

        if !onboarding_done {
            // The wizard is a tab like any other; it simply takes focus on a
            // fresh install, and stays in the background once the user has tabs
            // of their own to look at.
            let _ = state
                .manager
                .open_page(tabs::state::PageKind::Onboarding, !restored_any)
                .await;
        } else if !restored_any {
            let _ = state
                .manager
                .open_page(tabs::state::PageKind::NewTab, true)
                .await;
        }
        state.persist_tabs().await;
    });
}

/// `--data-root <path>` overrides the fallback chain (useful for tests and for
/// running two dshell instances side by side).
fn parse_args(args: impl Iterator<Item = String>) -> Option<PathBuf> {
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--data-root=") {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
            continue;
        }
        if arg == "--data-root" {
            if let Some(value) = args.next() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// Arm the session marker and report whether the *previous* run left one behind.
///
/// The write has to happen on every boot, before any window exists: a run that
/// dies during start-up must be distinguishable from one that never started.
/// Only [`clear_session_marker`] undoes it.
fn detect_unclean_exit(marker: &Path) -> Option<String> {
    let previous = std::fs::read_to_string(marker).ok();
    let _ = std::fs::write(
        marker,
        serde_json::json!({
            "format": SESSION_MARKER_FORMAT,
            "pid": std::process::id(),
            "startedAt": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        })
        .to_string(),
    );
    let unclean = previous.is_some_and(|text| {
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("format").and_then(|format| format.as_u64()))
            == Some(SESSION_MARKER_FORMAT)
    });
    unclean.then(|| {
        "上次 dshell 未正常退出。子进程由作业对象回收；若仍有残留的 node 进程，\
         可在任务管理器中结束它们。"
            .to_string()
    })
}

/// Mark this run as cleanly exited.
///
/// Missing is fine — a run that could not write the marker (read-only data root)
/// never armed it in the first place.
fn clear_session_marker(marker: &Path) {
    let _ = std::fs::remove_file(marker);
}

/// Open a URL with the user's default browser — the escape hatch every tab
/// offers when a session cannot be established.
pub fn shell_open(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        shell_execute("open", url, None)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

/// Reveal a file (or open a directory) in the OS file manager.
pub fn shell_reveal(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy().to_string();
        if path.is_dir() {
            shell_execute("open", &text, None)
        } else {
            shell_execute("open", "explorer.exe", Some(format!("/select,\"{text}\"")))
        }
    }
    #[cfg(not(windows))]
    {
        let target = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map(|_| ())
    }
}

#[cfg(windows)]
fn shell_execute(verb: &str, target: &str, params: Option<String>) -> std::io::Result<()> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> *mut c_void;
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let verb = wide(verb);
    let target = wide(target);
    let params = params.as_deref().map(wide);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            params
                .as_ref()
                .map(|p| p.as_ptr())
                .unwrap_or(std::ptr::null()),
            std::ptr::null(),
            1,
        )
    };
    // ShellExecuteW returns a value <= 32 on failure.
    if result as isize <= 32 {
        return Err(std::io::Error::other(format!(
            "ShellExecuteW failed ({})",
            result as isize
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker in its own directory, so tests never touch a real data root.
    fn marker_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dshell-session-marker-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        dir.join(SESSION_MARKER)
    }

    #[test]
    fn a_first_launch_has_nothing_to_report() {
        let marker = marker_path("first");
        assert!(detect_unclean_exit(&marker).is_none());
        // The marker is armed for the *next* launch, which is what makes its
        // absence meaningful.
        assert!(marker.exists());
    }

    #[test]
    fn a_leftover_marker_is_an_unclean_exit() {
        let marker = marker_path("leftover");
        assert!(detect_unclean_exit(&marker).is_none());
        assert!(
            detect_unclean_exit(&marker).is_some(),
            "the second boot must see the first one's marker"
        );
    }

    #[test]
    fn a_marker_from_before_this_fix_is_not_blamed() {
        let marker = marker_path("legacy");
        // The pre-fix marker: no `format` field, because that build never
        // removed it and so could not tell a clean exit from a crash.
        std::fs::write(&marker, r#"{"pid":1234,"startedAt":1}"#).unwrap();
        assert!(detect_unclean_exit(&marker).is_none());
        // Once this build has armed it, the next unclean exit *is* reported.
        assert!(detect_unclean_exit(&marker).is_some());
    }

    #[test]
    fn a_clean_exit_leaves_nothing_for_the_next_boot() {
        let marker = marker_path("clean");
        assert!(detect_unclean_exit(&marker).is_none());
        clear_session_marker(&marker);
        assert!(!marker.exists());
        assert!(detect_unclean_exit(&marker).is_none());
    }

    #[test]
    fn clearing_an_absent_marker_is_not_an_error() {
        let marker = marker_path("absent");
        clear_session_marker(&marker);
        assert!(!marker.exists());
    }
}
