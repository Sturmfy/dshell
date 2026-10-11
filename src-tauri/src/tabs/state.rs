//! Tab descriptors and runtime state.
//!
//! `TabSpec` is what gets persisted; `TabRuntime` never leaves memory. The split
//! exists so that a launch token cannot possibly reach `tabs.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::DshHomeSetting;
use crate::dsh::process::LogEntry;
use crate::error::Failure;
use crate::runtime::RuntimeSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSpec {
    pub id: String,
    pub title: String,
    pub connection: ConnectionKind,
    /// `http://127.0.0.1:PORT` — display and de-duplication only, token-free.
    #[serde(default)]
    pub display_origin: Option<String>,
}

/// serde's `rename_all` applies to a struct's own fields; enum *variant* fields
/// need explicit renames to keep the whole wire format camelCase for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConnectionKind {
    /// A backend dshell starts and supervises.
    LocalSpawn {
        /// Where the backend process starts. This is only DSH's *initial*
        /// workspace: DSH lets the user pick a project folder itself, so leaving
        /// this empty is the normal case, and resolving it to the user's home
        /// keeps DSH's own project selection meaningful. dshell must not pretend
        /// to own that decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(rename = "dshHome")]
        dsh_home: DshHomeSetting,
        /// `None` means "follow the configured default", which is itself
        /// "unset" unless the user changed it.
        #[serde(rename = "dshAgentsHome", default, skip_serializing_if = "Option::is_none")]
        dsh_agents_home: Option<DshHomeSetting>,
        runtime: RuntimeSpec,
        #[serde(rename = "killOnClose", default)]
        kill_on_close: bool,
    },
    /// A local instance dshell did not start.
    LocalAttach { url: String },
    /// A remote instance.
    Remote {
        url: String,
        #[serde(rename = "allowPlaintext", default)]
        allow_plaintext: bool,
    },
}

impl ConnectionKind {
    /// Short badge text for the tab strip / diagnostics.
    pub fn badge(&self) -> &'static str {
        match self {
            ConnectionKind::LocalSpawn { .. } => "local",
            ConnectionKind::LocalAttach { .. } => "attach",
            ConnectionKind::Remote { .. } => "remote",
        }
    }

    pub fn is_plaintext_remote(&self) -> bool {
        matches!(self, ConnectionKind::Remote { allow_plaintext: true, .. })
    }

    /// The working directory the user chose, if any. `None` means "let DSH
    /// decide" — dshell then starts the backend in the user's home directory.
    pub fn workspace(&self) -> Option<&str> {
        match self {
            ConnectionKind::LocalSpawn { workspace, .. } => workspace.as_deref(),
            _ => None,
        }
    }
}

/// The phase vocabulary, as plain strings so the frontend contract is obvious
/// and cannot drift through a serializer rename.
pub mod phase {
    pub const IDLE: &str = "idle";
    pub const PREPARING: &str = "preparing";
    pub const STARTING: &str = "starting";
    pub const WAITING_FOR_READY: &str = "waitingForReady";
    pub const READY: &str = "ready";
    pub const FAILED: &str = "failed";
    pub const EXITED: &str = "exited";
    pub const NEEDS_TOKEN: &str = "needsToken";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabView {
    pub id: String,
    pub title: String,
    pub badge: String,
    pub phase: String,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub dsh_home: Option<String>,
    /// Where DSH will actually keep its state (preview shown in the UI).
    #[serde(default)]
    pub dsh_home_effective: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub uptime_ms: Option<u64>,
    #[serde(default)]
    pub failure: Option<Failure>,
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub plaintext: bool,
    #[serde(default)]
    pub has_session_cookie: bool,
    #[serde(default)]
    pub log_path: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub ready: bool,
    /// The resolved spawn command line (token-free), for the diagnostics panel.
    #[serde(default)]
    pub command_line: Option<String>,
    /// Identifies "the same instance" (workspace + DSH_HOME). Two tabs sharing it
    /// share a webview storage, which is what keeps a login across restarts.
    #[serde(default)]
    pub instance_key: String,
}

/// In-memory-only state for one tab.
pub struct TabRuntime {
    pub spec: TabSpec,
    pub phase: String,
    pub handle: Option<crate::dsh::process::ProcessHandle>,
    /// The full launch URL **with token**. Memory only, by construction.
    pub launch_url: Option<crate::dsh::url::LaunchUrl>,
    pub log_tail: Vec<LogEntry>,
    pub failure: Option<Failure>,
    pub notice: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<std::time::Instant>,
    pub last_title: Option<String>,
    pub has_session_cookie: bool,
    /// The resolved spawn command line, for the diagnostics panel.
    pub command_line: Option<String>,
    /// Guards against acting on stale async results after a restart/close.
    pub generation: u64,
    pub log_path: Option<PathBuf>,
}

impl TabRuntime {
    pub fn new(spec: TabSpec) -> Self {
        Self {
            spec,
            phase: phase::IDLE.to_string(),
            handle: None,
            launch_url: None,
            log_tail: Vec::new(),
            failure: None,
            notice: None,
            exit_code: None,
            started_at: None,
            last_title: None,
            has_session_cookie: false,
            command_line: None,
            generation: 0,
            log_path: None,
        }
    }

    pub fn push_log(&mut self, entry: LogEntry) {
        if self.log_tail.len() >= crate::dsh::process::MAX_LOG_LINES {
            self.log_tail.remove(0);
        }
        self.log_tail.push(entry);
    }

    pub fn uptime_ms(&self) -> Option<u64> {
        self.started_at.map(|at| at.elapsed().as_millis() as u64)
    }
}
