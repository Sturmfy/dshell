//! The runtime abstraction: four ways to start a DSH backend, one interface.
//!
//! npx / npm-installed / source checkout / arbitrary binary all resolve to the
//! same `LaunchPlan`, so the supervisor, stdout parser, readiness probe and
//! error classifier are written exactly once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Action, Failure, FailureCode};
use crate::runtime::node::NodeInfo;

pub mod exec;
pub mod help;
pub mod node;
pub mod npxcache;
pub mod port;
pub mod source;

pub const DEFAULT_PORT_TIMEOUT: Duration = Duration::from_secs(90);
/// First-ever npx resolution downloads ~450 MB (26k files); the design measured
/// ~3 minutes on a warm network, so the cold-start budget is generous.
pub const COLD_START_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RuntimeSpec {
    /// `npx --yes @deepseek-ai/dsh@<pin> web …` — zero install, big first run.
    Npx { package_spec: String },
    /// `npm install --prefix <dir> @deepseek-ai/dsh@<pin>`, then the shim.
    Npm { root: String, package_spec: String },
    /// A tree the user already maintains: npm/pnpm install or git checkout.
    Managed { root: String, mode: ManagedMode },
    /// Any executable that can serve `web`; verified before adoption.
    External { binary: String, label: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedMode {
    Installed,
    Source,
}

impl RuntimeSpec {
    pub fn short_label(&self) -> String {
        match self {
            RuntimeSpec::Npx { .. } => "npx".into(),
            RuntimeSpec::Npm { .. } => "npm".into(),
            RuntimeSpec::Managed { mode: ManagedMode::Installed, .. } => "managed/npm".into(),
            RuntimeSpec::Managed { mode: ManagedMode::Source, .. } => "managed/source".into(),
            RuntimeSpec::External { label, binary, .. } => {
                label.clone().unwrap_or_else(|| {
                    Path::new(binary)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "binary".into())
                })
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub no_open: bool,
    pub port: Option<u16>,
    /// `--host` / `--trusted-host` are only offered for the "expose for remote
    /// access" case; dshell never generates `0.0.0.0` (DSH hard-rejects it).
    pub host: Option<String>,
    pub trusted_hosts: Vec<String>,
}

/// Everything the supervisor needs to start a backend, and nothing about DSH.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Extra environment on top of the inherited one. Absent keys are inherited.
    pub env: Vec<(String, String)>,
    /// A user-visible caveat, e.g. "this backend ignores DSH_HOME".
    pub notice: Option<String>,
}

impl LaunchPlan {
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            notice: None,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: &str, value: Option<String>) -> Self {
        if let Some(value) = value {
            self.env.push((key.to_string(), value));
        }
        self
    }

    pub fn command_line(&self) -> String {
        let mut out = format!("{}", self.program.display());
        for arg in &self.args {
            out.push(' ');
            if arg.contains(' ') {
                out.push('"');
                out.push_str(arg);
                out.push('"');
            } else {
                out.push_str(arg);
            }
        }
        out
    }
}

/// Build the `web` argument list from the flags a given DSH version supports.
///
/// Flags that do not exist on the target version are *not* passed: 0.2.0-rc.2
/// exits 1 on `--public-url`, so guessing is fatal.
///
/// Every flag and its value are **separate argv entries**. A single `"--port 0"`
/// entry reaches commander as one unknown token, and DSH exits 1 with
/// `unknown option '--port 0'` — the command line only *looks* right in a log.
pub fn web_args(flags: &Flags, supported: &help::FlagSupport) -> Vec<String> {
    let mut args = vec!["web".to_string()];
    if flags.no_open || supported.no_open {
        args.push("--no-open".into());
    }
    match flags.port {
        Some(port) if supported.port => {
            args.push("--port".into());
            args.push(port.to_string());
        }
        None if supported.port => {
            args.push("--port".into());
            // Last resort only: `resolve_port` normally hands DSH a concrete,
            // legal port, so this arm is reached only when the picker found
            // nothing free. 0 lets the OS pick, and the readiness guard reports
            // a forbidden result instead of leaving a blank tab behind.
            args.push("0".into());
        }
        _ => {}
    }
    if let Some(host) = &flags.host {
        if supported.host {
            args.push("--host".into());
            args.push(host.clone());
        }
    }
    for trusted in &flags.trusted_hosts {
        if supported.trusted_host {
            args.push("--trusted-host".into());
            args.push(trusted.clone());
        }
    }
    args
}

/// Decide the port this launch asks DSH for.
///
/// A launch never asks for "any port": the OS answers from the dynamic range,
/// which includes ports Chromium refuses to connect to at all (6666 was the
/// observed one), and the tab then dies with `ERR_UNSAFE_PORT` while the backend
/// behind it is perfectly healthy. dshell picks a legal free port itself — see
/// [`port`] — and keeps a port the caller asked for unless no browser would
/// accept it.
fn resolve_port(flags: &Flags, supported: &help::FlagSupport) -> Flags {
    let mut resolved = flags.clone();
    if !supported.port {
        return resolved;
    }
    let usable = resolved
        .port
        .filter(|requested| *requested != 0 && !port::is_browser_restricted_port(*requested));
    resolved.port = usable.or_else(port::pick_listen_port);
    resolved
}

/// Resolve a runtime spec into a concrete launch plan.
pub async fn resolve(
    spec: &RuntimeSpec,
    node: Option<&NodeInfo>,
    data_root: &Path,
    cwd: &Path,
    flags: &Flags,
) -> Result<LaunchPlan, Failure> {
    let supported = probe_flags(spec, node, data_root).await;
    let flags = resolve_port(flags, &supported);
    let args = web_args(&flags, &supported);

    match spec {
        RuntimeSpec::Npx { package_spec } => {
            let node = require_node(node)?;
            let npx = node.npx.clone().ok_or_else(|| {
                Failure::new(
                    FailureCode::NoNodeRuntime,
                    "找到了 Node，但同一安装目录下没有 npx",
                )
                .with_detail(format!("node: {}", node.node.display()))
            })?;
            let cache = data_root.join("runtimes").join("npx-cache");
            let mut plan = LaunchPlan::new(npx, cwd)
                .args(["--yes".to_string(), package_spec.clone()])
                .args(args)
                .env("npm_config_cache", Some(cache.to_string_lossy().to_string()))
                .env("npm_config_update_notifier", Some("false".into()));
            plan.notice = Some(format!(
                "首次运行需要下载 {package_spec}（约 450 MB / 26,000 个文件），可能耗时数分钟"
            ));
            Ok(plan)
        }
        RuntimeSpec::Npm { root, package_spec: _ } => {
            let root = PathBuf::from(root);
            let entry = npm_entry(&root).ok_or_else(|| {
                Failure::new(
                    FailureCode::NpxInstallFailed,
                    "本地 npm 安装不完整",
                )
                .with_detail(format!("未找到 {}", root.join(NPM_ENTRY).display()))
                .with_action(Action::primary("install-npm-runtime", "现在安装"))
            })?;
            Ok(LaunchPlan::new(entry, cwd).args(args).env(
                "npm_config_cache",
                Some(data_root.join("runtimes").join("npx-cache").to_string_lossy().to_string()),
            ))
        }
        RuntimeSpec::Managed { root, mode } => {
            let root = PathBuf::from(root);
            match mode {
                ManagedMode::Installed => {
                    let entry = npm_entry(&root).ok_or_else(|| {
                        Failure::new(FailureCode::NpxInstallFailed, "该目录不是已安装的 DSH")
                            .with_detail(format!(
                                "期望存在 {} 或 {}",
                                root.join(NPM_ENTRY).display(),
                                root.join(SOURCE_ENTRY).display()
                            ))
                    })?;
                    Ok(LaunchPlan::new(entry, cwd).args(args))
                }
                ManagedMode::Source => {
                    let node = require_node(node)?;
                    let entry = root.join(SOURCE_ENTRY);
                    if !entry.is_file() {
                        return Err(Failure::new(
                            FailureCode::BuildFailed,
                            "源码目录里没有找到 DSH CLI 入口，请先构建",
                        )
                        .with_detail(format!("期望存在 {}", entry.display()))
                        .with_action(Action::primary("build-source-runtime", "构建")));
                    }
                    // A source launch is `node --import <tsx loader> <entry> web …`,
                    // exactly what the repository's own `pnpm dsh` runs. Three
                    // details make it work from a *foreign* cwd, which is the
                    // normal case here because DSH treats its cwd as the session
                    // workspace and dshell starts it in the user's directory:
                    //
                    // * the entry is passed absolute, so cwd cannot change what runs;
                    // * the loader is passed as a `file://` URL — `node --import`
                    //   parses a bare `E:\…` as a URL with scheme `e:` and refuses it;
                    // * `TSX_TSCONFIG_PATH` names the checkout's tsconfig, because
                    //   tsx applies its `paths` mapping relative to the tsconfig it
                    //   finds, and DSH's sources need that mapping to resolve
                    //   `@deepseek-ai/*` to each workspace's `src`.
                    let entry = std::path::absolute(&entry).unwrap_or(entry);
                    let mut plan = LaunchPlan::new(node.node.clone(), cwd)
                        .args(source_prefix(&root))
                        .args([entry.to_string_lossy().to_string()])
                        .args(args)
                        .env("NODE_OPTIONS", None);
                    if root.join("tsconfig.json").is_file() {
                        plan = plan.env(
                            "TSX_TSCONFIG_PATH",
                            Some(root.join("tsconfig.json").to_string_lossy().to_string()),
                        );
                    }
                    Ok(plan)
                }
            }
        }
        RuntimeSpec::External { binary, label: _ } => {
            let binary = PathBuf::from(binary);
            if !binary.is_file() {
                return Err(Failure::new(
                    FailureCode::NoNodeRuntime,
                    "指定的后端二进制不存在",
                )
                .with_detail(binary.display().to_string()));
            }
            // No Node requirement at all: the whole point of this path.
            Ok(LaunchPlan::new(binary, cwd).args(args))
        }
    }
}

pub fn require_node(node: Option<&NodeInfo>) -> Result<&NodeInfo, Failure> {
    let node = node.ok_or_else(node::missing_failure)?;    if let Some(issue) = &node.version_issue {
        return Err(Failure::new(FailureCode::NoNodeRuntime, issue.clone())
            .with_detail(format!("node: {}", node.node.display()))
            .with_action(Action::primary("open-nodejs-org", "下载 Node.js（官方）")));
    }
    Ok(node)
}

pub const NPM_ENTRY: &str = "node_modules/.bin/dsh.cmd";
pub const NPM_ENTRY_ALT: &str = "node_modules/@deepseek-ai/dsh/lib/bin.js";
pub const SOURCE_ENTRY: &str = "apps/cli/src/bin.ts";

/// The `--import` prefix a source launch needs, empty when tsx is not installed.
///
/// `node --import` resolves its argument as a URL specifier, so a Windows
/// absolute path arrives as scheme `e:` and is rejected (`ERR_UNSUPPORTED_ESM_URL_SCHEME`).
/// A `file://` URL is the spelling that works from any cwd.
pub fn source_prefix(root: &Path) -> Vec<String> {
    let tsx = root
        .join("node_modules")
        .join("tsx")
        .join("dist")
        .join("esm")
        .join("index.mjs");
    if !tsx.is_file() {
        return Vec::new();
    }
    vec!["--import".to_string(), file_url(&tsx)]
}

fn file_url(path: &Path) -> String {
    let text = std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// The executable entry of an installed (npm/pnpm) tree, if any.
fn npm_entry(root: &Path) -> Option<PathBuf> {
    let shim = root.join(NPM_ENTRY);
    if shim.is_file() {
        return Some(shim);
    }
    let js = root.join(NPM_ENTRY_ALT);
    if js.is_file() {
        return Some(js);
    }
    let bin = root.join("node_modules").join(".bin").join("dsh");
    if bin.is_file() {
        return Some(bin);
    }
    None
}

/// Detect which flavour a user-supplied directory is.
pub fn detect_managed_mode(root: &Path) -> Option<ManagedMode> {
    if root.join("pnpm-workspace.yaml").is_file() && root.join("packages").is_dir() {
        return Some(ManagedMode::Source);
    }
    if npm_entry(root).is_some() || root.join("node_modules/@deepseek-ai/dsh").is_dir() {
        return Some(ManagedMode::Installed);
    }
    if root.join("package.json").is_file() && root.join("apps").is_dir() {
        return Some(ManagedMode::Source);
    }
    None
}

/// Probe `web --help` once per process per runtime.
///
/// This is what keeps dshell working across DSH releases: flags are discovered,
/// never assumed.
pub async fn probe_flags(
    spec: &RuntimeSpec,
    node: Option<&NodeInfo>,
    data_root: &Path,
) -> help::FlagSupport {
    let cache_key = format!(
        "{}|{}",
        spec.short_label(),
        match spec {
            RuntimeSpec::Npx { package_spec } => package_spec.clone(),
            RuntimeSpec::Npm { root, .. } => root.clone(),
            RuntimeSpec::Managed { root, .. } => root.clone(),
            RuntimeSpec::External { binary, .. } => binary.clone(),
        }
    );
    if let Some(hit) = help::cache_get(&cache_key) {
        return hit;
    }

    // Resolve the program without recursion: pass a permissive flag set and ask
    // the spec for its bare program.
    let program = match bare_program(spec, node, data_root) {
        Some(program) => program,
        None => return help::FlagSupport::assumed(),
    };

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(bare_prefix(spec));
    cmd.arg("web").arg("--help");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    if let RuntimeSpec::Npx { .. } = spec {
        let cache = data_root.join("runtimes").join("npx-cache");
        cmd.env("npm_config_cache", cache);
    }
    // The source probe must be run the way the launch is, or it would fail on
    // the tsconfig `paths` mapping and silently fall back to assumed flags.
    if let RuntimeSpec::Managed { root, mode: ManagedMode::Source } = spec {
        let root = PathBuf::from(root);
        if root.join("tsconfig.json").is_file() {
            cmd.env(
                "TSX_TSCONFIG_PATH",
                root.join("tsconfig.json").to_string_lossy().to_string(),
            );
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    crate::dsh::process::debug_log(
        data_root,
        &format!("capability probe: {}", {
            let mut text = program.display().to_string();
            for arg in bare_prefix(spec).into_iter().chain(["web".into(), "--help".into()]) {
                text.push(' ');
                text.push_str(&arg);
            }
            text
        }),
    );

    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(120), async {
        let child = cmd.spawn()?;
        child.wait_with_output().await
    })
    .await
    else {
        return help::FlagSupport::assumed();
    };

    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let flags = help::parse(&text);
    help::cache_put(cache_key, flags);
    flags
}

fn bare_program(spec: &RuntimeSpec, node: Option<&NodeInfo>, data_root: &Path) -> Option<PathBuf> {
    let _ = data_root;
    match spec {
        RuntimeSpec::Npx { .. } => node?.npx.clone(),
        RuntimeSpec::Npm { root, .. } => npm_entry(Path::new(root)),
        RuntimeSpec::Managed { root, mode } => match mode {
            ManagedMode::Installed => npm_entry(Path::new(root)),
            ManagedMode::Source => node.map(|n| n.node.clone()),
        },
        RuntimeSpec::External { binary, .. } => Some(PathBuf::from(binary)),
    }
}

fn bare_prefix(spec: &RuntimeSpec) -> Vec<String> {
    match spec {
        RuntimeSpec::Npx { package_spec } => vec!["--yes".into(), package_spec.clone()],
        RuntimeSpec::Managed { root, mode: ManagedMode::Source } => {
            let root = Path::new(root);
            let entry = root.join(SOURCE_ENTRY);
            let mut prefix = source_prefix(root);
            prefix.push(entry.to_string_lossy().to_string());
            prefix
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_flags_are_dropped() {
        let flags = Flags { no_open: true, port: None, ..Default::default() };
        let supported = help::FlagSupport { no_open: true, port: false, ..Default::default() };
        assert_eq!(web_args(&flags, &supported), vec!["web", "--no-open"]);
    }

    #[test]
    fn port_zero_is_used_when_supported() {
        let flags = Flags { no_open: true, port: None, ..Default::default() };
        let supported = help::FlagSupport::assumed();
        assert_eq!(
            web_args(&flags, &supported),
            vec!["web", "--no-open", "--port", "0"]
        );
    }

    /// Regression: `"--port 0"` as one argv entry makes DSH exit 1 with
    /// `unknown option '--port 0'`.
    #[test]
    fn flag_values_are_separate_argv_entries() {
        let flags = Flags {
            no_open: true,
            port: Some(9808),
            host: Some("127.0.0.1".into()),
            trusted_hosts: vec!["127.0.0.1:9808".into()],
        };
        assert_eq!(
            web_args(&flags, &help::FlagSupport::assumed()),
            vec![
                "web",
                "--no-open",
                "--port",
                "9808",
                "--host",
                "127.0.0.1",
                "--trusted-host",
                "127.0.0.1:9808"
            ]
        );
    }

    /// The regression this fix exists for: a launch must never leave the port
    /// choice to the OS. The dynamic range contains ports Chromium refuses
    /// outright, and 6666 — handed to DSH on 2026-10-17 — put a healthy backend
    /// behind a tab that could only show `ERR_UNSAFE_PORT`.
    #[test]
    fn a_launch_asks_for_a_concrete_browser_safe_port() {
        let flags = Flags { no_open: true, port: None, ..Default::default() };
        let supported = help::FlagSupport::assumed();
        let resolved = resolve_port(&flags, &supported);
        let picked = resolved.port.expect("a picked port");
        assert!((port::PICK_START..=port::PICK_END).contains(&picked), "{picked}");
        assert!(!port::is_browser_restricted_port(picked));
        assert_eq!(
            web_args(&resolved, &supported),
            vec![
                "web".to_string(),
                "--no-open".to_string(),
                "--port".to_string(),
                picked.to_string()
            ]
        );
    }

    #[test]
    fn a_requested_port_is_kept_unless_the_browser_would_refuse_it() {
        let supported = help::FlagSupport::assumed();
        let kept = resolve_port(&Flags { port: Some(9808), ..Default::default() }, &supported);
        assert_eq!(kept.port, Some(9808));

        let replaced = resolve_port(&Flags { port: Some(6666), ..Default::default() }, &supported);
        let picked = replaced.port.expect("a picked port");
        assert_ne!(picked, 6666);
        assert!(!port::is_browser_restricted_port(picked));
    }

    /// A DSH too old for `--port` keeps its own port: inventing a flag it does
    /// not know would turn a working launch into `unknown option`.
    #[test]
    fn a_backend_without_port_support_is_asked_for_no_port() {
        let supported =
            help::FlagSupport { no_open: true, port: false, ..Default::default() };
        let resolved = resolve_port(&Flags { port: Some(6666), ..Default::default() }, &supported);
        assert_eq!(web_args(&resolved, &supported), vec!["web", "--no-open"]);
    }

    #[test]
    fn managed_installed_tree_is_detected() {
        let dir = std::env::temp_dir().join(format!("dshell-detect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("node_modules/@deepseek-ai/dsh"));
        assert_eq!(detect_managed_mode(&dir), Some(ManagedMode::Installed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: `node --import E:\…\index.mjs` fails with
    /// `ERR_UNSUPPORTED_ESM_URL_SCHEME` ("Received protocol 'e:'"), because a
    /// Windows path is parsed as a URL. The loader must be a `file://` URL.
    #[test]
    fn the_tsx_loader_is_passed_as_a_file_url() {
        let url = file_url(Path::new(
            r"E:\projects\dshell\runtimes\source\node_modules\tsx\dist\esm\index.mjs",
        ));
        assert!(url.starts_with("file:///E:/"), "{url}");
        assert!(!url.contains('\\'), "{url}");
        assert!(url.ends_with("/node_modules/tsx/dist/esm/index.mjs"), "{url}");
    }

    #[test]
    fn source_prefix_is_empty_when_tsx_is_not_installed() {
        let dir = std::env::temp_dir().join(format!("dshell-tsx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(source_prefix(&dir).is_empty());

        let tsx = dir.join("node_modules").join("tsx").join("dist").join("esm");
        std::fs::create_dir_all(&tsx).unwrap();
        std::fs::write(tsx.join("index.mjs"), "// loader").unwrap();
        let prefix = source_prefix(&dir);
        assert_eq!(prefix.first().map(String::as_str), Some("--import"));
        assert!(prefix[1].starts_with("file:///"), "{prefix:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
