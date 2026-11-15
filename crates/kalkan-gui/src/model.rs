//! Platform-independent UI model: pages, state, and what each page shows.
//!
//! **Honesty rule.** Every displayed value is a [`Value`]. A value is either
//! [`Value::Real`] with the [`Source`] it came from, or
//! [`Value::Unavailable`] with the reason. Metric cards only become `Real`
//! when an [`EngineStatus`] from the engine contains the number. The GUI never
//! invents data.

use kalkan_core::{EngineState, EngineStatus, Event};

/// Navigation pages, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Dashboard,
    Network,
    Statistics,
    Logs,
    Settings,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Dashboard,
        Page::Network,
        Page::Statistics,
        Page::Logs,
        Page::Settings,
    ];

    pub fn index(self) -> usize {
        Page::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Option<Page> {
        Page::ALL.get(i).copied()
    }

    /// Sidebar label.
    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => "Dashboard",
            Page::Network => "Network Monitoring",
            Page::Statistics => "Statistics",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }

    /// One-line description under the page title.
    pub fn subtitle(self) -> &'static str {
        match self {
            Page::Dashboard => "Engine status and system overview",
            Page::Network => "Read-only view of network interface activity",
            Page::Statistics => "Totals and trends computed from real monitor samples",
            Page::Logs => "Diagnostic messages from KALKAN",
            Page::Settings => "Current configuration",
        }
    }
}

/// Where a real value came from. Shown as a badge next to the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Measured by the running engine.
    Engine,
    /// Queried from the operating system.
    System,
    /// Read from the configuration file.
    Config,
}

impl Source {
    pub fn badge(self) -> &'static str {
        match self {
            Source::Engine => "LIVE",
            Source::System => "SYSTEM",
            Source::Config => "CONFIG",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Real { text: String, source: Source },
    Unavailable { reason: &'static str },
}

impl Value {
    pub fn real(text: impl Into<String>, source: Source) -> Self {
        Value::Real {
            text: text.into(),
            source,
        }
    }
    pub fn is_real(&self) -> bool {
        matches!(self, Value::Real { .. })
    }
    /// Badge label.
    pub fn badge(&self) -> &'static str {
        match self {
            Value::Real { source, .. } => source.badge(),
            Value::Unavailable { .. } => "NOT AVAILABLE",
        }
    }
    /// Main text.
    pub fn text(&self) -> &str {
        match self {
            Value::Real { text, .. } => text,
            Value::Unavailable { .. } => "Not available",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub title: &'static str,
    pub value: Value,
    /// Secondary line. For unavailable values this is the reason.
    pub detail: String,
}

impl Card {
    fn new(title: &'static str, value: Value, detail: impl Into<String>) -> Self {
        let detail = match &value {
            Value::Unavailable { reason } => (*reason).to_owned(),
            Value::Real { .. } => detail.into(),
        };
        Card {
            title,
            value,
            detail,
        }
    }
}

/// Static facts about this installation, gathered by the app at start-up.
/// Every field here is real data. The GUI does not query the OS itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppInfo {
    pub version: String,
    pub platform: String,
    /// `None` if the privilege query failed.
    pub elevated: Option<bool>,
    pub config_path: String,
    /// Whether `config_path` exists. If not, defaults are in use.
    pub config_file_exists: bool,
    /// Display rows: (setting, value).
    pub settings: Vec<(String, String)>,
    pub log_level: String,
    /// Shown in the engine panel while the engine is not implemented.
    pub engine_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub message: String,
}

/// Mutable UI state. Changes only on user input or engine events.
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    pub page: Page,
    pub engine: EngineState,
    pub status: EngineStatus,
    pub notice: Option<Notice>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::Dashboard,
            engine: EngineState::Stopped,
            status: EngineStatus::default(),
            notice: None,
        }
    }
}

impl UiState {
    /// Navigate. Returns `true` if the page changed.
    pub fn navigate(&mut self, page: Page) -> bool {
        let changed = self.page != page;
        self.page = page;
        changed
    }

    /// Apply an engine event. Returns `true` if anything visible changed.
    pub fn apply(&mut self, ev: Event) -> bool {
        match ev {
            Event::StateChanged { to, .. } => {
                let changed = self.engine != to;
                self.engine = to;
                changed
            }
            Event::Status(s) => {
                if let Some(st) = s.state {
                    self.engine = st;
                }
                let changed = self.status != s;
                self.status = s;
                changed
            }
            Event::Error { message } => {
                self.notice = Some(Notice {
                    kind: NoticeKind::Error,
                    message,
                });
                true
            }
            Event::Info { message } => {
                self.notice = Some(Notice {
                    kind: NoticeKind::Info,
                    message,
                });
                true
            }
            Event::ShutdownComplete => false,
        }
    }

    pub fn can_start(&self) -> bool {
        self.engine.can_start()
    }
    pub fn can_stop(&self) -> bool {
        self.engine.can_stop()
    }
}

/// Everything a page draws, besides the title.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageView {
    /// Dashboard only: engine state panel with Start/Stop.
    pub engine_panel: bool,
    pub cards: Vec<Card>,
    /// Key/value table (Settings).
    pub table: Vec<(String, String)>,
    /// Explanatory paragraph at the bottom.
    pub note: Option<&'static str>,
}

const PHASE3_ENGINE: &str = "Measured by the engine (Phase 3)";
const PHASE4_MONITOR: &str = "Requires the network monitor (Phase 4)";
const PHASE5_LOGS: &str = "In-app log view arrives in Phase 5";

pub fn privilege_value(elevated: Option<bool>) -> Value {
    match elevated {
        Some(true) => Value::real("Administrator", Source::System),
        Some(false) => Value::real("Standard user", Source::System),
        None => Value::Unavailable {
            reason: "Privilege query failed",
        },
    }
}

pub fn page_view(page: Page, ui: &UiState, info: &AppInfo) -> PageView {
    let s = &ui.status;
    match page {
        Page::Dashboard => PageView {
            engine_panel: true,
            cards: vec![
                Card::new(
                    "CPU usage",
                    s.cpu_percent.map_or(
                        Value::Unavailable {
                            reason: PHASE3_ENGINE,
                        },
                        |p| Value::real(format!("{p:.1} %"), Source::Engine),
                    ),
                    "KALKAN process, one core = 100 %",
                ),
                Card::new(
                    "Memory usage",
                    s.memory_bytes.map_or(
                        Value::Unavailable {
                            reason: PHASE3_ENGINE,
                        },
                        |b| Value::real(format_bytes(b), Source::Engine),
                    ),
                    "KALKAN process working set",
                ),
                Card::new(
                    "Network activity",
                    rx_tx(s),
                    "Receive / send, all interfaces",
                ),
                Card::new(
                    "Privileges",
                    privilege_value(info.elevated),
                    "Not required for monitoring",
                ),
            ],
            table: vec![],
            note: None,
        },
        Page::Network => PageView {
            cards: vec![
                Card::new(
                    "Receive rate",
                    s.rx_bytes_per_sec.map_or(
                        Value::Unavailable {
                            reason: PHASE4_MONITOR,
                        },
                        |r| Value::real(format_rate(r), Source::Engine),
                    ),
                    "All interfaces",
                ),
                Card::new(
                    "Send rate",
                    s.tx_bytes_per_sec.map_or(
                        Value::Unavailable {
                            reason: PHASE4_MONITOR,
                        },
                        |r| Value::real(format_rate(r), Source::Engine),
                    ),
                    "All interfaces",
                ),
                Card::new(
                    "Interfaces",
                    Value::Unavailable {
                        reason: PHASE4_MONITOR,
                    },
                    "",
                ),
            ],
            note: Some(
                "Monitoring is read-only. KALKAN reads the interface counters that Windows \
                 already keeps. It never modifies, blocks or injects traffic.",
            ),
            ..PageView::default()
        },
        Page::Statistics => PageView {
            cards: vec![
                Card::new(
                    "Session received",
                    Value::Unavailable {
                        reason: PHASE4_MONITOR,
                    },
                    "",
                ),
                Card::new(
                    "Session sent",
                    Value::Unavailable {
                        reason: PHASE4_MONITOR,
                    },
                    "",
                ),
                Card::new(
                    "Peak receive rate",
                    Value::Unavailable {
                        reason: PHASE4_MONITOR,
                    },
                    "",
                ),
                Card::new(
                    "Peak send rate",
                    Value::Unavailable {
                        reason: PHASE4_MONITOR,
                    },
                    "",
                ),
            ],
            note: Some(
                "Statistics are computed only from real monitor samples. Nothing is shown \
                 until the network monitor exists.",
            ),
            ..PageView::default()
        },
        Page::Logs => PageView {
            cards: vec![
                Card::new(
                    "Log level",
                    Value::real(info.log_level.clone(), Source::Config),
                    "logging.level in kalkan.toml",
                ),
                Card::new(
                    "Log viewer",
                    Value::Unavailable {
                        reason: PHASE5_LOGS,
                    },
                    "",
                ),
                Card::new(
                    "Log files",
                    Value::Unavailable {
                        reason: "File logging arrives in Phase 5",
                    },
                    "",
                ),
            ],
            note: Some(
                "In the GUI, log messages are not captured anywhere yet. Run KALKAN from a \
                 terminal with --self-test or the CLI options to see diagnostic output.",
            ),
            ..PageView::default()
        },
        Page::Settings => PageView {
            cards: vec![Card::new(
                "Configuration file",
                Value::real(
                    if info.config_file_exists {
                        "Loaded"
                    } else {
                        "Defaults (no file yet)"
                    },
                    Source::Config,
                ),
                info.config_path.clone(),
            )],
            table: info.settings.clone(),
            note: Some(
                "Settings are read-only in this version. Edit kalkan.toml and restart KALKAN. \
                 Editing in the app arrives in Phase 5.",
            ),
            ..PageView::default()
        },
    }
}

fn rx_tx(s: &EngineStatus) -> Value {
    match (s.rx_bytes_per_sec, s.tx_bytes_per_sec) {
        (Some(rx), Some(tx)) => Value::real(
            format!("↓ {}  ↑ {}", format_rate(rx), format_rate(tx)),
            Source::Engine,
        ),
        _ => Value::Unavailable {
            reason: PHASE4_MONITOR,
        },
    }
}

/// Human-readable size in binary units (KiB = 1024 B), e.g. `"12.3 MB"`.
pub fn format_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn format_rate(bps: u64) -> String {
    format!("{}/s", format_bytes(bps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> AppInfo {
        AppInfo {
            log_level: "info".into(),
            elevated: Some(false),
            ..AppInfo::default()
        }
    }

    #[test]
    fn page_index_round_trip() {
        for p in Page::ALL {
            assert_eq!(Page::from_index(p.index()), Some(p));
        }
        assert_eq!(Page::from_index(5), None);
    }

    #[test]
    fn page_labels_match_spec() {
        let labels: Vec<_> = Page::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(
            labels,
            [
                "Dashboard",
                "Network Monitoring",
                "Statistics",
                "Logs",
                "Settings"
            ]
        );
    }

    #[test]
    fn no_engine_data_means_no_real_metrics_anywhere() {
        let ui = UiState::default();
        for p in Page::ALL {
            for c in page_view(p, &ui, &info()).cards {
                if let Value::Real { source, .. } = &c.value {
                    assert_ne!(
                        *source,
                        Source::Engine,
                        "{p:?}/{} claims live data",
                        c.title
                    );
                }
            }
        }
    }

    #[test]
    fn metrics_become_real_only_when_engine_reports_them() {
        let mut ui = UiState::default();
        ui.apply(Event::Status(EngineStatus {
            state: Some(EngineState::Running),
            cpu_percent: Some(1.3),
            memory_bytes: Some(5 * 1024 * 1024),
            rx_bytes_per_sec: Some(2048),
            tx_bytes_per_sec: None,
        }));
        assert_eq!(ui.engine, EngineState::Running);
        let v = page_view(Page::Dashboard, &ui, &info());
        assert_eq!(v.cards[0].value, Value::real("1.3 %", Source::Engine));
        assert_eq!(v.cards[1].value, Value::real("5.0 MB", Source::Engine));
        // rx without tx: the combined card must not show a half-real value.
        assert!(!v.cards[2].value.is_real());
        let n = page_view(Page::Network, &ui, &info());
        assert!(n.cards[0].value.is_real());
        assert!(!n.cards[1].value.is_real());
    }

    #[test]
    fn unavailable_cards_explain_why() {
        let v = page_view(Page::Dashboard, &UiState::default(), &info());
        assert_eq!(v.cards[0].detail, PHASE3_ENGINE);
        assert_eq!(v.cards[0].value.badge(), "NOT AVAILABLE");
    }

    #[test]
    fn privileges_are_system_data() {
        assert_eq!(
            privilege_value(Some(true)),
            Value::real("Administrator", Source::System)
        );
        assert!(!privilege_value(None).is_real());
    }

    #[test]
    fn events_update_state_and_notices() {
        let mut ui = UiState::default();
        assert!(ui.apply(Event::Info {
            message: "hi".into()
        }));
        assert_eq!(ui.notice.as_ref().unwrap().kind, NoticeKind::Info);
        assert!(ui.apply(Event::Error {
            message: "bad".into()
        }));
        assert_eq!(ui.notice.as_ref().unwrap().kind, NoticeKind::Error);
        assert!(ui.apply(Event::StateChanged {
            from: EngineState::Stopped,
            to: EngineState::Starting
        }));
        assert!(ui.can_stop() && !ui.can_start());
        assert!(!ui.apply(Event::ShutdownComplete));
    }

    #[test]
    fn navigate_reports_change() {
        let mut ui = UiState::default();
        assert!(!ui.navigate(Page::Dashboard));
        assert!(ui.navigate(Page::Logs));
        assert_eq!(ui.page, Page::Logs);
    }

    #[test]
    fn only_dashboard_has_engine_panel() {
        for p in Page::ALL {
            assert_eq!(
                page_view(p, &UiState::default(), &info()).engine_panel,
                p == Page::Dashboard
            );
        }
    }

    #[test]
    fn settings_page_shows_config_rows() {
        let mut i = info();
        i.settings = vec![("Log level".into(), "info".into())];
        i.config_path = "C:\\x\\kalkan.toml".into();
        let v = page_view(Page::Settings, &UiState::default(), &i);
        assert_eq!(v.table, i.settings);
        assert_eq!(v.cards[0].detail, "C:\\x\\kalkan.toml");
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536 * 1024), "1.5 MB");
        assert_eq!(format_rate(2048), "2.0 KB/s");
        assert!(format_bytes(u64::MAX).ends_with("TB"));
    }
}
