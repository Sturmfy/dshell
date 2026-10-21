//! dshell application state — the single owner of the data root, the config and
//! the tab manager.
//!
//! Everything that needs "where does dshell keep its data" asks here, so the
//! four-level fallback chain is resolved exactly once per process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::config::{Config, TabsFile};
use crate::error::Failure;
use crate::root::{self, DataRoot};
use crate::runtime::node::{self, EnvironmentSnapshot, NodeInfo};
use crate::runtime::npxcache::DistTags;
use crate::tabs::manager::TabManager;

pub struct AppState {
    pub data_root: DataRoot,
    pub manager: Arc<TabManager>,
    pub config: Mutex<Config>,
    /// A synchronous mirror of `config.close_to_tray`.
    ///
    /// The window's `CloseRequested` handler runs on the main thread and cannot
    /// await the config lock, but it must answer "hide or exit?" before returning
    /// (`prevent_close` is a synchronous decision). This is that answer; it is
    /// updated only through [`Self::update_config`], the single write point for
    /// the config.
    pub close_to_tray: AtomicBool,
    pub node: Mutex<Option<NodeInfo>>,
    pub environment: Mutex<Option<EnvironmentSnapshot>>,
    /// Serialises "install pnpm": two of dshell's pages can offer the button, and
    /// two concurrent global npm installs would fight over npm's own lock.
    pub pnpm_install: Mutex<()>,
    /// Serialises the owned native pickers. A second picker has nowhere to show
    /// a waiting state, so it fails with a readable busy error instead.
    pub picker_lock: Mutex<()>,
    /// Serialises runtime preparation: one npx download or git clone at a time.
    pub prepare: Mutex<()>,
    /// Registry dist-tags, cached for a few minutes. The only value in this
    /// process that describes something outside the machine.
    pub dist_tags: Mutex<Option<(std::time::Instant, DistTags)>>,
    pub tabs_file: PathBuf,
    /// Set when something could not be persisted or a fallback kicked in; the UI
    /// surfaces it instead of silently doing something else.
    pub boot_note: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(data_root: DataRoot, manager: Arc<TabManager>, config: Config) -> Self {
        let tabs_file = data_root.tabs_file();
        let note = data_root.note.clone();
        let close_to_tray = AtomicBool::new(config.close_to_tray);
        Self {
            data_root,
            manager,
            config: Mutex::new(config),
            close_to_tray,
            node: Mutex::new(None),
            environment: Mutex::new(None),
            pnpm_install: Mutex::new(()),
            picker_lock: Mutex::new(()),
            prepare: Mutex::new(()),
            dist_tags: Mutex::new(None),
            tabs_file,
            boot_note: Mutex::new(note),
        }
    }

    /// Where DSH will actually keep its state for a given tab.
    ///
    /// Resolved once, at tab creation, so the UI can always show the real path
    /// instead of a guess — including for the default case, where dshell sets
    /// nothing and DSH resolves `~/.dsh` itself.
    pub fn instance_home_for(&self, spec: &crate::tabs::state::TabSpec) -> Option<String> {
        use crate::config::DshHomeSetting;
        use crate::tabs::state::ConnectionKind;
        let ConnectionKind::LocalSpawn { workspace, dsh_home, .. } = &spec.connection else {
            return None;
        };
        match dsh_home {
            DshHomeSetting::System => {
                root::preview_default_dsh_home().map(|path| path.to_string_lossy().to_string())
            }
            DshHomeSetting::Instance => {
                // The slug follows whatever identifies this tab's work, falling
                // back to the same default the spawn itself uses.
                let name = workspace
                    .as_deref()
                    .map(PathBuf::from)
                    .or_else(root::default_workspace)
                    .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
                    .map(|name| root::slug(&name))
                    .unwrap_or_else(|| "workspace".into());
                Some(
                    self.data_root
                        .dsh_home_dir()
                        .join(name)
                        .to_string_lossy()
                        .to_string(),
                )
            }
            DshHomeSetting::Custom { path } => Some(path.clone()),
        }
    }

    /// Whether the user has completed the first-run wizard. The wizard is a tab
    /// like any other, but this is what decides whether it opens at startup and
    /// whether it may be closed.
    pub async fn onboarding_done(&self) -> bool {
        self.config.lock().await.onboarding_done
    }

    pub async fn node(&self) -> Option<NodeInfo> {
        {
            let cached = self.node.lock().await;
            if let Some(node) = cached.as_ref() {
                return Some(node.clone());
            }
        }
        let detected = node::detect_node(&self.data_root.path).await;
        let mut cached = self.node.lock().await;
        *cached = detected.clone();
        detected
    }

    /// Full environment snapshot, cached until the user asks to re-detect.
    pub async fn environment(&self, refresh: bool) -> EnvironmentSnapshot {
        if !refresh {
            let cached = self.environment.lock().await;
            if let Some(snapshot) = cached.as_ref() {
                return snapshot.clone();
            }
        }
        let snapshot = node::detect(&self.data_root.path).await;
        {
            let mut cached = self.environment.lock().await;
            *cached = Some(snapshot.clone());
        }
        {
            let mut node_cache = self.node.lock().await;
            *node_cache = snapshot.node.clone();
        }
        snapshot
    }

    pub async fn config_snapshot(&self) -> Config {
        self.config.lock().await.clone()
    }

    pub async fn update_config<F>(&self, mutate: F) -> Result<Config, Failure>
    where
        F: FnOnce(&mut Config),
    {
        let snapshot = {
            let mut config = self.config.lock().await;
            mutate(&mut config);
            config.clone()
        };
        snapshot.save(&self.data_root.config_file()).map_err(|err| {
            Failure::new(
                crate::error::FailureCode::DataRootUnwritable,
                format!("无法写入配置：{err}"),
            )
            .with_detail(self.data_root.config_file().display().to_string())
        })?;
        // Every config write goes through here, so the synchronous close policy
        // cannot drift from the saved file. Updated after the save: a rejected
        // write must not change how the next close behaves.
        self.close_to_tray
            .store(snapshot.close_to_tray, Ordering::Relaxed);
        Ok(snapshot)
    }

    /// Persist the tab list. `TabSpec` is token-free by construction, so this can
    /// never leak a credential to disk.
    pub async fn persist_tabs(&self) {
        let (specs, order) = self.manager.specs().await;
        let file = TabsFile {
            schema_version: crate::config::SCHEMA_VERSION,
            order,
            tabs: specs,
        };
        if let Err(err) = file.save(&self.tabs_file) {
            let mut note = self.boot_note.lock().await;
            *note = Some(format!("无法保存选项卡列表：{err}"));
        }
    }

    /// Tabs to restore, plus a note when some of them could not be read.
    pub async fn load_tabs(&self) -> (TabsFile, Option<String>) {
        TabsFile::load(&self.tabs_file)
    }
}

/// Resolve the data root, create its skeleton and load the config.
pub fn bootstrap(cli_override: Option<PathBuf>) -> (DataRoot, Config) {
    let data_root = root::locate(cli_override);
    for dir in [
        data_root.path.clone(),
        data_root.runtimes_dir(),
        data_root.webview_data_dir(),
        data_root.logs_dir(),
    ] {
        let _ = std::fs::create_dir_all(dir);
    }
    let config_file = data_root.config_file();
    let (config, note, migrated) = Config::load(&config_file);
    if let Some(note) = note {
        crate::dsh::process::debug_log(&data_root.path, &format!("config: {note}"));
    }
    if migrated {
        // Write the migrated values back once, so the file and the running app
        // cannot drift apart — and so the next launch has nothing to migrate.
        if let Err(err) = config.save(&config_file) {
            crate::dsh::process::debug_log(
                &data_root.path,
                &format!("could not persist the migrated config: {err}"),
            );
        }
    }
    (data_root, config)
}

pub type SharedState = Arc<AppState>;
