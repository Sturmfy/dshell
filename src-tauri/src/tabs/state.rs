//! Tab descriptors and runtime state.
//!
//! `TabSpec` is what gets persisted; `TabRuntime` never leaves memory. The split
//! exists so that a launch token cannot possibly reach `tabs.json`.
//!
//! **Every page in the window is a tab** — including dshell's own pages. The
//! first-run wizard, the new-connection chooser and the settings page are not a
//! special panel; they are tabs whose content happens to be a dshell asset
//! instead of a DSH page. That is why [`ConnectionKind`] carries the page
//! variants: to the window, a page *is* a tab, and anything else would mean a
//! second layout region and a second set of rules for focus, shortcuts and
//! closing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::DshHomeSetting;
use crate::dsh::process::LogEntry;
use crate::error::Failure;
use crate::runtime::RuntimeSpec;

/// dshell's own pages, rendered as tabs.
///
/// The serde names are pinned to [`PageKind::slug`] deliberately, instead of
/// being derived by a rename rule: the serialized variant doubles as the page's
/// addressable name (webview label, storage directory, `open_page` argument), so
/// a rule that rewrites one of them without the other produces a page tab that
/// cannot be loaded again. The unit test below asserts the two agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    #[serde(rename = "onboarding")]
    Onboarding,
    #[serde(rename = "new")]
    NewTab,
    #[serde(rename = "settings")]
    Settings,
}

impl PageKind {
    /// The stable short name of a page: the asset basename, the webview label
    /// suffix, the storage-directory suffix and the `open_page` argument.
    ///
    /// Single words on purpose. serde writes this enum as camelCase (`newTab`),
    /// and a two-word slug would make the serialized form and the addressable
    /// name disagree — which is exactly how a page tab ends up unparseable
    /// after a restart.
    pub fn slug(self) -> &'static str {
        match self {
            PageKind::Onboarding => "onboarding",
            PageKind::NewTab => "new",
            PageKind::Settings => "settings",
        }
    }

    /// The document dshell serves for this page.
    pub fn asset(self) -> String {
        format!("pages/{}.html", self.slug())
    }

    pub fn title(self) -> &'static str {
        match self {
            PageKind::Onboarding => "首次配置",
            PageKind::NewTab => "新建连接",
            PageKind::Settings => "设置",
        }
    }

    /// Whether the user may close this page.
    ///
    /// The wizard is the one page that refuses before it is finished: dshell has
    /// no data root, no runtimes and no working defaults until it returns, so a
    /// stray Ctrl+W must not be able to leave the app unusable. Once onboarding
    /// is done it is an ordinary closable tab again — including when it is
    /// reopened from settings to look at it a second time.
    ///
    /// The rule is a method on the kind, taking the one fact it depends on, so
    /// the strip (`TabView::closable`) and the authority (`close_tab`) can never
    /// disagree: the strip would offer a close button that Rust refuses, or hide
    /// one that would have worked.
    pub fn closable(self, onboarding_done: bool) -> bool {
        !matches!(self, PageKind::Onboarding) || onboarding_done
    }

    /// Accepts the slug as well as the spellings an author might guess, so a
    /// hand-written `tabs.json` is forgiving without ever resolving to the wrong
    /// page.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "onboarding" => Some(PageKind::Onboarding),
            "new" | "newTab" | "newtab" => Some(PageKind::NewTab),
            "settings" => Some(PageKind::Settings),
            _ => None,
        }
    }
}

/// `TabView::closable` defaults to `true`: only an unfinished wizard says no.
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSpec {
    pub id: String,
    pub title: String,
    /// The field name is kept as `connection` so a `tabs.json` written by an
    /// earlier build still parses — tabs are the user's session, and losing them
    /// to a rename would be a silly way to break the app.
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
    /// One of dshell's own pages, shown as a tab. Serialized `page` with a
    /// `kind` field, so a hand-written entry stays readable.
    Page { kind: PageKind },
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
            ConnectionKind::Page { .. } => "dshell",
            ConnectionKind::LocalSpawn { .. } => "local",
            ConnectionKind::LocalAttach { .. } => "attach",
            ConnectionKind::Remote { .. } => "remote",
        }
    }

    /// dshell's own page, or `None` for a tab that carries a DSH instance.
    pub fn page(&self) -> Option<PageKind> {
        match self {
            ConnectionKind::Page { kind } => Some(*kind),
            _ => None,
        }
    }

    pub fn is_page(&self) -> bool {
        self.page().is_some()
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
    /// `onboarding` / `new` / `settings` for a dshell page tab, else `None` —
    /// i.e. [`PageKind::slug`], which is what the pages themselves are addressed
    /// by. The strip uses it to pick icons and to hide actions a page has no use
    /// for.
    #[serde(default)]
    pub page_kind: Option<String>,
    /// `false` only for the first-run wizard while it is unfinished.
    #[serde(default = "default_true")]
    pub closable: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn page_spec(kind: PageKind) -> TabSpec {
        TabSpec {
            id: "p1".into(),
            title: kind.title().into(),
            connection: ConnectionKind::Page { kind },
            display_origin: None,
        }
    }

    #[test]
    fn page_kinds_have_distinct_labels_and_assets() {
        let kinds = [PageKind::Onboarding, PageKind::NewTab, PageKind::Settings];
        let mut slugs: Vec<&str> = kinds.iter().map(|kind| kind.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), kinds.len(), "webview labels must not collide");

        for kind in kinds {
            assert_eq!(kind.asset(), format!("pages/{}.html", kind.slug()));
            assert!(!kind.title().is_empty());
        }
    }

    /// The webview label is a security boundary, not a label: a page tab and a
    /// DSH tab must never be able to share one, or the capability glob would
    /// grant IPC to a remote page.
    #[test]
    fn page_labels_never_look_like_content_labels() {
        let page = crate::tabs::manager::TabManager::page_webview_label(PageKind::Settings);
        let content = crate::tabs::manager::TabManager::webview_label("abc123");
        assert_eq!(page, "page-settings");
        assert_eq!(content, "tab-abc123");
        assert!(!content.starts_with(crate::tabs::manager::PAGE_LABEL_PREFIX));
    }

    /// Only the wizard is ever unclosable, and only while onboarding is
    /// unfinished. If this ever flips for `Settings` the gear becomes a trap; if
    /// it flips for `Onboarding` a stray Ctrl+W can leave dshell with no data
    /// root; and if it stops depending on `onboarding_done`, the strip hides a
    /// close button that Rust would have honoured.
    #[test]
    fn only_the_unfinished_wizard_refuses_to_close() {
        assert!(!PageKind::Onboarding.closable(false));
        // Finished onboarding: the wizard is an ordinary tab, even when it was
        // reopened from settings to look at it again.
        assert!(PageKind::Onboarding.closable(true));
        assert!(PageKind::NewTab.closable(false));
        assert!(PageKind::NewTab.closable(true));
        assert!(PageKind::Settings.closable(false));
        assert!(PageKind::Settings.closable(true));
    }

    /// The slug is the same string serde writes for the variant, so a page tab
    /// survives a save/load cycle *and* can be spelled out by hand.
    #[test]
    fn page_slug_matches_the_serialized_variant() {
        for kind in [PageKind::Onboarding, PageKind::NewTab, PageKind::Settings] {
            let variant = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                variant.trim_matches('"'),
                kind.slug(),
                "serde spelling and slug must agree or a persisted page tab cannot be re-parsed"
            );
            assert_eq!(PageKind::parse(variant.trim_matches('"')), Some(kind));
        }
    }

    #[test]
    fn page_specs_round_trip_and_stay_identifiable() {
        for kind in [PageKind::Onboarding, PageKind::NewTab, PageKind::Settings] {
            let json = serde_json::to_string(&page_spec(kind)).unwrap();
            let back: TabSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(back.connection.page(), Some(kind));
            assert!(back.connection.is_page());
            assert!(!back.connection.is_plaintext_remote());
            assert_eq!(back.connection.workspace(), None);

            // Every page deserializes from the spelled-out wire form the strip
            // and any hand-written `tabs.json` would use.
            let wire = format!(
                r#"{{"id":"p1","title":"t","connection":{{"type":"page","kind":"{}"}}}}"#,
                kind.slug()
            );
            let parsed: TabSpec = serde_json::from_str(&wire).unwrap();
            assert_eq!(parsed.connection.page(), Some(kind));
        }
    }

    /// A `tabs.json` from the previous build must still parse: losing the user's
    /// tabs to a refactor would be a poor trade for a tidier type name.
    #[test]
    fn older_tabs_json_still_parses() {
        // Exactly what the previous build wrote, snake_case field names included.
        let old = r#"{"id":"a","title":"local","connection":{"type":"localSpawn",
            "workspace":"E:\\ws",
            "dshHome":{"mode":"system"},
            "runtime":{"kind":"npx","package_spec":"@deepseek-ai/dsh@0.2.0-rc.2"},
            "killOnClose":false}}"#;
        let parsed: TabSpec = serde_json::from_str(old).expect("old tabs.json must parse");
        assert!(!parsed.connection.is_page());
        assert_eq!(parsed.connection.badge(), "local");
        assert_eq!(parsed.connection.workspace(), Some("E:\\ws"));
    }

    #[test]
    fn page_kind_parses_only_known_slugs() {
        for kind in [PageKind::Onboarding, PageKind::NewTab, PageKind::Settings] {
            assert_eq!(PageKind::parse(kind.slug()), Some(kind));
        }
        // Tolerated spellings for a hand-written file, but never a wrong page.
        assert_eq!(PageKind::parse("newTab"), Some(PageKind::NewTab));
        assert_eq!(PageKind::parse("newtab"), Some(PageKind::NewTab));
        assert_eq!(PageKind::parse("panel"), None);
        assert_eq!(PageKind::parse(""), None);
    }

    /// `TabView::closable` defaults to true so an older payload (or a hand-built
    /// one) can never accidentally pin every tab.
    #[test]
    fn tab_view_defaults_to_closable() {
        let view: TabView = serde_json::from_str(
            r#"{"id":"a","title":"t","badge":"local","phase":"idle"}"#,
        )
        .unwrap();
        assert!(view.closable);
        assert_eq!(view.page_kind, None);
    }
}
