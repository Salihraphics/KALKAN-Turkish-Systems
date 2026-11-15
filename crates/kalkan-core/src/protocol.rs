//! GUI <-> engine message protocol.
//!
//! The GUI never calls engine internals directly. It sends [`Command`]s over a
//! channel and renders whatever [`Event`]s come back. This keeps the UI thread
//! free of blocking work and lets future network modules be added behind the
//! engine without touching the GUI.

use crate::lifecycle::EngineState;

/// Requests from the GUI (or any other front end) to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start the engine and its enabled modules.
    Start,
    /// Stop the engine; modules release their resources.
    Stop,
    /// Clear a fault so the engine may be started again.
    Reset,
    /// Request a one-off [`Event::Status`] snapshot.
    QueryStatus,
    /// Stop everything and terminate the engine thread.
    Shutdown,
}

/// A point-in-time summary suitable for the Dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStatus {
    pub state: Option<EngineState>,
    /// Process CPU usage in percent of one logical core, if measured.
    pub cpu_percent: Option<f32>,
    /// Process working set in bytes, if measured.
    pub memory_bytes: Option<u64>,
    /// Aggregate receive rate in bytes/second, if the monitor is running.
    pub rx_bytes_per_sec: Option<u64>,
    /// Aggregate send rate in bytes/second, if the monitor is running.
    pub tx_bytes_per_sec: Option<u64>,
}

/// Notifications from the engine to the GUI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    StateChanged {
        from: EngineState,
        to: EngineState,
    },
    Status(EngineStatus),
    /// A user-visible error. `message` must be suitable for display.
    Error {
        message: String,
    },
    /// A user-visible informational notice (not an error).
    Info {
        message: String,
    },
    /// The engine thread has exited; no further events will arrive.
    ShutdownComplete,
}
