//! KALKAN configuration.
//!
//! * Format: TOML.
//! * Unknown keys are rejected (typos are reported, not silently ignored).
//! * Every value is range-checked by [`Config::validate`] after parsing.
//! * A missing file means "use defaults"; a malformed file is an error that the
//!   caller must surface to the user — we never overwrite a user's broken file.

#![forbid(unsafe_code)]

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the configuration directory.
pub const CONFIG_FILE_NAME: &str = "kalkan.toml";

/// Current schema version written to new files.
pub const SCHEMA_VERSION: u32 = 1;

/// Inclusive bounds for the monitor sampling interval (milliseconds).
pub const SAMPLE_INTERVAL_MS_RANGE: (u32, u32) = (250, 60_000);

/// Inclusive bounds for the number of archived (rotated) log files kept.
pub const MAX_LOG_FILES_RANGE: (u16, u16) = (1, 90);

/// Inclusive bounds for the size at which the active log file is rotated (MB).
pub const MAX_FILE_SIZE_MB_RANGE: (u32, u32) = (1, 100);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub schema_version: u32,
    pub general: General,
    pub monitor: Monitor,
    pub logging: Logging,
    pub ui: Ui,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct General {
    /// Start the engine automatically when the application opens.
    /// Defaults to `false`: no network work happens until the user asks.
    pub start_engine_on_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Monitor {
    /// How often network counters are sampled while the engine runs.
    pub sample_interval_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// From least to most verbose.
    pub const ALL: [LogLevel; 5] = [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    /// Case-insensitive parse of `error`/`warn`/`warning`/`info`/`debug`/`trace`.
    pub fn parse(s: &str) -> Option<LogLevel> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" | "warning" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            "trace" => Some(LogLevel::Trace),
            _ => None,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Logging {
    pub level: LogLevel,
    /// Write logs to files in the log directory (in addition to the in-app view).
    pub file_logging: bool,
    /// How many archived log files to keep (`kalkan.1.log` … `kalkan.N.log`).
    pub max_log_files: u16,
    /// Rotate the active `kalkan.log` before it would exceed this size (MB).
    /// Added in Phase 5; files without it get the default.
    pub max_file_size_mb: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ui {
    pub start_minimized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            general: General::default(),
            monitor: Monitor::default(),
            logging: Logging::default(),
            ui: Ui::default(),
        }
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1_000,
        }
    }
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            file_logging: true,
            max_log_files: 7,
            max_file_size_mb: 5,
        }
    }
}

/// A single validation failure, with the offending key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub key: &'static str,
    pub message: String,
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid KALKAN configuration: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid configuration:\n{}", format_issues(.0))]
    Invalid(Vec<ValidationIssue>),
    #[error("could not serialize configuration: {0}")]
    Serialize(String),
}

fn format_issues(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|i| format!("  - {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Config {
    /// Parse and validate a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Self::parse(s, Path::new("<string>"))
    }

    fn parse(s: &str, path: &Path) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(s).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.message().to_owned(),
        })?;
        cfg.validate().map_err(ConfigError::Invalid)?;
        Ok(cfg)
    }

    /// Load from `path`. A missing file yields [`Config::default`].
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(s) => Self::parse(&s, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Validate and write atomically (temp file + rename) to `path`.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate().map_err(ConfigError::Invalid)?;
        let text = self.to_toml_string()?;
        let io_err = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(io_err)?;
        fs::rename(&tmp, path).map_err(io_err)?;
        Ok(())
    }

    /// Copy an existing (e.g. invalid) configuration file to
    /// `kalkan.toml.bak`, or `kalkan.toml.bak.2`, `.3`, … if that exists, so
    /// overwriting it never loses what the user wrote. Returns the backup path.
    pub fn backup_file(path: &Path) -> Result<PathBuf, ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut n = 1u32;
        let backup = loop {
            let mut name = path.as_os_str().to_owned();
            name.push(".bak");
            if n > 1 {
                name.push(format!(".{n}"));
            }
            let candidate = PathBuf::from(name);
            if !candidate.exists() {
                break candidate;
            }
            n += 1;
            if n > 1000 {
                return Err(io_err(io::Error::other("too many backup files")));
            }
        };
        fs::copy(path, &backup).map_err(io_err)?;
        Ok(backup)
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))
    }

    /// Check every value. Returns *all* problems, not just the first.
    pub fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
        let mut issues = Vec::new();

        if self.schema_version == 0 || self.schema_version > SCHEMA_VERSION {
            issues.push(ValidationIssue {
                key: "schema_version",
                message: format!(
                    "unsupported version {} (this build supports 1..={SCHEMA_VERSION})",
                    self.schema_version
                ),
            });
        }

        let (lo, hi) = SAMPLE_INTERVAL_MS_RANGE;
        if !(lo..=hi).contains(&self.monitor.sample_interval_ms) {
            issues.push(ValidationIssue {
                key: "monitor.sample_interval_ms",
                message: format!(
                    "{} is out of range (allowed {lo}..={hi})",
                    self.monitor.sample_interval_ms
                ),
            });
        }

        let (lo, hi) = MAX_LOG_FILES_RANGE;
        if !(lo..=hi).contains(&self.logging.max_log_files) {
            issues.push(ValidationIssue {
                key: "logging.max_log_files",
                message: format!(
                    "{} is out of range (allowed {lo}..={hi})",
                    self.logging.max_log_files
                ),
            });
        }

        let (lo, hi) = MAX_FILE_SIZE_MB_RANGE;
        if !(lo..=hi).contains(&self.logging.max_file_size_mb) {
            issues.push(ValidationIssue {
                key: "logging.max_file_size_mb",
                message: format!(
                    "{} is out of range (allowed {lo}..={hi})",
                    self.logging.max_file_size_mb
                ),
            });
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

/// Per-user application directories. Nothing is written outside these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppDirs {
    /// Resolve directories from the environment.
    ///
    /// * Windows: `%APPDATA%\Kalkan` (config) and `%LOCALAPPDATA%\Kalkan\logs`.
    /// * Other platforms (development only): `$XDG_CONFIG_HOME/kalkan` or
    ///   `$HOME/.config/kalkan`, logs under `.../kalkan/logs`.
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
    }

    fn from_lookup(get: impl Fn(&str) -> Option<PathBuf>) -> Option<Self> {
        if cfg!(windows) {
            let roaming = get("APPDATA")?;
            let local = get("LOCALAPPDATA").unwrap_or_else(|| roaming.clone());
            Some(Self {
                config_dir: roaming.join("Kalkan"),
                log_dir: local.join("Kalkan").join("logs"),
            })
        } else {
            let base = get("XDG_CONFIG_HOME").or_else(|| get("HOME").map(|h| h.join(".config")))?;
            let dir = base.join("kalkan");
            Some(Self {
                log_dir: dir.join("logs"),
                config_dir: dir,
            })
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_conservative() {
        let c = Config::default();
        assert!(c.validate().is_ok());
        assert!(!c.general.start_engine_on_launch);
    }

    #[test]
    fn empty_file_yields_defaults() {
        assert_eq!(Config::from_toml_str("").unwrap(), Config::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let c = Config::from_toml_str("[monitor]\nsample_interval_ms = 500\n").unwrap();
        assert_eq!(c.monitor.sample_interval_ms, 500);
        assert_eq!(c.logging, Logging::default());
    }

    #[test]
    fn round_trip() {
        let mut c = Config::default();
        c.logging.level = LogLevel::Debug;
        c.ui.start_minimized = true;
        let s = c.to_toml_string().unwrap();
        assert_eq!(Config::from_toml_str(&s).unwrap(), c);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = Config::from_toml_str("[monitor]\nsample_intervl_ms = 500\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        assert!(err.to_string().contains("sample_intervl_ms"), "{err}");
    }

    #[test]
    fn wrong_type_is_rejected() {
        let err = Config::from_toml_str("[logging]\nlevel = 3\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn bad_level_is_rejected() {
        assert!(Config::from_toml_str("[logging]\nlevel = \"loud\"\n").is_err());
    }

    #[test]
    fn all_range_errors_are_reported_together() {
        let err = Config::from_toml_str(
            "schema_version = 99\n[monitor]\nsample_interval_ms = 1\n[logging]\nmax_log_files = 0\n",
        )
        .unwrap_err();
        let ConfigError::Invalid(issues) = err else {
            panic!("expected Invalid, got {err}")
        };
        let keys: Vec<_> = issues.iter().map(|i| i.key).collect();
        assert_eq!(
            keys,
            [
                "schema_version",
                "monitor.sample_interval_ms",
                "logging.max_log_files"
            ]
        );
    }

    #[test]
    fn range_bounds_are_inclusive() {
        let (lo, hi) = SAMPLE_INTERVAL_MS_RANGE;
        for v in [lo, hi] {
            let s = format!("[monitor]\nsample_interval_ms = {v}\n");
            assert!(Config::from_toml_str(&s).is_ok(), "{v}");
        }
        for v in [lo - 1, hi + 1] {
            let s = format!("[monitor]\nsample_interval_ms = {v}\n");
            assert!(Config::from_toml_str(&s).is_err(), "{v}");
        }
    }

    #[test]
    fn load_missing_file_gives_defaults_and_save_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("kalkan-cfg-test-{}", std::process::id()));
        let path = dir.join("sub").join(CONFIG_FILE_NAME);
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(Config::load(&path).unwrap(), Config::default());

        let mut c = Config::default();
        c.monitor.sample_interval_ms = 2_000;
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file must be renamed away"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_refuses_invalid_config() {
        let mut c = Config::default();
        c.monitor.sample_interval_ms = 0;
        let path = std::env::temp_dir().join("kalkan-should-not-exist.toml");
        assert!(matches!(c.save(&path), Err(ConfigError::Invalid(_))));
        assert!(!path.exists());
    }

    #[test]
    fn phase4_files_without_new_keys_still_load() {
        // Exactly what Phase 1-4 wrote with --print-default-config.
        let old = "schema_version = 1\n\n[general]\nstart_engine_on_launch = false\n\n\
                   [monitor]\nsample_interval_ms = 1000\n\n[logging]\nlevel = \"info\"\n\
                   file_logging = true\nmax_log_files = 7\n\n[ui]\nstart_minimized = false\n";
        let c = Config::from_toml_str(old).unwrap();
        assert_eq!(c.logging.max_file_size_mb, 5, "new key gets its default");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn max_file_size_range() {
        let (lo, hi) = MAX_FILE_SIZE_MB_RANGE;
        for v in [lo, hi] {
            let s = format!("[logging]\nmax_file_size_mb = {v}\n");
            assert!(Config::from_toml_str(&s).is_ok(), "{v}");
        }
        for v in [0, hi + 1] {
            let s = format!("[logging]\nmax_file_size_mb = {v}\n");
            let Err(ConfigError::Invalid(i)) = Config::from_toml_str(&s) else {
                panic!("{v} should be invalid")
            };
            assert_eq!(i[0].key, "logging.max_file_size_mb");
        }
    }

    #[test]
    fn log_level_parse_and_names() {
        for l in LogLevel::ALL {
            assert_eq!(LogLevel::parse(l.as_str()), Some(l));
            assert_eq!(l.to_string(), l.as_str());
        }
        assert_eq!(LogLevel::parse(" WARNING "), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("loud"), None);
    }

    #[test]
    fn backup_never_overwrites_previous_backups() {
        let dir = std::env::temp_dir().join(format!("kalkan-cfg-bak-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);
        fs::write(&path, "broken = [").unwrap();
        let b1 = Config::backup_file(&path).unwrap();
        fs::write(&path, "broken again").unwrap();
        let b2 = Config::backup_file(&path).unwrap();
        assert_ne!(b1, b2);
        assert_eq!(fs::read_to_string(&b1).unwrap(), "broken = [");
        assert_eq!(fs::read_to_string(&b2).unwrap(), "broken again");
        assert!(b2.to_string_lossy().ends_with(".bak.2"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn app_dirs_resolution() {
        let env = |k: &str| match k {
            "APPDATA" => Some(PathBuf::from("R")),
            "LOCALAPPDATA" => Some(PathBuf::from("L")),
            "HOME" => Some(PathBuf::from("H")),
            _ => None,
        };
        let d = AppDirs::from_lookup(env).unwrap();
        if cfg!(windows) {
            assert_eq!(d.config_dir, PathBuf::from("R").join("Kalkan"));
            assert_eq!(d.log_dir, PathBuf::from("L").join("Kalkan").join("logs"));
        } else {
            assert_eq!(d.config_dir, PathBuf::from("H/.config/kalkan"));
        }
        assert!(AppDirs::from_lookup(|_| None).is_none());
    }
}
