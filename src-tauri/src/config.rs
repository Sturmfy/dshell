//! dshell's own configuration and the persisted tab list.
//!
//! Two files, both under the data root: `config.json` (preferences) and
//! `tabs.json` (what to restore). Neither ever contains a launch token.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Failure, FailureCode};

pub const PINNED_DSH_VERSION: &str = "0.2.0-rc.2";
/// `2`: `killOnClose` became `true` by default. See [`Config::migrate`].
pub const SCHEMA_VERSION: u32 = 2;
pub const MAX_RECENTS: usize = 8;
pub const MAX_RECENT_CONNECTIONS: usize = 8;

/// The public repository the "source" runtime clones.
pub const DSH_REPO_URL: &str = "https://github.com/deepseek-ai/deepseek-harness.git";

/// The subdirectory a clone lands in, inside the directory the user picked.
///
/// The user chooses a *place to keep it* — `E:\projects`, not
/// `E:\projects\deepseek-harness` — so dshell never takes over a directory they
/// already use for something else, and "reinstall from scratch" is a single
/// subdirectory to delete.
pub const SOURCE_CHECKOUT_DIR_NAME: &str = "deepseek-harness";

/// Where the source checkout's container lives when the user has not chosen one:
/// `<data root>/runtimes`, so the clone ends up in
/// `<data root>/runtimes/deepseek-harness`.
pub fn default_source_container(data_root: &Path) -> std::path::PathBuf {
    data_root.join("runtimes")
}

/// A saved pointer to "DSH lives here", for the "已有安装 / 源码" runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedInstall {
    pub id: String,
    pub label: String,
    pub root: String,
    /// `"installed"` (npm/pnpm tree) or `"source"` (git checkout).
    pub mode: String,
}

/// One remembered connection target, for the "快速连接" list.
///
/// The URL keeps its `token=` parameter on purpose. DSH's launch token stays
/// valid for the lifetime of the process that printed it, and an entry that
/// cannot reconnect is not worth remembering — that is the whole point of a
/// history list. DSH itself keeps the cookie signing key in plain text in
/// `$DSH_HOME/.credentials.yaml`, so a token in dshell's own config is the same
/// class of exposure on the same machine, scoped to this user's profile (see
/// `docs/reference/dsh-integration.md` §3.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentConnection {
    /// Stable key: the display origin. One entry per origin, refreshed in place.
    pub id: String,
    /// The full launch URL, token included.
    pub url: String,
    pub label: String,
    /// `true` for a non-loopback target (its own badge and warning apply).
    pub remote: bool,
    /// Milliseconds since the Unix epoch; the list is sorted by this.
    #[serde(default)]
    pub last_used_at: u64,
}

impl RecentConnection {
    pub fn new(url: &str, origin: &str, label: &str, remote: bool) -> Self {
        Self {
            id: origin.to_string(),
            url: url.to_string(),
            label: label.to_string(),
            remote,
            last_used_at: now_ms(),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub schema_version: u32,
    pub onboarding_done: bool,

    /// `DSH_HOME` for locally spawned instances.
    ///
    /// Defaults to "leave the variable unset", which makes DSH resolve to
    /// `~/.dsh` and therefore reuse the user's existing sessions, settings and
    /// API keys. Deriving a private home by default is the single most
    /// user-hostile thing dshell could do here.
    #[serde(default)]
    pub dsh_home: DshHomeSetting,
    #[serde(default)]
    pub dsh_agents_home: DshHomeSetting,

    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub recent_workspaces: Vec<String>,

    /// npm dist-tag whose version dshell pins for npx / npm runtimes.
    #[serde(default = "default_channel")]
    pub dsh_version_channel: String,

    #[serde(default = "default_runtime_id")]
    pub default_runtime: String,
    #[serde(default)]
    pub managed_installs: Vec<ManagedInstall>,
    #[serde(default)]
    pub extra_binary: Option<String>,

    /// The directory the "source" runtime keeps its checkout *in*.
    ///
    /// This is the parent, not the checkout: the clone always lands in
    /// `<container>/deepseek-harness`. `None` means
    /// [`default_source_container`] — `<data root>/runtimes` — which is what the
    /// "no checkout yet" dialog proposes.
    #[serde(default)]
    pub source_container: Option<String>,

    /// Remembered attach/remote targets, newest first. See [`RecentConnection`].
    #[serde(default)]
    pub recent_connections: Vec<RecentConnection>,

    /// Origins (`http://host:port`) the user pinned above the ordinary lists.
    ///
    /// Stored as the same origin key used by [`RecentConnection::id`] and the
    /// local scan, so a pin can be matched without persisting a second copy of
    /// the URL or its token.
    #[serde(default)]
    pub pinned_connections: Vec<String>,

    /// User-chosen tab names, keyed by the tab's stable instance identity.
    ///
    /// The identity is the same `instanceKey` the tab view and webview storage
    /// already use (workspace + DSH_HOME for a spawned backend, origin for an
    /// attached one), so a name follows its connection across restarts even
    /// when DSH changes its own session title. `tabs.json` also carries the
    /// current title; this map is what lets a *new* tab for the same instance
    /// (or a reconnect from history) pick the name back up.
    #[serde(default)]
    pub tab_names: BTreeMap<String, String>,

    /// Preferred UI language: `"system"`, `"zh"` or `"en"`.
    #[serde(default = "default_language")]
    pub language: String,

    /// `"move-offscreen"` keeps background sessions live (default);
    /// `"hide"` saves memory but can throttle SSE streams.
    #[serde(default = "default_background_mode")]
    pub background_mode: String,

    /// Remember the last port per instance so cookies survive restarts.
    #[serde(default)]
    pub fixed_port: bool,

    /// End a locally started backend when its tab closes.
    ///
    /// Defaults to `true`: a tab is the only handle the user has on a backend
    /// dshell started, so closing it and leaving the process running is a leak
    /// with a port and a `DSH_HOME` attached. The new-connection page exposes the
    /// switch in its advanced options, and a saved tab keeps its own answer.
    #[serde(default = "default_kill_on_close")]
    pub kill_on_close: bool,

    /// Keep DSH's working indicators animated even when Windows asks for
    /// reduced motion.
    ///
    /// This is a WebView2 browser argument on DSH content tabs only; dshell's
    /// own pages still follow the system preference. Browser arguments are
    /// fixed when a WebView2 environment is created, so the value applies to
    /// tabs created afterwards; restart dshell to apply it to tabs that are
    /// already open.
    #[serde(default = "default_force_dsh_motion")]
    pub force_dsh_motion: bool,

    /// Hide the main window to the system tray on close instead of exiting.
    ///
    /// Defaults to `true`: closing a window is not the same intent as ending
    /// every DSH session it hosts, and the tray's own 退出 item is the explicit
    /// stop. The value is mirrored into an atomic on [`crate::state::AppState`]
    /// because the window's `CloseRequested` handler runs on the main thread and
    /// cannot await the config lock.
    ///
    /// Additive on purpose: an older `config.json` without the field takes the
    /// default, so no schema migration is needed (the same precedent as
    /// `forceDshMotion`).
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,

    #[serde(default)]
    pub window: WindowConfig,
}

/// Initial size of the main window.
///
/// Only the *first* launch reads this. Once the window has closed normally its
/// frame is remembered in `window.json` (see [`crate::window_state`]), which is a
/// state file rather than a preference for two reasons: a close has to record it
/// synchronously, and a bad rectangle must not be able to corrupt the rest of the
/// configuration. Keeping the first-launch default here means a fresh install
/// still opens with a sensible window, and a hand-written size still works.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowConfig {
    pub width: f64,
    pub height: f64,
    /// Open maximized when there is no `window.json` yet.
    pub maximized: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 820.0,
            maximized: false,
        }
    }
}

/// Tri-state `DSH_HOME` choice, mirroring the three radio options in the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum DshHomeSetting {
    /// Leave the variable unset → DSH uses `~/.dsh`.
    #[default]
    System,
    /// `<data root>/dsh-home/<workspace slug>/` — explicit opt-in isolation.
    Instance,
    /// An absolute path the user typed.
    Custom { path: String },
}

impl DshHomeSetting {
    /// Resolve to the value to inject, or `None` when the variable must be left
    /// untouched. A relative path or a blank string is a hard error: DSH resolves
    /// relative paths against the *child's* cwd, so the state root would drift
    /// with the working directory.
    pub fn resolve(
        &self,
        data_root: &Path,
        dsh_home_root: &Path,
        workspace: &Path,
    ) -> Result<Option<String>, Failure> {
        match self {
            DshHomeSetting::System => Ok(None),
            DshHomeSetting::Instance => {
                let slug = crate::root::slug(
                    &workspace
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "workspace".into()),
                );
                Ok(Some(dsh_home_root.join(slug).to_string_lossy().to_string()))
            }
            DshHomeSetting::Custom { path } => {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                let candidate = Path::new(trimmed);
                if !candidate.is_absolute() {
                    return Err(Failure::new(
                        FailureCode::DshHomeInvalid,
                        format!("DSH_HOME 必须是绝对路径，收到相对路径：{trimmed}"),
                    ));
                }
                let _ = data_root;
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

fn default_channel() -> String {
    "pinned".into()
}
fn default_runtime_id() -> String {
    "npx".into()
}
fn default_language() -> String {
    "system".into()
}
fn default_background_mode() -> String {
    "move-offscreen".into()
}
fn default_kill_on_close() -> bool {
    true
}
fn default_force_dsh_motion() -> bool {
    true
}
fn default_close_to_tray() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            onboarding_done: false,
            dsh_home: DshHomeSetting::default(),
            dsh_agents_home: DshHomeSetting::default(),
            workspace: None,
            recent_workspaces: Vec::new(),
            dsh_version_channel: default_channel(),
            default_runtime: default_runtime_id(),
            managed_installs: Vec::new(),
            extra_binary: None,
            source_container: None,
            recent_connections: Vec::new(),
            pinned_connections: Vec::new(),
            tab_names: BTreeMap::new(),
            language: default_language(),
            background_mode: default_background_mode(),
            fixed_port: false,
            kill_on_close: default_kill_on_close(),
            force_dsh_motion: default_force_dsh_motion(),
            close_to_tray: default_close_to_tray(),
            window: WindowConfig::default(),
        }
    }
}

impl Config {
    /// Load `config.json`, migrating an older file in memory.
    ///
    /// Returns `(config, note, migrated)`: `note` is for the user (a file that
    /// would not parse), `migrated` tells the caller the file on disk is stale and
    /// should be written back once — otherwise every launch would migrate again
    /// and the file would keep disagreeing with what the app uses.
    pub fn load(path: &Path) -> (Self, Option<String>, bool) {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(strip_bom(&text)) {
                Ok(mut cfg) => {
                    let before = (cfg.schema_version, cfg.kill_on_close);
                    cfg.migrate();
                    let migrated = (cfg.schema_version, cfg.kill_on_close) != before;
                    if migrated {
                        if let Some(root) = path.parent() {
                            crate::dsh::process::debug_log(
                                root,
                                &format!(
                                    "config.json migrated: schema {} -> {} (killOnClose {} -> {})",
                                    before.0, cfg.schema_version, before.1, cfg.kill_on_close
                                ),
                            );
                        }
                    }
                    (cfg, None, migrated)
                }
                Err(err) => (
                    Config::default(),
                    Some(format!("config.json 无法解析（{err}），已使用默认配置")),
                    false,
                ),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (Config::default(), None, false)
            }
            Err(err) => (
                Config::default(),
                Some(format!("读取 config.json 失败（{err}），已使用默认配置")),
                false,
            ),
        }
    }

    /// Carry an older `config.json` onto the current defaults.
    ///
    /// `schemaVersion: 1` was written by a build whose *default* was
    /// `killOnClose: false`, and `update_config` persisted that default the first
    /// time anything was saved. So a stored `false` from that era is the old
    /// default, not a decision: it is exactly the value a user never chose, and it
    /// made the advanced-options checkbox come up unchecked while the product
    /// default is to end the backend with its tab. Schema 2 therefore forces it
    /// once; after that the value the user saves is preserved like any other.
    ///
    /// The migrated schema number is written on the next save, which is every
    /// launch that changes anything — and re-running this on an unmigrated file
    /// is idempotent.
    pub fn migrate(&mut self) {
        if self.schema_version < 2 {
            self.kill_on_close = true;
            self.schema_version = 2;
        }
        if self.schema_version > SCHEMA_VERSION {
            // A file from a newer build: keep it, but do not claim its version
            // (it is neither ours to read nor to downgrade).
            return;
        }
        self.schema_version = SCHEMA_VERSION;
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }

    pub fn remember_workspace(&mut self, workspace: &Path) {
        let path = workspace.to_string_lossy().to_string();
        self.recent_workspaces.retain(|p| p != &path);
        self.recent_workspaces.insert(0, path);
        self.recent_workspaces.truncate(MAX_RECENTS);
    }

    /// Record a successful connection target, newest first.
    ///
    /// Keyed by origin rather than by URL: the same instance prints a new token
    /// on every start, so keying on the whole URL would fill the list with
    /// near-duplicates of one target while leaving stale ones behind.
    pub fn remember_connection(&mut self, entry: RecentConnection) {
        self.recent_connections
            .retain(|existing| existing.id != entry.id);
        self.recent_connections.insert(0, entry);
        self.recent_connections.truncate(MAX_RECENT_CONNECTIONS);
    }

    pub fn forget_connection(&mut self, id: &str) {
        self.recent_connections.retain(|existing| existing.id != id);
    }

    /// The saved name for one instance, if the user has ever renamed it.
    pub fn tab_name(&self, instance_key: &str) -> Option<&str> {
        self.tab_names
            .get(instance_key)
            .map(String::as_str)
            .filter(|name| !name.trim().is_empty())
    }

    /// Persist a tab's user-chosen name under its stable instance identity.
    ///
    /// Names are trimmed and bounded here as well as at the command boundary:
    /// `config.json` is hand-editable, and a blank value must mean "no custom
    /// name" rather than a tab that renders as an empty title.
    pub fn remember_tab_name(&mut self, instance_key: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.tab_names.remove(instance_key);
            return;
        }
        self.tab_names
            .insert(instance_key.to_string(), name.chars().take(64).collect());
    }

    /// Keep a remembered connection's label in step with a tab rename.
    ///
    /// The history list is keyed by origin, so only attach/remote tabs have an
    /// entry to update; a spawned backend is identified by its `instance_key`.
    pub fn rename_connection_label(&mut self, origin: &str, label: &str) {
        for entry in &mut self.recent_connections {
            if entry.id == origin {
                entry.label = label.to_string();
            }
        }
    }

    /// The directory the user chose to keep the checkout *in*.
    pub fn source_container_dir(&self, data_root: &Path) -> std::path::PathBuf {
        match self
            .source_container
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(path) => std::path::PathBuf::from(path),
            None => default_source_container(data_root),
        }
    }

    /// The git checkout itself: always `<container>/deepseek-harness`.
    ///
    /// The subdirectory is fixed rather than configurable: it is what makes
    /// "choose a place" (a parent folder, possibly one with other projects in it)
    /// and "delete the runtime" (one folder) both work without dshell ever owning
    /// the directory the user picked.
    pub fn source_checkout_dir(&self, data_root: &Path) -> std::path::PathBuf {
        self.source_container_dir(data_root)
            .join(SOURCE_CHECKOUT_DIR_NAME)
    }

    /// The npm spec the UI should talk about: `@deepseek-ai/dsh@<channel>`.
    pub fn package_spec(&self) -> String {
        match self.dsh_version_channel.as_str() {
            "latest" => "@deepseek-ai/dsh@latest".to_string(),
            "alpha" => "@deepseek-ai/dsh@alpha".to_string(),
            _ => format!("@deepseek-ai/dsh@{PINNED_DSH_VERSION}"),
        }
    }
}

/// Persisted tab list. Deliberately token-free: `launch_url` only ever lives in
/// memory, so a stolen `tabs.json` leaks nothing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TabsFile {
    pub schema_version: u32,
    pub order: Vec<String>,
    pub tabs: Vec<crate::tabs::state::TabSpec>,
}

impl TabsFile {
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None)
            }
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!(
                        "读取 tabs.json 失败（{err}），本次没有恢复任何选项卡"
                    )),
                )
            }
        };
        let raw: serde_json::Value = match serde_json::from_str(strip_bom(&text)) {
            Ok(value) => value,
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!(
                        "tabs.json 不是合法 JSON（{err}），本次没有恢复任何选项卡"
                    )),
                )
            }
        };

        // Parse tab by tab: a single unreadable entry must not cost the user
        // every other tab. Skipped entries are reported rather than swallowed.
        let order: Vec<String> = raw
            .get("order")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let mut tabs = Vec::new();
        let mut skipped = 0usize;
        if let Some(array) = raw.get("tabs").and_then(|value| value.as_array()) {
            for entry in array {
                match serde_json::from_value::<crate::tabs::state::TabSpec>(entry.clone()) {
                    Ok(spec) => tabs.push(spec),
                    Err(_) => skipped += 1,
                }
            }
        }
        let note = if skipped > 0 {
            Some(format!("tabs.json 里有 {skipped} 个选项卡无法读取，已跳过"))
        } else {
            None
        };
        (
            Self {
                schema_version: SCHEMA_VERSION,
                order,
                tabs,
            },
            note,
        )
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }
}

/// Drop a UTF-8 byte-order mark before parsing.
///
/// Both files under the data root are meant to be hand-editable, and the way a
/// user on Windows actually edits them — Notepad saving "UTF-8 with BOM" —
/// prepends a byte that `serde_json` rejects. The failure mode is not cosmetic:
/// a `config.json` that will not parse resets the config, which loses the chosen
/// data root and re-runs the first-run wizard over the user's setup.
///
/// Only the BOM is stripped; leading whitespace is already legal JSON, and
/// masking real syntax errors would make a malformed file look almost-right.
fn strip_bom(text: &str) -> &str {
    text.trim_start_matches('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_relative_path_is_rejected() {
        let setting = DshHomeSetting::Custom {
            path: "relative/home".into(),
        };
        let err = setting
            .resolve(
                Path::new("E:\\data"),
                Path::new("E:\\data\\dsh-home"),
                Path::new("E:\\ws"),
            )
            .unwrap_err();
        assert_eq!(err.code, FailureCode::DshHomeInvalid);
    }

    #[test]
    fn blank_custom_path_means_system_default() {
        let setting = DshHomeSetting::Custom { path: "   ".into() };
        let value = setting
            .resolve(
                Path::new("E:\\data"),
                Path::new("E:\\data\\dsh-home"),
                Path::new("E:\\ws"),
            )
            .unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn instance_mode_derives_from_workspace_name() {
        let setting = DshHomeSetting::Instance;
        let value = setting
            .resolve(
                Path::new("E:\\data"),
                Path::new("E:\\data\\dsh-home"),
                Path::new("E:\\proj\\my-app"),
            )
            .unwrap()
            .unwrap();
        assert!(value.ends_with("dsh-home\\my-app"));
    }

    /// The restore path is only as good as this parser: a silent failure here
    /// means a user's tabs quietly disappear on restart.
    #[test]
    fn tabs_file_round_trips_a_managed_local_spawn() {
        let json = r#"{
          "schemaVersion": 1,
          "order": ["smoketab1"],
          "tabs": [
            {
              "id": "smoketab1",
              "title": "local",
              "connection": {
                "type": "localSpawn",
                "workspace": "E:\\ws",
                "dshHome": { "mode": "custom", "path": "E:\\home" },
                "dshAgentsHome": { "mode": "system" },
                "runtime": { "kind": "managed", "root": "E:\\rt", "mode": "installed" },
                "killOnClose": true
              }
            }
          ]
        }"#;
        let parsed: TabsFile = serde_json::from_str(json).expect("tabs.json must parse");
        assert_eq!(parsed.tabs.len(), 1);
        assert_eq!(parsed.order, vec!["smoketab1".to_string()]);

        let back = serde_json::to_string(&parsed).unwrap();
        let again: TabsFile = serde_json::from_str(&back).expect("round trip must parse");
        assert_eq!(again.tabs.len(), 1);
    }

    #[test]
    fn tabs_file_round_trips_attach_kinds() {
        for connection in [
            r#"{ "type": "localAttach", "url": "http://127.0.0.1:1/?token=x" }"#,
            r#"{ "type": "remote", "url": "http://10.0.0.1:1/?token=x", "allowPlaintext": true }"#,
        ] {
            let json = format!(
                r#"{{"schemaVersion":1,"order":["a"],"tabs":[{{"id":"a","title":"t","connection":{connection}}}]}}"#
            );
            let parsed: TabsFile =
                serde_json::from_str(&json).unwrap_or_else(|err| panic!("{connection}: {err}"));
            assert_eq!(parsed.tabs.len(), 1);
        }
    }

    #[test]
    fn config_round_trips_every_dsh_home_mode() {
        for mode in [
            DshHomeSetting::System,
            DshHomeSetting::Instance,
            DshHomeSetting::Custom {
                path: "E:\\h".into(),
            },
        ] {
            let config = Config {
                dsh_home: mode,
                ..Config::default()
            };
            let json = serde_json::to_string(&config).unwrap();
            let back: Config = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&back.dsh_home).unwrap(),
                serde_json::to_string(&config.dsh_home).unwrap()
            );
        }
    }

    /// Both files are meant to be hand-editable, and the editors people actually
    /// use put a BOM in front of what they save. A parse failure here silently
    /// throws away the user's configuration, so it is worth a test of its own.
    #[test]
    fn a_utf8_bom_does_not_wipe_the_config() {
        let dir = std::env::temp_dir().join(format!("dshell-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let config_file = dir.join("config.json");
        let body = r#"{"schemaVersion":1,"onboardingDone":true,"recentWorkspaces":["E:\\ws"]}"#;
        std::fs::write(&config_file, format!("\u{feff}{body}")).unwrap();
        let (config, note, _migrated) = Config::load(&config_file);
        assert!(config.onboarding_done, "a BOM must not reset the config");
        assert_eq!(config.recent_workspaces, vec!["E:\\ws".to_string()]);
        assert!(note.is_none(), "unexpected note: {note:?}");

        let tabs_file = dir.join("tabs.json");
        std::fs::write(&tabs_file, format!("\u{feff}{body}")).unwrap();
        let (_, note) = TabsFile::load(&tabs_file);
        assert!(
            note.is_none(),
            "a BOM must not discard the tab list: {note:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_bom_leaves_everything_else_alone() {
        assert_eq!(strip_bom("\u{feff}{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_bom("{\"a\":1}"), "{\"a\":1}");
        // Whitespace is legal JSON, so it is deliberately not touched.
        assert_eq!(strip_bom("  {\"a\":1}"), "  {\"a\":1}");
    }

    /// Closing a tab is the user's only handle on a backend dshell started, so
    /// the default is to end the process with it. A tab records its own answer,
    /// so flipping this default cannot change an already-created tab.
    #[test]
    fn closing_a_tab_ends_the_backend_by_default() {
        let fresh = Config::default();
        assert!(fresh.kill_on_close);
        assert_eq!(fresh.schema_version, SCHEMA_VERSION);

        // A file that predates the field at all: serde's default decides.
        let parsed: Config =
            serde_json::from_str(r#"{"schemaVersion":1,"onboardingDone":false}"#).unwrap();
        assert!(parsed.kill_on_close);
    }

    /// DSH's working indicators keep moving by default, even when Windows asks
    /// for reduced motion. A stored `false` is the user's choice and survives a
    /// save/load cycle; there is no migration because the field is additive.
    #[test]
    fn dsh_motion_defaults_on_and_a_stored_false_is_a_decision() {
        assert!(Config::default().force_dsh_motion);

        let parsed: Config =
            serde_json::from_str(r#"{"schemaVersion":2,"onboardingDone":true}"#).unwrap();
        assert!(
            parsed.force_dsh_motion,
            "an older config takes the new default"
        );

        let parsed: Config = serde_json::from_str(
            r#"{"schemaVersion":2,"onboardingDone":true,"forceDshMotion":false}"#,
        )
        .unwrap();
        assert!(
            !parsed.force_dsh_motion,
            "a stored false must not be overwritten"
        );

        let config = Config {
            force_dsh_motion: false,
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert!(!back.force_dsh_motion);
    }

    /// Closing a window means "put it away", not "end every DSH session", so the
    /// tray default is on. A config written before the field existed takes the
    /// default, while an explicit `false` is the user's decision and survives a
    /// save/load cycle — there is no migration because the field is additive.
    #[test]
    fn close_to_tray_defaults_on_and_a_stored_false_is_a_decision() {
        assert!(Config::default().close_to_tray);

        let parsed: Config =
            serde_json::from_str(r#"{"schemaVersion":2,"onboardingDone":true}"#).unwrap();
        assert!(
            parsed.close_to_tray,
            "an older config takes the new default"
        );

        let parsed: Config = serde_json::from_str(
            r#"{"schemaVersion":2,"onboardingDone":true,"closeToTray":false}"#,
        )
        .unwrap();
        assert!(
            !parsed.close_to_tray,
            "a stored false must not be overwritten"
        );

        let config = Config {
            close_to_tray: false,
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert!(!back.close_to_tray);
    }

    /// `schemaVersion: 1` stored `killOnClose: false`, because that *was* the
    /// default: `update_config` writes the whole config, so everyone who ever
    /// opened settings has a `false` on disk that they never chose. Reading it
    /// back unchanged would leave the advanced-options checkbox unchecked while
    /// the product default is "end the backend with its tab".
    #[test]
    fn an_older_config_gets_the_new_kill_on_close_default_once() {
        let dir = std::env::temp_dir().join(format!("dshell-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.json");

        std::fs::write(
            &file,
            r#"{"schemaVersion":1,"onboardingDone":true,"killOnClose":false,"recentWorkspaces":["E:\\ws"]}"#,
        )
        .unwrap();
        let (migrated, note, changed) = Config::load(&file);
        assert!(note.is_none(), "{note:?}");
        assert!(
            changed,
            "the caller has to know it should write the file back"
        );
        assert!(
            migrated.kill_on_close,
            "the old default must not survive as a choice"
        );
        assert_eq!(migrated.schema_version, SCHEMA_VERSION);
        assert_eq!(migrated.recent_workspaces, vec!["E:\\ws".to_string()]);

        // Once the file is at schema 2, a `false` is the user's own answer and is
        // preserved — including across a save/load cycle.
        migrated.save(&file).unwrap();
        let mut config = migrated;
        config.kill_on_close = false;
        config.save(&file).unwrap();
        let (again, note, changed) = Config::load(&file);
        assert!(note.is_none(), "{note:?}");
        assert!(!changed, "a schema-2 file needs no migration");
        assert!(!again.kill_on_close, "a schema-2 false is a decision");
        assert_eq!(again.schema_version, SCHEMA_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Migration is idempotent and never touches a file from a newer build.
    #[test]
    fn migrate_is_idempotent_and_leaves_future_versions_alone() {
        let mut config = Config {
            kill_on_close: false,
            ..Config::default()
        };
        config.migrate();
        assert!(!config.kill_on_close, "already at the current schema");

        let mut future = Config {
            schema_version: SCHEMA_VERSION + 1,
            kill_on_close: false,
            ..Config::default()
        };
        future.migrate();
        assert!(!future.kill_on_close);
        assert_eq!(future.schema_version, SCHEMA_VERSION + 1);
    }

    /// One entry per origin, newest first, bounded: the list is a shortcut, not
    /// a log, and the same instance prints a new token on every start.
    #[test]
    fn recent_connections_are_deduplicated_by_origin() {
        let mut config = Config::default();
        config.remember_connection(RecentConnection::new(
            "http://127.0.0.1:3080/?token=a",
            "http://127.0.0.1:3080",
            "本地",
            false,
        ));
        config.remember_connection(RecentConnection::new(
            "http://10.0.0.2:3080/?token=b",
            "http://10.0.0.2:3080",
            "远程",
            true,
        ));
        config.remember_connection(RecentConnection::new(
            "http://127.0.0.1:3080/?token=c",
            "http://127.0.0.1:3080",
            "本地",
            false,
        ));
        assert_eq!(config.recent_connections.len(), 2);
        assert_eq!(
            config.recent_connections[0].url,
            "http://127.0.0.1:3080/?token=c"
        );
        assert_eq!(config.recent_connections[1].id, "http://10.0.0.2:3080");

        config.forget_connection("http://127.0.0.1:3080");
        assert_eq!(config.recent_connections.len(), 1);

        for index in 0..(MAX_RECENT_CONNECTIONS + 4) {
            let origin = format!("http://127.0.0.1:{}", 9000 + index);
            config.remember_connection(RecentConnection::new(
                &format!("{origin}/?token=t"),
                &origin,
                "本地",
                false,
            ));
        }
        assert_eq!(config.recent_connections.len(), MAX_RECENT_CONNECTIONS);
    }

    /// A rename is remembered per instance, survives a save/load cycle, and
    /// updates the history label users actually see.
    #[test]
    fn tab_names_round_trip_and_rename_history_labels() {
        let mut config = Config::default();
        config.remember_connection(RecentConnection::new(
            "http://127.0.0.1:3080/?token=a",
            "http://127.0.0.1:3080",
            "本地 · 3080",
            false,
        ));

        config.remember_tab_name("inst-abc", "  工作台  ");
        config.remember_tab_name("inst-empty", "   ");
        config.rename_connection_label("http://127.0.0.1:3080", "工作台");
        assert_eq!(config.tab_name("inst-abc"), Some("工作台"));
        assert_eq!(config.tab_name("inst-empty"), None);
        assert_eq!(config.recent_connections[0].label, "工作台");

        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tab_name("inst-abc"), Some("工作台"));
        assert_eq!(back.recent_connections[0].label, "工作台");

        // A blank name removes the entry instead of persisting an empty tab.
        let mut config = back;
        config.remember_tab_name("inst-abc", "");
        assert_eq!(config.tab_name("inst-abc"), None);
    }

    #[test]
    fn source_checkout_falls_back_inside_the_data_root() {
        let config = Config::default();
        // The default container is `<data root>\runtimes`, and the checkout is one
        // fixed subdirectory inside whatever container the user picked.
        assert_eq!(
            config.source_checkout_dir(Path::new(r"E:\dshell-data")),
            std::path::PathBuf::from(r"E:\dshell-data\runtimes\deepseek-harness")
        );
        let chosen = Config {
            source_container: Some(r"D:\dsh".into()),
            ..Config::default()
        };
        assert_eq!(
            chosen.source_container_dir(Path::new(r"E:\dshell-data")),
            std::path::PathBuf::from(r"D:\dsh")
        );
        assert_eq!(
            chosen.source_checkout_dir(Path::new(r"E:\dshell-data")),
            std::path::PathBuf::from(r"D:\dsh\deepseek-harness")
        );
    }
}
