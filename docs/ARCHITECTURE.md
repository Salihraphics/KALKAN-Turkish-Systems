# KALKAN Architecture

*KALKAN — A Novera Labs Product. Engine: KOR — Kernel Operations Runtime.*

## Goals that shape the design

1. **Lightweight.** Few dependencies, no async runtime unless it is justified, and no polling while idle.
2. **Responsive UI.** The UI thread never does network or disk work. It only sends commands and draws events.
3. **Read-only by default.** Monitoring observes OS counters. It never modifies traffic.
4. **Contained unsafe code.** Every Win32 `unsafe` call lives in `kalkan-win`, or in `kalkan-gui`'s `win32` module. All other crates, including the engine in `kalkan-core`, use `#![forbid(unsafe_code)]`.
5. **Replaceable parts.** A new network module can be added behind the engine without changing the GUI.

## Workspace layout

```
kalkan/
├── Cargo.toml                 workspace, shared deps, lints, release profile
├── .cargo/config.toml         mingw linker for Linux→Windows cross builds
├── .github/workflows/ci.yml   Windows MSVC + Linux cross-compile CI
├── docs/ARCHITECTURE.md
├── docs/GUI.md               GUI design, layout, honesty rules, self-test
├── assets/                   logo: source PNG, 512 px PNG, kalkan.ico (embedded in the exe)
└── crates/
    ├── kalkan-app/      binary `kalkan`: argument parsing, wiring, startup/shutdown
    ├── kalkan-gui/      native Win32 UI: model/layout/theme (portable) + win32 (Windows)
    ├── kalkan-core/     KOR engine (threads), lifecycle, Command/Event, EngineLink
    ├── kalkan-net/      read-only NetworkMonitor trait, counters, rate math
    ├── kalkan-config/   TOML schema, defaults, validation, atomic save, app dirs
    ├── kalkan-logging/  tracing subscriber, size-rotated log file, in-memory buffer, history
    └── kalkan-win/      Windows integration (privileges, process CPU/memory, console)
```

### Dependency graph (enforced by Cargo.toml)

```
                 kalkan-app
     ┌──────┬──────┼───────┬────────┬─────────┐
     ▼      ▼      ▼       ▼        ▼         ▼
    gui ─► core ─► net ◄── win    config ◄── logging
     │                              ▲           ▲
     └──────────────────────────────┴───────────┘   (Phase 5)
```

* `kalkan-gui` depends on `kalkan-core` for the engine, and since Phase 5 also on `kalkan-config` and `kalkan-logging`. **Reason:** the Settings page must validate with exactly the rules the loader uses (`Config::validate`) and save with the same atomic writer, and the Logs page reads the live `LogBuffer` and changes the level and rotation at runtime through `LoggingHandle`. Duplicating either in the GUI would let them drift. The GUI still never touches `kalkan-net` or `kalkan-win`; it sees network data only through types `kalkan-core` re-exports.
* `kalkan-net` holds the read-only monitor trait and the platform-independent tracker. `kalkan-win` implements the trait for Windows. Only the app connects the two.
* `kalkan-core`, `kalkan-net` and `kalkan-config` are platform independent and are unit tested on any OS.
* `kalkan-win` pulls in the `windows` crate only under `cfg(windows)`.

## The engine: KOR — Kernel Operations Runtime (`kalkan-core::engine`)

KOR is KALKAN's background engine. **Today it manages its own lifecycle, reads the Windows interface counters (read-only, `GetIfTable2`) and measures KALKAN's own CPU and memory use. It contains no DPI circumvention, packet capture, packet injection or packet modification.**

* **User mode only.** KOR is a set of ordinary threads inside `kalkan.exe`. "Kernel" in *Kernel Operations Runtime* is part of the name; KOR does not run in Windows kernel mode, does not use or install a kernel driver, and is not a Windows kernel component.
* **Naming.** KOR was previously named *GoodBoyDPI*. In code: `ENGINE_NAME` = "KOR", `ENGINE_TITLE` = "KOR — Kernel Operations Runtime" (engine panel), `ENGINE_SCOPE` = "Read-only monitoring only. No DPI circumvention or packet processing.", the Settings help text "Start the KOR engine when KALKAN opens", and the worker thread `WORKER_THREAD_NAME` = `kor-core`. *This rename is implemented locally and has not run on Windows yet.*
* **Compatibility.** No configuration key contains the engine name, so existing `kalkan.toml` files are unaffected. Log files written before the rename contain the thread name `goodboydpi-core`; the log viewer's history parser does not depend on thread names and loads both (unit test `history_parses_old_and_new_engine_thread_names`).

### Threads

| Thread | Lifetime | Job | How it waits |
|---|---|---|---|
| UI (main) | app | Win32 message loop | `GetMessageW` |
| `kalkan-engine` (controller) | app (spawned at start-up, joined at exit) | Sole owner of the `Lifecycle`. Turns commands and worker reports into events | `mpsc::Receiver::recv()` |
| `kor-core` (worker; `goodboydpi-core` before the rename) | from Start to Stop | Initialisation, then periodic process metrics and network samples | `recv_timeout(sample_interval)` on its control channel |
| `kalkan-log-history` | once at start-up, a few ms | Reads the tail (≤ 512 KiB) of the previous log into the viewer | exits when done; joined at shutdown |
| `kalkan-io` | per Save / Reset / Export click | Writes `kalkan.toml` or an exported log file, so the UI never blocks on disk | exits after posting `WM_APP_IO`; joined at shutdown |

While the engine is stopped, two threads exist: the UI and the controller. The controller sleeps in `recv()`. While the engine is running, the worker wakes once per `monitor.sample_interval_ms` (default 1 s), reads `GetProcessTimes` and `GetProcessMemoryInfo`, and sleeps again. Nothing in the engine busy-waits or polls.

### Message flow

```
 UI thread                 controller "kalkan-engine"            worker "kor-core"
 ─────────                 ──────────────────────────            ────────────────────────
 Start click ─Command────► Stopped → Starting  (StateChanged)
                           spawn worker ───────────────────────► init: create probe,
                                                                  first sample
                           ◄──────────────────── Ready ──────────┘
 WM_APP_ENGINE ◄─waker──── Starting → Running  (StateChanged, EngineStarted)
 drain try_recv()          ◄──────────────────── Sample ───────── every interval
                           Status{cpu, memory}
 Stop click ──Command────► Running → Stopping ── Stop ──────────► loop ends, probe dropped
                           ◄──────────────────── Exited ───────── (drop guard, also on panic)
                           join worker
                           Stopping → Stopped  (StateChanged, EngineStopped)
 WM_CLOSE ──Shutdown─────► stop + join worker (≤ 5 s) → ShutdownComplete → thread exits
 DestroyWindow, then drop EngineHandle → join controller
```

* **Events have one producer.** Only the controller emits events, so the order is deterministic. Every `StateChanged` is a legal transition, and a randomized test with 2 500 commands checks this.
* **Waking the GUI.** The engine calls a waker that posts `WM_APP_ENGINE`. An atomic flag coalesces bursts into one message. The GUI then drains the queue with non-blocking `try_recv()`. Installing the waker triggers one immediate wake, so events queued earlier are not lost.
* **Running is reported only after initialisation succeeds.** `Started` is the only transition into `Running`, and the controller applies it only when the worker reports `Ready`.
* **Stop during Starting.** The state goes `Starting → Stopping`. A late `Ready` is ignored, and the engine never reports Running.
* **Stale messages.** Each worker message carries a generation number. Messages from an earlier worker are dropped.
* **Failures.** Possible causes are an `Err` from the probe factory, a failing first sample, or a panic anywhere in the worker. Each produces `→ Failed` and `EngineError` with the real message, for example `The engine failed to start: …` or `The engine crashed: …`. The worker is always joined, and `Start` from `Failed` retries.
* **Invalid commands** (Start while Starting or Running, Stop while Stopped or Stopping) are rejected. The state does not change, a `DiagnosticMessage` (warning) is emitted, and a `WARN` log line names the command and state.
* **Shutdown is bounded.** The controller waits at most 5 s for the worker, then reports an error and detaches it, because there is no safe way to kill a thread. The GUI waits at most 7 s for `ShutdownComplete`.
* **No orphaned threads.** `ThreadCount` guards maintain a per-handle count and a process-wide count (`live_engine_threads()`). Unit tests and the Windows self-test check that both reach 0 after shutdown.

### Lifecycle

```
             Start                 Started
   Stopped ─────────► Starting ─────────────► Running
     ▲  ▲               │   │                    │
     │  │      Stopped  │   │ Stop               │ Stop
     │  └───────────── Stopping ◄────────────────┘
     │                  │   ▲
     │ Start       Fail │   │
     └───────── Failed ◄┴───┴── Fail (from Starting, Running, Stopping)
```

`kalkan_core::lifecycle::next_state` is a pure function with exactly 9 legal transitions. Tests check all 25 state/transition pairs. The GUI's Start and Stop buttons are enabled from the same table through `can_start()` and `can_stop()`, and both are disabled while the app is closing.

### Protocol

| Commands (GUI → engine) | Events (engine → GUI) |
|---|---|
| `Start`, `Stop`, `QueryStatus`, `Shutdown`, `SetSampleInterval(Duration)` (Phase 5; 250 ms–60 s, out-of-range values are rejected with a warning; takes effect at the next Start) | `StateChanged{from,to}`, `EngineStarted`, `EngineStopped`, `EngineError{message}`, `DiagnosticMessage{level,message}`, `Status(EngineStatus)`, `ShutdownComplete` |

`EngineStatus` contains `cpu_percent`, `memory_bytes`, `network: Option<NetworkStatus>`, `network_error: Option<String>` and `running_for`. Each field is `Some` only when measured, and all of them are cleared whenever the engine leaves `Running`. Adapter changes arrive as `DiagnosticMessage` (Info), for example "Wi-Fi disconnected".

## Network monitoring (read-only)

```
 kor-core worker, once per sample_interval (default 1 s):
   WinNetworkMonitor::sample(&mut snapshot)      kalkan-win  (GetIfTable2 → FreeMibTable)
     drops: NDIS filter rows, loopback, down non-hardware interfaces
     fills: id (LUID), alias, description, kind, connected, hardware, MAC, In/OutOctets, link speed
   NetTracker::update(&snapshot)                 kalkan-net  (pure, unit-tested)
     per-interface rates (reset-safe), counted = connected ∧ hardware ∧ first-of-its-MAC
     totals, session bytes, peaks, InterfaceChange (Added/Removed/Connected/Disconnected)
   → WorkerMsg::Sample { process, net } → controller → Event::Status + DiagnosticMessage(s)
```

* **Read-only by construction.** The trait has a single `sample(&mut Snapshot)` method. There is no API for capturing, sending or modifying traffic. No admin rights or drivers are needed.
* **Counting rules**, chosen to avoid double counting:
  1. NDIS lightweight-filter rows repeat their NIC's counters, so they are dropped.
  2. Non-hardware (VPN, virtual) interfaces are shown but not counted.
  3. Connected hardware interfaces that share a **MAC address** count once. The lowest LUID wins, which keeps the choice stable across samples. The other interfaces are marked `duplicate_of`.

  Rule 3 was added after CI run 13 measured a 5 MB download as 10 MB on an Azure VM. A Hyper-V synthetic NIC and its Mellanox SR-IOV VF both reported the traffic. CI now checks both bounds of a real 5,000,000-byte download (run 15: +5,054,232 bytes).
* **Edge cases** (simulated-adapter tests in `kalkan-net/src/tests.rs`):
  - A counter that goes backwards produces 0 for that interval.
  - A new adapter has no rate until its second sample, and its lifetime bytes never appear as session traffic.
  - With every adapter disconnected, the rate is a real 0.
  - Renaming an adapter keeps its identity, which is the LUID.
  - An empty list is safe.
* **Failures.** If the monitor cannot be created, or a sample fails, the engine keeps running. `network_error` carries the reason, which the GUI shows. A warning is emitted once per failure streak, and stale numbers are never shown.
* **Cost.** Measured on the CI VM with 6 interfaces: 0.42 ms CPU per sample, about 0.04 % of one core at 1 Hz. Sampling shares the worker's existing tick. While the engine is stopped, nothing runs.

## Logging (Phase 5, `kalkan-logging`)

```
 any thread: tracing::info!(…)
   └─► Registry
        ├─ reload::Layer<LevelFilter>      level changed at runtime (Settings → Log level)
        ├─ fmt layer ─► SharedFile          one Mutex<RotatingFile>; one write_all per line
        │               or stderr           (console runs: CLI options, self-test)
        └─ BufferLayer ─► LogBuffer         ring of 5 000 Arc<LogEntry>, version counter,
                                            waker → PostMessageW(WM_APP_LOG), coalesced
```

* **Files.** GUI runs write `%LOCALAPPDATA%\Kalkan\logs\kalkan.log` (UTC timestamps). Before a line would push it past `logging.max_file_size_mb` (1–100, default 5), it is closed and renamed to `kalkan.1.log`. Older archives shift to `kalkan.2.log`, and so on. Archives numbered above `logging.max_log_files` (1–90, default 7) are deleted.
* **Safety rules:**
  - Only names that parse exactly as `kalkan.<number>.log` are ever deleted. The active file, `kalkan.log.old`, `kalkan.0.log` and unrelated files are never touched; a test checks this.
  - After a restart the existing file is continued (its size is re-read). A file that is already over the limit is rotated at open.
  - Rotation happens under the same lock as the write, so lines are never split or interleaved. An 8-thread test checks every line in every file after ~90 rotations.
  - If a rename fails (for example, another program holds the file open), the error is recorded, logging continues into the active file, and rotation is retried only after another quarter of the limit has been written, so a stuck file does not cost a rename attempt per line.
  - `close()` at shutdown flushes the file; later events are counted as dropped writes instead of being written to a closed handle.
  - Lowering retention in Settings prunes extra archives immediately.
* **Errors are visible.** The last I/O error, rotation count, size, archives and dropped writes are in `FileStatus`, which the Logs page shows. If the log file cannot be opened at start-up, KALKAN logs to stderr and says why.
* **Viewer data.** `LogBuffer` keeps the newest 5 000 entries; older ones are evicted and counted. The previous run's lines are parsed from the file on `kalkan-log-history` and shown as *history* (muted). Lines that cannot be parsed are skipped and counted; indented continuation lines are joined to their entry. A missing file is not an error.

## Configuration

* File: `%APPDATA%\Kalkan\kalkan.toml`. Console runs log to stderr.
* Unknown keys are rejected so typos surface. All range errors are reported together.
* **Backward compatible.** Keys added in Phase 5 (`logging.max_file_size_mb`) have serde defaults, so Phase 1–4 files load unchanged; a test checks this.
* A missing file means defaults are used.
* **Invalid file (Phase 5 change).** `--check-config` and `--benchmark` still exit with the full list of problems. A GUI start no longer stops: it uses defaults, logs a warning, and the Settings page shows the error. The file is **not touched** until the user saves. Saving first copies the invalid file to `kalkan.toml.bak` (`.bak.2`, `.bak.3`, … if one exists; a backup is never overwritten), so user settings are never silently discarded.
* Saving is atomic (write a temp file, then rename) and runs on the `kalkan-io` thread.
* Settings that apply immediately: log level, file size limit, archives kept. Sample interval applies at the next engine start (`SetSampleInterval`). File logging on/off, start engine on launch and start minimized apply at the next launch. The save confirmation names every changed setting that applies later.
* `start_engine_on_launch` defaults to `false`, so the engine does nothing until the user presses Start. If it is set to `true`, the GUI sends Start right after its window is created.

## Performance (Phase 6)

Measured with `kalkan --benchmark`, `kalkan --soak <minutes>` and
`crates/kalkan-gui/tests/soak_sim.rs`; methodology, numbers and limits are
in [PERFORMANCE.md](PERFORMANCE.md). The architecture was not changed:

* The engine, monitor and tracker were already cheap (about 0.34 ms CPU per
  1 s sample, 99 % of it in `GetIfTable2`; validated on Windows CI). The only
  change there is the tracker computing each interface's sort key once
  instead of per comparison (same order). *Measured on Linux only; not yet
  run on Windows CI.*
* The GUI kept its event-driven design (no periodic timers, no polling).
  Metrics-only updates no longer move or repaint child controls and skip the
  sidebar (*validated on Windows CI, run 36*). Repainting only the blocks
  that show live data is the next step (*local, not yet run on Windows*);
  see [GUI.md](GUI.md).
* No unbounded collection was found: graph history is a fixed 60-sample
  ring, the log viewer a 5 000-entry ring, the tracker's maps are rebuilt
  from each snapshot (removed adapters are dropped), and engine threads are
  joined on Stop. Heap, handles, GDI/USER objects and threads were flat in
  30-minute soaks and simulated long runs.

## Extension points (Phase 7, research only)

Packet-processing features, such as DPI compatibility work, would live in a **separate crate** behind a separate trait. They would be off by default, need an explicit user action in Settings, and need explicit administrator consent if a driver is involved. The GUI would see them only through new `Command`/`Event` variants. Nothing in Phases 1–6 installs drivers, changes system networking or touches security software.

## Error handling and shutdown

* Library crates return typed errors (`thiserror`). Only `kalkan-app` converts them into user-facing messages and exit codes.
* Release builds keep `panic = "unwind"`. A panic in the engine worker unwinds, its drop guard reports `Exited`, and the controller joins it, extracts the panic message and moves to `Failed`. The process does not abort. A panic hook also writes panics to the log.
* `NetworkMonitor::stop` must be idempotent and infallible, so cleanup is always safe to call.
