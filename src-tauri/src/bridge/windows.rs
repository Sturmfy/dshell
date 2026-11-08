//! The WebView2 transport for the content bridge.
//!
//! Everything here is Windows-only by construction: the message channel is
//! `ICoreWebView2`'s, the file paths come from `ICoreWebView2File`, and the
//! context menu and Print accelerator are WebView2 concepts. The protocol and
//! the origin fence stay in the parent module so they are testable everywhere.

use std::sync::{Arc, Mutex, RwLock};

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

use super::{
    align_paths, is_safe_css_color, parse_request, same_origin, BridgeRequest, PROTOCOL_VERSION,
};

/// The per-tab state of the Windows transport.
pub(super) struct Bridge {
    app: AppHandle,
    label: String,
    origin: Arc<RwLock<Option<String>>>,
    /// The user's custom tab name, when there is one. It is sent on every page
    /// ready so a reload gets the brand replacement too, and re-sent on rename.
    title: Arc<RwLock<Option<String>>>,
    /// The last dark/light mode successfully given to the WebView2 backing
    /// surface. The theme reporter polls every couple of seconds even when the
    /// theme has not changed, so this keeps that poll from becoming a stream of
    /// COM calls.
    background_dark: Arc<Mutex<Option<bool>>>,
}

impl Bridge {
    /// Register the WebView2 message handler on one content webview.
    ///
    /// Failure is non-fatal: the tab still works and file uploads continue.
    /// The injected script never receives `hello`, so a picker request fails
    /// fast instead of leaving DSH waiting on a dialog that cannot exist.
    pub(super) fn attach(
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
            background_dark: Arc::new(Mutex::new(None)),
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

    pub(super) fn set_origin(&self, origin: &str) {
        if let Ok(mut slot) = self.origin.write() {
            *slot = Some(origin.to_string());
        }
    }

    fn on_message(&self, sender: &ICoreWebView2, args: &ICoreWebView2WebMessageReceivedEventArgs) {
        let source = unsafe {
            let mut raw = PWSTR::null();
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
    pub(super) fn set_title(&self, title: &str) {
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
        let tab_id = self
            .label
            .strip_prefix("tab-")
            .unwrap_or(&self.label)
            .to_string();
        let _ = self.app.emit(
            crate::TAB_THEME_EVENT,
            json!({
                "tabId": &tab_id,
                "background": background,
                "foreground": foreground,
                "mode": mode,
            }),
        );
        // The strip consumes the event above; the host also uses the mode to
        // keep the WebView2 backing surface in step. Resizing a parked tab back
        // into view can expose that surface for a frame, and the default is
        // white on a dark theme. Only spend the COM call when the mode actually
        // changes — the reporter polls even when the theme is unchanged.
        let dark = mode == "dark";
        let needs_update = self
            .background_dark
            .lock()
            .map(|slot| *slot != Some(dark))
            .unwrap_or(true);
        if !needs_update {
            return;
        }
        let app = self.app.clone();
        let cache = self.background_dark.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<crate::state::AppState>>() {
                if state.manager.set_tab_background(&tab_id, dark).await {
                    if let Ok(mut slot) = cache.lock() {
                        *slot = Some(dark);
                    }
                }
            }
        });
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

impl Clone for Bridge {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            label: self.label.clone(),
            origin: self.origin.clone(),
            title: self.title.clone(),
            background_dark: self.background_dark.clone(),
        }
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

/// Read a page message in either wire form.
///
/// `content.js` posts a string today (works with `TryGetWebMessageAsString`);
/// the JSON fallback keeps a future object-shaped post working too.
fn read_message(args: &ICoreWebView2WebMessageReceivedEventArgs) -> String {
    let mut raw = PWSTR::null();
    if unsafe { args.TryGetWebMessageAsString(&mut raw) }.is_ok() {
        return take_pwstr(raw);
    }
    let mut raw = PWSTR::null();
    if unsafe { args.WebMessageAsJson(&mut raw) }.is_ok() {
        let json = take_pwstr(raw);
        if let Ok(Value::String(inner)) = serde_json::from_str(&json) {
            return inner;
        }
        return json;
    }
    String::new()
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
        let mut raw = PWSTR::null();
        match unsafe { file.Path(&mut raw) } {
            Ok(()) => paths.push(Some(take_pwstr(raw)).filter(|path| !path.is_empty())),
            Err(_) => paths.push(None),
        }
    }
    paths
}
