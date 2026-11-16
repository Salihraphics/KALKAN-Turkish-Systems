use super::*;
use std::fs;
use std::sync::atomic::AtomicUsize;

fn entry(level: LogLevel, msg: &str) -> LogEntry {
    LogEntry {
        unix_ms: 0,
        level,
        thread: "main".into(),
        target: "kalkan".into(),
        message: msg.into(),
        source: EntrySource::Live,
    }
}

#[test]
fn utc_formatting_and_parsing_round_trip() {
    assert_eq!(format_utc(0), "1970-01-01 00:00:00.000");
    // 2024-02-29 (leap day) 23:59:59.999
    let ms = parse_rfc3339_utc("2024-02-29T23:59:59.999123Z").unwrap();
    assert_eq!(format_utc(ms), "2024-02-29 23:59:59.999");
    assert_eq!(
        parse_rfc3339_utc("2026-11-15T20:43:05Z"),
        Some(1_794_775_385_000)
    );
    assert_eq!(format_utc(1_794_775_385_000), "2026-11-15 20:43:05.000");
    for bad in [
        "",
        "2026-13-01T00:00:00Z",
        "2026-11-15 20:43:05Z",
        "2026-11-15T20:43:05+02:00",
        "garbage-text-here!!",
    ] {
        assert_eq!(parse_rfc3339_utc(bad), None, "{bad}");
    }
}

#[test]
fn civil_dates_match_for_a_range_of_days() {
    for day in [-1, 0, 59, 365, 11_016, 19_782, 20_407, 100_000] {
        let (y, m, d) = civil_from_days(day);
        assert_eq!(days_from_civil(y, m, d), day, "{y}-{m}-{d}");
    }
}

#[test]
fn parses_lines_written_by_the_file_layer() {
    let e = parse_line(
        "2026-11-15T20:43:05.777616Z  INFO main kalkan_gui::win32: message loop finished code=0",
    )
    .unwrap();
    assert_eq!(e.level, LogLevel::Info);
    assert_eq!(e.thread, "main");
    assert_eq!(e.target, "kalkan_gui::win32");
    assert_eq!(e.message, "message loop finished code=0");
    assert_eq!(e.source, EntrySource::History);
    let e = parse_line("2026-11-15T20:43:05.777616Z ERROR kalkan-engine kalkan_core::engine: engine failed message=\"x: y\"").unwrap();
    assert_eq!(
        (e.level, e.thread.as_str(), e.target.as_str()),
        (LogLevel::Error, "kalkan-engine", "kalkan_core::engine")
    );
    assert_eq!(e.message, "engine failed message=\"x: y\"");
    let e = parse_line("2026-11-15T20:43:05.777616Z  WARN kalkan_core::engine: no thread name")
        .unwrap();
    assert_eq!(
        (e.thread.as_str(), e.target.as_str()),
        ("", "kalkan_core::engine")
    );
}

#[test]
fn rejects_lines_that_are_not_log_lines() {
    for bad in [
        "",
        "hello world",
        "2026-11-15T20:43:05Z LOUD x: y",
        "\u{fffd}\u{fffd}\u{fffd}",
    ] {
        assert!(parse_line(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn history_tolerates_damage_and_continuations() {
    let dir = std::env::temp_dir().join(format!("kalkan-hist-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kalkan.log");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"2026-11-15T20:00:00.000Z  INFO main kalkan: first\n");
    bytes.extend_from_slice(b"   continued line\n");
    bytes.extend_from_slice(b"\xff\xfe\x00 binary damage\n");
    bytes.extend_from_slice(b"2026-11-15T20:00:01.000Z ERROR main kalkan: second\n");
    bytes.extend_from_slice(b"2026-11-15T20:00:02.000Z  INFO main kalkan: written by this run\n");
    fs::write(&path, &bytes).unwrap();
    let boundary = (bytes.len()
        - b"2026-11-15T20:00:02.000Z  INFO main kalkan: written by this run\n".len())
        as u64;
    let (entries, info) = load_history(&path, boundary, 1 << 20, 100);
    assert_eq!(
        entries.len(),
        2,
        "this run's lines are excluded: {entries:#?}"
    );
    assert_eq!(entries[0].message, "first\n   continued line");
    assert_eq!(entries[1].level, LogLevel::Error);
    assert_eq!(info.skipped_lines, 1);
    assert!(info.error.is_none() && info.done);

    // Tail limit drops the partial first line.
    let (entries, _) = load_history(&path, boundary, 60, 100);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "second");

    // Missing file is not an error; a directory is a reported error.
    let (e, i) = load_history(&dir.join("missing.log"), 100, 100, 10);
    assert!(e.is_empty() && i.error.is_none());
    let (e, i) = load_history(&dir, 100, 100, 10);
    assert!(e.is_empty() && i.error.is_some(), "{i:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ring_buffer_is_bounded_and_history_goes_first() {
    let b = LogBuffer::new(3);
    for i in 0..5 {
        b.push(entry(LogLevel::Info, &format!("live {i}")));
    }
    let s = b.snapshot();
    assert_eq!(
        s.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
        ["live 2", "live 3", "live 4"]
    );
    assert_eq!(b.evicted(), 2);

    let b = LogBuffer::new(4);
    b.push(entry(LogLevel::Info, "live"));
    let older = (0..5)
        .map(|i| entry(LogLevel::Warn, &format!("old {i}")))
        .collect();
    b.prepend_history(
        older,
        HistoryInfo {
            loaded: 5,
            ..Default::default()
        },
    );
    let msgs: Vec<_> = b.snapshot().iter().map(|e| e.message.clone()).collect();
    assert_eq!(
        msgs,
        ["old 2", "old 3", "old 4", "live"],
        "newest history kept, before live"
    );
    assert!(b.history().done);
}

#[test]
fn waker_is_coalesced_until_acknowledged() {
    let b = LogBuffer::new(10);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    b.set_waker(Box::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "initial wake on install");
    let v0 = b.version();
    for _ in 0..10 {
        b.push(entry(LogLevel::Debug, "x"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no new wake before ack");
    assert_eq!(b.version(), v0 + 10);
    b.ack();
    b.push(entry(LogLevel::Debug, "y"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn layer_captures_real_tracing_events_with_fields() {
    use tracing_subscriber::prelude::*;
    let b = LogBuffer::new(100);
    let sub = tracing_subscriber::registry().with(BufferLayer(b.clone()));
    tracing::subscriber::with_default(sub, || {
        tracing::warn!(cmd = "Start", state = 3, "command rejected");
        tracing::error!("plain");
    });
    let s = b.snapshot();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].level, LogLevel::Warn);
    assert_eq!(s[0].message, "command rejected cmd=\"Start\" state=3");
    assert!(s[0].target.contains("buffer"), "{}", s[0].target);
    assert!(s[0].unix_ms > 1_700_000_000_000);
    assert_eq!(
        s[1].to_line().split_once(' ').unwrap().1.split(' ').nth(1),
        Some("ERROR")
    );
}

#[test]
fn concurrent_pushes_are_all_recorded() {
    let b = LogBuffer::new(10_000);
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let b = b.clone();
            std::thread::spawn(move || {
                for i in 0..500 {
                    b.push(entry(LogLevel::Info, &format!("{t}-{i}")));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(b.snapshot().len(), 4000);
    assert_eq!(b.evicted(), 0);
}

#[test]
fn history_parses_old_and_new_engine_thread_names() {
    // Log files written before the KOR rename name the worker thread
    // `goodboydpi-core`; newer ones `kor-core`. Both must load.
    for thread in ["goodboydpi-core", "kor-core"] {
        let line = format!(
            "2026-11-15T20:43:05.777616Z  INFO {thread} kalkan_core::engine: engine worker stopped gen=3"
        );
        let e = parse_line(&line).unwrap_or_else(|| panic!("{line}"));
        assert_eq!(e.thread, thread);
        assert_eq!(e.target, "kalkan_core::engine");
        assert_eq!(e.message, "engine worker stopped gen=3");
    }
}
