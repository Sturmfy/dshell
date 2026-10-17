//! Running a helper process and streaming what it says.
//!
//! Two callers need this: warming the npx tree, and the source pipeline
//! (`git clone` / `pnpm install` / `pnpm run build`). Both are long, both are
//! invisible unless their output is forwarded, and both must be killed by the
//! caller's timeout rather than left running.
//!
//! The output pump deliberately does **not** run in a spawned task that owns the
//! caller's callback: the callback borrows the caller's state (an event emitter, a
//! buffer), and moving it into a `'static` task would force every caller to wrap
//! its reporting in `Arc`s. Instead the pumps own only a channel sender, and the
//! caller consumes lines in its own task.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;

/// What a finished helper process produced.
pub struct Output {
    /// The timeout fired; the process was killed.
    pub timed_out: bool,
    pub code: Option<i32>,
    /// The last `keep` lines, in order.
    pub lines: Vec<String>,
}

impl Output {
    pub fn success(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    pub fn tail(&self) -> String {
        self.lines.join("\n")
    }
}

/// Spawn `program`, forward each output line to `on_line`, and wait for it.
///
/// `keep` bounds how many lines are retained for the failure report; a build log
/// can be tens of thousands of lines and the interesting part is always the end.
pub async fn run(
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
    env: &[(String, String)],
    timeout: Duration,
    keep: usize,
    on_line: &(dyn Fn(String) + Send + Sync),
) -> std::io::Result<Output> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("NO_COLOR", "1");
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let mut child = cmd.spawn()?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pumps.push(tokio::spawn(pump(stdout, tx.clone())));
    }
    if let Some(stderr) = child.stderr.take() {
        pumps.push(tokio::spawn(pump(stderr, tx.clone())));
    }
    drop(tx);

    let mut lines: Vec<String> = Vec::new();
    let mut output_done = false;
    let mut timed_out = false;
    let mut code = None;

    let clock = tokio::time::sleep(timeout);
    tokio::pin!(clock);
    {
        // The wait future borrows `child`; scoping it means the timeout path can
        // still kill the process afterwards.
        let mut wait = Box::pin(child.wait());
        loop {
            tokio::select! {
                result = &mut wait => {
                    code = result.ok().and_then(|status| status.code());
                    break;
                }
                _ = &mut clock => {
                    timed_out = true;
                    break;
                }
                line = rx.recv(), if !output_done => {
                    match line {
                        Some(text) => record(&mut lines, text, keep, on_line),
                        None => output_done = true,
                    }
                }
            }
        }
    }

    if timed_out {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
    while let Ok(text) = rx.try_recv() {
        record(&mut lines, text, keep, on_line);
    }

    // Windows `.cmd` shims (`npx.cmd`, `pnpm.cmd`) add a generation: the child
    // we spawned can exit while a grandchild still holds the pipe open, in which
    // case the pump never sees EOF. Waiting for the pumps is therefore bounded —
    // an exited process is the answer the caller asked for, and the remaining
    // reader is left to finish on its own.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        for pump_task in pumps {
            let _ = pump_task.await;
        }
    })
    .await;

    Ok(Output { timed_out, code, lines })
}

fn record(
    lines: &mut Vec<String>,
    text: String,
    keep: usize,
    on_line: &(dyn Fn(String) + Send + Sync),
) {
    on_line(text.clone());
    if lines.len() >= keep {
        lines.remove(0);
    }
    lines.push(text);
}

async fn pump(reader: impl AsyncRead + Unpin + Send + 'static, tx: mpsc::UnboundedSender<String>) {
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
                if text.trim().is_empty() {
                    continue;
                }
                if tx.send(text).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pump and the waiter race on purpose: a child that prints and exits
    /// immediately must still have its output delivered before `run` returns.
    ///
    /// Ignored by default because it spawns a real `cmd.exe`; run with
    /// `cargo test exec_ -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "spawns a real shell"]
    async fn forwards_output_before_returning() {
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let collect = |line: String| seen.lock().unwrap().push(line);
        let output = run(
            Path::new("cmd.exe"),
            &["/C".into(), "echo hello".into()],
            None,
            &[],
            Duration::from_secs(30),
            10,
            &collect,
        )
        .await
        .expect("spawn");
        assert!(output.success(), "code={:?}", output.code);
        assert!(
            seen.lock().unwrap().iter().any(|line| line.contains("hello")),
            "no output forwarded"
        );
    }
}
