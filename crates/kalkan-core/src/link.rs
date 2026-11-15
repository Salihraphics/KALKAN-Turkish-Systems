//! The connection between a front end (the GUI) and the engine.
//!
//! A front end holds a `Box<dyn EngineLink>` and nothing more specific. The
//! real implementation is [`crate::engine::EngineHandle`].

use crate::protocol::{Command, Event};

/// Called by the engine (on its own thread) whenever new events are queued.
/// A GUI typically posts a window message from it. It must not block.
pub type Waker = Box<dyn Fn() + Send + 'static>;

/// Bidirectional, non-blocking link to an engine.
///
/// All methods must return immediately: they are called on the UI thread.
pub trait EngineLink {
    /// Queue a command for the engine. Never blocks.
    fn send(&mut self, cmd: Command);
    /// Take the next pending event, if any. Never blocks.
    fn try_recv(&mut self) -> Option<Event>;
    /// Install the waker. Events queued before this call trigger one
    /// immediate wake, so none are missed.
    fn set_waker(&mut self, waker: Waker);
}
