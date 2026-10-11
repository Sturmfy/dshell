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
//! | [`commands`] | the `invoke` surface (all `async`) |

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// `Failure` is ~128 bytes, which clippy flags on every `Result<T, Failure>`.
// Tauri's command macro requires the error type by value, and the frontend
// contract *is* that structure (code + message + evidence + actions), so boxing
// it would only move the cost and obscure the API.
#![allow(clippy::result_large_err)]

pub mod commands;
pub mod config;
pub mod dialog;
pub mod dsh;
pub mod env;
pub mod error;
pub mod root;
pub mod runtime;
pub mod state;
pub mod tabs;
pub mod win;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{
    webview::WebviewBuilder, LogicalPosition, LogicalSize, Manager, WebviewUrl, WindowEvent,
};

use crate::config::Config;
use crate::state::AppState;
use crate::tabs::manager::{TabManager, CHROME_LABEL};
use crate::tabs::state::ConnectionKind;

pub const MAIN_WINDOW: &str = "main";

pub fn main() {
    let data_root_override = parse_args(std::env::args().skip(1));
    let (data_root, config) = state::bootstrap(data_root_override);

    let builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::list_tabs,
            commands::active_tab,
            commands::set_chrome_mode,
            commands::new_tab,
            commands::close_tab,
            commands::activate_tab,
            commands::rename_tab,
            commands::reorder_tabs,
            commands::restart_tab,
            commands::reload_tab,
            commands::clear_tab_data,
            commands::focus_chrome,
            commands::open_in_system_browser,
            commands::shortcut,
            commands::tab_diagnostics,
            commands::tab_failure_detail,
            commands::list_runtimes,
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
        .run(|_app, _event| {});
}

fn setup_app(
    app: &mut tauri::App,
    data_root: root::DataRoot,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();

    // Leftovers from an unclean exit are worth reporting: job objects should have
    // cleaned up, but a user deserves to know the check happened.
    let boot_note = detect_unclean_exit(&data_root);

    let window_config = config.window.clone();
    let mut window_builder = tauri::window::WindowBuilder::new(app, MAIN_WINDOW)
        .title("dshell")
        .inner_size(window_config.width, window_config.height)
        .min_inner_size(900.0, 560.0)
        .decorations(false)
        .resizable(true)
        .visible(true);
    window_builder = match (window_config.x, window_config.y) {
        (Some(x), Some(y)) => window_builder.position(x, y),
        _ => window_builder.center(),
    };
    let window = window_builder.build()?;

    let size = window.inner_size().unwrap_or_default();
    let scale = window.scale_factor().unwrap_or(1.0);
    let width = size.width as f64 / scale;
    let bar = tabs::layout::TAB_BAR_HEIGHT;

    // The chrome webview is dshell's own UI and the only holder of IPC access.
    window.add_child(
        WebviewBuilder::new(CHROME_LABEL, WebviewUrl::App("index.html".into()))
            .data_directory(data_root.webview_data_dir().join("chrome"))
            .disable_drag_drop_handler(),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(width, bar),
    )?;

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

    let state = Arc::new(AppState::new(data_root, manager, config));
    if let Some(note) = boot_note {
        let mut slot = tauri::async_runtime::block_on(state.boot_note.lock());
        let merged = match slot.take() {
            Some(existing) => format!("{existing}\n{note}"),
            None => note,
        };
        *slot = Some(merged);
    }
    app.manage(state.clone());

    if window_config.maximized {
        let _ = window.maximize();
    }

    restore_tabs(state);
    Ok(())
}

/// Reopen the tabs from the previous session.
///
/// `LocalSpawn` tabs are simply started again — dshell always captures a fresh
/// launch token, so nothing needs to be remembered. `Attach`/`Remote` tabs have
/// no URL on disk (tokens are never persisted, by design), so they come back as a
/// prompt to paste the URL again — unless their webview still holds a valid
/// session cookie, in which case they reconnect silently.
fn restore_tabs(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let (file, note) = state.load_tabs().await;
        if let Some(note) = note {
            let mut slot = state.boot_note.lock().await;
            *slot = Some(note);
        }
        if file.tabs.is_empty() {
            return;
        }
        let order = if file.order.is_empty() {
            file.tabs.iter().map(|spec| spec.id.clone()).collect::<Vec<_>>()
        } else {
            file.order.clone()
        };

        for id in order {
            let Some(mut spec) = file.tabs.iter().find(|spec| spec.id == id).cloned() else {
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
            }
        }
        state.manager.changed().await;
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

fn detect_unclean_exit(data_root: &root::DataRoot) -> Option<String> {
    let marker = data_root.path.join("session.lock");
    let previous = std::fs::read_to_string(&marker).ok();
    let _ = std::fs::write(
        &marker,
        serde_json::json!({
            "pid": std::process::id(),
            "startedAt": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        })
        .to_string(),
    );
    previous.map(|_| {
        "上次 dshell 未正常退出。子进程由作业对象回收；若仍有残留的 node 进程，\
         可在任务管理器中结束它们。"
            .to_string()
    })
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
        std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ())
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
        std::process::Command::new("xdg-open").arg(target).spawn().map(|_| ())
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
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    let verb = wide(verb);
    let target = wide(target);
    let params = params.as_deref().map(wide);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            params.as_ref().map(|p| p.as_ptr()).unwrap_or(std::ptr::null()),
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
