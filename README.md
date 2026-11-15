# 🛡️ KALKAN

**Complex network technology. Simple user experience.**

<p align="center">
  <img src="https://img.shields.io/badge/Language-Rust-orange?style=for-the-badge&logo=rust" />
  <img src="https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-blue?style=for-the-badge&logo=windows" />
  <img src="https://img.shields.io/badge/Architecture-Native-success?style=for-the-badge" />
  <img src="https://img.shields.io/badge/Status-Phase%201%20of%207-yellow?style=for-the-badge" />
</p>

<p align="center"><b>Lightweight. Native. Reliable.</b></p>

---

## ⚡ What is KALKAN?

KALKAN is an open-source Windows network utility written in Rust. It aims to be lightweight, reliable and easy to use.

Existing network tools are often hard to use. KALKAN puts complex networking operations behind a clean native interface and an efficient background engine.

> Make advanced network technology accessible without making the user deal with unnecessary complexity.

KALKAN is **not** a GUI wrapper around GoodbyeDPI or any other tool. It is an independent application.

## 🚧 Development status

KALKAN is built in phases. A feature is marked ✅ only after it has been compiled and tested.

| Phase | Scope | Status |
|---|---|---|
| 1 | Project foundation and compilation | ✅ Done |
| 2 | Native Win32 GUI | ⏳ Next |
| 3 | Core engine lifecycle (threaded runner) | 🟡 Planned |
| 4 | Read-only network monitoring | 🟡 Planned |
| 5 | Logging (files, rotation, in-app view) and configuration UI | 🟡 Planned |
| 6 | Performance benchmarking | 🟡 Planned |
| 7 | DPI research and controlled integration planning | 🔬 Research |

### What works today (Phase 1)

| Component | Status | Verified by |
|---|---|---|
| Cargo workspace, 7 crates, shared lints and release profile | ✅ | Builds on Linux and cross-compiles to `x86_64-pc-windows-gnu` |
| Engine lifecycle state machine (`kalkan-core`) | ✅ | 6 unit tests, including an exhaustive transition table |
| GUI ↔ engine `Command`/`Event` protocol (types) | ✅ | Compiles. It gets a runtime consumer in Phase 3 |
| TOML config: defaults, unknown-key rejection, range validation, atomic save | ✅ | 12 unit tests |
| Logging initialisation (`tracing`, stderr) | ✅ | 2 unit tests plus a manual run |
| Read-only `NetworkMonitor` trait, counter and rate math | ✅ | 7 unit tests (resets, saturation, ordering) |
| Administrator-privilege detection (`kalkan-win`) | ⚠️ | Cross-compiles for Windows. The runtime test runs only on Windows CI |
| `kalkan` CLI: `--version`, `--config`, `--check-config`, `--print-default-config` | ✅ | 3 unit tests plus a manual smoke test |
| Windows CI (MSVC build, clippy, tests, smoke test) | ⚠️ | Workflow written but not yet run on GitHub |

### Not implemented yet

- GUI window and pages (Dashboard, Network Monitoring, Statistics, Logs, Settings). Phase 1 runs as a console program.
- Engine thread, Start/Stop, CPU and memory metrics.
- Real network counters on Windows (`GetIfTable2`).
- Log files and rotation, and the in-app log view.
- System tray, gaming profile, benchmarks.

## 🧠 Architecture

```
kalkan-app ──► kalkan-gui ──► kalkan-core      (GUI only speaks Command/Event)
     │
     ├──► kalkan-net      read-only monitor trait (no packet modification API)
     ├──► kalkan-config   TOML + validation
     ├──► kalkan-logging  tracing
     └──► kalkan-win      all Win32 FFI lives here
```

See **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** for the full design, the lifecycle diagram, the threading model and the extension points.

## 🔧 Building

### On Windows (recommended)

Requirements: [Rust](https://rustup.rs) stable (1.80+) with the MSVC toolchain and the Visual Studio C++ Build Tools.

```powershell
cargo build --release          # -> target\release\kalkan.exe
cargo test --workspace
```

### Cross-compiling from Linux

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/kalkan.exe
```

Tests for the platform-independent crates run on any OS with `cargo test --workspace`.

### Usage (Phase 1)

```text
kalkan --print-default-config > kalkan.toml
kalkan --config kalkan.toml --check-config
kalkan --version
```

By default the configuration lives at `%APPDATA%\Kalkan\kalkan.toml`. If the file is missing, defaults are used. If the file is invalid, KALKAN lists every problem and exits. It never overwrites your file.

## 🎮 Gaming first

Performance is a requirement, not an afterthought:

- No background polling while idle, and no async runtime unless it is justified.
- Monitoring is read-only. It reads OS counters and does no packet processing.
- Phase 6 will measure CPU, memory, latency and jitter. **We do not claim performance numbers until they are measured.**

## 🔐 Security and transparency

- No hidden background processes. Nothing starts without a user action (`start_engine_on_launch = false` by default).
- Antivirus and security software are never disabled.
- No undocumented system modifications. No drivers are installed in Phases 1–6.
- Administrator privileges are not required for monitoring, and the current privilege level is shown to the user.
- `unsafe` code is confined to `kalkan-win`. Every other crate uses `#![forbid(unsafe_code)]`.
- Dependencies are small and well known: `windows`, `serde`, `toml`, `thiserror`, `tracing`, `tracing-subscriber`.

## 🤝 Contributing

Contributions, suggestions, bug reports and technical discussions are welcome. Please check the project issues before submitting major changes.

Before you open a PR, run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 📜 License

License: to be determined.

<p align="center">Built with 🦀 Rust</p>
