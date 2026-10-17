//! Failure taxonomy.
//!
//! Every entry in the design's failure table maps to a `FailureCode` here, and
//! every failure carries three things the UI is required to show: a readable
//! reason, the raw evidence, and the actions that can be taken next.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureCode {
    NoNodeRuntime,
    WorkspaceMissing,
    DshHomeInvalid,
    DshHomeUnwritable,
    FrontendMissing,
    NpxInstallFailed,
    /// `npm install -g pnpm` (the button on the environment report) failed.
    PnpmInstallFailed,
    BuildFailed,
    DshFlagUnsupported,
    StartupTimeout,
    PortBindFailed,
    TokenUrlUnparsed,
    AttachAuthRequired,
    AttachNotDsh,
    RemoteUnreachable,
    RemoteForbidden,
    ChildExited,
    DataRootUnwritable,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub code: FailureCode,
    /// One-line, human-readable cause.
    pub message: String,
    /// Raw evidence: stderr tail, status line, path, etc.
    #[serde(default)]
    pub detail: Option<String>,
    /// Path to DSH's own startup diagnostics log, when it printed one.
    #[serde(default)]
    pub log_path: Option<String>,
    /// Suggested next steps, rendered as buttons by the error page.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Where the failure happened, for pinning the blame in the UI.
    #[serde(default)]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: ActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    #[default]
    Primary,
    Secondary,
}

impl Action {
    pub fn primary(id: &str, label: &str) -> Self {
        Self { id: id.into(), label: label.into(), kind: ActionKind::Primary }
    }
    pub fn secondary(id: &str, label: &str) -> Self {
        Self { id: id.into(), label: label.into(), kind: ActionKind::Secondary }
    }
}

impl Failure {
    pub fn new(code: FailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            log_path: None,
            actions: Vec::new(),
            stage: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let text = detail.into();
        if !text.trim().is_empty() {
            self.detail = Some(text);
        }
        self
    }

    pub fn with_log(mut self, path: impl Into<String>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_stage(mut self, stage: impl Into<String>) -> Self {
        self.stage = Some(stage.into());
        self
    }

    /// Attach the default escape hatches every failure gets.
    pub fn with_defaults(mut self) -> Self {
        if !self.actions.iter().any(|a| a.id == "retry") {
            self.actions.push(Action::primary("retry", "重试"));
        }
        if self.log_path.is_some() && !self.actions.iter().any(|a| a.id == "open-log") {
            self.actions.push(Action::secondary("open-log", "打开完整日志"));
        }
        if !self.actions.iter().any(|a| a.id == "open-browser") {
            self.actions.push(Action::secondary("open-browser", "在系统浏览器中打开"));
        }
        if !self.actions.iter().any(|a| a.id == "diagnostics") {
            self.actions.push(Action::secondary("diagnostics", "查看诊断信息"));
        }
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for Failure {}
