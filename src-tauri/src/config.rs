//! dshell's own configuration and the persisted tab list.
//!
//! Two files, both under the data root: `config.json` (preferences) and
//! `tabs.json` (what to restore). Neither ever contains a launch token.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{FailureCode, Failure};

pub const PINNED_DSH_VERSION: &str = "0.2.0-rc.2";
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_RECENTS: usize = 8;

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

    #[serde(default)]
    pub kill_on_close: bool,

    #[serde(default)]
    pub window: WindowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowConfig {
    pub width: f64,
    pub height: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub maximized: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { width: 1280.0, height: 820.0, x: None, y: None, maximized: false }
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
            language: default_language(),
            background_mode: default_background_mode(),
            fixed_port: false,
            kill_on_close: false,
            window: WindowConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => (cfg, None),
                Err(err) => (
                    Config::default(),
                    Some(format!("config.json 无法解析（{err}），已使用默认配置")),
                ),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(err) => (
                Config::default(),
                Some(format!("读取 config.json 失败（{err}），已使用默认配置")),
            ),
        }
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
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("读取 tabs.json 失败（{err}），本次没有恢复任何选项卡")),
                )
            }
        };
        let raw: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("tabs.json 不是合法 JSON（{err}），本次没有恢复任何选项卡")),
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
        (Self { schema_version: SCHEMA_VERSION, order, tabs }, note)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_relative_path_is_rejected() {
        let setting = DshHomeSetting::Custom { path: "relative/home".into() };
        let err = setting
            .resolve(Path::new("E:\\data"), Path::new("E:\\data\\dsh-home"), Path::new("E:\\ws"))
            .unwrap_err();
        assert_eq!(err.code, FailureCode::DshHomeInvalid);
    }

    #[test]
    fn blank_custom_path_means_system_default() {
        let setting = DshHomeSetting::Custom { path: "   ".into() };
        let value = setting
            .resolve(Path::new("E:\\data"), Path::new("E:\\data\\dsh-home"), Path::new("E:\\ws"))
            .unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn instance_mode_derives_from_workspace_name() {
        let setting = DshHomeSetting::Instance;
        let value = setting
            .resolve(Path::new("E:\\data"), Path::new("E:\\data\\dsh-home"), Path::new("E:\\proj\\my-app"))
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
            DshHomeSetting::Custom { path: "E:\\h".into() },
        ] {
            let config = Config { dsh_home: mode, ..Config::default() };
            let json = serde_json::to_string(&config).unwrap();
            let back: Config = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&back.dsh_home).unwrap(),
                serde_json::to_string(&config.dsh_home).unwrap()
            );
        }
    }
}
