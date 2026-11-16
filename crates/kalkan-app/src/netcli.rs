//! `--interfaces` and `--benchmark`: console tools that use the same
//! read-only monitor and engine as the GUI.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use kalkan_config::{Config, LogLevel};
use kalkan_core::{Command, EngineHandle, EngineLink, Event};
use kalkan_net::{NetTracker, NetworkMonitor, Snapshot};
use kalkan_win::WinNetworkMonitor;

fn mib(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn speed(bps: Option<u64>) -> String {
    match bps {
        Some(b) if b >= 1_000_000_000 => format!("{:.1} Gbps", b as f64 / 1e9),
        Some(b) if b >= 1_000_000 => format!("{:.0} Mbps", b as f64 / 1e6),
        Some(b) => format!("{} kbps", b / 1000),
        None => "-".into(),
    }
}

/// Print every relevant interface with its OS counters. The `TOTAL_*` lines
/// are machine-readable (used by CI to verify real counters).
pub fn print_interfaces() -> ExitCode {
    let mut snap = Snapshot::empty();
    if let Err(e) = WinNetworkMonitor::new().sample(&mut snap) {
        eprintln!("error: could not read network interfaces: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "  {:<26} {:<9} {:<13} {:>10} {:>14} {:>14}  ADAPTER",
        "NAME", "TYPE", "STATE", "LINK", "RECEIVED", "SENT"
    );
    // Use the tracker's rules (incl. same-MAC de-duplication) so this output
    // matches what the engine counts.
    let (status, _) = kalkan_net::NetTracker::new().update(&snap);
    let (mut rx, mut tx, mut counted) = (0u64, 0u64, 0);
    for i in &status.interfaces {
        let is_counted = i.counted;
        if is_counted {
            rx += i.rx_bytes;
            tx += i.tx_bytes;
            counted += 1;
        }
        let state = match (i.connected, i.counted, &i.duplicate_of) {
            (true, true, _) => "connected",
            (true, false, Some(_)) => "duplicate",
            (true, false, None) => "up (virtual)",
            (false, _, _) => "disconnected",
        };
        println!(
            "{} {:<26} {:<9} {:<13} {:>10} {:>10.1} MiB {:>10.1} MiB  {}",
            if is_counted { "*" } else { " " },
            i.name,
            i.kind.label(),
            state,
            speed(i.link_speed_bps),
            mib(i.rx_bytes),
            mib(i.tx_bytes),
            match &i.duplicate_of {
                Some(o) => format!("{} (same MAC as {o})", i.description),
                None => i.description.clone(),
            }
        );
    }
    println!("(* = counted in totals: connected hardware interfaces, each MAC once)");
    println!("INTERFACES={}", snap.interfaces.len());
    println!("COUNTED={counted}");
    println!("TOTAL_RX={rx}");
    println!("TOTAL_TX={tx}");
    ExitCode::SUCCESS
}

struct Timing {
    mean_us: f64,
    p50_us: f64,
    p99_us: f64,
    max_us: f64,
    cpu_us_per_call: Option<f64>,
}

impl Timing {
    fn line(&self) -> String {
        format!(
            "mean {:.1} us, p50 {:.1} us, p99 {:.1} us, max {:.1} us, CPU {}",
            self.mean_us,
            self.p50_us,
            self.p99_us,
            self.max_us,
            self.cpu_us_per_call
                .map_or("n/a".into(), |c| format!("{c:.1} us/call"))
        )
    }
}

fn time_calls(n: usize, mut f: impl FnMut()) -> Timing {
    let cpu0 = kalkan_win::process_cpu_time();
    let mut t: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let s = Instant::now();
        f();
        t.push(s.elapsed().as_secs_f64() * 1e6);
    }
    let cpu1 = kalkan_win::process_cpu_time();
    t.sort_by(|a, b| a.total_cmp(b));
    Timing {
        mean_us: t.iter().sum::<f64>() / n as f64,
        p50_us: t[n / 2],
        p99_us: t[(n * 99) / 100],
        max_us: t[n - 1],
        cpu_us_per_call: cpu0
            .zip(cpu1)
            .map(|(a, b)| (b.saturating_sub(a)).as_secs_f64() * 1e6 / n as f64),
    }
}

/// `R` lines are machine-readable (`R <key> <value> <unit>`) so runs can be
/// compared; everything else is for humans.
fn result(key: &str, value: f64, unit: &str) {
    println!("R {key} {value:.3} {unit}");
}

/// Repeat `rounds` timings and print each round plus the mean of the means.
fn timed_rounds(key: &str, label: &str, rounds: usize, n: usize, mut f: impl FnMut()) {
    let mut means = Vec::new();
    let mut cpus = Vec::new();
    let mut p99 = 0f64;
    for r in 1..=rounds {
        let t = time_calls(n, &mut f);
        println!("  {label} round {r}/{rounds} (n={n}): {}", t.line());
        means.push(t.mean_us);
        p99 = p99.max(t.p99_us);
        if let Some(c) = t.cpu_us_per_call {
            cpus.push(c);
        }
    }
    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    result(&format!("{key}.mean_us"), avg(&means), "us");
    result(&format!("{key}.worst_p99_us"), p99, "us");
    if !cpus.is_empty() {
        result(&format!("{key}.cpu_us"), avg(&cpus), "us");
    }
}

/// CPU time used by the whole process during `wait`, while the engine is in
/// the given state. Returns (cpu ms, percent of one core).
fn cpu_over(h: &mut EngineHandle, wait: Duration) -> Option<(f64, f64)> {
    let c0 = kalkan_win::process_cpu_time()?;
    let t0 = Instant::now();
    // Consume events like a front end would (blocking receive, no polling).
    while let Some(left) = wait.checked_sub(t0.elapsed()) {
        let _ = h.recv_timeout(left);
    }
    let c1 = kalkan_win::process_cpu_time()?;
    let ms = c1.saturating_sub(c0).as_secs_f64() * 1e3;
    Some((ms, ms / t0.elapsed().as_secs_f64() / 10.0))
}

fn wait_for(h: &mut EngineHandle, pred: impl Fn(&Event) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match h.recv_timeout(left) {
            Some(e) if pred(&e) => return true,
            Some(_) => {}
            None => return false,
        }
    }
    false
}

fn print_resources(key: &str, label: &str) {
    if let Some(m) = kalkan_win::process_memory() {
        println!(
            "  memory {label}: working set {:.2} MiB, peak {:.2} MiB, private {:.2} MiB",
            mib(m.working_set),
            mib(m.peak_working_set),
            mib(m.private_bytes)
        );
        result(
            &format!("mem.{key}.working_set_mib"),
            mib(m.working_set),
            "MiB",
        );
        result(
            &format!("mem.{key}.private_mib"),
            mib(m.private_bytes),
            "MiB",
        );
    }
    if let Some(h) = kalkan_win::process_handles() {
        println!(
            "  handles {label}: kernel {}, GDI {}, USER {}, threads {}",
            h.kernel_handles, h.gdi_objects, h.user_objects, h.threads
        );
        result(
            &format!("handles.{key}.kernel"),
            h.kernel_handles as f64,
            "",
        );
        result(&format!("handles.{key}.threads"), h.threads as f64, "");
    }
}

/// A synthetic snapshot with `n` interfaces (SIMULATED data for scaling
/// measurements; half hardware, some sharing MACs, some disconnected).
fn synthetic_snapshot(n: usize, tick: u64) -> Snapshot {
    let mut s = Snapshot::empty();
    for i in 0..n as u64 {
        s.interfaces.push(kalkan_net::InterfaceSample {
            id: 1000 + i,
            name: format!("Simulated adapter {i}"),
            description: format!("Simulated NIC model {i} (benchmark only)"),
            kind: kalkan_net::InterfaceKind::Ethernet,
            connected: i % 5 != 4,
            hardware: i % 2 == 0,
            mac: Some(0x0200_0000_0000 + i / 2),
            rx_bytes: tick * (1000 + i * 37),
            tx_bytes: tick * (300 + i * 11),
            link_speed_bps: Some(1_000_000_000),
        });
    }
    s
}

/// Measure monitoring, logging and engine overhead. Results are printed, not
/// asserted: they are observations for the environment the benchmark runs in.
pub fn run_benchmark(config: &Config, spec: kalkan_core::EngineSpec) -> ExitCode {
    const N: usize = 1000;
    const ROUNDS: usize = 3;
    let window = Duration::from_secs(10);
    println!("KALKAN benchmark");
    println!("  environment: {}", kalkan_win::environment_description());
    println!(
        "  build: {} (version {})",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  sample interval: {} ms; rounds per measurement: {ROUNDS}",
        config.monitor.sample_interval_ms
    );
    print_resources("startup", "at start-up");

    // ── Network sampling, split into its parts ──────────────────────────
    let mut mon = WinNetworkMonitor::new();
    let mut snap = Snapshot::empty();
    if let Err(e) = mon.sample(&mut snap) {
        println!("  network sampling: unavailable ({e})");
    } else {
        println!("  real interfaces: {}", snap.interfaces.len());
        result("net.interfaces", snap.interfaces.len() as f64, "");
        timed_rounds("net.os_sample", "GetIfTable2 sample", ROUNDS, N, || {
            let _ = mon.sample(&mut snap);
        });
        let mut tracker = NetTracker::new();
        let mut fixed = snap.clone();
        timed_rounds(
            "net.tracker",
            "tracker update (real snapshot)",
            ROUNDS,
            N,
            || {
                fixed.at += Duration::from_secs(1);
                let _ = std::hint::black_box(tracker.update(&fixed));
            },
        );
        let mut tracker = NetTracker::new();
        timed_rounds(
            "net.sample_total",
            "sample + tracker update",
            ROUNDS,
            N,
            || {
                let _ = mon.sample(&mut snap);
                let _ = std::hint::black_box(tracker.update(&snap));
            },
        );
    }

    // ── Many adapters (SIMULATED snapshots, tracker only) ────────────────
    for n in [1usize, 8, 32, 128] {
        let mut tracker = NetTracker::new();
        let mut tick = 0u64;
        timed_rounds(
            &format!("net.tracker_sim{n}"),
            &format!("tracker update, {n} simulated interfaces"),
            ROUNDS,
            N / 2,
            || {
                tick += 1;
                let mut s = synthetic_snapshot(n, tick);
                s.at += Duration::from_secs(tick);
                let _ = std::hint::black_box(tracker.update(&s));
            },
        );
    }

    timed_rounds(
        "process.sample",
        "process metrics sample",
        ROUNDS,
        N,
        || {
            let _ = kalkan_win::process_cpu_time();
            let _ = kalkan_win::process_memory();
        },
    );

    // ── Logging (the production layer stack, writing a temp file) ────────
    bench_logging(&mut mon);

    // ── Engine CPU: stopped and running ──────────────────────────────────
    let mut h = match EngineHandle::spawn(spec) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: could not start the engine: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cpu_rounds = |h: &mut EngineHandle, key: &str, label: &str| {
        let mut v = Vec::new();
        for r in 1..=ROUNDS {
            match cpu_over(h, window) {
                Some((ms, pct)) => {
                    println!(
                        "  engine {label}, round {r}/{ROUNDS}, {}s: process CPU {ms:.1} ms ({pct:.3} % of one core)",
                        window.as_secs()
                    );
                    v.push(pct);
                }
                None => println!("  engine {label}: CPU time unavailable on this platform"),
            }
        }
        if !v.is_empty() {
            result(
                &format!("{key}.mean_pct"),
                v.iter().sum::<f64>() / v.len() as f64,
                "%",
            );
            result(
                &format!("{key}.max_pct"),
                v.iter().cloned().fold(0.0, f64::max),
                "%",
            );
        }
    };
    cpu_rounds(&mut h, "cpu.engine_stopped", "Stopped");
    h.send(Command::Start);
    if !wait_for(&mut h, |e| matches!(e, Event::EngineStarted)) {
        eprintln!("error: engine did not start");
        return ExitCode::FAILURE;
    }
    cpu_rounds(&mut h, "cpu.engine_running", "Running (sampling)");
    print_resources("running", "engine running");

    // ── Start/stop cycles: memory and threads must return to baseline ───
    const CYCLES: usize = 200;
    h.send(Command::Stop);
    let _ = wait_for(&mut h, |e| matches!(e, Event::EngineStopped));
    print_resources("stopped_before_cycles", "engine stopped, before cycles");
    let t0 = Instant::now();
    for i in 0..CYCLES {
        h.send(Command::Start);
        let started = wait_for(&mut h, |e| matches!(e, Event::EngineStarted));
        h.send(Command::Stop);
        let stopped = wait_for(&mut h, |e| matches!(e, Event::EngineStopped));
        if !(started && stopped) {
            eprintln!("error: start/stop cycle {i} did not complete");
            return ExitCode::FAILURE;
        }
    }
    println!(
        "  {CYCLES} start/stop cycles in {:.2} s",
        t0.elapsed().as_secs_f64()
    );
    print_resources("after_cycles", &format!("after {CYCLES} start/stop cycles"));

    match h.shutdown(Duration::from_secs(10)) {
        Ok(()) => {
            println!("  engine shut down cleanly");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Cost of one log event through the real subscriber (level filter, file
/// writer with rotation, viewer buffer), the cost of the GUI's
/// `debug!(?event)` line for a full network status when the level is Debug,
/// and whether a thread logging flat out slows network sampling down.
fn bench_logging(mon: &mut WinNetworkMonitor) {
    use kalkan_logging::{LogSettings, Output, RotationPolicy};
    const N: usize = 20_000;
    let dir = std::env::temp_dir().join(format!("kalkan-bench-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let settings = |level| LogSettings {
        level,
        output: Output::File(dir.join("kalkan.log")),
        rotation: RotationPolicy::from_mb(5, 2),
    };
    let (dispatch, handle) = match kalkan_logging::build(&settings(LogLevel::Info)) {
        Ok(v) => v,
        Err(e) => {
            println!("  logging: could not create the benchmark log ({e})");
            return;
        }
    };
    tracing::dispatcher::with_default(&dispatch, || {
        let mut i = 0u64;
        timed_rounds("log.info_event", "info event (file + buffer)", 3, N, || {
            i += 1;
            tracing::info!(i, "benchmark log line with a field");
        });
        timed_rounds(
            "log.debug_disabled",
            "debug event at Info level",
            3,
            N,
            || {
                tracing::debug!(i, "filtered out");
            },
        );

        // The GUI logs every engine event at debug level ("engine -> UI").
        let mut snap = Snapshot::empty();
        let net = mon.sample(&mut snap).ok().map(|()| {
            let mut t = NetTracker::new();
            let _ = t.update(&snap);
            snap.at += Duration::from_secs(1);
            t.update(&snap).0
        });
        let ev = Event::Status(kalkan_core::EngineStatus {
            network: net,
            cpu_percent: Some(0.1),
            memory_bytes: Some(1),
            ..Default::default()
        });
        timed_rounds(
            "log.status_event_at_info",
            "debug!(?Status) at Info level",
            3,
            N / 10,
            || {
                tracing::debug!(?ev, "engine -> UI");
            },
        );
        let _ = handle.set_level(LogLevel::Debug);
        timed_rounds(
            "log.status_event_at_debug",
            "debug!(?Status) at Debug level",
            3,
            N / 10,
            || {
                tracing::debug!(?ev, "engine -> UI");
            },
        );
        let _ = handle.set_level(LogLevel::Info);
    });

    // Network sampling while another thread logs as fast as it can.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let logger = {
        let (stop, d) = (stop.clone(), dispatch.clone());
        std::thread::spawn(move || {
            tracing::dispatcher::with_default(&d, || {
                let mut n = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    n += 1;
                    tracing::info!(n, "log flood");
                }
                n
            })
        })
    };
    let mut snap = Snapshot::empty();
    let mut tracker = NetTracker::new();
    let t0 = Instant::now();
    timed_rounds(
        "net.sample_total_under_log_flood",
        "sample + tracker while logging flat out",
        3,
        1000,
        || {
            let _ = mon.sample(&mut snap);
            let _ = std::hint::black_box(tracker.update(&snap));
        },
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let lines = logger.join().unwrap_or(0);
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "  log flood: {lines} events in {secs:.2} s ({:.0} events/s)",
        lines as f64 / secs
    );
    result("log.flood_events_per_s", lines as f64 / secs, "1/s");
    if let Some(st) = handle.file_status() {
        println!(
            "  log file after benchmark: {} rotations, {} archives, last error {:?}, dropped {}",
            st.rotations, st.archives, st.last_error, st.dropped_writes
        );
    }
    handle.close();
    drop(dispatch);
    let _ = std::fs::remove_dir_all(&dir);
}
