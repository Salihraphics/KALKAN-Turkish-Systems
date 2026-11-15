//! Engine lifecycle state machine.
//!
//! ```text
//!            start()            started()
//!  Stopped ──────────► Starting ──────────► Running
//!     ▲                   │                    │
//!     │                   │ fail()             │ stop()
//!     │                   ▼                    ▼
//!     │ reset()        Faulted ◄── fail() ── Stopping
//!     └────────────────── │                    │
//!     ▲                                        │ stopped()
//!     └────────────────────────────────────────┘
//! ```
//!
//! `fail()` is accepted from `Starting`, `Running` and `Stopping`.
//! `reset()` returns a faulted engine to `Stopped` once resources are released.

use std::fmt;

/// The externally visible state of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Faulted,
}

impl EngineState {
    /// Whether a Start control should be enabled in the UI.
    pub fn can_start(self) -> bool {
        matches!(self, EngineState::Stopped)
    }

    /// Whether a Stop control should be enabled in the UI.
    pub fn can_stop(self) -> bool {
        matches!(self, EngineState::Starting | EngineState::Running)
    }

    /// Whether the engine is in a transitional state.
    pub fn is_transitioning(self) -> bool {
        matches!(self, EngineState::Starting | EngineState::Stopping)
    }

    /// Short, user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            EngineState::Stopped => "Stopped",
            EngineState::Starting => "Starting",
            EngineState::Running => "Running",
            EngineState::Stopping => "Stopping",
            EngineState::Faulted => "Error",
        }
    }
}

impl fmt::Display for EngineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Lifecycle inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Start,
    Started,
    Stop,
    Stopped,
    Fail,
    Reset,
}

/// Returned when a transition is not legal from the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid engine transition {transition:?} while {from}")]
pub struct TransitionError {
    pub from: EngineState,
    pub transition: Transition,
}

/// Pure transition function. No I/O, no side effects.
pub fn next_state(from: EngineState, t: Transition) -> Result<EngineState, TransitionError> {
    use EngineState::*;
    use Transition as T;
    let to = match (from, t) {
        (Stopped, T::Start) => Starting,
        (Starting, T::Started) => Running,
        // Stop during start is allowed: start-up is abandoned and cleaned up.
        (Starting | Running, T::Stop) => Stopping,
        (Stopping, T::Stopped) => Stopped,
        (Starting | Running | Stopping, T::Fail) => Faulted,
        (Faulted, T::Reset) => Stopped,
        _ => {
            return Err(TransitionError {
                from,
                transition: t,
            })
        }
    };
    Ok(to)
}

/// Tracks the current state and enforces legal transitions.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: EngineState,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    pub const fn new() -> Self {
        Self {
            state: EngineState::Stopped,
        }
    }

    pub fn state(&self) -> EngineState {
        self.state
    }

    /// Apply a transition. On success returns `(previous, new)`.
    /// On failure the state is unchanged.
    pub fn apply(&mut self, t: Transition) -> Result<(EngineState, EngineState), TransitionError> {
        let from = self.state;
        let to = next_state(from, t)?;
        self.state = to;
        Ok((from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EngineState::*;
    use Transition as T;

    const ALL_STATES: [EngineState; 5] = [Stopped, Starting, Running, Stopping, Faulted];
    const ALL_T: [Transition; 6] = [T::Start, T::Started, T::Stop, T::Stopped, T::Fail, T::Reset];

    #[test]
    fn happy_path_start_stop() {
        let mut lc = Lifecycle::new();
        assert_eq!(lc.state(), Stopped);
        assert_eq!(lc.apply(T::Start).unwrap(), (Stopped, Starting));
        assert_eq!(lc.apply(T::Started).unwrap(), (Starting, Running));
        assert_eq!(lc.apply(T::Stop).unwrap(), (Running, Stopping));
        assert_eq!(lc.apply(T::Stopped).unwrap(), (Stopping, Stopped));
    }

    #[test]
    fn failure_and_recovery() {
        let mut lc = Lifecycle::new();
        lc.apply(T::Start).unwrap();
        lc.apply(T::Fail).unwrap();
        assert_eq!(lc.state(), Faulted);
        assert!(lc.apply(T::Start).is_err(), "must reset before restarting");
        lc.apply(T::Reset).unwrap();
        assert_eq!(lc.state(), Stopped);
    }

    #[test]
    fn stop_while_starting_is_allowed() {
        let mut lc = Lifecycle::new();
        lc.apply(T::Start).unwrap();
        assert_eq!(lc.apply(T::Stop).unwrap(), (Starting, Stopping));
    }

    #[test]
    fn invalid_transition_leaves_state_unchanged() {
        let mut lc = Lifecycle::new();
        let err = lc.apply(T::Stop).unwrap_err();
        assert_eq!(err.from, Stopped);
        assert_eq!(lc.state(), Stopped);
    }

    #[test]
    fn exhaustive_transition_table() {
        // Count of legal (state, transition) pairs must match the documented diagram.
        let legal = ALL_STATES
            .iter()
            .flat_map(|&s| ALL_T.iter().map(move |&t| (s, t)))
            .filter(|&(s, t)| next_state(s, t).is_ok())
            .count();
        // Start, Started, Stop x2, Stopped, Fail x3, Reset
        assert_eq!(legal, 9);
    }

    #[test]
    fn ui_affordances_match_transitions() {
        for s in ALL_STATES {
            assert_eq!(s.can_start(), next_state(s, T::Start).is_ok(), "{s}");
            assert_eq!(s.can_stop(), next_state(s, T::Stop).is_ok(), "{s}");
        }
    }
}
