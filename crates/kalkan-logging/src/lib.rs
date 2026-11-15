//! KALKAN logging.
//!
//! A global `tracing` subscriber that writes to one of two places:
//!
//! * [`Output::Stderr`] for CLI runs, `--self-test` and development.
//! * [`Output::File`] for normal GUI runs. A GUI-subsystem process has no
//!   console, so stderr would silently discard every message.
//!
//! Log files are bounded: at start-up a file larger than [`MAX_LOG_BYTES`] is
//! renamed to `<name>.old` (replacing an older one), so at most two files
//! exist. Daily rotation with retention (`logging.max_log_files`) and the
//! in-app log view are Phase 5.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kalkan_config::LogLevel;
use tracing_subscriber::filter::LevelFilter;

/// File name used inside the log directory.
pub const LOG_FILE_NAME: &str = "kalkan.log";
/// A log file larger than this is rolled over at start-up.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

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

/// Create the directory, roll an oversized file over to `.old`, and open the
/// log file for appending.
pub fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        // `rename` replaces an existing target on Windows and Unix.
        fs::rename(path, PathBuf::from(old))?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Install the global subscriber. Call once, early in `main`.
pub fn init(level: LogLevel, output: &Output) -> Result<(), LoggingError> {
    let builder = tracing_subscriber::fmt()
        .with_max_level(level_filter(level))
        .with_target(true)
        .with_thread_names(true);
    let result = match output {
        Output::Stderr => builder.with_writer(io::stderr).try_init(),
        Output::File(path) => {
            let file = open_log_file(path).map_err(|source| LoggingError::Io {
                path: path.clone(),
                source,
            })?;
            builder
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init()
        }
    };
    result.map_err(|_| LoggingError::AlreadyInitialized)
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
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kalkan-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn level_mapping_is_monotonic() {
        let levels = [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ];
        let filters: Vec<_> = levels.iter().map(|&l| level_filter(l)).collect();
        // LevelFilter orders OFF < ERROR < ... < TRACE in verbosity.
        assert!(filters.windows(2).all(|w| w[0] < w[1]), "{filters:?}");
    }

    #[test]
    fn second_init_fails_cleanly() {
        // The first call may race with other tests in this binary; only the
        // second call's outcome is asserted.
        let _ = init(LogLevel::Info, &Output::Stderr);
        assert!(matches!(
            init(LogLevel::Info, &Output::Stderr),
            Err(LoggingError::AlreadyInitialized)
        ));
    }

    #[test]
    fn log_file_is_created_with_its_directory_and_appended() {
        let dir = temp_dir("append");
        let path = dir.join("nested").join(LOG_FILE_NAME);
        open_log_file(&path).unwrap().write_all(b"one\n").unwrap();
        open_log_file(&path).unwrap().write_all(b"two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_log_is_rolled_over_once() {
        let dir = temp_dir("roll");
        let path = dir.join(LOG_FILE_NAME);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();
        open_log_file(&path).unwrap().write_all(b"fresh\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh\n");
        let old = dir.join(format!("{LOG_FILE_NAME}.old"));
        assert_eq!(fs::metadata(&old).unwrap().len(), MAX_LOG_BYTES + 1);
        // A second oversized file replaces the previous `.old`.
        fs::write(&path, vec![b'y'; MAX_LOG_BYTES as usize + 2]).unwrap();
        open_log_file(&path).unwrap();
        assert_eq!(fs::metadata(&old).unwrap().len(), MAX_LOG_BYTES + 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_writer_receives_formatted_events() {
        let dir = temp_dir("events");
        let path = dir.join(LOG_FILE_NAME);
        let file = open_log_file(&path).unwrap();
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .finish();
        tracing::subscriber::with_default(sub, || {
            tracing::info!(answer = 42, "engine started");
        });
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("engine started") && text.contains("answer=42"),
            "{text}"
        );
        assert!(!text.contains('\u{1b}'), "no ANSI escapes in files");
        fs::remove_dir_all(dir).unwrap();
    }
}
