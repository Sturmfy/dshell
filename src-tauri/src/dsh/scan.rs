//! Finding DSH servers that are already listening on this machine.
//!
//! A `dsh web` the user started in their own terminal is invisible to dshell:
//! dshell never spawned it, so it holds no token and no process handle. What it
//! *can* see is the port table, and a DSH answers an unauthenticated `GET /` with
//! its own 401 page. That page — not the status code — is the identity: 401 is
//! the most common answer in HTTP, and this scan once offered a Clash controller
//! to the user as "a dsh that wants the token it printed at startup". See
//! `probe::probe_dsh_server` for what the page has to say to count.
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
//!
//! The port table has no portable reader, so each platform gets its cheapest
//! one: `netstat -ano` on Windows, `/proc/net/tcp{,6}` on Linux (no process
//! launch), and `netstat -an -p tcp` on macOS. All three map into [`Listener`],
//! and only `bind`/`port` matter for the probe — `pid` is opportunistic.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Failure, FailureCode};

/// How long a listening port may take to answer the identification probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);
/// How many ports are probed at once.
const CONCURRENCY: usize = 16;
/// Reading the port table is cheap; a second is already generous.
const TABLE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// `127.0.0.1`, `::1`, … as the platform spelled it.
    pub bind: String,
    /// The process that owns the socket, when the platform printed it. Nothing
    /// reads it yet: the identity document that would make it decisive
    /// (`instance.pid == socket owner`) is not served by DSH. Capturing it here
    /// costs one field and saves a second table pass when it is.
    pub pid: Option<u32>,
}

/// Loopback listeners currently in `LISTEN` state.
pub async fn listeners() -> Result<Vec<Listener>, Failure> {
    #[cfg(windows)]
    {
        windows_listeners().await
    }
    #[cfg(target_os = "linux")]
    {
        linux_listeners().await
    }
    #[cfg(target_os = "macos")]
    {
        macos_listeners().await
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err(Failure::new(
            FailureCode::Internal,
            "这个平台还没有端口枚举实现",
        ))
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
async fn windows_listeners() -> Result<Vec<Listener>, Failure> {
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

#[cfg(windows)]
async fn netstat(family: &str) -> Result<String, Failure> {
    let mut cmd = tokio::process::Command::new("netstat");
    cmd.args(["-ano", "-p", family]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.creation_flags(crate::win::CREATE_NO_WINDOW);

    let output = tokio::time::timeout(TABLE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Failure::new(FailureCode::Internal, "netstat 超时，无法枚举本机端口"))?
        .map_err(|err| {
            Failure::new(FailureCode::Internal, format!("无法运行 netstat：{err}"))
        })?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `  TCP    127.0.0.1:9808    0.0.0.0:0    LISTENING    4242`
#[cfg(windows)]
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
    // `-o` puts the owning pid last. Its absence is not fatal — the bind address
    // and the port are what the probe needs — but it is worth keeping.
    let pid = fields.next().and_then(|value| value.parse::<u32>().ok());
    let (host, port) = split_endpoint(local)?;
    if !is_loopback(&host) {
        return None;
    }
    Some(Listener { port, bind: host, pid })
}

// ------------------------------------------------------------------ Linux

#[cfg(target_os = "linux")]
async fn linux_listeners() -> Result<Vec<Listener>, Failure> {
    let mut out = BTreeMap::<u16, Listener>::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            // IPv6 is commonly disabled; IPv4 is not optional.
            if path.ends_with('6') {
                continue;
            }
            return Err(Failure::new(
                FailureCode::Internal,
                format!("无法读取 {path}：这台机器可能没有挂载 /proc"),
            ));
        };
        for (host, port) in parse_proc_net_tcp(&text) {
            out.entry(port).or_insert(Listener {
                port,
                bind: host,
                pid: None,
            });
        }
    }
    Ok(out.into_values().collect())
}

/// Parse `/proc/net/tcp{,6}` and keep loopback rows in `LISTEN` state.
///
/// Columns are `sl local_address rem_address st …`; the local address is
/// `HEXIP:HEXPORT`, IPv4 as one little-endian `u32` and IPv6 as four of them.
/// State `0A` is `TCP_LISTEN`.
#[cfg(target_os = "linux")]
fn parse_proc_net_tcp(text: &str) -> Vec<(String, u16)> {
    let mut found = Vec::new();
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let Some(_slot) = fields.next() else {
            continue;
        };
        let Some(local) = fields.next() else {
            continue;
        };
        let Some(_remote) = fields.next() else {
            continue;
        };
        let Some(state) = fields.next() else {
            continue;
        };
        if state != "0A" {
            continue;
        }
        let Some((raw_host, raw_port)) = local.split_once(':') else {
            continue;
        };
        let Some(port) = u16::from_str_radix(raw_port, 16).ok() else {
            continue;
        };
        let Some(host) = decode_proc_address(raw_host) else {
            continue;
        };
        if is_loopback(&host) {
            found.push((host, port));
        }
    }
    found
}

/// `0100007F` → `127.0.0.1`; 32 hex chars → the `::1` spelling.
#[cfg(target_os = "linux")]
fn decode_proc_address(raw: &str) -> Option<String> {
    match raw.len() {
        8 => {
            let value = u32::from_str_radix(raw, 16).ok()?;
            let bytes = value.to_le_bytes();
            Some(format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]))
        }
        32 => {
            let mut bytes = [0u8; 16];
            for (index, chunk) in raw.as_bytes().as_chunks::<8>().0.iter().enumerate() {
                let text = std::str::from_utf8(chunk).ok()?;
                let value = u32::from_str_radix(text, 16).ok()?;
                bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            if bytes[..15].iter().all(|byte| *byte == 0) && bytes[15] == 1 {
                return Some("::1".to_string());
            }
            // A v4-mapped address (`::ffff:127.0.0.1`) is the same server.
            if bytes[..10].iter().all(|byte| *byte == 0)
                && bytes[10] == 0xff
                && bytes[11] == 0xff
            {
                return Some(format!(
                    "{}.{}.{}.{}",
                    bytes[12], bytes[13], bytes[14], bytes[15]
                ));
            }
            None
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ macOS

#[cfg(target_os = "macos")]
async fn macos_listeners() -> Result<Vec<Listener>, Failure> {
    let mut cmd = tokio::process::Command::new("netstat");
    cmd.args(["-an", "-p", "tcp"]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let output = tokio::time::timeout(TABLE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Failure::new(FailureCode::Internal, "netstat 超时，无法枚举本机端口"))?
        .map_err(|err| {
            Failure::new(FailureCode::Internal, format!("无法运行 netstat：{err}"))
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_macos_netstat(&text))
}

/// `tcp4       0      0  127.0.0.1.9808         *.*                    LISTEN`
///
/// BSD `netstat` separates the port with a dot and does not print an owner pid,
/// so that field stays `None`.
#[cfg(target_os = "macos")]
fn parse_macos_netstat(text: &str) -> Vec<Listener> {
    let mut out = BTreeMap::<u16, Listener>::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        if !fields[0].starts_with("tcp") {
            continue;
        }
        // The state column can be `LISTEN` on its own or be followed by options.
        if !fields.contains(&"LISTEN") {
            continue;
        }
        let Some(local) = fields.iter().find(|field| field.contains('.')) else {
            continue;
        };
        let Some((host, port)) = split_dotted_endpoint(local) else {
            continue;
        };
        if !is_loopback(&host) {
            continue;
        }
        out.entry(port).or_insert(Listener {
            port,
            bind: host,
            pid: None,
        });
    }
    out.into_values().collect()
}

/// `127.0.0.1.9808` → `("127.0.0.1", 9808)`; `::1.9808` → `("::1", 9808)`.
#[cfg(target_os = "macos")]
fn split_dotted_endpoint(endpoint: &str) -> Option<(String, u16)> {
    let (host, port) = endpoint.rsplit_once('.')?;
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() || host == "*" {
        return None;
    }
    Some((host.to_string(), port))
}

// ---------------------------------------------------------------- shared

fn is_loopback(host: &str) -> bool {
    matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1" | "localhost"
    )
}

/// The origin to probe, and the one to show the user.
///
/// It has to be the address the listener actually bound. Hard-coding
/// `127.0.0.1` looked harmless until `dsh web --host ::1` bound only `[::1]`:
/// the port was in the table, the probe went to an address nobody was listening
/// on, and a running instance was invisible.
fn origin_for(listener: &Listener) -> String {
    let host = listener.bind.trim_matches(['[', ']']);
    if host.contains(':') {
        format!("http://[{}]:{}", host, listener.port)
    } else {
        format!("http://{}:{}", host, listener.port)
    }
}

#[cfg(any(windows, test))]
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
            let origin = origin_for(&listener);
            crate::dsh::probe::probe_dsh_server(&origin, timeout).await?;
            Some(Instance {
                origin,
                port: listener.port,
                tab_id: None,
                title: None,
                connectable: false,
                // The probe only accepts DSH's own 401 page, so there is exactly
                // one thing left for the user to supply.
                notice: Some("需要这次启动打印的 token".to_string()),
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

    #[cfg(windows)]
    #[test]
    fn parses_loopback_listeners_only() {
        let loopback = "  TCP    127.0.0.1:9808         0.0.0.0:0              LISTENING       4242";
        let listener = parse_netstat_line(loopback).expect("loopback listener");
        assert_eq!(listener.port, 9808);
        assert_eq!(listener.bind, "127.0.0.1");
        assert_eq!(listener.pid, Some(4242));

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

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_proc_net_tcp_loopback_only() {
        // 127.0.0.1:9808 listening, 0.0.0.0:22 listening, established ignored.
        let text = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:2648 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2
   2: 0100007F:2648 0100007F:CCCC 01 00000000:00000000 00:00000000 00000000  1000        0 3
";
        let found = parse_proc_net_tcp(text);
        assert!(found.contains(&("127.0.0.1".to_string(), 9808)));
        assert_eq!(found.len(), 1, "wildcard and established rows are dropped");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn decodes_ipv6_loopback() {
        assert_eq!(
            decode_proc_address("00000000000000000000000001000000").as_deref(),
            Some("::1")
        );
        assert_eq!(
            decode_proc_address("0000000000000000ffff00000100007f").as_deref(),
            Some("127.0.0.1")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_macos_netstat_loopback_only() {
        let text = "\
tcp4       0      0  127.0.0.1.9808         *.*                    LISTEN
tcp4       0      0  0.0.0.0.22             *.*                    LISTEN
tcp6       0      0  ::1.3080               *.*                    LISTEN
tcp4       0      0  127.0.0.1.5555         127.0.0.1.50000        ESTABLISHED
";
        let found = parse_macos_netstat(text);
        let ports: Vec<u16> = found.iter().map(|listener| listener.port).collect();
        assert_eq!(ports, vec![3080, 9808]); // BTreeMap: ascending port order
        assert_eq!(found[0].bind, "::1");
    }

    /// Regression: the origin used to be hard-coded to `127.0.0.1`, so a
    /// `--host ::1` instance was probed at an address it never bound.
    #[test]
    fn probes_the_address_the_listener_bound() {
        let v4 = Listener { port: 9808, bind: "127.0.0.1".into(), pid: Some(1) };
        assert_eq!(origin_for(&v4), "http://127.0.0.1:9808");

        let v6 = Listener { port: 9808, bind: "[::1]".into(), pid: Some(1) };
        assert_eq!(origin_for(&v6), "http://[::1]:9808");

        // netstat has been seen to print the expanded spelling too.
        let expanded = Listener { port: 3080, bind: "0:0:0:0:0:0:0:1".into(), pid: None };
        assert_eq!(origin_for(&expanded), "http://[0:0:0:0:0:0:0:1]:3080");
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

    /// Live checks. They read this machine's real port table and talk to
    /// whatever is listening, so they are opt-in:
    ///
    /// ```text
    /// DSHELL_SCAN_EXPECT=http://127.0.0.1:11443 cargo test live_ -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "needs this machine's real port table"]
    async fn live_listeners_include_loopback_only() {
        let found = listeners().await.expect("port table");
        println!("{} loopback listeners", found.len());
        for listener in found.iter().take(20) {
            println!("  {} :{}", listener.bind, listener.port);
        }
        assert!(found.iter().all(|listener| listener.port > 0));
    }

    /// Proves the whole discovery path against a real `dsh web`: the port is
    /// found by the platform table and identified by the 401 an unauthenticated
    /// `GET /` returns. Point `DSHELL_SCAN_EXPECT` at that instance's origin.
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
