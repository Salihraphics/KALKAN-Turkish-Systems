//! KALKAN core engine.
//!
//! This crate is platform independent. It defines:
//!
//! * [`EngineState`] and [`Lifecycle`] — the engine's lifecycle state machine.
//! * [`Command`] and [`Event`] — the *only* vocabulary the GUI uses to talk to
//!   the engine. The GUI sends commands, the engine replies with events. Neither
//!   side reaches into the other's internals.
//!
//! Phase 1 status: types and the pure state machine are implemented and unit
//! tested. The threaded engine runner that consumes [`Command`]s is Phase 3.

#![forbid(unsafe_code)]

pub mod lifecycle;
pub mod protocol;

pub use lifecycle::{EngineState, Lifecycle, Transition, TransitionError};
pub use protocol::{Command, EngineStatus, Event};
