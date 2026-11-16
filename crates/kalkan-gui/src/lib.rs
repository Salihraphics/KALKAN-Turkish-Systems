//! KALKAN native Win32 GUI.
//!
//! * [`model`], [`layout`] and [`theme`] are platform-independent safe Rust and
//!   are unit-tested on any OS.
//! * `win32` (Windows only) contains the window, the message loop and GDI
//!   painting. It is the only module in this crate allowed to use `unsafe`.
//!
//! The GUI depends on `kalkan-core` only. It talks to the engine through
//! [`kalkan_core::EngineLink`] and never touches the network layer.

#![deny(unsafe_code)]

pub mod brand;
pub mod layout;
pub mod logview;
pub mod measure;
pub mod model;
pub mod settings;
pub mod theme;

#[cfg(windows)]
#[allow(unsafe_code)]
mod win32;

use std::path::PathBuf;

pub use kalkan_core::{Command, EngineLink, Event};
pub use model::{AppInfo, Page};

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Show the window minimised (from `ui.start_minimized`).
    pub start_minimized: bool,
    /// Run the scripted self-test instead of waiting for the user.
    pub self_test: bool,
    /// During self-test, save a BMP screenshot of each step here.
    pub screenshot_dir: Option<PathBuf>,
    /// Process CPU time source for the self-test's CPU measurement (the GUI
    /// itself does not query the OS for metrics).
    pub cpu_time: Option<fn() -> Option<std::time::Duration>>,
    /// Run the long-running soak test for this many minutes instead of
    /// waiting for the user (`kalkan --soak <minutes>`).
    pub soak_minutes: Option<u32>,
    /// Process resource counters for the soak test (supplied by the app,
    /// like `cpu_time`).
    pub resources: Option<fn() -> Option<Resources>>,
}

/// Process-wide resource counters, as read by the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resources {
    pub cpu_time: std::time::Duration,
    pub working_set: u64,
    pub private_bytes: u64,
    pub kernel_handles: u32,
    pub gdi_objects: u32,
    pub user_objects: u32,
    pub threads: u32,
}

/// What the GUI needs besides the engine: the logging handle (log viewer,
/// live level/rotation changes) and the settings state (Settings page).
pub struct Services {
    pub logging: Option<std::sync::Arc<kalkan_logging::LoggingHandle>>,
    pub settings: settings::SettingsState,
    pub config_path: PathBuf,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services")
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

/// One self-test assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SelfTestReport {
    pub checks: Vec<Check>,
    pub screenshots: Vec<PathBuf>,
    /// Screenshot failures are reported but do not fail the self-test.
    pub screenshot_errors: Vec<String>,
    /// Measurements to print (soak test), already printed as they happened.
    pub notes: Vec<String>,
}

impl SelfTestReport {
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.passed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    #[error("the graphical interface is only available on Windows")]
    Unsupported,
    #[error("Windows API call {call} failed: {message}")]
    Win32 { call: &'static str, message: String },
    #[error("an internal error occurred in the user interface")]
    Panicked,
}

/// Create the main window and run the message loop until it closes.
///
/// Returns a report when `opts.self_test` is set.
pub fn run(
    info: AppInfo,
    engine: Box<dyn EngineLink>,
    services: Services,
    opts: RunOptions,
) -> Result<Option<SelfTestReport>, GuiError> {
    #[cfg(windows)]
    {
        win32::run(info, engine, services, opts)
    }
    #[cfg(not(windows))]
    {
        let _ = (info, engine, services, opts);
        Err(GuiError::Unsupported)
    }
}

/// Show a blocking error dialog, or print to stderr where there is no GUI.
/// Used for start-up failures (e.g. invalid configuration) that a user who
/// double-clicked the program would otherwise never see.
pub fn show_error(title: &str, message: &str) {
    #[cfg(windows)]
    win32::message_box_error(title, message);
    #[cfg(not(windows))]
    eprintln!("{title}: {message}");
}
