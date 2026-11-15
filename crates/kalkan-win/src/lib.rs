//! KALKAN Windows integration.
//!
//! Every `unsafe` Win32 call in the project is confined to this crate, behind
//! small safe functions. On non-Windows hosts (used for development and CI of
//! the platform-independent crates) the functions return conservative
//! fallbacks so the rest of the workspace still compiles and tests.

/// Whether the current process runs with an elevated (administrator) token.
///
/// KALKAN does **not** require elevation for read-only monitoring. This is
/// reported on the Dashboard so users know exactly what privileges the app has.
/// Returns `None` if the query itself failed.
pub fn is_elevated() -> Option<bool> {
    imp::is_elevated()
}

/// Human-readable platform description for logs and the About box.
pub fn platform_description() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
}

#[cfg(not(windows))]
mod imp {
    pub fn is_elevated() -> Option<bool> {
        None
    }
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

    #[test]
    fn platform_description_is_nonempty() {
        assert!(!platform_description().trim().is_empty());
    }
}
