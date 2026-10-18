//! The `invoke` surface.
//!
//! Every command that can reach a webview API (`add_child`, `navigate`,
//! `cookies_for_url`) is `async`. That is not style: on Windows those calls
//! deadlock when made from a synchronous command or an event handler (wry#583).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::config::{Config, DshHomeSetting, ManagedInstall};
use crate::error::{Action, Failure, FailureCode};
use crate::runtime::node::EnvironmentSnapshot;
use crate::runtime::{self, ManagedMode, RuntimeSpec};
use crate::state::AppState;
use crate::tabs::manager::TabManager;
use crate::tabs::state::{ConnectionKind, PageKind, TabSpec, TabView};

/// Runtime preparation progress: `{ runtimeId, stage, message }`.
///
/// Separate from `dshell://runtime-progress` because that one is keyed by tab id
/// and describes a spawn; this one describes work that happens *before* a tab
/// exists, on the new-connection page itself.
pub const PREPARE_PROGRESS_EVENT: &str = "dshell://prepare-progress";

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
    /// Where a first checkout would be created: `<data root>/runtimes`, i.e. inside
    /// `$DSHELL_HOME`; the clone itself lands in its `deepseek-harness` subdirectory.
    pub default_source_container: String,
    /// The container actually in use (the default, or the user's own directory).
    pub source_container: String,
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
        root: root_view_config(&state, &config),
        tabs,
        config,
        environment,
        boot_note,
    })
}

fn root_view_config(state: &AppState, config: &Config) -> RootView {
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
        default_source_container: crate::config::default_source_container(&state.data_root.path)
            .to_string_lossy()
            .to_string(),
        source_container: config
            .source_container_dir(&state.data_root.path)
            .to_string_lossy()
            .to_string(),
    }
}

// ------------------------------------------------------------- environment

/// Install pnpm globally with the npm dshell detected.
///
/// dshell never runs `pnpm` itself — a source checkout needs it for the user's
/// own `pnpm install` / `pnpm run build`, which is exactly the orchestration
/// this app does not do. So this is a convenience, not a dependency: the report
/// already knows npm is present, and offering the one command that fixes the
/// warning is the difference between a dead end and a fixed machine.
///
/// It returns the refreshed snapshot, because the interesting part of a
/// successful install *is* the new report — the caller needs no second round
/// trip to re-detect.
#[tauri::command]
pub async fn install_pnpm(app: AppHandle) -> Result<EnvironmentSnapshot, Failure> {
    let state = state_of(&app)?;
    if let Some(snapshot) = pnpm_ready(&state).await {
        return Ok(snapshot);
    }
    // Both dshell pages can offer the button, and two concurrent global npm
    // installs would fight over npm's own lock.
    let _guard = state.pnpm_install.lock().await;
    if let Some(snapshot) = pnpm_ready(&state).await {
        return Ok(snapshot);
    }

    let node = state.node().await;
    let plan = crate::runtime::node::plan_pnpm_install(node.as_ref(), false).await;
    let Some(npm) = plan.npm_path() else {
        let blocked = plan
            .blocked
            .clone()
            .unwrap_or_else(|| "无法自动安装 pnpm".to_string());
        return Err(Failure::new(FailureCode::NoNodeRuntime, blocked).with_detail(
            "也可以在终端里自己运行 `npm install -g pnpm`，然后点「重新检测」。",
        ));
    };

    let output = crate::runtime::node::install_pnpm(&npm).await?;
    crate::dsh::process::debug_log(
        &state.data_root.path,
        &format!(
            "npm install -g pnpm ok; output tail: {}",
            output
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(10)
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    );

    let snapshot = state.environment(true).await;
    if snapshot.pnpm.is_some() {
        return Ok(snapshot);
    }

    // npm said it worked, yet nothing runnable showed up where dshell looks.
    // Say where the shims actually went instead of leaving the user to hunt.
    let prefix = crate::runtime::node::npm_global_prefix(&npm).await;
    Err(Failure::new(
        FailureCode::PnpmInstallFailed,
        "npm 报告安装成功，但 pnpm 仍然无法运行",
    )
    .with_detail(match prefix {
        Some(prefix) => format!(
            "npm 的全局前缀是 {}。请确认该目录在这个用户的 PATH 里（新开的终端可见），\
             然后点「重新检测」。",
            prefix.display()
        ),
        None => "点「重新检测」再试一次；仍然失败时请在终端里运行 `npm install -g pnpm`。\
                 已确认 npm 可用，安装本身应该没有问题。"
            .to_string(),
    }))
}

/// The cached snapshot, when pnpm already runs and there is nothing to install.
async fn pnpm_ready(state: &AppState) -> Option<EnvironmentSnapshot> {
    let snapshot = state.environment(false).await;
    snapshot.pnpm.is_some().then_some(snapshot)
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

/// The payload `web/pages/new.ts` sends.
///
/// Every field carries an explicit camelCase rename. This is not tidiness: serde's
/// `rename_all` on an enum renames *variants*, not the fields inside struct
/// variants, so without these the page's `runtimeId` was an unknown field (silently
/// ignored) and `runtime_id` was missing — a launch request that could never
/// deserialize. `tests` below pins the wire form.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NewTabRequest {
    /// Start a backend with dshell's runtime manager.
    Launch {
        /// Optional. DSH picks the project folder itself, so this only exists to
        /// give the backend a starting directory; unset means "use the default".
        #[serde(default)]
        workspace: Option<String>,
        #[serde(rename = "runtimeId")]
        runtime_id: String,
        #[serde(rename = "dshHome", default)]
        dsh_home: Option<DshHomeSetting>,
        #[serde(rename = "killOnClose", default)]
        kill_on_close: Option<bool>,
        #[serde(default)]
        title: Option<String>,
        /// The user has seen the "an instance is already running" warning and
        /// chose to continue.
        ///
        /// Two `dsh web` processes sharing one `DSH_HOME` write the same
        /// `profiles/web` and `storages/` — the design flags that as unverified
        /// (`docs/reference/dsh-integration.md` §6.2), so dshell refuses by
        /// default and makes the user say yes rather than silently doubling down.
        #[serde(rename = "confirmExistingRunning", default)]
        confirm_existing_running: bool,
    },
    /// Connect to an instance dshell did not start.
    Attach {
        url: String,
        #[serde(default)]
        remote: bool,
        #[serde(rename = "allowPlaintext", default)]
        allow_plaintext: bool,
        /// The name the user gave this connection in the confirmation step.
        /// It becomes the tab title and the history label; `None` keeps the
        /// old host/port-derived title.
        #[serde(default)]
        label: Option<String>,
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
        NewTabRequest::Launch {
            workspace,
            runtime_id,
            dsh_home,
            kill_on_close,
            title,
            confirm_existing_running,
        } => {
            // A second local backend on the same DSH_HOME is the one thing the
            // design explicitly has not verified as safe, so it is refused with
            // a warning the user can override — not silently allowed.
            if !confirm_existing_running {
                let running = running_local(state.manager.clone()).await;
                if !running.is_empty() {
                    return Err(instance_already_running(&running));
                }
            }
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
        NewTabRequest::Attach { url, remote, allow_plaintext, label } => {
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
            // "Remote" is about the transport, not about the button the user
            // pressed: the remote step accepts a loopback URL on purpose (it is
            // the default prefill), and a loopback target needs neither the
            // plaintext confirmation nor the remote tab badge.
            let loopback = parsed.is_loopback();
            let is_remote = remote && !loopback;
            if is_remote && parsed.scheme == "http" && !allow_plaintext {
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

            // A user-chosen name is the tab title *and* the history label. It is
            // trimmed and bounded here because it crosses the UI boundary; an
            // empty name falls back to the old host/port description rather
            // than producing a nameless tab.
            let named = label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(48).collect::<String>());
            let title = named.clone().unwrap_or_else(|| {
                if is_remote {
                    format!("远程 · {}", parsed.host)
                } else {
                    format!("本地 · {}", parsed.port)
                }
            });
            let connection = if is_remote {
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
            // Remembered before the connection has finished on purpose: the
            // remember step is what makes a retry possible from the history list,
            // and a failed probe leaves exactly that situation.
            let origin = parsed.origin.clone();
            let history_label =
                named.unwrap_or_else(|| format!("{} · {}", parsed.host, parsed.port));
            state
                .update_config(|config| {
                    config.remember_connection(crate::config::RecentConnection::new(
                        &url,
                        &origin,
                        &history_label,
                        is_remote,
                    ));
                })
                .await
                .ok();
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

/// The answer to "can this exact target be connected to?" before a tab exists.
///
/// `requiresToken` is not a failure: the server at the typed IP/port answered
/// like DSH, and the confirmation dialog can ask for the launch token without
/// throwing the whole flow away. A real failure (timeout, wrong service,
/// rejected token) is returned as `Err(Failure)` so the page can show Rust's
/// own diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProbe {
    pub ok: bool,
    pub requires_token: bool,
    pub origin: String,
}

const CONNECTION_PROBE_TIMEOUT: Duration = Duration::from_millis(2500);

/// Probe an origin with or without a token before opening a tab.
///
/// With a token this runs the same exchange the webview will run: a valid token
/// must reach DSH's frontend, so a stale history entry is caught here instead of
/// producing a tab stuck on `401`. Without one it only identifies the server,
/// which is enough to know that asking for a token makes sense.
#[tauri::command]
pub async fn probe_connection(
    origin: String,
    url: Option<String>,
) -> Result<ConnectionProbe, Failure> {
    let target = crate::dsh::url::LaunchUrl::parse(&origin).ok_or_else(|| {
        Failure::new(FailureCode::AttachNotDsh, "无法解析该地址")
            .with_detail("需要 `http://host:port` 形式。")
    })?;

    let Some(url) = url.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
    else {
        if crate::dsh::probe::probe_dsh_server(&target.origin, CONNECTION_PROBE_TIMEOUT)
            .await
            .is_some()
        {
            return Ok(ConnectionProbe {
                ok: false,
                requires_token: true,
                origin: target.origin,
            });
        }
        return Err(Failure::new(
            FailureCode::RemoteUnreachable,
            format!("未检测到 DSH 服务：{}", target.origin),
        )
        .with_detail("请确认 IP 与端口正确，且服务端允许该地址访问。"));
    };

    let parsed = crate::dsh::url::LaunchUrl::parse(&url).ok_or_else(|| {
        Failure::new(FailureCode::AttachAuthRequired, "启动 URL 无法解析")
            .with_detail("请粘贴 `dsh web` 打印的完整 URL，或只粘贴 token。")
    })?;
    if !parsed.has_token() {
        return Err(Failure::new(
            FailureCode::AttachAuthRequired,
            "启动 URL 里没有 token",
        )
        .with_detail("请在运行 `dsh web` 的终端里复制整行输出，或单独粘贴 token。"));
    }

    // Identify the service before exchanging a token: a random HTTP server can
    // answer 200 with a page that is not DSH, and `verify_ready`'s
    // "frontend missing" would then blame a DSH install that does not exist.
    let identified =
        crate::dsh::probe::probe_dsh_server(&parsed.origin, CONNECTION_PROBE_TIMEOUT).await;
    if identified.is_none() {
        return Err(Failure::new(
            FailureCode::AttachNotDsh,
            format!("{} 不是可识别的 DSH 服务", parsed.origin),
        )
        .with_detail("请确认 IP、端口正确，且服务端允许该地址访问。"));
    }

    crate::dsh::probe::verify_ready(&parsed.origin, &url, CONNECTION_PROBE_TIMEOUT).await?;
    Ok(ConnectionProbe { ok: true, requires_token: false, origin: parsed.origin })
}

/// Map a runtime id from the UI onto a concrete spec.
async fn resolve_runtime(
    config: &Config,
    runtime_id: &str,
    state: &AppState,
) -> Result<RuntimeSpec, Failure> {
    let pinned = config.package_spec();
    match runtime_id {
        "npx" => Ok(RuntimeSpec::Npx { package_spec: pinned }),
        "source" => {
            let root = config.source_checkout_dir(&state.data_root.path);
            let status = runtime::source::status(&root, None).await;
            if !status.usable() {
                return Err(Failure::new(
                    FailureCode::SourceMissing,
                    "源码版还没有准备好",
                )
                .with_detail(format!(
                    "{}：{}",
                    root.display(),
                    if let Some(blocked) = &status.blocked {
                        blocked.clone()
                    } else if !status.checkout {
                        "还没有克隆仓库".to_string()
                    } else if !status.installed {
                        "还没有安装依赖".to_string()
                    } else {
                        "还没有构建前端".to_string()
                    }
                ))
                .with_action(Action::primary("prepare-source", "现在下载并构建")));
            }
            Ok(runtime::source::runtime_spec(&root))
        }
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
            id: "npx".into(),
            label: "npx · 官方发行版".into(),
            description: format!(
                "无需安装，首次运行需下载约 450 MB（{}）",
                config.package_spec()
            ),
            available: node_ok,
            reason: node_reason.clone(),
        },
        RuntimeOption {
            id: "source".into(),
            label: "源码版 · git checkout".into(),
            description: format!(
                "clone 官方仓库到 {}，用 pnpm 安装并构建；每次启动前会同步更新",
                config.source_checkout_dir(&state.data_root.path).display()
            ),
            available: node_ok,
            reason: node_reason.clone(),
        },
        RuntimeOption {
            id: "npm".into(),
            label: "本地 npm 安装".into(),
            description: format!(
                "安装到 {}，版本锁定、不随 npm 缓存清理失效",
                state.data_root.runtimes_dir().join("npm").display()
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

// -------------------------------------------------- local runtime readiness

/// A backend dshell itself is supervising right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningLocal {
    pub tab_id: String,
    pub title: String,
    pub phase: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub origin: Option<String>,
}

/// The "npx 版" card's facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpxRuntime {
    pub package_spec: String,
    /// The version this channel names, when it names one offline (`pinned`).
    pub target_version: Option<String>,
    /// Every `@deepseek-ai/dsh` npx already has on disk.
    pub cached: Vec<crate::runtime::npxcache::CachedPackage>,
    /// The cached build a launch would reuse, if any.
    pub hit: Option<String>,
    /// `ready` (reuse as-is) · `install` (nothing cached) · `update` (another
    /// version is cached) · `blocked` (nothing can run it).
    pub state: String,
    pub cache_dir: String,
    /// Newest published version; only filled when the network check ran.
    pub latest_version: Option<String>,
    pub node_version: Option<String>,
    pub blocked: Option<String>,
}

/// The "源码版" card's facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRuntime {
    /// The checkout itself: `<container>/deepseek-harness`.
    pub status: crate::runtime::source::Status,
    /// The directory the user picked, which *contains* the checkout.
    pub container: String,
    /// The fixed subdirectory name, so the page can spell out where the clone goes.
    pub checkout_name: String,
    /// `ready` · `install` (clone+install+build on start) · `blocked`.
    pub state: String,
    pub repo: String,
    pub git: Option<String>,
    pub pnpm: Option<String>,
    pub node_version: Option<String>,
    pub blocked: Option<String>,
}

/// Everything the local step of the new-connection page needs, in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalRuntimeState {
    pub npx: NpxRuntime,
    pub source: SourceRuntime,
    /// DSH servers already listening on this machine (including dshell's own).
    pub instances: Vec<crate::dsh::scan::Instance>,
    pub running_local: Vec<RunningLocal>,
}

#[tauri::command]
pub async fn local_runtime_status(app: AppHandle, deep: Option<bool>) -> Result<LocalRuntimeState, Failure> {
    let state = state_of(&app)?;
    Ok(runtime_state(&state, deep.unwrap_or(false)).await)
}

async fn runtime_state(state: &AppState, deep: bool) -> LocalRuntimeState {
    let config = state.config_snapshot().await;
    let node = state.node().await;
    let node_ok = node
        .as_ref()
        .map(|node| node.version_issue.is_none())
        .unwrap_or(false);
    let node_version = node.as_ref().map(|node| node.version.clone());
    let node_reason = node.as_ref().and_then(|node| node.version_issue.clone());

    // ------------------------------------------------------------- npx
    let package_spec = config.package_spec();
    let pinned = config.dsh_version_channel == "pinned";
    let cached = crate::runtime::npxcache::cached(&state.data_root.path);
    let mut latest_version = None;
    let mut target_version = pinned.then(|| crate::config::PINNED_DSH_VERSION.to_string());
    if deep {
        if let Some(tags) = dist_tags(state).await {
            latest_version = tags.latest.clone();
            if !pinned {
                target_version = match config.dsh_version_channel.as_str() {
                    "alpha" => tags.alpha.clone().or_else(|| tags.latest.clone()),
                    _ => tags.latest.clone(),
                };
            }
        }
    }
    let hit = target_version
        .as_deref()
        .and_then(|wanted| crate::runtime::npxcache::version_of(&cached, wanted))
        .map(|entry| entry.version.clone());
    let npx_blocked = (!node_ok).then(|| {
        node_reason
            .clone()
            .unwrap_or_else(|| "未检测到可用的 Node.js".to_string())
    });
    let npx_state = if npx_blocked.is_some() {
        "blocked"
    } else if hit.is_some() {
        "ready"
    } else if cached.is_empty() {
        "install"
    } else {
        "update"
    };
    let npx = NpxRuntime {
        package_spec,
        target_version,
        cached,
        hit,
        state: npx_state.to_string(),
        cache_dir: crate::runtime::npxcache::managed_cache(&state.data_root.path)
            .to_string_lossy()
            .to_string(),
        latest_version,
        node_version: node_version.clone(),
        blocked: npx_blocked,
    };

    // ---------------------------------------------------------- source
    let container = config.source_container_dir(&state.data_root.path);
    let root = config.source_checkout_dir(&state.data_root.path);
    let git = crate::runtime::node::find_git().await;
    let status = crate::runtime::source::status(&root, git.as_ref().map(|(path, _)| path.as_path())).await;
    let pnpm = crate::runtime::node::find_pnpm(node.as_ref())
        .await
        .map(|(_, version)| version);
    let mut source_blocked = status.blocked.clone();
    if source_blocked.is_none() && !node_ok {
        source_blocked = Some(
            node_reason
                .clone()
                .unwrap_or_else(|| "未检测到可用的 Node.js".to_string()),
        );
    }
    if source_blocked.is_none() && git.is_none() {
        source_blocked = Some("未找到 git：源码版需要它来克隆与更新仓库".to_string());
    }
    if source_blocked.is_none() && pnpm.is_none() {
        source_blocked = Some("未找到 pnpm：源码版用它安装依赖并构建".to_string());
    }
    let source_state = if source_blocked.is_some() {
        "blocked"
    } else if status.usable() {
        "ready"
    } else {
        "install"
    };
    let source = SourceRuntime {
        status,
        container: container.to_string_lossy().to_string(),
        checkout_name: crate::config::SOURCE_CHECKOUT_DIR_NAME.to_string(),
        state: source_state.to_string(),
        repo: crate::config::DSH_REPO_URL.to_string(),
        git: git.map(|(_, version)| version),
        pnpm,
        node_version,
        blocked: source_blocked,
    };

    // ------------------------------------------------------- instances
    let mut instances = crate::dsh::scan::scan(crate::dsh::scan::PROBE_TIMEOUT)
        .await
        .unwrap_or_default();
    let running_local = running_local(state.manager.clone()).await;
    for running in &running_local {
        let Some(origin) = running.origin.clone() else {
            continue;
        };
        let known = instances.iter_mut().find(|instance| instance.origin == origin);
        match known {
            Some(instance) => {
                instance.tab_id = Some(running.tab_id.clone());
                instance.title = Some(running.title.clone());
                instance.connectable = true;
                instance.notice = Some("由 dshell 启动，已在选项卡中打开".to_string());
            }
            None => instances.push(crate::dsh::scan::Instance {
                origin,
                port: 0,
                tab_id: Some(running.tab_id.clone()),
                title: Some(running.title.clone()),
                connectable: true,
                notice: Some("由 dshell 启动，已在选项卡中打开".to_string()),
            }),
        }
    }
    // A remembered connection is a usable credential for its origin; that is the
    // difference between "we found a DSH" and "you can connect with one click".
    for instance in &mut instances {
        if instance.connectable {
            continue;
        }
        if let Some(saved) = config
            .recent_connections
            .iter()
            .find(|saved| saved.id == instance.origin)
        {
            instance.connectable = true;
            instance.title = Some(saved.label.clone());
            instance.notice = Some("使用已保存的连接（含 token）".to_string());
        }
    }

    LocalRuntimeState { npx, source, instances, running_local }
}

/// `npm view @deepseek-ai/dsh dist-tags`, cached for a few minutes.
///
/// The only network call the page makes before a launch, and it is optional: a
/// pinned channel never needs it, and a failure just leaves "是否有更新" unknown
/// instead of blocking the card.
async fn dist_tags(state: &AppState) -> Option<crate::runtime::npxcache::DistTags> {
    const TTL: Duration = Duration::from_secs(600);
    {
        let cached = state.dist_tags.lock().await;
        if let Some((at, tags)) = cached.as_ref() {
            if at.elapsed() < TTL {
                return Some(tags.clone());
            }
        }
    }
    let node = state.node().await;
    let npm = crate::runtime::node::find_npm(node.as_ref())?;
    let tags = crate::runtime::npxcache::dist_tags(&npm).await?;
    {
        let mut cached = state.dist_tags.lock().await;
        *cached = Some((std::time::Instant::now(), tags.clone()));
    }
    Some(tags)
}

/// Prepare a local runtime so the launch after it is a launch and nothing else.
///
/// This is the blocking half of the flow the page runs before "打开": npx
/// downloads / upgrades the package, the source runtime clones, installs and
/// builds. It streams `dshell://prepare-progress` events because the wait can
/// last minutes and a spinner alone would be a lie about progress.
#[tauri::command]
pub async fn prepare_runtime(
    app: AppHandle,
    runtime_id: String,
    source_container: Option<String>,
) -> Result<LocalRuntimeState, Failure> {
    let state = state_of(&app)?;
    // One prepare at a time: two npx resolutions would fight over the same cache
    // directory, and two clones over the same path.
    let _guard = state.prepare.lock().await;

    let emitter = app.clone();
    let runtime_for_event = runtime_id.clone();
    let report = move |stage: &str, message: &str| {
        let _ = emitter.emit(
            PREPARE_PROGRESS_EVENT,
            serde_json::json!({
                "runtimeId": runtime_for_event,
                "stage": stage,
                "message": message,
            }),
        );
    };

    match runtime_id.as_str() {
        "npx" => {
            let config = state.config_snapshot().await;
            let node = state.node().await;
            let node = crate::runtime::require_node(node.as_ref())?;
            let npx = node.npx.clone().ok_or_else(|| {
                Failure::new(
                    FailureCode::NoNodeRuntime,
                    "找到了 Node，但同一安装目录下没有 npx",
                )
                .with_detail(format!("node: {}", node.node.display()))
            })?;
            let cache = crate::runtime::npxcache::managed_cache(&state.data_root.path);
            let cached = crate::runtime::npxcache::cached(&state.data_root.path);
            let wanted = (config.dsh_version_channel == "pinned")
                .then_some(crate::config::PINNED_DSH_VERSION);
            if let Some(wanted) = wanted {
                if crate::runtime::npxcache::version_of(&cached, wanted).is_some() {
                    report("ready", &format!("npx 已缓存 {wanted}，无需下载"));
                    return Ok(runtime_state(&state, false).await);
                }
                if !cached.is_empty() {
                    report(
                        "upgrade",
                        &format!(
                            "正在切换到 {wanted}（缓存中已有 {}）",
                            cached
                                .iter()
                                .map(|entry| entry.version.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                } else {
                    report(
                        "download",
                        &format!(
                            "首次下载 {}（约 450 MB / 26,000 个文件，可能数分钟）",
                            config.package_spec()
                        ),
                    );
                }
            } else {
                report(
                    "download",
                    &format!("正在解析并下载 {} …", config.package_spec()),
                );
            }
            // One line of npx output is probe-style evidence, not a stage of its
            // own: keep the headline on "download" unless something better was
            // reported, and let the page show the tail as a log.
            let forward = |line: String| report("output", &line);
            let version = crate::runtime::npxcache::warm(
                &npx,
                &config.package_spec(),
                &cache,
                &forward,
            )
            .await?;
            report("done", &format!("npx 准备完成（{version}）"));
        }
        "source" => {
            // The parameter is the *container* the user chose; the checkout is
            // always `<container>/deepseek-harness`, so picking `E:\projects` does
            // not take over `E:\projects`.
            let chosen = source_container
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let config = if let Some(chosen) = &chosen {
                let path = PathBuf::from(chosen);
                if !path.is_absolute() {
                    return Err(Failure::new(
                        FailureCode::SourceMissing,
                        "安装位置必须是绝对路径",
                    )
                    .with_detail(format!("收到：{chosen}")));
                }
                state
                    .update_config(|config| {
                        config.source_container = Some(chosen.clone());
                    })
                    .await?
            } else {
                state.config_snapshot().await
            };
            let container = config.source_container_dir(&state.data_root.path);
            let root = config.source_checkout_dir(&state.data_root.path);
            report(
                "start",
                &format!(
                    "将在 {} 中克隆/更新仓库（{}）",
                    container.display(),
                    crate::config::SOURCE_CHECKOUT_DIR_NAME
                ),
            );
            let outcome =
                crate::runtime::source::prepare(&root, crate::config::DSH_REPO_URL, &report).await?;
            if let Some(notice) = &outcome.notice {
                report("notice", notice);
            }
            report(
                "done",
                &format!(
                    "源码准备完成（{}）",
                    outcome
                        .status
                        .commit
                        .as_deref()
                        .map(|commit| format!("提交 {commit}"))
                        .unwrap_or_else(|| "已就绪".to_string())
                ),
            );
        }
        other => {
            return Err(Failure::new(
                FailureCode::Internal,
                format!("该运行时不需要准备：{other}"),
            ))
        }
    }

    Ok(runtime_state(&state, false).await)
}

/// The DSH servers listening on this machine, with what dshell knows about each.
#[tauri::command]
pub async fn scan_instances(app: AppHandle) -> Result<Vec<crate::dsh::scan::Instance>, Failure> {
    let state = state_of(&app)?;
    Ok(runtime_state(&state, false).await.instances)
}

/// Forget one remembered connection.
#[tauri::command]
pub async fn forget_connection(state: Shared<'_>, id: String) -> Result<(), Failure> {
    state
        .update_config(|config| config.forget_connection(&id))
        .await?;
    Ok(())
}

/// Backends dshell is currently supervising.
async fn running_local(manager: Arc<TabManager>) -> Vec<RunningLocal> {
    let (tabs, _active) = manager.snapshot().await;
    tabs.into_iter()
        .filter(|view| {
            view.page_kind.is_none()
                && view.badge == "local"
                && matches!(
                    view.phase.as_str(),
                    crate::tabs::state::phase::STARTING
                        | crate::tabs::state::phase::WAITING_FOR_READY
                        | crate::tabs::state::phase::READY
                )
        })
        .map(|view| RunningLocal {
            tab_id: view.id,
            title: view.title,
            phase: view.phase,
            pid: view.pid,
            origin: view.origin,
        })
        .collect()
}

/// The failure a second local launch gets when one is already running.
fn instance_already_running(running: &[RunningLocal]) -> Failure {
    let list = running
        .iter()
        .map(|entry| {
            format!(
                "· {}（{}）{}",
                entry.title,
                entry.phase,
                entry
                    .origin
                    .as_deref()
                    .map(|origin| format!(" {origin}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Failure::new(
        FailureCode::InstanceAlreadyRunning,
        format!("已经有 {} 个本地 DSH 后端在运行", running.len()),
    )
    .with_detail(format!(
        "{list}\n\n两个 dsh web 共用同一个 DSH_HOME 会并发读写 profiles/ 与 storages/，\
         这个组合官方没有验证过。如果确实需要第二个实例，请为它设置独立的 DSH_HOME，\
         或确认后继续。"
    ))
    .with_action(Action::primary("confirm-existing", "仍然启动"))
    .with_action(Action::secondary("activate", "切换到已有选项卡"))
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

    let config = state
        .update_config(|config| {
            config.onboarding_done = true;
        })
        .await?;

    if !notes.is_empty() {
        let mut boot_note = state.boot_note.lock().await;
        *boot_note = Some(notes.join("\n"));
    }

    Ok(root_view_config(&state, &config))
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

/// The main window's HWND, so a native dialog can own itself to it.
///
/// Without an owner a dialog is not modal to dshell and may open *behind* the
/// main window — on a maximized window that is indistinguishable from "the button
/// did nothing". The handle travels as an integer because that is all the
/// hand-declared Win32 surface needs.
fn dialog_owner(app: &AppHandle) -> Option<isize> {
    #[cfg(windows)]
    {
        let window = app.get_window(crate::MAIN_WINDOW)?;
        window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        None
    }
}

#[tauri::command]
pub async fn pick_directory(
    app: AppHandle,
    initial: Option<String>,
) -> Result<Option<String>, Failure> {
    let state = state_of(&app)?;
    let result = crate::dialog::pick_directory(initial.as_deref(), dialog_owner(&app));
    // A native dialog either works or does not, and the difference is invisible
    // from the page: log the outcome so "the button did nothing" is answerable
    // from the debug log instead of guesswork.
    crate::dsh::process::debug_log(
        &state.data_root.path,
        &format!(
            "pick_directory(initial={initial:?}) -> {}",
            match &result {
                Ok(Some(path)) => path.to_string(),
                Ok(None) => "cancelled".to_string(),
                Err(err) => format!("FAILED: {err}"),
            }
        ),
    );
    result.map_err(|err| {
        Failure::new(FailureCode::Internal, err)
            .with_detail("目录选择器用 Win32 的 GetOpenFileNameW；也可以在输入框里直接填写绝对路径。")
    })
}

#[tauri::command]
pub async fn pick_file(app: AppHandle, title: Option<String>) -> Result<Option<String>, Failure> {
    let result = crate::dialog::pick_file(title.as_deref(), dialog_owner(&app));
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        crate::dsh::process::debug_log(
            &state.data_root.path,
            &format!(
                "pick_file() -> {}",
                match &result {
                    Ok(Some(path)) => path.to_string(),
                    Ok(None) => "cancelled".to_string(),
                    Err(err) => format!("FAILED: {err}"),
                }
            ),
        );
    }
    result.map_err(|err| Failure::new(FailureCode::Internal, err))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload `web/pages/new.ts` actually sends.
    ///
    /// `NewTabRequest` is an enum with struct variants, and serde's `rename_all`
    /// applies to *variants*, not to their fields — so this test is the only thing
    /// keeping the camelCase field names on the wire and in Rust in step. A
    /// mismatch would surface as "missing field `runtime_id`" at the moment the
    /// user presses 启动, i.e. the worst possible time.
    #[test]
    fn the_page_launch_payload_parses() {
        let json = r#"{
            "kind": "launch",
            "workspace": null,
            "runtimeId": "npx",
            "dshHome": { "mode": "system" },
            "killOnClose": true,
            "confirmExistingRunning": false
        }"#;
        let parsed: NewTabRequest = serde_json::from_str(json).expect("launch payload must parse");
        match parsed {
            NewTabRequest::Launch {
                runtime_id,
                workspace,
                kill_on_close,
                confirm_existing_running,
                ..
            } => {
                assert_eq!(runtime_id, "npx");
                assert!(workspace.is_none());
                assert!(kill_on_close.unwrap_or(false));
                assert!(!confirm_existing_running);
            }
            other => panic!("expected a launch request, got {other:?}"),
        }
    }

    #[test]
    fn the_page_attach_payload_parses() {
        let json = r#"{
            "kind": "attach",
            "url": "http://127.0.0.1:3080/?token=x",
            "remote": false,
            "allowPlaintext": false,
            "label": "我的连接"
        }"#;
        let parsed: NewTabRequest = serde_json::from_str(json).expect("attach payload must parse");
        match parsed {
            NewTabRequest::Attach { url, remote, allow_plaintext, label } => {
                assert_eq!(url, "http://127.0.0.1:3080/?token=x");
                assert!(!remote);
                assert!(!allow_plaintext);
                assert_eq!(label.as_deref(), Some("我的连接"));
            }
            other => panic!("expected an attach request, got {other:?}"),
        }
    }

    /// The "another backend is already running" failure must name the tabs and
    /// offer the two ways out, because the UI renders those actions verbatim.
    #[test]
    fn the_already_running_failure_is_actionable() {
        let running = vec![RunningLocal {
            tab_id: "abc".into(),
            title: "本地".into(),
            phase: "ready".into(),
            pid: Some(4242),
            origin: Some("http://127.0.0.1:9808".into()),
        }];
        let failure = instance_already_running(&running);
        assert_eq!(failure.code, FailureCode::InstanceAlreadyRunning);
        assert!(failure.detail.as_deref().unwrap_or("").contains("本地"));
        assert!(failure.actions.iter().any(|action| action.id == "confirm-existing"));
        assert!(failure.actions.iter().any(|action| action.id == "activate"));
    }
}
