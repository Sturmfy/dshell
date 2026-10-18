//! Credential-free HTTP probing.
//!
//! The supervisor only ever asks two questions:
//!
//! * "is this really a DSH web server?" — an unauthenticated `GET /` must answer
//!   DSH's own **401 page**, identified by its exact opening words plus the
//!   headers that come with it. The status code alone proves nothing: every
//!   authenticated service on a machine answers 401, and the local-instance scan
//!   did once list this machine's Clash controller as "a dsh that wants a token".
//! * "does this instance actually serve the frontend?" — the token URL must
//!   answer **200** with the boot marker. DSH prints its readiness line even
//!   when the frontend bundle is missing, in which case the page is a 404 and
//!   stderr is empty. Believing the readiness line alone would report a broken
//!   install as "ready".
//!
//! No HTTP crate is pulled in for this: the requests are tiny and plaintext, and
//! a hand-rolled client cannot change behaviour under a dependency bump.

use std::time::Duration;

use crate::error::{Action, Failure, FailureCode};

/// Marker that only the real DSH frontend emits.
const BOOT_MARKER: &[u8] = b"__DSH_BOOT__";

/// Opening words of the page DSH serves to an unauthenticated request.
///
/// This is the only positive identity a credential-free probe can obtain. It is
/// a contract with the server, not a guess: `writeUnauthorized` in
/// `packages/client/connection/src/browser-auth.ts` emits exactly this sentence,
/// as `text/plain; charset=utf-8`, with `cache-control: no-store`.
const DSH_AUTH_REQUIRED: &str = "dsh web authentication required";

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    /// Lower-cased header names in arrival order. Duplicates are kept, because
    /// several `Set-Cookie` lines are normal and none of them may be dropped.
    pub headers: Vec<(String, String)>,
    pub location: Option<String>,
    /// The `name=value` pairs from `Set-Cookie`, without attributes.
    ///
    /// The token exchange answers 303 and the follow-up request is *only*
    /// authenticated by this cookie — a plain second GET gets 401 even with a
    /// perfectly valid token.
    pub cookies: Vec<String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).chars().take(4000).collect()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    Ready,
    ReadyWithNotice(String),
}

/// Verify that a just-started DSH instance really serves its UI.
///
/// This performs the token exchange as a side effect (DSH answers 303 and sets
/// the session cookie), which is free validation of two things at once — but the
/// cookie is not kept here: the content webview does its own exchange and keeps
/// its own cookie jar.
pub async fn verify_ready(origin: &str, token_url: &str, timeout: Duration) -> Result<ProbeOutcome, Failure> {
    // Step 1: exchange the token exactly like a browser would. DSH answers 303
    // with a `Set-Cookie`; anything else here means the token was not accepted.
    let token_query = token_url.find('?').map(|idx| &token_url[idx..]).unwrap_or("");
    let path = format!("/{token_query}");
    let first = match get(origin, &path, timeout, false).await {
        Ok(response) => response,
        Err(err) => {
            return Err(Failure::new(
                FailureCode::RemoteUnreachable,
                format!("DSH 已打印就绪 URL，但无法连上 {origin}"),
            )
            .with_detail(err.to_string()))
        }
    };

    let response = match first.status {
        303 | 302 => {
            // The cookie minted by the exchange is required by the follow-up
            // request; DSH binds the browser session to it and answers 401
            // without it.
            let location = first.location.clone().unwrap_or_else(|| "./".into());
            let next = resolve_location(origin, &location);
            let cookie = first.cookies.first().cloned();
            match get_with_cookie(origin, &next, cookie.as_deref(), timeout).await {
                Ok(response) => response,
                Err(err) => {
                    return Err(Failure::new(
                        FailureCode::RemoteUnreachable,
                        format!("跟随 token 兑换跳转失败：{err}"),
                    ))
                }
            }
        }
        _ => first,
    };

    match response.status {
        200 => {
            if contains_marker(&response.body) {
                return Ok(ProbeOutcome::Ready);
            }
            // 200 but no boot marker: almost always a static host serving some
            // other index.html on the same port.
            Err(Failure::new(
                FailureCode::AttachNotDsh,
                format!("{origin} 返回了 200，但页面不含 DSH 前端标记"),
            )
            .with_detail(truncate(&response.body_text(), 400)))
        }
        404 => Err(frontend_missing(origin, &response)),
        403 => Err(Failure::new(
            FailureCode::RemoteForbidden,
            "就绪探测被信任围栏拒绝（403）".to_string(),
        )
        .with_detail(format!(
            "若这是远端实例，请在服务端加 --trusted-host {}",
            crate::dsh::url::authority_of(origin)
        ))),
        401 => Err(Failure::new(
            FailureCode::AttachAuthRequired,
            "token 未被接受（401）：实例可能已重启，或该 URL 的 token 已失效",
        )
        .with_detail(
            "DSH 的启动 token 只在产生它的那个进程生命周期内有效。\
             若该实例被重启过，请重新复制它打印的启动 URL。"
                .to_string(),
        )),
        other => Err(Failure::new(
            FailureCode::Internal,
            format!("就绪探测失败：{origin} 返回 {other}"),
        )
        .with_detail(truncate(&response.body_text(), 600))),
    }
}

fn frontend_missing(origin: &str, response: &HttpResponse) -> Failure {
    let mut failure = Failure::new(
        FailureCode::FrontendMissing,
        "服务器已启动，但前端资源缺失（安装不完整 / 前端未构建）",
    )
    .with_detail(format!(
        "GET {origin}/?token=… → 404。\n\
         这通常意味着 <install>/node_modules/@deepseek-ai/dsh-web-frontend/dist/index.html 不存在。\n\
         DSH 在这种情况下照常打印就绪 URL、stderr 为空、进程存活。"
    ))
    .with_action(Action::primary("reinstate-runtime", "重新安装该运行时"))
    .with_action(Action::secondary("diagnostics", "查看诊断"));
    if let Some(location) = &response.location {
        failure = failure.with_detail(format!("location: {location}"));
    }
    failure
}

/// Identify a server by port alone: is it a DSH that wants a token?
///
/// This is the local-instance scan's only question, and it is asked without a
/// token — which is the point, and also the difficulty: the answer may not rest
/// on the status code. 401 is the most common answer in HTTP; on the machine
/// this rule was written for, a Clash controller and a DSH both said 401, and
/// only one of them was DSH. A genuine DSH answers with its own page, so three
/// independent facts have to agree before a port may be called an instance:
/// the status, the exact opening words, and the headers DSH sets beside them.
///
/// 403 is deliberately *not* accepted. DSH's trust fence answers a bare
/// `forbidden`, which is indistinguishable from any other program's 403; a real
/// DSH answers 401 — with the same page — even when the `Host` is untrusted.
///
/// Returns the status code that identified it.
pub async fn probe_dsh_server(origin: &str, timeout: Duration) -> Option<u16> {
    let parsed = crate::dsh::url::LaunchUrl::parse(origin)?;
    if parsed.scheme != "http" {
        return None;
    }
    let response = get(&parsed.origin, "/", timeout, false).await.ok()?;
    is_dsh_auth_required(&response).then_some(response.status)
}

/// Does this response carry DSH's unauthenticated page?
///
/// All three checks are required, and each rejects a different impostor: the
/// body rejects every other authenticated service, the content type rejects a
/// JSON API that happens to echo the sentence, and `no-store` rejects a cached
/// answer that a previous occupant of the port left behind.
fn is_dsh_auth_required(response: &HttpResponse) -> bool {
    if response.status != 401 {
        return false;
    }
    let text_plain = response
        .header("content-type")
        .is_some_and(|value| value.trim().to_ascii_lowercase().starts_with("text/plain"));
    let no_store = response
        .header("cache-control")
        .is_some_and(|value| value.to_ascii_lowercase().contains("no-store"));
    text_plain && no_store && response.body_text().starts_with(DSH_AUTH_REQUIRED)
}

/// Probe an instance dshell did not start.
pub async fn probe_remote(url: &str, timeout: Duration) -> Result<ProbeOutcome, Failure> {
    let Some(parsed) = crate::dsh::url::LaunchUrl::parse(url) else {
        return Err(Failure::new(
            FailureCode::AttachNotDsh,
            "无法解析该地址（需要 http://host:port/… 形式）",
        ));
    };
    if !parsed.has_token() {
        return Err(Failure::new(
            FailureCode::AttachAuthRequired,
            "URL 里没有 token 参数，无法建立会话",
        )
        .with_detail(
            "请在运行 dsh web 的终端里复制整行 `dsh web: http://…/?token=…`，\
             连 token 一起粘贴。"
                .to_string(),
        )
        .with_action(Action::primary("retry", "重新粘贴")));
    }

    let response = get(&parsed.origin, "/", timeout, true)
        .await
        .map_err(|err| classify_transport(&parsed.origin, &err))?;

    match response.status {
        // Same rule as the scan: attaching to a port that merely answers 401
        // would hand the user a webview stuck on another program's login page.
        401 if is_dsh_auth_required(&response) => Ok(ProbeOutcome::Ready),
        401 => Err(Failure::new(
            FailureCode::AttachNotDsh,
            format!("{} 返回了 401，但不是 DSH 的鉴权页", parsed.origin),
        )
        .with_detail(
            "该端口上可能是另一个需要鉴权的服务（例如代理或管理面板），\
             它同样会对未授权请求回 401。请在运行 dsh web 的终端里核对实际端口。"
                .to_string(),
        )
        .with_detail(truncate(&response.body_text(), 300))),
        403 => Err(remote_forbidden(&parsed)),
        200 => Err(Failure::new(
            FailureCode::AttachNotDsh,
            format!("{} 无需鉴权就返回了 200，这不是 DSH web 服务", parsed.origin),
        )
        .with_detail(
            "该端口可能被其他程序占用。请在运行 dsh web 的终端里核对实际端口。".to_string(),
        )
        .with_detail(truncate(&response.body_text(), 300))),
        404 => Err(Failure::new(
            FailureCode::AttachNotDsh,
            format!("{} 返回 404，不像 DSH web 服务", parsed.origin),
        )
        .with_detail(truncate(&response.body_text(), 300))),
        other => Err(Failure::new(
            FailureCode::AttachNotDsh,
            format!("{} 返回了意外的状态码 {other}", parsed.origin),
        )
        .with_detail(truncate(&response.body_text(), 300))),
    }
}

fn remote_forbidden(parsed: &crate::dsh::url::LaunchUrl) -> Failure {
    let authority = crate::dsh::url::authority_of(&parsed.origin);
    Failure::new(
        FailureCode::RemoteForbidden,
        format!("服务端信任围栏拒绝了本机（authority {}）", authority),
    )
    .with_detail(format!(
        "在服务端机器上改用：\n\n  dsh web --host {} --trusted-host {}\n",
        parsed.host, authority
    ))
    .with_action(Action::primary("copy-fix-command", "复制修复命令"))
    .with_action(Action::secondary("retry", "重新检测"))
}

fn classify_transport(origin: &str, err: &std::io::Error) -> Failure {
    let message = match err.kind() {
        std::io::ErrorKind::ConnectionRefused => {
            format!("连接被拒绝：{origin} 上没有服务在监听（或防火墙拦截）")
        }
        std::io::ErrorKind::TimedOut => {
            format!("连接超时：{origin} 不可达（检查 IP、网络与防火墙）")
        }
        _ => format!("无法连接 {origin}：{err}"),
    };
    Failure::new(FailureCode::RemoteUnreachable, message)
        .with_detail(err.to_string())
        .with_action(Action::secondary("retry", "重新检测"))
}

/// Minimal HTTP GET.
///
/// * `follow_redirects = false` keeps the raw status — 401/403/404 are the
///   answers we care about.
/// * Only `http://` is supported: DSH's TLS flags are unreleased and the design
///   is explicitly plaintext HTTP until they ship.
async fn get(
    origin: &str,
    path: &str,
    timeout: Duration,
    follow_redirects: bool,
) -> std::io::Result<HttpResponse> {
    get_inner(origin, path, None, timeout, follow_redirects).await
}

async fn get_with_cookie(
    origin: &str,
    path: &str,
    cookie: Option<&str>,
    timeout: Duration,
) -> std::io::Result<HttpResponse> {
    get_inner(origin, path, cookie, timeout, false).await
}

async fn get_inner(
    origin: &str,
    path: &str,
    cookie: Option<&str>,
    timeout: Duration,
    follow_redirects: bool,
) -> std::io::Result<HttpResponse> {
    let parsed = crate::dsh::url::LaunchUrl::parse(origin)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid url"))?;
    if parsed.scheme != "http" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "仅为 http 实现了探测；HTTPS 实例请直接在页面中打开",
        ));
    }

    let target = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };

    // `parsed.host` is the bare host (IPv6 literals lose their brackets in
    // `LaunchUrl::parse`) and `parsed.port` the port, so the authority is
    // assembled exactly once here — bracketing a v6 literal again, because a
    // bare `::1:9808` is not a valid authority.
    let host_header = if parsed.host.contains(':') {
        format!("[{}]:{}", parsed.host, parsed.port)
    } else {
        format!("{}:{}", parsed.host, parsed.port)
    };
    let mut request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host_header}\r\nAccept: text/html,*/*\r\n"
    );
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");

    let response = tokio::time::timeout(timeout, async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect((parsed.host.as_str(), parsed.port)).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
        let mut buf = Vec::with_capacity(16 * 1024);
        // Cap the read: the frontend index.html is a few KB, and a hostile peer
        // should not be able to make us allocate without bound.
        let mut chunk = [0u8; 8192];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.len() > 1 << 20 {
                break;
            }
        }
        Ok::<Vec<u8>, std::io::Error>(buf)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "request timed out"))??;

    let response = parse_response(&response)?;

    if follow_redirects {
        if let (true, Some(location)) = ((300..400).contains(&response.status), response.location.clone())
        {
            let next = resolve_location(&parsed.origin, &location);
            let cookie = response.cookies.first().cloned();
            return Box::pin(get_inner(&parsed.origin, &next, cookie.as_deref(), timeout, false)).await;
        }
    }
    Ok(response)
}

fn resolve_location(origin: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    if let Some(rest) = location.strip_prefix("./") {
        return format!("/{rest}");
    }
    if location.starts_with('/') {
        return location.to_string();
    }
    let _ = origin;
    format!("/{location}")
}

fn parse_response(raw: &[u8]) -> std::io::Result<HttpResponse> {
    let split = find_subslice(raw, b"\r\n\r\n")
        .map(|idx| (idx, idx + 4))
        .or_else(|| find_subslice(raw, b"\n\n").map(|idx| (idx, idx + 2)))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed response"))?;

    let head = String::from_utf8_lossy(&raw[..split.0]).to_string();
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no status code"))?;

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut location = None;
    let mut cookies = Vec::new();
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "location" {
            location = Some(value.to_string());
        }
        if name == "set-cookie" {
            // Keep only `name=value`; the attributes are not ours to replay.
            if let Some(pair) = value.split(';').next() {
                if pair.contains('=') {
                    cookies.push(pair.trim().to_string());
                }
            }
        }
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
        headers.push((name, value.to_string()));
    }

    let body = &raw[split.1..];
    let body = if chunked { dechunk(body) } else { body.to_vec() };
    Ok(HttpResponse { status, headers, location, cookies, body })
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(idx) = find_subslice(rest, b"\r\n") {
        let size_text = String::from_utf8_lossy(&rest[..idx]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        let start = idx + 2;
        let end = (start + size).min(rest.len());
        out.extend_from_slice(&rest[start..end]);
        if end + 2 > rest.len() {
            break;
        }
        rest = &rest[end + 2..];
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn contains_marker(body: &[u8]) -> bool {
    find_subslice(body, BOOT_MARKER).is_some()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "\n…（已截断）"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_location() {
        let raw = b"HTTP/1.1 303 See Other\r\nLocation: ./\r\nSet-Cookie: a=b; Max-Age=1; HttpOnly; SameSite=Strict\r\n\r\nbody";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 303);
        assert_eq!(response.location.as_deref(), Some("./"));
        assert_eq!(response.cookies, vec!["a=b".to_string()]);
        assert_eq!(response.body_text(), "body");
    }

    /// Regression: DSH's follow-up request is authenticated only by the cookie
    /// the token exchange set, so dropping it turns a valid instance into a 401.
    #[test]
    fn keeps_every_set_cookie_pair() {
        let raw = b"HTTP/1.1 303 See Other\r\nSet-Cookie: dsh-auth-x=v1.abc.def; Path=/; HttpOnly\r\nSet-Cookie: other=1\r\n\r\n";
        let response = parse_response(raw).unwrap();
        assert_eq!(
            response.cookies,
            vec!["dsh-auth-x=v1.abc.def".to_string(), "other=1".to_string()]
        );
    }

    #[test]
    fn dechunks_transfer_encoded_bodies() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.body_text(), "hello");
    }

    #[test]
    fn boot_marker_detection_is_exact() {
        assert!(contains_marker(b"<script>window.__DSH_BOOT__={}</script>"));
        assert!(!contains_marker(b"<html></html>"));
    }

    /// The real DSH page, byte for byte as `browser-auth.ts` writes it.
    #[test]
    fn identifies_dsh_by_its_unauthenticated_page() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\
                    content-type: text/plain; charset=utf-8\r\n\
                    cache-control: no-store\r\n\
                    \r\n\
                    dsh web authentication required; reopen the URL printed by dsh web.\n";
        let response = parse_response(raw).unwrap();
        assert!(is_dsh_auth_required(&response));
        assert_eq!(response.status, 401);
    }

    /// Regression: the Clash controller on this machine answers
    /// `401 application/json {"message":"Unauthorized"}`. The scan offered it to
    /// the user as "a dsh that wants the token it printed at startup".
    #[test]
    fn rejects_a_json_401_from_another_service() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\
                    content-type: application/json\r\n\
                    \r\n\
                    {\"message\":\"Unauthorized\"}\n";
        let response = parse_response(raw).unwrap();
        assert!(!is_dsh_auth_required(&response));
    }

    /// A bare 403 is what DSH's trust fence writes, and also what a dozen other
    /// programs write. It is not evidence, so it is not accepted.
    #[test]
    fn rejects_bare_403() {
        let raw = b"HTTP/1.1 403 Forbidden\r\n\r\nforbidden";
        let response = parse_response(raw).unwrap();
        assert!(!is_dsh_auth_required(&response));
    }

    /// Each of the three facts has to hold on its own: a page that copies the
    /// sentence but is served as JSON, or without `no-store`, is not DSH.
    #[test]
    fn every_part_of_the_signature_is_required() {
        let no_cache_control = b"HTTP/1.1 401 Unauthorized\r\n\
                                 content-type: text/plain; charset=utf-8\r\n\
                                 \r\n\
                                 dsh web authentication required; reopen the URL printed by dsh web.\n";
        assert!(!is_dsh_auth_required(&parse_response(no_cache_control).unwrap()));

        let wrong_type = b"HTTP/1.1 401 Unauthorized\r\n\
                           content-type: text/html; charset=utf-8\r\n\
                           cache-control: no-store\r\n\
                           \r\n\
                           dsh web authentication required; reopen the URL printed by dsh web.\n";
        assert!(!is_dsh_auth_required(&parse_response(wrong_type).unwrap()));

        let truncated = b"HTTP/1.1 401 Unauthorized\r\n\
                          content-type: text/plain; charset=utf-8\r\n\
                          cache-control: no-store\r\n\
                          \r\n\
                          unauthorized\n";
        assert!(!is_dsh_auth_required(&parse_response(truncated).unwrap()));
    }

    #[test]
    fn relative_location_is_resolved() {
        assert_eq!(resolve_location("http://h:1", "./"), "/");
        assert_eq!(resolve_location("http://h:1", "/ui/"), "/ui/");
    }
}
