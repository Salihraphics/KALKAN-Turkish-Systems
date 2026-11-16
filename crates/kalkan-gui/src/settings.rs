//! Settings form model (platform independent).
//!
//! The form holds what the user typed (numbers as text, so invalid input can
//! be shown and explained instead of being silently coerced). Conversion back
//! to a [`Config`] uses `kalkan-config`'s own validation, so the GUI can never
//! accept a value the loader would reject.

use kalkan_config::{
    Config, LogLevel, ValidationIssue, MAX_FILE_SIZE_MB_RANGE, MAX_LOG_FILES_RANGE,
    SAMPLE_INTERVAL_MS_RANGE,
};

/// Every editable setting. Only settings with a real implementation exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    LogLevel,
    MaxFileSizeMb,
    MaxLogFiles,
    FileLogging,
    SampleIntervalMs,
    StartEngineOnLaunch,
    StartMinimized,
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Applied as soon as it is saved.
    Immediately,
    /// The next time the engine starts.
    NextEngineStart,
    /// The next time KALKAN starts.
    NextLaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One of the five log levels.
    Level,
    Number,
    Toggle,
}

impl Field {
    pub const ALL: [Field; 7] = [
        Field::LogLevel,
        Field::MaxFileSizeMb,
        Field::MaxLogFiles,
        Field::FileLogging,
        Field::SampleIntervalMs,
        Field::StartEngineOnLaunch,
        Field::StartMinimized,
    ];

    pub fn kind(self) -> Kind {
        match self {
            Field::LogLevel => Kind::Level,
            Field::MaxFileSizeMb | Field::MaxLogFiles | Field::SampleIntervalMs => Kind::Number,
            _ => Kind::Toggle,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::LogLevel => "Log level",
            Field::MaxFileSizeMb => "Maximum log file size",
            Field::MaxLogFiles => "Log archives to keep",
            Field::FileLogging => "Write logs to a file",
            Field::SampleIntervalMs => "Sample interval",
            Field::StartEngineOnLaunch => "Start engine on launch",
            Field::StartMinimized => "Start minimized",
        }
    }

    /// Help text shown under the label, including the allowed range.
    pub fn help(self) -> String {
        match self {
            Field::LogLevel => "Least severe level that is recorded (file and viewer)".into(),
            Field::MaxFileSizeMb => {
                let (lo, hi) = MAX_FILE_SIZE_MB_RANGE;
                format!("MB, {lo}–{hi}. kalkan.log is rotated before it grows past this")
            }
            Field::MaxLogFiles => {
                let (lo, hi) = MAX_LOG_FILES_RANGE;
                format!("{lo}–{hi} rotated files (kalkan.1.log …). Older ones are deleted")
            }
            Field::FileLogging => "Also write logs to kalkan.log (applies after restart)".into(),
            Field::SampleIntervalMs => {
                let (lo, hi) = SAMPLE_INTERVAL_MS_RANGE;
                format!("ms, {lo}–{hi}. How often network and process data are sampled")
            }
            Field::StartEngineOnLaunch => "Start GoodBoyDPI when KALKAN opens".into(),
            Field::StartMinimized => "Open KALKAN minimized".into(),
        }
    }

    pub fn applies(self) -> Applies {
        match self {
            Field::LogLevel | Field::MaxFileSizeMb | Field::MaxLogFiles => Applies::Immediately,
            Field::SampleIntervalMs => Applies::NextEngineStart,
            Field::FileLogging | Field::StartEngineOnLaunch | Field::StartMinimized => {
                Applies::NextLaunch
            }
        }
    }

    /// The configuration key the field edits.
    pub fn key(self) -> &'static str {
        match self {
            Field::LogLevel => "logging.level",
            Field::MaxFileSizeMb => "logging.max_file_size_mb",
            Field::MaxLogFiles => "logging.max_log_files",
            Field::FileLogging => "logging.file_logging",
            Field::SampleIntervalMs => "monitor.sample_interval_ms",
            Field::StartEngineOnLaunch => "general.start_engine_on_launch",
            Field::StartMinimized => "ui.start_minimized",
        }
    }

    fn from_key(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.key() == key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// `None` for problems not tied to an editable field.
    pub field: Option<Field>,
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.field {
            Some(field) => write!(f, "{}: {}", field.label(), self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// What the user has entered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    pub log_level: LogLevel,
    pub max_file_size_mb: String,
    pub max_log_files: String,
    pub file_logging: bool,
    pub sample_interval_ms: String,
    pub start_engine_on_launch: bool,
    pub start_minimized: bool,
}

impl SettingsForm {
    pub fn from_config(c: &Config) -> Self {
        SettingsForm {
            log_level: c.logging.level,
            max_file_size_mb: c.logging.max_file_size_mb.to_string(),
            max_log_files: c.logging.max_log_files.to_string(),
            file_logging: c.logging.file_logging,
            sample_interval_ms: c.monitor.sample_interval_ms.to_string(),
            start_engine_on_launch: c.general.start_engine_on_launch,
            start_minimized: c.ui.start_minimized,
        }
    }

    pub fn text(&self, f: Field) -> Option<&str> {
        match f {
            Field::MaxFileSizeMb => Some(&self.max_file_size_mb),
            Field::MaxLogFiles => Some(&self.max_log_files),
            Field::SampleIntervalMs => Some(&self.sample_interval_ms),
            _ => None,
        }
    }

    pub fn set_text(&mut self, f: Field, v: String) {
        match f {
            Field::MaxFileSizeMb => self.max_file_size_mb = v,
            Field::MaxLogFiles => self.max_log_files = v,
            Field::SampleIntervalMs => self.sample_interval_ms = v,
            _ => {}
        }
    }

    pub fn toggle(&self, f: Field) -> Option<bool> {
        match f {
            Field::FileLogging => Some(self.file_logging),
            Field::StartEngineOnLaunch => Some(self.start_engine_on_launch),
            Field::StartMinimized => Some(self.start_minimized),
            _ => None,
        }
    }

    pub fn set_toggle(&mut self, f: Field, v: bool) {
        match f {
            Field::FileLogging => self.file_logging = v,
            Field::StartEngineOnLaunch => self.start_engine_on_launch = v,
            Field::StartMinimized => self.start_minimized = v,
            _ => {}
        }
    }

    /// Parse and validate. `base` supplies everything the form does not edit
    /// (schema version), so nothing else in the file is lost.
    pub fn to_config(&self, base: &Config) -> Result<Config, Vec<FieldError>> {
        let mut errors = Vec::new();
        let mut num = |f: Field, text: &str| -> u64 {
            match text.trim().parse::<u64>() {
                Ok(v) => v,
                Err(_) => {
                    errors.push(FieldError {
                        field: Some(f),
                        message: if text.trim().is_empty() {
                            "a value is required".into()
                        } else {
                            format!("\"{}\" is not a whole number", text.trim())
                        },
                    });
                    0
                }
            }
        };
        let size = num(Field::MaxFileSizeMb, &self.max_file_size_mb);
        let keep = num(Field::MaxLogFiles, &self.max_log_files);
        let interval = num(Field::SampleIntervalMs, &self.sample_interval_ms);
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut c = base.clone();
        c.logging.level = self.log_level;
        // Saturate into the field type; out-of-range values are reported by
        // validate() below with the real limits.
        c.logging.max_file_size_mb = size.min(u32::MAX as u64) as u32;
        c.logging.max_log_files = keep.min(u16::MAX as u64) as u16;
        c.logging.file_logging = self.file_logging;
        c.monitor.sample_interval_ms = interval.min(u32::MAX as u64) as u32;
        c.general.start_engine_on_launch = self.start_engine_on_launch;
        c.ui.start_minimized = self.start_minimized;
        c.validate()
            .map_err(|issues| issues.into_iter().map(issue_to_error).collect::<Vec<_>>())?;
        Ok(c)
    }
}

fn issue_to_error(i: ValidationIssue) -> FieldError {
    FieldError {
        field: Field::from_key(i.key),
        message: i.message,
    }
}

/// The configuration file as it was found at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// No file yet: defaults are in use; saving creates it.
    Missing,
    Loaded,
    /// The file exists but could not be used. Defaults are in use; the file
    /// is left untouched until the user saves (a `.bak` copy is made first).
    Invalid(String),
}

/// Everything the Settings page needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    pub path: String,
    pub file: FileState,
    /// Values currently in effect (last loaded or saved).
    pub saved: Config,
    pub form: SettingsForm,
    pub errors: Vec<FieldError>,
    /// A save is running on the I/O thread.
    pub saving: bool,
}

impl SettingsState {
    pub fn new(path: String, file: FileState, saved: Config) -> Self {
        let form = SettingsForm::from_config(&saved);
        SettingsState {
            path,
            file,
            saved,
            form,
            errors: Vec::new(),
            saving: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.form != SettingsForm::from_config(&self.saved)
    }

    /// Discard edits.
    pub fn revert(&mut self) {
        self.form = SettingsForm::from_config(&self.saved);
        self.errors.clear();
    }

    /// Put the defaults into the form (the caller asks for confirmation and
    /// then saves).
    pub fn load_defaults_into_form(&mut self) {
        self.form = SettingsForm::from_config(&Config::default());
        self.errors.clear();
    }

    pub fn error_for(&self, f: Field) -> Option<&FieldError> {
        self.errors.iter().find(|e| e.field == Some(f))
    }

    /// Record a successful save.
    pub fn saved_as(&mut self, c: Config) {
        self.saved = c;
        self.form = SettingsForm::from_config(&self.saved);
        self.file = FileState::Loaded;
        self.errors.clear();
        self.saving = false;
    }
}

/// Fields whose saved value differs between `old` and `new`.
pub fn changed_fields(old: &Config, new: &Config) -> Vec<Field> {
    let (a, b) = (
        SettingsForm::from_config(old),
        SettingsForm::from_config(new),
    );
    Field::ALL
        .into_iter()
        .filter(|&f| match f.kind() {
            Kind::Level => a.log_level != b.log_level,
            Kind::Number => a.text(f) != b.text(f),
            Kind::Toggle => a.toggle(f) != b.toggle(f),
        })
        .collect()
}

/// User-facing summary of a save.
pub fn save_message(changed: &[Field]) -> String {
    if changed.is_empty() {
        return "Settings saved (no values changed).".into();
    }
    let later: Vec<&str> = changed
        .iter()
        .filter(|f| f.applies() == Applies::NextLaunch)
        .map(|f| f.label())
        .collect();
    let next_start: Vec<&str> = changed
        .iter()
        .filter(|f| f.applies() == Applies::NextEngineStart)
        .map(|f| f.label())
        .collect();
    let mut m = String::from("Settings saved.");
    if !next_start.is_empty() {
        m.push_str(&format!(
            " {} applies the next time the engine starts.",
            next_start.join(", ")
        ));
    }
    if !later.is_empty() {
        m.push_str(&format!(
            " {} applies when KALKAN is restarted.",
            later.join(", ")
        ));
    }
    m
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
