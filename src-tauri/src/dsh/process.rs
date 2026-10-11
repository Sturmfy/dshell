//! Child-process supervision.
//!
//! Three things happen here that are easy to get wrong and expensive to debug:
//!
//! 1. **Job objects.** DSH spawns children of its own (`pwsh`, `rg`, `git`, …)
//!    and Windows `.cmd` shims add another generation. Terminating only the
//!    direct child leaves orphans holding the workspace and the port. A job
//!    object with `KILL_ON_JOB_CLOSE` is the only mechanism the kernel enforces
//!    even when dshell itself is killed.
//! 2. **The readiness line, not a guess.** The supervisor reads stdout line by
//!    line looking for the exact `dsh web: <url>` line and keeps draining
//!    afterwards — stopping the read would eventually block the child on a full
//!    pipe.
//! 3. **Token hygiene.** The URL is captured in memory; anything written to the
//!    log or disk is redacted first.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::dsh::url::{self, LaunchUrl};
use crate::error::{Action, Failure, FailureCode};
use crate::runtime::LaunchPlan;

pub const MAX_LOG_LINES: usize = 2000;
pub const GRACE_BEFORE_FORCE_KILL: Duration = Duration::from_secs(5);
/// How many raw lines the classifier keeps for failure reports.
const TAIL_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub text: String,
    pub stream: String,
    /// Milliseconds since the child started.
    pub at: u64,
}

#[derive(Debug)]
pub enum LaunchEvent {
    /// The strict readiness line was seen: server bound, `/api` mounted.
    Ready(LaunchUrl),
    Log(LogEntry),
    /// DSH wrote a full startup diagnostic report; point the user at DSH's own
    /// file instead of maintaining a parallel log.
    Diagnostics(PathBuf),
    Exited(ExitReport),
    Failed(Failure),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitReport {
    pub code: Option<i32>,
    pub expected: bool,
    pub uptime_ms: u64,
}

/// An owned child plus the job object that guarantees its whole tree dies.
pub struct ProcessHandle {
    pub pid: u32,
    pub command_line: String,
    cancel: CancellationToken,
    job: Option<crate::win::JobObject>,
    /// Shared with a waiter task that consumes `Child::wait()`; the supervisor
    /// itself never blocks on the lock, so `terminate()` can always run.
    child: std::sync::Arc<Mutex<tokio::process::Child>>,
}

impl ProcessHandle {
    /// Terminate the whole tree: close the job (the kernel reclaims every member)
    /// and fall back to a direct kill for non-Windows or if the job assignment
    /// was refused.
    pub async fn terminate(&self) {
        self.cancel.cancel();
        if let Some(job) = &self.job {
            job.terminate();
        }
        let mut child = self.child.lock().await;
        if tokio::time::timeout(GRACE_BEFORE_FORCE_KILL, child.wait()).await.is_err() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }

    /// True while the child has not been terminated by us.
    pub fn is_running(&self) -> bool {
        !self.cancel.is_cancelled()
    }

    /// Stop delivering supervision events without killing the process.
    ///
    /// Used when a tab is closed but its backend was asked to keep running (the
    /// user may want to attach to it again later).
    pub fn cancel_supervision(&self) {
        self.cancel.cancel();
    }
}

pub struct Spawned {
    pub handle: ProcessHandle,
    pub supervisor: tokio::task::JoinHandle<()>,
}

pub struct SpawnRequest {
    pub plan: LaunchPlan,
    pub tab_id: String,
    /// Where DSH's own diagnostics reports are expected (`<DSH_HOME>/logs`).
    pub log_dir: Option<PathBuf>,
    /// Data root, used only for the opt-in `DSHELL_DEBUG` log.
    pub debug_root: PathBuf,
    /// Hard ceiling before giving up on the readiness line. Cold npx resolution
    /// can take minutes, so this is generous by design.
    pub ready_timeout: Duration,
}

/// Spawn and start supervising. Returns as soon as the process exists.
pub fn spawn_and_supervise(
    request: SpawnRequest,
    events: UnboundedSender<LaunchEvent>,
) -> std::io::Result<Spawned> {
    let SpawnRequest { plan, tab_id, log_dir, debug_root, ready_timeout } = request;

    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    cmd.current_dir(&plan.cwd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    for (key, value) in &plan.env {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let command_line = plan.command_line();
    let debug_root = debug_root.clone();
    debug_log(
        &debug_root,
        &format!("spawn: {command_line} (cwd={})", plan.cwd.display()),
    );
    let mut child = cmd.spawn().map_err(|err| {
        debug_log(&debug_root, &format!("spawn FAILED: {command_line} -> {err}"));
        err
    })?;
    let pid = child.id().unwrap_or(0);
    debug_log(&debug_root, &format!("spawned pid={pid} for `{command_line}`"));

    // Assign to the job immediately after spawn — before the child has any
    // realistic chance to fork its own children.
    #[cfg(windows)]
    let job = match crate::win::JobObject::new() {
        Ok(job) => match child.raw_handle() {
            // SAFETY: the handle comes straight from `Command::spawn` and the
            // child has not been reaped yet.
            Some(handle) if unsafe { job.assign(handle) }.is_ok() => Some(job),
            // Nested-job environments (CI, containers) can refuse the
            // assignment; the fallback is a direct kill.
            _ => None,
        },
        Err(_) => None,
    };
    #[cfg(not(windows))]
    let job: Option<crate::win::JobObject> = None;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let child = std::sync::Arc::new(Mutex::new(child));

    let (exit_tx, exit_rx) = mpsc::unbounded_channel();
    {
        let waiter_child = child.clone();
        tokio::spawn(async move {
            let status = waiter_child.lock().await.wait().await;
            let _ = exit_tx.send(status);
        });
    }

    let handle = ProcessHandle {
        pid,
        command_line: command_line.clone(),
        cancel: cancel.clone(),
        job,
        child: child.clone(),
    };

    let supervisor = tokio::spawn(async move {
        supervise(
            tab_id,
            started,
            stdout,
            stderr,
            cancel,
            log_dir,
            debug_root,
            ready_timeout,
            events,
            exit_rx,
        )
        .await;
    });

    Ok(Spawned { handle, supervisor })
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    tab_id: String,
    started: Instant,
    stdout: Option<impl AsyncRead + Unpin + Send + 'static>,
    stderr: Option<impl AsyncRead + Unpin + Send + 'static>,
    cancel: CancellationToken,
    log_dir: Option<PathBuf>,
    debug_root: PathBuf,
    ready_timeout: Duration,
    events: UnboundedSender<LaunchEvent>,
    mut exit_rx: mpsc::UnboundedReceiver<std::io::Result<std::process::ExitStatus>>,
) {
    let _ = &tab_id;
    let _ = log_dir;
    debug_log(&debug_root, "supervisor started");

    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_LINES);
    let mut saw_ready = false;
    let mut diagnostics_path: Option<PathBuf> = None;

    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<(LogStream, String)>();
    if let Some(stdout) = stdout {
        let tx = line_tx.clone();
        tokio::spawn(async move { pump(stdout, LogStream::Stdout, tx).await });
    }
    if let Some(stderr) = stderr {
        let tx = line_tx.clone();
        tokio::spawn(async move { pump(stderr, LogStream::Stderr, tx).await });
    }
    drop(line_tx);

    // Boxed so the select arm can borrow it (`Sleep` is `Unpin`, but the arm's
    // `&mut` must not outlive the task).
    let mut timeout_at = Box::pin(tokio::time::sleep(ready_timeout));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,

            _ = &mut timeout_at, if !saw_ready => {
                debug_log(&debug_root, "readiness timeout");
                let _ = events.send(LaunchEvent::Failed(startup_timeout(&tail, ready_timeout)));
                return;
            }

            line = line_rx.recv() => {
                let Some((stream, line)) = line else { continue };
                let entry = LogEntry {
                    text: url::redact_line(&line),
                    stream: match stream {
                        LogStream::Stdout => "stdout".into(),
                        LogStream::Stderr => "stderr".into(),
                    },
                    at: started.elapsed().as_millis() as u64,
                };
                tail.push_back(format!("[{}] {}", entry.stream, entry.text));
                while tail.len() > TAIL_LINES {
                    tail.pop_front();
                }

                if let Some(path) = parse_diagnostics_path(&line) {
                    diagnostics_path = Some(path.clone());
                    let _ = events.send(LaunchEvent::Diagnostics(path));
                }

                if !saw_ready {
                    if let Some(launch) = url::parse_ready_line(&line) {
                        saw_ready = true;
                        debug_log(&debug_root, &format!("ready line: {}", launch.redacted()));
                        let _ = events.send(LaunchEvent::Ready(launch));
                    }
                }

                let _ = events.send(LaunchEvent::Log(entry));
            }

            status = exit_rx.recv() => {
                let code = status
                    .and_then(|result| result.ok())
                    .and_then(|status| status.code());
                let uptime_ms = started.elapsed().as_millis() as u64;
                debug_log(
                    &debug_root,
                    &format!("child exited code={code:?} uptime={uptime_ms}ms saw_ready={saw_ready}\n--- tail ---\n{}",
                        tail_text(&tail)),
                );
                if !saw_ready {
                    // Dying before the readiness line is a startup failure, and
                    // stderr is where DSH explains it.
                    let failure = classify_exit(
                        code,
                        &tail_text(&tail),
                        diagnostics_path.clone(),
                        cancel.is_cancelled(),
                    );
                    let _ = events.send(LaunchEvent::Failed(failure));
                } else {
                    let _ = events.send(LaunchEvent::Exited(ExitReport {
                        code,
                        expected: cancel.is_cancelled(),
                        uptime_ms,
                    }));
                }
                return;
            }
        }
    }
}

async fn pump(
    reader: impl AsyncRead + Unpin,
    stream: LogStream,
    tx: mpsc::UnboundedSender<(LogStream, String)>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                if tx.send((stream, text)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// DSH prints `Full diagnostics: <path>` when a required plugin fails to
/// activate. Reusing DSH's own report beats writing a second one.
fn parse_diagnostics_path(line: &str) -> Option<PathBuf> {
    let rest = line.trim().strip_prefix("Full diagnostics:")?.trim();
    if rest.is_empty() {
        None
    } else {
        Some(PathBuf::from(rest))
    }
}

fn startup_timeout(tail: &VecDeque<String>, budget: Duration) -> Failure {
    Failure::new(
        FailureCode::StartupTimeout,
        format!("{} 秒内未出现 `dsh web:` 就绪行", budget.as_secs()),
    )
    .with_detail(tail_text(tail))
    .with_stage("startup")
    .with_defaults()
}

/// Classify an exit that happened before readiness.
fn classify_exit(
    code: Option<i32>,
    log: &str,
    diagnostics: Option<PathBuf>,
    cancelled: bool,
) -> Failure {
    if cancelled {
        return Failure::new(FailureCode::Cancelled, "已取消");
    }
    let lower = log.to_lowercase();
    let mut failure = if lower.contains("eaddrinuse") {
        Failure::new(FailureCode::PortBindFailed, "端口已被占用（EADDRINUSE）")
            .with_action(Action::primary("retry-free-port", "改用随机端口重试"))
    } else if lower.contains("enotdir") || lower.contains("eperm") || lower.contains("eacces") {
        Failure::new(
            FailureCode::DshHomeUnwritable,
            "DSH 无法写入它的状态目录（DSH_HOME）",
        )
        .with_detail(
            "DSH 在不可写目录上会以原始 V8 崩溃退出，不写诊断日志。\
             请检查 DSH_HOME 指向的路径是否可写。",
        )
    } else if lower.contains("cannot find package")
        || lower.contains("module_not_found")
        || lower.contains("failed to apply loader entry")
    {
        Failure::new(FailureCode::FrontendMissing, "DSH 的依赖树不完整（缺少包）")
    } else {
        Failure::new(
            FailureCode::ChildExited,
            format!(
                "DSH 进程在就绪前退出（退出码 {}）",
                code.map(|c| c.to_string()).unwrap_or_else(|| "无".into())
            ),
        )
    };

    failure = failure.with_detail(log.trim().to_string());
    if let Some(path) = diagnostics {
        failure = failure.with_log(path.to_string_lossy().to_string());
    }
    failure.with_stage("startup").with_defaults()
}

fn tail_text(tail: &VecDeque<String>) -> String {
    tail.iter()
        .rev()
        .take(24)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Snapshot of a `pids.json` entry, used to report leftovers after a crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftoverProcess {
    pub pid: u32,
    pub command_line: String,
    pub started_at: u64,
}

pub fn pids_file(data_root: &Path) -> PathBuf {
    data_root.join("pids.json")
}

/// Opt-in diagnostic log (`DSHELL_DEBUG=1`).
///
/// Everything interesting here happens in a background task, where a failure is
/// invisible by construction: no window shows it and nothing is printed. A
/// user-reportable file is the only way to debug "the tab said failed" without
/// asking anyone to attach a debugger.
pub fn debug_log(root: &Path, message: &str) {
    if std::env::var_os("DSHELL_DEBUG").is_none() {
        return;
    }
    let path = root.join("logs").join("dshell-debug.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(file, "[{stamp}] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diagnostics_line() {
        let line = r"Full diagnostics: C:\Users\x\.dsh\logs\startup-2026-01-01-abc.log";
        let path = parse_diagnostics_path(line).unwrap();
        assert!(path.to_string_lossy().ends_with("abc.log"));
        assert!(parse_diagnostics_path("not a diagnostics line").is_none());
    }

    #[test]
    fn classifies_addr_in_use() {
        let failure = classify_exit(
            Some(1),
            "Error: listen EADDRINUSE: address already in use 127.0.0.1:3080",
            None,
            false,
        );
        assert_eq!(failure.code, FailureCode::PortBindFailed);
    }

    #[test]
    fn classifies_unwritable_home_crash() {
        let failure = classify_exit(
            Some(1),
            "Error: EPERM: operation not permitted, mkdir 'C:\\Program Files\\x\\profiles\\web'",
            None,
            false,
        );
        assert_eq!(failure.code, FailureCode::DshHomeUnwritable);
    }
}
