//! The `invoke` surface.
//!
//! Every command that can reach a webview API (`add_child`, `navigate`,
//! `cookies_for_url`) is `async`. That is not style: on Windows those calls
//! deadlock when made from a synchronous command or an event handler (wry#583).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::config::{Config, DshHomeSetting, ManagedInstall};
use crate::error::{Action, Failure, FailureCode};
use crate::runtime::node::EnvironmentSnapshot;
use crate::runtime::{self, ManagedMode, RuntimeSpec};
use crate::state::AppState;
use crate::tabs::manager::TabManager;
use crate::tabs::state::{ConnectionKind, PageKind, TabSpec, TabView};

/// Tauri hands commands a `State<'_, T>`: the lifetime is the command's, not
/// `'static`, so this alias stays generic over it.
type Shared<'a> = State<'a, Arc<AppState>>;

fn state_of(app: &AppHandle) -> Result<Arc<AppState>, Failure> {
    app.try_state::<Arc<AppState>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| Failure::new(FailureCode::Internal, "应用状态尚未就绪"))
}

// ---------------------------------------------------------------- app info

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootView {
    pub path: String,
    pub source: String,
    pub source_label: String,
    pub note: Option<String>,
    pub config_file: String,
    pub runtimes_dir: String,
    pub webview_data_dir: String,
    pub env_var: String,
    pub env_value: Option<String>,
    pub default_dsh_home: Option<String>,
    pub default_agents_home: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub pinned_dsh_version: String,
    pub root: RootView,
    pub tabs: Vec<TabView>,
    pub config: Config,
    pub environment: EnvironmentSnapshot,
    pub boot_note: Option<String>,
}

#[tauri::command]
pub async fn app_info(
    app: AppHandle,
    refresh_environment: Option<bool>,
) -> Result<AppInfo, Failure> {
    let state = state_of(&app)?;
    let environment = state.environment(refresh_environment.unwrap_or(false)).await;
    let config = state.config_snapshot().await;
    let (tabs, _active) = state.manager.snapshot().await;
    let boot_note = state.boot_note.lock().await.clone();
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pinned_dsh_version: crate::config::PINNED_DSH_VERSION.to_string(),
        root: root_view(&state),
        tabs,
        config,
        environment,
        boot_note,
    })
}

fn root_view(state: &AppState) -> RootView {
    RootView {
        path: state.data_root.path.to_string_lossy().to_string(),
        source: serde_json::to_string(&state.data_root.source)
            .unwrap_or_else(|_| "\"portable\"".into())
            .trim_matches('"')
            .to_string(),
        source_label: state.data_root.source.label().to_string(),
        note: state.data_root.note.clone(),
        config_file: state.data_root.config_file().to_string_lossy().to_string(),
        runtimes_dir: state.data_root.runtimes_dir().to_string_lossy().to_string(),
        webview_data_dir: state.data_root.webview_data_dir().to_string_lossy().to_string(),
        env_var: crate::root::ENV_VAR.to_string(),
        env_value: std::env::var(crate::root::ENV_VAR).ok(),
        default_dsh_home: crate::root::preview_default_dsh_home()
            .map(|path| path.to_string_lossy().to_string()),
        default_agents_home: crate::root::preview_default_agents_home()
            .map(|path| path.to_string_lossy().to_string()),
    }
}

// --------------------------------------------------------------- tab list

/// The whole tab picture in one call: the tabs and which one is visible. The
/// frontend renders from this and never infers activation itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabsSnapshot {
    pub tabs: Vec<TabView>,
    pub active_id: Option<String>,
}

#[tauri::command]
pub async fn list_tabs(state: Shared<'_>) -> Result<TabsSnapshot, Failure> {
    let (tabs, active_id) = state.manager.snapshot().await;
    Ok(TabsSnapshot { tabs, active_id })
}

#[tauri::command]
pub async fn active_tab(state: Shared<'_>) -> Result<Option<String>, Failure> {
    Ok(state.manager.active_id().await)
}

/// Open one of dshell's own pages as a tab, or focus it if it is already open.
///
/// This replaces the old `set_panel`: dshell's pages are ordinary tabs now, so
/// "open settings" is just "open the settings tab", and the window's layout,
/// focus model and close rules stay exactly the ones every other tab uses.
#[tauri::command]
pub async fn open_page(state: Shared<'_>, page: String) -> Result<String, Failure> {
    let kind = PageKind::parse(&page).ok_or_else(|| {
        Failure::new(FailureCode::Internal, format!("未知的页面：{page}"))
    })?;
    state.manager.open_page(kind, true).await
}

/// Whether a tab may be closed right now.
///
/// The strip renders from `TabView::closable`, which carries the same rule, so
/// this is the on-demand form of it — for a page that wants to ask about a tab
/// other than the one it is showing. Both go through `PageKind::closable`, so
/// there is exactly one answer to "can this be closed".
#[tauri::command]
pub async fn tab_closable(state: Shared<'_>, tab_id: String) -> Result<bool, Failure> {
    Ok(state.manager.is_closable(&tab_id).await)
}

/// Report that a page tab finished its own start-up.
///
/// This exists to make one specific failure visible. dshell's pages are served
/// from dshell's origin and reach the `invoke` surface only while they stay
/// there (the `dshell-ui` capability matches `page-*` *and* a local origin). If
/// that ever stopped holding, the page would still render its static skeleton —
/// it would simply never fill in, because every command it needs would be
/// rejected. That is indistinguishable from a slow load on screen, so the page
/// reports the outcome and it lands in the debug log.
///
/// `error` is the browser's message when the page's first command failed, so a
/// denial ("not allowed by ACL") reads differently from a slow or broken
/// command.
#[tauri::command]
pub async fn page_ready(
    app: AppHandle,
    page: String,
    tab_id: String,
    ok: bool,
    error: Option<String>,
) -> Result<(), Failure> {
    let root = state_of(&app)?.data_root.path.clone();
    crate::dsh::process::debug_log(
        &root,
        &format!(
            "page {page} ({tab_id}) ipc {}",
            match (&ok, error.as_deref()) {
                (true, _) => "ok".to_string(),
                (false, Some(error)) => format!("FAILED: {error}"),
                (false, None) => "FAILED".to_string(),
            }
        ),
    );
    Ok(())
}

/// One step of a page's start-up, reported for the debug log.
///
/// Pairs with [`page_ready`]: between them they say how far a page got before
/// something stopped it, which is the difference between "the wizard is broken"
/// and "the wizard's second half is broken".
#[tauri::command]
pub async fn client_step(app: AppHandle, page: String, step: String) -> Result<(), Failure> {
    let root = state_of(&app)?.data_root.path.clone();
    crate::dsh::process::debug_log(&root, &format!("page {page}: {step}"));
    Ok(())
}

// ------------------------------------------------------------- new tab

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NewTabRequest {
    /// Start a backend with dshell's runtime manager.
    Launch {
        /// Optional. DSH picks the project folder itself, so this only exists to
        /// give the backend a starting directory; unset means "use the default".
        #[serde(default)]
        workspace: Option<String>,
        runtime_id: String,
        #[serde(default)]
        dsh_home: Option<DshHomeSetting>,
        #[serde(default)]
        kill_on_close: Option<bool>,
        #[serde(default)]
        title: Option<String>,
    },
    /// Connect to an instance dshell did not start.
    Attach {
        url: String,
        #[serde(default)]
        remote: bool,
        #[serde(default)]
        allow_plaintext: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTabResult {
    pub id: String,
    pub instance_key: String,
}

#[tauri::command]
pub async fn new_tab(app: AppHandle, request: NewTabRequest) -> Result<NewTabResult, Failure> {
    let state = state_of(&app)?;
    let config = state.config_snapshot().await;
    let id = TabManager::new_tab_id();

    match request {
        NewTabRequest::Launch { workspace, runtime_id, dsh_home, kill_on_close, title } => {
            // A directory the user chose is validated here: a bad path is a form
            // error, not a tab state. Leaving it empty is the normal case — DSH
            // selects the project itself, and dshell starts the backend in the
            // user's home directory meanwhile.
            let chosen_workspace = workspace
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let workspace_path = match &chosen_workspace {
                Some(chosen) => {
                    let path = PathBuf::from(chosen);
                    if !path.is_dir() {
                        return Err(Failure::new(
                            FailureCode::WorkspaceMissing,
                            format!("工作目录不存在：{chosen}"),
                        )
                        .with_detail("留空即可，DSH 会在自己的界面里让你选择项目目录。"));
                    }
                    if let Err(err) = crate::env::probe_writable(&path) {
                        return Err(Failure::new(
                            FailureCode::WorkspaceMissing,
                            format!("工作目录不可写：{chosen}"),
                        )
                        .with_detail(err.to_string()));
                    }
                    path
                }
                None => PathBuf::new(),
            };

            let dsh_home_setting = dsh_home.unwrap_or_else(|| config.dsh_home.clone());
            // Resolve up-front so a bad DSH_HOME is reported by the form instead
            // of by a tab that has already appeared.
            dsh_home_setting
                .resolve(
                    &state.data_root.path,
                    &state.data_root.dsh_home_dir(),
                    &workspace_path,
                )
                .map_err(|failure| failure.with_defaults())?;

            let runtime_spec = resolve_runtime(&config, &runtime_id, &state).await?;
            let title = title.unwrap_or_else(|| {
                match chosen_workspace.as_deref().and_then(|chosen| {
                    PathBuf::from(chosen)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                }) {
                    Some(name) => format!("本地 · {name}"),
                    None => "本地".to_string(),
                }
            });

            let spec = TabSpec {
                id: id.clone(),
                title,
                display_origin: None,
                connection: ConnectionKind::LocalSpawn {
                    workspace: chosen_workspace.clone(),
                    dsh_home: dsh_home_setting,
                    dsh_agents_home: Some(config.dsh_agents_home.clone()),
                    runtime: runtime_spec,
                    kill_on_close: kill_on_close.unwrap_or(config.kill_on_close),
                },
            };

            let instance_home = state.instance_home_for(&spec);
            let instance_key = state.manager.open_tab(spec.clone(), instance_home).await?;
            state.manager.activate(Some(&id)).await;
            // Only directories the user picked are remembered as projects.
            if !workspace_path.as_os_str().is_empty() {
                state
                    .update_config(|config| {
                        config.remember_workspace(&workspace_path);
                    })
                    .await
                    .ok();
            }
            state.persist_tabs().await;

            // Spawning runs in the background: npx on a cold cache can take
            // minutes, and the tab must appear immediately.
            let state_for_spawn = state.clone();
            tauri::async_runtime::spawn(async move {
                let _ = state_for_spawn.manager.spawn_connection(spec).await;
            });

            Ok(NewTabResult { id, instance_key })
        }
        NewTabRequest::Attach { url, remote, allow_plaintext } => {
            let parsed = crate::dsh::url::LaunchUrl::parse(&url).ok_or_else(|| {
                Failure::new(FailureCode::AttachNotDsh, "无法解析该地址")
                    .with_detail("需要 `http://host:port/?token=…` 形式。")
            })?;
            if !parsed.has_token() {
                return Err(Failure::new(
                    FailureCode::AttachAuthRequired,
                    "URL 里没有 token 参数，无法建立会话",
                )
                .with_detail(
                    "请在运行 `dsh web` 的终端里复制整行输出（形如 \
                     `dsh web: http://127.0.0.1:9808/?token=…`），连 token 一起粘贴。",
                ));
            }
            if remote && !parsed.is_loopback() && parsed.scheme == "http" && !allow_plaintext {
                return Err(Failure::new(
                    FailureCode::RemoteUnreachable,
                    "明文 HTTP 远程连接需要显式确认",
                )
                .with_detail(
                    "明文连接意味着 token 与会话内容不经加密传输；\
                     建议改用 SSH 端口转发、VPN 或内网。",
                )
                .with_action(Action::primary("confirm-plaintext", "我已了解风险，继续")));
            }

            let title = if remote {
                format!("远程 · {}", parsed.host)
            } else {
                format!("本地 · {}", parsed.port)
            };
            let connection = if remote {
                ConnectionKind::Remote { url: url.clone(), allow_plaintext }
            } else {
                ConnectionKind::LocalAttach { url: url.clone() }
            };
            let spec = TabSpec {
                id: id.clone(),
                title,
                display_origin: Some(parsed.origin.clone()),
                connection,
            };
            let instance_key = state.manager.open_tab(spec.clone(), None).await?;
            state.manager.activate(Some(&id)).await;
            state.persist_tabs().await;

            let state_for_connect = state.clone();
            let has_session = state_for_connect.manager.probe_session(&id).await;
            tauri::async_runtime::spawn(async move {
                let _ = state_for_connect
                    .manager
                    .connect_attach(spec, url, allow_plaintext, has_session)
                    .await;
            });

            Ok(NewTabResult { id, instance_key })
        }
    }
}

/// Map a runtime id from the UI onto a concrete spec.
async fn resolve_runtime(
    config: &Config,
    runtime_id: &str,
    state: &AppState,
) -> Result<RuntimeSpec, Failure> {
    let pinned = package_spec(config);
    match runtime_id {
        "npx" => Ok(RuntimeSpec::Npx { package_spec: pinned }),
        "npm" => Ok(RuntimeSpec::Npm {
            root: state
                .data_root
                .runtimes_dir()
                .join("npm")
                .to_string_lossy()
                .to_string(),
            package_spec: pinned,
        }),
        id if id.starts_with("managed:") => {
            let wanted = id.trim_start_matches("managed:");
            let install = config
                .managed_installs
                .iter()
                .find(|install| install.id == wanted)
                .ok_or_else(|| {
                    Failure::new(FailureCode::Internal, "该本地维护项已不存在")
                        .with_action(Action::primary("settings", "打开设置"))
                })?;
            let root = PathBuf::from(&install.root);
            let mode = runtime::detect_managed_mode(&root).ok_or_else(|| {
                Failure::new(
                    FailureCode::NpxInstallFailed,
                    format!("{} 里没有找到 DSH", root.display()),
                )
                .with_detail("期望存在 node_modules/.bin/dsh.cmd，或 apps/cli/src/bin.ts。")
            })?;
            Ok(RuntimeSpec::Managed { root: install.root.clone(), mode })
        }
        "external" => {
            let binary = config.extra_binary.clone().ok_or_else(|| {
                Failure::new(FailureCode::NpxInstallFailed, "还没有指定后端二进制")
                    .with_action(Action::primary("pick-binary", "选择文件"))
            })?;
            Ok(RuntimeSpec::External { binary, label: None })
        }
        other => Err(Failure::new(
            FailureCode::Internal,
            format!("未知的运行时：{other}"),
        )),
    }
}

fn package_spec(config: &Config) -> String {
    match config.dsh_version_channel.as_str() {
        "latest" => "@deepseek-ai/dsh@latest".to_string(),
        "alpha" => "@deepseek-ai/dsh@alpha".to_string(),
        _ => format!("@deepseek-ai/dsh@{}", crate::config::PINNED_DSH_VERSION),
    }
}

// ------------------------------------------------------------ tab actions

#[tauri::command]
pub async fn close_tab(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.close_tab(&tab_id).await
}

#[tauri::command]
pub async fn activate_tab(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.activate(Some(&tab_id)).await;
    Ok(())
}

#[tauri::command]
pub async fn rename_tab(state: Shared<'_>, tab_id: String, title: String) -> Result<(), Failure> {
    state.manager.rename(&tab_id, title.trim()).await
}

#[tauri::command]
pub async fn reorder_tabs(state: Shared<'_>, tab_ids: Vec<String>) -> Result<(), Failure> {
    state.manager.reorder(tab_ids).await
}

#[tauri::command]
pub async fn restart_tab(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.restart(&tab_id).await
}

#[tauri::command]
pub async fn reload_tab(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.reload(&tab_id).await
}

#[tauri::command]
pub async fn clear_tab_data(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.clear_instance_data(&tab_id).await
}

#[tauri::command]
pub async fn focus_chrome(state: Shared<'_>) -> Result<(), Failure> {
    state.manager.focus_chrome().await;
    Ok(())
}

#[tauri::command]
pub async fn open_in_system_browser(state: Shared<'_>, tab_id: String) -> Result<(), Failure> {
    state.manager.open_in_system_browser(&tab_id).await
}

#[tauri::command]
pub async fn shortcut(app: AppHandle, action: String) -> Result<(), Failure> {
    let state = state_of(&app)?;
    state.manager.emit_shortcut(&action).await;
    Ok(())
}

#[tauri::command]
pub async fn session_state(state: Shared<'_>, tab_id: String) -> Result<bool, Failure> {
    Ok(state.manager.probe_session(&tab_id).await)
}

// ------------------------------------------------------------ diagnostics

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabDiagnostics {
    pub view: TabView,
    pub log_tail: Vec<crate::dsh::process::LogEntry>,
    pub command_line: Option<String>,
    pub node: Option<crate::runtime::node::NodeInfo>,
    pub data_root: String,
}

#[tauri::command]
pub async fn tab_diagnostics(
    state: Shared<'_>,
    tab_id: String,
) -> Result<TabDiagnostics, Failure> {
    let (tabs, _active) = state.manager.snapshot().await;
    let view = tabs
        .into_iter()
        .find(|view| view.id == tab_id)
        .ok_or_else(|| Failure::new(FailureCode::Internal, "选项卡不存在"))?;
    let log_tail = state.manager.log_tail(&tab_id).await;
    let data_root = state.data_root.path.to_string_lossy().to_string();
    let command_line = state.manager.command_line(&tab_id).await;
    Ok(TabDiagnostics {
        view,
        log_tail,
        command_line,
        node: state.node().await,
        data_root,
    })
}

/// Detail payload for dshell's own error page, which runs *inside* the failing
/// tab's webview. It can call back precisely because it is a dshell asset; a DSH
/// page never can.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabFailureDetail {
    pub failure: Option<Failure>,
    pub log_tail: Vec<crate::dsh::process::LogEntry>,
    pub command_line: Option<String>,
    pub workspace: Option<String>,
    pub dsh_home: Option<String>,
    pub runtime: Option<String>,
    pub data_dir: Option<String>,
    pub tail_text: String,
}

#[tauri::command]
pub async fn tab_failure_detail(
    state: Shared<'_>,
    tab_id: String,
) -> Result<TabFailureDetail, Failure> {
    let (tabs, _active) = state.manager.snapshot().await;
    let view = tabs.into_iter().find(|view| view.id == tab_id);
    let (failure, workspace, runtime_label) = match view {
        Some(view) => (view.failure.clone(), view.workspace.clone(), view.runtime.clone()),
        None => (None, None, None),
    };
    let log_tail = state.manager.log_tail(&tab_id).await;
    let tail_text = log_tail
        .iter()
        .rev()
        .take(60)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| format!("[{}] {}", entry.stream, entry.text))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(TabFailureDetail {
        failure,
        log_tail,
        command_line: state.manager.command_line(&tab_id).await,
        workspace,
        dsh_home: None,
        runtime: runtime_label,
        data_dir: state.manager.data_dir(&tab_id).await,
        tail_text,
    })
}

// ------------------------------------------------------------- runtime mgmt

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOption {
    pub id: String,
    pub label: String,
    pub description: String,
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[tauri::command]
pub async fn list_runtimes(state: Shared<'_>) -> Result<Vec<RuntimeOption>, Failure> {
    let config = state.config_snapshot().await;
    let node = state.node().await;
    let node_ok = node
        .as_ref()
        .map(|node| node.version_issue.is_none())
        .unwrap_or(false);
    let node_reason = node.as_ref().and_then(|node| node.version_issue.clone());

    let mut options = vec![
        RuntimeOption {
            id: "npm".into(),
            label: "本地 npm 安装（推荐）".into(),
            description: format!(
                "安装到 {}，版本锁定、不随 npm 缓存清理失效",
                state.data_root.runtimes_dir().join("npm").display()
            ),
            available: node_ok,
            reason: node_reason.clone(),
        },
        RuntimeOption {
            id: "npx".into(),
            label: "npx · 官方发行版".into(),
            description: format!(
                "无需安装，首次运行需下载约 450 MB（{}）",
                package_spec(&config)
            ),
            available: node_ok,
            reason: node_reason.clone(),
        },
    ];

    for install in &config.managed_installs {
        let mode = runtime::detect_managed_mode(&PathBuf::from(&install.root));
        options.push(RuntimeOption {
            id: format!("managed:{}", install.id),
            label: format!("本地维护 · {}", install.label),
            description: format!(
                "{}{}",
                install.root,
                match mode {
                    Some(ManagedMode::Source) => "（源码 checkout）",
                    Some(ManagedMode::Installed) => "（已安装的依赖树）",
                    None => "（未检测到 DSH）",
                }
            ),
            available: mode.is_some() && node_ok,
            reason: if mode.is_none() {
                Some("该目录里没有找到 DSH".into())
            } else {
                node_reason.clone()
            },
        });
    }

    options.push(RuntimeOption {
        id: "external".into(),
        label: "其他后端二进制".into(),
        description: match &config.extra_binary {
            Some(binary) => format!("已选择：{binary}（不需要 Node）"),
            None => "指定任意能运行 `dsh web` 的可执行文件，不需要 Node".into(),
        },
        available: config.extra_binary.is_some(),
        reason: if config.extra_binary.is_none() {
            Some("尚未选择文件".into())
        } else {
            None
        },
    });

    Ok(options)
}

#[tauri::command]
pub async fn set_extra_binary(state: Shared<'_>, path: String) -> Result<(), Failure> {
    let binary = PathBuf::from(&path);
    if !binary.is_file() {
        return Err(Failure::new(FailureCode::WorkspaceMissing, "该路径不是文件"));
    }
    // Adoption requires proof: `--version` parses and `web --help` advertises the
    // flags dshell passes. A user can easily point at an old, patched or
    // unrelated program, and it must be refused with a reason.
    let version = crate::runtime::node::run_version(&binary, &["--version"]).await;
    let help = crate::runtime::node::run_text(&binary, &["web", "--help"]).await;
    let Some(help) = help else {
        return Err(Failure::new(
            FailureCode::DshFlagUnsupported,
            "该程序不支持 `web --help`，看起来不是 DSH 后端",
        )
        .with_detail(format!("--version ⇒ {version:?}")));
    };
    let flags = crate::runtime::help::parse(&help);
    if !flags.port || !flags.no_open {
        return Err(Failure::new(
            FailureCode::DshFlagUnsupported,
            "该程序的 `web` 子命令不支持 dshell 需要的参数（--port / --no-open）",
        )
        .with_detail(help.chars().take(1200).collect::<String>()));
    }
    state
        .update_config(|config| {
            config.extra_binary = Some(path.clone());
        })
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn managed_install_add(
    state: Shared<'_>,
    root: String,
    label: Option<String>,
) -> Result<ManagedInstall, Failure> {
    let path = PathBuf::from(&root);
    if !path.is_dir() {
        return Err(Failure::new(FailureCode::WorkspaceMissing, "该路径不是目录"));
    }
    let Some(mode) = runtime::detect_managed_mode(&path) else {
        return Err(Failure::new(
            FailureCode::NpxInstallFailed,
            "该目录里没有找到 DSH",
        )
        .with_detail(
            "期望存在 node_modules/@deepseek-ai/dsh，或是一个包含 \
             pnpm-workspace.yaml 的源码 checkout。",
        ));
    };
    let install = ManagedInstall {
        id: crate::root::slug(&path.to_string_lossy()),
        label: label.unwrap_or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone())
        }),
        root: root.clone(),
        mode: match mode {
            ManagedMode::Installed => "installed".into(),
            ManagedMode::Source => "source".into(),
        },
    };
    let install_for_config = install.clone();
    state
        .update_config(|config| {
            config
                .managed_installs
                .retain(|existing| existing.root != root);
            config.managed_installs.push(install_for_config.clone());
        })
        .await?;
    Ok(install)
}

#[tauri::command]
pub async fn managed_install_remove(state: Shared<'_>, id: String) -> Result<(), Failure> {
    state
        .update_config(|config| {
            config.managed_installs.retain(|install| install.id != id);
        })
        .await?;
    Ok(())
}

// ------------------------------------------------------------------ settings

#[tauri::command]
pub async fn get_config(state: Shared<'_>) -> Result<Config, Failure> {
    Ok(state.config_snapshot().await)
}

#[tauri::command]
pub async fn set_config(state: Shared<'_>, config: Config) -> Result<Config, Failure> {
    // The one value that must never be accepted: a relative DSH_HOME. DSH
    // resolves it against the *child's* cwd, so the state root would silently
    // drift with the working directory.
    if let DshHomeSetting::Custom { path } = &config.dsh_home {
        let trimmed = path.trim();
        if !trimmed.is_empty() && !PathBuf::from(trimmed).is_absolute() {
            return Err(
                Failure::new(FailureCode::DshHomeInvalid, "DSH_HOME 必须是绝对路径")
                    .with_detail(format!("收到：{trimmed}")),
            );
        }
    }
    state
        .update_config(|current| {
            *current = config.clone();
        })
        .await
}

/// Finish the first-run wizard.
///
/// There is deliberately no "choose a working directory" here: DSH picks the
/// project folder in its own UI, so asking for it up front would be dshell
/// pretending to own a decision that is not its.
#[tauri::command]
pub async fn complete_onboarding(
    state: Shared<'_>,
    data_root: Option<String>,
    write_env: Option<bool>,
) -> Result<RootView, Failure> {
    let mut notes: Vec<String> = Vec::new();

    if let Some(path) = data_root {
        let target = PathBuf::from(&path);
        if let Err(err) = crate::env::probe_writable(&target) {
            return Err(Failure::new(
                FailureCode::DataRootUnwritable,
                format!("该位置不可写：{}", target.display()),
            )
            .with_detail(err.to_string()));
        }
        if write_env.unwrap_or(true) {
            match crate::env::set_user_var(crate::root::ENV_VAR, &path) {
                Ok(()) => notes.push(format!(
                    "已把 {}={} 写入用户环境变量（新开的终端可见；已运行的进程需重启）",
                    crate::root::ENV_VAR,
                    path
                )),
                Err(err) => notes.push(format!(
                    "写入环境变量失败（{err}）；dshell 仍会通过回落链找到数据根"
                )),
            }
        }
    }

    state
        .update_config(|config| {
            config.onboarding_done = true;
        })
        .await?;

    if !notes.is_empty() {
        let mut boot_note = state.boot_note.lock().await;
        *boot_note = Some(notes.join("\n"));
    }

    Ok(root_view(&state))
}

#[tauri::command]
pub async fn remove_env_var() -> Result<(), Failure> {
    crate::env::remove_user_var(crate::root::ENV_VAR)
        .map_err(|err| Failure::new(FailureCode::Internal, format!("移除环境变量失败：{err}")))
}

#[tauri::command]
pub async fn probe_writable_path(path: String) -> Result<bool, Failure> {
    Ok(crate::env::probe_writable(&PathBuf::from(path)).is_ok())
}

#[tauri::command]
pub async fn open_path(path: String) -> Result<(), Failure> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(Failure::new(FailureCode::Internal, "路径不存在"));
    }
    crate::shell_reveal(&target)
        .map_err(|err| Failure::new(FailureCode::Internal, format!("无法打开路径：{err}")))
}

#[tauri::command]
pub async fn pick_directory(initial: Option<String>) -> Result<Option<String>, Failure> {
    Ok(crate::dialog::pick_directory(initial.as_deref()))
}

#[tauri::command]
pub async fn pick_file(title: Option<String>) -> Result<Option<String>, Failure> {
    Ok(crate::dialog::pick_file(title.as_deref()))
}

// --------------------------------------------------------------- app events

#[tauri::command]
pub async fn notify_tabs_changed(state: Shared<'_>) -> Result<(), Failure> {
    state.manager.changed().await;
    Ok(())
}

#[tauri::command]
pub async fn set_window_title(app: AppHandle, title: String) -> Result<(), Failure> {
    if let Some(window) = app.get_window(crate::MAIN_WINDOW) {
        let _ = window.set_title(&title);
    }
    Ok(())
}

#[tauri::command]
pub async fn persist_now(state: Shared<'_>) -> Result<(), Failure> {
    state.persist_tabs().await;
    Ok(())
}
