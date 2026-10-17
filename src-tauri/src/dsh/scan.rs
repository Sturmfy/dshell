//! Finding DSH servers that are already listening on this machine.
//!
//! A `dsh web` the user started in their own terminal is invisible to dshell:
//! dshell never spawned it, so it holds no token and no process handle. What it
//! *can* see is the port table, and DSH is easy to recognise from outside — an
//! unauthenticated `GET /` answers **401** ("I am DSH, and I want a token"),
//! which is a far more specific signal than any banner.
//!
//! Only loopback listeners are candidates. DSH refuses to bind a wildcard
//! address at all (`webserver` throws on `0.0.0.0`), so a wildcard entry belongs
//! to some other service and probing it would only add noise.
//!
//! Discovery is deliberately unable to hand over a session by itself: the token
//! lives in the terminal that printed it. The list is a *shortcut* — it fills in
//! the origin and tells the user what is missing — plus a one-click path for the
//! origins dshell already has a credential for (its own tabs, or a remembered
//! connection).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Failure, FailureCode};

/// How long a listening port may take to answer the identification probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);
/// How many ports are probed at once.
const CONCURRENCY: usize = 16;
/// `netstat` reads the whole TCP table; a second is already generous.
const NETSTAT_TIMEOUT: Duration = Duration::from_secs(5);

/// One DSH server found on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    /// `http://127.0.0.1:PORT` — always the loopback spelling, because a session
    /// cookie is bound to the exact authority and mixing `localhost` with
    /// `127.0.0.1` silently invalidates it.
    pub origin: String,
    pub port: u16,
    /// The tab that already shows this instance, when dshell started it.
    #[serde(default)]
    pub tab_id: Option<String>,
    pub title: Option<String>,
    /// dshell holds a token for this origin (its own tab, or a saved connection).
    pub connectable: bool,
    /// One line explaining what the user has to do next.
    #[serde(default)]
    pub notice: Option<String>,
}

/// A TCP listener on loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    /// `127.0.0.1`, `::1`, … as netstat spelled it.
    pub bind: String,
}

/// Loopback listeners currently in `LISTENING` state.
///
/// Parsed from `netstat -ano` rather than asked of the OS directly: the table is
/// one process launch, needs no FFI for `GetExtendedTcpTable`, and produces the
/// same list `netstat` shows a user who wants to check by hand.
pub async fn listeners() -> Result<Vec<Listener>, Failure> {
    let mut out = BTreeMap::<u16, Listener>::new();
    for family in ["TCP", "TCPv6"] {
        let text = match netstat(family).await {
            Ok(text) => text,
            // IPv6 is absent on plenty of machines; only a total failure is
            // worth reporting.
            Err(_) if family == "TCPv6" => continue,
            Err(err) => return Err(err),
        };
        for line in text.lines() {
            let Some(listener) = parse_netstat_line(line) else {
                continue;
            };
            out.entry(listener.port).or_insert(listener);
        }
    }
    Ok(out.into_values().collect())
}

async fn netstat(family: &str) -> Result<String, Failure> {
    let mut cmd = tokio::process::Command::new("netstat");
    cmd.args(["-ano", "-p", family]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let output = tokio::time::timeout(NETSTAT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            Failure::new(FailureCode::Internal, "netstat 超时，无法枚举本机端口")
        })?
        .map_err(|err| {
            Failure::new(
                FailureCode::Internal,
                format!("无法运行 netstat：{err}"),
            )
        })?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `  TCP    127.0.0.1:9808    0.0.0.0:0    LISTENING    4242`
fn parse_netstat_line(line: &str) -> Option<Listener> {
    let mut fields = line.split_whitespace();
    let proto = fields.next()?;
    if !proto.eq_ignore_ascii_case("tcp") && !proto.eq_ignore_ascii_case("tcpv6") {
        return None;
    }
    let local = fields.next()?;
    let _foreign = fields.next()?;
    let state = fields.next()?;
    if !state.eq_ignore_ascii_case("LISTENING") {
        return None;
    }
    let (host, port) = split_endpoint(local)?;
    let loopback = matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1" | "localhost"
    );
    if !loopback {
        return None;
    }
    Some(Listener { port, bind: host })
}

fn split_endpoint(endpoint: &str) -> Option<(String, u16)> {
    let (host, port) = endpoint.rsplit_once(':')?;
    Some((host.to_string(), port.parse::<u16>().ok()?))
}

/// Probe every loopback listener and keep the ones that answer like a DSH server.
pub async fn scan(timeout: Duration) -> Result<Vec<Instance>, Failure> {
    let listeners = listeners().await?;
    if listeners.is_empty() {
        return Ok(Vec::new());
    }
    let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    let mut tasks = Vec::new();
    for listener in listeners {
        let limit = limit.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = limit.acquire().await.ok()?;
            let origin = format!("http://127.0.0.1:{}", listener.port);
            let status = crate::dsh::probe::probe_dsh_server(&origin, timeout).await?;
            Some(Instance {
                origin,
                port: listener.port,
                tab_id: None,
                title: None,
                connectable: false,
                notice: Some(if status == 401 {
                    "需要这次启动打印的 token".to_string()
                } else {
                    "该端口上的 DSH 不接受 loopback 访问（403）".to_string()
                }),
            })
        }));
    }

    let mut found = Vec::new();
    for task in tasks {
        if let Ok(Some(instance)) = task.await {
            found.push(instance);
        }
    }
    found.sort_by_key(|instance| instance.port);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loopback_listeners_only() {
        let loopback = "  TCP    127.0.0.1:9808         0.0.0.0:0              LISTENING       4242";
        let listener = parse_netstat_line(loopback).expect("loopback listener");
        assert_eq!(listener.port, 9808);
        assert_eq!(listener.bind, "127.0.0.1");

        let v6 = "  TCP    [::1]:3080             [::]:0                 LISTENING       4242";
        assert_eq!(parse_netstat_line(v6).map(|l| l.port), Some(3080));

        // A wildcard listener is never DSH: it refuses to bind one at all, and
        // probing every system service would only add noise.
        let wildcard = "  TCP    0.0.0.0:445            0.0.0.0:0              LISTENING       4";
        assert!(parse_netstat_line(wildcard).is_none());

        // Non-listening rows describe sockets, not servers.
        let established =
            "  TCP    127.0.0.1:9808         127.0.0.1:52411        ESTABLISHED     4242";
        assert!(parse_netstat_line(established).is_none());

        // The IPv6 header and the UDP table are skipped without a fuss.
        assert!(parse_netstat_line("活动连接").is_none());
        assert!(parse_netstat_line("  UDP    0.0.0.0:5353           *:*                                    4").is_none());
    }

    #[test]
    fn parses_both_endpoint_spellings() {
        assert_eq!(
            split_endpoint("127.0.0.1:9808"),
            Some(("127.0.0.1".to_string(), 9808))
        );
        assert_eq!(
            split_endpoint("[::1]:3080"),
            Some(("[::1]".to_string(), 3080))
        );
        assert_eq!(split_endpoint("no-port"), None);
    }

    /// Live checks. They spawn `netstat` and talk to whatever is listening, so
    /// they are opt-in:
    ///
    /// ```powershell
    /// $env:DSHELL_SCAN_EXPECT='http://127.0.0.1:11443'
    /// cargo test live_ -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "needs netstat and this machine's real port table"]
    async fn live_listeners_include_loopback_only() {
        let found = listeners().await.expect("netstat");
        println!("{} loopback listeners", found.len());
        for listener in found.iter().take(20) {
            println!("  {} :{}", listener.bind, listener.port);
        }
        assert!(!found.is_empty(), "a Windows box always has some");
        assert!(found.iter().all(|listener| listener.port > 0));
    }

    /// Proves the whole discovery path against a real `dsh web`: the port is
    /// found by `netstat` and identified by the 401 an unauthenticated `GET /`
    /// returns. Point `DSHELL_SCAN_EXPECT` at that instance's origin.
    #[tokio::test]
    #[ignore = "needs a real dsh web listening on this machine"]
    async fn live_scan_finds_a_running_dsh() {
        let expected = std::env::var("DSHELL_SCAN_EXPECT")
            .expect("set DSHELL_SCAN_EXPECT to the instance origin");
        let found = scan(PROBE_TIMEOUT).await.expect("scan");
        let origins: Vec<&str> = found.iter().map(|instance| instance.origin.as_str()).collect();
        println!("found: {origins:?}");
        assert!(
            origins.contains(&expected.as_str()),
            "scan missed {expected}: {origins:?}"
        );
        let instance = found
            .iter()
            .find(|instance| instance.origin == expected)
            .expect("checked above");
        assert!(!instance.connectable, "an unscanned instance has no credential");
    }
}
