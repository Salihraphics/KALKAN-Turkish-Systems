//! Long-running soak test: `kalkan --soak <minutes>`.
//!
//! Drives the real window, engine, network monitor and logger for the given
//! time and records, once per minute: process CPU, memory, kernel/GDI/USER
//! handles, threads, paint work, and input latency (a helper thread posts a
//! timestamped message every 100 ms; the delay until the UI thread handles it
//! is how long a click or key press would have waited).
//!
//! Each minute has one activity, repeating every five minutes:
//! 0 steady monitoring (Network page) · 1 twenty start/stop cycles ·
//! 2 every page, minimize/restore, resizes, DPI change · 3 a burst of 50 000
//! log lines while the Logs page is open · 4 settings save + engine restart.
//! "Steady" minutes are the clean measurement points.
//!
//! The numbers are observations of the machine the soak ran on, not a
//! benchmark of user hardware.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_MINIMIZE,
    SW_RESTORE, WM_APP, WM_CLOSE,
};

use kalkan_core::EngineState;

use super::{
    click, pump, run_message_loop_for, simulate_dpi, wait_until, with_state, with_state_mut,
};
use crate::model::Page;
use crate::{Check, Resources, SelfTestReport};

/// Timestamped "is the UI thread free?" probe (soak only).
pub(super) const WM_APP_PING: u32 = WM_APP + 4;

// Work counters. Always on: one relaxed atomic add per paint is negligible
// next to the paint itself, and it lets the soak see redundant redraws.
pub(super) static PAINTS: AtomicU64 = AtomicU64::new(0);
pub(super) static PAINT_NS: AtomicU64 = AtomicU64::new(0);
pub(super) static DRAW_ITEMS: AtomicU64 = AtomicU64::new(0);
pub(super) static DRAW_ITEM_NS: AtomicU64 = AtomicU64::new(0);
pub(super) static RELAYOUTS: AtomicU64 = AtomicU64::new(0);
pub(super) static RELAYOUT_NS: AtomicU64 = AtomicU64::new(0);
// WM_PAINT breakdown: back-buffer setup, drawing, BitBlt to the window.
pub(super) static PAINT_SETUP_NS: AtomicU64 = AtomicU64::new(0);
pub(super) static PAINT_DRAW_NS: AtomicU64 = AtomicU64::new(0);
pub(super) static PAINT_BLIT_NS: AtomicU64 = AtomicU64::new(0);

static PING_BASE: OnceLock<Instant> = OnceLock::new();
static PINGS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// WM_APP_PING handler: record how long the message waited.
pub(super) fn on_ping(wp: WPARAM) {
    if let Some(base) = PING_BASE.get() {
        let now = base.elapsed().as_micros() as u64;
        let waited = now.saturating_sub(wp.0 as u64).min(u32::MAX as u64) as u32;
        if let Ok(mut v) = PINGS.lock() {
            v.push(waited);
        }
    }
}

fn take_pings() -> Vec<u32> {
    let mut v = PINGS
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    v.sort_unstable();
    v
}

fn pct(v: &[u32], p: usize) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v[(v.len() - 1) * p / 100] as f64 / 1000.0
}

#[derive(Clone, Copy, Default)]
struct Counters {
    paints: u64,
    paint_ns: u64,
    draw_items: u64,
    draw_item_ns: u64,
    relayouts: u64,
    relayout_ns: u64,
}

fn counters() -> Counters {
    Counters {
        paints: PAINTS.load(Ordering::Relaxed),
        paint_ns: PAINT_NS.load(Ordering::Relaxed),
        draw_items: DRAW_ITEMS.load(Ordering::Relaxed),
        draw_item_ns: DRAW_ITEM_NS.load(Ordering::Relaxed),
        relayouts: RELAYOUTS.load(Ordering::Relaxed),
        relayout_ns: RELAYOUT_NS.load(Ordering::Relaxed),
    }
}

/// One row of the soak table.
#[derive(Clone)]
struct Minute {
    index: u32,
    activity: &'static str,
    ok: bool,
    cpu_pct: f64,
    res: Option<Resources>,
    paints_per_s: f64,
    draw_items_per_s: f64,
    relayouts_per_s: f64,
    gui_ms_per_s: f64,
    relayout_ms_per_s: f64,
    ping_p50_ms: f64,
    ping_p99_ms: f64,
    ping_max_ms: f64,
    log_entries: usize,
}

const MINUTE: Duration = Duration::from_secs(60);
const STEP: Duration = Duration::from_secs(15);

fn mib(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

pub(super) fn soak(
    hwnd: HWND,
    minutes: u32,
    resources: Option<fn() -> Option<Resources>>,
) -> SelfTestReport {
    let mut r = SelfTestReport::default();
    let mut check = |r: &mut SelfTestReport, name: String, passed: bool| {
        tracing::info!(passed, "soak: {name}");
        r.checks.push(Check { name, passed });
    };
    let note = |r: &mut SelfTestReport, line: String| {
        println!("{line}");
        r.notes.push(line);
    };
    let res = || resources.and_then(|f| f());
    pump();
    let Some((nav, start, stop)) = with_state(hwnd, |s| (s.nav, s.start_btn, s.stop_btn)) else {
        check(&mut r, "window state available".into(), false);
        post_close(hwnd);
        return r;
    };
    let net_live = |s: &super::WindowState| {
        s.ui.engine == EngineState::Running
            && s.ui
                .status
                .network
                .as_ref()
                .is_some_and(|n| n.rx_rate.is_some())
    };

    note(
        &mut r,
        format!("soak: {minutes} minute(s), real window, engine, monitor and logger"),
    );
    let r_start = res();

    // ── Start monitoring on the Network page ─────────────────────────────
    click(start);
    let live = wait_until(hwnd, STEP, net_live);
    check(&mut r, "engine running with live network data".into(), live);
    click(nav[Page::Network.index()]);
    run_message_loop_for(hwnd, Duration::from_secs(3));

    // ── Paint cost of one frame on the live Network page ─────────────────
    for (key, flags) in [
        (
            "frame_all_children",
            RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
        ),
        ("frame_main_only", RDW_INVALIDATE | RDW_UPDATENOW),
    ] {
        const N: u32 = 200;
        let parts = |s: &AtomicU64| s.load(Ordering::Relaxed);
        let (s0, d0, b0, i0) = (
            parts(&PAINT_SETUP_NS),
            parts(&PAINT_DRAW_NS),
            parts(&PAINT_BLIT_NS),
            parts(&DRAW_ITEM_NS),
        );
        let t0 = Instant::now();
        for _ in 0..N {
            // SAFETY: synchronous repaint of our own window; no borrow is held.
            unsafe {
                let _ = RedrawWindow(Some(hwnd), None, None, flags);
            }
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3 / N as f64;
        let per = |a: u64, b: u64| (a - b) as f64 / 1e6 / N as f64;
        let (setup, draw, blit, items) = (
            per(parts(&PAINT_SETUP_NS), s0),
            per(parts(&PAINT_DRAW_NS), d0),
            per(parts(&PAINT_BLIT_NS), b0),
            per(parts(&DRAW_ITEM_NS), i0),
        );
        note(&mut r, format!("soak: paint {key}: {ms:.3} ms per frame (n={N}): back buffer {setup:.3}, drawing {draw:.3}, blit {blit:.3}, owner-drawn controls {items:.3} ms"));
        note(&mut r, format!("R soak.paint.{key}_ms {ms:.4} ms"));
        note(&mut r, format!("R soak.paint.{key}_draw_ms {draw:.4} ms"));
    }

    // ── Input-latency probe ──────────────────────────────────────────────
    let base = *PING_BASE.get_or_init(Instant::now);
    let _ = take_pings();
    let stop_ping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pinger = {
        let (stop_ping, target) = (stop_ping.clone(), hwnd.0 as usize);
        std::thread::Builder::new()
            .name("kalkan-soak-ping".into())
            .spawn(move || {
                while !stop_ping.load(Ordering::Relaxed) {
                    let t = base.elapsed().as_micros() as usize;
                    // SAFETY: PostMessageW is thread-safe; a stale handle
                    // only makes the call fail.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_PING,
                            WPARAM(t),
                            LPARAM(0),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok()
    };

    // ── The minutes ──────────────────────────────────────────────────────
    let mut rows: Vec<Minute> = Vec::new();
    let mut minimized_cpu: Option<f64> = None;
    for m in 0..minutes {
        let activity = match m % 5 {
            0 => "steady monitoring",
            1 => "20 start/stop cycles",
            2 => "pages, minimize, resize, DPI",
            3 => "50k log lines, Logs page",
            _ => "settings save + engine restart",
        };
        let t0 = Instant::now();
        let c0 = counters();
        let cpu0 = res().map(|x| x.cpu_time);
        let _ = take_pings();
        let ok = match m % 5 {
            0 => true,
            1 => start_stop_cycles(hwnd, start, stop, 20),
            2 => {
                let (ok, cpu) = ui_churn(hwnd, nav, resources);
                minimized_cpu = minimized_cpu.or(cpu);
                ok
            }
            3 => log_burst(hwnd, nav, 50_000),
            _ => settings_restart(hwnd, nav, start, stop),
        };
        // Back to steady monitoring on the Network page for the rest of the
        // minute (also restarts the engine if an activity left it stopped).
        if with_state(hwnd, |s| s.ui.engine) != Some(EngineState::Running) {
            click(start);
        }
        click(nav[Page::Network.index()]);
        let ok = ok && wait_until(hwnd, STEP, net_live);
        if let Some(left) = MINUTE.checked_sub(t0.elapsed()) {
            run_message_loop_for(hwnd, left);
        }
        let wall = t0.elapsed().as_secs_f64();
        let c1 = counters();
        let now = res();
        let pings = take_pings();
        let row = Minute {
            index: m,
            activity,
            ok,
            cpu_pct: match (cpu0, now.map(|x| x.cpu_time)) {
                (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() / wall * 100.0,
                _ => f64::NAN,
            },
            res: now,
            paints_per_s: (c1.paints - c0.paints) as f64 / wall,
            draw_items_per_s: (c1.draw_items - c0.draw_items) as f64 / wall,
            relayouts_per_s: (c1.relayouts - c0.relayouts) as f64 / wall,
            gui_ms_per_s: ((c1.paint_ns - c0.paint_ns) + (c1.draw_item_ns - c0.draw_item_ns))
                as f64
                / 1e6
                / wall,
            relayout_ms_per_s: (c1.relayout_ns - c0.relayout_ns) as f64 / 1e6 / wall,
            ping_p50_ms: pct(&pings, 50),
            ping_p99_ms: pct(&pings, 99),
            ping_max_ms: pct(&pings, 100),
            log_entries: with_state(hwnd, |s| s.pages.log.total_len()).unwrap_or(0),
        };
        let line = format_row(&row);
        tracing::info!("soak: {line}");
        note(&mut r, format!("soak: {line}"));
        note(
            &mut r,
            format!("R soak.m{m:02}.cpu_pct {:.3} %", row.cpu_pct),
        );
        if let Some(x) = row.res {
            note(
                &mut r,
                format!("R soak.m{m:02}.private_mib {:.2} MiB", mib(x.private_bytes)),
            );
            note(
                &mut r,
                format!(
                    "R soak.m{m:02}.working_set_mib {:.2} MiB",
                    mib(x.working_set)
                ),
            );
        }
        note(
            &mut r,
            format!("R soak.m{m:02}.paints_per_s {:.2}", row.paints_per_s),
        );
        note(
            &mut r,
            format!(
                "R soak.m{m:02}.draw_items_per_s {:.2}",
                row.draw_items_per_s
            ),
        );
        note(
            &mut r,
            format!("R soak.m{m:02}.gui_ms_per_s {:.3} ms", row.gui_ms_per_s),
        );
        note(
            &mut r,
            format!("R soak.m{m:02}.ping_p99_ms {:.2} ms", row.ping_p99_ms),
        );
        check(&mut r, format!("minute {m}: {activity} completed"), ok);
        rows.push(row);
    }
    stop_ping.store(true, Ordering::Relaxed);
    if let Some(t) = pinger {
        let _ = t.join();
    }
    if let Some(c) = minimized_cpu {
        note(&mut r, format!("R soak.minimized_cpu_pct {c:.3} %"));
    }

    // ── Summary and leak checks ──────────────────────────────────────────
    summarize(&mut r, &rows, r_start, &mut check, &note);

    click(stop);
    let stopped = wait_until(hwnd, STEP, |s| s.ui.engine == EngineState::Stopped);
    check(
        &mut r,
        "engine stops at the end of the soak".into(),
        stopped,
    );
    post_close(hwnd);
    r
}

fn format_row(m: &Minute) -> String {
    let (ws, private, kernel, gdi, user, threads) =
        m.res.map_or((f64::NAN, f64::NAN, 0, 0, 0, 0), |x| {
            (
                mib(x.working_set),
                mib(x.private_bytes),
                x.kernel_handles,
                x.gdi_objects,
                x.user_objects,
                x.threads,
            )
        });
    format!(
        "min {:>2} {:<31} {} cpu {:6.3} % | ws {:6.2} MiB private {:6.2} MiB | handles {} gdi {} user {} threads {} | paints {:5.2}/s drawitems {:6.2}/s relayouts {:5.2}/s ({:.3} ms/s) gui {:6.3} ms/s | input latency p50 {:.2} p99 {:.2} max {:.2} ms | log entries {}",
        m.index,
        m.activity,
        if m.ok { "ok  " } else { "FAIL" },
        m.cpu_pct,
        ws,
        private,
        kernel,
        gdi,
        user,
        threads,
        m.paints_per_s,
        m.draw_items_per_s,
        m.relayouts_per_s,
        m.relayout_ms_per_s,
        m.gui_ms_per_s,
        m.ping_p50_ms,
        m.ping_p99_ms,
        m.ping_max_ms,
        m.log_entries
    )
}

fn summarize(
    r: &mut SelfTestReport,
    rows: &[Minute],
    start: Option<Resources>,
    check: &mut impl FnMut(&mut SelfTestReport, String, bool),
    note: &impl Fn(&mut SelfTestReport, String),
) {
    let steady: Vec<&Minute> = rows.iter().filter(|m| m.index % 5 == 0).collect();
    if let Some(s) = start {
        note(
            r,
            format!(
                "soak: at start-up (before the engine ran): ws {:.2} MiB, private {:.2} MiB, handles {}, gdi {}, user {}, threads {}",
                mib(s.working_set),
                mib(s.private_bytes),
                s.kernel_handles,
                s.gdi_objects,
                s.user_objects,
                s.threads
            ),
        );
    }
    for want in [0u32, 10, 30] {
        // Resources after `want` minutes = end of minute want-1 (or the
        // first minute for 0).
        let idx = want.saturating_sub(1);
        if let Some(m) = rows.iter().find(|m| m.index == idx) {
            if let Some(x) = m.res {
                note(
                    r,
                    format!(
                        "soak: after {:>2} min: ws {:.2} MiB, private {:.2} MiB, handles {}, gdi {}, user {}, threads {}",
                        idx + 1,
                        mib(x.working_set),
                        mib(x.private_bytes),
                        x.kernel_handles,
                        x.gdi_objects,
                        x.user_objects,
                        x.threads
                    ),
                );
            }
        }
    }
    if !steady.is_empty() {
        let n = steady.len() as f64;
        let cpu = steady.iter().map(|m| m.cpu_pct).sum::<f64>() / n;
        let cpu_max = steady.iter().map(|m| m.cpu_pct).fold(0.0, f64::max);
        let p99 = steady.iter().map(|m| m.ping_p99_ms).fold(0.0, f64::max);
        let gui = steady.iter().map(|m| m.gui_ms_per_s).sum::<f64>() / n;
        let paints = steady.iter().map(|m| m.paints_per_s).sum::<f64>() / n;
        let items = steady.iter().map(|m| m.draw_items_per_s).sum::<f64>() / n;
        let relayout = steady.iter().map(|m| m.relayout_ms_per_s).sum::<f64>() / n;
        note(
            r,
            format!("R soak.steady.relayout_ms_per_s {relayout:.3} ms"),
        );
        note(r, format!("R soak.steady.cpu_mean_pct {cpu:.3} %"));
        note(r, format!("R soak.steady.cpu_max_pct {cpu_max:.3} %"));
        note(r, format!("R soak.steady.ping_worst_p99_ms {p99:.2} ms"));
        note(r, format!("R soak.steady.gui_ms_per_s {gui:.3} ms"));
        note(r, format!("R soak.steady.paints_per_s {paints:.2}"));
        note(r, format!("R soak.steady.draw_items_per_s {items:.2}"));
        check(
            r,
            format!(
                "steady monitoring CPU {cpu:.3} % mean, {cpu_max:.3} % max (sanity bound < 5 %)"
            ),
            cpu_max < 5.0,
        );
        check(
            r,
            format!(
                "input latency during steady monitoring: worst p99 {p99:.2} ms (bound < 100 ms)"
            ),
            p99 < 100.0,
        );
    }
    let worst = rows.iter().map(|m| m.ping_max_ms).fold(0.0, f64::max);
    note(r, format!("R soak.all.ping_max_ms {worst:.2} ms"));

    // Leak checks: compare the first steady minute after one full activity
    // cycle (all buffers filled, every code path run once) with the last
    // steady minute.
    let first = steady.iter().find(|m| m.index >= 5).or(steady.first());
    let last = steady.last();
    if let (Some(a), Some(b)) = (first, last) {
        if let (Some(x), Some(y)) = (a.res, b.res) {
            let grow = mib(y.private_bytes) - mib(x.private_bytes);
            let dh = y.kernel_handles as i64 - x.kernel_handles as i64;
            let dg = y.gdi_objects as i64 - x.gdi_objects as i64;
            let du = y.user_objects as i64 - x.user_objects as i64;
            let dt = y.threads as i64 - x.threads as i64;
            note(r, format!("R soak.growth.private_mib {grow:.3} MiB"));
            note(r, format!("R soak.growth.kernel_handles {dh}"));
            note(r, format!("R soak.growth.gdi {dg}"));
            note(r, format!("R soak.growth.user {du}"));
            note(r, format!("R soak.growth.threads {dt}"));
            let span = format!("minute {} -> {}", a.index, b.index);
            check(
                r,
                format!("private memory growth {grow:+.2} MiB, {span} (bound < 4 MiB)"),
                grow < 4.0,
            );
            check(
                r,
                format!("kernel handle growth {dh:+}, {span} (bound <= 20)"),
                dh <= 20,
            );
            check(
                r,
                format!("GDI object growth {dg:+}, {span} (bound <= 5)"),
                dg <= 5,
            );
            check(
                r,
                format!("USER object growth {du:+}, {span} (bound <= 5)"),
                du <= 5,
            );
            check(
                r,
                format!("thread count change {dt:+}, {span} (bound <= 1)"),
                dt <= 1,
            );
        }
    }
}

fn post_close(hwnd: HWND) {
    // SAFETY: posting to our own window; the message loop handles the
    // two-step close.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn start_stop_cycles(hwnd: HWND, start: HWND, stop: HWND, n: usize) -> bool {
    for _ in 0..n {
        click(stop);
        if !wait_until(hwnd, STEP, |s| s.ui.engine == EngineState::Stopped) {
            return false;
        }
        click(start);
        if !wait_until(hwnd, STEP, |s| s.ui.engine == EngineState::Running) {
            return false;
        }
    }
    true
}

/// Every page; 10 s minimized (CPU measured); resizes; DPI 144 → 96.
fn ui_churn(
    hwnd: HWND,
    nav: [HWND; 5],
    resources: Option<fn() -> Option<Resources>>,
) -> (bool, Option<f64>) {
    for &b in &nav {
        click(b);
        run_message_loop_for(hwnd, Duration::from_millis(500));
    }
    click(nav[Page::Network.index()]);
    let cpu = || resources.and_then(|f| f()).map(|x| x.cpu_time);
    // SAFETY: plain ShowWindow on our own top-level window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
    let (c0, t0) = (cpu(), Instant::now());
    run_message_loop_for(hwnd, Duration::from_secs(10));
    let minimized = match (c0, cpu()) {
        (Some(a), Some(b)) => {
            Some(b.saturating_sub(a).as_secs_f64() / t0.elapsed().as_secs_f64() * 100.0)
        }
        _ => None,
    };
    // SAFETY: as above.
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    pump();
    for (w, h) in [(900, 700), (1300, 850), (800, 640), (1100, 760)] {
        // SAFETY: resizing our own window; no borrow is held.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        run_message_loop_for(hwnd, Duration::from_millis(300));
    }
    simulate_dpi(hwnd, 144);
    run_message_loop_for(hwnd, Duration::from_millis(500));
    simulate_dpi(hwnd, 96);
    run_message_loop_for(hwnd, Duration::from_millis(500));
    let ok = with_state(hwnd, |s| {
        let (w, h) = s.client_size();
        s.dpi == 96 && (s.layout.client.w, s.layout.client.h) == (w, h)
    })
    .unwrap_or(false);
    (ok, minimized)
}

/// High log volume from another thread while the Logs page is shown. Passes
/// when the viewer catches up and the UI stays responsive (measured by the
/// latency probe for this minute).
fn log_burst(hwnd: HWND, nav: [HWND; 5], lines: u32) -> bool {
    click(nav[Page::Logs.index()]);
    let t0 = Instant::now();
    let writer = std::thread::Builder::new()
        .name("kalkan-soak-log".into())
        .spawn(move || {
            for i in 0..lines {
                tracing::info!(i, "soak: high-volume log line");
            }
        });
    let Ok(writer) = writer else { return false };
    while !writer.is_finished() {
        run_message_loop_for(hwnd, Duration::from_millis(100));
    }
    let _ = writer.join();
    let secs = t0.elapsed().as_secs_f64();
    tracing::info!(lines, secs, "soak: log burst written");
    // The viewer shows the newest entry once it has caught up.
    let caught_up = wait_until(hwnd, STEP, |s| {
        s.pages
            .logging
            .as_ref()
            .is_some_and(|l| Some(l.buffer().version()) == s.pages.log.version())
    });
    let file_ok = with_state(hwnd, |s| {
        s.pages
            .logging
            .as_ref()
            .and_then(|l| l.file_status())
            .is_none_or(|st| st.last_error.is_none())
    })
    .unwrap_or(false);
    caught_up && file_ok
}

/// Change the sample interval in Settings, save (real file, I/O thread),
/// restart the engine so it applies, then restore the original value.
fn settings_restart(hwnd: HWND, nav: [HWND; 5], start: HWND, stop: HWND) -> bool {
    click(nav[Page::Settings.index()]);
    let Some((edit, save, original)) = with_state(hwnd, |s| {
        (
            s.pages.ctl.edits[2],
            s.pages.ctl.save,
            s.pages.settings.saved.monitor.sample_interval_ms,
        )
    }) else {
        return false;
    };
    let mut ok = true;
    for value in [500u32, original] {
        super::set_edit_text(edit, &value.to_string());
        click(save);
        ok &= wait_until(hwnd, STEP, |s| {
            !s.pages.settings.saving && s.pages.settings.saved.monitor.sample_interval_ms == value
        });
        click(stop);
        ok &= wait_until(hwnd, STEP, |s| s.ui.engine == EngineState::Stopped);
        click(start);
        ok &= wait_until(hwnd, STEP, |s| s.ui.engine == EngineState::Running);
    }
    let _ = with_state_mut(hwnd, |s| s.ui.page_notice = None);
    ok
}
