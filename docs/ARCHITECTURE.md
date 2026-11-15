# KALKAN Architecture

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
└── crates/
    ├── kalkan-app/      binary `kalkan`: argument parsing, wiring, startup/shutdown
    ├── kalkan-gui/      native Win32 UI: model/layout/theme (portable) + win32 (Windows)
    ├── kalkan-core/     GoodBoyDPI engine (threads), lifecycle, Command/Event, EngineLink
    ├── kalkan-net/      read-only NetworkMonitor trait, counters, rate math
    ├── kalkan-config/   TOML schema, defaults, validation, atomic save, app dirs
    ├── kalkan-logging/  tracing subscriber (stderr or log file), panic hook
    └── kalkan-win/      Windows integration (privileges, process CPU/memory, console)
```

### Dependency graph (enforced by Cargo.toml)

```
                 kalkan-app
     ┌──────┬──────┼───────┬────────┬─────────┐
     ▼      ▼      ▼       ▼        ▼         ▼
  gui ──► core    net    config ◄── logging   win
```

* `kalkan-gui` depends **only** on `kalkan-core`. It cannot reach the network layer directly.
* `kalkan-core`, `kalkan-net` and `kalkan-config` are platform independent and are unit tested on any OS.
* `kalkan-win` pulls in the `windows` crate only under `cfg(windows)`.

## The engine: GoodBoyDPI (`kalkan-core::engine`)

"GoodBoyDPI — Rust Core Engine" is the internal name of KALKAN's background engine. **Today it manages its own lifecycle and measures KALKAN's own CPU and memory use. It contains no DPI circumvention, packet capture or packet modification.**

### Threads

| Thread | Lifetime | Job | How it waits |
|---|---|---|---|
| UI (main) | app | Win32 message loop | `GetMessageW` |
| `kalkan-engine` (controller) | app (spawned at start-up, joined at exit) | Sole owner of the `Lifecycle`. Turns commands and worker reports into events | `mpsc::Receiver::recv()` |
| `goodboydpi-core` (worker) | from Start to Stop | Initialisation, then periodic process metrics | `recv_timeout(sample_interval)` on its control channel |

While the engine is stopped, two threads exist: the UI and the controller. The controller sleeps in `recv()`. While the engine is running, the worker wakes once per `monitor.sample_interval_ms` (default 1 s), reads `GetProcessTimes` and `GetProcessMemoryInfo`, and sleeps again. Nothing in the engine busy-waits or polls.

### Message flow

```
 UI thread                 controller "kalkan-engine"            worker "goodboydpi-core"
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
| `Start`, `Stop`, `QueryStatus`, `Shutdown` | `StateChanged{from,to}`, `EngineStarted`, `EngineStopped`, `EngineError{message}`, `DiagnosticMessage{level,message}`, `Status(EngineStatus)`, `ShutdownComplete` |

`EngineStatus` metrics are `Option`s. They are `Some` only when measured, and they are cleared whenever the engine leaves `Running`.

## Network monitoring (Phase 4 plan)

* Trait: `kalkan_net::NetworkMonitor` with `start`, `sample`, `stop`, and nothing that writes.
* Windows implementation: IP Helper `GetIfTable2` (64-bit interface counters), sampled on the existing `goodboydpi-core` worker at the configured interval (default 1 s, minimum 250 ms).
* Rates: `Rates::between` handles counter resets (it reports 0, not garbage), saturation and out-of-order samples.
* No admin rights are needed for this. Elevation is detected and shown, but it is not required.

## Configuration

* File: `%APPDATA%\Kalkan\kalkan.toml`. GUI runs log to `%LOCALAPPDATA%\Kalkan\logs\kalkan.log`, which is renamed to `.old` above 5 MB. Console runs log to stderr.
* Unknown keys are rejected so typos surface. All range errors are reported together.
* A missing file means defaults are used. A malformed file is an error and is **never** overwritten.
* Saving is atomic (write a temp file, then rename).
* `start_engine_on_launch` defaults to `false`, so the engine does nothing until the user presses Start. If it is set to `true`, the GUI sends Start right after its window is created.

## Extension points (Phase 7, research only)

Packet-processing features, such as DPI compatibility work, would live in a **separate crate** behind a separate trait. They would be off by default, need an explicit user action in Settings, and need explicit administrator consent if a driver is involved. The GUI would see them only through new `Command`/`Event` variants. Nothing in Phases 1–6 installs drivers, changes system networking or touches security software.

## Error handling and shutdown

* Library crates return typed errors (`thiserror`). Only `kalkan-app` converts them into user-facing messages and exit codes.
* Release builds keep `panic = "unwind"`. A panic in the engine worker unwinds, its drop guard reports `Exited`, and the controller joins it, extracts the panic message and moves to `Failed`. The process does not abort. A panic hook also writes panics to the log.
* `NetworkMonitor::stop` must be idempotent and infallible, so cleanup is always safe to call.
