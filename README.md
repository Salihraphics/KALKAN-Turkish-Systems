<p align="center">
  <img src="./kalkan-logo_20261116_111629_0000.png" alt="KALKAN logo" width="160" />
</p>

<h1 align="center">🛡️ KALKAN — Turkish Systems</h1>

<p align="center">
  <strong>A lightweight Windows network utility, built with Rust.</strong>
  <br />
  <b>A Novera Labs Product</b>
  <br />
  Developed in Türkiye 🇹🇷 · Built in Türkiye. Powered by Rust.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Language-Rust-orange?logo=rust" alt="Language: Rust" />
  <img src="https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D4?logo=windows" alt="Platform: Windows 10 and 11" />
  <img src="https://img.shields.io/badge/GUI-Native%20Win32-blue" alt="GUI: native Win32" />
  <img src="https://img.shields.io/badge/Status-Phase%206%20of%207%20(in%20progress)-yellow" alt="Status: Phase 6 of 7, in progress" />
  <img src="https://img.shields.io/badge/License-MIT-green" alt="License: MIT" />
  <img src="https://img.shields.io/badge/Origin-T%C3%BCrkiye-red" alt="Origin: Türkiye" />
</p>

---

## About KALKAN

KALKAN is a lightweight Windows network utility developed in Türkiye using Rust. It combines a native Windows interface with a modular Rust architecture, and focuses on performance, reliability and simplicity.

> «Complex network technology. Simple user experience.»

KALKAN is **not** a GUI wrapper around GoodbyeDPI or any other tool. It is an independent application.

| | |
|---|---|
| Product | **KALKAN** |
| Developer brand | **Novera Labs** — *A Novera Labs Product* |
| Engine | **KOR — Kernel Operations Runtime** |
| Core language | Rust |
| GUI | Native Win32 |
| Origin | Türkiye 🇹🇷 |

### The KOR engine

KOR is KALKAN's background engine: an ordinary **user-mode** part of the application (threads inside `kalkan.exe`). The word "Kernel" in *Kernel Operations Runtime* does **not** mean it runs in Windows kernel mode, uses or installs a kernel driver, or is a Windows kernel component. KALKAN installs no drivers.

KOR was previously named *GoodBoyDPI*. The source code published so far still shows that name in the engine panel and in internal identifiers; the rename in code is in progress.

## Where the code is

> **This branch (`main`) currently contains the README, the licence and the logo only.** The Rust source code, tests, CI and the technical documentation are on the development branch [`ccr-95fdd807-9vps1t`](https://github.com/Salihraphics/KALKAN-Turkish-Systems/tree/ccr-95fdd807-9vps1t) and are reviewed in [pull request #1](https://github.com/Salihraphics/KALKAN-Turkish-Systems/pull/1); they have not been merged into `main` yet.

Technical documentation (development branch):
[architecture](https://github.com/Salihraphics/KALKAN-Turkish-Systems/blob/ccr-95fdd807-9vps1t/docs/ARCHITECTURE.md) ·
[GUI](https://github.com/Salihraphics/KALKAN-Turkish-Systems/blob/ccr-95fdd807-9vps1t/docs/GUI.md)

## Current capabilities

Implemented and tested on the development branch:

**Network monitoring (read-only)**
- Reads the interface counters Windows already keeps, through the IP Helper API (`GetIfTable2`).
- Download and upload speed, 60-second graphs, session traffic totals, peaks and averages.
- Every relevant network interface with its type, link speed, connection status, live rates and cumulative bytes.
- No double counting: filter-driver duplicates are dropped, virtual/VPN adapters are listed but not counted, and adapters that share a MAC address are counted once.
- Adapter changes (added, removed, connected, disconnected) are reported; counter resets never show as spikes.

**Native Windows interface**
- Lightweight Win32 GUI: Dashboard, Network Monitoring, Statistics, Logs and Settings pages.
- KALKAN's own CPU and memory use.
- Native window and taskbar icon; per-monitor DPI awareness.

**Logging and diagnostics**
- ERROR, WARN, INFO, DEBUG and TRACE levels.
- In-app log viewer with level filters, search, pause/resume, copy and export.
- Size-based log rotation with configurable retention.

**Configuration**
- TOML configuration with validation and atomic saving.
- Settings page: edit, validate, save, revert, reset to defaults.
- An invalid configuration file does not stop the app: defaults are used, the problem is shown, and the old file is kept as a backup when settings are saved.

**Architecture**
- Event-driven communication between the GUI and the engine; no expensive work on the UI thread.
- A modular Rust workspace (`kalkan-app`, `kalkan-gui`, `kalkan-core`, `kalkan-net`, `kalkan-config`, `kalkan-logging`, `kalkan-win`).

**Not implemented:** KALKAN does **not** implement DPI circumvention, packet capture, packet manipulation or injection, or any restriction bypass, and it does not enable access to any blocked service. Its network monitoring is strictly read-only.

## Development status

KALKAN is built in phases. A feature is marked done only after it has been compiled and tested.

| Phase | Scope | Status |
|---|---|---|
| 1 | Project foundation and compilation | ✅ Done |
| 2 | Native Win32 GUI | ✅ Done |
| 3 | Core engine lifecycle (KOR) | ✅ Done |
| 4 | Read-only network monitoring and UI refinement | ✅ Done |
| 5 | Logging and configuration | ✅ Done |
| 6 | Performance benchmarking and optimization | 🔧 In progress |
| 7 | DPI research and controlled integration planning | 🔬 Research and planning only |

Also planned: security hardening, and a release with an installer. Features listed as planned or in progress should not be considered available.

### Performance (Phase 6, in progress)

Measured on GitHub Actions Windows virtual machines, **not on desktop hardware**:

- Network monitoring costs about 0.34 ms of CPU per 1-second sample.
- With the live Network page open, the whole application stayed below 1 % of one CPU core in every steady minute of three 30-minute soak tests.
- Memory, handles and threads stayed flat over 30 minutes; input latency (p99) was about 0.2 ms.

Further optimizations are in progress and not yet validated. Performance on real desktop hardware has not been measured yet, and no numbers are claimed for it.

## Platform support

| Platform | Status |
|---|---|
| Windows 10 / 11 (64-bit) | Primary development platform |
| Linux | Future consideration |
| Android | Future consideration |
| iOS | Future consideration |

## Building from source

The source code is on the development branch (see [Where the code is](#where-the-code-is)).

Requirements: [Rust](https://rustup.rs) stable 1.85 or newer, with the MSVC toolchain and the Visual Studio C++ Build Tools on Windows.

```powershell
git clone https://github.com/Salihraphics/KALKAN-Turkish-Systems.git
cd KALKAN-Turkish-Systems
git checkout ccr-95fdd807-9vps1t
cargo build --release          # -> target\release\kalkan.exe
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Security and privacy

- Network monitoring is read-only: KALKAN does not capture, inject, modify or block network packets.
- No hidden background processes; nothing starts without a user action.
- No drivers are installed, and antivirus or security software is never disabled.
- Monitoring is designed not to need administrator rights (not yet tested under a standard user account); the current privilege level is shown in the app.

## License

KALKAN is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

Copyright (c) 2026 Salih Kıvanç TULUM.

---

<p align="center">
  <strong>KALKAN — A Novera Labs Product</strong>
  <br />
  Built in Türkiye 🇹🇷 · Powered by Rust 🦀
</p>
