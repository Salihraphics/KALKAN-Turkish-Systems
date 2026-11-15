//! KALKAN native Win32 GUI.
//!
//! **Phase 1: not implemented.** This crate exists so the dependency graph is
//! fixed from the start: the GUI depends on `kalkan-core` (for [`Command`] /
//! [`Event`]) and nothing else from the engine side. It must never depend on
//! `kalkan-net` directly.
//!
//! Phase 2 will add the main window, the five pages (Dashboard, Network
//! Monitoring, Statistics, Logs, Settings) and the message pump.

#![forbid(unsafe_code)]

pub use kalkan_core::{Command, Event};

/// Names of the pages the GUI will provide, in navigation order.
pub const PAGES: [&str; 5] = [
    "Dashboard",
    "Network Monitoring",
    "Statistics",
    "Logs",
    "Settings",
];

/// Whether this build contains a working GUI.
pub const IMPLEMENTED: bool = false;
