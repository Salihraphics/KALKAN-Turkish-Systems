//! KALKAN logging.
//!
//! One global `tracing` subscriber with three parts:
//!
//! * a **level filter** that can be changed at runtime ([`LoggingHandle::set_level`]),
//! * an optional **text output**: stderr (CLI runs) or a size-rotated log
//!   file (GUI runs; see [`rotate`]),
//! * a **buffer layer** feeding the in-app log viewer ([`buffer::LogBuffer`]).
//!
//! Errors are never dropped silently: file problems are recorded in
//! [`rotate::FileStatus::last_error`] and shown by the GUI.

#![forbid(unsafe_code)]

pub mod buffer;
pub mod rotate;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kalkan_config::LogLevel;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{reload, Registry};

pub use buffer::{format_utc, EntrySource, HistoryInfo, LogBuffer, LogEntry};
pub use rotate::{FileStatus, RotatingFile, RotationPolicy, SharedFile};

/// File name used inside the log directory.
pub const LOG_FILE_NAME: &str = "kalkan.log";
/// Entries kept in memory for the viewer.
pub const BUFFER_CAPACITY: usize = 5_000;
/// How much of the previous log file the viewer loads as history.
pub const HISTORY_TAIL_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Stderr,
    File(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("a global logger is already installed")]
    AlreadyInitialized,
    #[error("could not open log file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Map the configuration level to a `tracing` filter.
pub fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

/// Everything needed to start logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSettings {
    pub level: LogLevel,
    pub output: Output,
    pub rotation: RotationPolicy,
}

/// Runtime control over the installed logger.
pub struct LoggingHandle {
    level: reload::Handle<LevelFilter, Registry>,
    current_level: std::sync::Mutex<LogLevel>,
    buffer: Arc<LogBuffer>,
    file: Option<SharedFile>,
    /// Size of the log file before this run wrote to it: bytes before this
    /// offset are history.
    history_end: u64,
}

impl std::fmt::Debug for LoggingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingHandle")
            .field("file", &self.file_path())
            .finish_non_exhaustive()
    }
}

impl LoggingHandle {
    pub fn buffer(&self) -> Arc<LogBuffer> {
        self.buffer.clone()
    }

    pub fn level(&self) -> LogLevel {
        *self.current_level.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Change the level immediately for all threads.
    pub fn set_level(&self, level: LogLevel) -> Result<(), String> {
        self.level
            .modify(|f| *f = level_filter(level))
            .map_err(|e| format!("could not change the log level: {e}"))?;
        *self.current_level.lock().unwrap_or_else(|p| p.into_inner()) = level;
        Ok(())
    }

    /// Apply new rotation limits (prunes archives beyond `keep` now).
    pub fn set_rotation(&self, policy: RotationPolicy) {
        if let Some(f) = &self.file {
            f.lock().set_policy(policy);
        }
    }

    pub fn file_path(&self) -> Option<PathBuf> {
        self.file.as_ref().map(|f| f.lock().path().to_path_buf())
    }

    /// Current file state (no disk I/O).
    pub fn file_status(&self) -> Option<FileStatus> {
        self.file.as_ref().map(|f| f.lock().status())
    }

    /// Load the previous run's tail of the log file into the buffer on a
    /// short-lived background thread (never on the UI thread). Returns the
    /// thread so the caller may join it at shutdown.
    pub fn spawn_history_load(&self) -> Option<std::thread::JoinHandle<()>> {
        let path = self.file_path()?;
        let (buffer, end) = (self.buffer.clone(), self.history_end);
        std::thread::Builder::new()
            .name("kalkan-log-history".into())
            .spawn(move || {
                let (entries, info) =
                    buffer::load_history(&path, end, HISTORY_TAIL_BYTES, BUFFER_CAPACITY);
                if let Some(e) = &info.error {
                    tracing::warn!(error = %e, "log history unavailable");
                }
                buffer.prepend_history(entries, info);
            })
            .ok()
    }

    /// Flush and close the log file (application shutdown). Later events are
    /// counted as dropped instead of being written.
    pub fn close(&self) {
        if let Some(f) = &self.file {
            f.lock().close();
        }
    }
}

/// Install the global subscriber. Call once, early in `main`.
pub fn init(settings: &LogSettings) -> Result<LoggingHandle, LoggingError> {
    let (dispatch, handle) = build(settings)?;
    tracing::dispatcher::set_global_default(dispatch)
        .map_err(|_| LoggingError::AlreadyInitialized)?;
    Ok(handle)
}

/// Build the same subscriber [`init`] installs, without installing it.
/// Used by the benchmark to measure logging cost with the real layer stack
/// (`tracing::dispatcher::with_default`).
pub fn build(settings: &LogSettings) -> Result<(tracing::Dispatch, LoggingHandle), LoggingError> {
    let buffer = LogBuffer::new(BUFFER_CAPACITY);
    let (file, history_end, writer) = match &settings.output {
        Output::Stderr => (None, 0, BoxMakeWriter::new(io::stderr)),
        Output::File(path) => {
            let history_end = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let rf =
                RotatingFile::open(path, settings.rotation).map_err(|source| LoggingError::Io {
                    path: path.clone(),
                    source,
                })?;
            // A rotation at open moved the history into archive 1.
            let history_end = if rf.size() < history_end {
                0
            } else {
                history_end
            };
            let shared = SharedFile::new(rf);
            (
                Some(shared.clone()),
                history_end,
                BoxMakeWriter::new(shared),
            )
        }
    };
    let (filter, handle) = reload::Layer::new(level_filter(settings.level));
    let text = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(matches!(settings.output, Output::Stderr))
        .with_target(true)
        .with_thread_names(true);
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(text)
        .with(buffer::BufferLayer(buffer.clone()));
    Ok((
        tracing::Dispatch::new(subscriber),
        LoggingHandle {
            level: handle,
            current_level: std::sync::Mutex::new(settings.level),
            buffer,
            file,
            history_end,
        },
    ))
}

/// The log file path inside a log directory.
pub fn log_file_in(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE_NAME)
}

/// Route panics through `tracing` (in addition to the default hook), so a
/// panic in a GUI run ends up in the log file instead of an absent console.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("<unnamed>"),
            "panic: {info}"
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_mapping_is_monotonic() {
        let filters: Vec<_> = LogLevel::ALL.iter().map(|&l| level_filter(l)).collect();
        // LevelFilter orders OFF < ERROR < ... < TRACE in verbosity.
        assert!(filters.windows(2).all(|w| w[0] < w[1]), "{filters:?}");
    }

    /// The only test in this binary that installs the global subscriber.
    #[test]
    fn init_writes_file_feeds_buffer_reloads_level_and_rotates() {
        let dir = std::env::temp_dir().join(format!("kalkan-log-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = log_file_in(&dir);
        std::fs::write(
            &path,
            "2026-11-15T20:00:00.000Z  INFO main kalkan: previous run\n",
        )
        .unwrap();
        let h = init(&LogSettings {
            level: LogLevel::Info,
            output: Output::File(path.clone()),
            rotation: RotationPolicy {
                max_bytes: 4096,
                keep: 2,
            },
        })
        .expect("first init");
        assert!(matches!(
            init(&LogSettings {
                level: LogLevel::Info,
                output: Output::Stderr,
                rotation: h.file_status().unwrap().policy
            }),
            Err(LoggingError::AlreadyInitialized)
        ));

        tracing::info!(answer = 42, "engine started");
        tracing::debug!("hidden at info");
        let snap = h.buffer().snapshot();
        assert!(snap.iter().any(|e| e.message == "engine started answer=42"));
        assert!(!snap.iter().any(|e| e.message == "hidden at info"));

        h.set_level(LogLevel::Debug).unwrap();
        assert_eq!(h.level(), LogLevel::Debug);
        tracing::debug!("visible at debug");
        assert!(h
            .buffer()
            .snapshot()
            .iter()
            .any(|e| e.message == "visible at debug"));

        // History: only the line that existed before init.
        h.spawn_history_load().unwrap().join().unwrap();
        let snap = h.buffer().snapshot();
        assert_eq!(snap[0].message, "previous run");
        assert_eq!(snap[0].source, EntrySource::History);
        assert_eq!(
            snap.iter()
                .filter(|e| e.source == EntrySource::History)
                .count(),
            1
        );

        // File output, without ANSI escapes, rotating at the configured size.
        for i in 0..200 {
            tracing::info!(i, "filler line to force rotation");
        }
        let st = h.file_status().unwrap();
        assert!(st.rotations > 0 && st.archives == 2, "{st:?}");
        assert!(st.size <= 4096);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(
            text.lines().all(|l| buffer::parse_line(l).is_some()),
            "file lines are parseable"
        );

        h.set_rotation(RotationPolicy {
            max_bytes: 4096,
            keep: 1,
        });
        assert_eq!(h.file_status().unwrap().archives, 1);
        h.close();
        tracing::info!("after close");
        assert!(h.file_status().unwrap().dropped_writes >= 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
