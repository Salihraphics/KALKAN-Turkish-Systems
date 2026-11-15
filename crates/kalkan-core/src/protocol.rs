//! GUI <-> engine message protocol.
//!
//! The GUI never calls engine internals directly. It sends [`Command`]s and
//! renders the [`Event`]s that come back. All events come from a single
//! thread (the engine controller), so they arrive in a deterministic order.

use crate::lifecycle::EngineState;

/// Requests from the GUI (or any other front end) to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Start the engine. Accepted in `Stopped` and `Failed`.
    Start,
    /// Stop the engine. Accepted in `Starting` and `Running`.
    Stop,
    /// Request an [`Event::Status`] snapshot.
    QueryStatus,
    /// Stop everything and terminate the engine threads. The last event is
    /// [`Event::ShutdownComplete`].
    Shutdown,
}

/// A point-in-time summary suitable for the Dashboard.
///
/// Every metric is `None` unless it was actually measured.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStatus {
    pub state: Option<EngineState>,
    /// KALKAN process CPU usage, percent of one logical core.
    pub cpu_percent: Option<f32>,
    /// KALKAN process working set in bytes.
    pub memory_bytes: Option<u64>,
    /// Aggregate receive rate in bytes/second (network monitor, Phase 4).
    pub rx_bytes_per_sec: Option<u64>,
    /// Aggregate send rate in bytes/second (network monitor, Phase 4).
    pub tx_bytes_per_sec: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagLevel {
    Info,
    Warning,
}

/// Notifications from the engine to the GUI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Every lifecycle transition, in order.
    StateChanged { from: EngineState, to: EngineState },
    /// Initialisation succeeded; the engine is Running.
    EngineStarted,
    /// The worker has terminated after a stop request.
    EngineStopped,
    /// Something failed. `message` is suitable for display.
    EngineError { message: String },
    /// Non-fatal information, e.g. a command rejected in the current state.
    DiagnosticMessage { level: DiagLevel, message: String },
    /// Metrics snapshot (periodic while Running, or on QueryStatus).
    Status(EngineStatus),
    /// The engine threads are finishing. No further events will arrive.
    ShutdownComplete,
}
