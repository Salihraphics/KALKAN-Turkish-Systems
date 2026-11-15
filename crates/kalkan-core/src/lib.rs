//! KALKAN core engine.
//!
//! This crate is platform independent. It defines:
//!
//! * [`EngineState`] and [`Lifecycle`] — the engine's lifecycle state machine.
//! * [`Command`] and [`Event`] — the *only* vocabulary the GUI uses to talk to
//!   the engine. The GUI sends commands, the engine replies with events. Neither
//!   side reaches into the other's internals.
//!
//! * [`EngineLink`] — the trait the GUI holds; [`UnimplementedEngine`] is the
//!   honest stand-in used until the engine runner exists.
//!
//! Status: types, the pure state machine and the stand-in link are implemented
//! and unit tested. The threaded engine runner is Phase 3.

#![forbid(unsafe_code)]

pub mod lifecycle;
pub mod link;
pub mod protocol;

pub use lifecycle::{EngineState, Lifecycle, Transition, TransitionError};
pub use link::{EngineLink, UnimplementedEngine};
pub use protocol::{Command, EngineStatus, Event};
