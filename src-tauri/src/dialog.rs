//! Native Win32 dialogs for picking a directory or a file.
//!
//! Deliberately not a Tauri plugin: this is the entire need (two pickers) and a
//! hand-declared surface cannot break under a dependency bump. The dialog must be
//! shown from a thread where COM/modeless rules allow it, which is why the
//! commands that call this are `async` — Tauri runs them on the async runtime,
//! never on the event loop thread.

#[cfg(windows)]
mod imp {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    const MAX_PATH_LONG: usize = 32768;
    const OFN_EXPLORER: u32 = 0x0008_0000;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_DONTADDTORECENT: u32 = 0x0200_0000;

    #[repr(C)]
    #[derive(Default)]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: *mut c_void,
        h_instance: *mut c_void,
        filter: *const u16,
        custom_filter: *const u16,
        max_custom_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        flags: u32,
        filter_index_buffer: u16,
        custom_filter_buffer: u16,
        reserved: [u8; 24],
    }

    extern "system" {
        fn GetOpenFileNameW(dialog: *mut OpenFileNameW) -> i32;
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn to_path(buffer: &[u16]) -> PathBuf {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..end]))
    }

    pub fn pick_directory(title: &str, initial: Option<&str>) -> Option<PathBuf> {
        let mut buffer = vec![0u16; MAX_PATH_LONG];
        if let Some(initial) = initial {
            let initial = wide(initial);
            let len = initial.len().saturating_sub(1).min(buffer.len() - 1);
            buffer[..len].copy_from_slice(&initial[..len]);
        }
        let title_w = wide(title);
        let filter = wide("所有文件\0*.*\0\0");

        let mut dialog = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            filter: filter.as_ptr(),
            file: buffer.as_mut_ptr(),
            max_file: buffer.len() as u32,
            file_title: title_w.as_ptr() as *mut u16,
            flags: OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_DONTADDTORECENT,
            ..Default::default()
        };
        // The directory picker is the same dialog with the "file must exist"
        // flag absent: the classic Win32 way to select a folder.
        dialog.flags &= !OFN_FILEMUSTEXIST;
        let shown = unsafe { GetOpenFileNameW(&mut dialog) };
        if shown == 0 {
            return None;
        }
        let path = to_path(&buffer);
        Some(if path.is_file() {
            path.parent().map(PathBuf::from).unwrap_or(path)
        } else {
            path
        })
    }

    pub fn pick_file(title: &str, initial: Option<&str>, filter_pairs: &str) -> Option<PathBuf> {
        let mut buffer = vec![0u16; MAX_PATH_LONG];
        if let Some(initial) = initial {
            if let Some(parent) = PathBuf::from(initial).parent() {
                let parent = wide(&parent.to_string_lossy());
                let len = parent.len().saturating_sub(1).min(buffer.len() - 1);
                buffer[..len].copy_from_slice(&parent[..len]);
            }
        }
        let title_w = wide(title);
        let filter = wide(filter_pairs);

        let mut dialog = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            filter: filter.as_ptr(),
            file: buffer.as_mut_ptr(),
            max_file: buffer.len() as u32,
            file_title: title_w.as_ptr() as *mut u16,
            flags: OFN_EXPLORER
                | OFN_NOCHANGEDIR
                | OFN_PATHMUSTEXIST
                | OFN_FILEMUSTEXIST
                | OFN_DONTADDTORECENT,
            ..Default::default()
        };
        let shown = unsafe { GetOpenFileNameW(&mut dialog) };
        if shown == 0 {
            return None;
        }
        Some(to_path(&buffer))
    }
}

#[cfg(windows)]
pub fn pick_directory(initial: Option<&str>) -> Option<String> {
    imp::pick_directory("选择目录", initial).map(|path| path.to_string_lossy().to_string())
}

#[cfg(windows)]
pub fn pick_file(title: Option<&str>) -> Option<String> {
    const FILTER: &str = "可执行文件\0*.exe\0所有文件\0*.*\0\0";
    imp::pick_file(title.unwrap_or("选择 DSH 后端可执行文件"), None, FILTER)
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(not(windows))]
pub fn pick_directory(_initial: Option<&str>) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn pick_file(_title: Option<&str>) -> Option<String> {
    None
}
