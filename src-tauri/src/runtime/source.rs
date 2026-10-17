//! The "source" runtime: a git checkout of DSH that dshell keeps up to date.
//!
//! `root` here is always the **checkout** — `<container>/deepseek-harness`, where
//! the container is the directory the user picked (by default
//! `<data root>/runtimes`). dshell never clones into the chosen directory itself:
//! that directory may already hold other projects, and owning it would turn
//! "remove the runtime" into a question about someone else's files.
//!
//! Running DSH from source is `node --import tsx/esm apps/cli/src/bin.ts web …`
//! (see [`crate::runtime::resolve`]), but getting there is a pipeline:
//!
//! | step | command | why it is here |
//! |---|---|---|
//! | clone | `git clone --depth 1 <repo> <root>` | only when the checkout does not exist yet |
//! | fetch | `git fetch --depth 1 origin` | every start, so "开始" means "latest" |
//! | reset | `git reset --hard FETCH_HEAD` | only when the tree is clean |
//! | install | `pnpm install` | ~350 workspaces; minutes on a cold machine |
//! | build | `pnpm run build` | tsc + tsdown + the web frontend's `dist/` |
//! | verify | `apps/web/dist/index.html` | without it DSH serves a 404 while claiming to be ready |
//!
//! Two rules the pipeline follows:
//!
//! * **Never destroy the user's work.** `reset --hard` runs only on a clean tree;
//!   a checkout with local modifications is built as it stands, with a notice.
//! * **Report every step.** The page waiting on this blocks its own controls, so
//!   the step name and the child's output are the only evidence the wait is alive.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Action, Failure, FailureCode};

pub const FRONTEND_ENTRY: &str = "apps/web/dist/index.html";
pub const CLI_ENTRY: &str = "apps/cli/src/bin.ts";

pub const CLONE_TIMEOUT: Duration = Duration::from_secs(1800);
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(2400);
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(2400);
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);

/// What a checkout looks like right now. Everything here is filesystem-local;
/// the network answer ("are we behind?") only exists inside [`prepare`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub root: String,
    /// `<root>/.git` exists.
    pub checkout: bool,
    /// The CLI entry and an installed dependency tree are both present.
    pub installed: bool,
    /// `apps/web/dist/index.html` — the frontend the web profile serves.
    pub frontend_built: bool,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub remote: Option<String>,
    /// `git status --porcelain` was non-empty; an update is then refused.
    pub dirty: bool,
    /// The directory exists but holds something other than a checkout.
    pub blocked: Option<String>,
}

impl Status {
    pub fn missing(root: &Path) -> Self {
        Self {
            root: root.to_string_lossy().to_string(),
            checkout: false,
            installed: false,
            frontend_built: false,
            version: None,
            commit: None,
            branch: None,
            remote: None,
            dirty: false,
            blocked: None,
        }
    }

    /// True when a launch would have something to start.
    pub fn usable(&self) -> bool {
        self.checkout && self.installed && self.frontend_built
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub status: Status,
    /// The checkout was cloned, fetched, or reset during this run.
    pub updated: bool,
    /// `pnpm install` ran.
    pub installed: bool,
    /// `pnpm run build` ran.
    pub built: bool,
    /// Something was skipped and the user should know why.
    pub notice: Option<String>,
}

/// Inspect a checkout directory without touching the network.
pub async fn status(root: &Path, git: Option<&Path>) -> Status {
    let mut status = Status::missing(root);
    if !root.exists() {
        return status;
    }
    if !root.is_dir() {
        status.blocked = Some("该路径不是目录".to_string());
        return status;
    }
    status.checkout = root.join(".git").exists();
    status.version = read_package_version(&root.join("package.json"));
    status.installed = root.join(CLI_ENTRY).is_file() && root.join("node_modules").is_dir();
    status.frontend_built = root.join(FRONTEND_ENTRY).is_file();

    if !status.checkout {
        // A directory with content that is not a checkout cannot be managed:
        // cloning into it would either fail or bury whatever is already there.
        let empty = std::fs::read_dir(root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty {
            status.blocked = Some(
                "该目录非空，且不是一个 git checkout。请选择空目录，或删除其中内容。".to_string(),
            );
        }
        return status;
    }

    if let Some(git) = git {
        status.commit = git_out(git, root, &["rev-parse", "--short", "HEAD"]).await;
        status.branch = git_out(git, root, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
        status.remote = git_out(git, root, &["remote", "get-url", "origin"]).await;
        status.dirty = git_out(git, root, &["status", "--porcelain"])
            .await
            .is_some_and(|text| !text.trim().is_empty());
    }
    status
}

fn read_package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

async fn git_out(git: &Path, cwd: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(git);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);
    let output = tokio::time::timeout(QUICK_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Where the pipeline reports its progress.
///
/// `stage` is the stable name the UI keys on ("fetch", "install", …) and
/// `message` is either the human sentence for that stage or one line of a
/// child's output, which is the only thing that changes during a ten-minute
/// build.
pub type Reporter<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// Bring `root` to a launchable state, cloning / updating / installing / building
/// as needed.
pub async fn prepare(
    root: &Path,
    repo: &str,
    report: Reporter<'_>,
) -> Result<Outcome, Failure> {
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Failure::new(
                FailureCode::SourcePrepareFailed,
                format!("无法创建目录：{}", parent.display()),
            )
            .with_detail(err.to_string())
        })?;
    }

    let (git, git_version) = crate::runtime::node::find_git().await.ok_or_else(|| {
        Failure::new(
            FailureCode::GitUnavailable,
            "未找到 git，无法维护源码 checkout",
        )
        .with_detail(
            "「源码版」需要一个 git 来克隆和更新 DSH 仓库。\
             安装 Git for Windows（https://git-scm.com/download/win）后重试。"
                .to_string(),
        )
        .with_action(Action::primary("open-git", "打开 Git 下载页"))
    })?;
    report("git", &format!("git {git_version}"));

    let before = status(root, Some(&git)).await;
    if let Some(blocked) = &before.blocked {
        return Err(Failure::new(FailureCode::SourcePrepareFailed, blocked.clone())
            .with_detail(root.display().to_string()));
    }

    let mut outcome = Outcome {
        status: before.clone(),
        updated: false,
        installed: false,
        built: false,
        notice: None,
    };

    // ---------------------------------------------------------------- checkout
    if !before.checkout {
        report("clone", &format!("正在克隆 {repo}"));
        run(
            &git,
            &[
                "clone".to_string(),
                "--depth".to_string(),
                "1".to_string(),
                repo.to_string(),
                root.to_string_lossy().to_string(),
            ],
            root.parent().unwrap_or(root),
            CLONE_TIMEOUT,
            "clone",
            report,
        )
        .await
        .map_err(|failure| {
            failure
                .with_detail(format!("目标目录：{}", root.display()))
                .with_action(Action::primary("choose-source-dir", "换一个目录"))
        })?;
        outcome.updated = true;
    } else if before.dirty {
        let notice = "该 checkout 有未提交的本地修改，已跳过 git 更新（按现状构建）";
        report("fetch", notice);
        outcome.notice = Some(notice.to_string());
    } else {
        // ---------------------------------------------------------------- update
        report("fetch", "正在检查远程更新…");
        let fetched = run(
            &git,
            &words(&["fetch", "--depth", "1", "origin"]),
            root,
            QUICK_TIMEOUT.max(Duration::from_secs(300)),
            "fetch",
            report,
        )
        .await;
        match fetched {
            Ok(()) => {
                let local = git_out(&git, root, &["rev-parse", "HEAD"]).await;
                let remote = git_out(&git, root, &["rev-parse", "FETCH_HEAD"]).await;
                if let (Some(local), Some(remote)) = (local, remote) {
                    if local != remote {
                        report("reset", "发现新提交，正在更新到最新…");
                        run(
                            &git,
                            &words(&["reset", "--hard", "FETCH_HEAD"]),
                            root,
                            QUICK_TIMEOUT,
                            "reset",
                            report,
                        )
                        .await?;
                        outcome.updated = true;
                    }
                }
            }
            // A dead network must not make a working checkout unusable: the
            // build already on disk is still a build.
            Err(failure) => {
                let notice = format!("无法检查远程更新（{}），将按现有提交启动", failure.message);
                report("fetch", &notice);
                outcome.notice = Some(notice);
            }
        }
    }

    let after_checkout = status(root, Some(&git)).await;
    if !after_checkout.checkout || !root.join(CLI_ENTRY).is_file() {
        return Err(Failure::new(
            FailureCode::SourcePrepareFailed,
            "checkout 里没有找到 DSH CLI 入口",
        )
        .with_detail(format!(
            "期望存在 {}",
            root.join(CLI_ENTRY).display()
        )));
    }

    let node = crate::runtime::node::detect_node(root).await;
    let node = crate::runtime::require_node(node.as_ref()).map_err(|failure| {
        failure
            .with_stage("source")
            .with_detail("源码版需要 Node 来执行 pnpm 与构建。")
    })?;
    let (pnpm, pnpm_version) = crate::runtime::node::find_pnpm(Some(node))
        .await
        .ok_or_else(|| {
            Failure::new(
                FailureCode::PnpmInstallFailed,
                "未找到 pnpm，无法安装/构建源码 checkout",
            )
            .with_detail(
                "源码版用 pnpm 维护约 350 个 workspace。\
                 可以在「首次配置 / 设置」里点「安装 pnpm」，或在终端运行 `npm install -g pnpm`。"
                    .to_string(),
            )
            .with_action(Action::primary("install-pnpm", "安装 pnpm"))
        })?;
    report("pnpm", &format!("pnpm {pnpm_version}"));

    // ----------------------------------------------------------------- install
    //
    // `pnpm install` is cheap when the lockfile is satisfied and expensive
    // otherwise; running it on every start is what keeps "开始" honest after a
    // `reset --hard` pulled new dependencies in.
    report("install", "正在安装依赖（pnpm install）…");
    run(
        &pnpm,
        &["install".to_string()],
        root,
        INSTALL_TIMEOUT,
        "install",
        report,
    )
    .await?;
    outcome.installed = true;

    // ------------------------------------------------------------------- build
    //
    // The frontend `dist/` is not optional: DSH prints its readiness line and
    // serves a 404 without it (docs/reference/dsh-integration.md §9.6).
    let built = status(root, Some(&git)).await;
    if built.frontend_built && !outcome.updated && !outcome.installed {
        report("verify", "已是最新构建，跳过 pnpm run build");
    } else {
        report("build", "正在构建（pnpm run build，可能需要数分钟）…");
        run(
            &pnpm,
            &["run".to_string(), "build".to_string()],
            root,
            BUILD_TIMEOUT,
            "build",
            report,
        )
        .await?;
        outcome.built = true;
    }

    if !root.join(FRONTEND_ENTRY).is_file() {
        report("build", "前端产物缺失，单独构建 web 前端…");
        run(
            &pnpm,
            &words(&["run", "build:web"]),
            root,
            BUILD_TIMEOUT,
            "build",
            report,
        )
        .await?;
    }

    outcome.status = status(root, Some(&git)).await;
    if !outcome.status.frontend_built {
        return Err(Failure::new(
            FailureCode::SourcePrepareFailed,
            "构建完成，但缺少前端产物",
        )
        .with_detail(format!(
            "期望存在 {}。DSH 在前端缺失时仍会打印就绪 URL，但页面是 404 —— \
             这正是 dshell 要在启动前拦住的失败。",
            root.join(FRONTEND_ENTRY).display()
        )));
    }
    if !outcome.status.installed {
        return Err(Failure::new(
            FailureCode::SourcePrepareFailed,
            "安装完成，但依赖树不完整",
        )
        .with_detail(format!(
            "期望存在 {} 与 {}\\node_modules",
            root.join(CLI_ENTRY).display(),
            root.display()
        )));
    }

    Ok(outcome)
}

/// Owned argv from string literals.
fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// Run one pipeline step, streaming its output.
#[allow(clippy::too_many_arguments)]
async fn run(
    program: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    stage: &str,
    report: Reporter<'_>,
) -> Result<(), Failure> {
    let label = format!("{} {}", program.display(), args.join(" "));
    report(stage, &label);

    // git must never wait for credentials on a GUI app's invisible stdin.
    let env = vec![
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("CI".to_string(), "1".to_string()),
    ];
    // One line per report, but a build log is huge: the page keeps its own ring
    // buffer, so the callback receives everything and only the failure report is
    // bounded here.
    let forward = |line: String| report(stage, &line);
    let output = crate::runtime::exec::run(program, args, Some(cwd), &env, timeout, 40, &forward)
        .await
        .map_err(|err| {
            Failure::new(
                FailureCode::SourcePrepareFailed,
                format!("无法运行 {label}：{err}"),
            )
            .with_stage(stage)
        })?;

    if output.timed_out {
        return Err(Failure::new(
            FailureCode::SourcePrepareFailed,
            format!("{label} 超时（{} 秒）", timeout.as_secs()),
        )
        .with_stage(stage)
        .with_detail(output.tail())
        .with_action(Action::primary("retry", "重试")));
    }
    if !output.success() {
        let code = output
            .code
            .map(|code| format!("退出码 {code}"))
            .unwrap_or_else(|| "被信号终止".into());
        return Err(Failure::new(
            FailureCode::SourcePrepareFailed,
            format!("{label} 失败（{code}）"),
        )
        .with_stage(stage)
        .with_detail(output.tail())
        .with_action(Action::primary("retry", "重试")));
    }
    Ok(())
}

/// The launch spec for a source checkout.
pub fn runtime_spec(root: &Path) -> crate::runtime::RuntimeSpec {
    crate::runtime::RuntimeSpec::Managed {
        root: root.to_string_lossy().to_string(),
        mode: crate::runtime::ManagedMode::Source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_directory_is_a_clone_target_not_a_blocker() {
        let dir = std::env::temp_dir().join(format!("dshell-src-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let status = status(&dir, None).await;
        assert!(!status.checkout);
        assert!(status.blocked.is_none(), "{:?}", status.blocked);
        assert!(!status.usable());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_non_empty_non_checkout_directory_is_refused() {
        let dir = std::env::temp_dir().join(format!("dshell-src-busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "here be dragons").unwrap();
        let status = status(&dir, None).await;
        assert!(status.blocked.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `usable()` is what gates a launch: a checkout that has not been installed
    /// or built would fail later as a 404 or a missing `tsx`, which is a much
    /// worse place to find out.
    #[tokio::test]
    async fn usability_requires_entry_install_and_frontend() {
        let dir = std::env::temp_dir().join(format!("dshell-src-usable-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let bare = status(&dir, None).await;
        assert!(bare.checkout);
        assert!(!bare.usable());

        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.join("apps").join("cli").join("src")).unwrap();
        std::fs::write(dir.join(CLI_ENTRY), "// entry").unwrap();
        std::fs::create_dir_all(dir.join("apps").join("web").join("dist")).unwrap();
        std::fs::write(dir.join(FRONTEND_ENTRY), "<html></html>").unwrap();
        let ready = status(&dir, None).await;
        assert!(ready.installed);
        assert!(ready.frontend_built);
        assert!(ready.usable());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_directory_reports_itself_as_missing() {
        let dir = std::env::temp_dir().join(format!("dshell-src-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let status = status(&dir, None).await;
        assert!(!status.checkout);
        assert!(status.blocked.is_none());
        assert_eq!(status.root, dir.to_string_lossy());
    }

    /// The real pipeline against the real repository: clone, `pnpm install`,
    /// `pnpm run build`. Minutes and gigabytes, so it is opt-in:
    ///
    /// ```powershell
    /// $env:DSHELL_SOURCE_TRY='E:\some\empty\dir'
    /// cargo test live_prepare -- --ignored --nocapture
    /// ```
    ///
    /// Running it twice against the same directory exercises the update path
    /// (`git fetch` + compare, then reinstall/rebuild only when something moved).
    #[tokio::test]
    #[ignore = "clones and builds the DSH repository"]
    async fn live_prepare_clones_installs_and_builds() {
        let root = std::path::PathBuf::from(
            std::env::var("DSHELL_SOURCE_TRY").expect("set DSHELL_SOURCE_TRY to a checkout path"),
        );
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let report = |stage: &str, message: &str| {
            let line = format!("[{stage}] {message}");
            println!("{line}");
            let mut seen = seen.lock().unwrap();
            if seen.last().map(String::as_str) != Some(line.as_str()) {
                seen.push(line);
            }
        };

        let started = std::time::Instant::now();
        let outcome = prepare(&root, crate::config::DSH_REPO_URL, &report)
            .await
            .expect("prepare");
        println!(
            "prepare took {:?}: updated={} installed={} built={} notice={:?}",
            started.elapsed(),
            outcome.updated,
            outcome.installed,
            outcome.built,
            outcome.notice
        );
        println!("status: {:#?}", outcome.status);
        assert!(outcome.status.checkout, "no checkout at {}", root.display());
        assert!(outcome.status.usable(), "{:?}", outcome.status);
    }

    /// The launch half: resolve a prepared checkout through the *same* code path
    /// the app uses and check that it really prints the readiness line — from a
    /// cwd that is not the checkout, which is the case in the app (DSH treats its
    /// cwd as the session workspace, so a local launch starts in the user's home).
    ///
    /// ```powershell
    /// $env:DSHELL_SOURCE_TRY='E:\...\source-try'
    /// cargo test live_source_checkout -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "needs a prepared checkout and runs it for a minute"]
    async fn live_source_checkout_serves_web() {
        let root = std::path::PathBuf::from(
            std::env::var("DSHELL_SOURCE_TRY").expect("set DSHELL_SOURCE_TRY to a checkout path"),
        );
        let data_root = std::env::temp_dir().join(format!("dshell-srclaunch-{}", std::process::id()));
        std::fs::create_dir_all(&data_root).unwrap();
        // Anything but the checkout: proves the plan does not depend on cwd.
        let workspace = data_root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let spec = runtime_spec(&root);
        let node = crate::runtime::node::detect_node(&data_root).await;
        let flags = crate::runtime::Flags {
            no_open: true,
            ..Default::default()
        };
        let plan = crate::runtime::resolve(&spec, node.as_ref(), &data_root, &workspace, &flags)
            .await
            .expect("resolve");
        println!("command: {}", plan.command_line());
        assert!(plan.args.iter().any(|arg| arg == "web"), "{:?}", plan.args);
        assert_eq!(plan.cwd, workspace);

        let env = plan.env.clone();
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let collect = |line: String| {
            println!("| {line}");
            seen.lock().unwrap().push(line);
        };
        // `dsh web` never exits; the readiness line arrives long before the
        // timeout, and `exec::run` kills the process when the clock runs out.
        let output = crate::runtime::exec::run(
            &plan.program,
            &plan.args,
            Some(&plan.cwd),
            &env,
            Duration::from_secs(90),
            400,
            &collect,
        )
        .await
        .expect("spawn");
        let lines = output.lines;
        let ready = lines
            .iter()
            .find(|line| line.starts_with("dsh web: http://"))
            .cloned();
        assert!(ready.is_some(), "no readiness line; last lines: {lines:?}");
        println!("ready: {}", ready.unwrap());

        let _ = std::fs::remove_dir_all(&data_root);
    }
}
