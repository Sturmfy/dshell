//! Parsing and sanitising DSH launch URLs.
//!
//! The token in `dsh web: http://127.0.0.1:PORT/?token=…` is a bearer credential
//! for the local server. It lives in memory only: never in `tabs.json`, never in
//! a log file, never in the diagnostic panel.

use serde::{Deserialize, Serialize};

/// The strict readiness pattern. `contains("dsh web:")` would happily match npm
/// noise, so the whole line has to match.
pub fn parse_ready_line(line: &str) -> Option<LaunchUrl> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("dsh web:")?;
    let rest = rest.trim();
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    LaunchUrl::parse(rest)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchUrl {
    /// Full URL including the token query parameter.
    pub url: String,
    /// `http://127.0.0.1:PORT` — safe to persist and display.
    pub origin: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl LaunchUrl {
    pub fn parse(raw: &str) -> Option<Self> {
        let (scheme, rest) = split_scheme(raw)?;
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let (authority, _path) = split_authority(rest);
        let authority = authority.split('@').next_back()?.to_string();
        let (host, port) = split_host_port(&authority, scheme)?;
        if host.is_empty() {
            return None;
        }
        // An IPv6 literal loses its brackets when split, but the origin it is
        // rebuilt into needs them back: `http://::1:9808` is not an authority,
        // and a scan that binds `[::1]` only has to hand that spelling on.
        let authority_host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        Some(Self {
            url: raw.to_string(),
            origin: format!("{scheme}://{authority_host}:{port}"),
            scheme: scheme.to_string(),
            host,
            port,
        })
    }

    /// Same URL with the token replaced, for logs and tooltips.
    pub fn redacted(&self) -> String {
        redact_token(&self.url)
    }

    pub fn is_loopback(&self) -> bool {
        is_loopback_host(&self.host)
    }

    pub fn has_token(&self) -> bool {
        has_token_param(&self.url)
    }

    /// Is this a page dshell can act as a browser origin for?
    ///
    /// Tauri's own pages live at `http://tauri.localhost/…` in release builds,
    /// which parses as a perfectly ordinary http URL. Treating one of those as a
    /// DSH origin would poison `display_origin`, the session probe and the
    /// persisted tab record, so every origin decision goes through here.
    pub fn is_web_origin(&self) -> bool {
        self.scheme == "http" || self.scheme == "https"
    }

    /// True when the parsed host belongs to dshell itself rather than to a DSH
    /// instance.
    pub fn is_dshell_page(&self) -> bool {
        let host = self.host.to_lowercase();
        host == "tauri.localhost" || host.ends_with(".tauri.localhost")
    }
}

pub fn redact_token(url: &str) -> String {
    match url.find("token=") {
        Some(idx) => format!("{}token=***", &url[..idx]),
        None => url.to_string(),
    }
}

/// Redact any `token=…` occurrence inside an arbitrary log line.
pub fn redact_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(idx) = rest.find("token=") {
        out.push_str(&rest[..idx]);
        out.push_str("token=***");
        rest = &rest[idx + "token=".len()..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

pub fn has_token_param(url: &str) -> bool {
    match url.find('?') {
        Some(idx) => url[idx + 1..].split('&').any(|pair| {
            let mut it = pair.splitn(2, '=');
            matches!(it.next(), Some("token")) && it.next().map(|v| !v.is_empty()).unwrap_or(false)
        }),
        None => false,
    }
}

pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']').to_lowercase();
    if host == "localhost" || host == "::1" {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(addr) => addr.is_loopback(),
        Err(_) => false,
    }
}

/// The authority string a browser would send in `Host:` — needed to tell the
/// user the exact `--trusted-host` value to use on the server.
pub fn authority_of(origin: &str) -> String {
    match split_scheme(origin) {
        Some((_scheme, rest)) => split_authority(rest).0.to_string(),
        None => origin.to_string(),
    }
}

fn split_scheme(raw: &str) -> Option<(&str, &str)> {
    let idx = raw.find("://")?;
    let scheme = &raw[..idx];
    if scheme.is_empty() {
        return None;
    }
    Some((scheme, &raw[idx + 3..]))
}

fn split_authority(rest: &str) -> (&str, &str) {
    match rest.find(['/', '?', '#']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    }
}

fn split_host_port(authority: &str, scheme: &str) -> Option<(String, u16)> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6 literal
        let end = stripped.find(']')?;
        let host = stripped[..end].to_string();
        let after = &stripped[end + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            p.parse::<u16>().ok()?
        } else {
            default_port
        };
        return Some((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Some((host.to_lowercase(), port.parse::<u16>().ok()?))
        }
        _ => Some((authority.to_lowercase(), default_port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_ready_line() {
        let line = "dsh web: http://127.0.0.1:9808/?token=pK_gNICfnPUjLiwgXGGbycus_8ngT2YhDlwIctW72V4";
        let url = parse_ready_line(line).unwrap();
        assert_eq!(url.origin, "http://127.0.0.1:9808");
        assert_eq!(url.host, "127.0.0.1");
        assert_eq!(url.port, 9808);
        assert!(url.is_loopback());
        assert!(url.has_token());
    }

    #[test]
    fn rejects_noise_that_merely_contains_the_prefix() {
        assert!(parse_ready_line("npm warn dsh web: not really").is_none());
        assert!(parse_ready_line("dsh web: ").is_none());
        assert!(parse_ready_line("dsh web: ftp://127.0.0.1:1/?token=x").is_none());
    }

    #[test]
    fn redacts_tokens_in_logs() {
        let line = "curl 'http://127.0.0.1:9808/?token=abc123&x=1'";
        let redacted = redact_line(line);
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("token=***"));
    }

    #[test]
    fn detects_plaintext_remote_and_authority() {
        let url = LaunchUrl::parse("http://192.168.31.93:3080/?token=t").unwrap();
        assert!(!url.is_loopback());
        assert_eq!(url.scheme, "http");
        assert_eq!(authority_of(&url.origin), "192.168.31.93:3080");
    }

    #[test]
    fn handles_https_default_port() {
        let url = LaunchUrl::parse("https://app.example.com/?token=t").unwrap();
        assert_eq!(url.port, 443);
        assert_eq!(url.origin, "https://app.example.com:443");
    }

    /// A listener bound to `::1` only is reachable at `[::1]` and nowhere else.
    /// The origin has to keep the brackets the browser needs, while `host` stays
    /// bare so `TcpStream::connect` can resolve it.
    #[test]
    fn keeps_brackets_in_an_ipv6_origin() {
        let url = LaunchUrl::parse("http://[::1]:9808/?token=t").unwrap();
        assert_eq!(url.origin, "http://[::1]:9808");
        assert_eq!(url.host, "::1");
        assert_eq!(url.port, 9808);
        assert!(url.is_loopback());
    }

    #[test]
    fn missing_token_is_detected() {
        assert!(!has_token_param("http://127.0.0.1:9808/"));
        assert!(!has_token_param("http://127.0.0.1:9808/?token="));
        assert!(has_token_param("http://127.0.0.1:9808/?x=1&token=t"));
    }

    /// Regression: Tauri's own pages parse as ordinary http URLs, and treating
    /// one as a DSH origin stored `http://tauri.localhost:80` as a tab's origin.
    #[test]
    fn dshell_pages_are_not_dsh_origins() {
        let placeholder = LaunchUrl::parse("http://tauri.localhost/shell/placeholder.html").unwrap();
        assert!(placeholder.is_web_origin());
        assert!(placeholder.is_dshell_page());

        let dsh = LaunchUrl::parse("http://127.0.0.1:9808/?token=t").unwrap();
        assert!(dsh.is_web_origin());
        assert!(!dsh.is_dshell_page());
    }
}
