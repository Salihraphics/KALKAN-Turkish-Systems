//! KALKAN network monitoring.
//!
//! **Read-only by design.** A [`NetworkMonitor`] can only *read* the
//! interface counters the operating system already keeps. There is no API
//! here for capturing, modifying, injecting or dropping traffic.
//!
//! * [`InterfaceSample`] / [`Snapshot`]: one reading of all relevant
//!   interfaces, produced by a platform monitor (Windows: `kalkan-win`).
//! * [`NetTracker`]: turns consecutive snapshots into per-interface and total
//!   rates, session totals and peaks. It survives counter resets, adapters
//!   appearing or disappearing, and connect/disconnect, and reports those
//!   changes as [`InterfaceChange`]s.
//!
//! Everything in this crate is platform independent and unit-tested.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Stable identifier of an interface (on Windows: the interface LUID).
pub type InterfaceId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterfaceKind {
    Ethernet,
    WiFi,
    Cellular,
    Tunnel,
    Other,
}

impl InterfaceKind {
    pub fn label(self) -> &'static str {
        match self {
            InterfaceKind::Ethernet => "Ethernet",
            InterfaceKind::WiFi => "Wi-Fi",
            InterfaceKind::Cellular => "Cellular",
            InterfaceKind::Tunnel => "Tunnel",
            InterfaceKind::Other => "Other",
        }
    }
}

/// One interface as reported by the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceSample {
    pub id: InterfaceId,
    /// User-visible name, e.g. "Ethernet" or "Wi-Fi".
    pub name: String,
    /// Adapter description, e.g. the driver's product name.
    pub description: String,
    pub kind: InterfaceKind,
    /// Operationally up (link established).
    pub connected: bool,
    /// A hardware interface (not a VPN, tunnel or virtual switch port).
    /// Only hardware interfaces are counted in totals, so traffic that
    /// passes through a VPN and a physical NIC is not counted twice.
    pub hardware: bool,
    /// MAC address (48-bit, packed), if the interface has one. Two hardware
    /// interfaces with the same MAC carry the same traffic (e.g. a Hyper-V
    /// synthetic NIC and its SR-IOV virtual function under Azure
    /// Accelerated Networking), so only one of them is counted.
    pub mac: Option<u64>,
    /// Cumulative counters kept by the OS (since the adapter came up).
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Negotiated link speed in bits per second, if known.
    pub link_speed_bps: Option<u64>,
}

/// All relevant interfaces at one monotonic instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub at: Instant,
    pub interfaces: Vec<InterfaceSample>,
}

impl Snapshot {
    pub fn empty() -> Self {
        Snapshot {
            at: Instant::now(),
            interfaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MonitorError {
    #[error("network monitoring is not supported on this platform")]
    Unsupported,
    #[error("{call} failed with error {code}")]
    Os { call: &'static str, code: u32 },
}

/// A read-only source of interface counters.
///
/// `sample` is called on the engine worker thread once per sample interval,
/// never on the UI thread. It fills `out` (reusing its allocation).
pub trait NetworkMonitor: Send {
    /// Stable identifier for logs.
    fn name(&self) -> &'static str;
    fn sample(&mut self, out: &mut Snapshot) -> Result<(), MonitorError>;
}

/// Bytes per second from a counter delta. A counter that went backwards
/// (adapter reset, driver reload) yields 0 for that interval, never a bogus
/// huge value. `None` when no time elapsed.
pub fn rate(prev: u64, cur: u64, dt: Duration) -> Option<u64> {
    let nanos = dt.as_nanos();
    if nanos == 0 {
        return None;
    }
    let delta = cur.saturating_sub(prev) as u128;
    Some(u64::try_from(delta * 1_000_000_000 / nanos).unwrap_or(u64::MAX))
}

/// Live state of one interface, as shown in the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceStatus {
    pub id: InterfaceId,
    pub name: String,
    pub description: String,
    pub kind: InterfaceKind,
    pub connected: bool,
    /// Included in the totals (connected hardware interface, not a
    /// duplicate of another counted interface).
    pub counted: bool,
    /// Name of the counted interface this one duplicates (same MAC), if any.
    pub duplicate_of: Option<String>,
    /// OS cumulative counters.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// `None` on the first sample of this interface.
    pub rx_rate: Option<u64>,
    pub tx_rate: Option<u64>,
    pub link_speed_bps: Option<u64>,
}

/// Aggregate network state after a sample.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkStatus {
    /// Sorted: counted first, then connected, then by name.
    pub interfaces: Vec<InterfaceStatus>,
    /// Sum over counted interfaces. `None` until two samples exist.
    pub rx_rate: Option<u64>,
    pub tx_rate: Option<u64>,
    /// Bytes moved on counted interfaces since monitoring started.
    pub session_rx_bytes: u64,
    pub session_tx_bytes: u64,
    pub peak_rx_rate: u64,
    pub peak_tx_rate: u64,
    /// Time covered by the session totals.
    pub session_duration: Duration,
    /// Number of samples taken in this session.
    pub samples: u64,
}

impl NetworkStatus {
    pub fn connected_count(&self) -> usize {
        self.interfaces.iter().filter(|i| i.connected).count()
    }
    /// The busiest counted interface (by current receive + send rate), or
    /// the first counted one.
    pub fn primary(&self) -> Option<&InterfaceStatus> {
        self.interfaces
            .iter()
            .filter(|i| i.counted)
            .max_by_key(|i| i.rx_rate.unwrap_or(0) + i.tx_rate.unwrap_or(0))
    }
    pub fn average_rates(&self) -> Option<(u64, u64)> {
        let secs = self.session_duration.as_secs_f64();
        (secs > 0.0).then(|| {
            (
                (self.session_rx_bytes as f64 / secs) as u64,
                (self.session_tx_bytes as f64 / secs) as u64,
            )
        })
    }
}

/// An adapter event worth telling the user about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceChange {
    Added(String),
    Removed(String),
    Connected(String),
    Disconnected(String),
}

impl InterfaceChange {
    pub fn message(&self) -> String {
        match self {
            InterfaceChange::Added(n) => format!("Network adapter added: {n}"),
            InterfaceChange::Removed(n) => format!("Network adapter removed: {n}"),
            InterfaceChange::Connected(n) => format!("{n} connected"),
            InterfaceChange::Disconnected(n) => format!("{n} disconnected"),
        }
    }
}

#[derive(Debug, Clone)]
struct Prev {
    rx: u64,
    tx: u64,
    connected: bool,
    name: String,
}

/// Turns snapshots into rates and totals. One tracker per monitoring session.
#[derive(Debug, Default)]
pub struct NetTracker {
    prev_at: Option<Instant>,
    started_at: Option<Instant>,
    prev: HashMap<InterfaceId, Prev>,
    next: HashMap<InterfaceId, Prev>,
    session_rx: u64,
    session_tx: u64,
    peak_rx: u64,
    peak_tx: u64,
    samples: u64,
}

impl NetTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a snapshot. Returns the new status and the changes since the
    /// previous snapshot (none for the very first one).
    pub fn update(&mut self, snap: &Snapshot) -> (NetworkStatus, Vec<InterfaceChange>) {
        let first = self.prev_at.is_none();
        let dt = self.prev_at.and_then(|p| snap.at.checked_duration_since(p));
        self.started_at.get_or_insert(snap.at);
        self.samples += 1;

        // Same-MAC groups among connected hardware interfaces: count only the
        // lowest id (stable across samples, so totals do not jump).
        let mut owner_by_mac: HashMap<u64, (InterfaceId, &str)> = HashMap::new();
        for s in snap.interfaces.iter().filter(|s| s.connected && s.hardware) {
            if let Some(mac) = s.mac.filter(|&m| m != 0) {
                let e = owner_by_mac.entry(mac).or_insert((s.id, &s.name));
                if s.id < e.0 {
                    *e = (s.id, &s.name);
                }
            }
        }
        let duplicate_of = |s: &InterfaceSample| -> Option<String> {
            let mac = s.mac.filter(|&m| m != 0)?;
            let (owner, name) = owner_by_mac.get(&mac)?;
            (s.connected && s.hardware && *owner != s.id).then(|| (*name).to_owned())
        };

        let mut changes = Vec::new();
        let mut interfaces = Vec::with_capacity(snap.interfaces.len());
        let (mut rx_sum, mut tx_sum) = (0u64, 0u64);
        let mut any_rate = false;
        self.next.clear();

        for s in &snap.interfaces {
            let dup = duplicate_of(s);
            let counted = s.connected && s.hardware && dup.is_none();
            let old = self.prev.get(&s.id);
            let (rx_rate, tx_rate) = match (old, dt) {
                (Some(o), Some(dt)) => (rate(o.rx, s.rx_bytes, dt), rate(o.tx, s.tx_bytes, dt)),
                _ => (None, None),
            };
            if let Some(o) = old {
                if counted {
                    // Reset-safe: a counter going backwards contributes 0.
                    self.session_rx += s.rx_bytes.saturating_sub(o.rx);
                    self.session_tx += s.tx_bytes.saturating_sub(o.tx);
                }
                if o.connected != s.connected && !first {
                    changes.push(if s.connected {
                        InterfaceChange::Connected(s.name.clone())
                    } else {
                        InterfaceChange::Disconnected(s.name.clone())
                    });
                }
            } else if !first {
                changes.push(InterfaceChange::Added(s.name.clone()));
            }
            if counted {
                if let (Some(r), Some(t)) = (rx_rate, tx_rate) {
                    rx_sum = rx_sum.saturating_add(r);
                    tx_sum = tx_sum.saturating_add(t);
                    any_rate = true;
                }
            }
            self.next.insert(
                s.id,
                Prev {
                    rx: s.rx_bytes,
                    tx: s.tx_bytes,
                    connected: s.connected,
                    name: s.name.clone(),
                },
            );
            interfaces.push(InterfaceStatus {
                id: s.id,
                name: s.name.clone(),
                description: s.description.clone(),
                kind: s.kind,
                connected: s.connected,
                counted,
                duplicate_of: dup,
                rx_bytes: s.rx_bytes,
                tx_bytes: s.tx_bytes,
                rx_rate,
                tx_rate,
                link_speed_bps: s.link_speed_bps,
            });
        }
        if !first {
            for (id, p) in &self.prev {
                if !self.next.contains_key(id) {
                    changes.push(InterfaceChange::Removed(p.name.clone()));
                }
            }
        }
        std::mem::swap(&mut self.prev, &mut self.next);
        self.prev_at = Some(snap.at);

        // With no counted interface (e.g. everything disconnected) the total
        // rate is a real 0 once we have two samples.
        let (rx_rate, tx_rate) = if dt.is_some() {
            if any_rate {
                (Some(rx_sum), Some(tx_sum))
            } else {
                (Some(0), Some(0))
            }
        } else {
            (None, None)
        };
        if let (Some(r), Some(t)) = (rx_rate, tx_rate) {
            self.peak_rx = self.peak_rx.max(r);
            self.peak_tx = self.peak_tx.max(t);
        }
        // Same order as before; the lower-cased name is computed once per
        // interface instead of twice per comparison.
        interfaces.sort_by_cached_key(|i| (!i.counted, !i.connected, i.name.to_lowercase()));
        changes.sort_by_key(|c| c.message());

        let status = NetworkStatus {
            interfaces,
            rx_rate,
            tx_rate,
            session_rx_bytes: self.session_rx,
            session_tx_bytes: self.session_tx,
            peak_rx_rate: self.peak_rx,
            peak_tx_rate: self.peak_tx,
            session_duration: self
                .started_at
                .and_then(|s| snap.at.checked_duration_since(s))
                .unwrap_or_default(),
            samples: self.samples,
        };
        (status, changes)
    }
}

/// A monitor that reports no interfaces. Used where no platform monitor
/// exists and in engine tests.
#[derive(Debug, Default)]
pub struct NullMonitor;

impl NetworkMonitor for NullMonitor {
    fn name(&self) -> &'static str {
        "null"
    }
    fn sample(&mut self, out: &mut Snapshot) -> Result<(), MonitorError> {
        out.at = Instant::now();
        out.interfaces.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
