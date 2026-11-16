//! Log viewer model (platform independent): level filter, text search,
//! pause, selection and export text. The entries come from
//! `kalkan_logging::LogBuffer`; nothing here invents log lines.

use std::sync::Arc;

use kalkan_config::LogLevel;
use kalkan_logging::LogEntry;

/// The level toggles shown in the toolbar. `Debug` also covers `Trace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LevelChip {
    Error,
    Warn,
    Info,
    Debug,
}

impl LevelChip {
    pub const ALL: [LevelChip; 4] = [
        LevelChip::Error,
        LevelChip::Warn,
        LevelChip::Info,
        LevelChip::Debug,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LevelChip::Error => "ERROR",
            LevelChip::Warn => "WARN",
            LevelChip::Info => "INFO",
            LevelChip::Debug => "DEBUG",
        }
    }

    pub fn of(level: LogLevel) -> LevelChip {
        match level {
            LogLevel::Error => LevelChip::Error,
            LogLevel::Warn => LevelChip::Warn,
            LogLevel::Info => LevelChip::Info,
            LogLevel::Debug | LogLevel::Trace => LevelChip::Debug,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    shown: [bool; 4],
    query: String,
    /// Lower-cased query, computed once per change.
    needle: String,
}

impl Default for LogFilter {
    fn default() -> Self {
        LogFilter {
            shown: [true; 4],
            query: String::new(),
            needle: String::new(),
        }
    }
}

impl LogFilter {
    pub fn shows(&self, chip: LevelChip) -> bool {
        self.shown[chip.index()]
    }
    pub fn set_shown(&mut self, chip: LevelChip, on: bool) {
        self.shown[chip.index()] = on;
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_owned();
        self.needle = q.trim().to_lowercase();
    }

    /// Case-insensitive search over level, thread, target and message.
    pub fn matches(&self, e: &LogEntry) -> bool {
        if !self.shows(LevelChip::of(e.level)) {
            return false;
        }
        if self.needle.is_empty() {
            return true;
        }
        let n = self.needle.as_str();
        e.message.to_lowercase().contains(n)
            || e.target.to_lowercase().contains(n)
            || e.thread.to_lowercase().contains(n)
            || e.level.as_str().contains(n)
    }
}

/// State of the Logs page.
#[derive(Debug, Clone, Default)]
pub struct LogView {
    pub filter: LogFilter,
    pub paused: bool,
    /// Last snapshot taken from the buffer (not refreshed while paused).
    entries: Vec<Arc<LogEntry>>,
    /// Indices into `entries` that pass the filter, oldest first.
    visible: Vec<u32>,
    /// Buffer version of `entries`.
    version: Option<u64>,
    /// Selected row (index into `visible`).
    pub selected: Option<usize>,
    /// The user is at the newest entry: keep following new entries.
    pub follow: bool,
}

impl LogView {
    pub fn new() -> Self {
        LogView {
            follow: true,
            ..Default::default()
        }
    }

    /// Take a new snapshot unless paused or unchanged. Returns `true` if the
    /// visible rows changed.
    /// Buffer version the view last showed (`None` before the first refresh).
    pub fn version(&self) -> Option<u64> {
        self.version
    }

    pub fn refresh(&mut self, version: u64, snapshot: impl FnOnce() -> Vec<Arc<LogEntry>>) -> bool {
        if self.paused || self.version == Some(version) {
            return false;
        }
        let selected_entry = self.selected_entry().cloned();
        self.entries = snapshot();
        self.version = Some(version);
        self.refilter();
        // Keep the same entry selected if it is still there.
        if let Some(sel) = selected_entry {
            self.selected = self
                .visible
                .iter()
                .position(|&i| Arc::ptr_eq(&self.entries[i as usize], &sel));
        }
        true
    }

    /// Recompute the visible rows after a filter change.
    pub fn refilter(&mut self) {
        let selected_entry = self.selected_entry().cloned();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.filter.matches(e))
            .map(|(i, _)| i as u32)
            .collect();
        self.selected = selected_entry.and_then(|sel| {
            self.visible
                .iter()
                .position(|&i| Arc::ptr_eq(&self.entries[i as usize], &sel))
        });
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            // Force the next refresh to take a fresh snapshot.
            self.version = None;
        }
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn total_len(&self) -> usize {
        self.entries.len()
    }

    pub fn row(&self, i: usize) -> Option<&Arc<LogEntry>> {
        self.visible
            .get(i)
            .and_then(|&k| self.entries.get(k as usize))
    }

    pub fn selected_entry(&self) -> Option<&Arc<LogEntry>> {
        self.selected.and_then(|i| self.row(i))
    }

    /// Per-chip counts over the whole snapshot (for the toolbar).
    pub fn counts(&self) -> [usize; 4] {
        let mut c = [0; 4];
        for e in &self.entries {
            c[LevelChip::of(e.level).index()] += 1;
        }
        c
    }

    /// Text for exporting the currently visible (filtered) rows.
    pub fn export_text(&self) -> String {
        let mut s = String::new();
        for i in 0..self.visible.len() {
            if let Some(e) = self.row(i) {
                s.push_str(&e.to_line());
                s.push_str("\r\n");
            }
        }
        s
    }
}

/// `kalkan-logs-20261115-204305.txt` from a UTC timestamp.
pub fn export_file_name(unix_ms: i64) -> String {
    let t = kalkan_logging::format_utc(unix_ms); // "2026-11-15 20:43:05.777"
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    format!("kalkan-logs-{}-{}.txt", &digits[..8], &digits[8..14])
}

#[cfg(test)]
#[path = "logview_tests.rs"]
mod tests;
