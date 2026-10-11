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
    /// `writable` for each DSH_HOME candidate the user selected, etc.
    pub notes: Vec<String>,
}

/// Full environment detection. Runs off the UI thread; shells out at most twice.
pub async fn detect(data_root: &Path) -> EnvironmentSnapshot {
    let node = detect_node(data_root).await;
    let pnpm = detect_pnpm().await;
    EnvironmentSnapshot {
        node,
        webview2_version: webview2_version(),
        pnpm,
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

async fn detect_pnpm() -> Option<String> {
    for name in ["pnpm.cmd", "pnpm.exe", "pnpm"] {
        for dir in path_dirs() {
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

/// Ordered candidates: portable → versioned install dirs → `PATH`.
fn node_candidates(data_root: &Path) -> Vec<(PathBuf, NodeSource)> {
    let mut out: Vec<(PathBuf, NodeSource)> = Vec::new();

    // 1) dshell-managed portable Node (the "download a portable Node" escape).
    out.push((data_root.join("runtimes").join("node").join("node.exe"), NodeSource::Portable));

    // 2) Well-known install directories, newest-first is not observable here, so
    //    a plain fixed order is enough — all of them are validated by --version.
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
    for root in roots {
        out.push((root.join("node.exe"), NodeSource::Installed));
    }

    // 3) PATH, enumerated by hand so a stale process environment is not the
    //    only source of truth (and so "re-detect" can pick up new locations).
    for dir in path_dirs() {
        out.push((dir.join("node.exe"), NodeSource::Path));
    }

    out
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
}
