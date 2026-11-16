# KALKAN Performance (Phase 6, completed)

*KALKAN — A Novera Labs Product. Engine: KOR — Kernel Operations Runtime (user mode; previously GoodBoyDPI).*

## Validation status at a glance

| Status | What |
|---|---|
| **Validated on Windows CI** | Baseline benchmark and 30-minute soaks (runs 28, 32), frame breakdown (run 34); the first GUI optimization, commit `06e2997`: no relayout and no child-control repaints on metrics updates, sidebar skipped, quieter Logs page, status log line at Trace (run 36: benchmark, self-test 63/63, 2-minute and 30-minute soaks 43/43); GUI-mode log file with rotation during the soak (run 32 listing: `kalkan.log` + 6 archives of ≤ 5 MiB) |
| **Validated on Windows CI, run 38** (commits `5731234`, `ed1873c`, `a5b7255`) | Live-block repaint (one update 55 % cheaper than the original path, same process), tracker sort keys, Settings rendering and resize measurements, KOR rename; self-test 63/63, 2-minute soak 15/15, 30-minute soak 43/43 |
| **Simulated** | `tests/soak_sim.rs`: scripted adapters, counter resets and API failures, fake process probe (runs on Linux and in Windows CI) |
| **Not measured at all** | Desktop/laptop hardware, Windows 10, high-DPI monitors, real adapter churn, sessions longer than 30 minutes, a standard user |

This page records how KALKAN's runtime cost was measured, what was found,
what was changed, and the before/after numbers. Every number below comes
from a tool in this repository and a CI run that can be repeated.

> **These are CI measurements, not desktop benchmarks.** All Windows numbers
> come from GitHub Actions `windows-latest` virtual machines: Windows Server
> (build 26100), AMD EPYC 7763, 4 logical processors, x86_64, no GPU, an idle
> virtual display at 96 DPI. Desktop hardware, laptops on battery, high-DPI
> monitors and real Wi-Fi adapters have not been measured.

## Tools

| Tool | What it runs | What it reports |
|---|---|---|
| `kalkan --benchmark` | Headless: the real monitor, tracker, engine and logging stack. About 2 minutes | Environment and build; 3 rounds × n=1000 of `GetIfTable2`, the tracker and both together (mean, p50, p99, max, CPU per call); tracker scaling with 1–128 **simulated** interfaces; cost of a log event through the production subscriber (file + viewer buffer); size and cost of the GUI's status log line; network sampling while another thread logs flat out; engine CPU stopped and running (3 × 10 s each); memory, handles and threads at start-up, while running and after 200 start/stop cycles |
| `kalkan --soak <minutes>` | The real window, engine, monitor and log file (GUI-mode file logging with rotation; in CI run 32 the log file rotated at 5 MiB and kept 6 archives within the default retention of 7) | Per minute: process CPU, working set, private bytes, kernel/GDI/USER handles, threads, main-window paints, owner-drawn control paints, relayouts and metrics fast-path updates, GUI paint time, and **input latency** (a helper thread posts a timestamped message every 100 ms; the delay until the UI thread handles it is how long a click would wait). Activities rotate every 5 minutes: steady monitoring, 20 start/stop cycles, every page + minimize/restore + 4 resizes + DPI 144→96, a 50 000-line log burst with the Logs page open, and a settings save + engine restart. At start: the cost of one frame (back buffer / drawing / blit / owner-drawn controls; validated in runs 34 and 36). Since run 38 also: drawing time per block, one metrics update through the previous and the current code path in the same process, and Settings page rendering and resize handling (below) |
| `tests/soak_sim.rs` | Any OS. The real engine and lifecycle, tracker, GUI model and logging stack; a **scripted** monitor (adapter churn, counter resets, API failures every 41 samples) and a fake process probe | A counting global allocator measures live heap bytes and allocations per sample; heap growth over 5 000 samples, 300 start/stop cycles and a 100 000-line log flood from 4 threads; asserted bounds |

CI runs the benchmark, the simulated test and a 2-minute soak on every push,
and the 30-minute soak as a separate job.

`R <key> <value>` lines in the output are machine-readable so runs can be
compared.

## Measurement caveats

* **CPU time granularity.** Windows charges process CPU in about 15.6 ms
  ticks. A 10 s window can only show 0, 0.156 %, 0.312 %…; a 60 s soak minute
  resolves about 0.026 %. Short-window CPU readings (including the self-test's
  5 s check, which read 1.25 % in one run and 0.00 % in another) are noise at
  KALKAN's load. Per-minute soak averages and direct timings (µs per call,
  ms of paint time) are used instead.
* **Machine-to-machine variance.** The same full-window frame took 2.28 ms in
  CI run 34 and 4.68 ms in run 36 on different runner VMs. Absolute GUI
  timings are therefore only compared **within one run** (the A/B
  measurement) or normalised to that run's own frame cost; work *counts*
  (paints, owner-drawn controls, relayouts per second) do not depend on the
  machine.

## Baseline (before any optimization)

Runs 28 and 32 (code identical to Phase 5 except measurement code), run 34
for the frame breakdown.

### Engine, monitoring, logging (headless benchmark)

| Measurement | Run 28 | Run 32 |
|---|---|---|
| `GetIfTable2` sample, 6 real interfaces | 591 µs wall, 354 µs CPU | 598 µs, 307 µs CPU |
| Tracker update (real snapshot) | 2.8 µs | 2.9 µs |
| Sample + tracker, total | 558 µs wall, 344 µs CPU | 563 µs, 318 µs CPU |
| Tracker, 1 / 8 / 32 / 128 **simulated** interfaces | — (benchmark flaw, see below) | 0.36 / 3.2 / 24 / 119 µs |
| Process metrics sample | 0.6 µs | 0.6 µs |
| One info log event (file + viewer buffer) | 3.6 µs | 3.6 µs |
| GUI's `debug!(?Status)` line at Info level (filtered) | 0.04 µs | 0.04 µs |
| Same line at Debug level | 16.7 µs, about 2.0 KB of text | 16.8 µs |
| Sample + tracker while another thread logs ~280 000 lines/s | 716 µs wall (+28 %) | 720 µs |
| Engine Stopped, 3 × 10 s | 0.0 ms CPU | 0.0 ms |
| Engine Running (sampling at 1 s), 3 × 10 s | 0.0 ms CPU | 0.0–15.6 ms (0.05 % mean) |
| Memory at start-up (no engine) | 7.6 MiB working set, 1.5 MiB private | same |
| Engine running | 9.2 MiB, 2.8 MiB private | 9.2 MiB, 2.7 MiB |
| After 200 start/stop cycles | 9.2 MiB, 2.8 MiB private; 131 handles, 2 threads (same as before the cycles) | same |

The first run's tracker-scaling numbers included building the simulated
snapshots (2 strings per interface) inside the timed loop; the benchmark was
corrected before any code was changed, and only run 32 numbers are used.

**Reading:** at the default 1 s interval, network monitoring costs about
0.34 ms CPU per second, roughly 0.03 % of one core. 99 % of that is the
Windows `GetIfTable2` call itself; the tracker is 2.8 µs. Logging does not
slow monitoring down through locks (the sampler does not log per sample);
the +28 % under a log flood is CPU contention from a thread writing 280 000
lines per second on a 4-core VM, far beyond any real log rate.

### GUI and long-running behaviour (30-minute soak, runs 28 and 32)

| Measurement (steady monitoring, Network page) | Run 28 | Run 32 |
|---|---|---|
| Main-window paints per second | 1.00 | 1.00 |
| Owner-drawn control repaints per second | 5.05 | 5.05 |
| Relayouts per second (every control moved/shown/enabled) | 1.00 | 1.00 |
| GUI paint time per second | 3.9 ms | 5.0 ms |
| One full frame: main window / with children | 2.30 / 2.93 ms | 3.47 / 4.19 ms |
| Process CPU, mean / max of the steady minutes | 0.58 % / 1.43 % | 1.00 % / 1.69 % |
| Input latency, worst p99 / max over all 30 minutes | 0.17 ms / 9.5 ms | 0.17 ms / 14.7 ms |
| Private memory after 1 / 10 / 30 min | 2.6 / 5.7 / 5.8 MiB | 2.5 / 5.0 / 5.3 MiB |
| Growth minute 5 → 25: private / handles / GDI / USER / threads | +0.37 MiB / 0 / 0 / 0 / −3 | +0.79 MiB / 0 / 0 / 0 / −3 |

Frame breakdown (run 34, main window, 2.28 ms): back buffer 0.04 ms (1.6 %),
**drawing 2.06 ms (90 %)**, blit 0.17 ms (7 %); the owner-drawn controls add
0.32 ms. Relayout: 0.14 ms per second.

**Reading:** the private-memory rise in the first minutes is the log viewer's
5 000-entry ring filling up (the soak writes 50 000 lines at minute 3); after
that, memory, handles and GDI/USER objects are flat for 25 minutes. The
"−3 threads" is the soak's own helper threads and finished I/O threads being
joined, not a leak. The UI thread never waited more than 15 ms for input.

## Problems found

| # | Problem | Evidence | Decision |
|---|---|---|---|
| 1 | Every 1 s status repainted the **whole window and all child controls** and re-ran the full relayout (MoveWindow/ShowWindow/EnableWindow on every control), although only numbers and graphs changed | 1 relayout + 5 owner-drawn repaints per second; children 0.32 ms per frame | Fixed (metrics fast path) |
| 2 | The whole page was redrawn every second, including static parts: sidebar, page title, table header area, and a word-wrapped explanatory paragraph | Drawing is 90 % of the frame | Fixed in two steps: sidebar skipped (`06e2997`, run 36); static header and note skipped (`5731234`, run 38) |
| 3 | The Logs page repainted the whole window on every log wake-up even when the view did not change | Code inspection; 3.4 relayouts/s during log bursts | Fixed |
| 4 | At Debug level the GUI logged the full engine status every second: about 2.0 KB per line with 6 adapters, 7 MB of log file per hour, and up to ~10 MB of the viewer's 5 000-entry buffer | Benchmark: 2036 bytes per event | Status dump moved to Trace; other engine events stay at Debug |
| 5 | The tracker sorted interfaces with `to_lowercase()` twice per comparison (2·n·log n string allocations per sample) | Superlinear scaling: 24 µs at 32, 119 µs at 128 simulated interfaces | Fixed (`sort_by_cached_key`, same order); negligible at 6 adapters |
| 6 | A new back buffer is created for every paint | Back buffer is 0.04 ms of a 2.28 ms frame (1.6 %) | **Not changed**: no meaningful benefit |
| 7 | `GetIfTable2` dominates monitoring cost | 99 % of 0.34 ms CPU per sample | **Not changed**: it is the only read-only source of the counters; sampling less or per interface would change what is measured or delay adapter changes |
| 8 | About 237 allocations per sample (engine + tracker + GUI model), 161 of them building the Network page view | Simulated test | **Not changed**: a few µs per second; no measurable benefit without added complexity |

No leak was found: heap, handles, GDI/USER objects and threads are flat in
the 30-minute soak, the simulated 5 000-sample run (heap −5 KiB), 300
simulated start/stop cycles, 200 real start/stop cycles and a 100 000-line
log flood.

## Changes

1. **Metrics fast path** (`win32.rs`, `refresh_metrics`): a status that only
   changes numbers rebuilds the page view and layout; if the layout is
   unchanged, only the main window's page area is invalidated. No control is
   moved, shown, enabled or repainted. State changes, button-state changes
   and layout changes still take the full relayout. *(Pushed in `06e2997`.)*
2. **Sidebar skipped** when it is outside the update region
   (`win32_paint.rs`). *(Pushed in `06e2997`.)*
3. **Logs page**: no repaint when the viewer did not change; a paused viewer
   repaints only the page area so the file status stays live. *(Pushed.)*
4. **Status at Trace**: the periodic status is logged at Trace; other engine
   events at Debug. *(Pushed.)*
5. **Live-block invalidation** (`5731234`, validated in run 38): the fast path invalidates only the
   engine panel, tiles, cards, strip and table; the header, notice and the
   explanatory note are skipped when outside the update region.
6. **Tracker sort keys cached** (`5731234`, `kalkan-net`, validated in run 38): identical order.
7. Measurement code: benchmark, soak, simulated test, per-block paint timing,
   same-process A/B of the old and new update path, Settings and resize timing (`ed1873c`), counters for
   paints, owner-drawn controls, relayouts and fast-path updates. The
   counters are relaxed atomic adds per paint, negligible next to the paint.

Nothing was changed in sampling, its interval, what is counted, the engine's
threads, the configuration defaults or the file format.

## After (CI run 36, commit `06e2997`, changes 1–4)

| Measurement (steady monitoring, Network page) | Baseline (runs 28 / 32 / 34) | After (run 36) |
|---|---|---|
| Owner-drawn control repaints per second | 5.05 | **0.03** |
| Relayouts per second | 1.00 | **0.00** (1.02 fast-path updates) |
| Relayout/update time per second | 0.136 ms (run 34) | **0.056 ms** |
| Main-window paints per second | 1.00 | 1.02 |
| Paint work per second, in frames of that run's own full-window cost | 1.44–1.70 | **1.22** |
| Headless engine/monitor/logging numbers | see above | unchanged (same code) |
| Self-test | 63/63 | 63/63 |

### 30-minute soak after the change (run 36)

| Measurement (steady minutes) | Baseline run 28 | Baseline run 32 | After run 36 |
|---|---|---|---|
| Owner-drawn control repaints per second | 5.05 | 5.05 | **0.03** |
| Relayouts per second | 1.00 | 1.00 | **0.00** (1.00 fast-path) |
| Full frame (main window) in that run | 2.30 ms | 3.47 ms | 3.32 ms |
| GUI paint time per second | 3.9 ms | 5.0 ms | 4.2 ms |
| … in frames of that run's own frame cost | 1.70 | 1.44 | **1.25** |
| Process CPU, mean / max of steady minutes | 0.58 % / 1.43 % | 1.00 % / 1.69 % | 0.47 % / 0.94 % |
| Input latency, worst p99 / max | 0.17 / 9.5 ms | 0.17 / 14.7 ms | 0.20 / 16.6 ms |
| Private memory after 1 / 10 / 30 min | 2.6 / 5.7 / 5.8 MiB | 2.5 / 5.0 / 5.3 MiB | 2.6 / 5.3 / 6.0 MiB |
| Growth minute 5 → 25 (private / handles / GDI / USER) | +0.37 MiB / 0 / 0 / 0 | +0.79 MiB / 0 / 0 / 0 | +0.21 MiB / 0 / 0 / 0 |
| Checks | 43/43 | 43/43 | 43/43 |

**Reading:** the redundant work is gone (child repaints and relayouts on
metrics updates). The paint work per second fell by roughly 15–25 % once
normalised for the runner. Process CPU is within the run-to-run noise of
the VMs (0.47 % vs 0.58 % and 1.00 %), so no CPU improvement is claimed
from these runs; it is below 1 % of one core in every steady minute of all
three soaks. Memory and handles stay flat, and input latency is unchanged
(sub-millisecond p99).

## Settings rendering and resize measurements

Added to `kalkan --soak` (code: `win32_soak.rs`, statistics helper and its
unit tests: `measure.rs`). They run **once at the start of a soak**, after
the existing frame measurements and before the timed minutes, so they do not
affect the per-minute numbers. They drive the same paths a user triggers
(`WM_PAINT`, `relayout`, `SetWindowPos` → `WM_SIZE`), read the existing
always-on counters, and add no timer, no polling and no code to normal
operation. Every operation is followed by a synchronous `RedrawWindow
(RDW_UPDATENOW | RDW_ALLCHILDREN)`, so its paints are included in its time.

| Key | What is timed | n |
|---|---|---|
| `settings.frame_all_children` | Whole window and every child control of the Settings page repainted (number boxes, level segments, switches, buttons) | 100 |
| `settings.frame_main_only` | Main window only (labels, help text, status card, row frames) | 100 |
| `settings.relayout_and_paint` | A relayout of the Settings page (what navigating to it does), then its paints | 100 |
| `resize.network` | One window resize on the live Network page: `SetWindowPos`, the `WM_SIZE` relayout, the resulting paints | 40 |
| `resize.settings` | The same on the Settings page (most child controls) | 40 |

Resizes cycle through 800×650, 1000×740, 860×690 and 960×720 (window size:
distinct, above the minimum, and within a 1024×768 screen so Windows does
not clamp two steps to the same size); the original size is restored
afterwards. Each key
reports mean, p50, p95 and max per operation, and per operation the number
and time of relayouts, main-window paints (and the drawing part) and
owner-drawn control paints, plus drawing time per block (the Settings page
itself is in the `note_pages` block).

**Results (CI run 38, Windows VM; 2-minute soak / 30-minute soak):**

| Key | Mean | p95 | Max | Work per operation (30-min soak) |
|---|---|---|---|---|
| `settings.frame_all_children` | 6.16 / 5.34 ms | 8.10 / 6.34 ms | 9.92 / 6.81 ms | 1 main paint (2.1 ms) + 16 owner-drawn controls |
| `settings.frame_main_only` | 2.15 / 1.96 ms | 3.18 / 2.57 ms | 3.87 / 3.14 ms | 1 main paint, drawing 1.5 ms |
| `settings.relayout_and_paint` | 6.18 / 5.46 ms | 7.91 / 6.31 ms | 9.56 / 6.60 ms | relayout 0.19 ms + paints |
| `resize.network` | 9.09 / 8.39 ms | 11.64 / 10.17 ms | 13.69 / 10.93 ms | relayout 0.11–0.16 ms, main paint ~5 ms, 5 owner-drawn controls |
| `resize.settings` | 14.21 / 11.81 ms | 18.10 / 13.76 ms | 18.71 / 13.90 ms | **relayout 3.5–4.3 ms**, main paint ~2.2 ms, 16 owner-drawn controls |

**Reading:** the Settings page is cheap to draw (main window 2 ms; the 16
owner-drawn controls add about 1.5 ms). A resize of the Network page costs
about one Network frame plus a 0.1 ms relayout. A resize of the Settings
page spends 3.5–4.3 ms in the relayout itself: moving its ~20 child
controls one by one (`MoveWindow` per control). Every mean stays below one
60 Hz frame (16.7 ms); the worst single Settings resize was 18.7 ms in the
2-minute soak. Possible improvement (not implemented, needs approval): move
the controls in one batch with `BeginDeferWindowPos` / `EndDeferWindowPos`.

## Validation of the remaining changes (CI run 38, commit `a5b7255`)

All CI steps passed: Windows unit tests, Clippy with warnings as errors,
formatting, release build, engine tests repeated 25×, smoke test, the real
5 MB counter check, the simulated long-run test, the benchmark, the GUI
self-test (63/63, screenshots now show "KOR — Kernel Operations Runtime"),
the 2-minute soak (15/15) and the 30-minute soak (43/43); the Linux job
(tests, Windows-target Clippy, cross-build) passed too.

**Live-block repaint, same process (no machine variance):**

| One metrics update on the Network page | 2-minute soak | 30-minute soak |
|---|---|---|
| Original path (full relayout, whole window and children) | 6.15 ms; 1 paint, 5 owner-drawn controls | 5.85 ms; 1 paint, 5 owner-drawn controls |
| Current path (live blocks only) | **2.79 ms**; 1 paint, 0 owner-drawn controls | **2.62 ms**; 1 paint, 0 owner-drawn controls |
| Drawing saved per update | sidebar 0.29, header 0.12, note 1.36 ms | sidebar 0.26, header 0.11, note 1.34 ms |

The change is kept: one update is 55 % cheaper, and what remains is the
tiles and the interface table, which do change every second.

**30-minute soak (steady minutes):** paint work 3.58 ms per second, which is
0.78 frames of that run's own full-window cost per second (baseline
1.44–1.70, after `06e2997` 1.25); owner-drawn control repaints 0.03/s;
relayouts 0/s (1 fast-path update per second); process CPU 0.80 % mean,
1.30 % max (within the VMs' run-to-run noise, so no CPU improvement is
claimed); input latency worst p99 0.14 ms, max over 30 minutes 18.8 ms;
private memory 2.5 / 5.7 / 5.9 MiB after 1 / 10 / 30 minutes; growth
minute 5 → 25: +0.41 MiB, kernel handles +2, GDI 0, USER +1, threads −3
(all within the bounds; the memory rise in the first minutes is the log
viewer's 5 000-entry ring filling up).

**Tracker sort keys (benchmark, simulated interfaces):** 32 interfaces
10.3 µs and 128 interfaces 45.6 µs in run 38, against 23.7 µs and 119.4 µs
in the baseline run 32 (different VMs; on one Linux machine the same change
measured −31 % and −45 %). Real 6-adapter tracker update: 2.3 µs.

**Headless numbers (benchmark, run 38):** network sample + tracker 632 µs
wall, 318 µs CPU; one info log event 3.9 µs; engine stopped 0.0 % CPU,
running 0.05 % mean; memory 7.6 MiB at start-up, 9.2 MiB running, unchanged
after 200 start/stop cycles.

## Real-hardware validation still needed

* Idle and monitoring CPU on a real Windows 10 and Windows 11 desktop and a
  laptop on battery (with a CPU-time source finer than 15.6 ms, e.g. ETW or
  Windows Performance Recorder).
* GUI paint cost with a real GPU/driver, at 125–200 % scaling, and with the
  window on a second monitor.
* Real Wi-Fi, USB and VPN adapters appearing and disappearing (only
  simulated so far), and a machine with many adapters (Hyper-V hosts,
  Docker).
* Multi-hour and multi-day sessions (the longest run is 30 minutes).
* A standard (non-elevated) user account (CI runs as administrator).
