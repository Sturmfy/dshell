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

use std::sync::{Arc, Mutex, RwLock};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Webview};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2AcceleratorKeyPressedEventArgs,
    ICoreWebView2ContextMenuItemCollection, ICoreWebView2ContextMenuRequestedEventArgs,
    ICoreWebView2File, ICoreWebView2NavigationStartingEventArgs3,
    ICoreWebView2WebMessageReceivedEventArgs, ICoreWebView2WebMessageReceivedEventArgs2,
    ICoreWebView2_11, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND,
    COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SUBMENU, COREWEBVIEW2_KEY_EVENT_KIND,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
    COREWEBVIEW2_NAVIGATION_KIND, COREWEBVIEW2_NAVIGATION_KIND_BACK_OR_FORWARD,
};
use webview2_com::{
    take_pwstr, AcceleratorKeyPressedEventHandler, ContextMenuRequestedEventHandler,
    NavigationStartingEventHandler, WebMessageReceivedEventHandler,
};
use windows_core::{Interface, HSTRING, PWSTR};

/// The script injected into content tabs, before any page script runs.
pub const SCRIPT: &str = include_str!("content.js");

/// Wire version of the page/host conversation.
const PROTOCOL_VERSION: u8 = 1;

/// One page request. Unknown shapes are ignored by the caller.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum BridgeRequest {
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

/// The per-content-webview bridge. Cloning is cheap and keeps the handler
/// able to post after the original manager entry is gone.
#[derive(Clone)]
pub struct ContentBridge {
    app: AppHandle,
    label: String,
    origin: Arc<RwLock<Option<String>>>,
    /// The user's custom tab name, when there is one. It is sent on every page
    /// ready so a reload gets the brand replacement too, and re-sent on rename.
    title: Arc<RwLock<Option<String>>>,
}

impl ContentBridge {
    /// Register the WebView2 message handler on one content webview.
    ///
    /// Failure is non-fatal: the tab still works and file uploads continue.
    /// The injected script never receives `hello`, so a picker request fails
    /// fast instead of leaving DSH waiting on a dialog that cannot exist.
    pub fn attach(
        app: AppHandle,
        webview: &Webview,
        label: String,
        title: Option<String>,
    ) -> Result<Self, String> {
        let bridge = Self {
            app,
            label,
            origin: Arc::new(RwLock::new(None)),
            title: Arc::new(RwLock::new(title)),
        };
        let handler_bridge = bridge.clone();
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot = error.clone();
        webview
            .with_webview(move |platform| {
                let result = (|| -> Result<(), String> {
                    let controller = platform.controller();
                    let core = unsafe { controller.CoreWebView2() }
                        .map_err(|err| format!("CoreWebView2: {err}"))?;

                    // DSH is an app, not a browser page, but its context menu
                    // is the browser's menu: filtering three browser-only
                    // commands keeps Copy/Paste and the link/image actions,
                    // which a hand-built menu would lose.
                    let context_menu =
                        ContextMenuRequestedEventHandler::create(Box::new(|_sender, args| {
                            if let Some(args) = args {
                                hide_browser_context_items(&args);
                            }
                            Ok(())
                        }));
                    let mut context_menu_token = 0;
                    if let Ok(context_core) = core.cast::<ICoreWebView2_11>() {
                        let _ = unsafe {
                            context_core
                                .add_ContextMenuRequested(&context_menu, &mut context_menu_token)
                        };
                    }

                    let navigation =
                        NavigationStartingEventHandler::create(Box::new(|_sender, args| {
                            if let Some(args) = args {
                                if let Ok(args) =
                                    args.cast::<ICoreWebView2NavigationStartingEventArgs3>()
                                {
                                    let mut kind = COREWEBVIEW2_NAVIGATION_KIND::default();
                                    if unsafe { args.NavigationKind(&mut kind) }.is_ok()
                                        && kind == COREWEBVIEW2_NAVIGATION_KIND_BACK_OR_FORWARD
                                    {
                                        let _ = unsafe { args.SetCancel(true) };
                                    }
                                }
                            }
                            Ok(())
                        }));
                    let mut navigation_token = 0;
                    unsafe { core.add_NavigationStarting(&navigation, &mut navigation_token) }
                        .map_err(|err| format!("add_NavigationStarting: {err}"))?;

                    let accelerator =
                        AcceleratorKeyPressedEventHandler::create(Box::new(|_sender, args| {
                            if let Some(args) = args {
                                suppress_print_accelerator(&args);
                            }
                            Ok(())
                        }));
                    let mut accelerator_token = 0;
                    unsafe {
                        controller.add_AcceleratorKeyPressed(&accelerator, &mut accelerator_token)
                    }
                    .map_err(|err| format!("add_AcceleratorKeyPressed: {err}"))?;

                    let handler =
                        WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
                            if let (Some(sender), Some(args)) = (sender, args) {
                                handler_bridge.on_message(&sender, &args);
                            }
                            Ok(())
                        }));
                    let mut token = 0;
                    unsafe { core.add_WebMessageReceived(&handler, &mut token) }
                        .map_err(|err| format!("add_WebMessageReceived: {err}"))?;
                    Ok(())
                })();
                if let Err(err) = result {
                    if let Ok(mut slot) = error_slot.lock() {
                        *slot = Some(err);
                    }
                }
            })
            .map_err(|err| format!("with_webview: {err}"))?;
        let failure = error.lock().ok().and_then(|mut slot| slot.take());
        match failure {
            Some(err) => Err(err),
            None => Ok(bridge),
        }
    }

    /// Record the tab's current DSH origin. Messages from any other origin are
    /// rejected, including after a main-frame navigation away from DSH.
    pub fn set_origin(&self, origin: &str) {
        if let Ok(mut slot) = self.origin.write() {
            *slot = Some(origin.to_string());
        }
    }

    fn on_message(&self, sender: &ICoreWebView2, args: &ICoreWebView2WebMessageReceivedEventArgs) {
        let source = unsafe {
            let mut raw = windows_core::PWSTR::null();
            match args.Source(&mut raw) {
                Ok(()) => take_pwstr(raw),
                Err(_) => String::new(),
            }
        };
        let raw = read_message(args);
        let Some(request) = parse_request(&raw) else {
            return;
        };
        // `ready` is the handshake, not a capability request: answer it before
        // the origin fence so a picker cannot be left looking stuck just because
        // the launch origin landed after the first document script.
        if let BridgeRequest::Ready { features, .. } = &request {
            crate::dsh::process::debug_log(
                &self.data_root(),
                &format!("content bridge ready on {}: {features}", self.label),
            );
            let mut hello = json!({ "v": PROTOCOL_VERSION, "type": "hello" });
            if let Some(title) = self.title.read().ok().and_then(|slot| slot.clone()) {
                hello["title"] = json!(title);
            }
            post_json(sender, &hello);
            return;
        }
        if !self.origin_allows(&source) {
            if let BridgeRequest::PickDirectory { id, .. } = &request {
                post_json(
                    sender,
                    &pick_result_error(id, "bridge origin is not the current DSH tab"),
                );
            }
            return;
        }
        match request {
            BridgeRequest::PickDirectory { id, initial, .. } => self.start_pick(id, initial),
            BridgeRequest::HostPaths { id, keys, .. } => {
                let paths = additional_paths(args);
                crate::dsh::process::debug_log(
                    &self.data_root(),
                    &format!(
                        "content bridge hostPaths on {}: keys={} objects={}",
                        self.label,
                        keys.len(),
                        paths.len()
                    ),
                );
                let aligned = align_paths(&keys, paths);
                post_json(
                    sender,
                    &json!({
                        "v": PROTOCOL_VERSION,
                        "type": "hostPathsResult",
                        "id": id,
                        "paths": aligned,
                    }),
                );
            }
            BridgeRequest::Ready { .. } => {}
            BridgeRequest::Theme {
                background,
                foreground,
                mode,
                ..
            } => self.report_theme(&background, &foreground, &mode),
            BridgeRequest::SessionAttention {
                error,
                needs_input,
                completed,
                running,
                ..
            } => self.report_attention(error, needs_input, completed, running),
        }
    }

    /// Store the custom tab name and tell the page to paint it over DSH's
    /// top-left brand. Called on rename; page loads get it through `hello`.
    pub fn set_title(&self, title: &str) {
        let title = title.trim();
        if let Ok(mut slot) = self.title.write() {
            *slot = (!title.is_empty()).then(|| title.to_string());
        }
        self.post(&json!({
            "v": PROTOCOL_VERSION,
            "type": "title",
            "title": title,
        }));
    }

    /// Hand the page's session counts to the tab manager.
    ///
    /// The WebView2 message callback is synchronous, and the manager needs
    /// async locks, so the update is spawned. Counts are bounded here: the page
    /// is trusted only to describe its own UI, never to allocate unbounded
    /// labels or numbers.
    fn report_attention(&self, error: u32, needs_input: u32, completed: u32, running: u32) {
        const LIMIT: u32 = 10_000;
        if [error, needs_input, completed, running]
            .iter()
            .any(|count| *count > LIMIT)
        {
            return;
        }
        let tab_id = self
            .label
            .strip_prefix("tab-")
            .unwrap_or(&self.label)
            .to_string();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<crate::state::AppState>>() {
                state
                    .manager
                    .set_session_attention(
                        &tab_id,
                        crate::tabs::state::SessionAttention {
                            error,
                            needs_input,
                            completed,
                            running,
                        },
                    )
                    .await;
            }
        });
    }

    fn report_theme(&self, background: &str, foreground: &str, mode: &str) {
        if !matches!(mode, "dark" | "light")
            || !is_safe_css_color(background)
            || !is_safe_css_color(foreground)
        {
            return;
        }
        let tab_id = self.label.strip_prefix("tab-").unwrap_or(&self.label);
        let _ = self.app.emit(
            crate::TAB_THEME_EVENT,
            json!({
                "tabId": tab_id,
                "background": background,
                "foreground": foreground,
                "mode": mode,
            }),
        );
    }

    /// Run the picker off the WebView2 thread and answer the page when it
    /// settles. The picker serialises itself, so a second tab gets a readable
    /// busy error instead of an invisible wait.
    fn start_pick(&self, id: String, initial: Option<String>) {
        crate::dsh::process::debug_log(
            &self.data_root(),
            &format!("content bridge pick requested on {}", self.label),
        );
        let app = self.app.clone();
        let bridge = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = crate::picker::pick_directory(&app, initial, "选择工作区文件夹").await;
            let payload = match result {
                Ok(path) => json!({
                    "v": PROTOCOL_VERSION,
                    "type": "pickDirectoryResult",
                    "id": id,
                    "ok": true,
                    "path": path,
                }),
                Err(err) => pick_result_error(&id, &err.message),
            };
            bridge.post(&payload);
        });
    }

    fn post(&self, payload: &Value) {
        let Some(webview) = self.app.get_webview(&self.label) else {
            return;
        };
        let json = payload.to_string();
        let _ = webview.with_webview(move |platform| {
            let Ok(core) = (unsafe { platform.controller().CoreWebView2() }) else {
                return;
            };
            let _ = unsafe { core.PostWebMessageAsJson(&HSTRING::from(json)) };
        });
    }

    fn origin_allows(&self, source: &str) -> bool {
        let Some(expected) = self.origin.read().ok().and_then(|slot| slot.clone()) else {
            return false;
        };
        same_origin(source, &expected)
    }

    fn data_root(&self) -> std::path::PathBuf {
        self.app
            .try_state::<Arc<crate::state::AppState>>()
            .map(|state| state.data_root.path.clone())
            .unwrap_or_default()
    }
}

fn pick_result_error(id: &str, message: &str) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "type": "pickDirectoryResult",
        "id": id,
        "ok": false,
        "error": message,
    })
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

/// Remove the browser-only commands this app has deliberately closed. The
/// rest of WebView2's menu (editing, links, images, save, inspect) stays.
fn hide_browser_context_items(args: &ICoreWebView2ContextMenuRequestedEventArgs) {
    let Ok(target) = (unsafe { args.ContextMenuTarget() }) else {
        let _ = unsafe { args.SetHandled(true) };
        return;
    };
    let mut editable = windows_core::BOOL::default();
    if unsafe { target.IsEditable(&mut editable) }.is_err() || !editable.as_bool() {
        // DSH's app surface is not a browser document: only real editable
        // fields keep a context menu, so the rest of the page does not fall
        // back to WebView2's page/link/image commands.
        let _ = unsafe { args.SetHandled(true) };
        return;
    }
    let Ok(items) = (unsafe { args.MenuItems() }) else {
        return;
    };
    remove_browser_context_items(&items);
}

fn remove_browser_context_items(items: &ICoreWebView2ContextMenuItemCollection) {
    let mut count = 0;
    if unsafe { items.Count(&mut count) }.is_err() {
        return;
    }
    for index in (0..count).rev() {
        let Ok(item) = (unsafe { items.GetValueAtIndex(index) }) else {
            continue;
        };
        let mut raw = PWSTR::null();
        if unsafe { item.Name(&mut raw) }.is_ok() {
            let name = take_pwstr(raw);
            if matches!(name.as_str(), "back" | "forward" | "print") {
                let _ = unsafe { items.RemoveValueAtIndex(index) };
                continue;
            }
        }
        let mut kind = COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND::default();
        if unsafe { item.Kind(&mut kind) }.is_ok()
            && kind == COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SUBMENU
        {
            if let Ok(children) = unsafe { item.Children() } {
                remove_browser_context_items(&children);
            }
        }
    }
}

/// Browser Print is the one accelerator this app suppresses; Refresh and Find
/// are allowed by request, and the remaining browser commands either do
/// nothing in WebView2 or have been removed from the context menu.
fn suppress_print_accelerator(args: &ICoreWebView2AcceleratorKeyPressedEventArgs) {
    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
    if unsafe { args.KeyEventKind(&mut kind) }.is_err()
        || (kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
            && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN)
    {
        return;
    }
    let mut key = 0u32;
    if unsafe { args.VirtualKey(&mut key) }.is_err() || key != 0x50 {
        return;
    }
    if control_pressed() {
        let _ = unsafe { args.SetHandled(true) };
    }
}

fn control_pressed() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetKeyState(virtual_key: i32) -> i16;
    }
    const VK_CONTROL: i32 = 0x11;
    unsafe { GetKeyState(VK_CONTROL) < 0 }
}

/// Reply on the event callback's own WebView2 interface. This runs on the
/// WebView2 thread and deliberately avoids another `with_webview` dispatch.
fn post_json(core: &ICoreWebView2, payload: &Value) {
    let _ = unsafe { core.PostWebMessageAsJson(&HSTRING::from(payload.to_string())) };
}

fn parse_request(raw: &str) -> Option<BridgeRequest> {
    let request: BridgeRequest = serde_json::from_str(raw).ok()?;
    (request.version() == PROTOCOL_VERSION).then_some(request)
}

/// Read a page message in either wire form.
///
/// `content.js` posts a string today (works with `TryGetWebMessageAsString`);
/// the JSON fallback keeps a future object-shaped post working too.
fn read_message(args: &ICoreWebView2WebMessageReceivedEventArgs) -> String {
    let mut raw = windows_core::PWSTR::null();
    if unsafe { args.TryGetWebMessageAsString(&mut raw) }.is_ok() {
        return take_pwstr(raw);
    }
    let mut raw = windows_core::PWSTR::null();
    if unsafe { args.WebMessageAsJson(&mut raw) }.is_ok() {
        let json = take_pwstr(raw);
        if let Ok(Value::String(inner)) = serde_json::from_str(&json) {
            return inner;
        }
        return json;
    }
    String::new()
}

/// Whether `source` is the same scheme/host/port as the tab's DSH origin.
fn same_origin(source: &str, expected: &str) -> bool {
    let (Ok(source), Ok(expected)) = (tauri::Url::parse(source), tauri::Url::parse(expected))
    else {
        return false;
    };
    source.scheme() == expected.scheme()
        && source.host_str() == expected.host_str()
        && source.port_or_known_default() == expected.port_or_known_default()
}

/// Read the paths of the `File` objects attached to a WebView2 message.
///
/// A missing `AdditionalObjects` collection, a non-file object, or a failed
/// `Path` call yields `None`; DSH then uploads that file as before.
fn additional_paths(args: &ICoreWebView2WebMessageReceivedEventArgs) -> Vec<Option<String>> {
    let Ok(args) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>() else {
        return Vec::new();
    };
    let Ok(collection) = (unsafe { args.AdditionalObjects() }) else {
        return Vec::new();
    };
    let mut count = 0u32;
    if unsafe { collection.Count(&mut count) }.is_err() {
        return Vec::new();
    }
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let Ok(object) = (unsafe { collection.GetValueAtIndex(index) }) else {
            paths.push(None);
            continue;
        };
        let Ok(file) = object.cast::<ICoreWebView2File>() else {
            paths.push(None);
            continue;
        };
        let mut raw = windows_core::PWSTR::null();
        match unsafe { file.Path(&mut raw) } {
            Ok(()) => paths.push(Some(take_pwstr(raw)).filter(|path| !path.is_empty())),
            Err(_) => paths.push(None),
        }
    }
    paths
}

/// Keep the response aligned with the request keys; extra files are ignored.
fn align_paths(keys: &[String], mut paths: Vec<Option<String>>) -> Vec<Option<String>> {
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
