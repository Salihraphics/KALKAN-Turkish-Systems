//! Simulated long-running stability test (runs on any OS).
//!
//! **Everything here is simulated**: a scripted network monitor adds and
//! removes adapters, resets counters and fails on purpose; the process probe
//! is fake. What is real: the engine threads and lifecycle, the tracker, the
//! GUI model (`UiState`, `page_view`, `layout::compute`) that runs on every
//! update, and the logging stack (file rotation + viewer buffer).
//!
//! A counting global allocator measures live heap bytes and allocation
//! counts, so memory growth is checked directly rather than through the OS
//! working set. Run with `--nocapture` to see the numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kalkan_core::{
    Command, EngineHandle, EngineLink, EngineSpec, Event, ProcessProbe, ProcessSample,
};
use kalkan_gui::layout::{self, Content};
use kalkan_gui::model::{page_view, Page, UiState};
use kalkan_gui::AppInfo;
use kalkan_net::{InterfaceKind, InterfaceSample, MonitorError, NetworkMonitor, Snapshot};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn live_kib() -> f64 {
    LIVE.load(Ordering::Relaxed) as f64 / 1024.0
}

struct FakeProbe(u64);
impl ProcessProbe for FakeProbe {
    fn sample(&mut self) -> Result<ProcessSample, String> {
        self.0 += 1;
        Ok(ProcessSample {
            cpu_time: Duration::from_micros(300 * self.0),
            memory_bytes: 20 << 20,
        })
    }
}

/// Six stable adapters (two sharing a MAC), plus a USB adapter that comes
/// and goes, a VPN that connects and disconnects, a counter reset every 97
/// samples and an API failure every 41 samples.
struct ScriptedMonitor {
    n: u64,
}

impl NetworkMonitor for ScriptedMonitor {
    fn name(&self) -> &'static str {
        "scripted (simulated)"
    }
    fn sample(&mut self, out: &mut Snapshot) -> Result<(), MonitorError> {
        self.n += 1;
        let n = self.n;
        if n % 41 == 0 {
            return Err(MonitorError::Os {
                call: "GetIfTable2 (simulated)",
                code: 8,
            });
        }
        out.at = Instant::now();
        out.interfaces.clear();
        let reset = if n % 97 == 0 { 0 } else { n };
        for i in 0..6u64 {
            out.interfaces.push(InterfaceSample {
                id: 10 + i,
                name: format!("Ethernet {i}"),
                description: format!("Simulated NIC {i}"),
                kind: InterfaceKind::Ethernet,
                connected: i != 5,
                hardware: i < 4,
                mac: Some(0xAA00 + i.min(2)),
                rx_bytes: reset * 15_000 * (i + 1),
                tx_bytes: reset * 3_000 * (i + 1),
                link_speed_bps: Some(1_000_000_000),
            });
        }
        if (n / 10) % 2 == 0 {
            // A different USB adapter each time it reappears (new id + name),
            // which is what grows maps if removals are not cleaned up.
            let k = n / 20;
            out.interfaces.push(InterfaceSample {
                id: 1_000 + k,
                name: format!("USB adapter #{k}"),
                description: "Simulated USB NIC".into(),
                kind: InterfaceKind::Ethernet,
                connected: true,
                hardware: true,
                mac: Some(0xBB00 + k),
                rx_bytes: n * 100,
                tx_bytes: n * 10,
                link_speed_bps: None,
            });
        }
        out.interfaces.push(InterfaceSample {
            id: 99,
            name: "VPN".into(),
            description: "Simulated tunnel".into(),
            kind: InterfaceKind::Tunnel,
            connected: (n / 7) % 2 == 0,
            hardware: false,
            mac: None,
            rx_bytes: n * 500,
            tx_bytes: n * 50,
            link_speed_bps: None,
        });
        Ok(())
    }
}

fn spec(interval: Duration) -> EngineSpec {
    EngineSpec {
        sample_interval: interval,
        probe: Arc::new(|| Ok(Box::new(FakeProbe(0)) as Box<dyn ProcessProbe>)),
        network: Some(Arc::new(|| {
            Ok(Box::new(ScriptedMonitor { n: 0 }) as Box<dyn NetworkMonitor>)
        })),
    }
}

fn info() -> AppInfo {
    AppInfo {
        version: "test".into(),
        ..AppInfo::default()
    }
}

/// What the GUI does for every engine event: apply it, then build the view
/// and layout of the visible page.
struct Gui {
    ui: UiState,
    info: AppInfo,
    statuses: u64,
    diagnostics: u64,
    /// Allocations made while building the view + layout (this thread).
    view_allocs: u64,
}

impl Gui {
    fn on(&mut self, ev: Event) {
        match &ev {
            Event::Status(_) => self.statuses += 1,
            Event::DiagnosticMessage { .. } => self.diagnostics += 1,
            _ => {}
        }
        self.ui.apply(ev);
        // Like the window: only the visible page is rebuilt.
        let a0 = ALLOCS.load(Ordering::Relaxed);
        {
            let v = page_view(self.ui.page, &self.ui, &self.info);
            let c = Content {
                engine_panel: v.engine_panel,
                notice: self.ui.visible_notice().is_some(),
                tiles: v.tiles.len(),
                cards: v.cards.len(),
                strip: !v.strip.is_empty(),
                table_rows: v.table.as_ref().map(|t| t.rows.len()),
                note: v.note.is_some(),
                header_buttons: v.header_buttons,
                log_viewer: v.log_viewer,
                settings_rows: v.settings_rows,
            };
            std::hint::black_box(layout::compute(1040, 680, 96, c));
        }
        self.view_allocs += ALLOCS.load(Ordering::Relaxed) - a0;
    }

    /// Consume events until `n` statuses have been applied.
    fn run_statuses(&mut self, h: &mut EngineHandle, n: u64) {
        let target = self.statuses + n;
        while self.statuses < target {
            match h.recv_timeout(Duration::from_secs(10)) {
                Some(ev) => self.on(ev),
                None => panic!("engine stopped sending events"),
            }
        }
    }

    fn wait(&mut self, h: &mut EngineHandle, pred: impl Fn(&Event) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .expect("timeout");
            let ev = h.recv_timeout(left).expect("timeout waiting for event");
            let hit = pred(&ev);
            self.on(ev);
            if hit {
                return;
            }
        }
    }
}

#[test]
fn simulated_long_run_has_no_memory_or_thread_growth() {
    let dir = std::env::temp_dir().join(format!("kalkan-soak-sim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let logging = kalkan_logging::init(&kalkan_logging::LogSettings {
        level: kalkan_config::LogLevel::Info,
        output: kalkan_logging::Output::File(dir.join("kalkan.log")),
        rotation: kalkan_logging::RotationPolicy::from_mb(1, 2),
    })
    .expect("logging");

    let mut h = EngineHandle::spawn(spec(Duration::from_millis(1))).expect("engine");
    let mut gui = Gui {
        ui: UiState::default(),
        info: info(),
        statuses: 0,
        diagnostics: 0,
        view_allocs: 0,
    };
    gui.ui.navigate(Page::Network);
    h.send(Command::Start);
    gui.wait(&mut h, |e| matches!(e, Event::EngineStarted));

    // Phase 1: warm-up (every adapter/failure pattern seen at least once,
    // graph history and log buffer filled).
    gui.run_statuses(&mut h, 1_000);
    for i in 0..6_000 {
        tracing::info!(i, "warm-up line");
    }
    let warm = live_kib();

    // Phase 2: long simulated run, measuring allocations per update.
    // 5 000 samples cover every churn/failure pattern many times (periods
    // 7-97 samples). Windows timers wait ~15.6 ms for a 1 ms timeout, so
    // more would make CI slow without testing anything new.
    const SAMPLES: u64 = 5_000;
    let (a0, v0, t0) = (
        ALLOCS.load(Ordering::Relaxed),
        gui.view_allocs,
        Instant::now(),
    );
    gui.run_statuses(&mut h, SAMPLES);
    let allocs = ALLOCS.load(Ordering::Relaxed) - a0;
    let view = gui.view_allocs - v0;
    let secs = t0.elapsed().as_secs_f64();
    let after_run = live_kib();
    println!(
        "simulated run: {SAMPLES} samples in {secs:.1} s, {} diagnostics, {:.1} allocations per sample (engine + tracker + GUI model, all threads)",
        gui.diagnostics,
        allocs as f64 / SAMPLES as f64
    );
    println!(
        "R sim.allocs_per_sample {:.2}",
        allocs as f64 / SAMPLES as f64
    );
    println!(
        "R sim.view_allocs_per_sample {:.2} (Network page view + layout, part of the total)",
        view as f64 / SAMPLES as f64
    );
    println!("live heap: warm {warm:.1} KiB, after {SAMPLES} samples {after_run:.1} KiB");

    // Phase 3: start/stop cycles.
    for _ in 0..300 {
        h.send(Command::Stop);
        gui.wait(&mut h, |e| matches!(e, Event::EngineStopped));
        h.send(Command::Start);
        gui.wait(&mut h, |e| matches!(e, Event::EngineStarted));
    }
    gui.run_statuses(&mut h, 200);
    let after_cycles = live_kib();
    println!("live heap after 300 start/stop cycles: {after_cycles:.1} KiB");

    // Phase 4: log flood from 4 threads while monitoring continues.
    let flood: Vec<_> = (0..4)
        .map(|t| {
            std::thread::spawn(move || {
                for i in 0..25_000 {
                    tracing::info!(t, i, "simulated log flood");
                }
            })
        })
        .collect();
    gui.run_statuses(&mut h, 500);
    for f in flood {
        f.join().unwrap();
    }
    gui.run_statuses(&mut h, 500);
    let after_flood = live_kib();
    let st = logging.file_status().expect("file status");
    println!(
        "live heap after 100k log lines: {after_flood:.1} KiB; log file rotations {}, archives {}, last error {:?}, buffer {} entries (capacity {})",
        st.rotations,
        st.archives,
        st.last_error,
        logging.buffer().snapshot().len(),
        logging.buffer().capacity()
    );

    // Phase 5: config change + restart (sample interval 2 ms).
    h.send(Command::SetSampleInterval(Duration::from_millis(250)));
    h.send(Command::Stop);
    gui.wait(&mut h, |e| matches!(e, Event::EngineStopped));
    h.send(Command::Start);
    gui.wait(&mut h, |e| matches!(e, Event::EngineStarted));
    gui.run_statuses(&mut h, 3);

    h.shutdown(Duration::from_secs(10)).expect("clean shutdown");
    drop(h);
    let after_shutdown = live_kib();
    println!("live heap after engine shutdown: {after_shutdown:.1} KiB");
    println!("R sim.heap_growth_run_kib {:.1}", after_run - warm);
    println!("R sim.heap_growth_cycles_kib {:.1}", after_cycles - warm);
    println!("R sim.heap_growth_flood_kib {:.1}", after_flood - warm);

    // Bounds: the run touches tens of thousands of distinct adapter names,
    // so any per-adapter leak would be far larger than these.
    assert!(
        after_run - warm < 128.0,
        "heap grew by {:.1} KiB during the run",
        after_run - warm
    );
    assert!(
        after_cycles - warm < 128.0,
        "heap grew by {:.1} KiB over start/stop cycles",
        after_cycles - warm
    );
    assert!(
        after_flood - warm < 256.0,
        "heap grew by {:.1} KiB after the log flood",
        after_flood - warm
    );
    assert!(
        after_shutdown < after_flood,
        "engine shutdown released its memory"
    );
    assert!(st.last_error.is_none(), "{:?}", st.last_error);
    assert!(st.rotations > 0 && st.archives <= 2);
    assert_eq!(
        kalkan_core::live_engine_threads(),
        0,
        "no engine threads left"
    );
    assert!(
        gui.diagnostics > 100,
        "adapter changes and failures were reported"
    );
    logging.close();
    let _ = std::fs::remove_dir_all(&dir);
}
