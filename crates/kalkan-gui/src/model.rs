//! Platform-independent UI model: pages, state, and what each page shows.
//!
//! **Honesty rule.** Every displayed value is a [`Value`]. A value is either
//! [`Value::Real`] with the [`Source`] it came from, or
//! [`Value::Unavailable`] with the reason. Metric cards only become `Real`
//! when an [`EngineStatus`] from the engine contains the number. The GUI never
//! invents data.

use kalkan_core::{DiagLevel, EngineState, EngineStatus, Event, InterfaceStatus, NetworkStatus};

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
    /// Measured by the running engine (process metrics, network monitor).
    Engine,
    /// Queried from the operating system at start-up.
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
    Real {
        text: String,
        source: Source,
    },
    /// No real value exists; `reason` says why (shown to the user).
    Unavailable {
        reason: String,
    },
}

impl Value {
    pub fn real(text: impl Into<String>, source: Source) -> Self {
        Value::Real {
            text: text.into(),
            source,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Value::Unavailable {
            reason: reason.into(),
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

/// Line icons drawn by the GUI (no image resources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Dashboard,
    Network,
    Statistics,
    Logs,
    Settings,
    Download,
    Upload,
    Cpu,
    Memory,
    Shield,
    Interface,
    Clock,
    Peak,
    Average,
    File,
    Search,
}

impl Page {
    pub fn icon(self) -> Icon {
        match self {
            Page::Dashboard => Icon::Dashboard,
            Page::Network => Icon::Network,
            Page::Statistics => Icon::Statistics,
            Page::Logs => Icon::Logs,
            Page::Settings => Icon::Settings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub title: &'static str,
    pub icon: Icon,
    pub value: Value,
    /// Secondary line. For unavailable values this is the reason.
    pub detail: String,
}

impl Card {
    fn new(title: &'static str, icon: Icon, value: Value, detail: impl Into<String>) -> Self {
        let detail = match &value {
            Value::Unavailable { reason } => reason.clone(),
            Value::Real { .. } => detail.into(),
        };
        Card {
            title,
            icon,
            value,
            detail,
        }
    }
}

/// One labelled value in a horizontal information strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripItem {
    pub label: &'static str,
    pub icon: Icon,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Download,
    Upload,
}

/// A large live metric with a history graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub title: &'static str,
    pub icon: Icon,
    pub series: Series,
    pub value: Value,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub title: &'static str,
    /// Share of the spare width.
    pub weight: u16,
    /// Minimum width in DIP.
    pub min_w: i32,
    pub align: Align,
    /// 0 = always shown; higher numbers are hidden first when space is short.
    pub priority: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    None,
    /// Connected and counted in totals.
    Connected,
    /// Up, but not counted (virtual adapter).
    Virtual,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<String>,
    pub status: RowStatus,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// Shown instead of rows when there are none.
    pub empty: Option<String>,
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
    /// Full path of the log file in use, if logging goes to a file.
    pub log_file: Option<String>,
    /// Why there is no log file (e.g. disabled in the configuration).
    pub log_note: String,
    /// Send Start as soon as the window exists (`general.start_engine_on_launch`).
    pub start_engine_on_launch: bool,
    /// Engine sample interval (`monitor.sample_interval_ms`).
    pub sample_interval_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub message: String,
}

/// How many lifecycle events the GUI has received (diagnostics and tests).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventCounts {
    pub started: u32,
    pub stopped: u32,
    pub errors: u32,
    pub diagnostics: u32,
}

/// Number of samples kept for the traffic graphs (60 s at the default 1 s).
pub const HISTORY_LEN: usize = 60;

/// Fixed-size ring buffer of download/upload rates. No allocation after
/// construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    rx: [u64; HISTORY_LEN],
    tx: [u64; HISTORY_LEN],
    head: usize,
    len: usize,
}

impl Default for History {
    fn default() -> Self {
        History {
            rx: [0; HISTORY_LEN],
            tx: [0; HISTORY_LEN],
            head: 0,
            len: 0,
        }
    }
}

impl History {
    pub fn push(&mut self, rx: u64, tx: u64) {
        self.rx[self.head] = rx;
        self.tx[self.head] = tx;
        self.head = (self.head + 1) % HISTORY_LEN;
        self.len = (self.len + 1).min(HISTORY_LEN);
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Values of one series, oldest first.
    pub fn iter(&self, series: Series) -> impl Iterator<Item = u64> + '_ {
        let buf = match series {
            Series::Download => &self.rx,
            Series::Upload => &self.tx,
        };
        let start = (self.head + HISTORY_LEN - self.len) % HISTORY_LEN;
        (0..self.len).map(move |i| buf[(start + i) % HISTORY_LEN])
    }
    pub fn max(&self, series: Series) -> u64 {
        self.iter(series).max().unwrap_or(0)
    }
}

/// Mutable UI state. Changes only on user input or engine events.
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    pub page: Page,
    /// Last state reported by the engine. Never set by the GUI itself.
    pub engine: EngineState,
    pub status: EngineStatus,
    pub notice: Option<Notice>,
    pub counts: EventCounts,
    /// Download/upload rates of the current run, for the graphs.
    pub history: History,
    /// Result of a page action (save, export, copy), shown on that page.
    pub page_notice: Option<(Page, Notice)>,
    /// The engine confirmed shutdown (last event it will ever send).
    pub shutdown_complete: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::Dashboard,
            engine: EngineState::Stopped,
            status: EngineStatus::default(),
            notice: None,
            counts: EventCounts::default(),
            history: History::default(),
            page_notice: None,
            shutdown_complete: false,
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
                self.engine = to;
                self.status.state = Some(to);
                if to != EngineState::Running {
                    // Measurements describe a running engine only.
                    self.status.cpu_percent = None;
                    self.status.memory_bytes = None;
                    self.status.network = None;
                    self.status.network_error = None;
                    self.status.running_for = None;
                }
                true
            }
            Event::EngineStarted => {
                self.counts.started += 1;
                self.notice = None;
                self.history.clear();
                true
            }
            Event::EngineStopped => {
                self.counts.stopped += 1;
                self.notice = None;
                true
            }
            Event::EngineError { message } => {
                self.counts.errors += 1;
                self.notice = Some(Notice {
                    kind: NoticeKind::Error,
                    message,
                });
                true
            }
            Event::DiagnosticMessage { level, message } => {
                self.counts.diagnostics += 1;
                let kind = match level {
                    DiagLevel::Info => NoticeKind::Info,
                    DiagLevel::Warning => NoticeKind::Warning,
                };
                self.notice = Some(Notice { kind, message });
                true
            }
            Event::Status(s) => {
                if let Some(st) = s.state {
                    self.engine = st;
                }
                if let Some(n) = &s.network {
                    if let (Some(rx), Some(tx)) = (n.rx_rate, n.tx_rate) {
                        self.history.push(rx, tx);
                    }
                }
                let changed = self.status != s;
                self.status = s;
                changed
            }
            Event::ShutdownComplete => {
                self.shutdown_complete = true;
                false
            }
        }
    }

    /// The notice to show on the current page, if any: engine notices on the
    /// Dashboard, action results on the page that produced them.
    pub fn visible_notice(&self) -> Option<&Notice> {
        match &self.page_notice {
            Some((p, n)) if *p == self.page => Some(n),
            _ if self.page == Page::Dashboard => self.notice.as_ref(),
            _ => None,
        }
    }

    pub fn set_page_notice(&mut self, page: Page, kind: NoticeKind, message: impl Into<String>) {
        self.page_notice = Some((
            page,
            Notice {
                kind,
                message: message.into(),
            },
        ));
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
    /// Dashboard only: engine panel with identity, state and Start/Stop.
    pub engine_panel: bool,
    pub tiles: Vec<Tile>,
    pub cards: Vec<Card>,
    pub strip: Vec<StripItem>,
    pub table: Option<Table>,
    /// Explanatory paragraph at the bottom.
    pub note: Option<String>,
    /// Action buttons in the header (Logs: Pause/Copy/Export; Settings:
    /// Save/Revert/Reset).
    pub header_buttons: usize,
    /// Logs page: native search box, level chips and log list.
    pub log_viewer: bool,
    /// Settings page: editable form with this many rows.
    pub settings_rows: usize,
}

const NEEDS_RUNNING: &str = "Measured while the engine is running";
const FIRST_SAMPLE: &str = "Waiting for the first sample";
const NET_NEEDS_RUNNING: &str = "Start the engine to monitor network activity";
const NET_FIRST_SAMPLES: &str = "Waiting for two samples to compute a rate";

pub fn privilege_value(elevated: Option<bool>) -> Value {
    match elevated {
        Some(true) => Value::real("Administrator", Source::System),
        Some(false) => Value::real("Standard user", Source::System),
        None => Value::unavailable("Privilege query failed"),
    }
}

/// Why network numbers are missing right now.
fn net_reason(ui: &UiState) -> String {
    if ui.engine != EngineState::Running {
        return NET_NEEDS_RUNNING.into();
    }
    match (&ui.status.network, &ui.status.network_error) {
        (_, Some(e)) => format!("Network monitor unavailable: {e}"),
        (Some(_), None) => NET_FIRST_SAMPLES.into(),
        (None, None) => FIRST_SAMPLE.into(),
    }
}

fn rate_value(ui: &UiState, rate: impl Fn(&NetworkStatus) -> Option<u64>) -> Value {
    match ui.status.network.as_ref().and_then(&rate) {
        Some(r) => Value::real(format_rate(r), Source::Engine),
        None => Value::unavailable(net_reason(ui)),
    }
}

fn net_value(ui: &UiState, f: impl Fn(&NetworkStatus) -> String) -> Value {
    match &ui.status.network {
        Some(n) => Value::real(f(n), Source::Engine),
        None => Value::unavailable(net_reason(ui)),
    }
}

fn metric_reason(ui: &UiState) -> &'static str {
    if ui.engine == EngineState::Running {
        FIRST_SAMPLE
    } else {
        NEEDS_RUNNING
    }
}

fn interface_summary(i: &InterfaceStatus) -> String {
    match i.link_speed_bps {
        Some(s) => format!("{} · {}", i.name, format_link_speed(s)),
        None => i.name.clone(),
    }
}

fn interfaces_table(ui: &UiState) -> Table {
    let col = |title, weight, min_w, align, priority| Column {
        title,
        weight,
        min_w,
        align,
        priority,
    };
    let columns = vec![
        col("Interface", 4, 120, Align::Left, 0),
        col("Type", 2, 64, Align::Left, 3),
        col("Link", 2, 72, Align::Right, 4),
        col("Download", 2, 80, Align::Right, 0),
        col("Upload", 2, 80, Align::Right, 0),
        col("Received", 2, 80, Align::Right, 1),
        col("Sent", 2, 80, Align::Right, 2),
    ];
    let (rows, empty) = match &ui.status.network {
        Some(n) if n.interfaces.is_empty() => {
            (Vec::new(), Some("No network interfaces were found.".into()))
        }
        Some(n) => {
            let rate = |r: Option<u64>| r.map_or("…".to_owned(), format_rate);
            let rows = n
                .interfaces
                .iter()
                .map(|i| Row {
                    cells: vec![
                        i.name.clone(),
                        i.kind.label().to_owned(),
                        i.link_speed_bps.map_or("–".to_owned(), format_link_speed),
                        rate(i.rx_rate),
                        rate(i.tx_rate),
                        format_bytes(i.rx_bytes),
                        format_bytes(i.tx_bytes),
                    ],
                    status: match (i.connected, i.counted) {
                        (true, true) => RowStatus::Connected,
                        (true, false) => RowStatus::Virtual,
                        (false, _) => RowStatus::Disconnected,
                    },
                    muted: !i.counted,
                })
                .collect();
            (rows, None)
        }
        None => (Vec::new(), Some(net_reason(ui))),
    };
    Table {
        columns,
        rows,
        empty,
    }
}

pub fn page_view(page: Page, ui: &UiState, info: &AppInfo) -> PageView {
    let s = &ui.status;
    let net = s.network.as_ref();
    match page {
        Page::Dashboard => PageView {
            engine_panel: true,
            cards: vec![
                Card::new(
                    "Download",
                    Icon::Download,
                    rate_value(ui, |n| n.rx_rate),
                    format!(
                        "{} received this session",
                        format_bytes(net.map_or(0, |n| n.session_rx_bytes))
                    ),
                ),
                Card::new(
                    "Upload",
                    Icon::Upload,
                    rate_value(ui, |n| n.tx_rate),
                    format!(
                        "{} sent this session",
                        format_bytes(net.map_or(0, |n| n.session_tx_bytes))
                    ),
                ),
                Card::new(
                    "CPU usage",
                    Icon::Cpu,
                    s.cpu_percent
                        .map_or(Value::unavailable(metric_reason(ui)), |p| {
                            Value::real(format!("{p:.1} %"), Source::Engine)
                        }),
                    "KALKAN process, one core = 100 %",
                ),
                Card::new(
                    "Memory usage",
                    Icon::Memory,
                    s.memory_bytes
                        .map_or(Value::unavailable(metric_reason(ui)), |b| {
                            Value::real(format_bytes(b), Source::Engine)
                        }),
                    "KALKAN process working set",
                ),
            ],
            strip: vec![
                StripItem {
                    label: "Active interface",
                    icon: Icon::Interface,
                    value: match net {
                        Some(n) => match n.primary() {
                            Some(i) => Value::real(interface_summary(i), Source::Engine),
                            None => Value::unavailable("No connected network adapter"),
                        },
                        None => Value::unavailable(net_reason(ui)),
                    },
                },
                StripItem {
                    label: "Interfaces",
                    icon: Icon::Network,
                    value: net_value(ui, |n| {
                        format!(
                            "{} connected of {}",
                            n.connected_count(),
                            n.interfaces.len()
                        )
                    }),
                },
                StripItem {
                    label: "Privileges",
                    icon: Icon::Shield,
                    value: privilege_value(info.elevated),
                },
            ],
            ..PageView::default()
        },
        Page::Network => PageView {
            tiles: vec![
                Tile {
                    title: "Download",
                    icon: Icon::Download,
                    series: Series::Download,
                    value: rate_value(ui, |n| n.rx_rate),
                    detail: net.map_or(net_reason(ui), |n| {
                        format!("Peak {}", format_rate(n.peak_rx_rate))
                    }),
                },
                Tile {
                    title: "Upload",
                    icon: Icon::Upload,
                    series: Series::Upload,
                    value: rate_value(ui, |n| n.tx_rate),
                    detail: net.map_or(net_reason(ui), |n| {
                        format!("Peak {}", format_rate(n.peak_tx_rate))
                    }),
                },
            ],
            table: Some(interfaces_table(ui)),
            note: Some(
                "Read-only: KALKAN reads the counters Windows already keeps and never captures, \
                 blocks or modifies traffic. Totals count each connected hardware adapter once: \
                 VPN/virtual adapters and adapters sharing a MAC address with a counted one \
                 (e.g. accelerated networking) are listed but not counted, to avoid double counting."
                    .into(),
            ),
            ..PageView::default()
        },
        Page::Statistics => PageView {
            cards: vec![
                Card::new(
                    "Received",
                    Icon::Download,
                    net_value(ui, |n| format_bytes(n.session_rx_bytes)),
                    "Since the engine started",
                ),
                Card::new(
                    "Sent",
                    Icon::Upload,
                    net_value(ui, |n| format_bytes(n.session_tx_bytes)),
                    "Since the engine started",
                ),
                Card::new(
                    "Peak download",
                    Icon::Peak,
                    net_value(ui, |n| format_rate(n.peak_rx_rate)),
                    "Highest one-interval rate",
                ),
                Card::new(
                    "Peak upload",
                    Icon::Peak,
                    net_value(ui, |n| format_rate(n.peak_tx_rate)),
                    "Highest one-interval rate",
                ),
                Card::new(
                    "Average download",
                    Icon::Average,
                    net.and_then(|n| n.average_rates())
                        .map_or(Value::unavailable(net_reason(ui)), |(rx, _)| {
                            Value::real(format_rate(rx), Source::Engine)
                        }),
                    "Session total / session time",
                ),
                Card::new(
                    "Average upload",
                    Icon::Average,
                    net.and_then(|n| n.average_rates())
                        .map_or(Value::unavailable(net_reason(ui)), |(_, tx)| {
                            Value::real(format_rate(tx), Source::Engine)
                        }),
                    "Session total / session time",
                ),
            ],
            strip: vec![
                StripItem {
                    label: "Engine running for",
                    icon: Icon::Clock,
                    value: s
                        .running_for
                        .map_or(Value::unavailable(NEEDS_RUNNING), |d| {
                            Value::real(format_duration(d), Source::Engine)
                        }),
                },
                StripItem {
                    label: "Samples",
                    icon: Icon::Statistics,
                    value: net_value(ui, |n| n.samples.to_string()),
                },
                StripItem {
                    label: "Sample interval",
                    icon: Icon::Clock,
                    value: Value::real(format!("{} ms", info.sample_interval_ms), Source::Config),
                },
            ],
            ..PageView::default()
        },
        // The Logs and Settings pages are made of native controls and
        // custom-drawn rows (see win32); their data comes from the log buffer
        // and the settings state, not from this view.
        Page::Logs => PageView {
            header_buttons: 3,
            log_viewer: true,
            ..PageView::default()
        },
        Page::Settings => PageView {
            header_buttons: 3,
            settings_rows: crate::settings::Field::ALL.len(),
            ..PageView::default()
        },
    }
}

/// `1:02:03` or `02:03`.
pub fn format_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// Link speed in decimal units, as Windows shows it (`1 Gbps`, `866.7 Mbps`).
pub fn format_link_speed(bps: u64) -> String {
    let trim = |v: f64| {
        if (v - v.round()).abs() < 0.05 {
            format!("{v:.0}")
        } else {
            format!("{v:.1}")
        }
    };
    match bps {
        b if b >= 1_000_000_000 => format!("{} Gbps", trim(b as f64 / 1e9)),
        b if b >= 1_000_000 => format!("{} Mbps", trim(b as f64 / 1e6)),
        b => format!("{} kbps", trim(b as f64 / 1e3)),
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
#[path = "model_tests.rs"]
mod tests;
