//! The connection between a front end (the GUI) and the engine.
//!
//! A front end holds a `Box<dyn EngineLink>` and never anything more specific.
//! Phase 2 uses [`UnimplementedEngine`]. Phase 3 replaces it with a link to the
//! threaded engine runner without changing the GUI.

use std::collections::VecDeque;

use crate::lifecycle::EngineState;
use crate::protocol::{Command, EngineStatus, Event};

/// Bidirectional, non-blocking link to an engine.
///
/// Both methods must return quickly: they are called on the UI thread.
pub trait EngineLink {
    /// Queue a command for the engine. Never blocks.
    fn send(&mut self, cmd: Command);
    /// Take the next pending event, if any. Never blocks.
    fn try_recv(&mut self) -> Option<Event>;
}

/// Stand-in engine used until the engine runner exists (Phase 3).
///
/// It does **not** pretend to work. Start and Stop produce an
/// [`Event::Info`] saying that nothing happened. The reported state is always
/// [`EngineState::Stopped`], which is true because nothing is running. Status
/// snapshots contain no metrics.
#[derive(Debug, Default)]
pub struct UnimplementedEngine {
    queue: VecDeque<Event>,
}

impl UnimplementedEngine {
    pub const START_MESSAGE: &'static str =
        "The engine is not implemented yet (Phase 3). Nothing was started.";
    pub const STOP_MESSAGE: &'static str =
        "The engine is not implemented yet (Phase 3). Nothing is running.";

    pub fn new() -> Self {
        Self::default()
    }
}

impl EngineLink for UnimplementedEngine {
    fn send(&mut self, cmd: Command) {
        let ev = match cmd {
            Command::Start => Event::Info {
                message: Self::START_MESSAGE.into(),
            },
            Command::Stop | Command::Reset => Event::Info {
                message: Self::STOP_MESSAGE.into(),
            },
            Command::QueryStatus => Event::Status(EngineStatus {
                state: Some(EngineState::Stopped),
                ..EngineStatus::default()
            }),
            Command::Shutdown => Event::ShutdownComplete,
        };
        self.queue.push_back(ev);
    }

    fn try_recv(&mut self) -> Option<Event> {
        self.queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(cmd: Command) -> Vec<Event> {
        let mut e = UnimplementedEngine::new();
        e.send(cmd);
        std::iter::from_fn(|| e.try_recv()).collect()
    }

    #[test]
    fn start_reports_nothing_happened_and_never_changes_state() {
        let evs = roundtrip(Command::Start);
        assert_eq!(evs.len(), 1);
        assert!(
            matches!(&evs[0], Event::Info { message } if message.contains("Nothing was started"))
        );
        assert!(!evs.iter().any(|e| matches!(e, Event::StateChanged { .. })));
    }

    #[test]
    fn status_has_no_metrics() {
        let evs = roundtrip(Command::QueryStatus);
        let [Event::Status(s)] = evs.as_slice() else {
            panic!("{evs:?}")
        };
        assert_eq!(s.state, Some(EngineState::Stopped));
        assert!(s.cpu_percent.is_none() && s.memory_bytes.is_none());
        assert!(s.rx_bytes_per_sec.is_none() && s.tx_bytes_per_sec.is_none());
    }

    #[test]
    fn shutdown_completes() {
        assert_eq!(roundtrip(Command::Shutdown), vec![Event::ShutdownComplete]);
    }

    #[test]
    fn empty_queue_returns_none() {
        assert!(UnimplementedEngine::new().try_recv().is_none());
    }

    #[test]
    fn usable_as_trait_object() {
        let mut l: Box<dyn EngineLink> = Box::new(UnimplementedEngine::new());
        l.send(Command::Stop);
        assert!(l.try_recv().is_some());
    }
}
