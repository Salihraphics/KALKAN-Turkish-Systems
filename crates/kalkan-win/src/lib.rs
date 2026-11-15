//! KALKAN Windows integration.
//!
//! Every `unsafe` Win32 call outside the GUI's window code (`kalkan-gui`'s
//! `win32` module) is confined to this crate, behind small safe functions. On non-Windows hosts (used for development and CI of
//! the platform-independent crates) the functions return conservative
//! fallbacks so the rest of the workspace still compiles and tests.

pub mod net;
pub use net::WinNetworkMonitor;

/// Whether the current process runs with an elevated (administrator) token.
///
/// KALKAN does **not** require elevation for read-only monitoring. This is
/// reported on the Dashboard so users know exactly what privileges the app has.
/// Returns `None` if the query itself failed.
pub fn is_elevated() -> Option<bool> {
    imp::is_elevated()
}

/// Memory counters of the current process, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub working_set: u64,
    pub peak_working_set: u64,
    /// Private committed memory (Task Manager's "Memory" column).
    pub private_bytes: u64,
}

/// Query this process's memory counters. `None` if the query failed or the
/// platform is not Windows.
pub fn process_memory() -> Option<ProcessMemory> {
    imp::process_memory()
}

/// Total CPU time (user + kernel) consumed by this process so far.
/// `None` if the query failed or the platform is not Windows.
pub fn process_cpu_time() -> Option<std::time::Duration> {
    imp::process_cpu_time()
}

/// Make `println!`/`eprintln!` reach the terminal that launched us.
///
/// KALKAN is a GUI-subsystem program, so Windows gives it no console. When it
/// is run from a terminal with CLI options, attach to the parent's console.
/// Nothing happens when the standard handles are already valid (redirected to
/// a file or pipe) or when there is no parent console (started from Explorer).
pub fn attach_parent_console() {
    imp::attach_parent_console()
}

/// Human-readable platform description for logs and the About box.
pub fn platform_description() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, OpenProcessToken};

    use super::ProcessMemory;

    /// Owns a token handle and closes it on drop.
    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by OpenProcessToken, is owned
            // exclusively by this wrapper and is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn is_elevated() -> Option<bool> {
        let mut token = HANDLE::default();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `token` is a valid out-pointer for the duration of the call.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
        let token = OwnedHandle(token);

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        // SAFETY: the buffer pointer/size describe `elevation`, which lives on
        // the stack for the whole call; `returned` is a valid out-pointer.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                Some(&mut elevation as *mut TOKEN_ELEVATION as *mut c_void),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        }
        .ok()?;
        Some(elevation.TokenIsElevated != 0)
    }

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the EX struct starts with PROCESS_MEMORY_COUNTERS and `cb`
        // tells the API its real size; it lives on the stack for the call.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                c.cb,
            )
        }
        .ok()?;
        Some(ProcessMemory {
            working_set: c.WorkingSetSize as u64,
            peak_working_set: c.PeakWorkingSetSize as u64,
            private_bytes: c.PrivateUsage as u64,
        })
    }

    pub fn process_cpu_time() -> Option<std::time::Duration> {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        // SAFETY: four valid FILETIME out-pointers on the stack; the
        // pseudo-handle from GetCurrentProcess needs no closing.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .ok()?;
        // FILETIME durations are in 100 ns units.
        let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        let total = ticks(kernel) + ticks(user);
        Some(std::time::Duration::from_nanos(total.saturating_mul(100)))
    }

    pub fn attach_parent_console() {
        // SAFETY: plain queries/calls without pointers. A failed attach (no
        // parent console) is expected and ignored.
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            let have_stdout = matches!(out, Ok(h) if !h.is_invalid() && !h.0.is_null());
            if !have_stdout {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_elevated() -> Option<bool> {
        None
    }
    pub fn process_memory() -> Option<super::ProcessMemory> {
        None
    }
    pub fn process_cpu_time() -> Option<std::time::Duration> {
        None
    }
    pub fn attach_parent_console() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_query_does_not_panic() {
        let r = is_elevated();
        if cfg!(windows) {
            assert!(
                r.is_some(),
                "token query should succeed for our own process"
            );
        } else {
            assert!(r.is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn process_memory_is_plausible() {
        let m = process_memory().expect("query should succeed on Windows");
        assert!(m.working_set > 0 && m.peak_working_set >= m.working_set);
    }

    #[cfg(windows)]
    #[test]
    fn process_cpu_time_increases_with_work() {
        let a = process_cpu_time().expect("query should succeed on Windows");
        // Burn ~100 ms of CPU; Windows accounts CPU time in ~15.6 ms ticks.
        let t0 = std::time::Instant::now();
        let mut x = 0u64;
        while t0.elapsed() < std::time::Duration::from_millis(100) {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        let b = process_cpu_time().unwrap();
        assert!(b > a, "cpu time did not increase: {a:?} -> {b:?}");
    }

    #[cfg(not(windows))]
    #[test]
    fn process_memory_unavailable_off_windows() {
        assert!(process_memory().is_none());
    }

    #[test]
    fn platform_description_is_nonempty() {
        assert!(!platform_description().trim().is_empty());
    }
}
