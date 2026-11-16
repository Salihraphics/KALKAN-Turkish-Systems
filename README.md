🛡️ KALKAN — Turkish Systems

<p align="center">
  <strong>A lightweight Windows network utility, built with Rust.</strong>
  <br />
  Developed in Türkiye 🇹🇷
</p><p align="center">
  <img src="assets/kalkan-logo.png" alt="KALKAN Logo" width="160" />
</p><p align="center">
  <img src="https://img.shields.io/badge/Language-Rust-orange?logo=rust" />
  <img src="https://img.shields.io/badge/Platform-Windows-0078D4?logo=windows" />
  <img src="https://img.shields.io/badge/GUI-Native%20Win32-blue" />
  <img src="https://img.shields.io/badge/License-MIT-green" />
  <img src="https://img.shields.io/badge/Origin-Türkiye-red" />
</p>---

About KALKAN

KALKAN is a lightweight Windows network utility developed in Türkiye using Rust.

Designed with performance, reliability, and simplicity in mind, KALKAN combines a native Windows interface with a modular Rust architecture.

The project focuses on providing a clean and efficient experience for network monitoring, application configuration, diagnostics, and system resource management.

Our philosophy:

«Complex technology. Simple user experience.»

Features

Network Monitoring

- Real-time network traffic monitoring.
- Download and upload speed tracking.
- Network interface detection.
- Traffic statistics and historical graphs.
- Support for monitoring multiple network interfaces.
- Read-only interaction with Windows network information APIs.

Native Windows Interface

- Lightweight Win32 GUI.
- Dashboard and network monitoring views.
- Statistics and performance information.
- Integrated log viewer.
- Configuration management.
- Native Windows window and taskbar icon.

Logging & Diagnostics

- INFO, WARN, ERROR, DEBUG and TRACE logging levels.
- Searchable log viewer.
- Log filtering and live updates.
- Log export and clipboard support.
- Automatic log rotation.
- Configurable log retention.

Configuration

- TOML-based configuration.
- Configuration validation.
- Atomic configuration saving.
- Reset to default settings.
- Recovery from invalid configuration files.

Performance & Reliability

- Event-driven GUI architecture.
- Dedicated engine and worker threads.
- Controlled memory usage.
- Graceful engine shutdown.
- Performance measurement and optimization.

Technology Stack

Component| Technology
Core language| Rust
GUI| Native Win32 API
Windows integration| Windows API
Configuration| TOML
Logging| Custom logging system
Build system| Cargo
CI| GitHub Actions

Architecture

KALKAN uses a modular Rust workspace:

KALKAN/
├── kalkan-app/
├── kalkan-gui/
├── kalkan-core/
├── kalkan-net/
├── kalkan-config/
├── kalkan-logging/
├── kalkan-win/
├── assets/
├── docs/
└── Cargo.toml

Each crate has a dedicated responsibility, keeping the application modular and maintainable.

The GUI communicates with the engine through an event-driven architecture. Network monitoring and application logic remain separated from the presentation layer.

Development Status

KALKAN is actively under development.

Component| Status
Rust workspace architecture| Completed
Native Windows GUI| Completed
Engine lifecycle management| Completed
Read-only network monitoring| Completed
Logging system| Completed
Configuration management| Completed
Performance optimization| In progress
Security hardening| Planned
Release and installer| Planned

Features listed as planned or in progress should not be considered available in current releases.

Platform Support

Platform| Status
Windows 10/11| Primary development platform
Linux| Future consideration
Android| Future consideration
iOS| Future consideration

Building from Source

Requirements:

- Rust toolchain
- Cargo
- Windows development environment
- Windows target support

Clone the repository:

git clone https://github.com/YOUR_USERNAME/KALKAN-Turkish-Systems.git
cd KALKAN-Turkish-Systems

Build:

cargo build --release

Run tests:

cargo test --workspace

Run formatting checks:

cargo fmt --all -- --check

Run Clippy:

cargo clippy --workspace --all-targets -- -D warnings

Performance Philosophy

KALKAN is designed to remain lightweight while operating in the background.

We prioritize:

- Low CPU overhead.
- Stable memory usage.
- Efficient background processing.
- Minimal unnecessary GUI updates.
- Reliable long-running operation.

Performance claims are evaluated through repeatable benchmarks rather than assumptions.

Security & Privacy

KALKAN's current network monitoring implementation is read-only.

It does not currently capture, inject, modify, or block network packets.

The application aims to minimize unnecessary data collection and avoid storing sensitive information in diagnostic logs.

License

KALKAN is licensed under the MIT License.

Copyright (c) 2026 Salih Kıvanç TULUM.

See the "LICENSE" (LICENSE) file for details.

---

<p align="center">
  <strong>KALKAN</strong>
  <br />
  Built in Türkiye 🇹🇷
  <br />
  Powered by Rust 🦀
</p>
