//! Engine tests. They use fake probes, so they run on any OS. Timeouts are
//! generous so a loaded CI machine cannot cause false failures; a passing
//! test normally completes in milliseconds.

use super::*;
use crate::lifecycle::next_state;
use std::sync::atomic::AtomicU32;
use std::sync::Mutex;

const T: Duration = Duration::from_secs(10);
const FAST: Duration = Duration::from_millis(20);

#[derive(Default)]
struct FakeProbe {
    n: u32,
    panic_on: Option<u32>,
    fail_on: Option<u32>,
}

impl ProcessProbe for FakeProbe {
    fn sample(&mut self) -> Result<ProcessSample, String> {
        self.n += 1;
        if self.panic_on == Some(self.n) {
            panic!("probe exploded on sample {}", self.n);
        }
        if self.fail_on == Some(self.n) {
            return Err(format!("sample {} failed", self.n));
        }
        Ok(ProcessSample {
            cpu_time: Duration::from_millis(5 * self.n as u64),
            memory_bytes: 1_000_000 + self.n as u64,
        })
    }
}

fn spec_with(
    f: impl Fn() -> Result<Box<dyn ProcessProbe>, String> + Send + Sync + 'static,
) -> EngineSpec {
    EngineSpec {
        sample_interval: FAST,
        probe: Arc::new(f),
        network: None,
    }
}

fn ok_spec() -> EngineSpec {
    spec_with(|| Ok(Box::new(FakeProbe::default())))
}

/// Factory that sleeps during initialisation (to hit the Starting window).
fn slow_spec(delay: Duration) -> EngineSpec {
    spec_with(move || {
        thread::sleep(delay);
        Ok(Box::new(FakeProbe::default()))
    })
}

fn spawn(spec: EngineSpec) -> EngineHandle {
    EngineHandle::spawn(spec).expect("spawn engine")
}

/// Receive events until `pred` matches; returns everything received
/// (including the match). Panics with the events seen on timeout.
fn until(h: &mut EngineHandle, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + T;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match h.recv_timeout(left) {
            Some(ev) => {
                let done = pred(&ev);
                seen.push(ev);
                if done {
                    return seen;
                }
            }
            None => panic!("timed out; events so far: {seen:#?}"),
        }
    }
}

fn states(evs: &[Event]) -> Vec<(EngineState, EngineState)> {
    evs.iter()
        .filter_map(|e| match e {
            Event::StateChanged { from, to } => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

/// Every StateChanged must be a legal transition continuing from the last.
fn assert_legal_sequence(evs: &[Event]) {
    let mut cur = EngineState::Stopped;
    for (from, to) in states(evs) {
        assert_eq!(from, cur, "state chain broken in {evs:#?}");
        assert!(
            Transition::ALL
                .iter()
                .any(|&t| next_state(from, t) == Ok(to)),
            "illegal transition {from} -> {to}"
        );
        cur = to;
    }
}

fn start_running(h: &mut EngineHandle) -> Vec<Event> {
    h.send(Command::Start);
    until(h, |e| matches!(e, Event::EngineStarted))
}

fn stop_stopped(h: &mut EngineHandle) -> Vec<Event> {
    h.send(Command::Stop);
    until(h, |e| matches!(e, Event::EngineStopped))
}

fn shutdown_clean(mut h: EngineHandle) {
    h.send(Command::Shutdown);
    let evs = until(&mut h, |e| matches!(e, Event::ShutdownComplete));
    assert_legal_sequence_from_any(&evs);
    h.shutdown(T).expect("controller joined");
    assert_eq!(h.threads_alive(), 0, "no engine threads may remain");
}

fn assert_legal_sequence_from_any(evs: &[Event]) {
    for (from, to) in states(evs) {
        assert!(
            Transition::ALL
                .iter()
                .any(|&t| next_state(from, t) == Ok(to)),
            "illegal {from} -> {to}"
        );
    }
}

fn diag(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::DiagnosticMessage { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

// ── Lifecycle through the real threads ───────────────────────────────────────

#[test]
fn start_reports_running_only_after_initialisation() {
    let mut h = spawn(slow_spec(Duration::from_millis(50)));
    let evs = start_running(&mut h);
    assert_eq!(
        states(&evs),
        vec![
            (EngineState::Stopped, EngineState::Starting),
            (EngineState::Starting, EngineState::Running)
        ]
    );
    // EngineStarted comes after the Running transition, never before.
    assert!(matches!(evs.last(), Some(Event::EngineStarted)));
    assert_eq!(h.threads_alive(), 2, "controller + worker");
    shutdown_clean(h);
}

#[test]
fn stop_is_confirmed_after_worker_terminates() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    let evs = stop_stopped(&mut h);
    assert_eq!(
        states(&evs),
        vec![
            (EngineState::Running, EngineState::Stopping),
            (EngineState::Stopping, EngineState::Stopped)
        ]
    );
    // EngineStopped is emitted only after the worker was joined.
    assert_eq!(h.threads_alive(), 1, "only the controller remains");
    shutdown_clean(h);
}

#[test]
fn duplicate_start_is_rejected_while_running() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    h.send(Command::Start);
    let evs = until(&mut h, |e| matches!(e, Event::DiagnosticMessage { .. }));
    assert_eq!(
        diag(&evs),
        vec!["Start ignored: the engine is already running."]
    );
    assert!(states(&evs).is_empty(), "no state change: {evs:#?}");
    assert!(evs.iter().all(|e| !matches!(e, Event::EngineStarted)));
    h.send(Command::QueryStatus);
    let evs = until(&mut h, |e| matches!(e, Event::Status(_)));
    assert!(matches!(evs.last(), Some(Event::Status(s)) if s.state == Some(EngineState::Running)));
    assert_eq!(h.threads_alive(), 2, "still exactly one worker");
    shutdown_clean(h);
}

#[test]
fn duplicate_start_is_rejected_while_starting() {
    let mut h = spawn(slow_spec(Duration::from_millis(150)));
    h.send(Command::Start);
    h.send(Command::Start);
    let evs = until(&mut h, |e| matches!(e, Event::EngineStarted));
    assert_eq!(diag(&evs), vec!["Start ignored: the engine is starting."]);
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e, Event::EngineStarted))
            .count(),
        1
    );
    assert_eq!(h.threads_alive(), 2, "exactly one worker");
    shutdown_clean(h);
}

#[test]
fn stop_while_stopped_is_rejected() {
    let mut h = spawn(ok_spec());
    h.send(Command::Stop);
    h.send(Command::QueryStatus);
    let evs = until(&mut h, |e| matches!(e, Event::Status(_)));
    assert_eq!(diag(&evs), vec!["Stop ignored: the engine is not running."]);
    assert!(states(&evs).is_empty());
    shutdown_clean(h);
}

#[test]
fn duplicate_stop_is_rejected_and_stops_once() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    h.send(Command::Stop);
    h.send(Command::Stop); // arrives while Stopping (or already Stopped)
    let mut evs = until(&mut h, |e| matches!(e, Event::EngineStopped));
    // The rejection may be emitted before or after EngineStopped; QueryStatus
    // is processed after both commands, so its reply bounds the history.
    h.send(Command::QueryStatus);
    evs.extend(until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.state == Some(EngineState::Stopped)),
    ));
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e, Event::EngineStopped))
            .count(),
        1
    );
    let d = diag(&evs);
    assert_eq!(d.len(), 1, "{evs:#?}");
    assert!(d[0].starts_with("Stop ignored"), "{d:?}");
    assert_eq!(h.threads_alive(), 1);
    shutdown_clean(h);
}

#[test]
fn stop_during_initialisation_never_reports_running() {
    let mut h = spawn(slow_spec(Duration::from_millis(150)));
    h.send(Command::Start);
    h.send(Command::Stop);
    let evs = until(&mut h, |e| matches!(e, Event::EngineStopped));
    assert_eq!(
        states(&evs),
        vec![
            (EngineState::Stopped, EngineState::Starting),
            (EngineState::Starting, EngineState::Stopping),
            (EngineState::Stopping, EngineState::Stopped)
        ]
    );
    assert!(!evs.iter().any(|e| matches!(e, Event::EngineStarted)));
    assert_eq!(h.threads_alive(), 1);
    shutdown_clean(h);
}

#[test]
fn query_status_when_stopped_has_no_metrics() {
    let mut h = spawn(ok_spec());
    h.send(Command::QueryStatus);
    let evs = until(&mut h, |e| matches!(e, Event::Status(_)));
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert_eq!(s.state, Some(EngineState::Stopped));
    assert!(s.cpu_percent.is_none() && s.memory_bytes.is_none());
    assert!(s.network.is_none() && s.network_error.is_none() && s.running_for.is_none());
    shutdown_clean(h);
}

// ── Failures ─────────────────────────────────────────────────────────────────

#[test]
fn startup_failure_reports_the_real_error_and_allows_retry() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let mut h = spawn(spec_with(move || {
        if c.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("adapter list unavailable (test)".into())
        } else {
            Ok(Box::new(FakeProbe::default()))
        }
    }));
    h.send(Command::Start);
    let evs = until(&mut h, |e| matches!(e, Event::EngineError { .. }));
    assert_eq!(
        states(&evs),
        vec![
            (EngineState::Stopped, EngineState::Starting),
            (EngineState::Starting, EngineState::Failed)
        ]
    );
    let Some(Event::EngineError { message }) = evs.last() else {
        unreachable!()
    };
    assert_eq!(
        message,
        "The engine failed to start: adapter list unavailable (test)"
    );
    assert!(!evs.iter().any(|e| matches!(e, Event::EngineStarted)));
    assert_eq!(h.threads_alive(), 1, "failed worker was joined");

    // Consistent state: Failed accepts Start (retry), which now succeeds.
    let evs = start_running(&mut h);
    assert_eq!(
        states(&evs)[0],
        (EngineState::Failed, EngineState::Starting)
    );
    shutdown_clean(h);
}

#[test]
fn first_sample_failure_is_a_startup_failure() {
    let mut h = spawn(spec_with(|| {
        Ok(Box::new(FakeProbe {
            fail_on: Some(1),
            ..Default::default()
        }))
    }));
    h.send(Command::Start);
    let evs = until(&mut h, |e| matches!(e, Event::EngineError { .. }));
    let Some(Event::EngineError { message }) = evs.last() else {
        unreachable!()
    };
    assert!(
        message.contains("process metrics unavailable: sample 1 failed"),
        "{message}"
    );
    shutdown_clean(h);
}

#[test]
fn panic_during_initialisation_is_reported_and_cleaned_up() {
    let mut h = spawn(spec_with(|| panic!("init blew up (test)")));
    h.send(Command::Start);
    let evs = until(&mut h, |e| matches!(e, Event::EngineError { .. }));
    let Some(Event::EngineError { message }) = evs.last() else {
        unreachable!()
    };
    assert_eq!(message, "The engine crashed: init blew up (test)");
    assert_eq!(
        states(&evs).last(),
        Some(&(EngineState::Starting, EngineState::Failed))
    );
    assert_eq!(h.threads_alive(), 1);
    shutdown_clean(h);
}

#[test]
fn panic_while_running_moves_to_failed() {
    let mut h = spawn(spec_with(|| {
        Ok(Box::new(FakeProbe {
            panic_on: Some(3),
            ..Default::default()
        }))
    }));
    start_running(&mut h);
    let evs = until(&mut h, |e| matches!(e, Event::EngineError { .. }));
    assert_eq!(
        states(&evs),
        vec![(EngineState::Running, EngineState::Failed)]
    );
    let Some(Event::EngineError { message }) = evs.last() else {
        unreachable!()
    };
    assert!(message.contains("probe exploded on sample 3"), "{message}");
    assert_eq!(h.threads_alive(), 1);
    shutdown_clean(h);
}

#[test]
fn sample_error_while_running_is_reported_once_and_not_fatal() {
    let mut h = spawn(spec_with(|| {
        Ok(Box::new(FakeProbe {
            fail_on: Some(2),
            ..Default::default()
        }))
    }));
    start_running(&mut h);
    let evs = until(&mut h, |e| matches!(e, Event::DiagnosticMessage { .. }));
    assert!(diag(&evs)[0].contains("sample 2 failed"));
    // Still running and still sampling afterwards.
    until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.memory_bytes == Some(1_000_003)),
    );
    stop_stopped(&mut h);
    shutdown_clean(h);
}

// ── Metrics ──────────────────────────────────────────────────────────────────

#[test]
fn metrics_are_real_samples_while_running_and_cleared_after_stop() {
    let mut h = spawn(ok_spec());
    let evs = start_running(&mut h);
    assert!(!evs
        .iter()
        .any(|e| matches!(e, Event::Status(s) if s.cpu_percent.is_some())));
    let evs = until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.cpu_percent.is_some()),
    );
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert_eq!(s.state, Some(EngineState::Running));
    assert!(s.memory_bytes.unwrap() > 1_000_000);
    assert!(s.cpu_percent.unwrap() >= 0.0);
    assert!(
        s.network.is_none(),
        "no network monitor configured in this spec"
    );
    assert!(s.running_for.is_some());
    stop_stopped(&mut h);
    h.send(Command::QueryStatus);
    let evs = until(&mut h, |e| matches!(e, Event::Status(_)));
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert!(s.cpu_percent.is_none() && s.memory_bytes.is_none());
    shutdown_clean(h);
}

#[test]
fn cpu_percent_math() {
    let ms = Duration::from_millis;
    assert_eq!(cpu_percent(ms(0), ms(500), ms(1000)), Some(50.0));
    assert_eq!(
        cpu_percent(ms(0), ms(2000), ms(1000)),
        Some(200.0),
        "multi-core"
    );
    assert_eq!(
        cpu_percent(ms(10), ms(5), ms(1000)),
        None,
        "counter went backwards"
    );
    assert_eq!(cpu_percent(ms(0), ms(5), Duration::ZERO), None);
}

// ── GUI communication ────────────────────────────────────────────────────────

#[test]
fn waker_fires_for_events_and_flushes_events_queued_before_it_was_set() {
    let mut h = spawn(ok_spec());
    h.send(Command::QueryStatus); // queued before any waker exists
    let (wtx, wrx) = mpsc::channel::<()>();
    let wtx = Mutex::new(wtx);
    h.set_waker(Box::new(move || {
        let _ = wtx.lock().unwrap().send(());
    }));
    // The flush wake arrives even though no new event was produced.
    wrx.recv_timeout(T).expect("flush wake");
    while wrx.try_recv().is_ok() {}
    // Non-blocking drain, as the GUI does it.
    let mut drained = Vec::new();
    while let Some(e) = h.try_recv() {
        drained.push(e);
    }
    assert!(
        drained.iter().any(|e| matches!(e, Event::Status(_))),
        "{drained:#?}"
    );

    h.send(Command::Start);
    wrx.recv_timeout(T).expect("wake for new events");
    shutdown_clean(h);
}

#[test]
fn try_recv_never_blocks() {
    let mut h = spawn(slow_spec(Duration::from_millis(300)));
    h.send(Command::Start);
    let t0 = Instant::now();
    for _ in 0..1000 {
        let _ = h.try_recv();
    }
    assert!(
        t0.elapsed() < Duration::from_millis(250),
        "try_recv blocked"
    );
    shutdown_clean(h);
}

// ── Shutdown and thread termination ──────────────────────────────────────────

#[test]
fn shutdown_while_running_stops_worker_then_completes() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    h.send(Command::Shutdown);
    let evs = until(&mut h, |e| matches!(e, Event::ShutdownComplete));
    assert_eq!(
        states(&evs),
        vec![
            (EngineState::Running, EngineState::Stopping),
            (EngineState::Stopping, EngineState::Stopped)
        ]
    );
    let stopped = evs
        .iter()
        .position(|e| matches!(e, Event::EngineStopped))
        .unwrap();
    assert_eq!(stopped, evs.len() - 2, "ShutdownComplete is the last event");
    h.shutdown(T).unwrap();
    assert_eq!(h.threads_alive(), 0);
}

#[test]
fn shutdown_while_starting_completes() {
    let mut h = spawn(slow_spec(Duration::from_millis(100)));
    h.send(Command::Start);
    h.send(Command::Shutdown);
    let evs = until(&mut h, |e| matches!(e, Event::ShutdownComplete));
    assert!(!evs.iter().any(|e| matches!(e, Event::EngineStarted)));
    h.shutdown(T).unwrap();
    assert_eq!(h.threads_alive(), 0);
}

#[test]
fn drop_without_shutdown_joins_all_threads() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    let alive = h.alive.clone();
    assert_eq!(alive.load(Ordering::SeqCst), 2);
    drop(h);
    assert_eq!(
        alive.load(Ordering::SeqCst),
        0,
        "drop must join controller and worker"
    );
}

#[test]
fn commands_after_shutdown_report_an_error_instead_of_vanishing() {
    let mut h = spawn(ok_spec());
    h.shutdown(T).unwrap();
    h.send(Command::Start);
    let ev = h.try_recv().expect("local error event");
    assert!(
        matches!(ev, Event::EngineError { ref message } if message.contains("shut down")),
        "{ev:?}"
    );
}

#[test]
fn second_shutdown_is_harmless() {
    let mut h = spawn(ok_spec());
    h.shutdown(T).unwrap();
    h.shutdown(T).unwrap();
}

// ── Repetition / race detection ──────────────────────────────────────────────

#[test]
fn repeated_start_stop_cycles() {
    let mut h = spawn(ok_spec());
    for i in 0..100 {
        let evs = start_running(&mut h);
        assert!(
            states(&evs).contains(&(EngineState::Starting, EngineState::Running)),
            "cycle {i}"
        );
        let evs = stop_stopped(&mut h);
        assert!(
            states(&evs).contains(&(EngineState::Stopping, EngineState::Stopped)),
            "cycle {i}"
        );
        assert_eq!(h.threads_alive(), 1, "cycle {i}: worker leaked");
    }
    shutdown_clean(h);
}

/// Fire a long pseudo-random burst of commands without waiting, then check
/// the whole event history for consistency.
#[test]
fn randomized_command_storm_keeps_lifecycle_consistent() {
    for seed in [1u64, 7, 42, 1234, 99999] {
        let mut h = spawn(spec_with(|| Ok(Box::new(FakeProbe::default()))));
        let mut x = seed;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..500 {
            match rnd() % 7 {
                0..=2 => h.send(Command::Start),
                3..=5 => h.send(Command::Stop),
                _ => h.send(Command::QueryStatus),
            }
            if rnd() % 50 == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        h.send(Command::Shutdown);
        let evs = until(&mut h, |e| matches!(e, Event::ShutdownComplete));
        assert_legal_sequence(&evs);
        let sc = states(&evs);
        let count = |pred: &dyn Fn(&Event) -> bool| evs.iter().filter(|e| pred(e)).count();
        assert_eq!(
            count(&|e| matches!(e, Event::EngineStarted)),
            sc.iter()
                .filter(|t| **t == (EngineState::Starting, EngineState::Running))
                .count(),
            "seed {seed}"
        );
        assert_eq!(
            count(&|e| matches!(e, Event::EngineStopped)),
            sc.iter().filter(|t| t.1 == EngineState::Stopped).count(),
            "seed {seed}"
        );
        assert_eq!(
            count(&|e| matches!(e, Event::EngineError { .. })),
            0,
            "seed {seed}"
        );
        // Running only ever follows Starting.
        assert!(sc
            .iter()
            .all(|t| t.1 != EngineState::Running || t.0 == EngineState::Starting));
        assert!(matches!(evs.last(), Some(Event::ShutdownComplete)));
        h.shutdown(T).unwrap();
        assert_eq!(h.threads_alive(), 0, "seed {seed}");
    }
}

// ── Network monitoring through the engine ────────────────────────────────────

use kalkan_net::{InterfaceKind, InterfaceSample, MonitorError, NetworkMonitor, Snapshot};

/// Scripted monitor: each sample adds `step` bytes to the Ethernet counters;
/// Wi-Fi disappears from sample `drop_wifi_at` on.
struct ScriptedNet {
    n: u64,
    step: u64,
    drop_wifi_at: Option<u64>,
    fail_from: Option<u64>,
}

impl NetworkMonitor for ScriptedNet {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn sample(&mut self, out: &mut Snapshot) -> Result<(), MonitorError> {
        self.n += 1;
        if self.fail_from.is_some_and(|f| self.n >= f) {
            return Err(MonitorError::Os {
                call: "GetIfTable2",
                code: 87,
            });
        }
        out.at = Instant::now();
        out.interfaces.clear();
        let mk = |id, name: &str, kind, rx| InterfaceSample {
            id,
            name: name.into(),
            description: String::new(),
            kind,
            connected: true,
            hardware: true,
            mac: None,
            rx_bytes: rx,
            tx_bytes: rx / 2,
            link_speed_bps: None,
        };
        out.interfaces.push(mk(
            1,
            "Ethernet",
            InterfaceKind::Ethernet,
            self.n * self.step,
        ));
        if self.drop_wifi_at.is_none_or(|d| self.n < d) {
            out.interfaces.push(mk(2, "Wi-Fi", InterfaceKind::WiFi, 0));
        }
        Ok(())
    }
}

fn net_spec(
    make: impl Fn() -> Result<Box<dyn NetworkMonitor>, String> + Send + Sync + 'static,
) -> EngineSpec {
    EngineSpec {
        network: Some(Arc::new(make)),
        ..ok_spec()
    }
}

fn scripted(
    step: u64,
    drop_wifi_at: Option<u64>,
    fail_from: Option<u64>,
) -> Box<dyn NetworkMonitor> {
    Box::new(ScriptedNet {
        n: 0,
        step,
        drop_wifi_at,
        fail_from,
    })
}

#[test]
fn network_rates_and_totals_flow_to_status() {
    let mut h = spawn(net_spec(|| Ok(scripted(10_000, None, None))));
    start_running(&mut h);
    let evs = until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.network.as_ref().is_some_and(|n| n.rx_rate.is_some_and(|r| r > 0))),
    );
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    let n = s.network.as_ref().unwrap();
    assert_eq!(n.interfaces.len(), 2);
    assert!(n.session_rx_bytes >= 10_000);
    assert!(s.network_error.is_none());
    stop_stopped(&mut h);
    h.send(Command::QueryStatus);
    let evs = until(&mut h, |e| matches!(e, Event::Status(_)));
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert!(
        s.network.is_none(),
        "network data is cleared when the engine stops"
    );
    shutdown_clean(h);
}

#[test]
fn adapter_changes_become_info_diagnostics() {
    let mut h = spawn(net_spec(|| Ok(scripted(1, Some(3), None))));
    start_running(&mut h);
    let evs = until(&mut h, |e| matches!(e, Event::DiagnosticMessage { .. }));
    let Some(Event::DiagnosticMessage { level, message }) = evs.last() else {
        unreachable!()
    };
    assert_eq!(
        (*level, message.as_str()),
        (DiagLevel::Info, "Network adapter removed: Wi-Fi")
    );
    shutdown_clean(h);
}

#[test]
fn network_monitor_init_failure_is_not_fatal_and_is_explained() {
    let mut h = spawn(net_spec(|| Err("IP Helper unavailable (test)".into())));
    let evs = start_running(&mut h);
    assert!(states(&evs).contains(&(EngineState::Starting, EngineState::Running)));
    let evs = until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.network_error.is_some()),
    );
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert!(s.network.is_none());
    assert_eq!(
        s.network_error.as_deref(),
        Some("IP Helper unavailable (test)")
    );
    assert!(s.memory_bytes.is_some(), "process metrics still work");
    assert_eq!(
        diag(&evs),
        vec!["Network data unavailable: IP Helper unavailable (test)"]
    );
    shutdown_clean(h);
}

#[test]
fn network_sample_failure_clears_numbers_reports_once_and_keeps_running() {
    let mut h = spawn(net_spec(|| Ok(scripted(100, None, Some(3)))));
    start_running(&mut h);
    let evs = until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.network_error.is_some()),
    );
    let Some(Event::Status(s)) = evs.last() else {
        unreachable!()
    };
    assert!(s.network.is_none(), "no stale numbers after a failure");
    assert!(s.network_error.as_deref().unwrap().contains("GetIfTable2"));
    // Several more failing ticks: still exactly one warning, still Running.
    let mut more = Vec::new();
    for _ in 0..4 {
        more.extend(until(&mut h, |e| matches!(e, Event::Status(_))));
    }
    let all: Vec<_> = evs.iter().chain(more.iter()).cloned().collect();
    assert_eq!(diag(&all).len(), 1, "{:#?}", diag(&all));
    assert!(states(&more).is_empty());
    shutdown_clean(h);
}

#[test]
fn running_for_is_reported_by_the_engine() {
    let mut h = spawn(ok_spec());
    start_running(&mut h);
    thread::sleep(Duration::from_millis(60));
    h.send(Command::QueryStatus);
    let evs = until(
        &mut h,
        |e| matches!(e, Event::Status(s) if s.running_for.is_some_and(|d| d >= Duration::from_millis(50))),
    );
    assert!(!evs.is_empty());
    shutdown_clean(h);
}
