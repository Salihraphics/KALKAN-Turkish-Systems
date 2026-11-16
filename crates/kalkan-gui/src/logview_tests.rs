use super::*;
use kalkan_logging::EntrySource;

fn e(level: LogLevel, target: &str, msg: &str) -> Arc<LogEntry> {
    Arc::new(LogEntry {
        unix_ms: 1_794_775_385_000,
        level,
        thread: "kalkan-engine".into(),
        target: target.into(),
        message: msg.into(),
        source: EntrySource::Live,
    })
}

fn sample() -> Vec<Arc<LogEntry>> {
    vec![
        e(LogLevel::Info, "kalkan_core::engine", "engine started"),
        e(
            LogLevel::Warn,
            "kalkan_core::engine",
            "invalid state transition; command rejected cmd=Start",
        ),
        e(
            LogLevel::Error,
            "kalkan_gui::win32",
            "engine did not confirm shutdown",
        ),
        e(LogLevel::Debug, "kalkan_core::engine", "engine event"),
        e(LogLevel::Trace, "kalkan_net", "sample"),
    ]
}

fn view() -> LogView {
    let mut v = LogView::new();
    v.refresh(1, sample);
    v
}

fn messages(v: &LogView) -> Vec<String> {
    (0..v.visible_len())
        .map(|i| v.row(i).unwrap().message.clone())
        .collect()
}

#[test]
fn shows_everything_by_default() {
    let v = view();
    assert_eq!(v.visible_len(), 5);
    assert_eq!(v.counts(), [1, 1, 1, 2], "error, warn, info, debug(+trace)");
}

#[test]
fn level_filter() {
    let mut v = view();
    v.filter.set_shown(LevelChip::Debug, false);
    v.filter.set_shown(LevelChip::Info, false);
    v.refilter();
    assert_eq!(
        messages(&v),
        [
            "invalid state transition; command rejected cmd=Start",
            "engine did not confirm shutdown"
        ]
    );
    for c in LevelChip::ALL {
        v.filter.set_shown(c, false);
    }
    v.refilter();
    assert_eq!(v.visible_len(), 0);
}

#[test]
fn search_is_case_insensitive_and_covers_target_thread_and_level() {
    let mut v = view();
    v.filter.set_query("  ENGINE STARTED ");
    v.refilter();
    assert_eq!(messages(&v), ["engine started"]);
    v.filter.set_query("kalkan_gui");
    v.refilter();
    assert_eq!(messages(&v), ["engine did not confirm shutdown"]);
    v.filter.set_query("warn");
    v.refilter();
    assert_eq!(
        messages(&v),
        ["invalid state transition; command rejected cmd=Start"]
    );
    v.filter.set_query("KALKAN-ENGINE");
    v.refilter();
    assert_eq!(v.visible_len(), 5, "thread name matches");
    v.filter.set_query("no such text");
    v.refilter();
    assert_eq!(v.visible_len(), 0);
}

#[test]
fn search_and_level_combine() {
    let mut v = view();
    v.filter.set_query("engine");
    v.filter.set_shown(LevelChip::Info, false);
    v.refilter();
    assert_eq!(
        messages(&v),
        [
            "invalid state transition; command rejected cmd=Start",
            "engine did not confirm shutdown",
            "engine event",
            "sample" // matches through its thread name "kalkan-engine"
        ]
    );
}

#[test]
fn pause_freezes_the_snapshot_and_resume_catches_up() {
    let mut v = view();
    v.set_paused(true);
    let mut more = sample();
    more.push(e(LogLevel::Info, "x", "new while paused"));
    assert!(!v.refresh(2, || more.clone()));
    assert_eq!(v.visible_len(), 5);
    v.set_paused(false);
    assert!(v.refresh(2, || more.clone()));
    assert_eq!(v.visible_len(), 6);
    assert!(!v.refresh(2, || unreachable!("same version: no snapshot taken")));
}

#[test]
fn selection_follows_its_entry_across_refresh_and_filter() {
    let entries = sample();
    let mut v = LogView::new();
    v.refresh(1, || entries.clone());
    v.selected = Some(2); // the ERROR entry
    let mut more = entries.clone();
    more.insert(0, e(LogLevel::Info, "x", "older history"));
    v.refresh(2, || more.clone());
    assert_eq!(
        v.selected_entry().unwrap().message,
        "engine did not confirm shutdown"
    );
    v.filter.set_shown(LevelChip::Error, false);
    v.refilter();
    assert_eq!(v.selected, None, "hidden entry deselected");
}

#[test]
fn export_contains_only_visible_rows_as_lines() {
    let mut v = view();
    v.filter.set_query("shutdown");
    v.refilter();
    let text = v.export_text();
    assert_eq!(text, "2026-11-15 20:43:05.000 ERROR [kalkan-engine] kalkan_gui::win32: engine did not confirm shutdown\r\n");
}

#[test]
fn export_file_name_from_time() {
    assert_eq!(
        export_file_name(1_794_775_385_777),
        "kalkan-logs-20261115-204305.txt"
    );
}
