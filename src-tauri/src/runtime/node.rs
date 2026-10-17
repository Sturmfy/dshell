//! Node.js detection.
//!
//! Detection never trusts `PATH` alone: after a user installs Node the *already
//! running* dshell process still has the old `PATH`, so "re-detect" would lie.
//! We probe known install locations first, then enumerate `PATH` ourselves.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Action, Failure, FailureCode};

/// DSH's `engines` requirement.
pub const MIN_NODE_22: (u32, u32) = (22, 19);
pub const MIN_NODE_24: u32 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub node: PathBuf,
    pub npx: Option<PathBuf>,
    pub npm: Option<PathBuf>,
    pub version: String,
    pub source: NodeSource,
    /// Empty when the version satisfies DSH's `engines`.
    #[serde(default)]
    pub version_issue: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeSource {
    /// `<data root>/runtimes/node/` — dshell-managed portable Node.
    Portable,
    /// A versioned install directory (Program Files, LOCALAPPDATA, …).
    Installed,
    /// Found by walking `PATH`.
    Path,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub node: Option<NodeInfo>,
    pub webview2_version: Option<String>,
    pub pnpm: Option<String>,
    /// Whether dshell can install pnpm itself, and how. See [`PnpmInstall`].
    #[serde(default)]
    pub pnpm_install: PnpmInstall,
    /// `writable` for each DSH_HOME candidate the user selected, etc.
    pub notes: Vec<String>,
}

/// How dshell could make `pnpm` available itself.
///
/// dshell never runs `pnpm`: a source checkout needs it for the user's own
/// `pnpm install` / `pnpm run build`, and that build orchestration is
/// deliberately out of scope. But reporting "pnpm 未找到" and stopping there is a
/// dead end when npm is sitting right next to it — `npm install -g pnpm` is the
/// exact command the user would type, so the report carries it and the UI can
/// offer it as a button.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PnpmInstall {
    /// The `npm` dshell would run, when pnpm can be installed automatically.
    pub npm: Option<String>,
    /// Its `--version`, as evidence of what would be used.
    pub npm_version: Option<String>,
    /// The exact command, quoted verbatim by the UI so the button cannot
    /// promise something different from what runs.
    pub command: Option<String>,
    /// Why the button is not offered, when it is not.
    pub blocked: Option<String>,
}

impl PnpmInstall {
    pub fn npm_path(&self) -> Option<PathBuf> {
        self.npm.as_ref().map(PathBuf::from)
    }
}

/// Full environment detection. Runs off the UI thread.
pub async fn detect(data_root: &Path) -> EnvironmentSnapshot {
    let node = detect_node(data_root).await;
    // pnpm's shims can live somewhere this process never saw on `PATH`, so
    // detection asks with the Node it just found rather than `PATH` alone.
    let pnpm = detect_pnpm(node.as_ref()).await;
    let pnpm_install = plan_pnpm_install(node.as_ref(), pnpm.is_some()).await;
    EnvironmentSnapshot {
        node,
        webview2_version: webview2_version(),
        pnpm,
        pnpm_install,
        notes: Vec::new(),
    }
}

pub async fn detect_node(data_root: &Path) -> Option<NodeInfo> {
    for (candidate, source) in node_candidates(data_root) {
        let node = candidate;
        if !node.is_file() {
            continue;
        }
        let Some(version) = run_version(&node, &["--version"]).await else {
            continue;
        };
        if !version.starts_with('v') {
            continue;
        }
        let dir = node.parent().map(Path::to_path_buf).unwrap_or_default();
        let npx = shim(&dir, "npx");
        let npm = shim(&dir, "npm");
        let version_issue = if satisfies(&version) {
            None
        } else {
            Some(format!("检测到 Node {version}，DSH 需要 ^22.19.0 || >=24.0.0"))
        };
        return Some(NodeInfo { node, npx, npm, version, source, version_issue });
    }
    None
}

async fn detect_pnpm(node: Option<&NodeInfo>) -> Option<String> {
    for dir in pnpm_dirs(node) {
        for name in ["pnpm.cmd", "pnpm.exe", "pnpm"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                if let Some(v) = run_version(&candidate, &["--version"]).await {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Where a `pnpm` could be, in the order worth asking.
///
/// `PATH` is the user's own answer, so it comes first — but it is not enough on
/// its own. This process captured `PATH` at start-up, so a pnpm installed a
/// minute ago by the button on this page is on disk while still absent from it.
/// The global-shim directories a package manager writes into are therefore
/// probed explicitly, which is also what makes "re-detect" honest.
fn pnpm_dirs(node: Option<&NodeInfo>) -> Vec<PathBuf> {
    let mut dirs = path_dirs();
    if let Some(dir) = node.and_then(|info| info.node.parent()) {
        dirs.push(dir.to_path_buf());
    }
    for var in ["PNPM_HOME", "APPDATA", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            // npm's default global prefix (`%APPDATA%\npm` on Windows) and
            // pnpm's own default `PNPM_HOME`: both are where `install -g` drops
            // its shims.
            dirs.push(base.join("npm"));
            dirs.push(base.join("pnpm"));
        }
    }
    dirs
}

/// Ordered candidates: portable → versioned install dirs → `PATH`.
fn node_candidates(data_root: &Path) -> Vec<(PathBuf, NodeSource)> {
    let mut out: Vec<(PathBuf, NodeSource)> = Vec::new();

    // 1) dshell-managed portable Node (the "download a portable Node" escape).
    out.push((data_root.join("runtimes").join("node").join("node.exe"), NodeSource::Portable));

    // 2) Well-known install directories. Newest-first is not observable here, so
    //    a plain fixed order is enough — all of them are validated by --version.
    for root in known_node_dirs() {
        out.push((root.join("node.exe"), NodeSource::Installed));
    }

    // 3) PATH, enumerated by hand so a stale process environment is not the
    //    only source of truth (and so "re-detect" can pick up new locations).
    for dir in path_dirs() {
        out.push((dir.join("node.exe"), NodeSource::Path));
    }

    out
}

/// The install directories a Node.js is commonly found in.
fn known_node_dirs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            roots.push(base.join("nodejs"));
            roots.push(base.join("Programs").join("nodejs"));
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let base = PathBuf::from(profile);
        roots.push(base.join("scoop").join("apps").join("nodejs").join("current"));
        roots.push(base.join(".volta").join("bin"));
        roots.push(base.join("AppData").join("Roaming").join("nvm"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\nodejs"));
    roots
}

/// Locate an `npm` dshell could run.
///
/// The npm that belongs to the detected Node goes first: it is the one whose
/// global prefix that Node's own resolution uses. Then the well-known install
/// directories and `PATH`, in the same order [`node_candidates`] uses — so
/// "Node found, npm missing" cannot happen merely because Node lives in a
/// directory this process's `PATH` never mentioned.
pub fn find_npm(node: Option<&NodeInfo>) -> Option<PathBuf> {
    if let Some(npm) = node.and_then(|info| info.npm.as_ref()).filter(|npm| npm.is_file()) {
        return Some(npm.clone());
    }
    for dir in known_node_dirs().into_iter().chain(path_dirs()) {
        if let Some(npm) = shim(&dir, "npm") {
            return Some(npm);
        }
    }
    None
}

/// Decide whether dshell can offer "install pnpm", and with which npm.
pub async fn plan_pnpm_install(node: Option<&NodeInfo>, pnpm_present: bool) -> PnpmInstall {
    // Nothing to offer when pnpm already runs; the report has its version.
    if pnpm_present {
        return PnpmInstall::default();
    }
    let Some(npm) = find_npm(node) else {
        return PnpmInstall {
            blocked: Some("没有找到 npm：自动安装需要 Node.js 自带的 npm".to_string()),
            ..PnpmInstall::default()
        };
    };
    let npm_version = run_version(&npm, &["--version"]).await;
    PnpmInstall {
        command: Some(command_line(&npm, &PNPM_INSTALL_ARGS)),
        npm: Some(npm.to_string_lossy().to_string()),
        npm_version,
        blocked: None,
    }
}

/// The command every "install pnpm" runs, and the one the UI quotes back.
pub const PNPM_INSTALL_ARGS: [&str; 3] = ["install", "-g", "pnpm"];

const PNPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Run `npm install -g pnpm`, returning npm's own output.
///
/// This is the only place dshell writes outside its data root, and only because
/// the user pressed a button: a global npm install touches the user's npm
/// prefix. Nothing here runs at start-up.
pub async fn install_pnpm(npm: &Path) -> Result<String, Failure> {
    run_npm(npm, &PNPM_INSTALL_ARGS).await
}

/// The one place a package manager is spawned for pnpm purposes, so the install
/// and the live test cannot drift apart in how they run npm.
async fn run_npm(npm: &Path, args: &[&str]) -> Result<String, Failure> {
    let mut cmd = tokio::process::Command::new(npm);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    // The shims npm is about to write land in its own directory (or in the
    // prefix it was configured with); putting this npm's directory first on the
    // child's `PATH` is part of what makes them findable straight afterwards.
    if let Some(dir) = npm.parent() {
        if let Some(path) = path_with_first(dir) {
            cmd.env("PATH", path);
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let child = cmd.spawn().map_err(|err| {
        Failure::new(FailureCode::PnpmInstallFailed, format!("无法运行 npm：{err}"))
            .with_detail(npm.display().to_string())
    })?;
    let output = match tokio::time::timeout(PNPM_INSTALL_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(Failure::new(
                FailureCode::PnpmInstallFailed,
                format!("npm 运行中断：{err}"),
            )
            .with_detail(command_line(npm, args)))
        }
        Err(_) => {
            return Err(Failure::new(
                FailureCode::PnpmInstallFailed,
                format!("npm {} 超时（{} 秒）", args.join(" "), PNPM_INSTALL_TIMEOUT.as_secs()),
            )
            .with_detail(command_line(npm, args)))
        }
    };

    let text = combined_output(&output);
    if !output.status.success() {
        return Err(Failure::new(
            FailureCode::PnpmInstallFailed,
            format!("npm {} 失败（{}）", args.join(" "), status_text(output.status)),
        )
        .with_detail(output_tail(&text)));
    }
    Ok(text)
}

/// `npm config get prefix`: where npm would have written the pnpm shims.
///
/// Only asked on the unhappy path ("npm said it installed, pnpm still does not
/// run"), because it costs a process launch and answers a question nobody has on
/// a normal start.
pub async fn npm_global_prefix(npm: &Path) -> Option<PathBuf> {
    let text = run_text(npm, &["config", "get", "prefix"]).await?;
    text.lines()
        .map(str::trim)
        .find(|line| Path::new(line).is_absolute())
        .map(PathBuf::from)
}

/// `dir` first, then the inherited `PATH`.
fn path_with_first(dir: &Path) -> Option<std::ffi::OsString> {
    let mut parts: Vec<PathBuf> = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(&std::env::var_os("PATH")?));
    std::env::join_paths(parts).ok()
}

fn command_line(program: &Path, args: &[&str]) -> String {
    let mut out = program.display().to_string();
    for arg in args {
        out.push(' ');
        out.push_str(arg);
    }
    out
}

/// stdout and stderr in one string: npm picks a stream per message, and the
/// interesting part is whichever one it chose.
fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn status_text(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("退出码 {code}"),
        None => "被信号终止".to_string(),
    }
}

/// Cap a captured output at a readable length: the UI renders it in one notice.
fn tail_chars(text: &str, keep: usize) -> String {
    let trimmed = text.trim();
    let total = trimmed.chars().count();
    if total <= keep {
        return trimmed.to_string();
    }
    let tail: String = trimmed.chars().skip(total - keep).collect();
    format!("…{tail}")
}

/// npm's own words, plus the one hint its output does not spell out.
fn output_tail(text: &str) -> String {
    let tail = tail_chars(text, 1200);
    let lower = tail.to_lowercase();
    if lower.contains("eperm") || lower.contains("eacces") || lower.contains("permission denied") {
        format!(
            "{tail}\n\n提示：全局目录不可写。用管理员权限重跑 `npm install -g pnpm`，\
             或把 npm 的全局前缀改到用户目录（`npm config set prefix %APPDATA%\\npm`）。"
        )
    } else {
        tail
    }
}

fn path_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Some(path) = std::env::var_os("PATH") else {
        return dirs;
    };
    for entry in std::env::split_paths(&path) {
        if !entry.as_os_str().is_empty() {
            dirs.push(entry);
        }
    }
    dirs
}

fn shim(dir: &Path, name: &str) -> Option<PathBuf> {
    for ext in [".cmd", ".exe", ""] {
        let candidate = dir.join(format!("{name}{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `node --version` → `Some("v24.9.0")`, with a hard timeout so a broken
/// `node.exe` on `PATH` cannot hang the probe.
pub async fn run_version(program: &Path, args: &[&str]) -> Option<String> {
    let text = run_text(program, args).await?;
    let first = text.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

/// Same, but keeps every line — `web --help` output is the capability probe.
pub async fn run_text(program: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let child = cmd.spawn().ok()?;
    let output = match tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await
    {
        Ok(Ok(output)) => output,
        _ => return None,
    };
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `^22.19.0 || >=24.0.0`
pub fn satisfies(version: &str) -> bool {
    let Some((major, minor)) = parse_major_minor(version) else {
        return false;
    };
    if major == MIN_NODE_22.0 {
        return minor >= MIN_NODE_22.1;
    }
    major >= MIN_NODE_24
}

fn parse_major_minor(version: &str) -> Option<(u32, u32)> {
    let cleaned = version.trim().trim_start_matches('v');
    let mut parts = cleaned.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
    Some((major, minor))
}

/// Node is only needed for "launch local". Missing Node must never block
/// "connect", so this failure carries the three escape routes from the design.
pub fn missing_failure() -> Failure {
    Failure::new(
        FailureCode::NoNodeRuntime,
        "未检测到可用的 Node.js（启动本地需要它；连接功能不受影响）",
    )
    .with_detail(
        "已探测：便携 Node（<数据根>/runtimes/node）、常见安装目录、PATH。\n\
         也可以用「其他后端二进制」完全绕过 Node。",
    )
    .with_action(Action::primary("open-nodejs-org", "下载 Node.js（官方）"))
    .with_action(Action::secondary("use-external-binary", "使用其他后端二进制"))
    .with_action(Action::secondary("redetect", "重新检测"))
}

/// The WebView2 runtime is what makes the whole app possible. dshell does not
/// ship it, so on a machine without it there is no window to render an error in.
pub fn webview2_version() -> Option<String> {
    #[cfg(windows)]
    {
        use std::ffi::{c_void, OsStr};
        use std::os::windows::ffi::OsStrExt;

        const HKEY_LOCAL_MACHINE: usize = 0x8000_0002;
        const KEY_READ: u32 = 0x2_0019;
        const REG_SZ: u32 = 1;
        type Hkey = *mut c_void;

        extern "system" {
            fn RegOpenKeyExW(
                key: Hkey,
                subkey: *const u16,
                options: u32,
                desired: u32,
                result: *mut Hkey,
            ) -> i32;
            fn RegQueryValueExW(
                key: Hkey,
                name: *const u16,
                reserved: *mut u32,
                kind: *mut u32,
                data: *mut u8,
                len: *mut u32,
            ) -> i32;
            fn RegCloseKey(key: Hkey) -> i32;
        }

        fn wide(s: &str) -> Vec<u16> {
            OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
        }

        let subkey =
            wide(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}");
        let mut key: Hkey = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE as Hkey, subkey.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            return None;
        }
        let name = wide("pv");
        let mut kind: u32 = 0;
        let mut buf = vec![0u8; 512];
        let mut len = buf.len() as u32;
        let rc = unsafe {
            RegQueryValueExW(key, name.as_ptr(), std::ptr::null_mut(), &mut kind, buf.as_mut_ptr(), &mut len)
        };
        unsafe { RegCloseKey(key) };
        if rc != 0 || kind != REG_SZ {
            return None;
        }
        let wide_slice: Vec<u16> = buf[..len as usize]
            .as_chunks::<2>().0.iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|c| *c != 0)
            .collect();
        let text = String::from_utf16_lossy(&wide_slice).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_range_matches_dsh() {
        assert!(satisfies("v22.19.0"));
        assert!(satisfies("v22.20.1"));
        assert!(satisfies("v24.0.0"));
        assert!(satisfies("v26.1.0"));
        assert!(!satisfies("v22.18.9"));
        assert!(!satisfies("v20.11.0"));
        assert!(!satisfies("garbage"));
    }

    #[test]
    fn captured_output_is_capped() {
        assert_eq!(tail_chars("  hi  \n", 10), "hi");
        let long = "字".repeat(2000);
        let tail = tail_chars(&long, 100);
        // The ellipsis is one character of its own.
        assert_eq!(tail.chars().count(), 101);
        assert!(tail.starts_with('…'));
    }

    #[test]
    fn permission_failures_gain_a_hint() {
        let tail = output_tail("npm ERR! code EPERM\nnpm ERR! EPERM: operation not permitted");
        assert!(tail.contains("管理员权限"), "{tail}");
        // Unrelated failures are quoted as npm wrote them.
        let plain = output_tail("npm ERR! 404 Not Found");
        assert!(!plain.contains("管理员权限"), "{plain}");
    }

    /// The UI quotes `PnpmInstall.command` verbatim, so it has to be the same
    /// line `install_pnpm` runs — this is the assertion that keeps them equal.
    #[test]
    fn install_command_matches_the_arguments() {
        let npm = Path::new(r"C:\Program Files\nodejs\npm.cmd");
        assert_eq!(
            command_line(npm, &PNPM_INSTALL_ARGS),
            r"C:\Program Files\nodejs\npm.cmd install -g pnpm"
        );
    }

    /// Live check, not run by default: proves dshell finds an npm on *this*
    /// machine and can spawn it through the exact code path the button uses.
    ///
    /// ```powershell
    /// cargo test live_pnpm -- --ignored --nocapture
    /// ```
    ///
    /// It runs `npm --version`, never the install itself: a global install is
    /// something only the user's click is allowed to do.
    #[tokio::test]
    #[ignore = "needs a real Node.js + npm on this machine"]
    async fn live_pnpm_plan_and_spawn() {
        let node = detect_node(Path::new(".")).await;
        let plan = plan_pnpm_install(node.as_ref(), false).await;
        let npm = plan.npm_path().unwrap_or_else(|| {
            panic!("no npm found: {}", plan.blocked.clone().unwrap_or_default())
        });
        println!("npm: {} ({:?})", npm.display(), plan.npm_version);

        let output = run_npm(&npm, &["--version"]).await.expect("npm --version");
        assert!(
            output.chars().any(|c| c.is_ascii_digit()),
            "npm printed nothing usable: {output:?}"
        );
    }
}
