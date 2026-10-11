//! Read/write the persistent user environment (`HKCU\Environment`).
//!
//! Only `DSHELL_HOME` is ever touched, and only because the design calls for the
//! data root to survive moving `dshell.exe`. Everything here is best-effort: the
//! four-level fallback chain in `root::locate` is what actually guarantees the
//! app finds its data.

#[cfg(windows)]
pub fn set_user_var(name: &str, value: &str) -> std::io::Result<()> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: usize = 0x8000_0001;
    const KEY_SET_VALUE: u32 = 0x0002;
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
        fn RegSetValueExW(
            key: Hkey,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            len: u32,
        ) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    let subkey = wide("Environment");
    let mut key: Hkey = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER as Hkey,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }

    let name_w = wide(name);
    let value_w = wide(value);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            value_w.as_ptr() as *const u8,
            value_w.len() * std::mem::size_of::<u16>(),
        )
    };
    let rc = unsafe {
        RegSetValueExW(
            key,
            name_w.as_ptr(),
            0,
            REG_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(key) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }

    crate::win::broadcast_environment_change();
    Ok(())
}

#[cfg(windows)]
pub fn remove_user_var(name: &str) -> std::io::Result<()> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: usize = 0x8000_0001;
    const KEY_SET_VALUE: u32 = 0x0002;

    type Hkey = *mut c_void;

    extern "system" {
        fn RegOpenKeyExW(
            key: Hkey,
            subkey: *const u16,
            options: u32,
            desired: u32,
            result: *mut Hkey,
        ) -> i32;
        fn RegDeleteValueW(key: Hkey, name: *const u16) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    let subkey = wide("Environment");
    let mut key: Hkey = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER as Hkey,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }
    let name_w = wide(name);
    let rc = unsafe { RegDeleteValueW(key, name_w.as_ptr()) };
    unsafe { RegCloseKey(key) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }
    crate::win::broadcast_environment_change();
    Ok(())
}

#[cfg(not(windows))]
pub fn set_user_var(_name: &str, _value: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "not implemented on this platform",
    ))
}

#[cfg(not(windows))]
pub fn remove_user_var(_name: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "not implemented on this platform",
    ))
}

/// Probe a directory for writability by creating and deleting a temp file.
///
/// DSH does **not** report an unwritable home with a structured signal — it dies
/// with a raw V8 `EPERM`/`ENOTDIR` stack trace and no `logs/` report. So dshell
/// has to answer this question itself, before spawning anything.
pub fn probe_writable(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let probe = dir.join(format!(".dshell-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}
