//! KALKAN logging.
//!
//! Phase 1: a global `tracing` subscriber writing to stderr at the configured
//! level. File output with rotation/retention and the in-app Logs page sink
//! are Phase 5.

#![forbid(unsafe_code)]

use kalkan_config::LogLevel;
use tracing_subscriber::filter::LevelFilter;

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("a global logger is already installed")]
    AlreadyInitialized,
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

/// Install the global subscriber. Call once, early in `main`.
pub fn init(level: LogLevel) -> Result<(), LoggingError> {
    tracing_subscriber::fmt()
        .with_max_level(level_filter(level))
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInitialized)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = init(LogLevel::Info);
        assert!(matches!(
            init(LogLevel::Info),
            Err(LoggingError::AlreadyInitialized)
        ));
    }
}
