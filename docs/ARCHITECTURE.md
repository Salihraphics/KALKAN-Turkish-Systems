# KALKAN Architecture

## Goals that shape the design

1. **Lightweight.** Few dependencies, no async runtime unless it is justified, and no polling while idle.
2. **Responsive UI.** The UI thread never does network or disk work. It only sends commands and draws events.
3. **Read-only by default.** Monitoring observes OS counters. It never modifies traffic.
4. **Contained unsafe code.** Every Win32 `unsafe` call lives in `kalkan-win`, or in `kalkan-gui` for window code from Phase 2 on. All other crates use `#![forbid(unsafe_code)]`.
5. **Replaceable parts.** A new network module can be added behind the engine without changing the GUI.

## Workspace layout

```
kalkan/
├── Cargo.toml                 workspace, shared deps, lints, release profile
├── .cargo/config.toml         mingw linker for Linux→Windows cross builds
├── .github/workflows/ci.yml   Windows MSVC + Linux cross-compile CI
├── docs/ARCHITECTURE.md
└── crates/
    ├── kalkan-app/      binary `kalkan`: argument parsing, wiring, startup/shutdown
    ├── kalkan-gui/      native Win32 UI (Phase 2)
    ├── kalkan-core/     lifecycle state machine + Command/Event protocol
    ├── kalkan-net/      read-only NetworkMonitor trait, counters, rate math
    ├── kalkan-config/   TOML schema, defaults, validation, atomic save, app dirs
    ├── kalkan-logging/  tracing subscriber setup
    └── kalkan-win/      Windows integration (privileges, OS queries); all FFI
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

## GUI ↔ engine interface

```
 UI thread                         Engine thread (Phase 3)
 ─────────                         ───────────────────────
 button click ──Command::Start──►  Lifecycle: Stopped→Starting
                                   monitor.start()
              ◄─Event::StateChanged Starting→Running
 timer/paint  ◄─Event::Status───── periodic, at sample_interval_ms
 Stop click   ──Command::Stop────► Running→Stopping, monitor.stop()
              ◄─Event::StateChanged Stopping→Stopped
 window close ──Command::Shutdown► cleanup ─► Event::ShutdownComplete
```

Planned transport (Phase 3): `std::sync::mpsc` for commands. Events go into a queue, and the engine wakes the UI with `PostMessageW(hwnd, WM_APP_ENGINE_EVENT, …)`. The UI thread then drains the queue in its message loop. Neither side polls.

### Lifecycle

`kalkan_core::lifecycle` is a pure function `next_state(state, transition)` with 9 legal transitions. Tests check every pair of state and transition. The UI's Start and Stop buttons get their enabled state from the same table through `can_start()` and `can_stop()`, so the UI can never offer an action the engine would reject.

## Network monitoring (Phase 4 plan)

* Trait: `kalkan_net::NetworkMonitor` with `start`, `sample`, `stop`, and nothing that writes.
* Windows implementation: IP Helper `GetIfTable2` (64-bit interface counters) runs on the engine thread at the configured interval, which defaults to 1 s with a minimum of 250 ms.
* Rates: `Rates::between` handles counter resets (it reports 0, not garbage), saturation and out-of-order samples.
* No admin rights are needed for this. Elevation is detected and shown, but it is not required.

## Configuration

* File: `%APPDATA%\Kalkan\kalkan.toml`. Logs go to `%LOCALAPPDATA%\Kalkan\logs`.
* Unknown keys are rejected so typos surface. All range errors are reported together.
* A missing file means defaults are used. A malformed file is an error and is **never** overwritten.
* Saving is atomic (write a temp file, then rename).
* `start_engine_on_launch` defaults to `false`, so nothing network related runs until the user asks.

## Extension points (Phase 7, research only)

Packet-processing features, such as DPI compatibility work, would live in a **separate crate** behind a separate trait. They would be off by default, need an explicit user action in Settings, and need explicit administrator consent if a driver is involved. The GUI would see them only through new `Command`/`Event` variants. Nothing in Phases 1–6 installs drivers, changes system networking or touches security software.

## Error handling and shutdown

* Library crates return typed errors (`thiserror`). Only `kalkan-app` converts them into user-facing messages and exit codes.
* Release builds keep `panic = "unwind"` so the engine thread can catch a module panic, move to `Faulted` and release resources. The process does not abort.
* `NetworkMonitor::stop` must be idempotent and infallible, so cleanup is always safe to call.
