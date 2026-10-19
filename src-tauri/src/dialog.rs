//! Native Win32 dialogs for picking a folder or a file.
//!
//! Two different Windows APIs, because the two pickers answer different questions
//! and users notice when the wrong one is used:
//!
//! * **Folder** — `SHBrowseForFolderW`. It *is* a folder picker: no file-name box,
//!   no "Open" button, a "New folder" button, and it returns a directory. The
//!   classic `GetOpenFileNameW` trick (same dialog without `OFN_FILEMUSTEXIST`)
//!   looks like a file picker and asks the user to type a path, which is what made
//!   "选择安装位置" read as "选择文件".
//! * **File** — `GetOpenFileNameW`, with the filters the caller asked for.
//!
//! Everything here is hand-declared (no `windows` crate), so two things that a
//! generated binding would give for free are asserted instead:
//!
//! * `OPENFILENAMEW` is **152 bytes** on x64 and `BROWSEINFOW` **56**; a size
//!   Windows does not recognise is refused with `CommDlgExtendedError` `0x0001`
//!   (`CDERR_STRUCTSIZE`) *before* any window appears. The layout tests below pin
//!   both, layout included, because that failure is silent from the caller's side.
//! * `SHBrowseForFolderW` with `BIF_NEWDIALOGSTYLE` requires the calling thread to
//!   have an apartment; Tauri runs `async` commands on the tokio runtime, whose
//!   threads have none, so one is created around the call.
//!
//! The owner window is passed through as well: an unowned dialog is not modal to
//! dshell and can open *behind* it, which on a maximized window looks like a
//! button that does nothing.

#[cfg(windows)]
mod imp {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    const MAX_PATH_LONG: usize = 32768;
    const MAX_PATH: usize = 260;

    // --- GetOpenFileNameW
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_EXPLORER: u32 = 0x0008_0000;
    const OFN_DONTADDTORECENT: u32 = 0x0200_0000;

    // --- SHBrowseForFolderW
    /// Only filesystem directories can be returned.
    const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
    /// The resizable folder dialog with a list view and a "new folder" button.
    const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;
    /// An edit box so a path can be typed as well as browsed.
    const BIF_EDITBOX: u32 = 0x0000_0010;
    /// `BFFM_INITIALIZED` — the one message the callback needs.
    const BFFM_INITIALIZED: u32 = 1;
    /// `BFFM_SETSELECTIONW` (`WM_USER + 103`): selects the folder named by `lParam`.
    const BFFM_SETSELECTIONW: u32 = 0x0400 + 103;

    /// `COINIT_APARTMENTTHREADED`.
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    /// `LPOFNHOOKPROC`.
    type HookFn = extern "system" fn(*mut c_void, u32, usize, isize) -> usize;
    /// `BFFCALLBACK`.
    type BrowseCallback = extern "system" fn(*mut c_void, u32, isize, isize) -> i32;

    /// `OPENFILENAMEW`, field for field (x64: 152 bytes).
    ///
    /// | offset | field | offset | field |
    /// |---|---|---|---|
    /// | 0 | `lStructSize` | 88 | `lpstrTitle` |
    /// | 8 | `hwndOwner` | 96 | `Flags` |
    /// | 16 | `hInstance` | 100 | `nFileOffset` |
    /// | 24 | `lpstrFilter` | 102 | `nFileExtension` |
    /// | 32 | `lpstrCustomFilter` | 104 | `lpstrDefExt` |
    /// | 40 | `nMaxCustFilter` | 112 | `lCustData` |
    /// | 44 | `nFilterIndex` | 120 | `lpfnHook` |
    /// | 48 | `lpstrFile` | 128 | `lpTemplateName` |
    /// | 56 | `nMaxFile` | 136 | `pvReserved` |
    /// | 64 | `lpstrFileTitle` | 144 | `dwReserved` |
    /// | 72 | `nMaxFileTitle` | 148 | `FlagsEx` |
    /// | 80 | `lpstrInitialDir` | 152 | **total size** |
    #[repr(C)]
    #[derive(Default)]
    pub(crate) struct OpenFileNameW {
        pub(crate) l_struct_size: u32,
        pub(crate) hwnd_owner: *mut c_void,
        pub(crate) h_instance: *mut c_void,
        pub(crate) filter: *const u16,
        pub(crate) custom_filter: *mut u16,
        pub(crate) max_custom_filter: u32,
        pub(crate) filter_index: u32,
        pub(crate) file: *mut u16,
        pub(crate) max_file: u32,
        pub(crate) file_title: *mut u16,
        pub(crate) max_file_title: u32,
        pub(crate) initial_dir: *const u16,
        pub(crate) title: *const u16,
        pub(crate) flags: u32,
        pub(crate) file_offset: u16,
        pub(crate) file_extension: u16,
        pub(crate) def_ext: *const u16,
        pub(crate) cust_data: isize,
        pub(crate) hook: Option<HookFn>,
        pub(crate) template_name: *const u16,
        pub(crate) reserved_ptr: *mut c_void,
        pub(crate) reserved_dword: u32,
        pub(crate) flags_ex: u32,
    }

    /// `BROWSEINFOW` (x64: 8 + 8 + 8 + 8 + 4(+4) + 8 + 8 + 4(+4) = 56 bytes).
    #[repr(C)]
    #[derive(Default)]
    pub(crate) struct BrowseInfoW {
        pub(crate) hwnd_owner: *mut c_void,
        pub(crate) pidl_root: *const c_void,
        pub(crate) display_name: *mut u16,
        pub(crate) title: *const u16,
        pub(crate) flags: u32,
        pub(crate) callback: Option<BrowseCallback>,
        pub(crate) lparam: isize,
        pub(crate) image: i32,
    }

    // Each import library is named explicitly: `comdlg32` arrives by accident
    // through a dependency's default link set, but `shell32` and `ole32` do not,
    // and a missing one is a link error rather than a runtime one.
    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(dialog: *mut OpenFileNameW) -> i32;
        fn CommDlgExtendedError() -> u32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHBrowseForFolderW(info: *mut BrowseInfoW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, path: *mut u16) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(memory: *mut c_void);
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn to_path(buffer: &[u16]) -> PathBuf {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..end]))
    }

    /// The folder a picker should start in: `initial` when it is an existing
    /// directory, otherwise its nearest existing ancestor.
    ///
    /// A path that does not exist yet is the normal case here
    /// (`<data root>\runtimes` before the first clone), and handing such a path to a
    /// picker is what turns a would-be window into an immediate failure.
    pub(crate) fn initial_dir(initial: Option<&str>) -> Option<String> {
        let mut path = PathBuf::from(initial?);
        loop {
            if path.is_dir() {
                return Some(path.display().to_string());
            }
            if !path.pop() {
                return None;
            }
        }
    }

    /// Run `run` with an initialized COM apartment.
    ///
    /// `RPC_E_CHANGED_MODE` (0x8001_0106) means the thread already lives in another
    /// apartment; that is fine — the dialog works, and the matching
    /// `CoUninitialize` must then be skipped because this call did not initialize
    /// anything.
    fn with_com<T>(run: impl FnOnce() -> T) -> T {
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        let initialized = hr >= 0;
        let result = run();
        if initialized {
            unsafe { CoUninitialize() };
        }
        result
    }

    /// Selects the starting folder once the browse dialog exists.
    extern "system" fn select_initial(
        hwnd: *mut c_void,
        message: u32,
        _wparam: isize,
        lparam: isize,
    ) -> i32 {
        if message == BFFM_INITIALIZED && lparam != 0 {
            // `TRUE` tells the dialog that `lParam` is a path string.
            unsafe { SendMessageW(hwnd, BFFM_SETSELECTIONW, 1, lparam) };
        }
        0
    }

    /// A real folder picker: `SHBrowseForFolderW` with the resizable dialog.
    pub fn pick_folder(
        title: &str,
        initial: Option<&str>,
        owner: Option<isize>,
    ) -> Result<Option<PathBuf>, String> {
        let title = wide(title);
        let mut display = vec![0u16; MAX_PATH];
        // The callback receives this pointer through `lParam`, so it has to
        // outlive the (modal) call — which it does: it lives in this frame.
        let selection = initial_dir(initial).map(|path| wide(&path));
        let selection_ptr = selection
            .as_ref()
            .map_or(0, |text| text.as_ptr() as isize);

        let mut info = BrowseInfoW {
            hwnd_owner: owner.map_or(std::ptr::null_mut(), |handle| handle as *mut c_void),
            pidl_root: std::ptr::null(),
            display_name: display.as_mut_ptr(),
            title: title.as_ptr(),
            flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            callback: Some(select_initial),
            lparam: selection_ptr,
            image: 0,
        };

        let pidl = with_com(|| unsafe { SHBrowseForFolderW(&mut info) });
        if pidl.is_null() {
            // No extended-error channel exists for this API; a null pidl is the
            // documented "cancelled" answer.
            return Ok(None);
        }

        let mut path_buffer = vec![0u16; MAX_PATH_LONG];
        let ok = unsafe { SHGetPathFromIDListW(pidl, path_buffer.as_mut_ptr()) };
        unsafe { CoTaskMemFree(pidl) };
        if ok == 0 {
            return Err("无法把所选文件夹转换成路径".to_string());
        }
        Ok(Some(to_path(&path_buffer)))
    }

    /// `GetOpenFileNameW`, with an initialized apartment and an owner window.
    pub fn pick_file(
        title: &str,
        filter_pairs: &str,
        owner: Option<isize>,
    ) -> Result<Option<PathBuf>, String> {
        let mut file = vec![0u16; MAX_PATH_LONG];
        let title = wide(title);
        // A filter list is pairs of NUL-terminated strings, and the list itself
        // ends with an empty string (`desc\0pattern\0\0`). Building it here means a
        // caller cannot half-specify the terminator and silently get no filter.
        let mut filter: Vec<u16> = OsStr::new(filter_pairs.trim_end_matches('\0'))
            .encode_wide()
            .collect();
        filter.push(0);
        filter.push(0);
        let def_ext = wide("exe");

        let mut dialog = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            hwnd_owner: owner.map_or(std::ptr::null_mut(), |handle| handle as *mut c_void),
            h_instance: std::ptr::null_mut(),
            filter: filter.as_ptr(),
            custom_filter: std::ptr::null_mut(),
            max_custom_filter: 0,
            filter_index: 1,
            file: file.as_mut_ptr(),
            max_file: file.len() as u32,
            file_title: std::ptr::null_mut(),
            max_file_title: 0,
            initial_dir: std::ptr::null(),
            title: title.as_ptr(),
            flags: OFN_EXPLORER
                | OFN_NOCHANGEDIR
                | OFN_PATHMUSTEXIST
                | OFN_FILEMUSTEXIST
                | OFN_DONTADDTORECENT,
            file_offset: 0,
            file_extension: 0,
            def_ext: def_ext.as_ptr(),
            cust_data: 0,
            hook: None,
            template_name: std::ptr::null(),
            reserved_ptr: std::ptr::null_mut(),
            reserved_dword: 0,
            flags_ex: 0,
        };

        let shown = with_com(|| unsafe { GetOpenFileNameW(&mut dialog) });
        if shown == 0 {
            let code = unsafe { CommDlgExtendedError() };
            // 0 is the documented "the user cancelled" answer. Anything else means
            // the dialog never opened, and reporting it is the difference between a
            // fixable message and a button that looks dead.
            if code == 0 {
                return Ok(None);
            }
            return Err(format!(
                "系统文件对话框未能打开（CommDlgExtendedError = 0x{code:04X}{}）",
                describe(code)
            ));
        }
        Ok(Some(to_path(&file)))
    }

    fn describe(code: u32) -> &'static str {
        match code {
            0x0001 => "，CDERR_STRUCTSIZE：OPENFILENAMEW 的 lStructSize 不被接受",
            0x0002 => "，CDERR_INITIALIZATION：对话框初始化失败",
            0x0003 => "，CDERR_NOTEMPLATE：缺少对话框模板",
            0x0004 => "，CDERR_NOHINSTANCE：缺少 hInstance",
            0x0009 => "，CDERR_MEMALLOCFAILURE：无法分配内存",
            0x000B => "，CDERR_NOHOOK：声明了钩子却没有函数",
            0x3002 => "，FNERR_INVALIDFILENAME：初始文件名无效",
            0x3003 => "，FNERR_BUFFERTOOSMALL：路径缓冲区太小",
            _ => "",
        }
    }
}

#[cfg(windows)]
pub fn pick_directory(
    initial: Option<&str>,
    owner: Option<isize>,
    title: &str,
) -> Result<Option<String>, String> {
    imp::pick_folder(title, initial, owner)
        .map(|picked| picked.map(|path| path.to_string_lossy().to_string()))
}

#[cfg(windows)]
pub fn pick_file(title: Option<&str>, owner: Option<isize>) -> Result<Option<String>, String> {
    const FILTER: &str = "可执行文件\0*.exe\0所有文件\0*.*";
    imp::pick_file(
        title.unwrap_or("选择 DSH 后端可执行文件"),
        FILTER,
        owner,
    )
    .map(|picked| picked.map(|path| path.to_string_lossy().to_string()))
}

#[cfg(not(windows))]
pub fn pick_directory(
    _initial: Option<&str>,
    _owner: Option<isize>,
    _title: &str,
) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn pick_file(_title: Option<&str>, _owner: Option<isize>) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(all(test, windows))]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::imp;

    /// `CDERR_STRUCTSIZE` (`0x0001`) is what `GetOpenFileNameW` answers when
    /// `lStructSize` is not a size Windows knows, and it answers it *before* any
    /// window appears. An earlier revision dropped two members (`lpstrInitialDir`,
    /// `lpstrDefExt`), which shifted every field after them and made the dialog
    /// silently refuse to open.
    #[test]
    fn the_open_file_name_layout_matches_win32() {
        assert_eq!(size_of::<imp::OpenFileNameW>(), 152, "OPENFILENAMEW on x64");
        assert_eq!(offset_of!(imp::OpenFileNameW, hwnd_owner), 8);
        assert_eq!(offset_of!(imp::OpenFileNameW, filter), 24);
        assert_eq!(offset_of!(imp::OpenFileNameW, file), 48);
        assert_eq!(offset_of!(imp::OpenFileNameW, file_title), 64);
        assert_eq!(offset_of!(imp::OpenFileNameW, initial_dir), 80);
        assert_eq!(offset_of!(imp::OpenFileNameW, title), 88);
        assert_eq!(offset_of!(imp::OpenFileNameW, flags), 96);
        assert_eq!(offset_of!(imp::OpenFileNameW, file_offset), 100);
        assert_eq!(offset_of!(imp::OpenFileNameW, def_ext), 104);
        assert_eq!(offset_of!(imp::OpenFileNameW, hook), 120);
        assert_eq!(offset_of!(imp::OpenFileNameW, flags_ex), 148);
    }

    /// `BROWSEINFOW` has no size field, so a wrong layout does not produce a tidy
    /// error code — it produces a crash or a garbage window. Pinned here.
    #[test]
    fn the_browse_info_layout_matches_win32() {
        assert_eq!(size_of::<imp::BrowseInfoW>(), 64, "BROWSEINFOW on x64");
        assert_eq!(offset_of!(imp::BrowseInfoW, hwnd_owner), 0);
        assert_eq!(offset_of!(imp::BrowseInfoW, pidl_root), 8);
        assert_eq!(offset_of!(imp::BrowseInfoW, display_name), 16);
        assert_eq!(offset_of!(imp::BrowseInfoW, title), 24);
        assert_eq!(offset_of!(imp::BrowseInfoW, flags), 32);
        assert_eq!(offset_of!(imp::BrowseInfoW, callback), 40);
        assert_eq!(offset_of!(imp::BrowseInfoW, lparam), 48);
        assert_eq!(offset_of!(imp::BrowseInfoW, image), 56);
    }

    /// The starting folder must exist: a path that is about to be *created*
    /// (`<data root>\runtimes` before the first clone) is the normal input here.
    #[test]
    fn the_start_folder_walks_up_to_an_existing_directory() {
        let missing = std::env::temp_dir()
            .join(format!("dshell-start-{}", std::process::id()))
            .join("not")
            .join("there");
        let start = imp::initial_dir(Some(&missing.to_string_lossy())).expect("temp dir exists");
        assert!(std::path::Path::new(&start).is_dir(), "must exist: {start}");
        assert!(
            !start.ends_with("not"),
            "the missing tail must be dropped: {start}"
        );

        assert_eq!(imp::initial_dir(None), None);
    }

    /// Does the folder picker actually come up? Answered without a human.
    ///
    /// `SHBrowseForFolderW` has no error channel, so the observable outcomes are
    /// "returns" and "blocks on a live dialog". A watchdog separates them: if the
    /// call is still waiting after a few seconds the dialog exists, and the watchdog
    /// exits the process with 0 (which also closes it). A crash or an immediate
    /// return would fail the test. **A window is visible for a few seconds.**
    ///
    /// ```powershell
    /// cargo test dialog::tests::live_folder_picker -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "opens a real folder dialog for a few seconds, then exits the process"]
    fn live_folder_picker_opens() {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(8));
            println!("watchdog: still blocked on the folder dialog → it opened");
            std::process::exit(0);
        });

        let start = Some(std::env::temp_dir().to_string_lossy().to_string());
        match imp::pick_folder("dshell folder probe", start.as_deref(), None) {
            Ok(picked) => {
                println!("folder picker returned {picked:?}");
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("the folder picker failed: {err}");
                std::process::exit(3);
            }
        }
    }
}
