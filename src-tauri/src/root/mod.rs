//! The data root (`DSHELL_HOME`) — the only place that resolves it.
//!
//! Hard rule (same spirit as `tabs::manager` owning multi-webview): nothing else
//! in the codebase is allowed to join `DSHELL_HOME` by hand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const ENV_VAR: &str = "DSHELL_HOME";

/// Which level of the fallback chain actually won. Shown to the user verbatim:
/// "where does my data live" must never be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootSource {
    /// 1) `--data-root <path>`
    Cli,
    /// 2) `$DSHELL_HOME`
    Env,
    /// 3) `<exe dir>/dshell-data/`
    Portable,
    /// 4) `%LOCALAPPDATA%\dshell\`
    LocalAppData,
}

impl RootSource {
    pub fn label(self) -> &'static str {
        match self {
            RootSource::Cli => "命令行参数 --data-root",
            RootSource::Env => "$DSHELL_HOME 环境变量",
            RootSource::Portable => "exe 同级 dshell-data/（便携）",
            RootSource::LocalAppData => "%LOCALAPPDATA%\\dshell（回退）",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataRoot {
    pub path: PathBuf,
    pub source: RootSource,
    /// Set when an earlier level was skipped, e.g. `$DSHELL_HOME` pointed at a
    /// read-only directory. Surfacing this is the honest thing to do.
    pub note: Option<String>,
}

impl DataRoot {
    pub fn config_file(&self) -> PathBuf {
        self.path.join("config.json")
    }
    pub fn tabs_file(&self) -> PathBuf {
        self.path.join("tabs.json")
    }
    pub fn runtimes_dir(&self) -> PathBuf {
        self.path.join("runtimes")
    }
    pub fn webview_data_dir(&self) -> PathBuf {
        self.path.join("webview-data")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.path.join("logs")
    }
    pub fn dsh_home_dir(&self) -> PathBuf {
        self.path.join("dsh-home")
    }
}

/// Resolve the data root through the four-level fallback chain.
pub fn locate(cli_override: Option<PathBuf>) -> DataRoot {
    // 1) explicit flag — for tests and side-by-side debugging.
    if let Some(path) = cli_override {
        let abs = absolutize(&path);
        let mut note = None;
        if let Err(err) = crate::env::probe_writable(&abs) {
            note = Some(format!("--data-root 不可写（{err}），仍按参数继续（便于排查）"));
        }
        return DataRoot { path: abs, source: RootSource::Cli, note };
    }

    // 2) $DSHELL_HOME — written on first run, may be stale or unwritable.
    let env_note = match std::env::var_os(ENV_VAR) {
        Some(raw) => {
            let raw = raw.to_string_lossy().trim().to_string();
            if raw.is_empty() {
                None
            } else {
                let abs = absolutize(Path::new(&raw));
                match crate::env::probe_writable(&abs) {
                    Ok(()) => {
                        return DataRoot { path: abs, source: RootSource::Env, note: None };
                    }
                    Err(err) => Some(format!(
                        "$DSHELL_HOME={} 不可用（{err}），已回退",
                        abs.display()
                    )),
                }
            }
        }
        None => None,
    };

    // 3) next to the executable — the true portable default.
    // 4) %LOCALAPPDATA%\dshell — read-only media / restricted permissions.
    let (path, source) = portable_or_local();
    DataRoot { path, source, note: env_note }
}

fn portable_or_local() -> (PathBuf, RootSource) {
    if let Some(dir) = portable_dir() {
        if crate::env::probe_writable(&dir).is_ok() {
            return (dir, RootSource::Portable);
        }
    }
    (local_app_data_dir(), RootSource::LocalAppData)
}

fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("dshell-data");
    Some(absolutize(&dir))
}

fn local_app_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(dirs_fallback)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("dshell")
}

fn dirs_fallback() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|p| p.join("AppData").join("Local"))
}

/// Make a path absolute without requiring it to exist.
pub fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return normalize(path);
    }
    match std::env::current_dir() {
        Ok(cwd) => normalize(&cwd.join(path)),
        Err(_) => path.to_path_buf(),
    }
}

/// Collapse `.` and `..` textually (no symlink resolution, no filesystem hit).
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Lowercased, separator-normalized form of a path — used both for comparing
/// paths and for the stable instance key.
pub fn normalize_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let trimmed = text.trim_end_matches('\\');
    trimmed.to_lowercase()
}

/// Stable short id for "the same instance" (same workspace + same DSH_HOME).
///
/// A webview's persistent storage is keyed on this, not on the tab id, so
/// reopening the same workspace reuses the same cookies and localStorage.
pub fn instance_key(workspace: &Path, dsh_home: Option<&Path>) -> String {
    let mut input = normalize_key(workspace);
    input.push('|');
    if let Some(home) = dsh_home {
        input.push_str(&normalize_key(home));
    }
    fnv1a16(&input)
}

fn fnv1a16(input: &str) -> String {
    // FNV-1a 64-bit, then hex. Short, stable across runs and platforms, and
    // unlike DefaultHasher it is guaranteed not to change between releases.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A filesystem-safe directory name derived from a user-visible label.
pub fn slug(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_alphanumeric() {
            // Keep non-ASCII letters (Chinese project names are common here)
            // instead of collapsing everything to a dash.
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

/// DSH's own default home (`~/.dsh`), used only to *preview* what DSH will pick
/// when the user leaves `DSH_HOME` empty.
pub fn preview_default_dsh_home() -> Option<PathBuf> {
    Some(user_home()?.join(".dsh"))
}

pub fn preview_default_agents_home() -> Option<PathBuf> {
    Some(user_home()?.join(".agents"))
}

/// Where a local backend is started when the user did not choose a directory.
///
/// Deliberately neutral. DSH treats its process cwd as the *initial* workspace and
/// lets the user pick a project itself, so dshell has no business guessing a
/// project folder — it only needs somewhere harmless, and the home directory is
/// exactly what a user gets from running `dsh web` in their own shell.
pub fn default_workspace() -> Option<PathBuf> {
    user_home()
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dots() {
        assert_eq!(normalize(Path::new(r"E:\a\b\..\c")), PathBuf::from(r"E:\a\c"));
    }

    #[test]
    fn instance_key_is_stable_and_path_case_insensitive() {
        let a = instance_key(Path::new(r"E:\Work\App"), Some(Path::new(r"E:\Home")));
        let b = instance_key(Path::new(r"e:/work/app/"), Some(Path::new(r"E:\Home\")));
        assert_eq!(a, b);
        let c = instance_key(Path::new(r"E:\Work\App"), None);
        assert_ne!(a, c);
    }

    #[test]
    fn slug_keeps_unicode_letters() {
        assert_eq!(slug("my-app"), "my-app");
        assert_eq!(slug(r"E:\Projects\My App"), "e-projects-my-app");
        assert_eq!(slug("项目 A"), "项目-a");
    }
}
