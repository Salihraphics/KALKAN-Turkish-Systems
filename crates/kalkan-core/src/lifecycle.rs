//! Engine lifecycle state machine.
//!
//! ```text
//!                  Start                 Started
//!   ┌─► Stopped ─────────► Starting ─────────────► Running
//!   │      ▲                 │   │                    │
//!   │      │ Stopped         │   │ Stop               │ Stop
//!   │      │                 │   ▼                    ▼
//!   │      └─────────────────┼─ Stopping ◄────────────┘
//!   │                        │   │
//!   │                   Fail │   │ Fail          (Running ─Fail─► Failed too)
//!   │                        ▼   ▼
//!   └──────── Start ───── Failed
//! ```
//!
//! * `Start` is accepted from `Stopped` and from `Failed` (retry).
//! * `Started` is accepted only from `Starting`. This is the *only* way to
//!   reach `Running`, so the engine never reports Running before its
//!   initialisation succeeded.
//! * `Stop` is accepted from `Starting` (abandon start-up) and `Running`.
//! * `Fail` is accepted from `Starting`, `Running` and `Stopping`.

use std::fmt;

/// The externally visible state of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl EngineState {
    pub const ALL: [EngineState; 5] = [
        EngineState::Stopped,
        EngineState::Starting,
        EngineState::Running,
        EngineState::Stopping,
        EngineState::Failed,
    ];

    /// Whether a Start control should be enabled in the UI.
    pub fn can_start(self) -> bool {
        matches!(self, EngineState::Stopped | EngineState::Failed)
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
            EngineState::Failed => "Failed",
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
    /// A start was requested.
    Start,
    /// Initialisation succeeded.
    Started,
    /// A stop was requested.
    Stop,
    /// The worker has terminated after a stop.
    Stopped,
    /// An error made the engine unusable.
    Fail,
}

impl Transition {
    pub const ALL: [Transition; 5] = [
        Transition::Start,
        Transition::Started,
        Transition::Stop,
        Transition::Stopped,
        Transition::Fail,
    ];
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
        (Stopped | Failed, T::Start) => Starting,
        (Starting, T::Started) => Running,
        // Stop during start is allowed: start-up is abandoned and cleaned up.
        (Starting | Running, T::Stop) => Stopping,
        (Stopping, T::Stopped) => Stopped,
        (Starting | Running | Stopping, T::Fail) => Failed,
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

    /// The complete table of legal transitions. Every other pair must fail.
    const LEGAL: [(EngineState, Transition, EngineState); 9] = [
        (Stopped, T::Start, Starting),
        (Failed, T::Start, Starting),
        (Starting, T::Started, Running),
        (Starting, T::Stop, Stopping),
        (Running, T::Stop, Stopping),
        (Stopping, T::Stopped, Stopped),
        (Starting, T::Fail, Failed),
        (Running, T::Fail, Failed),
        (Stopping, T::Fail, Failed),
    ];

    #[test]
    fn every_valid_transition() {
        for (from, t, to) in LEGAL {
            assert_eq!(next_state(from, t), Ok(to), "{from} --{t:?}-->");
        }
    }

    #[test]
    fn every_invalid_transition_is_rejected() {
        for from in EngineState::ALL {
            for t in Transition::ALL {
                let legal = LEGAL.iter().any(|&(f, tt, _)| f == from && tt == t);
                assert_eq!(next_state(from, t).is_ok(), legal, "{from} --{t:?}-->");
                if !legal {
                    assert_eq!(
                        next_state(from, t),
                        Err(TransitionError {
                            from,
                            transition: t
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn running_is_reachable_only_through_started() {
        for from in EngineState::ALL {
            for t in Transition::ALL {
                if next_state(from, t) == Ok(Running) {
                    assert_eq!((from, t), (Starting, T::Started));
                }
            }
        }
    }

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
    fn failure_then_retry() {
        let mut lc = Lifecycle::new();
        lc.apply(T::Start).unwrap();
        lc.apply(T::Fail).unwrap();
        assert_eq!(lc.state(), Failed);
        assert!(
            lc.apply(T::Stop).is_err(),
            "nothing to stop after a failure"
        );
        assert_eq!(lc.apply(T::Start).unwrap(), (Failed, Starting));
    }

    #[test]
    fn invalid_transition_leaves_state_unchanged() {
        let mut lc = Lifecycle::new();
        let err = lc.apply(T::Stop).unwrap_err();
        assert_eq!(err.from, Stopped);
        assert_eq!(lc.state(), Stopped);
    }

    #[test]
    fn ui_affordances_match_transitions() {
        for s in EngineState::ALL {
            assert_eq!(s.can_start(), next_state(s, T::Start).is_ok(), "{s}");
            assert_eq!(s.can_stop(), next_state(s, T::Stop).is_ok(), "{s}");
        }
    }
}
