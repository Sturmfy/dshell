//! Platform services that are neither webview- nor tab-specific.
//!
//! Everything here has exactly one implementation per platform family, and it
//! is the *only* place dshell reaches for the OS. Windows keeps its raw Win32
//! helpers in [`crate::win`]; this module is the higher-level surface the rest
//! of the crate calls.
//!
//! The contract is deliberately small:
//!
//! * `shell_open` / `shell_reveal` — hand a URL or a path to the OS;
//! * `webview_runtime` — what the host's web engine calls itself, for the
//!   environment report (WebView2 on Windows, WebKitGTK on Linux, WebKit on
//!   macOS). The UI names it, so a Linux user must not be told to install
//!   WebView2.

use std::io;
use std::path::Path;

/// The web engine the running host actually provides.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewRuntime {
    /// `WebView2`, `WebKitGTK`, or `WebKit`.
    pub name: String,
    /// The version when the platform exposes one cheaply, otherwise `None`.
    pub version: Option<String>,
}

/// Open `url` in the user's default browser.
pub fn shell_open(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::shell_execute("open", url, None)
    }
    #[cfg(target_os = "macos")]
    {
        spawn("open", &[url])
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        spawn("xdg-open", &[url])
    }
}

/// Show `path` in the OS file manager, selecting it when the OS supports that.
pub fn shell_reveal(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy().to_string();
        if path.is_dir() {
            windows::shell_execute("open", &text, None)
        } else {
            windows::shell_execute("open", "explorer.exe", Some(format!("/select,\"{text}\"")))
        }
    }
    #[cfg(target_os = "macos")]
    {
        let text = path.to_string_lossy().to_string();
        if path.is_dir() {
            spawn("open", &[text.as_str()])
        } else {
            spawn("open", &["-R", text.as_str()])
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = path.parent().unwrap_or(path).to_string_lossy().to_string();
        spawn("xdg-open", &[target.as_str()])
    }
}

/// The origin Tauri serves dshell's own documents from.
///
/// Windows uses `http://tauri.localhost` (never https: that would forbid mixed
/// content to `http://127.0.0.1`, and flipping it later would move every
/// webview's storage). macOS and Linux use the custom `tauri://localhost`
/// scheme, so nothing may hardcode the Windows form.
pub fn app_origin() -> &'static str {
    #[cfg(windows)]
    {
        "http://tauri.localhost"
    }
    #[cfg(not(windows))]
    {
        "tauri://localhost"
    }
}

/// What a webview-creation failure most likely means on this platform.
///
/// The failure itself is a Tauri error string; the hint is what turns it into
/// something a user can act on, and it must name the engine that platform
/// actually runs.
pub fn webview_failure_hint() -> &'static str {
    #[cfg(windows)]
    {
        "这通常意味着 WebView2 运行时不可用或已损坏。\
         可在 https://developer.microsoft.com/microsoft-edge/webview2/ 安装。"
    }
    #[cfg(target_os = "linux")]
    {
        "这通常意味着 WebKitGTK 4.1 运行时缺失或损坏；\
         请安装 libwebkit2gtk-4.1-0（以及 libgtk-3-0）。"
    }
    #[cfg(target_os = "macos")]
    {
        "这通常意味着系统 WebKit 无法初始化；\
         请确认 macOS 版本不低于 11。"
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        "系统 Web 引擎无法初始化。"
    }
}

/// The host's web engine, for the environment report.
pub fn webview_runtime() -> WebviewRuntime {
    #[cfg(windows)]
    {
        WebviewRuntime {
            name: "WebView2".to_string(),
            version: windows::webview2_version(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        // The GTK bindings dshell links against expose no runtime version
        // getter; presence is guaranteed by the fact that this binary linked.
        WebviewRuntime {
            name: "WebKitGTK".to_string(),
            version: None,
        }
    }
    #[cfg(target_os = "macos")]
    {
        WebviewRuntime {
            name: "WebKit".to_string(),
            version: macos_version(),
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        WebviewRuntime {
            name: "WebKit".to_string(),
            version: None,
        }
    }
}

#[cfg(not(windows))]
fn spawn(program: &str, args: &[&str]) -> io::Result<()> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn macos_version() -> Option<String> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(windows)]
mod windows {
    use std::ffi::{c_void, OsStr};
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// `ShellExecuteW`, hand-declared for the same reason as [`crate::win`]:
    /// three calls do not justify a `windows` crate dependency.
    pub(super) fn shell_execute(
        verb: &str,
        target: &str,
        params: Option<String>,
    ) -> io::Result<()> {
        extern "system" {
            fn ShellExecuteW(
                hwnd: *mut c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> *mut c_void;
        }

        let verb = wide(verb);
        let target = wide(target);
        let params = params.map(|value| wide(&value));
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                params
                    .as_ref()
                    .map(|p| p.as_ptr())
                    .unwrap_or(std::ptr::null()),
                std::ptr::null(),
                1,
            )
        };
        // ShellExecuteW returns a value <= 32 on failure.
        if result as isize <= 32 {
            return Err(io::Error::other(format!(
                "ShellExecuteW failed ({})",
                result as isize
            )));
        }
        Ok(())
    }

    /// The WebView2 runtime version as written by its installer.
    ///
    /// dshell does not ship WebView2; on a machine without it there is no
    /// window to render an error in, so this value is what the wizard and the
    /// settings page report instead.
    pub(super) fn webview2_version() -> Option<String> {
        use std::ffi::c_void;

        const HKEY_LOCAL_MACHINE: usize = 0x8000_0002;
        const KEY_READ: u32 = 0x2_0019;
        const REG_SZ: u32 = 1;
        type Hkey = *mut c_void;

        extern "system" {
            fn RegOpenKeyExW(
                key: Hkey,
                subkey: *const u16,
                options: u32,
                desired: u32,
                result: *mut Hkey,
            ) -> i32;
            fn RegQueryValueExW(
                key: Hkey,
                name: *const u16,
                reserved: *mut u32,
                kind: *mut u32,
                data: *mut u8,
                len: *mut u32,
            ) -> i32;
            fn RegCloseKey(key: Hkey) -> i32;
        }

        let subkey = wide(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}");
        let name = wide("pv");
        let mut key: Hkey = std::ptr::null_mut();
        let rc = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE as Hkey,
                subkey.as_ptr(),
                0,
                KEY_READ,
                &mut key,
            )
        };
        if rc != 0 {
            return None;
        }
        let mut kind = 0u32;
        let mut size = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if rc != 0 || kind != REG_SZ || size == 0 {
            unsafe { RegCloseKey(key) };
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let rc = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr(),
                &mut size,
            )
        };
        unsafe { RegCloseKey(key) };
        if rc != 0 {
            return None;
        }
        let wide_slice: Vec<u16> = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|value| *value != 0)
            .collect();
        let text = String::from_utf16_lossy(&wide_slice).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}
