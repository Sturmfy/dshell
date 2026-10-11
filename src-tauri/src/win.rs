/// Windows-only helpers that need raw Win32 access.
///
/// Everything here is deliberately hand-declared instead of pulling in the
/// `windows` / `windows-sys` crates: the surface is ~20 functions, and a
/// hand-declared surface cannot break under a crate version bump.
#[cfg(windows)]
mod platform {
    use std::ffi::{c_void, OsStr};
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    pub type Handle = *mut c_void;
    pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    pub fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    extern "system" {
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: u32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: u32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    /// A Win32 job object with `KILL_ON_JOB_CLOSE`.
    ///
    /// This is the only termination mechanism that survives a hard crash of
    /// dshell itself: the kernel reclaims every process in the job when the
    /// last handle to it closes (including on process death), so a `.cmd` shim's
    /// grandchild `node` cannot be orphaned.
    pub struct JobObject {
        handle: Handle,
    }

    // The handle is only used for synchronous Win32 calls and is guarded by the
    // owning process handle's lifetime.
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        pub fn new() -> io::Result<Self> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let job = Self { handle };
            let mut info = ExtendedLimitInformation::default();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let ok = unsafe {
                SetInformationJobObject(
                    job.handle,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    &mut info as *mut _ as *mut c_void,
                    std::mem::size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        /// Assign a process to the job.
        ///
        /// # Safety
        /// `process` must be a live process handle carrying `PROCESS_SET_QUOTA`
        /// and `PROCESS_TERMINATE` access — in practice a handle just returned by
        /// `Command::spawn`, which is the only caller.
        pub unsafe fn assign(&self, process: Handle) -> io::Result<()> {
            let ok = AssignProcessToJobObject(self.handle, process);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Ask the kernel to terminate every process currently in the job.
        pub fn terminate(&self) {
            unsafe { TerminateJobObject(self.handle, 1) };
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    /// Ask Explorer et al. to re-read the environment from the registry.
    ///
    /// `setx` would be the obvious tool but it truncates at 1024 characters, so
    /// dshell writes `HKCU\Environment` directly and broadcasts this instead.
    pub fn broadcast_environment_change() {
        const HWND_BROADCAST: Handle = 0xffff as Handle;
        const WM_SETTINGCHANGE: u32 = 0x001A;
        const SMTO_ABORTIFHUNG: u32 = 0x0002;

        extern "system" {
            fn SendMessageTimeoutW(
                hwnd: Handle,
                msg: u32,
                wparam: usize,
                lparam: isize,
                flags: u32,
                timeout: u32,
                result: *mut usize,
            ) -> isize;
        }

        let param = wide(OsStr::new("Environment"));
        let mut result: usize = 0;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                2000,
                &mut result,
            );
        }
    }

    /// `CREATE_NO_WINDOW` — required so a GUI-subsystem dshell does not flash a
    /// console window for every `dsh` child process.
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
}

#[cfg(windows)]
pub use platform::{broadcast_environment_change, JobObject, CREATE_NO_WINDOW};

#[cfg(not(windows))]
pub use fallback::{broadcast_environment_change, JobObject, CREATE_NO_WINDOW};

#[cfg(not(windows))]
mod fallback {
    use std::io;

    pub const CREATE_NO_WINDOW: u32 = 0;

    /// Job objects are a Windows concept. On other platforms the child is
    /// killed directly (see `dsh::process`); this is a no-op.
    #[derive(Default)]
    pub struct JobObject;

    impl JobObject {
        pub fn new() -> io::Result<Self> {
            Ok(Self)
        }

        pub fn assign(&self, _process: *mut std::ffi::c_void) -> io::Result<()> {
            Ok(())
        }

        pub fn terminate(&self) {}
    }

    pub fn broadcast_environment_change() {}
}
