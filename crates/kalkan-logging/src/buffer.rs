//! In-memory log buffer for the GUI's log viewer.
//!
//! A `tracing` layer ([`BufferLayer`]) records every event that passes the
//! level filter into a bounded ring ([`LogBuffer`]). The viewer reads
//! snapshots (cheap: entries are `Arc`s) — it never reads the log file on the
//! UI thread. A coalesced waker notifies the GUI when entries arrive.
//!
//! [`parse_line`] / [`load_history`] read the *previous* contents of the log
//! file (written before this run started) so the viewer can show recent
//! history; damaged or foreign lines are tolerated and counted.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use kalkan_config::LogLevel;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySource {
    /// Logged by this run.
    Live,
    /// Read from the log file (an earlier run).
    History,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Milliseconds since the Unix epoch (UTC).
    pub unix_ms: i64,
    pub level: LogLevel,
    /// Thread name, if known.
    pub thread: String,
    pub target: String,
    /// Message plus `key=value` fields, as written to the file.
    pub message: String,
    pub source: EntrySource,
}

impl LogEntry {
    /// One line, as used for copy and export.
    pub fn to_line(&self) -> String {
        let mut s = format!(
            "{} {:<5} ",
            format_utc(self.unix_ms),
            self.level.as_str().to_uppercase()
        );
        if !self.thread.is_empty() {
            let _ = write!(s, "[{}] ", self.thread);
        }
        if !self.target.is_empty() {
            let _ = write!(s, "{}: ", self.target);
        }
        s.push_str(&self.message);
        s
    }
}

pub type LogWaker = Box<dyn Fn() + Send + Sync>;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HistoryInfo {
    /// Entries loaded from the file.
    pub loaded: usize,
    /// Lines that could not be parsed (damaged or foreign), skipped.
    pub skipped_lines: usize,
    /// Why history could not be read, if it could not.
    pub error: Option<String>,
    pub done: bool,
}

struct Inner {
    entries: VecDeque<Arc<LogEntry>>,
    capacity: usize,
    /// Entries dropped because the ring was full.
    evicted: u64,
    history: HistoryInfo,
}

/// Bounded, thread-safe ring of log entries.
pub struct LogBuffer {
    inner: Mutex<Inner>,
    /// Bumped on every change; lets the GUI skip identical refreshes.
    version: AtomicU64,
    waker: Mutex<Option<LogWaker>>,
    wake_pending: AtomicBool,
}

impl std::fmt::Debug for LogBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBuffer")
            .field("version", &self.version())
            .finish_non_exhaustive()
    }
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(LogBuffer {
            inner: Mutex::new(Inner {
                entries: VecDeque::with_capacity(capacity.min(4096)),
                capacity: capacity.max(1),
                evicted: 0,
                history: HistoryInfo::default(),
            }),
            version: AtomicU64::new(0),
            waker: Mutex::new(None),
            wake_pending: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn push(&self, e: LogEntry) {
        {
            let mut g = self.lock();
            if g.entries.len() == g.capacity {
                g.entries.pop_front();
                g.evicted += 1;
            }
            g.entries.push_back(Arc::new(e));
        }
        self.changed();
    }

    /// Insert history (older entries) in front of live entries, keeping the
    /// newest ones if the ring would overflow.
    pub fn prepend_history(&self, mut older: Vec<LogEntry>, info: HistoryInfo) {
        {
            let mut g = self.lock();
            let room = g.capacity.saturating_sub(g.entries.len());
            if older.len() > room {
                let cut = older.len() - room;
                g.evicted += cut as u64;
                older.drain(..cut);
            }
            for e in older.into_iter().rev() {
                g.entries.push_front(Arc::new(e));
            }
            g.history = HistoryInfo { done: true, ..info };
        }
        self.changed();
    }

    fn changed(&self) {
        self.version.fetch_add(1, Ordering::Release);
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            if let Some(w) = self
                .waker
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            {
                w();
            }
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Install the GUI waker. It is called (from any thread) at most once
    /// until [`LogBuffer::ack`] re-arms it.
    pub fn set_waker(&self, w: LogWaker) {
        *self.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(w);
        self.wake_pending.store(false, Ordering::Release);
        self.changed();
    }

    /// Re-arm the waker. Call *before* taking a snapshot.
    pub fn ack(&self) {
        self.wake_pending.store(false, Ordering::Release);
    }

    pub fn snapshot(&self) -> Vec<Arc<LogEntry>> {
        self.lock().entries.iter().cloned().collect()
    }

    pub fn evicted(&self) -> u64 {
        self.lock().evicted
    }

    pub fn capacity(&self) -> usize {
        self.lock().capacity
    }

    pub fn history(&self) -> HistoryInfo {
        self.lock().history.clone()
    }
}

/// Maps `tracing` levels onto the configuration's levels.
pub fn level_of(l: &tracing::Level) -> LogLevel {
    match *l {
        tracing::Level::ERROR => LogLevel::Error,
        tracing::Level::WARN => LogLevel::Warn,
        tracing::Level::INFO => LogLevel::Info,
        tracing::Level::DEBUG => LogLevel::Debug,
        tracing::Level::TRACE => LogLevel::Trace,
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// `tracing` layer that feeds a [`LogBuffer`].
#[derive(Debug)]
pub struct BufferLayer(pub Arc<LogBuffer>);

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut v = MessageVisitor::default();
        event.record(&mut v);
        let mut message = v.message;
        message.push_str(&v.fields);
        self.0.push(LogEntry {
            unix_ms: now_ms(),
            level: level_of(meta.level()),
            thread: std::thread::current().name().unwrap_or("").to_owned(),
            target: meta.target().to_owned(),
            message,
            source: EntrySource::Live,
        });
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── UTC date arithmetic (no external crate) ──────────────────────────────────

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-11-15 20:43:05.777` (UTC).
pub fn format_utc(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let ms = unix_ms.rem_euclid(1000);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}.{ms:03}",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fraction]Z` (the file format's timestamps).
pub fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())
            .flatten()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)? as u32, num(8..10)? as u32);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let rest = s.get(19..)?;
    let (frac, z) = match rest.strip_prefix('.') {
        Some(r) => {
            let digits = r.bytes().take_while(|c| c.is_ascii_digit()).count();
            (&r[..digits], &r[digits..])
        }
        None => ("", rest),
    };
    if z != "Z" {
        return None;
    }
    let ms = frac.get(..3.min(frac.len())).map_or(0, |f| {
        let v: i64 = f.parse().unwrap_or(0);
        v * 10i64.pow(3 - f.len() as u32)
    });
    Some(((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se) * 1000) + ms)
}

/// Parse one line written by our file layer:
/// `<RFC3339 UTC>  <LEVEL> [<thread>] <target>: <message>`.
pub fn parse_line(line: &str) -> Option<LogEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut it = line.splitn(2, ' ');
    let ts = parse_rfc3339_utc(it.next()?)?;
    let rest = it.next()?.trim_start();
    let (lvl, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    let level = LogLevel::parse(lvl)?;
    let rest = rest.trim_start();
    // "<thread> <target>: <message>" or "<target>: <message>"
    let (head, message) = match rest.split_once(": ") {
        Some((h, m)) if !h.is_empty() && h.split(' ').count() <= 2 => (h, m),
        _ => ("", rest),
    };
    let (thread, target) = match head.rsplit_once(' ') {
        Some((t, g)) => (t, g),
        None => ("", head),
    };
    Some(LogEntry {
        unix_ms: ts,
        level,
        thread: thread.to_owned(),
        target: target.to_owned(),
        message: message.to_owned(),
        source: EntrySource::History,
    })
}

/// Read up to `max_tail` bytes before offset `end` of `path` and parse them.
/// Indented lines that do not parse continue the previous entry (multi-line
/// messages); any other unparseable line (damage, foreign text) is skipped and
/// counted.
pub fn load_history(
    path: &Path,
    end: u64,
    max_tail: u64,
    max_entries: usize,
) -> (Vec<LogEntry>, HistoryInfo) {
    let mut info = HistoryInfo {
        done: true,
        ..HistoryInfo::default()
    };
    let read = || -> std::io::Result<Vec<u8>> {
        let mut f = File::open(path)?;
        let end = end.min(f.metadata()?.len());
        let start = end.saturating_sub(max_tail);
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; (end - start) as usize];
        f.read_exact(&mut buf)?;
        if start > 0 {
            // Drop the partial first line.
            let cut = buf
                .iter()
                .position(|&b| b == b'\n')
                .map_or(buf.len(), |p| p + 1);
            buf.drain(..cut);
        }
        Ok(buf)
    };
    let bytes = match read() {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), info),
        Err(e) => {
            info.error = Some(format!("could not read {}: {e}", path.display()));
            return (Vec::new(), info);
        }
    };
    // Invalid UTF-8 (a damaged file) is replaced, not fatal.
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<LogEntry> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(e) => out.push(e),
            None => match out.last_mut() {
                // Indented lines continue a multi-line message.
                Some(prev) if line.starts_with([' ', '\t']) => {
                    prev.message.push('\n');
                    prev.message.push_str(line);
                }
                _ => info.skipped_lines += 1,
            },
        }
    }
    if out.len() > max_entries {
        out.drain(..out.len() - max_entries);
    }
    info.loaded = out.len();
    (out, info)
}

#[cfg(test)]
#[path = "buffer_tests.rs"]
mod tests;
