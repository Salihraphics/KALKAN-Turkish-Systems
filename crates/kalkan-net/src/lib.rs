//! KALKAN network monitoring.
//!
//! **Read-only by design.** Every monitor implements [`NetworkMonitor`], which
//! can only *observe* counters. There is no API here for modifying, injecting
//! or dropping traffic. Any future packet-processing module will be a separate
//! crate behind its own explicitly user-enabled trait (see
//! `docs/ARCHITECTURE.md`, "Extension points").
//!
//! Phase 1 status: the trait, counter types and the rate calculation are
//! implemented and tested. The Windows implementation (IP Helper
//! `GetIfTable2`) is Phase 4.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

/// Cumulative byte/packet counters summed over the monitored interfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

/// Counters captured at a monotonic instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub at: Instant,
    pub counters: Counters,
}

/// Per-second rates derived from two snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rates {
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub rx_packets_per_sec: u64,
    pub tx_packets_per_sec: u64,
}

impl Rates {
    /// Compute rates between `prev` and `next`.
    ///
    /// Returns `None` if no time has elapsed. If a counter went *backwards*
    /// (interface reset, adapter removed, driver reload) that counter's rate is
    /// reported as 0 for this interval rather than a huge bogus value.
    pub fn between(prev: &Snapshot, next: &Snapshot) -> Option<Self> {
        let dt = next.at.checked_duration_since(prev.at)?;
        Self::from_delta(&prev.counters, &next.counters, dt)
    }

    pub fn from_delta(prev: &Counters, next: &Counters, dt: Duration) -> Option<Self> {
        let nanos = dt.as_nanos();
        if nanos == 0 {
            return None;
        }
        let rate = |a: u64, b: u64| -> u64 {
            let delta = b.saturating_sub(a) as u128;
            // Saturate rather than overflow on absurd inputs.
            u64::try_from(delta * 1_000_000_000 / nanos).unwrap_or(u64::MAX)
        };
        Some(Self {
            rx_bytes_per_sec: rate(prev.rx_bytes, next.rx_bytes),
            tx_bytes_per_sec: rate(prev.tx_bytes, next.tx_bytes),
            rx_packets_per_sec: rate(prev.rx_packets, next.rx_packets),
            tx_packets_per_sec: rate(prev.tx_packets, next.tx_packets),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    #[error("network monitoring is not supported on this platform")]
    Unsupported,
    #[error("operating system call {call} failed with code {code}")]
    Os { call: &'static str, code: u32 },
    #[error("monitor is not running")]
    NotRunning,
}

/// A read-only source of network counters.
///
/// Implementations must be cheap to [`sample`](Self::sample): it is called on
/// the engine thread at the configured interval, never on the UI thread.
pub trait NetworkMonitor: Send {
    /// Stable identifier for logs.
    fn name(&self) -> &'static str;
    /// Acquire any OS resources. Must be idempotent.
    fn start(&mut self) -> Result<(), MonitorError>;
    /// Read current cumulative counters.
    fn sample(&mut self) -> Result<Snapshot, MonitorError>;
    /// Release OS resources. Must be idempotent and must not fail.
    fn stop(&mut self);
}

/// A monitor that always reports zero traffic. Used on unsupported platforms
/// and in tests of the engine lifecycle.
#[derive(Debug, Default)]
pub struct NullMonitor {
    running: bool,
}

impl NetworkMonitor for NullMonitor {
    fn name(&self) -> &'static str {
        "null"
    }
    fn start(&mut self) -> Result<(), MonitorError> {
        self.running = true;
        Ok(())
    }
    fn sample(&mut self) -> Result<Snapshot, MonitorError> {
        if !self.running {
            return Err(MonitorError::NotRunning);
        }
        Ok(Snapshot {
            at: Instant::now(),
            counters: Counters::default(),
        })
    }
    fn stop(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(rx: u64, tx: u64) -> Counters {
        Counters {
            rx_bytes: rx,
            tx_bytes: tx,
            rx_packets: rx / 100,
            tx_packets: tx / 100,
        }
    }

    #[test]
    fn simple_rate() {
        let r =
            Rates::from_delta(&c(1_000, 0), &c(3_000, 500), Duration::from_millis(500)).unwrap();
        assert_eq!(r.rx_bytes_per_sec, 4_000);
        assert_eq!(r.tx_bytes_per_sec, 1_000);
        assert_eq!(r.rx_packets_per_sec, 40);
    }

    #[test]
    fn zero_elapsed_is_none() {
        assert!(Rates::from_delta(&c(0, 0), &c(10, 10), Duration::ZERO).is_none());
    }

    #[test]
    fn counter_reset_reports_zero_not_garbage() {
        let r =
            Rates::from_delta(&c(10_000, 10_000), &c(5, 20_000), Duration::from_secs(1)).unwrap();
        assert_eq!(r.rx_bytes_per_sec, 0);
        assert_eq!(r.tx_bytes_per_sec, 10_000);
    }

    #[test]
    fn huge_delta_saturates() {
        let r = Rates::from_delta(&c(0, 0), &c(u64::MAX, 0), Duration::from_nanos(1)).unwrap();
        assert_eq!(r.rx_bytes_per_sec, u64::MAX);
    }

    #[test]
    fn snapshots_out_of_order_is_none() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        let a = Snapshot {
            at: t1,
            counters: c(0, 0),
        };
        let b = Snapshot {
            at: t0,
            counters: c(1, 1),
        };
        assert!(Rates::between(&a, &b).is_none());
    }

    #[test]
    fn null_monitor_lifecycle() {
        let mut m = NullMonitor::default();
        assert!(matches!(m.sample(), Err(MonitorError::NotRunning)));
        m.start().unwrap();
        m.start().unwrap(); // idempotent
        assert_eq!(m.sample().unwrap().counters, Counters::default());
        m.stop();
        m.stop(); // idempotent
        assert!(m.sample().is_err());
    }

    #[test]
    fn monitor_is_object_safe() {
        let _boxed: Box<dyn NetworkMonitor> = Box::new(NullMonitor::default());
    }
}
