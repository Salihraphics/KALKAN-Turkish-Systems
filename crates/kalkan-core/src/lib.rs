//! KALKAN core engine.
//!
//! This crate is platform independent. It defines:
//!
//! * [`EngineState`] and [`Lifecycle`]: the engine's lifecycle state machine.
//! * [`Command`] and [`Event`]: the *only* vocabulary the GUI uses to talk to
//!   the engine.
//! * [`EngineLink`]: the non-blocking trait the GUI holds.
//! * [`EngineHandle`]: the engine (GoodBoyDPI) itself. It is a controller
//!   thread plus a worker thread that runs while the engine is started. See
//!   [`engine`].

#![forbid(unsafe_code)]

pub mod engine;
pub mod lifecycle;
pub mod link;
pub mod protocol;

/// Re-exported so front ends need no direct dependency on `kalkan-net`.
pub use kalkan_net::{InterfaceKind, InterfaceStatus, NetworkStatus};

pub use engine::{
    live_engine_threads, EngineHandle, EngineSpec, NetworkFactory, ProbeFactory, ProcessProbe,
    ProcessSample, ENGINE_NAME, ENGINE_SCOPE, ENGINE_TITLE, MAX_SAMPLE_INTERVAL,
    MIN_SAMPLE_INTERVAL,
};
pub use lifecycle::{EngineState, Lifecycle, Transition, TransitionError};
pub use link::{EngineLink, Waker};
pub use protocol::{Command, DiagLevel, EngineStatus, Event};
