//! Scenario tests with simulated adapters. These cover the hardware events
//! (cable pulled, Wi-Fi dropped, USB adapter removed, driver reset) that CI
//! cannot produce physically.

use super::*;

const SEC: Duration = Duration::from_secs(1);

fn iface(id: u64, name: &str, kind: InterfaceKind, rx: u64, tx: u64) -> InterfaceSample {
    InterfaceSample {
        id,
        name: name.into(),
        description: format!("{name} adapter"),
        kind,
        connected: true,
        hardware: true,
        rx_bytes: rx,
        tx_bytes: tx,
        link_speed_bps: Some(1_000_000_000),
    }
}

fn eth(rx: u64, tx: u64) -> InterfaceSample {
    iface(1, "Ethernet", InterfaceKind::Ethernet, rx, tx)
}

fn wifi(rx: u64, tx: u64) -> InterfaceSample {
    iface(2, "Wi-Fi", InterfaceKind::WiFi, rx, tx)
}

/// Feeds snapshots one second apart.
struct Sim {
    t0: Instant,
    n: u32,
    tracker: NetTracker,
}

impl Sim {
    fn new() -> Self {
        Sim {
            t0: Instant::now(),
            n: 0,
            tracker: NetTracker::new(),
        }
    }
    fn step(&mut self, interfaces: Vec<InterfaceSample>) -> (NetworkStatus, Vec<InterfaceChange>) {
        let snap = Snapshot {
            at: self.t0 + SEC * self.n,
            interfaces,
        };
        self.n += 1;
        self.tracker.update(&snap)
    }
}

fn find<'a>(s: &'a NetworkStatus, name: &str) -> &'a InterfaceStatus {
    s.interfaces.iter().find(|i| i.name == name).unwrap()
}

#[test]
fn rate_math() {
    assert_eq!(rate(1_000, 3_000, Duration::from_millis(500)), Some(4_000));
    assert_eq!(rate(0, 10, Duration::ZERO), None);
    assert_eq!(rate(10_000, 5, SEC), Some(0), "counter reset yields 0");
    assert_eq!(rate(0, u64::MAX, Duration::from_nanos(1)), Some(u64::MAX));
}

#[test]
fn first_sample_has_no_rates_and_no_change_events() {
    let mut sim = Sim::new();
    let (s, changes) = sim.step(vec![eth(100, 50), wifi(10, 5)]);
    assert!(
        changes.is_empty(),
        "initial listing is not a change: {changes:?}"
    );
    assert_eq!((s.rx_rate, s.tx_rate), (None, None));
    assert!(s.interfaces.iter().all(|i| i.rx_rate.is_none()));
    assert_eq!(s.session_rx_bytes, 0);
    assert_eq!(s.samples, 1);
}

#[test]
fn multiple_adapters_are_summed() {
    let mut sim = Sim::new();
    sim.step(vec![eth(1_000, 500), wifi(0, 0)]);
    let (s, _) = sim.step(vec![eth(11_000, 2_500), wifi(4_000, 1_000)]);
    assert_eq!(s.rx_rate, Some(14_000));
    assert_eq!(s.tx_rate, Some(3_000));
    assert_eq!(find(&s, "Ethernet").rx_rate, Some(10_000));
    assert_eq!(find(&s, "Wi-Fi").tx_rate, Some(1_000));
    assert_eq!((s.session_rx_bytes, s.session_tx_bytes), (14_000, 3_000));
    assert_eq!(s.peak_rx_rate, 14_000);
    assert_eq!(
        s.primary().unwrap().name,
        "Ethernet",
        "busiest counted interface"
    );
}

#[test]
fn virtual_interfaces_are_shown_but_not_double_counted() {
    let mut sim = Sim::new();
    let vpn = |rx, tx| InterfaceSample {
        hardware: false,
        ..iface(9, "VPN", InterfaceKind::Tunnel, rx, tx)
    };
    sim.step(vec![eth(0, 0), vpn(0, 0)]);
    // VPN traffic also flows (encrypted) through the physical NIC.
    let (s, _) = sim.step(vec![eth(5_000, 1_000), vpn(4_000, 800)]);
    assert_eq!(s.rx_rate, Some(5_000), "only hardware counted");
    let v = find(&s, "VPN");
    assert!(!v.counted && v.connected);
    assert_eq!(v.rx_rate, Some(4_000), "still shown with its own rate");
    assert_eq!(
        s.interfaces[0].name, "Ethernet",
        "counted interfaces sort first"
    );
}

#[test]
fn wifi_disconnect_and_reconnect() {
    let mut sim = Sim::new();
    sim.step(vec![eth(0, 0), wifi(0, 0)]);
    sim.step(vec![eth(1_000, 0), wifi(2_000, 0)]);
    let down = InterfaceSample {
        connected: false,
        ..wifi(2_000, 0)
    };
    let (s, changes) = sim.step(vec![eth(2_000, 0), down]);
    assert_eq!(changes, vec![InterfaceChange::Disconnected("Wi-Fi".into())]);
    let w = find(&s, "Wi-Fi");
    assert!(!w.connected && !w.counted);
    assert_eq!(
        s.rx_rate,
        Some(1_000),
        "disconnected adapter leaves the total"
    );
    assert_eq!(s.connected_count(), 1);

    let (s, changes) = sim.step(vec![eth(3_000, 0), wifi(2_500, 0)]);
    assert_eq!(changes, vec![InterfaceChange::Connected("Wi-Fi".into())]);
    assert_eq!(s.rx_rate, Some(1_500));
    assert_eq!(changes[0].message(), "Wi-Fi connected");
}

#[test]
fn all_interfaces_disconnected_reports_real_zero() {
    let mut sim = Sim::new();
    sim.step(vec![eth(0, 0)]);
    let off = InterfaceSample {
        connected: false,
        ..eth(0, 0)
    };
    let (s, _) = sim.step(vec![off]);
    assert_eq!((s.rx_rate, s.tx_rate), (Some(0), Some(0)));
}

#[test]
fn counter_reset_counts_zero_not_garbage() {
    let mut sim = Sim::new();
    sim.step(vec![eth(10_000_000, 5_000_000)]);
    let (s, changes) = sim.step(vec![eth(1_000, 500)]); // driver reloaded
    assert!(changes.is_empty());
    assert_eq!((s.rx_rate, s.tx_rate), (Some(0), Some(0)));
    assert_eq!(s.session_rx_bytes, 0);
    let (s, _) = sim.step(vec![eth(3_000, 1_500)]);
    assert_eq!(s.rx_rate, Some(2_000), "resumes from the new baseline");
    assert_eq!(s.session_rx_bytes, 2_000);
}

#[test]
fn adapter_removed_and_added_again() {
    let mut sim = Sim::new();
    sim.step(vec![eth(0, 0), wifi(0, 0)]);
    let (s, changes) = sim.step(vec![eth(1_000, 0)]); // USB Wi-Fi unplugged
    assert_eq!(changes, vec![InterfaceChange::Removed("Wi-Fi".into())]);
    assert_eq!(s.interfaces.len(), 1);
    let (s, changes) = sim.step(vec![eth(2_000, 0), wifi(500_000, 0)]); // plugged in
    assert_eq!(changes, vec![InterfaceChange::Added("Wi-Fi".into())]);
    assert_eq!(
        find(&s, "Wi-Fi").rx_rate,
        None,
        "no rate until its second sample"
    );
    assert_eq!(
        s.rx_rate,
        Some(1_000),
        "new adapter's lifetime bytes are not a spike"
    );
    assert_eq!(s.session_rx_bytes, 2_000);
}

#[test]
fn no_interfaces_at_all_is_safe() {
    let mut sim = Sim::new();
    let (s, c) = sim.step(vec![]);
    assert!(s.interfaces.is_empty() && c.is_empty() && s.primary().is_none());
    let (s, c) = sim.step(vec![]);
    assert_eq!(s.rx_rate, Some(0));
    assert!(c.is_empty());
}

#[test]
fn renamed_adapter_keeps_its_counters() {
    let mut sim = Sim::new();
    sim.step(vec![eth(0, 0)]);
    let renamed = InterfaceSample {
        name: "LAN".into(),
        ..eth(1_000, 0)
    };
    let (s, changes) = sim.step(vec![renamed]);
    assert!(changes.is_empty(), "identity is the id, not the name");
    assert_eq!(s.rx_rate, Some(1_000));
}

#[test]
fn session_duration_peaks_and_averages() {
    let mut sim = Sim::new();
    sim.step(vec![eth(0, 0)]);
    sim.step(vec![eth(3_000, 1_000)]);
    let (s, _) = sim.step(vec![eth(4_000, 1_000)]);
    assert_eq!(s.session_duration, 2 * SEC);
    assert_eq!(s.peak_rx_rate, 3_000);
    assert_eq!(s.average_rates(), Some((2_000, 500)));
    assert_eq!(s.samples, 3);
}

#[test]
fn null_monitor_reports_nothing() {
    let mut m = NullMonitor;
    let mut snap = Snapshot::empty();
    snap.interfaces.push(eth(1, 1));
    m.sample(&mut snap).unwrap();
    assert!(snap.interfaces.is_empty());
    let _boxed: Box<dyn NetworkMonitor> = Box::new(NullMonitor);
}
