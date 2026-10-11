//! The tab manager: the **only** module allowed to touch multi-webview APIs.
//!
//! `tauri`'s `unstable` feature is what lets one window hold many top-level
//! webviews — which the design proves is the only arrangement in which a DSH
//! page can work at all (its session cookie is `SameSite=Strict` and bound to the
//! host authority, and the `/api` fence rejects `Origin: null`). Concentrating
//! every `add_child`, `navigate`, `set_bounds` and `cookies_for_url` call here
//! keeps that experimental surface in one reviewable file.
//!
//! Two Windows hazards shape the code below:
//!
//! * creating a webview from a *synchronous* command deadlocks (wry#583), so
//!   every command that can reach `add_child` is `async`;
//! * child webviews have no z-order API, so nothing may depend on stacking
//!   order — exactly one content webview is ever inside the visible area.
//!
//! Locking discipline: the tab table is a `tokio::sync::Mutex`, and no guard is
//! ever held across an `.await` that could block on something else (webview
//! creation, process termination, HTTP probes). State is read out in a short
//! scope, the async work happens, and the result is written back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{
    webview::{PageLoadEvent, WebviewBuilder},
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl, Window,
};
use tokio::sync::Mutex;

use crate::config::DshHomeSetting;
use crate::dsh::process::{self, LaunchEvent, SpawnRequest};
use crate::dsh::url::LaunchUrl;
use crate::error::{Action, Failure, FailureCode};
use crate::runtime::{self, Flags, RuntimeSpec};
use crate::tabs::layout;
use crate::tabs::state::{phase, ConnectionKind, TabRuntime, TabSpec, TabView};

pub const CHROME_LABEL: &str = "chrome";
pub const CHROME_PLACEHOLDER: &str = "shell/placeholder.html";
pub const CHROME_ERROR: &str = "shell/error.html";

pub const SHORTCUT_EVENT: &str = "dshell://shortcut";
pub const TABS_CHANGED_EVENT: &str = "dshell://tabs-changed";
pub const TAB_CHANGED_EVENT: &str = "dshell://tab-changed";
pub const RUNTIME_PROGRESS_EVENT: &str = "dshell://runtime-progress";

/// Readiness budget for a runtime that may have to be fetched first.
const COLD_START_BUDGET: Duration = Duration::from_secs(300);
/// How often log lines trigger a full state broadcast at most.
const LOG_BROADCAST_INTERVAL: Duration = Duration::from_millis(400);

static TAB_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TabEntry {
    runtime: TabRuntime,
    webview: Webview,
    /// Stable id of (workspace, DSH_HOME) — decides the webview data directory,
    /// so reopening the same instance keeps its session cookie.
    instance_key: String,
    /// Where DSH's state will actually land for this tab, resolved once at
    /// creation so the UI never has to re-derive it.
    instance_home: Option<String>,
    /// One-shot: only the *first* finished load means "ready". DSH is an SPA, so
    /// switching sessions inside it never fires this again.
    first_load_seen: bool,
}

impl TabEntry {
    fn id(&self) -> &str {
        &self.runtime.spec.id
    }
}

pub struct TabManager {
    window: Window,
    app: AppHandle,
    tabs: Mutex<HashMap<String, TabEntry>>,
    order: Mutex<Vec<String>>,
    active: Mutex<Option<String>>,
    /// Whether dshell's own panel is occupying the area below the tab strip.
    chrome_mode: Mutex<layout::ChromeMode>,
}

impl TabManager {
    pub fn new(window: Window, app: AppHandle) -> Self {
        Self {
            window,
            app,
            tabs: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            active: Mutex::new(None),
            chrome_mode: Mutex::new(layout::ChromeMode::Strip),
        }
    }

    pub fn new_tab_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = TAB_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{:x}{:x}{:x}", nanos & 0xffff_ffff, std::process::id(), counter)
    }

    pub fn webview_label(id: &str) -> String {
        format!("tab-{id}")
    }

    // ------------------------------------------------------------- geometry

    fn window_logical_size(&self) -> (f64, f64) {
        let size = self.window.inner_size().unwrap_or_default();
        let scale = self.window.scale_factor().unwrap_or(1.0);
        (size.width as f64 / scale, size.height as f64 / scale)
    }

    fn apply_bounds(&self, entries: &HashMap<String, TabEntry>, active: Option<&str>, mode: layout::ChromeMode) {
        let (width, height) = self.window_logical_size();
        let chrome = layout::chrome_bounds(width, height, mode);
        if let Some(webview) = self.app.get_webview(CHROME_LABEL) {
            let _ = webview.set_position(LogicalPosition::new(chrome.x, chrome.y));
            let _ = webview.set_size(LogicalSize::new(chrome.width, chrome.height));
        }
        for entry in entries.values() {
            // Only the active tab is ever inside the visible area, and only when
            // dshell is not showing a panel over it.
            let bounds = if active == Some(entry.id()) && mode == layout::ChromeMode::Strip {
                layout::content_bounds(width, height, chrome.height)
            } else {
                layout::parked_bounds(width)
            };
            let _ = entry
                .webview
                .set_position(LogicalPosition::new(bounds.x, bounds.y));
            let _ = entry.webview.set_size(LogicalSize::new(bounds.width, bounds.height));
        }
    }

    pub async fn set_chrome_mode(&self, mode: layout::ChromeMode) {
        {
            let mut current = self.chrome_mode.lock().await;
            *current = mode;
        }
        let entries = self.tabs.lock().await;
        let active = self.active.lock().await.clone();
        self.apply_bounds(&entries, active.as_deref(), mode);
    }

    pub async fn chrome_mode(&self) -> layout::ChromeMode {
        *self.chrome_mode.lock().await
    }

    /// Recompute every webview position. Called from the window resize handler,
    /// which runs on the main thread, so it must never block: `try_lock` is used
    /// and a missed frame is harmless because the next resize fixes it.
    pub fn relayout(&self) {
        let active = self.active.try_lock().ok().and_then(|guard| guard.clone());
        let mode = self.chrome_mode.try_lock().ok().map(|guard| *guard).unwrap_or_default();
        let Ok(entries) = self.tabs.try_lock() else { return };
        self.apply_bounds(&entries, active.as_deref(), mode);
    }

    // ---------------------------------------------------------------- views

    pub async fn views(&self) -> Vec<TabView> {
        let tabs = self.tabs.lock().await;
        let order = self.order.lock().await;
        order
            .iter()
            .filter_map(|id| tabs.get(id))
            .map(build_view)
            .collect()
    }

    /// Tabs plus which one is visible. The frontend treats this as the whole
    /// truth — it never infers activation locally.
    pub async fn snapshot(&self) -> (Vec<TabView>, Option<String>) {
        let views = self.views().await;
        let active = self.active_id().await;
        (views, active)
    }

    pub async fn changed(&self) {
        let (views, active) = self.snapshot().await;
        let _ = self.app.emit(
            TABS_CHANGED_EVENT,
            serde_json::json!({ "tabs": views, "activeId": active }),
        );
    }

    pub async fn changed_one(&self, id: &str, reset: bool) {
        let payload = {
            let tabs = self.tabs.lock().await;
            tabs.get(id).map(|entry| {
                serde_json::json!({ "tab": build_view(entry), "reset": reset })
            })
        };
        if let Some(payload) = payload {
            let _ = self.app.emit(TAB_CHANGED_EVENT, payload);
        }
    }

    // ---------------------------------------------------------- tab lifecycle

    /// Insert a tab and its webview, showing dshell's own loading page first so
    /// the tab is visible immediately even when the backend takes minutes.
    ///
    /// `instance_home` is the resolved DSH_HOME for this tab (or `None` when the
    /// user kept the system default) — `AppState` computes it from the data root.
    pub async fn open_tab(
        &self,
        spec: TabSpec,
        instance_home: Option<String>,
    ) -> Result<String, Failure> {
        let id = spec.id.clone();
        let label = Self::webview_label(&id);
        let (instance_key, data_dir) = instance_identity(&spec, &self.app);
        std::fs::create_dir_all(&data_dir).ok();

        let app_for_title = self.app.clone();
        let app_for_load = self.app.clone();

        // Webview creation hops to the event loop and blocks until it lands;
        // running it from the async runtime (never the main thread) is what keeps
        // this from deadlocking.
        let webview = self
            .window
            .add_child(
                WebviewBuilder::new(label.clone(), WebviewUrl::App(CHROME_PLACEHOLDER.into()))
                    .data_directory(data_dir)
                    .disable_drag_drop_handler()
                    .focused(false)
                    .on_document_title_changed(move |webview, title| {
                        // DSH is an SPA: this callback is the only reliable way
                        // to learn the current session title. The page URL is
                        // passed along so dshell's own shell pages cannot rename
                        // a tab to "正在启动 DSH…".
                        let app = app_for_title.clone();
                        let tab_id = webview.label().trim_start_matches("tab-").to_string();
                        let page_url = webview
                            .url()
                            .map(|url| url.to_string())
                            .unwrap_or_default();
                        tauri::async_runtime::spawn(async move {
                            if let Some(state) = app.try_state::<Arc<crate::AppState>>() {
                                state
                                    .manager
                                    .set_title_from_page(&tab_id, &page_url, title)
                                    .await;
                            }
                        });
                    })
                    .on_page_load(move |webview, payload| {
                        if payload.event() != PageLoadEvent::Finished {
                            return;
                        }
                        let app = app_for_load.clone();
                        let tab_id = webview.label().trim_start_matches("tab-").to_string();
                        let url = payload.url().to_string();
                        tauri::async_runtime::spawn(async move {
                            if let Some(state) = app.try_state::<Arc<crate::AppState>>() {
                                state.manager.mark_loaded(&tab_id, &url).await;
                            }
                        });
                    }),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(1.0, 1.0),
            )
            .map_err(|err| {
                Failure::new(FailureCode::Internal, format!("创建 webview 失败：{err}")).with_detail(
                    "这通常意味着 WebView2 运行时不可用或已损坏。\
                     可在 https://developer.microsoft.com/microsoft-edge/webview2/ 安装。",
                )
            })?;

        let entry = TabEntry {
            runtime: TabRuntime::new(spec),
            webview,
            instance_key: instance_key.clone(),
            instance_home,
            first_load_seen: false,
        };
        {
            let mut tabs = self.tabs.lock().await;
            tabs.insert(id.clone(), entry);
            let mut order = self.order.lock().await;
            order.push(id.clone());
            let mut active = self.active.lock().await;
            *active = Some(id.clone());
        }
        {
            let mode = self.chrome_mode().await;
            let entries = self.tabs.lock().await;
            let active = self.active.lock().await.clone();
            self.apply_bounds(&entries, active.as_deref(), mode);
        }
        self.changed().await;
        Ok(instance_key)
    }

    pub async fn close_tab(&self, id: &str) -> Result<(), Failure> {
        let entry = {
            let mut tabs = self.tabs.lock().await;
            tabs.remove(id)
        };
        {
            let mut order = self.order.lock().await;
            order.retain(|existing| existing != id);
        }

        let next_active = {
            let mut active = self.active.lock().await;
            if active.as_deref() == Some(id) {
                let order = self.order.lock().await.clone();
                *active = order.last().cloned();
            }
            active.clone()
        };

        let Some(entry) = entry else {
            self.activate(next_active.as_deref()).await;
            return Ok(());
        };

        let kill = match &entry.runtime.spec.connection {
            ConnectionKind::LocalSpawn { kill_on_close, .. } => *kill_on_close,
            _ => false,
        };
        if let Some(handle) = &entry.runtime.handle {
            if kill {
                handle.terminate().await;
            } else {
                // Keep the backend alive (the user may want to attach to it
                // later) but stop delivering events for a tab that is gone.
                handle.cancel_supervision();
            }
        }
        let _ = entry.webview.close();

        self.activate(next_active.as_deref()).await;
        self.persist().await;
        Ok(())
    }

    pub async fn activate(&self, id: Option<&str>) {
        {
            let mut active = self.active.lock().await;
            *active = id.map(str::to_string);
        }
        {
            let mode = self.chrome_mode().await;
            let entries = self.tabs.lock().await;
            self.apply_bounds(&entries, id, mode);
        }
        if let Some(id) = id {
            let webview = {
                let entries = self.tabs.lock().await;
                entries.get(id).map(|entry| entry.webview.clone())
            };
            if let Some(webview) = webview {
                let _ = webview.set_focus();
            }
        }
        self.changed().await;
    }

    pub async fn rename(&self, id: &str, title: &str) -> Result<(), Failure> {
        {
            let mut tabs = self.tabs.lock().await;
            let entry = tabs
                .get_mut(id)
                .ok_or_else(|| Failure::new(FailureCode::Internal, "选项卡不存在"))?;
            entry.runtime.spec.title = title.to_string();
            // Mark it as user-chosen so the page title stops winning.
            entry.runtime.last_title = Some(title.to_string());
        }
        self.changed_one(id, false).await;
        self.persist().await;
        Ok(())
    }

    pub async fn reorder(&self, ids: Vec<String>) -> Result<(), Failure> {
        {
            let tabs = self.tabs.lock().await;
            let mut order = self.order.lock().await;
            let mut next: Vec<String> = ids.into_iter().filter(|id| tabs.contains_key(id)).collect();
            for id in order.iter() {
                if !next.contains(id) {
                    next.push(id.clone());
                }
            }
            *order = next;
        }
        self.persist().await;
        self.changed().await;
        Ok(())
    }

    pub async fn tab_ids(&self) -> Vec<String> {
        self.order.lock().await.clone()
    }

    pub async fn active_id(&self) -> Option<String> {
        self.active.lock().await.clone()
    }

    pub async fn has_tab(&self, id: &str) -> bool {
        self.tabs.lock().await.contains_key(id)
    }

    pub async fn specs(&self) -> (Vec<TabSpec>, Vec<String>) {
        let tabs = self.tabs.lock().await;
        let order = self.order.lock().await;
        let specs = order
            .iter()
            .filter_map(|id| tabs.get(id))
            .map(|entry| entry.runtime.spec.clone())
            .collect();
        (specs, order.clone())
    }

    pub async fn log_tail(&self, id: &str) -> Vec<crate::dsh::process::LogEntry> {
        self.tabs
            .lock()
            .await
            .get(id)
            .map(|entry| entry.runtime.log_tail.clone())
            .unwrap_or_default()
    }

    pub async fn runtime_value(&self, id: &str) -> Option<RuntimeSpec> {
        self.tabs.lock().await.get(id).and_then(|entry| match &entry.runtime.spec.connection {
            ConnectionKind::LocalSpawn { runtime, .. } => Some(runtime.clone()),
            _ => None,
        })
    }

    pub async fn command_line(&self, id: &str) -> Option<String> {
        self.tabs
            .lock()
            .await
            .get(id)
            .and_then(|entry| entry.runtime.command_line.clone())
    }

    // ---------------------------------------------------------------- restart

    /// Kill the backend (if any) and start a fresh one, then re-navigate.
    pub async fn restart(&self, id: &str) -> Result<(), Failure> {
        let (spec, old_handle) = {
            let mut tabs = self.tabs.lock().await;
            let entry = tabs
                .get_mut(id)
                .ok_or_else(|| Failure::new(FailureCode::Internal, "选项卡不存在"))?;
            entry.runtime.generation += 1;
            entry.runtime.launch_url = None;
            entry.runtime.failure = None;
            entry.runtime.exit_code = None;
            entry.runtime.has_session_cookie = false;
            entry.runtime.phase = phase::PREPARING.to_string();
            entry.first_load_seen = false;
            (entry.runtime.spec.clone(), entry.runtime.handle.take())
        };
        if let Some(handle) = old_handle {
            handle.terminate().await;
        }
        self.show_placeholder(id).await;
        self.changed_one(id, true).await;

        match spec.connection.clone() {
            ConnectionKind::LocalSpawn { .. } => self.spawn_connection(spec).await,
            ConnectionKind::LocalAttach { url } | ConnectionKind::Remote { url, .. } => {
                let plaintext = spec.connection.is_plaintext_remote();
                let has_session = self.probe_session(id).await;
                self.connect_attach(spec, url, plaintext, has_session).await
            }
        }
    }

    // -------------------------------------------------------------- connecting

    /// Start a local backend, supervise it, and drive the tab's phase machine.
    pub async fn spawn_connection(&self, spec: TabSpec) -> Result<(), Failure> {
        let id = spec.id.clone();
        let ConnectionKind::LocalSpawn {
            workspace,
            dsh_home,
            dsh_agents_home,
            runtime: runtime_spec,
            ..
        } = spec.connection.clone()
        else {
            return Err(Failure::new(FailureCode::Internal, "该选项卡不是本地启动类型"));
        };

        let state = match self.app.try_state::<Arc<crate::AppState>>() {
            Some(state) => state.inner().clone(),
            None => return Err(Failure::new(FailureCode::Internal, "应用状态不可用")),
        };

        let workspace_path = PathBuf::from(&workspace);
        if !workspace_path.is_dir() {
            return self
                .fail(
                    &id,
                    Failure::new(
                        FailureCode::WorkspaceMissing,
                        format!("工作目录不存在：{workspace}"),
                    )
                    .with_action(Action::primary("new-tab", "重新选择")),
                )
                .await;
        }

        // DSH_HOME is injected only when the user asked for it. The default is to
        // leave the variable unset so DSH keeps using the user's existing
        // sessions, settings and API keys.
        let dsh_home_value = match dsh_home.resolve(
            &state.data_root.path,
            &state.data_root.dsh_home_dir(),
            &workspace_path,
        ) {
            Ok(value) => value,
            Err(failure) => return self.fail(&id, failure).await,
        };
        let agents_value = match dsh_agents_home.as_ref() {
            Some(setting) => setting
                .resolve(
                    &state.data_root.path,
                    &state.data_root.dsh_home_dir(),
                    &workspace_path,
                )
                .unwrap_or(None),
            None => None,
        };

        // Pre-flight writability: DSH's failure mode for an unwritable home is a
        // raw V8 crash with no structured signal and no log file.
        if let Some(home) = &dsh_home_value {
            let path = PathBuf::from(home);
            let target = if path.exists() && !path.is_dir() {
                path.parent().map(PathBuf::from).unwrap_or_else(|| path.clone())
            } else {
                path.clone()
            };
            if let Err(err) = crate::env::probe_writable(&target) {
                return self
                    .fail(
                        &id,
                        Failure::new(
                            FailureCode::DshHomeUnwritable,
                            format!("DSH_HOME 不可写：{}", path.display()),
                        )
                        .with_detail(err.to_string())
                        .with_action(Action::primary("new-tab", "改用其他路径")),
                    )
                    .await;
            }
        }

        let node = state.node().await;
        let cold = matches!(runtime_spec, RuntimeSpec::Npx { .. });
        let flags = Flags { no_open: true, port: None, host: None, trusted_hosts: Vec::new() };

        let resolved = runtime::resolve(
            &runtime_spec,
            node.as_ref(),
            &state.data_root.path,
            &workspace_path,
            &flags,
        )
        .await;
        let plan = match resolved {
            Ok(plan) => plan,
            Err(failure) => {
                process::debug_log(
                    &state.data_root.path,
                    &format!("resolve failed for {}: {failure}", runtime_spec.short_label()),
                );
                return self.fail(&id, failure.with_defaults()).await;
            }
        };

        // Environment: forward only what the design allows. `DSHELL_HOME` is
        // dshell's own root and is deliberately never forwarded.
        let mut env = plan.env.clone();
        if let Some(home) = &dsh_home_value {
            env.push(("DSH_HOME".into(), home.clone()));
        }
        if let Some(home) = &agents_value {
            env.push(("DSH_AGENTS_HOME".into(), home.clone()));
        }
        let notice = plan.notice.clone();
        let command_line = plan.command_line();
        let dsh_home_for_logs = dsh_home_value.clone();
        let plan = runtime::LaunchPlan { env, ..plan };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
        let request = SpawnRequest {
            plan,
            tab_id: id.clone(),
            log_dir: Some(
                dsh_home_for_logs
                    .as_ref()
                    .map(PathBuf::from)
                    .or_else(crate::root::preview_default_dsh_home)
                    .unwrap_or_else(|| state.data_root.path.clone())
                    .join("logs"),
            ),
            debug_root: state.data_root.path.clone(),
            ready_timeout: if cold { COLD_START_BUDGET } else { runtime::DEFAULT_PORT_TIMEOUT },
        };

        let spawned = match process::spawn_and_supervise(request, tx) {
            Ok(spawned) => spawned,
            Err(err) => {
                return self
                    .fail(
                        &id,
                        Failure::new(FailureCode::NpxInstallFailed, format!("无法启动子进程：{err}"))
                            .with_detail(command_line),
                    )
                    .await
            }
        };

        let generation = {
            let mut tabs = self.tabs.lock().await;
            let Some(entry) = tabs.get_mut(&id) else {
                spawned.handle.terminate().await;
                return Ok(());
            };
            entry.runtime.handle = Some(spawned.handle);
            entry.runtime.started_at = Some(Instant::now());
            entry.runtime.phase = phase::STARTING.to_string();
            entry.runtime.notice = notice;
            entry.runtime.command_line = Some(command_line.clone());
            entry.runtime.generation
        };

        let _ = self.app.emit(
            RUNTIME_PROGRESS_EVENT,
            serde_json::json!({
                "tabId": id,
                "stage": if cold { "resolving" } else { "starting" },
                "message": command_line,
            }),
        );
        self.changed_one(&id, true).await;

        let supervisor = spawned.supervisor;
        tokio::spawn(async move {
            run_event_loop(id, generation, rx, supervisor).await
        });

        Ok(())
    }

    /// Connect to an instance dshell did not start (`LocalAttach` / `Remote`).
    pub async fn connect_attach(
        &self,
        spec: TabSpec,
        url: String,
        plaintext: bool,
        has_session: bool,
    ) -> Result<(), Failure> {
        let id = spec.id.clone();
        let parsed = LaunchUrl::parse(&url).ok_or_else(|| {
            Failure::new(FailureCode::AttachNotDsh, "启动 URL 无法解析")
                .with_detail(url.clone())
                .with_action(Action::primary("new-tab", "重新粘贴"))
        })?;

        {
            let mut tabs = self.tabs.lock().await;
            if let Some(entry) = tabs.get_mut(&id) {
                entry.runtime.spec.display_origin = Some(parsed.origin.clone());
                entry.runtime.launch_url = Some(parsed.clone());
                entry.runtime.has_session_cookie = has_session;
                entry.runtime.phase = phase::WAITING_FOR_READY.to_string();
            }
        }
        let _ = plaintext;
        self.changed_one(&id, true).await;

        // Probe before navigating so a bad address fails here rather than as a
        // blank page. A 401 is the positive signal: it is DSH and it wants a
        // token.
        if let Err(failure) = crate::dsh::probe::probe_remote(&url, Duration::from_secs(8)).await {
            // An existing session cookie from an earlier run makes a token-less
            // URL usable, so only a failure *without* a session is fatal.
            if !has_session {
                return self.fail(&id, failure.with_defaults()).await;
            }
        }

        if let Err(failure) = self.navigate_to_dsh(&id, &url).await {
            return self.fail(&id, failure).await;
        }
        Ok(())
    }

    /// Record a failure and show the dshell error page inside the tab.
    pub async fn fail(&self, id: &str, failure: Failure) -> Result<(), Failure> {
        {
            let mut tabs = self.tabs.lock().await;
            if let Some(entry) = tabs.get_mut(id) {
                entry.runtime.failure = Some(failure.clone());
                entry.runtime.phase = phase::FAILED.to_string();
            }
        }
        self.show_error_page(id, &failure).await;
        self.changed_one(id, true).await;
        Err(failure)
    }

    // -------------------------------------------------------------- navigation

    async fn webview_of(&self, id: &str) -> Option<Webview> {
        self.tabs.lock().await.get(id).map(|entry| entry.webview.clone())
    }

    pub async fn show_placeholder(&self, id: &str) {
        let Some(webview) = self.webview_of(id).await else { return };
        let url = asset_url(&self.app, CHROME_PLACEHOLDER, &[("tab", id.to_string())]);
        let _ = webview.navigate(url);
    }

    async fn show_error_page(&self, id: &str, failure: &Failure) {
        let Some(webview) = self.webview_of(id).await else { return };
        let url = asset_url(
            &self.app,
            CHROME_ERROR,
            &[
                ("code", format!("{:?}", failure.code)),
                ("tab", id.to_string()),
                ("stage", failure.stage.clone().unwrap_or_else(|| "connect".into())),
            ],
        );
        let _ = webview.navigate(url);
    }

    /// Navigate a content webview to a DSH launch URL (token included).
    pub async fn navigate_to_dsh(&self, id: &str, url: &str) -> Result<(), Failure> {
        let parsed = LaunchUrl::parse(url).ok_or_else(|| {
            Failure::new(FailureCode::TokenUrlUnparsed, "启动 URL 无法解析")
                .with_detail(crate::dsh::url::redact_token(url))
        })?;
        let target = tauri::Url::parse(&parsed.url)
            .map_err(|err| Failure::new(FailureCode::Internal, format!("URL 解析失败：{err}")))?;
        let Some(webview) = self.webview_of(id).await else {
            return Err(Failure::new(FailureCode::Internal, "选项卡已关闭"));
        };
        webview
            .navigate(target)
            .map_err(|err| Failure::new(FailureCode::Internal, format!("导航失败：{err}")))
    }

    /// Called by `on_page_load`; only the first finished load of a real DSH page
    /// counts. The initial load is dshell's own placeholder, and DSH is an SPA
    /// whose internal navigation never fires this again.
    pub async fn mark_loaded(&self, id: &str, url: &str) {
        let parsed = LaunchUrl::parse(url);
        let is_dsh_page = parsed
            .as_ref()
            .map(|parsed| parsed.is_web_origin() && !parsed.is_dshell_page())
            .unwrap_or(false);
        if !is_dsh_page {
            return;
        }

        let mut flip = false;
        let mut record_origin = false;
        {
            let mut tabs = self.tabs.lock().await;
            if let Some(entry) = tabs.get_mut(id) {
                if !entry.first_load_seen {
                    entry.first_load_seen = true;
                    entry.runtime.phase = phase::READY.to_string();
                    entry.runtime.failure = None;
                    record_origin = entry.runtime.spec.display_origin.is_none();
                    flip = true;
                }
            }
        }
        if record_origin {
            let origin = parsed.map(|parsed| parsed.origin);
            let mut tabs = self.tabs.lock().await;
            if let Some(entry) = tabs.get_mut(id) {
                entry.runtime.spec.display_origin = origin;
            }
        }
        if flip {
            self.changed_one(id, true).await;
            self.persist().await;
        }
    }

    /// DSH mirrors its active session into `document.title`, which is exactly
    /// what a tab should show. dshell's own loading/error pages report their own
    /// titles, and those must not become tab titles.
    pub async fn set_title_from_page(&self, id: &str, page_url: &str, title: String) {
        let from_dsh_page = LaunchUrl::parse(page_url)
            .map(|parsed| parsed.is_web_origin() && !parsed.is_dshell_page())
            .unwrap_or(false);
        if !from_dsh_page || title.trim().is_empty() {
            return;
        }

        let mut changed = false;
        {
            let mut tabs = self.tabs.lock().await;
            if let Some(entry) = tabs.get_mut(id) {
                if entry.runtime.last_title.as_deref() != Some(title.as_str()) {
                    entry.runtime.last_title = Some(title.clone());
                    entry.runtime.spec.title = title;
                    changed = true;
                }
            }
        }
        if changed {
            self.changed_one(id, false).await;
        }
    }

    // ---------------------------------------------------------- session probe

    /// Does this tab still hold a DSH session cookie?
    ///
    /// Existence only — the value is never read, and DSH's on-disk secret is
    /// never touched. Runs off the event loop thread because `cookies_for_url`
    /// deadlocks on Windows in a synchronous context (the same wry#583 hazard).
    pub async fn probe_session(&self, id: &str) -> bool {
        let (webview, origin) = {
            let tabs = self.tabs.lock().await;
            let Some(entry) = tabs.get(id) else { return false };
            let origin = entry
                .runtime
                .spec
                .display_origin
                .clone()
                .or_else(|| entry.runtime.launch_url.as_ref().map(|u| u.origin.clone()));
            (entry.webview.clone(), origin)
        };
        let Some(origin) = origin else { return false };
        let Ok(url) = tauri::Url::parse(&format!("{origin}/")) else { return false };
        let Ok(cookies) = webview.cookies_for_url(url) else { return false };
        let has = cookies.iter().any(|cookie| cookie.name().starts_with("dsh-auth-"));
        let mut tabs = self.tabs.lock().await;
        if let Some(entry) = tabs.get_mut(id) {
            entry.runtime.has_session_cookie = has;
        }
        has
    }

    pub async fn clear_instance_data(&self, id: &str) -> Result<(), Failure> {
        let Some(webview) = self.webview_of(id).await else {
            return Err(Failure::new(FailureCode::Internal, "选项卡不存在"));
        };
        webview
            .clear_all_browsing_data()
            .map_err(|err| Failure::new(FailureCode::Internal, format!("清空数据失败：{err}")))?;
        let mut tabs = self.tabs.lock().await;
        if let Some(entry) = tabs.get_mut(id) {
            entry.runtime.has_session_cookie = false;
        }
        Ok(())
    }

    pub async fn open_in_system_browser(&self, id: &str) -> Result<(), Failure> {
        let url = {
            let tabs = self.tabs.lock().await;
            let entry = tabs
                .get(id)
                .ok_or_else(|| Failure::new(FailureCode::Internal, "选项卡不存在"))?;
            entry
                .runtime
                .launch_url
                .as_ref()
                .map(|u| u.url.clone())
                .or_else(|| entry.runtime.spec.display_origin.as_ref().map(|o| format!("{o}/")))
        };
        let Some(url) = url else {
            return Err(Failure::new(FailureCode::Internal, "该选项卡还没有可用的地址"));
        };
        crate::shell_open(&url)
            .map_err(|err| Failure::new(FailureCode::Internal, format!("无法打开系统浏览器：{err}")))
    }

    pub async fn reload(&self, id: &str) -> Result<(), Failure> {
        let Some(webview) = self.webview_of(id).await else {
            return Err(Failure::new(FailureCode::Internal, "选项卡不存在"));
        };
        webview
            .reload()
            .map_err(|err| Failure::new(FailureCode::Internal, format!("重新加载失败：{err}")))
    }

    /// Hand the active tab's webview focus back to the tab strip so keyboard
    /// shortcuts reach dshell again.
    pub async fn focus_chrome(&self) {
        if let Some(chrome) = self.app.get_webview(CHROME_LABEL) {
            let _ = chrome.set_focus();
        }
    }

    pub async fn persist(&self) {
        if let Some(state) = self.app.try_state::<Arc<crate::AppState>>() {
            state.persist_tabs().await;
        }
    }

    /// Record the launch URL (memory only) for a tab, if it is still the one we
    /// asked for.
    pub async fn set_launch_url(&self, id: &str, generation: u64, url: &LaunchUrl) -> bool {
        let mut tabs = self.tabs.lock().await;
        match tabs.get_mut(id) {
            Some(entry) if entry.runtime.generation == generation => {
                entry.runtime.launch_url = Some(url.clone());
                entry.runtime.spec.display_origin = Some(url.origin.clone());
                true
            }
            _ => false,
        }
    }

    /// Record that a tab's content webview was navigated to its backend, and
    /// where — the diagnostic trail for "the tab never showed DSH".
    pub async fn record_navigation(&self, id: &str, origin: &str) {
        let root = self
            .app
            .try_state::<Arc<crate::AppState>>()
            .map(|state| state.data_root.path.clone());
        if let Some(root) = root {
            process::debug_log(&root, &format!("navigating tab {id} -> {origin}"));
        }
    }

    pub async fn emit_shortcut(&self, action: &str) {
        let _ = self.app.emit(SHORTCUT_EVENT, serde_json::json!({ "action": action }));
    }

    /// The webview data directory for a tab, so the UI can show and open it.
    pub async fn data_dir(&self, id: &str) -> Option<String> {
        let tabs = self.tabs.lock().await;
        tabs.get(id).map(|entry| {
            self.app
                .try_state::<Arc<crate::AppState>>()
                .map(|state| {
                    state
                        .data_root
                        .webview_data_dir()
                        .join(format!("inst-{}", entry.instance_key))
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_default()
        })
    }

    // ------------------------------------------------------ event-loop helpers

    pub(crate) async fn record_log(
        &self,
        id: &str,
        generation: u64,
        entry: crate::dsh::process::LogEntry,
    ) -> bool {
        let mut tabs = self.tabs.lock().await;
        match tabs.get_mut(id) {
            Some(tab) if tab.runtime.generation == generation => {
                tab.runtime.push_log(entry);
                true
            }
            _ => false,
        }
    }

    pub(crate) async fn record_diagnostics(&self, id: &str, path: PathBuf) {
        let mut tabs = self.tabs.lock().await;
        if let Some(tab) = tabs.get_mut(id) {
            tab.runtime.log_path = Some(path);
        }
    }

    pub(crate) async fn set_phase(&self, id: &str, generation: u64, phase: &str) {
        let mut tabs = self.tabs.lock().await;
        if let Some(tab) = tabs.get_mut(id) {
            if tab.runtime.generation == generation {
                tab.runtime.phase = phase.to_string();
            }
        }
    }

    pub(crate) async fn handle_exit(&self, id: &str, generation: u64, report: process::ExitReport) {
        {
            let mut tabs = self.tabs.lock().await;
            if let Some(tab) = tabs.get_mut(id) {
                if tab.runtime.generation == generation {
                    tab.runtime.exit_code = report.code;
                    tab.runtime.handle = None;
                    tab.runtime.phase = if report.expected {
                        phase::IDLE.to_string()
                    } else {
                        phase::EXITED.to_string()
                    };
                    tab.runtime.failure = if report.expected {
                        None
                    } else {
                        Some(
                            Failure::new(
                                FailureCode::ChildExited,
                                format!(
                                    "DSH 进程已退出（退出码 {}）",
                                    report
                                        .code
                                        .map(|code| code.to_string())
                                        .unwrap_or_else(|| "无".into())
                                ),
                            )
                            .with_stage("runtime")
                            .with_defaults(),
                        )
                    };
                }
            }
        }
        self.changed_one(id, true).await;
    }

    pub(crate) async fn handle_failure(&self, id: &str, generation: u64, failure: Failure) {
        let current = {
            let tabs = self.tabs.lock().await;
            tabs.get(id)
                .map(|tab| tab.runtime.generation == generation)
                .unwrap_or(false)
        };
        if !current {
            return;
        }
        let _ = self.fail(id, failure).await;
    }
}

// ---------------------------------------------------------------- event loop

/// Consume a supervisor's events and drive the tab's phase machine.
async fn run_event_loop(
    tab_id: String,
    generation: u64,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<LaunchEvent>,
    supervisor: tokio::task::JoinHandle<()>,
) {
    let mut last_broadcast = Instant::now();
    let mut pending_broadcast = false;
    // Set when the readiness line arrived; the verification probe then runs on
    // the next idle tick so log flow is not blocked by an HTTP round trip.
    let mut pending_verification: Option<LaunchUrl> = None;

    loop {
        if pending_verification.is_none() && pending_broadcast
            && last_broadcast.elapsed() >= LOG_BROADCAST_INTERVAL {
                pending_broadcast = false;
                last_broadcast = Instant::now();
                broadcast_one(&tab_id, false).await;
            }

        let idle = Duration::from_millis(if pending_verification.is_some() { 10 } else { 400 });
        let event = tokio::select! {
            event = rx.recv() => event,
            _ = tokio::time::sleep(idle) => {
                if let Some(url) = pending_verification.take() {
                    verify_and_navigate(tab_id.clone(), generation, url).await;
                }
                continue;
            }
        };

        let Some(event) = event else { break };
        match event {
            LaunchEvent::Log(entry) => {
                if record_log(&tab_id, generation, entry).await {
                    pending_broadcast = true;
                }
            }
            LaunchEvent::Diagnostics(path) => {
                record_diagnostics(&tab_id, path).await;
            }
            LaunchEvent::Ready(url) => {
                set_phase(&tab_id, generation, phase::WAITING_FOR_READY).await;
                broadcast_one(&tab_id, true).await;
                pending_verification = Some(url);
            }
            LaunchEvent::Exited(report) => {
                handle_exit(&tab_id, generation, report).await;
                break;
            }
            LaunchEvent::Failed(failure) => {
                handle_failure(&tab_id, generation, failure).await;
                break;
            }
        }
    }
    let _ = supervisor.await;
}

async fn record_log(
    tab_id: &str,
    generation: u64,
    entry: crate::dsh::process::LogEntry,
) -> bool {
    match current_manager() {
        Some(manager) => manager.record_log(tab_id, generation, entry).await,
        None => false,
    }
}

async fn record_diagnostics(tab_id: &str, path: PathBuf) {
    if let Some(manager) = current_manager() {
        manager.record_diagnostics(tab_id, path).await;
    }
}

async fn set_phase(tab_id: &str, generation: u64, phase: &str) {
    if let Some(manager) = current_manager() {
        manager.set_phase(tab_id, generation, phase).await;
    }
}

async fn handle_exit(tab_id: &str, generation: u64, report: process::ExitReport) {
    if let Some(manager) = current_manager() {
        manager.handle_exit(tab_id, generation, report).await;
    }
}

async fn handle_failure(tab_id: &str, generation: u64, failure: Failure) {
    if let Some(manager) = current_manager() {
        manager.handle_failure(tab_id, generation, failure).await;
    }
}

async fn broadcast_one(tab_id: &str, reset: bool) {
    if let Some(manager) = current_manager() {
        manager.changed_one(tab_id, reset).await;
    }
}

/// Step 2 of the readiness chain.
///
/// The readiness line only proves the server is bound. A DSH install whose
/// frontend bundle is missing prints the same line, keeps its stderr empty, stays
/// alive — and serves 404. So confirm the UI is actually served before calling
/// the tab ready.
async fn verify_and_navigate(tab_id: String, generation: u64, url: LaunchUrl) {
    let outcome = crate::dsh::probe::verify_ready(&url.origin, &url.url, Duration::from_secs(15)).await;
    let Some(manager) = current_manager() else { return };
    if let Some(state) = current_state() {
        process::debug_log(
            &state.data_root.path,
            &format!(
                "readiness verification for tab {tab_id} at {}: {}",
                url.origin,
                match &outcome {
                    Ok(_) => "serving the DSH frontend".to_string(),
                    Err(failure) => format!("FAILED ({failure})"),
                }
            ),
        );
    }
    match outcome {
        Ok(_) => {
            if !manager.set_launch_url(&tab_id, generation, &url).await {
                return;
            }
            manager.record_navigation(&tab_id, &url.origin).await;
            if let Err(failure) = manager.navigate_to_dsh(&tab_id, &url.url).await {
                let _ = manager.fail(&tab_id, failure).await;
            } else {
                manager.changed_one(&tab_id, true).await;
            }
        }
        Err(failure) => {
            let _ = manager.fail(&tab_id, failure.with_defaults()).await;
        }
    }
}

// ------------------------------------------------------------------ helpers

/// The running app's handle. `AppState` is a process-lifetime singleton, so
/// background tasks resolve it lazily through this process-wide handle.
pub fn install_global(app: AppHandle) {
    let _ = GLOBAL_APP.set(app);
}

static GLOBAL_APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

/// The shared tab manager, or `None` while the app is still starting up.
pub fn current_manager() -> Option<Arc<TabManager>> {
    GLOBAL_APP
        .get()
        .and_then(|app| app.try_state::<Arc<crate::AppState>>())
        .map(|state| state.manager.clone())
}

/// The whole app state, or `None` while the app is still starting up.
pub fn current_state() -> Option<Arc<crate::AppState>> {
    GLOBAL_APP
        .get()
        .and_then(|app| app.try_state::<Arc<crate::AppState>>())
        .map(|state| state.inner().clone())
}

fn build_view(entry: &TabEntry) -> TabView {
    let runtime = &entry.runtime;
    let spec = &runtime.spec;
    let workspace = match &spec.connection {
        ConnectionKind::LocalSpawn { workspace, .. } => Some(workspace.clone()),
        _ => None,
    };

    TabView {
        id: spec.id.clone(),
        title: spec.title.clone(),
        badge: spec.connection.badge().to_string(),
        phase: runtime.phase.clone(),
        origin: spec.display_origin.clone(),
        workspace,
        dsh_home: entry.instance_home.clone(),
        dsh_home_effective: entry.instance_home.clone(),
        runtime: match &spec.connection {
            ConnectionKind::LocalSpawn { runtime, .. } => Some(runtime.short_label()),
            _ => None,
        },
        pid: runtime.handle.as_ref().map(|handle| handle.pid),
        uptime_ms: runtime.uptime_ms(),
        failure: runtime.failure.clone(),
        notice: runtime.notice.clone(),
        plaintext: spec.connection.is_plaintext_remote(),
        has_session_cookie: runtime.has_session_cookie,
        log_path: runtime
            .log_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        exit_code: runtime.exit_code,
        ready: runtime.phase == phase::READY,
        command_line: runtime.command_line.clone(),
    }
}

fn instance_identity(spec: &TabSpec, app: &AppHandle) -> (String, PathBuf) {
    let base = app
        .try_state::<Arc<crate::AppState>>()
        .map(|state| state.data_root.webview_data_dir())
        .unwrap_or_else(|| PathBuf::from("."));
    let key = match &spec.connection {
        ConnectionKind::LocalSpawn { workspace, dsh_home, .. } => {
            let home = match dsh_home {
                DshHomeSetting::Custom { path } => Some(PathBuf::from(path)),
                _ => None,
            };
            crate::root::instance_key(&PathBuf::from(workspace), home.as_deref())
        }
        ConnectionKind::LocalAttach { url } | ConnectionKind::Remote { url, .. } => LaunchUrl::parse(url)
            .map(|parsed| crate::root::instance_key(&PathBuf::from(&parsed.origin), None))
            .unwrap_or_else(|| crate::root::instance_key(&PathBuf::from(&spec.id), None)),
    };
    (key.clone(), base.join(format!("inst-{key}")))
}

fn asset_url(app: &AppHandle, relative: &str, query: &[(&str, String)]) -> tauri::Url {
    // On Windows the app origin is `http://tauri.localhost` (never https: that
    // would forbid mixed content to `http://127.0.0.1`, and flipping it later
    // would move every webview's storage).
    let base = app
        .config()
        .build
        .dev_url
        .as_ref()
        .map(|url| url.to_string())
        .unwrap_or_else(|| "http://tauri.localhost/".to_string());
    let mut joined = format!("{}/{}", base.trim_end_matches('/'), relative);
    if !query.is_empty() {
        let params: Vec<String> = query
            .iter()
            .map(|(key, value)| format!("{key}={}", urlencode(value)))
            .collect();
        joined.push('?');
        joined.push_str(&params.join("&"));
    }
    tauri::Url::parse(&joined)
        .unwrap_or_else(|_| tauri::Url::parse("http://tauri.localhost/").expect("static url"))
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
