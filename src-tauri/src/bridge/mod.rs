//! Narrow page-to-shell bridge for DSH content tabs.
//!
//! DSH reads two optional page globals:
//!
//! * `__DSH_DIRECTORY_PICKER__` for a workspace-folder chooser;
//! * `__DSH_HOST_PATHS__` for the real path of a dropped or picked file.
//!
//! On Windows the Host-owned native chooser is a separate, unowned top-level
//! window, which can open behind dshell; and WebView2 exposes no
//! `getPathForFile` equivalent to a plain browser. This module supplies both
//! through the WebView2 message channel instead of granting the content
//! webview a Tauri capability:
//!
//! * `content.js` is injected into every content webview at document start;
//! * the page posts JSON (and, for host paths, the `File` objects as
//!   additional objects) through `window.chrome.webview`;
//! * the host validates the sender's origin against the tab's DSH origin and
//!   answers with `PostWebMessageAsJson`.
//!
//! No request type can read an arbitrary path: `hostPaths` only returns the
//! path of a `File` object the page already holds, via
//! `ICoreWebView2File::Path`.
//!
//! ## Other platforms
//!
//! The transport above is WebView2-specific and lives in [`windows`]. On Linux
//! and macOS no script is injected at all, so the two globals stay undefined and
//! DSH falls back to the Host's own directory chooser and its browser upload
//! path (`packages/client/ui-directory-picker-native` and `ui-conversation` are
//! written for exactly that fallback). The trade-off is recorded in
//! `docs/platforms.md`: the native folder chooser and real-path `@file` chips
//! are Windows-only for now. That is deliberate — a half-tested WebKit message
//! transport would be worse than a documented fallback DSH already supports.

#[cfg(any(windows, test))]
use serde::Deserialize;
#[cfg(any(windows, test))]
use serde_json::Value;

#[cfg(windows)]
mod windows;

/// The script injected into content tabs, before any page script runs.
pub const SCRIPT: &str = include_str!("content.js");

/// Wire version of the page/host conversation.
pub const PROTOCOL_VERSION: u8 = 1;

/// The injected script, or `None` where the bridge is not implemented.
///
/// `None` is not a degraded mode dshell has to fake: DSH treats the missing
/// globals as "use the Host's own chooser", which is the pre-desktop-app path.
pub fn script() -> Option<&'static str> {
    #[cfg(windows)]
    {
        Some(SCRIPT)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// One page request. Unknown shapes are ignored by the caller.
#[cfg(any(windows, test))]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum BridgeRequest {
    /// Open a native folder chooser owned by dshell's main window.
    PickDirectory {
        v: u8,
        id: String,
        #[serde(default)]
        initial: Option<String>,
    },
    /// Resolve the on-disk paths of the `File` objects sent with the message.
    HostPaths {
        v: u8,
        id: String,
        keys: Vec<String>,
    },
    /// Feature announcement used only for diagnostics.
    Ready {
        v: u8,
        #[serde(default)]
        features: Value,
    },
    /// DSH's current sidebar surface, used by the chrome strip to style the
    /// active tab without reading the remote page's DOM itself.
    Theme {
        v: u8,
        background: String,
        foreground: String,
        mode: String,
    },
    /// Counts of the session-row states DSH already paints: error, waiting for
    /// the user, finished-but-unread, and running.
    SessionAttention {
        v: u8,
        #[serde(default)]
        error: u32,
        #[serde(default, rename = "needsInput")]
        needs_input: u32,
        #[serde(default)]
        completed: u32,
        #[serde(default)]
        running: u32,
    },
}

#[cfg(any(windows, test))]
impl BridgeRequest {
    fn version(&self) -> u8 {
        match self {
            Self::PickDirectory { v, .. }
            | Self::HostPaths { v, .. }
            | Self::Ready { v, .. }
            | Self::Theme { v, .. }
            | Self::SessionAttention { v, .. } => *v,
        }
    }
}

/// The per-content-webview bridge. Cloning is cheap and keeps the handler able
/// to post after the original manager entry is gone.
///
/// On platforms without a transport this is a zero-sized no-op: the tab manager
/// can keep calling it unconditionally, and DSH's own fallbacks take over.
#[derive(Clone)]
pub struct ContentBridge {
    #[cfg(windows)]
    inner: windows::Bridge,
}

impl ContentBridge {
    /// Register the platform message handler on one content webview.
    ///
    /// On Windows, failure is non-fatal: the tab still works and file uploads
    /// continue. The injected script never receives `hello`, so a picker request
    /// fails fast instead of leaving DSH waiting on a dialog that cannot exist.
    pub fn attach(
        app: tauri::AppHandle,
        webview: &tauri::Webview,
        label: String,
        title: Option<String>,
    ) -> Result<Self, String> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::Bridge::attach(app, webview, label, title)?,
            })
        }
        #[cfg(not(windows))]
        {
            // DSH falls back to the Host's chooser when the globals are absent;
            // nothing to attach to.
            let _ = (app, webview, label, title);
            Ok(Self {})
        }
    }

    /// Record the tab's current DSH origin. Messages from any other origin are
    /// rejected, including after a main-frame navigation away from DSH.
    pub fn set_origin(&self, origin: &str) {
        #[cfg(windows)]
        self.inner.set_origin(origin);
        #[cfg(not(windows))]
        let _ = origin;
    }

    /// Store the custom tab name and tell the page to paint it over DSH's
    /// top-left brand. Called on rename; page loads get it through `hello`.
    pub fn set_title(&self, title: &str) {
        #[cfg(windows)]
        self.inner.set_title(title);
        #[cfg(not(windows))]
        let _ = title;
    }
}

/// Colors cross the bridge into a CSS custom property, so accept only the
/// computed forms DSH actually produces (never arbitrary CSS text).
pub(crate) fn is_safe_css_color(value: &str) -> bool {
    let value = value.trim();
    value.len() <= 64
        && (value.starts_with('#')
            || value.starts_with("rgb(")
            || value.starts_with("rgba(")
            || value.starts_with("hsl(")
            || value.starts_with("hsla("))
}

#[cfg(any(windows, test))]
pub(crate) fn parse_request(raw: &str) -> Option<BridgeRequest> {
    let request: BridgeRequest = serde_json::from_str(raw).ok()?;
    (request.version() == PROTOCOL_VERSION).then_some(request)
}

/// Whether `source` is the same scheme/host/port as the tab's DSH origin.
#[cfg(any(windows, test))]
pub(crate) fn same_origin(source: &str, expected: &str) -> bool {
    let (Ok(source), Ok(expected)) = (tauri::Url::parse(source), tauri::Url::parse(expected))
    else {
        return false;
    };
    source.scheme() == expected.scheme()
        && source.host_str() == expected.host_str()
        && source.port_or_known_default() == expected.port_or_known_default()
}

/// Keep the response aligned with the request keys; extra files are ignored.
#[cfg(any(windows, test))]
pub(crate) fn align_paths(keys: &[String], mut paths: Vec<Option<String>>) -> Vec<Option<String>> {
    paths.truncate(keys.len());
    paths.resize(keys.len(), None);
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_requests_and_ready() {
        let pick: BridgeRequest =
            parse_request(r#"{"v":1,"type":"pickDirectory","id":"a","initial":null}"#)
                .expect("pick request");
        assert!(matches!(pick, BridgeRequest::PickDirectory { .. }));

        let paths: BridgeRequest =
            parse_request(r#"{"v":1,"type":"hostPaths","id":"b","keys":["k"]}"#)
                .expect("host paths request");
        assert!(matches!(paths, BridgeRequest::HostPaths { .. }));

        let ready: BridgeRequest =
            parse_request(r#"{"v":1,"type":"ready","features":{"hostPaths":true}}"#)
                .expect("ready request");
        assert!(matches!(ready, BridgeRequest::Ready { .. }));

        let theme: BridgeRequest = parse_request(
            r#"{"v":1,"type":"theme","background":"rgb(27, 27, 28)","foreground":"rgb(249, 250, 251)","mode":"dark"}"#,
        )
        .expect("theme request");
        assert!(matches!(theme, BridgeRequest::Theme { .. }));

        let attention: BridgeRequest = parse_request(
            r#"{"v":1,"type":"sessionAttention","error":0,"needsInput":2,"completed":1,"running":0}"#,
        )
        .expect("session attention request");
        match attention {
            BridgeRequest::SessionAttention {
                needs_input,
                completed,
                ..
            } => {
                assert_eq!(needs_input, 2);
                assert_eq!(completed, 1);
            }
            _ => panic!("expected session attention"),
        }
    }

    #[test]
    fn accepts_only_computed_css_colors() {
        assert!(is_safe_css_color("rgb(27, 27, 28)"));
        assert!(is_safe_css_color("rgba(0, 0, 0, 0.5)"));
        assert!(is_safe_css_color("#1b1b1c"));
        assert!(!is_safe_css_color("red; background: url(x)"));
        assert!(!is_safe_css_color("var(--x)"));
        assert!(!is_safe_css_color(""));
    }

    #[test]
    fn rejects_other_protocol_versions_and_malformed_payloads() {
        assert!(parse_request(r#"{"v":2,"type":"pickDirectory","id":"a"}"#).is_none());
        assert!(parse_request(r#"{"v":1,"type":"unknown","id":"a"}"#).is_none());
        assert!(parse_request("not json").is_none());
    }

    #[test]
    fn compares_scheme_host_and_port_only() {
        assert!(same_origin(
            "http://127.0.0.1:24565/",
            "http://127.0.0.1:24565"
        ));
        assert!(same_origin(
            "http://localhost:24565/sessions/1",
            "http://localhost:24565"
        ));
        assert!(!same_origin(
            "http://127.0.0.1:24566/",
            "http://127.0.0.1:24565"
        ));
        assert!(!same_origin(
            "https://127.0.0.1:24565/",
            "http://127.0.0.1:24565"
        ));
        assert!(!same_origin(
            "https://example.com/",
            "http://127.0.0.1:24565"
        ));
        assert!(!same_origin("not a url", "http://127.0.0.1:24565"));
    }

    #[test]
    fn aligns_paths_with_keys() {
        assert_eq!(
            align_paths(
                &["a".into(), "b".into(), "c".into()],
                vec![Some("A".into())]
            ),
            vec![Some("A".into()), None, None]
        );
        assert_eq!(
            align_paths(&["a".into()], vec![Some("A".into()), Some("B".into())]),
            vec![Some("A".into())]
        );
        assert_eq!(
            align_paths(&[], vec![Some("A".into())]),
            Vec::<Option<String>>::new()
        );
    }
}
