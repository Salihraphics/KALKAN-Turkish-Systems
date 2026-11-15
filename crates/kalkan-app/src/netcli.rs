//! `--interfaces` and `--benchmark`: console tools that use the same
//! read-only monitor and engine as the GUI.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use kalkan_config::Config;
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
    let (mut rx, mut tx, mut counted) = (0u64, 0u64, 0);
    for i in &snap.interfaces {
        let is_counted = i.connected && i.hardware;
        if is_counted {
            rx += i.rx_bytes;
            tx += i.tx_bytes;
            counted += 1;
        }
        let state = match (i.connected, i.hardware) {
            (true, true) => "connected",
            (true, false) => "up (virtual)",
            (false, _) => "disconnected",
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
            i.description
        );
    }
    println!("(* = counted in totals: connected hardware interfaces)");
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
    cpu_us_per_call: Option<f64>,
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
        cpu_us_per_call: cpu0
            .zip(cpu1)
            .map(|(a, b)| (b.saturating_sub(a)).as_secs_f64() * 1e6 / n as f64),
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

/// Measure monitoring overhead. Results are printed, not asserted: they are
/// observations for the environment the benchmark runs in.
pub fn run_benchmark(config: &Config, spec: kalkan_core::EngineSpec) -> ExitCode {
    const N: usize = 1000;
    let window = Duration::from_secs(10);
    println!("KALKAN benchmark");
    println!("  platform: {}", kalkan_win::platform_description());
    println!(
        "  sample interval: {} ms",
        config.monitor.sample_interval_ms
    );

    let mut mon = WinNetworkMonitor::new();
    let mut snap = Snapshot::empty();
    if let Err(e) = mon.sample(&mut snap) {
        println!("  network sampling: unavailable ({e})");
    } else {
        let ifaces = snap.interfaces.len();
        let mut tracker = NetTracker::new();
        let t = time_calls(N, || {
            let _ = mon.sample(&mut snap);
            let _ = tracker.update(&snap);
        });
        println!(
            "  network sample + tracker update ({ifaces} interfaces, n={N}): mean {:.1} us, p50 {:.1} us, p99 {:.1} us, CPU {}",
            t.mean_us,
            t.p50_us,
            t.p99_us,
            t.cpu_us_per_call.map_or("n/a".into(), |c| format!("{c:.1} us/sample"))
        );
    }
    let t = time_calls(N, || {
        let _ = kalkan_win::process_cpu_time();
        let _ = kalkan_win::process_memory();
    });
    println!(
        "  process metrics sample (n={N}): mean {:.1} us, p50 {:.1} us, p99 {:.1} us",
        t.mean_us, t.p50_us, t.p99_us
    );

    let mut h = match EngineHandle::spawn(spec) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: could not start the engine: {e}");
            return ExitCode::FAILURE;
        }
    };
    match cpu_over(&mut h, window) {
        Some((ms, pct)) => println!(
            "  engine Stopped, {}s: process CPU {ms:.1} ms ({pct:.3} % of one core)",
            window.as_secs()
        ),
        None => println!("  engine Stopped: CPU time unavailable on this platform"),
    }
    h.send(Command::Start);
    if !wait_for(&mut h, |e| matches!(e, Event::EngineStarted)) {
        eprintln!("error: engine did not start");
        return ExitCode::FAILURE;
    }
    match cpu_over(&mut h, window) {
        Some((ms, pct)) => println!(
            "  engine Running (sampling), {}s: process CPU {ms:.1} ms ({pct:.3} % of one core)",
            window.as_secs()
        ),
        None => println!("  engine Running: CPU time unavailable on this platform"),
    }
    if let Some(m) = kalkan_win::process_memory() {
        println!(
            "  memory: working set {:.1} MiB, peak {:.1} MiB, private {:.1} MiB",
            mib(m.working_set),
            mib(m.peak_working_set),
            mib(m.private_bytes)
        );
    }
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
